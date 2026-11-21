//! What only Valkey needs: its `valkey.conf` on disk, how it runs, and how
//! it says it is ready.
//!
//! Valkey has no first init: it starts on an empty data folder, and
//! `SHUTDOWN SAVE` writes `dump.rdb` there at each stop. It writes no pid
//! file of its own and takes no lock on the folder, so the check before a
//! start is the port and our own record of its pid (`run/valkey.pid`).

use std::io;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::Duration;

use marsa_core::databases::valkey::{
    ValkeyDirs, command, is_pong, valkey_args, valkey_env, valkey_stop,
};
use marsa_core::databases::{ServerKind, UnwritablePath};
use marsa_core::platform::daemon_file::create_private_dir;
use marsa_core::recipes::Os;
use marsa_core::services::{RestartPolicy, ServiceSpec};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const KIND: ServerKind = ServerKind::Valkey;
/// For each attempt of the readiness check.
const CONNECT_LIMIT: Duration = Duration::from_millis(500);
/// Owner-only, like every file in `run/`.
const PRIVATE: u32 = 0o600;

/// How the supervisor runs Valkey: `valkey-server run/valkey.conf`, stopped
/// with `SHUTDOWN SAVE` on its port, then SIGTERM, then a kill
/// ([`valkey_stop`]); Windows has no SIGTERM, so there the kill follows the
/// request. Valkey logs to stdout (`logfile ""`), which the supervisor keeps
/// in `logs/valkey.log`. On Windows the path and the environment are for
/// the Cygwin build ([`valkey_args`], [`valkey_env`]), and the supervisor
/// follows the process it started, whose pid is the one Windows knows.
///
/// Never restarted after it exits, as PostgreSQL and MySQL: a refusal at its
/// start would only come again, and the status must keep the run's own
/// exit.
///
/// # Errors
///
/// [`UnwritablePath`] on Windows for a config path that Cygwin's form
/// cannot hold.
pub(super) fn spec(
    program: PathBuf,
    dirs: &ValkeyDirs,
    port: u16,
    os: Os,
) -> Result<ServiceSpec, UnwritablePath> {
    Ok(ServiceSpec {
        id: KIND.id().to_owned(),
        program,
        args: valkey_args(&dirs.config(), os)?,
        env: valkey_env(os).into_iter().collect(),
        cwd: None,
        restart: RestartPolicy::Never,
        stop: valkey_stop(port),
    })
}

/// Writes `text`, Marsa's `valkey.conf`, to `run/valkey.conf`, owner-only,
/// and makes the data folder owner-only: `dump.rdb` holds the apps' cached
/// data and their queued jobs. Before each start, so a new port takes
/// effect. Valkey goes into the data folder at its start, so it must exist.
pub(super) async fn write_config(dirs: &ValkeyDirs, text: String) -> io::Result<()> {
    let dirs = dirs.clone();
    let job = tokio::task::spawn_blocking(move || {
        create_private_dir(&dirs.run)?;
        create_private_dir(&dirs.data)?;
        marsa_core::fs::write_atomic(&dirs.config(), text.as_bytes(), Some(PRIVATE))
    });
    job.await.map_err(io::Error::other)?
}

/// Whether Valkey takes commands on `127.0.0.1:<port>`: it answers `PING`
/// with `+PONG` ([`is_pong`]). A bare TCP connection is not enough: while
/// it loads `dump.rdb` it answers `-LOADING`. Valkey logs nothing for this.
pub(super) async fn answers(port: u16) -> bool {
    let ask = async {
        let address = (Ipv4Addr::LOCALHOST, port);
        let mut stream = tokio::net::TcpStream::connect(address).await.ok()?;
        stream.write_all(&command(&["PING"])).await.ok()?;
        let mut reply = [0_u8; 7];
        stream.read_exact(&mut reply).await.ok()?;
        Some(is_pong(&reply))
    };
    matches!(
        tokio::time::timeout(CONNECT_LIMIT, ask).await,
        Ok(Some(true))
    )
}

/// The test binary stands in for `valkey-server`, run by a script at
/// `bin/valkey-server`: macOS and Linux, the systems Marsa runs Valkey on
/// for now.
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::sync::Arc;

    use marsa_core::config::{self, ConfigPaths, GlobalConfig, ServerSettings};
    use marsa_core::databases::valkey::valkey_conf;
    use marsa_core::orphan::{Liveness, PidRecord};
    use marsa_core::platform::process::{self, Signal};
    use marsa_core::types::{ServiceAction, ServiceState, ServiceStatus};
    use tempfile::TempDir;

    use super::*;
    use crate::daemon::databases::ActionError;
    use crate::daemon::databases::orphan;
    use crate::daemon::databases::tests::server::{Orphan, listening};
    use crate::daemon::databases::tests::{FakeProbes, alive, free_port, probes};
    use crate::daemon::services::probe::ProcessProbe;
    use crate::daemon::state::Daemon;
    use crate::daemon::supervisor::STOP_MARGIN;

    const FAKE: &str = "MARSA_FAKE_VALKEY";
    const FAKE_CONF: &str = "MARSA_FAKE_VALKEY_CONF";
    const WAIT: Duration = Duration::from_secs(30);

    /// Not a check: the fake server's entry point. In a normal test run the
    /// variable is unset and this returns at once.
    ///
    /// It reads `port` and `dir` from the `valkey.conf` it was given, answers
    /// `PING` with `+PONG`, and at `SHUTDOWN SAVE` writes `dump.rdb` in
    /// `dir` and exits, as Valkey does. At SIGTERM it exits too. It prints
    /// each stop request it gets.
    #[test]
    fn fake_valkey() {
        if std::env::var(FAKE).is_err() {
            return;
        }
        let conf = fs::read_to_string(std::env::var(FAKE_CONF).unwrap()).unwrap();
        let setting = |name: &str| {
            let found = conf.lines().find_map(|line| line.strip_prefix(name));
            found.unwrap().trim_matches('"').to_owned()
        };
        let port: u16 = setting("port ").parse().unwrap();
        let dump = Path::new(&setting("dir ")).join("dump.rdb");
        let serve = async {
            use tokio::signal::unix::{SignalKind, signal};
            let mut term = signal(SignalKind::terminate()).unwrap();
            let address = (Ipv4Addr::LOCALHOST, port);
            let listener = tokio::net::TcpListener::bind(address).await.unwrap();
            println!("fake ready");
            loop {
                tokio::select! {
                    _ = term.recv() => {
                        println!("fake got SIGTERM");
                        std::process::exit(0);
                    }
                    Ok((mut stream, _)) = listener.accept() => {
                        let dump = dump.clone();
                        tokio::spawn(async move {
                            let mut request = [0_u8; 64];
                            let read = stream.read(&mut request).await.unwrap_or(0);
                            let request = &request[..read];
                            if request == command(&["PING"]) {
                                let _ = stream.write_all(b"+PONG\r\n").await;
                            } else if request == command(&["SHUTDOWN", "SAVE"]) {
                                println!("fake got SHUTDOWN SAVE");
                                fs::write(&dump, "REDIS0011").unwrap();
                                std::process::exit(0);
                            }
                        });
                    }
                }
            }
        };
        tokio::runtime::Runtime::new().unwrap().block_on(serve);
    }

    struct Vk {
        dir: TempDir,
        daemon: Arc<Daemon>,
        port: u16,
        probes: Arc<FakeProbes>,
        dirs: ValkeyDirs,
        server: PathBuf,
    }

    /// A daemon whose Valkey 8.1.11 is the fake, with `[valkey]` on, on a
    /// free port, in the data folder `name`. The script writes to
    /// `calls.log`.
    fn vk_in(name: &str, probes: FakeProbes) -> Vk {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join(name);
        let server = data.join("runtimes/valkey/8.1.11/bin/valkey-server");
        fs::create_dir_all(server.parent().unwrap()).unwrap();
        let calls = dir.path().join("calls.log").display().to_string();
        let exe = std::env::current_exe().unwrap().display().to_string();
        let script = format!(
            "#!/bin/sh\necho \"server $*\" >> '{calls}'\n\
             {FAKE}=serve {FAKE_CONF}=\"$1\" exec '{exe}' \
             daemon::databases::valkey::tests::fake_valkey --exact --nocapture\n"
        );
        fs::write(&server, script).unwrap();
        fs::set_permissions(&server, fs::Permissions::from_mode(0o755)).unwrap();
        let port = free_port();
        let global = GlobalConfig {
            valkey: ServerSettings {
                default: Some("8.1".to_owned()),
                enabled: true,
                port: Some(port),
            },
            ..GlobalConfig::default()
        };
        config::save_global(&ConfigPaths::in_dir(&data).global, &global).unwrap();
        let probes = Arc::new(probes);
        let mut daemon = Daemon::new(data.clone());
        daemon.valkey.probes = Arc::clone(&probes) as Arc<dyn ProcessProbe>;
        Vk {
            dir,
            daemon: Arc::new(daemon),
            port,
            probes,
            dirs: ValkeyDirs::new(&data),
            server,
        }
    }

    fn vk(probes: FakeProbes) -> Vk {
        vk_in("data", probes)
    }

    impl Vk {
        async fn act(&self, action: ServiceAction) -> Result<ServiceStatus, ActionError> {
            let valkey = &self.daemon.valkey;
            valkey.run_waiting(&self.daemon, action, WAIT).await
        }

        fn calls(&self) -> Vec<String> {
            let text = fs::read_to_string(self.dir.path().join("calls.log"));
            text.unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn texts(&self) -> Vec<String> {
            let lines = self.daemon.valkey.recent_logs();
            lines.into_iter().map(|line| line.text).collect()
        }

        fn count(&self, text: &str) -> usize {
            self.texts().iter().filter(|line| *line == text).count()
        }

        /// What `run/valkey.pid` names now.
        fn recorded(&self) -> Option<PidRecord> {
            let path = orphan::pid_path(KIND, &self.daemon.data_dir);
            process::read_pid_file(&path)
        }
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// The system the tests run on, as the manager sees it.
    fn os() -> Os {
        marsa_core::recipes::Platform::current()
            .expect("a known system")
            .os()
    }

    #[tokio::test]
    async fn a_start_writes_its_config_and_a_stop_sends_shutdown_save() {
        let vk = vk(probes(Liveness::Gone, None));
        // The fake runs as our installed `valkey-server`.
        *vk.probes.now.lock().unwrap() = alive(&vk.server);
        let status = vk.act(ServiceAction::Start).await.unwrap();
        assert_eq!(status.state, ServiceState::Running);
        let config = vk.dirs.config();
        assert_eq!(vk.calls(), [format!("server {}", config.display())]);
        assert_eq!(
            fs::read_to_string(&config).unwrap(),
            valkey_conf(&vk.dirs, vk.port, os()).unwrap()
        );
        assert_eq!(mode(&config), 0o600);
        assert_eq!(mode(&vk.dirs.data), 0o700);
        assert!(answers(vk.port).await);
        let program = vk.daemon.valkey.running_program(&vk.daemon).unwrap();
        assert_eq!(program, vk.server);
        // A shutdown gives it the time of its whole stop.
        let limit = vk.daemon.procs.lock().unwrap()[KIND.id()].stop_limit;
        assert_eq!(limit, valkey_stop(vk.port).total_wait() + STOP_MARGIN);
        let recorded = vk.recorded().expect("its pid is recorded");
        assert_eq!(Some(recorded.pid), status.pid);

        let begun = std::time::Instant::now();
        let status = vk.act(ServiceAction::Stop).await.unwrap();
        assert_eq!(status.state, ServiceState::Stopped);
        // It took the request: no wait, and no signal.
        assert!(
            begun.elapsed() < Duration::from_secs(5),
            "{:?}",
            begun.elapsed()
        );
        assert_eq!(vk.count("fake got SHUTDOWN SAVE"), 1);
        assert_eq!(vk.count("fake got SIGTERM"), 0);
        assert_eq!(
            fs::read_to_string(vk.dirs.data.join("dump.rdb")).unwrap(),
            "REDIS0011"
        );
        assert_eq!(vk.recorded(), None, "the end of the run deletes it");
        let log = fs::read_to_string(vk.daemon.data_dir.join("logs/valkey.log")).unwrap();
        assert!(log.contains("fake got SHUTDOWN SAVE"), "{log}");
    }

    /// Only PostgreSQL refuses an administrator's token: an elevated
    /// `marsad` runs Valkey.
    #[tokio::test]
    async fn an_elevated_daemon_still_starts_valkey() {
        let mut vk = vk(probes(Liveness::Gone, None));
        let daemon = Arc::get_mut(&mut vk.daemon).expect("one owner");
        daemon.valkey.elevated = || true;
        let status = vk.act(ServiceAction::Start).await.unwrap();
        assert_eq!(status.state, ServiceState::Running);
        vk.act(ServiceAction::Stop).await.unwrap();
    }

    /// Homebrew's Redis on the port: nothing starts, nothing is written, and
    /// nothing is stopped.
    #[tokio::test]
    async fn a_busy_port_starts_nothing_and_writes_nothing() {
        let vk = vk(probes(Liveness::Gone, Some("redis-server(901)")));
        let err = vk.act(ServiceAction::Start).await.unwrap_err();
        assert_eq!(err.code(), "service_port_busy");
        assert!(err.to_string().contains("redis-server(901)"), "{err}");
        let failed = ServiceState::Failed {
            code: "service_port_busy".into(),
        };
        assert_eq!(vk.daemon.valkey.status().state, failed);
        assert!(vk.calls().is_empty());
        assert!(!vk.dirs.data.exists());
        assert!(!vk.dirs.config().exists());
        assert!(vk.probes.signals.lock().unwrap().is_empty());

        // A listener on `127.0.0.1:<port>` itself, and no lsof (Linux).
        let vk = self::vk(probes(Liveness::Gone, None));
        let held = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, vk.port)).unwrap();
        let err = vk.act(ServiceAction::Start).await.unwrap_err();
        assert_eq!(err.code(), "service_port_busy");
        assert!(err.to_string().contains("another program"), "{err}");
        assert!(vk.calls().is_empty());
        drop(held);
    }

    /// A data folder whose path `valkey.conf` cannot hold: refused before
    /// anything else, and the status says why.
    #[tokio::test]
    async fn a_data_folder_path_it_cannot_hold_is_refused() {
        let vk = vk_in("a\"b", probes(Liveness::Gone, None));
        let err = vk.act(ServiceAction::Start).await.unwrap_err();
        assert_eq!(err.code(), "valkey_path_unsupported");
        let failed = ServiceState::Failed {
            code: "valkey_path_unsupported".into(),
        };
        assert_eq!(vk.daemon.valkey.status().state, failed);
        assert!(vk.calls().is_empty());
        assert!(!vk.dirs.config().exists());
    }

    /// A listener that does not answer `PING`, or answers it with an error,
    /// is not Valkey ready.
    #[tokio::test]
    async fn readiness_is_pong_not_the_port() {
        let silent = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        assert!(!answers(silent.local_addr().unwrap().port()).await);
        let loading = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = loading.local_addr().unwrap().port();
        let reply = tokio::spawn(async move {
            let (mut stream, _) = loading.accept().await.unwrap();
            let mut request = [0_u8; 14];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(request.as_slice(), command(&["PING"]));
            let error = b"-LOADING Valkey is loading the dataset in memory\r\n";
            stream.write_all(error).await.unwrap();
        });
        assert!(!answers(port).await);
        reply.await.unwrap();
        assert!(!answers(free_port()).await);
    }

    /// A `marsad` that died left our Valkey running: the next start stops
    /// it with SIGTERM, which saves its data (`shutdown-on-sigterm save`),
    /// and never with `SHUTDOWN SAVE` on the port, which could reach
    /// another program. Then a new server starts.
    #[tokio::test]
    async fn our_orphan_gets_sigterm_and_no_request() {
        let vk = vk(probes(Liveness::Gone, None));
        // The old server, on the same port, as `marsad` started it.
        fs::create_dir_all(&vk.dirs.run).unwrap();
        fs::create_dir_all(&vk.dirs.data).unwrap();
        let conf = vk.dir.path().join("old.conf");
        fs::write(&conf, valkey_conf(&vk.dirs, vk.port, os()).unwrap()).unwrap();
        let log = fs::File::create(vk.dir.path().join("orphan.log")).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "daemon::databases::valkey::tests::fake_valkey",
                "--exact",
                "--nocapture",
            ])
            .env(FAKE, "serve")
            .env(FAKE_CONF, &conf)
            .stdin(std::process::Stdio::null())
            .stdout(log)
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut orphan = Orphan(child);
        listening(vk.port).await;
        let pid = orphan.0.id();
        vk.probes
            .orphans
            .lock()
            .unwrap()
            .push((pid, vk.server.clone()));
        let started = loop {
            if let Liveness::Alive { started, .. } = process::identity(pid).await {
                break started;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let record = PidRecord {
            pid,
            exe: vk.server.clone(),
            started,
        };
        process::write_pid_file(&orphan::pid_path(KIND, &vk.daemon.data_dir), &record).unwrap();
        // The new server runs as our installed `valkey-server`.
        *vk.probes.now.lock().unwrap() = alive(&vk.server);

        let status = vk.act(ServiceAction::Start).await.unwrap();
        assert_eq!(status.state, ServiceState::Running);
        let gone = orphan.0.wait().unwrap();
        assert!(gone.success(), "it ended at SIGTERM: {gone:?}");
        let said = fs::read_to_string(vk.dir.path().join("orphan.log")).unwrap();
        assert!(said.contains("fake got SIGTERM"), "{said}");
        assert!(!said.contains("SHUTDOWN SAVE"), "{said}");
        let signals = vk.probes.signals.lock().unwrap().clone();
        assert_eq!(signals, [(pid, Signal::Term)]);
        let line = "an earlier marsad left running; it ended after SIGTERM";
        assert!(
            vk.texts().iter().any(|t| t.contains(line)),
            "{:?}",
            vk.texts()
        );
        vk.act(ServiceAction::Stop).await.unwrap();
    }

    #[test]
    fn valkey_runs_with_its_config_file_and_stops_with_shutdown_save() {
        let dirs = ValkeyDirs::new(Path::new("/m"));
        let program = PathBuf::from("/m/runtimes/valkey/8.1.11/bin/valkey-server");
        let spec = spec(program.clone(), &dirs, 6380, Os::Mac).unwrap();
        assert_eq!(spec.id, "valkey");
        assert_eq!(spec.program, program);
        assert_eq!(spec.args, ["/m/run/valkey.conf"]);
        assert_eq!(spec.stop, valkey_stop(6380));
        assert_eq!(spec.restart, RestartPolicy::Never);
        assert!(spec.env.is_empty() && spec.cwd.is_none());
    }
}
