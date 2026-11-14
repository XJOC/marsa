//! `marsa install` and `marsa runtimes` through the daemon (task M3-12),
//! `marsa use` with its route, `PUT /v1/runtimes/{tool}/default` (task M5-01),
//! and `marsa uninstall` with `DELETE /v1/runtimes/{tool}/{version}` (task
//! M5-07).
//!
//! The real `marsa` binary talks to the real router, auth layer, and runtimes
//! routes, served in this process on `127.0.0.1` for a temp data folder, with
//! the lock and `run/daemon.json` that `marsad` makes. `marsa` finds that
//! daemon as it finds `marsad`, so it starts no other.
//!
//! Only the installer is a fake (`Daemon::installs.runner`). The real one
//! needs the published manifest, signed with the key built into the binary,
//! over https; tests cannot swap any of them, on purpose (task M2-05). The
//! core installer's own tests cover the signed manifest, the checksums, and
//! the atomic move. No network beyond loopback.

use std::fs::{self, File, OpenOptions};
use std::net::Ipv4Addr;
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use marsa::daemon::api::router;
use marsa::daemon::auth::AuthConfig;
use marsa::daemon::php::fpm_id;
use marsa::daemon::runtimes_api::{InstallFuture, RunInstall};
use marsa::daemon::state::Daemon;
use marsa_core::config::{self, ConfigPaths};
use marsa_core::installer::{InstallError, InstallRequest, InstalledVersion};
use marsa_core::php::PhpMinor;
use marsa_core::platform::daemon_file::{
    DaemonFile, Token, daemon_authority, daemon_lock_path, write_daemon_file,
};
use marsa_core::recipes::Platform;
use marsa_core::runtimes::{Runtime, RuntimesDir};
use marsa_core::types::{
    DefaultVersion, ErrorBody, Event, InstallAccepted, InstallEvent, InstallProgress,
    InstallUpdate, RuntimeList, SiteList, Uninstalled, VersionInUse,
};
use serde::de::DeserializeOwned;
use tempfile::TempDir;
use tokio::sync::{Semaphore, broadcast};
use tower::ServiceExt as _;

/// Generous: a binary can start slowly on a busy CI machine.
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(20);
const TOTAL: u64 = 2_000_000;

/// What the fake installer was asked for.
type Asked = (Runtime, Option<String>, bool);

/// Sends the steps the real installer sends, then makes the version folder.
/// `8.9` is a version the "manifest" does not have.
#[derive(Default)]
struct FakeInstall {
    /// When set, each install waits here after its first event until the
    /// test adds a permit.
    gate: Option<Semaphore>,
    asked: Mutex<Vec<Asked>>,
}

impl FakeInstall {
    fn gated() -> Self {
        Self {
            gate: Some(Semaphore::new(0)),
            ..Self::default()
        }
    }

    fn open_gate(&self) {
        self.gate.as_ref().expect("a gated fake").add_permits(1);
    }

    fn asked(&self) -> Vec<Asked> {
        self.asked.lock().expect("lock").clone()
    }
}

impl RunInstall for FakeInstall {
    fn run<'a>(
        &'a self,
        data_dir: &'a Path,
        request: InstallRequest<'a>,
        events: &'a mut (dyn FnMut(InstallEvent) + Send),
    ) -> InstallFuture<'a> {
        Box::pin(async move {
            let asked = (
                request.runtime,
                request.version.map(str::to_owned),
                request.force,
            );
            self.asked.lock().expect("lock").push(asked);
            events(InstallEvent::ManifestVerified { serial: 3 });
            if let Some(gate) = &self.gate {
                // Dropped at once, so the gate stays open once opened.
                let _permit = gate.acquire().await.expect("the gate is not closed");
            }
            if request.version == Some("8.9") {
                return Err(InstallError::NoSuchVersion {
                    runtime: "php",
                    requested: Some("8.9".into()),
                    available: vec!["8.3.27".into(), "8.4.23".into()],
                });
            }
            let runtime = request.runtime;
            let version = match runtime {
                Runtime::Php => "8.4.23",
                Runtime::Node => "22.11.0",
                Runtime::Composer => "2.8.4",
                Runtime::Caddy => "2.11.8",
            };
            events(InstallEvent::Selected {
                runtime,
                version: version.into(),
            });
            let runtimes = RuntimesDir::in_dir(data_dir);
            let dir = runtimes.runtime_dir(runtime).join(version);
            let installed = runtimes.installed(runtime)?;
            let already = !request.force && installed.iter().any(|v| v.to_string() == version);
            if !already {
                for done in [0, TOTAL / 2, TOTAL] {
                    let artifact = "cli".into();
                    events(InstallEvent::Downloading {
                        artifact,
                        done,
                        total: TOTAL,
                    });
                }
                events(InstallEvent::Verified {
                    artifact: "cli".into(),
                });
                events(InstallEvent::Extracting {
                    artifact: "cli".into(),
                });
                events(InstallEvent::Checking);
                fs::create_dir_all(dir.join("bin")).map_err(|source| InstallError::Io {
                    path: dir.clone(),
                    source,
                })?;
            }
            events(InstallEvent::Installed { dir: dir.clone() });
            Ok(InstalledVersion {
                runtime,
                version: version.parse().expect("a semver version"),
                dir,
                already,
            })
        })
    }
}

/// The daemon's routes on a loopback port, found by `marsa` through
/// `run/daemon.lock` and `run/daemon.json`. Dropping it stops the server.
struct Served {
    daemon: Arc<Daemon>,
    fake: Arc<FakeInstall>,
    /// The same router, for requests without a socket (`oneshot`).
    router: Router,
    bearer: String,
    port: u16,
    server: tokio::task::JoinHandle<()>,
    /// Held as `marsad` holds it, so `marsa` does not start one.
    _lock: File,
    /// Last: deleted after the rest.
    dir: TempDir,
}

impl Served {
    async fn start(fake: FakeInstall) -> Self {
        let dir = TempDir::new().expect("a temp data folder");
        let run = dir.path().join("run");
        fs::create_dir_all(&run).expect("the run folder");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(daemon_lock_path(&run))
            .expect("the lock file");
        lock.try_lock().expect("the daemon lock");

        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("a loopback port");
        let port = listener.local_addr().expect("an address").port();
        let fake = Arc::new(fake);
        let mut daemon = Daemon::new(dir.path().to_path_buf());
        daemon.installs.runner = fake.clone();
        let daemon = Arc::new(daemon);

        let file = DaemonFile {
            pid: std::process::id(),
            port,
            token: Token::generate().expect("a token"),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        write_daemon_file(&run, &file).expect("run/daemon.json");
        let bearer = file.token.bearer_header();
        let router = router(daemon.clone(), AuthConfig::new(Arc::new(file.token), port));
        let server = tokio::spawn({
            let router = router.clone();
            async move { axum::serve(listener, router).await.expect("serve") }
        });
        Served {
            daemon,
            fake,
            router,
            bearer,
            port,
            server,
            _lock: lock,
            dir,
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn command(&self, args: &[&str]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_marsa"));
        command
            .arg("--data-dir")
            .arg(self.path())
            .args(args)
            .env_remove("MARSA_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    async fn marsa(&self, args: &[&str]) -> Output {
        let output = self.command(args).output();
        tokio::time::timeout(WAIT, output)
            .await
            .unwrap_or_else(|_| panic!("marsa {args:?} still runs"))
            .expect("run marsa")
    }

    /// Checks one run of `marsa`: its exit code, stdout, and stderr.
    async fn expect(&self, args: &[&str], code: i32, stdout: &str, stderr: &str) {
        check(args, &self.marsa(args).await, code, stdout, stderr);
    }

    /// A request through the router, with the token and the right `Host`.
    async fn send(&self, method: &str, path: &str, body: Option<&str>) -> (StatusCode, Vec<u8>) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", daemon_authority(self.port))
            .header("authorization", &self.bearer);
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let request = request
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_owned())))
            .expect("a request");
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("a response");
        let status = response.status();
        let body = to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("a body");
        (status, body.to_vec())
    }

    async fn json<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        want: StatusCode,
    ) -> T {
        let (status, bytes) = self.send(method, path, body).await;
        let text = String::from_utf8_lossy(&bytes);
        assert_eq!(status, want, "{method} {path}: {text}");
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|err| panic!("{method} {path}: {err}: {text}"))
    }

    async fn error(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        want: StatusCode,
    ) -> String {
        let body: ErrorBody = self.json(method, path, body, want).await;
        body.error.code
    }

    /// Waits until an install runs, or none does.
    async fn wait_running(&self, running: bool) {
        let started = Instant::now();
        while self.daemon.installs.is_running() != running {
            assert!(started.elapsed() < WAIT, "an install still runs: {running}");
            tokio::time::sleep(POLL).await;
        }
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

#[track_caller]
fn check(args: &[&str], out: &Output, code: i32, stdout: &str, stderr: &str) {
    let context = format!(
        "marsa {args:?}\nstdout:\n{}\nstderr:\n{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(code), "{context}");
    assert_eq!(text(&out.stdout), stdout, "{context}");
    assert_eq!(text(&out.stderr), stderr, "{context}");
}

fn platform() -> String {
    Platform::current().map_or_else(|| "this system".to_owned(), |p| p.to_string())
}

/// Reads the bus until an update of install `id` that `wanted` accepts.
async fn wait_update(
    events: &mut broadcast::Receiver<Event>,
    id: u64,
    wanted: impl Fn(&InstallUpdate) -> bool,
) -> InstallUpdate {
    let started = Instant::now();
    loop {
        let left = WAIT.saturating_sub(started.elapsed());
        let event = tokio::time::timeout(left, events.recv())
            .await
            .unwrap_or_else(|_| panic!("no such update of install {id}"))
            .expect("the bus is open");
        if let Event::Install(InstallProgress { id: got, update }) = event
            && got == id
            && wanted(&update)
        {
            return update;
        }
    }
}

/// The last update of install `id`: `done` or `failed`.
async fn last_update(events: &mut broadcast::Receiver<Event>, id: u64) -> InstallUpdate {
    wait_update(events, id, |update| {
        !matches!(update, InstallUpdate::Running(_))
    })
    .await
}

const BUSY: &str =
    "marsa: Another install is running. Wait for it to end, then try again. [install_busy]\n";

#[tokio::test(flavor = "multi_thread")]
async fn install_through_the_daemon_prints_progress_and_installs() {
    let served = Served::start(FakeInstall::default()).await;
    served
        .expect(
            &["runtimes"],
            0,
            "PHP       none installed\n\
             Node.js   none installed\n\
             Composer  none installed\n\
             Caddy     none installed\n",
            "",
        )
        .await;

    let dir = RuntimesDir::in_dir(served.path())
        .runtime_dir(Runtime::Php)
        .join("8.4.23");
    served
        .expect(
            &["install", "php", "8.4"],
            0,
            &format!("Installed PHP 8.4.23 in {}\n", dir.display()),
            &format!(
                "Verified the version list (serial 3)\n\
                 Installing PHP 8.4.23 for {}\n\
                 \x20 cli      [#####     ] 1.0 MB of 2.0 MB (50%)\n\
                 \x20 cli      [##########] 2.0 MB of 2.0 MB (100%)\n\
                 \x20 cli      [##########] 2.0 MB, sha256 ok\n\
                 \x20 cli      unpacking\n\
                 Checking PHP 8.4.23...\n\
                 Tip: run \"marsa install composer\" for Composer.\n",
                platform()
            ),
        )
        .await;
    assert!(dir.join("bin").is_dir());
    assert_eq!(
        served.fake.asked(),
        [(Runtime::Php, Some("8.4".to_owned()), false)]
    );

    served
        .expect(&["runtimes", "php"], 0, "PHP       8.4.23\n", "")
        .await;
    let list: RuntimeList = served
        .json("GET", "/v1/runtimes", None, StatusCode::OK)
        .await;
    assert_eq!(list.runtimes[0].installed, ["8.4.23"]);

    // Already there: the same exit code 0, and no download.
    served
        .expect(
            &["install", "php"],
            0,
            "PHP 8.4.23 is already installed. Use --force to reinstall.\n",
            &format!(
                "Verified the version list (serial 3)\n\
                 Installing PHP 8.4.23 for {}\n\
                 Tip: run \"marsa install composer\" for Composer.\n",
                platform()
            ),
        )
        .await;
    served
        .expect(
            &["install", "composer", "--force"],
            0,
            &format!(
                "Installed Composer 2.8.4 in {}\n",
                RuntimesDir::in_dir(served.path())
                    .runtime_dir(Runtime::Composer)
                    .join("2.8.4")
                    .display()
            ),
            &format!(
                "Verified the version list (serial 3)\n\
                 Installing Composer 2.8.4 for {}\n\
                 \x20 cli      [#####     ] 1.0 MB of 2.0 MB (50%)\n\
                 \x20 cli      [##########] 2.0 MB of 2.0 MB (100%)\n\
                 \x20 cli      [##########] 2.0 MB, sha256 ok\n\
                 \x20 cli      unpacking\n\
                 Checking Composer 2.8.4...\n",
                platform()
            ),
        )
        .await;
    assert_eq!(
        served.fake.asked()[1..],
        [(Runtime::Php, None, false), (Runtime::Composer, None, true)]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_install_prints_the_text_of_its_code_and_its_facts() {
    let served = Served::start(FakeInstall::default()).await;
    served
        .expect(
            &["install", "php", "8.9"],
            1,
            "",
            "Verified the version list (serial 3)\n\
             marsa: No PHP version matches \"8.9\". Available: 8.3.27, 8.4.23 \
             [install_no_such_version]\n",
        )
        .await;
    assert!(!served.daemon.installs.is_running());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_install_while_one_runs_is_409_install_busy() {
    let served = Served::start(FakeInstall::gated()).await;
    let first = served
        .command(&["install", "node"])
        .spawn()
        .expect("start marsa");
    served.wait_running(true).await;

    // Through the CLI: its text for the code, exit code 1.
    served.expect(&["install", "php", "8.4"], 1, "", BUSY).await;
    // Through the API: 409 with the code.
    let body = Some(r#"{ "version": "2", "force": false }"#);
    let code = served
        .error(
            "POST",
            "/v1/runtimes/composer/install",
            body,
            StatusCode::CONFLICT,
        )
        .await;
    assert_eq!(code, "install_busy");
    assert_eq!(served.fake.asked().len(), 1, "{:?}", served.fake.asked());

    served.fake.open_gate();
    let out = tokio::time::timeout(WAIT, first.wait_with_output())
        .await
        .expect("the first install ends")
        .expect("marsa ran");
    let dir = RuntimesDir::in_dir(served.path())
        .runtime_dir(Runtime::Node)
        .join("22.11.0");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        format!("Installed Node.js 22.11.0 in {}\n", dir.display())
    );
    served.wait_running(false).await;

    // Free again: the next one is accepted, with the next id.
    let mut events = served.daemon.events.subscribe();
    let accepted: InstallAccepted = served
        .json(
            "POST",
            "/v1/runtimes/composer/install",
            body,
            StatusCode::ACCEPTED,
        )
        .await;
    assert_eq!(accepted, InstallAccepted { id: 2 });
    match last_update(&mut events, 2).await {
        InstallUpdate::Done(done) => assert_eq!(
            (done.runtime, done.version.as_str(), done.already),
            (Runtime::Composer, "2.8.4", false)
        ),
        other => panic!("not done: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_install_ends_when_the_daemon_stops() {
    let served = Served::start(FakeInstall::gated()).await;
    let mut events = served.daemon.events.subscribe();
    let install = served
        .command(&["install", "php"])
        .spawn()
        .expect("start marsa");
    // The fake now waits at its gate, after its first step.
    wait_update(&mut events, 1, |update| {
        matches!(update, InstallUpdate::Running(_))
    })
    .await;
    // What `server::serve` does on a stop request.
    served.daemon.shutdown.cancel();
    served.daemon.publish(Event::ShuttingDown);
    served.wait_running(false).await;

    let out = tokio::time::timeout(WAIT, install.wait_with_output())
        .await
        .expect("marsa ends")
        .expect("marsa ran");
    check(
        &["install", "php"],
        &out,
        1,
        "",
        "Verified the version list (serial 3)\n\
         marsa: The Marsa daemon stopped before the install ended. Run \"marsa runtimes\" \
         to see what is installed, then try again. [install_interrupted]\n",
    );
    assert!(!served.path().join("runtimes").join("php").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_install_requests_get_their_codes() {
    let served = Served::start(FakeInstall::default()).await;
    let empty = Some("{}");
    for (method, path, body, status, code) in [
        (
            "POST",
            "/v1/runtimes/ruby/install",
            empty,
            StatusCode::NOT_FOUND,
            "unknown_tool",
        ),
        (
            "POST",
            "/v1/runtimes/PHP/install",
            empty,
            StatusCode::NOT_FOUND,
            "unknown_tool",
        ),
        (
            "POST",
            "/v1/runtimes/php/install",
            Some("not json"),
            StatusCode::BAD_REQUEST,
            "invalid_body",
        ),
        (
            "POST",
            "/v1/runtimes/php/install",
            Some(r#"{ "force": "yes" }"#),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_body",
        ),
        (
            "POST",
            "/v1/runtimes/php/install",
            None,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "invalid_body",
        ),
        (
            "GET",
            "/v1/runtimes/php/install",
            None,
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        ),
        (
            "POST",
            "/v1/runtimes",
            empty,
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        ),
    ] {
        let got = served.error(method, path, body, status).await;
        assert_eq!(got, code, "{method} {path} {body:?}");
    }
    assert!(served.fake.asked().is_empty());
    assert!(!served.daemon.installs.is_running());

    // `{}` is a whole request: the newest version, without force.
    let mut events = served.daemon.events.subscribe();
    let accepted: InstallAccepted = served
        .json(
            "POST",
            "/v1/runtimes/node/install",
            empty,
            StatusCode::ACCEPTED,
        )
        .await;
    assert!(matches!(
        last_update(&mut events, accepted.id).await,
        InstallUpdate::Done(_)
    ));
    assert_eq!(served.fake.asked(), [(Runtime::Node, None, false)]);
}

// ---------------------------------------------------------------------------
// The default version (task M5-01)

/// The next `sites_changed` on the bus; other events are skipped.
async fn next_sites(events: &mut broadcast::Receiver<Event>) -> SiteList {
    let started = Instant::now();
    loop {
        let left = WAIT.saturating_sub(started.elapsed());
        let event = tokio::time::timeout(left, events.recv())
            .await
            .expect("a sites_changed event")
            .expect("the bus is open");
        if let Event::SitesChanged(list) = event {
            return list;
        }
    }
}

/// The PHP version of the site `name`.
fn php_of(list: &SiteList, name: &str) -> Option<String> {
    let site = list.sites.iter().find(|site| site.name == name);
    let site = site.unwrap_or_else(|| panic!("no site {name} in {list:?}"));
    site.php.as_ref().map(ToString::to_string)
}

fn install_dirs(served: &Served, versions: &[(Runtime, &str)]) {
    let runtimes = RuntimesDir::in_dir(served.path());
    for (runtime, version) in versions {
        let dir = runtimes.runtime_dir(*runtime).join(version);
        fs::create_dir_all(dir).expect("a version folder");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn use_makes_a_version_the_default_and_the_sites_follow() {
    let served = Served::start(FakeInstall::default()).await;
    install_dirs(
        &served,
        &[
            (Runtime::Php, "8.3.12"),
            (Runtime::Php, "8.4.23"),
            (Runtime::Node, "22.11.0"),
        ],
    );
    // A site with no version file: it runs the default.
    let code = served.path().join("code");
    fs::create_dir_all(code.join("blog")).expect("a site folder");
    let park = serde_json::json!({ "path": code }).to_string();
    let list: SiteList = served
        .json("POST", "/v1/sites/park", Some(&park), StatusCode::OK)
        .await;
    assert_eq!(php_of(&list, "blog"), None, "no default yet");
    let global_toml = ConfigPaths::in_dir(served.path()).global;
    let mut events = served.daemon.events.subscribe();

    served
        .expect(
            &["use", "php", "8.3"],
            0,
            "PHP 8.3 is now the default (8.3.12).\n",
            "",
        )
        .await;
    let text = fs::read_to_string(&global_toml).expect("global.toml");
    assert!(text.contains("[php]\ndefault = \"8.3\"\n"), "{text}");
    let global = config::load_global(&global_toml).expect("global.toml");
    assert_eq!(global.node.default, None);
    let list = next_sites(&mut events).await;
    assert_eq!(php_of(&list, "blog").as_deref(), Some("8.3.12"));

    // Through the API: a full version is kept as written, without spaces.
    let body = Some(r#"{ "version": " 8.4.23 " }"#);
    let answer: DefaultVersion = served
        .json("PUT", "/v1/runtimes/php/default", body, StatusCode::OK)
        .await;
    let want = DefaultVersion {
        runtime: Runtime::Php,
        default: "8.4.23".into(),
        version: "8.4.23".into(),
    };
    assert_eq!(answer, want);
    // The window learns the new default first, then the new site list.
    let changed = events.try_recv().expect("a default_changed event");
    assert_eq!(changed, Event::DefaultChanged(want));
    let global = config::load_global(&global_toml).expect("global.toml");
    assert_eq!(global.php.default.as_deref(), Some("8.4.23"));
    let list = next_sites(&mut events).await;
    assert_eq!(php_of(&list, "blog").as_deref(), Some("8.4.23"));

    // Node has its own table. No site runs Node, so the list stays the same
    // and only `default_changed` is sent.
    served
        .expect(
            &["use", "node", "22"],
            0,
            "Node.js 22 is now the default (22.11.0).\n",
            "",
        )
        .await;
    let global = config::load_global(&global_toml).expect("global.toml");
    assert_eq!(
        (
            global.php.default.as_deref(),
            global.node.default.as_deref()
        ),
        (Some("8.4.23"), Some("22"))
    );
    let node = DefaultVersion {
        runtime: Runtime::Node,
        default: "22".into(),
        version: "22.11.0".into(),
    };
    assert_eq!(events.try_recv().ok(), Some(Event::DefaultChanged(node)));
    assert!(events.try_recv().is_err(), "the site list did not change");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_default_leaves_global_toml_as_it_was() {
    let served = Served::start(FakeInstall::default()).await;
    install_dirs(&served, &[(Runtime::Php, "8.3.12")]);
    let global_toml = ConfigPaths::in_dir(served.path()).global;
    let php = "/v1/runtimes/php/default";

    // No file yet: a refusal makes none.
    let body = Some(r#"{ "version": "8.9" }"#);
    let code = served.error("PUT", php, body, StatusCode::CONFLICT).await;
    assert_eq!(code, "version_not_installed");
    assert!(!global_toml.exists());

    let body = Some(r#"{ "version": "8.3" }"#);
    let _: DefaultVersion = served.json("PUT", php, body, StatusCode::OK).await;
    let before = fs::read(&global_toml).expect("global.toml");
    let mut events = served.daemon.events.subscribe();
    let (bad, conflict) = (StatusCode::BAD_REQUEST, StatusCode::CONFLICT);
    for (method, path, body, status, want) in [
        (
            "PUT",
            php,
            Some(r#"{ "version": "8.9" }"#),
            conflict,
            "version_not_installed",
        ),
        (
            "PUT",
            php,
            Some(r#"{ "version": "8.3.0" }"#),
            conflict,
            "version_not_installed",
        ),
        (
            "PUT",
            php,
            Some(r#"{ "version": "abc" }"#),
            bad,
            "invalid_version",
        ),
        (
            "PUT",
            php,
            Some(r#"{ "version": " " }"#),
            bad,
            "invalid_version",
        ),
        (
            "PUT",
            php,
            Some(r#"{ "version": "latest" }"#),
            bad,
            "unsupported_alias",
        ),
        (
            "PUT",
            "/v1/runtimes/node/default",
            Some(r#"{ "version": "22" }"#),
            conflict,
            "version_not_installed",
        ),
        (
            "PUT",
            "/v1/runtimes/composer/default",
            Some(r#"{ "version": "2" }"#),
            bad,
            "no_default_version",
        ),
        (
            "PUT",
            "/v1/runtimes/caddy/default",
            Some(r#"{ "version": "2.11" }"#),
            bad,
            "no_default_version",
        ),
        (
            "PUT",
            "/v1/runtimes/ruby/default",
            Some(r#"{ "version": "3" }"#),
            StatusCode::NOT_FOUND,
            "unknown_tool",
        ),
        (
            "PUT",
            php,
            Some("{}"),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_body",
        ),
        (
            "GET",
            php,
            None,
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        ),
    ] {
        let got = served.error(method, path, body, status).await;
        assert_eq!(got, want, "{method} {path} {body:?}");
        let now = fs::read(&global_toml).expect("global.toml");
        assert_eq!(now, before, "{method} {path} {body:?}");
    }

    // The CLI: the text of each code, and exit code 1.
    served
        .expect(
            &["use", "php", "8.9"],
            1,
            "",
            "marsa: PHP 8.9 is not installed. Run \"marsa install php 8.9\", then try again. \
             [version_not_installed]\n",
        )
        .await;
    served
        .expect(
            &["use", "caddy", "2"],
            1,
            "",
            "marsa: Caddy has no default version: Marsa always runs the newest installed one. \
             [no_default_version]\n",
        )
        .await;
    assert_eq!(fs::read(&global_toml).expect("global.toml"), before);
    assert!(events.try_recv().is_err(), "a refusal sends no event");

    // A broken file is the daemon's problem, and it stays as it is.
    fs::write(&global_toml, "php = 1\n").expect("write global.toml");
    let body = Some(r#"{ "version": "8.3" }"#);
    let code = served
        .error("PUT", php, body, StatusCode::INTERNAL_SERVER_ERROR)
        .await;
    assert_eq!(code, "config_invalid");
    served
        .expect(
            &["use", "php", "8.3"],
            1,
            "",
            &format!(
                "marsa: {} has an error, so Marsa cannot read it. Fix the file, then try again. \
                 [config_invalid]\n",
                global_toml.display()
            ),
        )
        .await;
    let text = fs::read_to_string(&global_toml).expect("global.toml");
    assert_eq!(text, "php = 1\n");
}

// ---------------------------------------------------------------------------
// Uninstall (task M5-07)

/// Registers a task as the running service `id`, as the supervisor does.
/// `on_stop` runs when the daemon stops it.
fn pretend_runs(served: &Served, id: &str, on_stop: impl FnOnce() + Send + 'static) {
    use marsa::daemon::supervisor::{ServiceHandle, ServiceRuntime};
    let cancel = tokio_util::sync::CancellationToken::new();
    let stopped = cancel.clone();
    let task = tokio::spawn(async move {
        stopped.cancelled().await;
        on_stop();
    });
    let runtime = Arc::new(Mutex::new(ServiceRuntime::new(id)));
    let handle = ServiceHandle {
        cancel,
        task,
        runtime,
    };
    served.daemon.add_proc(id, handle);
}

/// The entries of `runtimes/<runtime>/`, sorted.
fn entries(served: &Served, runtime: Runtime) -> Vec<String> {
    let dir = RuntimesDir::in_dir(served.path()).runtime_dir(runtime);
    let names = fs::read_dir(dir).expect("the runtime folder").flatten();
    let mut names: Vec<String> = names
        .map(|e| e.file_name().to_string_lossy().into())
        .collect();
    names.sort();
    names
}

#[tokio::test(flavor = "multi_thread")]
async fn uninstall_deletes_a_version_nothing_uses() {
    let served = Served::start(FakeInstall::default()).await;
    install_dirs(
        &served,
        &[(Runtime::Php, "8.3.12"), (Runtime::Php, "8.4.23")],
    );
    let out = "Uninstalled PHP 8.3.12.\n";
    served
        .expect(&["uninstall", "php", "8.3"], 0, out, "")
        .await;
    assert_eq!(entries(&served, Runtime::Php), ["8.4.23"], "no del- folder");
    let list: RuntimeList = served
        .json("GET", "/v1/runtimes", None, StatusCode::OK)
        .await;
    assert_eq!(list.runtimes[0].installed, ["8.4.23"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_version_in_use_is_refused_and_nothing_changes() {
    let served = Served::start(FakeInstall::default()).await;
    let (php, caddy) = (Runtime::Php, Runtime::Caddy);
    let versions = [
        (php, "8.3.12"),
        (php, "8.4.23"),
        (caddy, "2.11.7"),
        (caddy, "2.11.8"),
    ];
    install_dirs(&served, &versions);
    let paths = ConfigPaths::in_dir(served.path());
    let mut global = config::GlobalConfig::default();
    global.php.default = Some("8.3".into());
    config::save_global(&paths.global, &global).expect("global.toml");
    let site = |(name, php): (&str, &str)| {
        let root = served.path().join(name);
        let (php, node, secure, proxy) = (Some(php.into()), None, true, None);
        let site = config::SiteSettings {
            root,
            php,
            node,
            secure,
            proxy,
        };
        (name.to_owned(), site)
    };
    let sites = [("blog", "8.4"), ("shop", "8.4.23"), ("old", "8.3.12")].map(site);
    let sites = config::SitesConfig {
        parked: Vec::new(),
        sites: sites.into(),
    };
    config::save_sites(&paths.sites, &sites).expect("sites.toml");
    // Caddy 2.11.7 runs, though 2.11.8 is newer: it started before.
    let program =
        RuntimesDir::in_dir(served.path()).version_dir(caddy, &"2.11.7".parse().expect("v"));
    *served.daemon.caddy.started.lock().expect("lock") = Some(program.join("caddy"));
    pretend_runs(&served, "caddy", || {});
    let files = || [&paths.global, &paths.sites].map(|path| fs::read(path).expect("a file"));
    let before = files();

    let refused = async |path: &str| -> VersionInUse {
        let body: ErrorBody = served
            .json("DELETE", path, None, StatusCode::CONFLICT)
            .await;
        assert_eq!(body.error.code, "version_in_use");
        body.in_use.expect("what uses it")
    };
    let want = VersionInUse {
        version: "8.3.12".into(),
        default: Some("8.3".into()),
        sites: vec!["old".into()],
        ..VersionInUse::default()
    };
    assert_eq!(refused("/v1/runtimes/php/8.3.12").await, want);
    let running = refused("/v1/runtimes/caddy/2.11.7").await;
    assert!(running.caddy && running.default.is_none() && running.sites.is_empty());
    let err = "marsa: PHP 8.4.23 is in use, so it was not uninstalled. The sites blog, shop are \
               set to it: give them another version first. [version_in_use]\n";
    served
        .expect(&["uninstall", "php", "8.4"], 1, "", err)
        .await;
    assert_eq!(files(), before);
    assert_eq!(entries(&served, php), ["8.3.12", "8.4.23"]);

    // The Caddy that does not run goes.
    let done: Uninstalled = served
        .json("DELETE", "/v1/runtimes/caddy/2.11.8", None, StatusCode::OK)
        .await;
    assert_eq!((done.runtime, done.version.as_str()), (caddy, "2.11.8"));
    assert_eq!(entries(&served, caddy), ["2.11.7"]);
}

// ---------------------------------------------------------------------------
// The default of the last version (task M5-19)

/// The first line of every file Marsa writes.
const HEADER: &str = "# Managed by Marsa. Comments in this file are not kept.\n";

/// Writes `global.toml` with these defaults, and Xdebug on to show that the
/// rest of the file stays.
fn save_defaults(served: &Served, php: Option<&str>, node: Option<&str>) {
    let mut global = config::GlobalConfig::default();
    global.php.default = php.map(str::to_owned);
    global.php.xdebug = true;
    global.node.default = node.map(str::to_owned);
    let path = ConfigPaths::in_dir(served.path()).global;
    config::save_global(&path, &global).expect("global.toml");
}

fn global_text(served: &Served) -> String {
    let path = ConfigPaths::in_dir(served.path()).global;
    fs::read_to_string(path).expect("global.toml")
}

/// The answer of a refused `DELETE`, with its code.
async fn refusal(served: &Served, path: &str) -> (String, VersionInUse) {
    let body: ErrorBody = served
        .json("DELETE", path, None, StatusCode::CONFLICT)
        .await;
    (body.error.code, body.in_use.expect("what uses it"))
}

/// Parks `<data>/code` with these sites: `(name, files)`, each file empty
/// but `.php-version`, which asks for 8.3.
async fn park_sites(served: &Served, sites: &[(&str, &[&str])]) -> SiteList {
    let code = served.path().join("code");
    for (name, files) in sites {
        let root = code.join(name);
        fs::create_dir_all(&root).expect("a site folder");
        for file in *files {
            let text = if *file == ".php-version" { "8.3\n" } else { "" };
            fs::write(root.join(file), text).expect("a site file");
        }
    }
    let park = serde_json::json!({ "path": code }).to_string();
    served
        .json("POST", "/v1/sites/park", Some(&park), StatusCode::OK)
        .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_last_node_goes_with_its_default() {
    let served = Served::start(FakeInstall::default()).await;
    install_dirs(
        &served,
        &[(Runtime::Php, "8.4.23"), (Runtime::Node, "24.22.2")],
    );
    save_defaults(&served, Some("8.4"), Some("24"));

    let out = "Uninstalled Node.js 24.22.2.\n";
    served
        .expect(&["uninstall", "node", "24"], 0, out, "")
        .await;
    assert!(entries(&served, Runtime::Node).is_empty(), "no del- folder");
    // The `[node]` table is gone; the rest of the file stays.
    let want = format!("{HEADER}\n[php]\ndefault = \"8.4\"\nxdebug = true\n");
    assert_eq!(global_text(&served), want);
    let list: RuntimeList = served
        .json("GET", "/v1/runtimes", None, StatusCode::OK)
        .await;
    let node = &list.runtimes[1];
    assert_eq!(
        (node.runtime, node.default.as_deref()),
        (Runtime::Node, None)
    );
    assert!(node.installed.is_empty());
    assert_eq!(list.runtimes[0].default.as_deref(), Some("8.4"));
    served
        .expect(&["runtimes", "node"], 0, "Node.js   none installed\n", "")
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_move_keeps_the_default() {
    let served = Served::start(FakeInstall::default()).await;
    install_dirs(&served, &[(Runtime::Node, "24.22.2")]);
    save_defaults(&served, None, Some("24"));
    let before = global_text(&served);
    // Another process holds the install lock, so the move cannot start. The
    // default was cleared before it, and is written back.
    let runtimes = RuntimesDir::in_dir(served.path());
    fs::create_dir_all(runtimes.tmp_dir()).expect("runtimes/.tmp");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(runtimes.lock_path())
        .expect("the install lock");
    lock.try_lock().expect("the install lock is free");

    let code = served
        .error(
            "DELETE",
            "/v1/runtimes/node/24.22.2",
            None,
            StatusCode::CONFLICT,
        )
        .await;
    assert_eq!(code, "install_busy");
    assert_eq!(global_text(&served), before);
    assert_eq!(entries(&served, Runtime::Node), ["24.22.2"]);

    drop(lock);
    let _: Uninstalled = served
        .json("DELETE", "/v1/runtimes/node/24.22.2", None, StatusCode::OK)
        .await;
    assert_eq!(
        global_text(&served),
        format!("{HEADER}\n[php]\nxdebug = true\n")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_default_with_other_versions_or_a_pin_keeps_its_version() {
    let served = Served::start(FakeInstall::default()).await;
    let node = Runtime::Node;
    install_dirs(&served, &[(node, "22.11.0"), (node, "24.22.2")]);
    save_defaults(&served, None, Some("24"));
    let before = global_text(&served);

    // Another Node is installed: the user picks it as the default first.
    let (code, used) = refusal(&served, "/v1/runtimes/node/24.22.2").await;
    let want = VersionInUse {
        version: "24.22.2".into(),
        default: Some("24".into()),
        ..VersionInUse::default()
    };
    assert_eq!((code.as_str(), used), ("version_in_use", want));
    let err = "marsa: Node.js 24.22.2 is in use, so it was not uninstalled. It is the default \
               (24): make another version the default first with \"marsa use node <version>\". \
               [version_in_use]\n";
    served
        .expect(&["uninstall", "node", "24"], 1, "", err)
        .await;

    // The last one, set in a site's own settings: refused as before, and
    // the default stays. The default is not the reason: it would go too.
    served
        .expect(
            &["uninstall", "node", "22"],
            0,
            "Uninstalled Node.js 22.11.0.\n",
            "",
        )
        .await;
    let sites = config::SitesConfig {
        parked: Vec::new(),
        sites: [(
            "app".to_owned(),
            config::SiteSettings {
                root: served.path().join("app"),
                php: None,
                node: Some("24".into()),
                secure: true,
                proxy: None,
            },
        )]
        .into(),
    };
    let sites_toml = ConfigPaths::in_dir(served.path()).sites;
    config::save_sites(&sites_toml, &sites).expect("sites.toml");
    let (code, used) = refusal(&served, "/v1/runtimes/node/24.22.2").await;
    let want = VersionInUse {
        version: "24.22.2".into(),
        sites: vec!["app".into()],
        ..VersionInUse::default()
    };
    assert_eq!((code.as_str(), used), ("version_in_use", want));
    assert_eq!(global_text(&served), before);
    assert_eq!(entries(&served, node), ["24.22.2"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_last_php_goes_with_its_default_when_no_site_follows_it() {
    let served = Served::start(FakeInstall::default()).await;
    install_dirs(
        &served,
        &[(Runtime::Php, "8.3.12"), (Runtime::Node, "22.11.0")],
    );
    save_defaults(&served, Some("8.3"), Some("22"));
    // A site with its own version file, and a static site: neither follows
    // the default with PHP.
    let list = park_sites(
        &served,
        &[("api", &["index.php", ".php-version"]), ("docs", &[])],
    )
    .await;
    assert_eq!(php_of(&list, "api").as_deref(), Some("8.3.12"));
    let mut events = served.daemon.events.subscribe();

    let done: Uninstalled = served
        .json("DELETE", "/v1/runtimes/php/8.3.12", None, StatusCode::OK)
        .await;
    assert_eq!(done.version, "8.3.12");
    assert!(entries(&served, Runtime::Php).is_empty());
    let want = format!("{HEADER}\n[php]\nxdebug = true\n\n[node]\ndefault = \"22\"\n");
    assert_eq!(global_text(&served), want);
    let list = next_sites(&mut events).await;
    assert_eq!(php_of(&list, "api"), None);
    let list: RuntimeList = served
        .json("GET", "/v1/runtimes", None, StatusCode::OK)
        .await;
    assert_eq!(list.runtimes[0].default, None);
    assert_eq!(list.runtimes[1].default.as_deref(), Some("22"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_last_php_stays_while_sites_follow_its_default() {
    let served = Served::start(FakeInstall::default()).await;
    install_dirs(&served, &[(Runtime::Php, "8.3.12")]);
    save_defaults(&served, Some("8.3"), None);
    let list = park_sites(
        &served,
        &[
            ("shop", &["index.php"]),
            ("blog", &["index.php"]),
            ("api", &["index.php", ".php-version"]),
            ("docs", &[]),
        ],
    )
    .await;
    assert_eq!(php_of(&list, "blog").as_deref(), Some("8.3.12"));
    let paths = ConfigPaths::in_dir(served.path());
    let files = || [&paths.global, &paths.sites].map(|path| fs::read(path).expect("a file"));
    let before = files();

    let (code, used) = refusal(&served, "/v1/runtimes/php/8.3.12").await;
    let want = VersionInUse {
        version: "8.3.12".into(),
        default_sites: vec!["blog".into(), "shop".into()],
        ..VersionInUse::default()
    };
    assert_eq!((code.as_str(), used), ("sites_follow_default", want));
    let err = "marsa: PHP 8.3.12 is in use, so it was not uninstalled. The sites blog, shop \
               follow the default, and it is the only PHP installed: install another version \
               first with \"marsa install php <version>\", then make it the default with \
               \"marsa use php <version>\". [sites_follow_default]\n";
    served
        .expect(&["uninstall", "php", "8.3"], 1, "", err)
        .await;

    // A site set to it as well: every use, under the general code.
    let body = serde_json::json!({ "php": "8.3" }).to_string();
    let _: SiteList = served
        .json("PATCH", "/v1/sites/shop", Some(&body), StatusCode::OK)
        .await;
    let (code, used) = refusal(&served, "/v1/runtimes/php/8.3.12").await;
    let want = VersionInUse {
        version: "8.3.12".into(),
        sites: vec!["shop".into()],
        default_sites: vec!["blog".into()],
        ..VersionInUse::default()
    };
    assert_eq!((code.as_str(), used), ("version_in_use", want));
    assert_eq!(files()[0], before[0], "global.toml as it was");
    assert_eq!(entries(&served, Runtime::Php), ["8.3.12"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pool_of_the_version_stops_before_the_delete_and_the_sites_follow() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let served = Served::start(FakeInstall::default()).await;
    install_dirs(
        &served,
        &[(Runtime::Php, "8.3.12"), (Runtime::Php, "8.4.23")],
    );
    let php = RuntimesDir::in_dir(served.path()).runtime_dir(Runtime::Php);
    fs::create_dir_all(php.join("8.3.12").join("bin")).expect("bin");
    // A PHP site that asks for 8.3 in a version file: no default, no setting.
    let blog = served.path().join("code").join("blog");
    fs::create_dir_all(&blog).expect("a site folder");
    fs::write(blog.join("index.php"), "<?php").expect("index.php");
    fs::write(blog.join(".php-version"), "8.3\n").expect(".php-version");
    let park = serde_json::json!({ "path": served.path().join("code") }).to_string();
    let list: SiteList = served
        .json("POST", "/v1/sites/park", Some(&park), StatusCode::OK)
        .await;
    assert_eq!(php_of(&list, "blog").as_deref(), Some("8.3.12"));
    // The service ids of the two pools: php-fpm, or php-cgi on Windows.
    let (pool83, pool84) = (
        fpm_id(PhpMinor { major: 8, minor: 3 }),
        fpm_id(PhpMinor { major: 8, minor: 4 }),
    );
    // When the pool stops, the folder moved aside is still whole.
    let whole = Arc::new(AtomicBool::new(false));
    pretend_runs(&served, &pool83, {
        let whole = whole.clone();
        move || {
            let mut names = fs::read_dir(&php).expect("php").flatten();
            let aside = names.find(|e| e.file_name().to_string_lossy().starts_with("del-"));
            let bin = aside.map(|aside| aside.path().join("bin"));
            whole.store(bin.is_some_and(|bin| bin.is_dir()), Ordering::SeqCst);
        }
    });
    pretend_runs(&served, &pool84, || {});
    let mut events = served.daemon.events.subscribe();

    let _: Uninstalled = served
        .json("DELETE", "/v1/runtimes/php/8.3.12", None, StatusCode::OK)
        .await;
    assert!(whole.load(Ordering::SeqCst), "stopped before the delete");
    assert!(!served.daemon.proc_alive(&pool83));
    assert!(served.daemon.proc_alive(&pool84), "another line runs on");
    assert_eq!(php_of(&next_sites(&mut events).await, "blog"), None);
    assert_eq!(entries(&served, Runtime::Php), ["8.4.23"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_uninstall_requests_get_their_codes() {
    let served = Served::start(FakeInstall::gated()).await;
    install_dirs(
        &served,
        &[(Runtime::Php, "8.3.12"), (Runtime::Php, "8.3.20")],
    );
    let (bad, missing) = (StatusCode::BAD_REQUEST, StatusCode::NOT_FOUND);
    for (method, path, status, want) in [
        (
            "DELETE",
            "/v1/runtimes/php/8.9",
            missing,
            "version_not_installed",
        ),
        (
            "DELETE",
            "/v1/runtimes/php/8.3",
            StatusCode::CONFLICT,
            "version_ambiguous",
        ),
        ("DELETE", "/v1/runtimes/php/abc", bad, "invalid_version"),
        (
            "DELETE",
            "/v1/runtimes/php/latest",
            bad,
            "unsupported_alias",
        ),
        ("DELETE", "/v1/runtimes/ruby/3.0.0", missing, "unknown_tool"),
        (
            "GET",
            "/v1/runtimes/php/8.3.12",
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        ),
    ] {
        let got = served.error(method, path, None, status).await;
        assert_eq!(got, want, "{method} {path}");
    }
    let err = "marsa: PHP 8.9 is not installed. Run \"marsa runtimes php\" to see the installed \
               versions. [version_not_installed]\n";
    served
        .expect(&["uninstall", "php", "8.9"], 1, "", err)
        .await;

    // While an install runs: its own code, and nothing moves.
    let install = served
        .command(&["install", "node"])
        .spawn()
        .expect("start marsa");
    served.wait_running(true).await;
    served
        .expect(&["uninstall", "php", "8.3.12"], 1, "", BUSY)
        .await;
    served.fake.open_gate();
    let out = tokio::time::timeout(WAIT, install.wait_with_output()).await;
    assert_eq!(
        out.expect("in time").expect("marsa ran").status.code(),
        Some(0)
    );
    assert_eq!(entries(&served, Runtime::Php), ["8.3.12", "8.3.20"]);
}
