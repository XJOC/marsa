//! Starts Caddy through the supervisor and talks to its admin API.
//!
//! `marsa_core::caddy` builds the config as data. This module writes it to
//! `caddy/caddy.json`, runs the newest installed Caddy
//! (`runtimes/caddy/<version>/caddy run --config caddy/caddy.json`) under the
//! supervisor, and waits until the admin API answers. Like a Laravel job
//! that starts a queue worker, then checks that it is healthy.
//!
//! Before each start, [`orphan`] stops a Caddy that a crashed daemon left
//! behind, or refuses to start when another program holds our ports. A
//! `stop` with no Caddy running stops such an orphan too. After each spawn
//! the watcher writes `run/caddy.pid`, and removes it once Caddy has exited
//! (ADR 0009). The `admin` object in `caddy.json` carries this data folder's
//! tag, `marsa-instance-<id>`, so the daemon of another folder never takes
//! our Caddy for its own.
//!
//! The config is always `sites_config` of the sites and PHP pools that the
//! sync task (`sites_sync`) last gave [`CaddyManager::apply`]. While Caddy
//! runs, each change goes to its admin API (`POST /load`), so Caddy changes
//! without a restart. The ports stay the same: no new listener.
//!
//! When `marsad` starts, [`start_at_launch`] starts an installed Caddy in the
//! background, through the same start.

pub mod orphan;

mod admin;
mod launch;
mod manager;
mod watch;

pub use self::admin::LoadError;
pub use self::launch::{Launch, Launched, start_at_launch};
pub use self::manager::{Applied, CaddyManager};

use marsa_core::caddy::{CaddyError, CaddyPaths, caddy_env};
use marsa_core::codes;
use marsa_core::services::{ProgramNotAbsolute, RestartPolicy, ServiceSpec, StopMethod};
use marsa_core::types::ServiceId;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

/// How Caddy and each php-fpm pool restart after a failure: up to 3 times,
/// after 0.5 s, then twice as long each time, at most 5 s.
pub const RESTART: RestartPolicy = RestartPolicy::OnFailure {
    max_retries: 3,
    initial: Duration::from_millis(500),
    max: Duration::from_secs(5),
};
/// How long a stop of Caddy or of a php-fpm pool waits after SIGTERM.
pub const STOP_GRACE: Duration = Duration::from_secs(2);

/// How the supervisor runs Caddy: the newest installed Caddy
/// ([`CaddyPaths::program`]) with `run --config caddy/caddy.json`, and
/// Caddy's own data inside `caddy/`.
///
/// # Errors
///
/// The error of [`CaddyPaths::program`]: `caddy_not_installed` when
/// `runtimes/caddy` has no version.
pub fn caddy_spec(paths: &CaddyPaths) -> Result<ServiceSpec, CaddyError> {
    Ok(caddy_spec_for(paths, paths.program()?))
}

/// [`caddy_spec`] with `program` as Caddy. Tests run a fake this way.
pub fn caddy_spec_for(paths: &CaddyPaths, program: PathBuf) -> ServiceSpec {
    ServiceSpec {
        id: ServiceId::Caddy,
        program,
        // `to_string_lossy` as in `caddy_env`: the app data dir comes from the
        // OS and is valid Unicode in practice.
        args: vec![
            "run".to_owned(),
            "--config".to_owned(),
            paths.config_file.to_string_lossy().into_owned(),
        ],
        env: caddy_env(paths),
        env_remove: Vec::new(),
        cwd: None,
        restart: RESTART,
        stop: StopMethod::term(STOP_GRACE),
    }
}

/// Why a service action failed. The route turns it into an `ErrorBody`.
#[derive(Debug)]
pub enum ActionError {
    /// [`CaddyPaths::program`] found no Caddy to run: no version in
    /// `runtimes/caddy`, or, rarely, no Caddy for this system or a broken
    /// recipe.
    NoCaddy(CaddyError),
    /// The program to run is not a file: a version folder without its
    /// `caddy`.
    NotInstalled(PathBuf),
    /// Another action on the service has not finished.
    Busy,
    /// `caddy.json` cannot be written.
    ConfigWrite(io::Error),
    ProgramNotAbsolute(ProgramNotAbsolute),
}

impl ActionError {
    /// Stable error name for the UI. Every [`ActionError::NoCaddy`] is
    /// `caddy_not_installed`, the code the CLI and GUI know, with
    /// `marsa install caddy` as the fix; its text keeps the exact reason.
    pub fn code(&self) -> &'static str {
        match self {
            ActionError::NoCaddy(_) | ActionError::NotInstalled(_) => codes::CADDY_NOT_INSTALLED,
            ActionError::Busy => codes::SERVICE_BUSY,
            ActionError::ConfigWrite(_) => codes::CADDY_CONFIG_WRITE,
            ActionError::ProgramNotAbsolute(err) => err.code(),
        }
    }
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActionError::NoCaddy(err) => err.fmt(f),
            ActionError::NotInstalled(path) => write!(f, "no Caddy at {}", path.display()),
            ActionError::Busy => f.write_str("another action on this service is running"),
            ActionError::ConfigWrite(err) => write!(f, "cannot write caddy.json: {err}"),
            ActionError::ProgramNotAbsolute(err) => err.fmt(f),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::data_dir::DataDir;
    use std::env::consts::EXE_SUFFIX;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use axum::http::StatusCode;
    use axum::routing::post;
    use marsa_core::caddy::{CaddyInput, PhpPools, caddy_config};
    use marsa_core::sites::Site;
    use marsa_core::sites::SiteKind;
    use marsa_core::types::{LogLine, LogStream};
    use serde_json::Value;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use super::admin::load_error;
    use super::manager::{tag_config, write_config};
    use super::watch::is_port_conflict;
    use crate::daemon::services::supervisor::{ServiceHandle, ServiceRuntime};
    use crate::daemon::state::Daemon;

    fn line(stream: LogStream, text: &str) -> LogLine {
        LogLine {
            service: ServiceId::Caddy,
            seq: 0,
            at_ms: 0,
            stream,
            text: text.into(),
        }
    }

    #[test]
    fn caddy_spec_runs_the_newest_caddy_with_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let caddy = DataDir::new(dir.path()).runtimes().join("caddy");
        for version in ["2.11.8", "2.11.10", "2.11.9"] {
            std::fs::create_dir_all(caddy.join(version)).unwrap();
        }
        let paths = CaddyPaths::in_dir(dir.path());
        let spec = caddy_spec(&paths).unwrap();

        let program = caddy.join("2.11.10").join(format!("caddy{EXE_SUFFIX}"));
        assert_eq!(spec.program, program);
        assert!(spec.program.is_absolute());
        let config = dir.path().join("caddy").join("caddy.json");
        let config = config.to_str().unwrap();
        assert_eq!(spec.args, ["run", "--config", config]);
        assert_eq!(spec.env, caddy_env(&paths));
        assert_eq!(spec.id, ServiceId::Caddy);
        assert_eq!(spec.cwd, None);
        let restart = RestartPolicy::OnFailure {
            max_retries: 3,
            initial: Duration::from_millis(500),
            max: Duration::from_secs(5),
        };
        assert_eq!(spec.restart, restart);
        assert_eq!(RESTART, restart);
        assert_eq!(spec.stop, StopMethod::term(Duration::from_secs(2)));
        assert_eq!(caddy_spec_for(&paths, program), spec);
    }

    #[test]
    fn caddy_spec_without_caddy_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = CaddyPaths::in_dir(dir.path());

        let err = caddy_spec(&paths).unwrap_err();
        assert_eq!(err.code(), "caddy_not_installed");
        // Every reason for no Caddy is the code the CLI and GUI know.
        for err in [
            err,
            CaddyError::UnsupportedPlatform,
            CaddyError::RecipeInvalid { reason: "x".into() },
        ] {
            assert_eq!(ActionError::NoCaddy(err).code(), "caddy_not_installed");
        }
        let missing = ActionError::NotInstalled(paths.runtime_dir().join("2.11.8"));
        assert_eq!(missing.code(), "caddy_not_installed");
    }

    #[test]
    fn a_taken_port_is_found_on_stderr_only() {
        let go = "Error: loading initial config: loading new config: http app module: start: \
                  listening on 127.0.0.1:18080: listen tcp 127.0.0.1:18080: bind: address already in use";
        let windows = "listen tcp 127.0.0.1:18080: bind: Only one usage of each socket \
                       address (protocol/network address/port) is normally permitted.";
        // Port 80 on a Windows machine where IIS runs.
        let held_alone = "listening on 127.0.0.1:80: listen tcp 127.0.0.1:80: bind: An attempt \
                          was made to access a socket in a way forbidden by its access permissions.";
        assert!(is_port_conflict(&line(LogStream::Stderr, go)));
        assert!(is_port_conflict(&line(LogStream::Stderr, windows)));
        assert!(is_port_conflict(&line(LogStream::Stderr, held_alone)));
        assert!(!is_port_conflict(&line(LogStream::Stdout, go)));
        let other = "bind: permission denied";
        assert!(!is_port_conflict(&line(LogStream::Stderr, other)));
    }

    /// Linux after setup (ADR 0014): the next config has ports 80 and 443.
    /// The manager asks at every config, so a running Caddy moves with no
    /// restart of the daemon.
    #[test]
    fn the_config_takes_ports_80_and_443_once_the_system_allows_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut caddy = CaddyManager::new(dir.path());
        let tag = "marsa-instance-0123456789abcdef0123456789abcdef";
        let ports = |config: &Value| {
            let http = &config["apps"]["http"];
            (
                http["http_port"].as_u64(),
                http["https_port"].as_u64(),
                config["admin"]["listen"].as_str().map(str::to_owned),
            )
        };
        let admin = Some("127.0.0.1:18019".to_owned());

        // A new manager never asks the system: tests stay on the high ports.
        assert_eq!(caddy.ports(), CaddyInput::default());
        let high = caddy.config(tag).unwrap();
        assert_eq!(ports(&high), (Some(18080), Some(18443), admin.clone()));

        caddy.low_ports = || true;
        assert_eq!(caddy.ports(), CaddyInput::default().on_low_ports());
        let low = caddy.config(tag).unwrap();
        assert_eq!(ports(&low), (Some(80), Some(443), admin.clone()));

        // And back, if the system stops allowing them.
        caddy.low_ports = || false;
        assert_eq!(
            ports(&caddy.config(tag).unwrap()),
            (Some(18080), Some(18443), admin)
        );
    }

    #[tokio::test]
    async fn write_config_replaces_caddy_json_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caddy").join("caddy.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "old").unwrap();

        let input = CaddyInput {
            admin_port: 1,
            ..CaddyInput::default()
        };
        let tag = "marsa-instance-0123456789abcdef0123456789abcdef";
        let mut config = caddy_config(&input);
        tag_config(&mut config, tag);
        write_config(&path, &config).await.unwrap();
        let mut written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // The core config, plus our tag on the admin object, and nothing else.
        assert_eq!(written["admin"]["@id"], tag);
        let admin = written["admin"].as_object_mut().unwrap();
        admin.remove("@id");
        assert_eq!(written, caddy_config(&input));
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["caddy.json"], "no temp file left");
    }

    /// What Caddy v2.11.7 answered when another program held its HTTPS
    /// address (ADR 0008, finding 11, T9), verbatim.
    const T9: &str = "{\"error\":\"loading config: loading new config: http app module: start: \
                      listening on 127.0.0.1:18443: listen tcp 127.0.0.1:18443: bind: address already in use\"}";

    #[test]
    fn a_taken_address_in_a_load_answer_is_port_in_use() {
        let err = load_error(400, T9);
        assert_eq!(err.code(), "port_in_use");
        let LoadError::PortInUse { address, message } = err else {
            panic!("not port_in_use");
        };
        assert_eq!(address.as_deref(), Some("127.0.0.1:18443"));
        assert!(message.starts_with("loading config:"), "{message}");

        // The redirect port (T9b), and a low port Caddy may not use.
        let t9b = T9.replace("18443", "18080");
        let LoadError::PortInUse { address, .. } = load_error(400, &t9b) else {
            panic!("not port_in_use");
        };
        assert_eq!(address.as_deref(), Some("127.0.0.1:18080"));
        let denied = "{\"error\":\"loading config: loading new config: http app module: start: \
                      listening on [::1]:443: listen tcp [::1]:443: bind: permission denied\"}";
        let LoadError::PortInUse { address, .. } = load_error(400, denied) else {
            panic!("not port_in_use");
        };
        assert_eq!(address.as_deref(), Some("[::1]:443"));
        // Windows, with IIS on port 80.
        let held_alone = "{\"error\":\"loading config: loading new config: http app module: start: \
                          listening on 127.0.0.1:80: listen tcp 127.0.0.1:80: bind: An attempt was \
                          made to access a socket in a way forbidden by its access permissions.\"}";
        let LoadError::PortInUse { address, .. } = load_error(400, held_alone) else {
            panic!("not port_in_use");
        };
        assert_eq!(address.as_deref(), Some("127.0.0.1:80"));
    }

    #[test]
    fn other_load_answers_are_rejected_with_their_text() {
        let bad = "{\"error\":\"loading config: decoding request body: unexpected EOF\"}";
        let err = load_error(400, bad);
        assert_eq!(err.code(), "caddy_config_rejected");
        assert!(err.to_string().contains("unexpected EOF"), "{err}");
        // Only a 400 says that Caddy refused the config itself.
        assert_eq!(load_error(500, T9).code(), "caddy_config_rejected");
        // A body that is not JSON is kept as text.
        let LoadError::Rejected { status, message } = load_error(502, " bad gateway\n") else {
            panic!("not rejected");
        };
        assert_eq!((status, message.as_str()), (502, "bad gateway"));
        // No `listening on`: still the code, without an address.
        let short = "{\"error\":\"bind: address already in use\"}";
        let LoadError::PortInUse { address, .. } = load_error(400, short) else {
            panic!("not port_in_use");
        };
        assert_eq!(address, None);
    }

    /// A fake admin API: `POST /load` sends each body on the channel and
    /// answers `status` with `body`.
    async fn fake_admin(
        status: StatusCode,
        body: &'static str,
    ) -> (u16, mpsc::UnboundedReceiver<Value>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let app = axum::Router::new().route(
            "/load",
            post(move |axum::Json(config): axum::Json<Value>| async move {
                tx.send(config).unwrap();
                (status, body)
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (port, rx)
    }

    /// Registers a task as the running Caddy, so `apply` sends to the admin API.
    fn pretend_caddy_runs(daemon: &Daemon) {
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let cancel = cancel.clone();
            async move { cancel.cancelled().await }
        });
        let runtime = Arc::new(Mutex::new(ServiceRuntime::new(ServiceId::Caddy)));
        daemon.add_proc(
            ServiceId::Caddy,
            ServiceHandle {
                cancel,
                task,
                runtime,
                stop_limit: Duration::ZERO,
            },
        );
    }

    fn shop(root: PathBuf) -> Site {
        Site {
            name: "shop".into(),
            docroot: root.clone(),
            root,
            kind: SiteKind::Static,
            php: None,
            php_source: None,
            secure: true,
            queue: false,
            schedule: false,
        }
    }

    #[tokio::test]
    async fn apply_writes_the_file_only_while_caddy_does_not_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::new(dir.path().to_path_buf());
        // Nothing listens there: a call would fail.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        daemon.caddy_mut().input.admin_port = free.local_addr().unwrap().port();
        drop(free);
        let sites = vec![shop(dir.path().join("shop"))];
        let applied = daemon
            .caddy()
            .apply(&daemon, sites, PhpPools::default())
            .await;
        assert_eq!(applied.unwrap(), Applied::Written);

        let text = std::fs::read_to_string(&daemon.caddy().paths.config_file).unwrap();
        let written: Value = serde_json::from_str(&text).unwrap();
        let hosts = &written["apps"]["http"]["servers"]["marsa-tls"]["routes"][0]["match"][0];
        assert_eq!(hosts["host"][0], "shop.test");
        let tag = written["admin"]["@id"].as_str().unwrap();
        assert!(tag.starts_with("marsa-instance-"), "{tag}");
    }

    #[tokio::test]
    async fn apply_loads_the_config_into_a_running_caddy() {
        let (port, mut loads) = fake_admin(StatusCode::OK, "").await;
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::new(dir.path().to_path_buf());
        daemon.caddy_mut().input.admin_port = port;
        pretend_caddy_runs(&daemon);

        let sites = vec![shop(dir.path().join("shop"))];
        let applied = daemon
            .caddy()
            .apply(&daemon, sites, PhpPools::default())
            .await;
        assert_eq!(applied.unwrap(), Applied::Loaded);
        let loaded = loads.recv().await.unwrap();
        let text = std::fs::read_to_string(&daemon.caddy().paths.config_file).unwrap();
        let written: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(loaded, written, "the file and Caddy have the same config");
        daemon.stop_procs(Duration::from_secs(5)).await;
    }

    #[tokio::test]
    async fn a_refused_load_is_port_in_use_and_noted_in_the_log() {
        let (port, mut loads) = fake_admin(StatusCode::BAD_REQUEST, T9).await;
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::new(dir.path().to_path_buf());
        daemon.caddy_mut().input.admin_port = port;
        pretend_caddy_runs(&daemon);

        let sites = vec![shop(dir.path().join("shop"))];
        let err = daemon
            .caddy()
            .apply(&daemon, sites, PhpPools::default())
            .await
            .unwrap_err();
        assert!(loads.recv().await.is_some());
        assert_eq!(err.code(), "port_in_use");
        assert!(err.to_string().contains("127.0.0.1:18443"), "{err}");
        let last = daemon.caddy().recent_logs().pop().unwrap();
        assert!(
            last.text
                .starts_with("marsad: The new config was not loaded"),
            "{}",
            last.text
        );
        assert!(last.text.contains("127.0.0.1:18443"), "{}", last.text);
        daemon.stop_procs(Duration::from_secs(5)).await;
    }

    /// A Laravel site on PHP 8.3 and a pool for it.
    fn laravel(root: PathBuf) -> (Vec<Site>, PhpPools) {
        use marsa_core::caddy::Dial;
        use marsa_core::php::PhpMinor;
        let site = Site {
            kind: SiteKind::Laravel,
            php: Some("8.3.14".parse().unwrap()),
            ..shop(root)
        };
        let pools = PhpPools(std::collections::BTreeMap::from([(
            PhpMinor { major: 8, minor: 3 },
            vec![Dial::Unix(PathBuf::from("/run/php-8.3.sock"))],
        )]));
        (vec![site], pools)
    }

    /// The dump server turning on or off reaches a running Caddy, and a
    /// Caddy that does not run reads it from the next config.
    #[tokio::test]
    async fn caddy_follows_the_dump_server() {
        use crate::daemon::dumps::DumpsService;
        let (port, mut loads) = fake_admin(StatusCode::OK, "").await;
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::new(dir.path().to_path_buf());
        daemon.caddy_mut().input.admin_port = port;
        daemon.set_dumps(DumpsService::new(0));
        let daemon = Arc::new(daemon);
        let (sites, pools) = laravel(dir.path().join("shop"));
        daemon.caddy().apply(&daemon, sites, pools).await.unwrap();
        let follower = crate::daemon::dumps::caddy::follow(&daemon);
        assert_eq!(daemon.caddy().dumps_target(), None);

        // Off to on while Caddy does not run: the target is kept, nothing sent.
        let shutdown = daemon.shutdown().clone();
        daemon.dumps().apply(true, &shutdown).await;
        let addr = daemon.dumps().local_addr().unwrap();
        wait_for(|| daemon.caddy().dumps_target() == Some(addr)).await;
        assert!(loads.try_recv().is_err(), "no Caddy runs: nothing is sent");

        // Caddy runs: off sends a config without the parameters, on with them.
        pretend_caddy_runs(&daemon);
        daemon.dumps().apply(false, &shutdown).await;
        let off = loads.recv().await.unwrap();
        assert!(!off.to_string().contains("VAR_DUMPER"), "{off}");
        daemon.dumps().apply(true, &shutdown).await;
        let on = loads.recv().await.unwrap();
        let addr = daemon.dumps().local_addr().unwrap();
        let text = on.to_string();
        assert!(text.contains("\"VAR_DUMPER_FORMAT\":\"server\""), "{text}");
        assert!(
            text.contains(&format!("\"VAR_DUMPER_SERVER\":\"{addr}\"")),
            "{text}"
        );

        shutdown.cancel();
        follower.await.unwrap();
        daemon.dumps().shutdown(Duration::from_secs(2)).await;
        daemon.stop_procs(Duration::from_secs(5)).await;
    }

    /// A port that another program holds is not a place to send dumps to.
    #[tokio::test]
    async fn a_busy_dump_port_gives_caddy_no_address() {
        use crate::daemon::dumps::DumpsService;
        let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut daemon = Daemon::new(tempfile::tempdir().unwrap().path().to_path_buf());
        daemon.set_dumps(DumpsService::new(blocker.local_addr().unwrap().port()));
        let daemon = Arc::new(daemon);
        let follower = crate::daemon::dumps::caddy::follow(&daemon);
        let shutdown = daemon.shutdown().clone();
        daemon.dumps().apply(true, &shutdown).await;
        assert_eq!(daemon.caddy().dumps_target(), None);
        shutdown.cancel();
        follower.await.unwrap();
    }

    async fn wait_for(mut done: impl FnMut() -> bool) {
        for _ in 0..500 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the condition never held");
    }
}
