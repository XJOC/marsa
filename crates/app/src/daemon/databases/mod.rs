//! The database servers of `marsad` (ADR 0023), on macOS and Linux:
//! PostgreSQL, MySQL, and Valkey. `marsa_core::databases` decides; this
//! module runs the programs.
//!
//! The first start of a line makes its data folder ([`init::ensure_data`]):
//! for PostgreSQL its cluster in `databases/postgres/<major>/`
//! ([`postgres::ensure_cluster`]). All of it happens in a temp folder next to
//! the target, which is renamed into place at the end, so a data folder that
//! is half made is never taken for a ready one.
//!
//! Before each start, [`facts::start_facts`] reads the port and the
//! server's own pid file (`postmaster.pid`) from the system for
//! `decide_start`. It only reads: nothing here signals a process or deletes
//! that file.
//!
//! [`ServerManager`] starts and stops one server under the supervisor, one
//! action at a time. A failed start leaves the status `failed`, with the
//! reason in the recent lines. A run that ends by itself before it takes
//! connections is read once: if PostgreSQL refused because a live process
//! holds one of its lock files, the start fails as the check before it
//! would have. Nothing reads the log after the server is ready.
//!
//! [`start_at_launch`] starts a server with `marsad` when its table in
//! `global.toml` says `enabled = true` (ADR 0023: a server that is on
//! starts with the daemon). A failure leaves the daemon running and the
//! status `failed`.

mod facts;
mod init;
mod manager;
mod mysql;
mod orphan;
mod postgres;
mod settings;
mod valkey;

pub use init::InitError;
pub use manager::{ActionError, ServerManager};
pub use settings::{start_after_install, start_at_launch};

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::net::{Ipv4Addr, TcpListener};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use marsa_core::config::{self, ConfigPaths, ServerSettings};
    use marsa_core::databases::{
        PidFileCheck, PortCheck, PostgresDirs, ServerKind, ServiceBusy, StartDecision,
        decide_start, postgres_args, postgres_stop, postmaster_pid,
    };
    use marsa_core::orphan::Liveness;
    use marsa_core::platform::process::{self, Signal};
    use marsa_core::ports::PortLookup;
    use marsa_core::recipes::Platform;
    use marsa_core::runtimes::{Runtime, RuntimesDir};
    use marsa_core::services::RestartPolicy;
    use marsa_core::types::ServiceState;
    use semver::Version;

    use super::facts::start_facts;
    use super::init::installed_servers;
    use super::postgres::{programs, spec as postgres_spec};
    use super::settings::{after_install, first_install};
    use crate::daemon::caddy::Launched;
    use crate::daemon::services::probe::{BoxFuture, ProcessProbe};
    use crate::daemon::state::Daemon;

    // Only the `server` and `init` modules below use these, and they run
    // fake programs through shell scripts, so on Unix only.
    #[cfg(unix)]
    use {
        super::postgres::{Programs, SSL_REQUEST, ensure_cluster},
        crate::daemon::supervisor::lock,
        marsa_core::databases::{PG_HBA_CONF, ROOT_ROLE_SQL, initdb_args, single_user_args},
        marsa_core::services::StopMethod,
        marsa_core::types::{ServiceAction, ServiceStatus},
        tokio::io::AsyncWriteExt as _,
    };

    /// Answers what it was given, and keeps the pids it was asked about.
    #[derive(Debug)]
    pub(super) struct FakeProbes {
        /// What runs at every pid.
        pub(super) now: Mutex<Liveness>,
        lookup: PortLookup,
        pub(super) asked: Mutex<Vec<u32>>,
        /// Real processes that run as these programs: their real start
        /// time and life, with this path. Only they get real signals.
        pub(super) orphans: Mutex<Vec<(u32, PathBuf)>>,
        /// Every signal asked for, to any pid.
        pub(super) signals: Mutex<Vec<(u32, Signal)>>,
        /// After the first signal, the orphans cannot be told any more.
        unknown_after_signal: Mutex<bool>,
    }

    /// `owner` listens on the port, or nobody.
    pub(super) fn probes(now: Liveness, owner: Option<&str>) -> FakeProbes {
        let lookup = owner.map_or(PortLookup::Nobody, |owner| PortLookup::Owner(owner.into()));
        FakeProbes {
            now: Mutex::new(now),
            lookup,
            asked: Mutex::default(),
            orphans: Mutex::default(),
            signals: Mutex::default(),
            unknown_after_signal: Mutex::default(),
        }
    }

    impl ProcessProbe for FakeProbes {
        fn real_identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
            self.asked.lock().unwrap().push(pid);
            let orphan = self
                .orphans
                .lock()
                .unwrap()
                .iter()
                .find(|(p, _)| *p == pid)
                .cloned();
            let now = self.now.lock().unwrap().clone();
            let unknown = *self.unknown_after_signal.lock().unwrap()
                && !self.signals.lock().unwrap().is_empty();
            Box::pin(async move {
                let Some((_, exe)) = orphan else {
                    return now;
                };
                if unknown {
                    return Liveness::Unknown;
                }
                match process::identity(pid).await {
                    Liveness::Alive { started, .. } => Liveness::Alive { exe, started },
                    other => other,
                }
            })
        }

        fn port_lookup(&self, _port: u16) -> BoxFuture<'_, PortLookup> {
            Box::pin(std::future::ready(self.lookup.clone()))
        }

        fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>> {
            self.signals.lock().unwrap().push((pid, signal));
            let real = self.orphans.lock().unwrap().iter().any(|(p, _)| *p == pid);
            Box::pin(async move {
                if real {
                    process::send_signal(pid, signal).await
                } else {
                    Ok(())
                }
            })
        }
    }

    pub(super) fn alive(exe: &Path) -> Liveness {
        Liveness::Alive {
            exe: exe.to_path_buf(),
            started: "Wed Nov 18 10:00:00 2026".parse().expect("a start time"),
        }
    }

    /// A port nothing listens on now.
    pub(super) fn free_port() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    const POSTMASTER_PID: &str = "4242\n/m/databases/postgres/18\n1763460000\n5432\n/m/run\n\
                                  127.0.0.1\n  5432001         0\nready   \n";

    #[tokio::test]
    async fn the_port_is_busy_when_the_bind_fails_or_lsof_names_an_owner() {
        let data = tempfile::tempdir().unwrap();
        let facts = async |port, owner| {
            let probes = probes(Liveness::Gone, owner);
            start_facts(
                &probes,
                port,
                &data.path().join("postmaster.pid"),
                postmaster_pid,
                &[],
            )
            .await
            .port_check
        };
        let port = free_port();
        assert_eq!(facts(port, None).await, PortCheck::Free);
        // Docker on `*:<port>`: our bind works on macOS, but lsof sees it.
        let docker = Some("com.docker.backend(77)");
        let busy = PortCheck::Busy {
            owner: docker.map(str::to_owned),
        };
        assert_eq!(facts(port, docker).await, busy);
        // A listener on `127.0.0.1:<port>`, and no lsof (Linux).
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let busy = PortCheck::Busy { owner: None };
        assert_eq!(facts(port, None).await, busy);
    }

    /// macOS: when lsof fails or takes too long, a holder of `*:<port>`
    /// could be there unseen, so there is no start, and it says why.
    #[tokio::test]
    async fn a_failed_look_at_the_port_refuses_as_unknown() {
        let data = tempfile::tempdir().unwrap();
        let mut failed = probes(Liveness::Gone, None);
        failed.lookup = PortLookup::Failed;
        let port = free_port();
        let facts = start_facts(
            &failed,
            port,
            &data.path().join("postmaster.pid"),
            postmaster_pid,
            &[],
        )
        .await;
        assert_eq!(facts.port_check, PortCheck::Unknown);
        let refused = StartDecision::Refuse(ServiceBusy::PortUnknown { port });
        assert_eq!(decide_start(&facts), refused);
    }

    #[tokio::test]
    async fn postmaster_pid_is_read_and_its_pid_classified() {
        let data = tempfile::tempdir().unwrap();
        let servers = [data.path().join("runtimes/postgres/18.7.0/bin/postgres")];
        let check = async |now: Liveness| {
            let probes = probes(now, None);
            let facts = start_facts(
                &probes,
                free_port(),
                &data.path().join("postmaster.pid"),
                postmaster_pid,
                &servers,
            )
            .await;
            (facts.pid_file, probes.asked.into_inner().unwrap())
        };
        // No file: nobody is asked about any pid.
        let gone = || Liveness::Gone;
        assert_eq!(check(gone()).await, (PidFileCheck::NoPid, vec![]));

        let file = data.path().join("postmaster.pid");
        std::fs::write(&file, POSTMASTER_PID).unwrap();
        let pid = 4242;
        let server = check(alive(&servers[0])).await;
        assert_eq!(server, (PidFileCheck::Server { pid }, vec![pid]));
        let other = check(alive(Path::new("/bin/sleep"))).await.0;
        assert_eq!(other, PidFileCheck::Other { pid });
        assert_eq!(check(gone()).await.0, PidFileCheck::Dead { pid });
        let unknown = check(Liveness::Unknown).await.0;
        assert_eq!(unknown, PidFileCheck::Unknown { pid });

        // Junk, and a file too long to be PostgreSQL's: no pid.
        let long = format!("{POSTMASTER_PID}{}", "x".repeat(5000));
        for text in ["", "abc\n", "0\n", long.as_str()] {
            std::fs::write(&file, text).unwrap();
            assert_eq!(
                check(alive(&servers[0])).await,
                (PidFileCheck::NoPid, vec![])
            );
        }
    }

    /// A link is never followed, even to a real `postmaster.pid`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_linked_postmaster_pid_names_no_pid() {
        let data = tempfile::tempdir().unwrap();
        let real = data.path().join("elsewhere.pid");
        std::fs::write(&real, POSTMASTER_PID).unwrap();
        std::os::unix::fs::symlink(&real, data.path().join("postmaster.pid")).unwrap();
        let probes = probes(Liveness::Gone, None);
        let facts = start_facts(
            &probes,
            free_port(),
            &data.path().join("postmaster.pid"),
            postmaster_pid,
            &[],
        )
        .await;
        assert_eq!(facts.pid_file, PidFileCheck::NoPid);
        assert!(probes.asked.into_inner().unwrap().is_empty());
    }

    /// The facts as the system gives them, through `decide_start`.
    #[tokio::test]
    async fn real_facts_lead_to_their_decision() {
        let data = tempfile::tempdir().unwrap();
        let ours = data.path().join("runtimes/postgres/17.12.0/bin/postgres");
        let servers = [ours.clone()];
        std::fs::write(data.path().join("postmaster.pid"), POSTMASTER_PID).unwrap();
        let decide = async |port, now, owner| {
            let probes = probes(now, owner);
            decide_start(
                &start_facts(
                    &probes,
                    port,
                    &data.path().join("postmaster.pid"),
                    postmaster_pid,
                    &servers,
                )
                .await,
            )
        };
        // Our older version still runs on the folder: refused, whatever
        // holds the port.
        let busy = StartDecision::Refuse(ServiceBusy::DataDir { pid: Some(4242) });
        assert_eq!(decide(free_port(), alive(&ours), None).await, busy);
        // A stale file, or a pid that another program has now: PostgreSQL
        // decides at its start.
        let sleep = alive(Path::new("/bin/sleep"));
        assert_eq!(decide(free_port(), sleep, None).await, StartDecision::Start);
        assert_eq!(
            decide(free_port(), Liveness::Gone, None).await,
            StartDecision::Start
        );
        // Homebrew's PostgreSQL holds the port.
        let port = free_port();
        let owner = Some("postgres(901)");
        let refused = StartDecision::Refuse(ServiceBusy::Port {
            port,
            owner: owner.map(str::to_owned),
        });
        assert_eq!(decide(port, Liveness::Gone, owner).await, refused);
    }

    #[test]
    fn installed_servers_are_each_versions_postgres() {
        let dir = tempfile::tempdir().unwrap();
        let platform = Platform::current().unwrap();
        let version = |text: &str| Version::parse(text).unwrap();
        let mut want = Vec::new();
        for text in ["17.12.0", "18.7.0"] {
            let programs = programs(dir.path(), platform, &version(text)).unwrap();
            std::fs::create_dir_all(programs.postgres.parent().unwrap()).unwrap();
            std::fs::write(&programs.postgres, "").unwrap();
            assert!(
                programs
                    .initdb
                    .starts_with(programs.postgres.parent().unwrap())
            );
            want.push(programs.postgres);
        }
        let servers = installed_servers(ServerKind::Postgres, dir.path(), platform);
        for program in &want {
            assert!(servers.contains(program), "{servers:?}");
        }
        let mut names = servers.iter().filter_map(|path| path.file_stem());
        assert!(names.all(|name| name == "postgres"));
    }

    /// A broken `global.toml` without PostgreSQL it
    /// leaves PostgreSQL's status and lines alone; with one, the status
    /// says why it did not start.
    #[tokio::test]
    async fn a_broken_global_toml_fails_postgres_only_when_one_is_installed() {
        let dir = tempfile::tempdir().unwrap();
        let global = ConfigPaths::in_dir(dir.path()).global;
        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        std::fs::write(&global, "[postgres\n").unwrap();
        let daemon = Arc::new(Daemon::new(dir.path().to_path_buf()));
        let launched = start_at_launch(&daemon, ServerKind::Postgres)
            .wait(Duration::from_secs(5))
            .await;
        assert_eq!(launched, Some(Launched::NotInstalled));
        assert_eq!(daemon.postgres.status().state, ServiceState::Stopped);
        assert!(daemon.postgres.recent_logs().is_empty());

        let version =
            RuntimesDir::in_dir(dir.path()).version_dir(Runtime::Postgres, &Version::new(18, 7, 0));
        std::fs::create_dir_all(version).unwrap();
        let launched = start_at_launch(&daemon, ServerKind::Postgres)
            .wait(Duration::from_secs(5))
            .await;
        assert_eq!(launched, Some(Launched::Failed("config_invalid")));
        let failed = ServiceState::Failed {
            code: "config_invalid".into(),
        };
        assert_eq!(daemon.postgres.status().state, failed);
        assert_eq!(daemon.postgres.recent_logs().len(), 1);
    }

    #[test]
    fn postgres_runs_on_loopback_and_stops_with_sigint_then_sigquit() {
        let dirs = PostgresDirs::new(Path::new("/m"), 18);
        let program = PathBuf::from("/m/runtimes/postgres/18.7.0/bin/postgres");
        let spec = postgres_spec(program.clone(), &dirs, 5433);
        assert_eq!(spec.id, ServerKind::Postgres.id());
        assert_eq!(spec.program, program);
        assert_eq!(spec.args, postgres_args(&dirs, 5433));
        assert_eq!(spec.stop, postgres_stop());
        assert_eq!(spec.restart, RestartPolicy::Never);
        assert!(spec.env.is_empty() && spec.cwd.is_none());
    }

    /// The test binary stands in for the `postgres` server, run by a script
    /// at `bin/postgres` with a fake `initdb` next to it: macOS and Linux
    /// only, the systems of this task.
    #[cfg(unix)]
    pub(super) mod server {
        use super::*;
        use std::fs;
        use std::os::unix::fs::PermissionsExt as _;

        use marsa_core::config::GlobalConfig;
        use marsa_core::orphan::PidRecord;
        use marsa_core::ps::StartTime;
        use tempfile::TempDir;
        use tokio::task::JoinHandle;

        use crate::daemon::supervisor::STOP_MARGIN;

        const FAKE: &str = "MARSA_FAKE_POSTGRES";
        /// Generous: a test binary can start slowly on a busy CI machine.
        const WAIT: Duration = Duration::from_secs(30);
        /// The program the pid of the lock file runs now.
        const OTHER: &str = "/Applications/Other.app/Contents/MacOS/Other";

        /// Not a check: the fake server's entry point. In a normal test run
        /// the variable is unset and this returns at once.
        ///
        /// - `serve` listens on the `port=` of its arguments, answers an
        ///   `SSLRequest` with `N`, prints each stop signal, ignores
        ///   SIGTERM, and exits at SIGINT, as PostgreSQL does with a client
        ///   connected.
        /// - `noisy` serves too, but prints PostgreSQL's refusal for
        ///   `postmaster.pid` first, and again after its first answer.
        /// - `locked` prints that refusal, held by pid 4242, and exits 1.
        /// - `locked-no-hint` prints the refusal alone, and exits 1.
        /// - `locked-hang` prints the refusal, then waits for a signal: a
        ///   shutdown stops it.
        /// - `socket-locked` does the same as `locked` for the lock of its
        ///   socket.
        /// - `mute` listens and never answers, so a start waits for it.
        /// - `deaf` serves, and ignores SIGINT and SIGQUIT too: only SIGKILL
        ///   ends it.
        #[test]
        fn fake_postgres() {
            let Ok(mode) = std::env::var(FAKE) else {
                return;
            };
            let args = std::env::var("MARSA_FAKE_POSTGRES_ARGS").unwrap();
            let setting = |name: &str| {
                let found = args.split(' ').find_map(|arg| arg.strip_prefix(name));
                found.unwrap().to_owned()
            };
            let port: u16 = setting("port=").parse().unwrap();
            let at = format!("2026-11-18 10:00:00.123 UTC [{}]", std::process::id());
            let data_dir = || {
                eprintln!("{at} FATAL:  lock file \"postmaster.pid\" already exists");
                eprintln!(
                    "{at} HINT:  Is another postmaster (PID 4242) running in data directory \
                     \"/m\"?"
                );
            };
            match mode.as_str() {
                "locked" => {
                    data_dir();
                    std::process::exit(1);
                }
                "locked-no-hint" => {
                    eprintln!("{at} FATAL:  lock file \"postmaster.pid\" already exists");
                    std::process::exit(1);
                }
                "locked-hang" => {
                    data_dir();
                    let hang = async {
                        use tokio::signal::unix::{SignalKind, signal};
                        let mut int = signal(SignalKind::interrupt()).unwrap();
                        int.recv().await;
                        std::process::exit(0);
                    };
                    tokio::runtime::Runtime::new().unwrap().block_on(hang);
                }
                "socket-locked" => {
                    let socket = format!("{}/.s.PGSQL.{port}", setting("unix_socket_directories="));
                    eprintln!("{at} FATAL:  lock file \"{socket}.lock\" already exists");
                    eprintln!(
                        "{at} HINT:  Is another postmaster (PID 4242) using socket file \
                         \"{socket}\"?"
                    );
                    std::process::exit(1);
                }
                "noisy" => data_dir(),
                _ => {}
            }
            let serve = async {
                use tokio::io::AsyncReadExt as _;
                use tokio::signal::unix::{SignalKind, signal};
                let mut term = signal(SignalKind::terminate()).unwrap();
                let mut int = signal(SignalKind::interrupt()).unwrap();
                let mut quit = signal(SignalKind::quit()).unwrap();
                let address = (Ipv4Addr::LOCALHOST, port);
                let listener = tokio::net::TcpListener::bind(address).await.unwrap();
                eprintln!("fake ready");
                let mut answered = false;
                loop {
                    tokio::select! {
                        _ = term.recv() => eprintln!("fake got SIGTERM"),
                        _ = int.recv() => {
                            eprintln!("fake got SIGINT");
                            if mode != "deaf" {
                                std::process::exit(0);
                            }
                        }
                        _ = quit.recv() => {
                            eprintln!("fake got SIGQUIT");
                            if mode != "deaf" {
                                std::process::exit(0);
                            }
                        }
                        Ok((mut stream, _)) = listener.accept(), if mode != "mute" => {
                            let mut request = [0_u8; 8];
                            if stream.read_exact(&mut request).await.is_ok()
                                && request == SSL_REQUEST
                            {
                                let _ = stream.write_all(b"N").await;
                                if mode == "noisy" && !answered {
                                    data_dir();
                                }
                                answered = true;
                            }
                        }
                    }
                }
            };
            tokio::runtime::Runtime::new().unwrap().block_on(serve);
        }

        /// A daemon whose PostgreSQL 18.7.0 is the fake in `mode`, with
        /// `[postgres]` on, on a free port. The fakes write to `calls.log`.
        struct Pg {
            dir: TempDir,
            daemon: Arc<Daemon>,
            port: u16,
            probes: Arc<FakeProbes>,
            dirs: PostgresDirs,
        }

        fn pg(mode: &str, probes: FakeProbes) -> Pg {
            pg_in("data", mode, probes)
        }

        /// [`pg`] with the daemon's data folder named `name`.
        fn pg_in(name: &str, mode: &str, probes: FakeProbes) -> Pg {
            let dir = tempfile::tempdir().unwrap();
            let data = dir.path().join(name);
            let platform = Platform::current().unwrap();
            let version = Version::new(18, 7, 0);
            let programs = programs(&data, platform, &version).unwrap();
            fs::create_dir_all(programs.postgres.parent().unwrap()).unwrap();
            let calls = dir.path().join("calls.log").display().to_string();
            let initdb = format!(
                "#!/bin/sh\necho initdb >> '{calls}'\nmkdir \"$2\" || exit 3\n\
                 echo 18 > \"$2/PG_VERSION\"\necho x > \"$2/pg_hba.conf\"\n"
            );
            let exe = std::env::current_exe().unwrap().display().to_string();
            let postgres = format!(
                "#!/bin/sh\nif [ \"$1\" = --single ]; then cat > /dev/null; \
                 echo single >> '{calls}'; exit 0; fi\necho \"server $*\" >> '{calls}'\n\
                 {FAKE}='{mode}' MARSA_FAKE_POSTGRES_ARGS=\"$*\" exec '{exe}' \
                 daemon::databases::tests::server::fake_postgres --exact --nocapture\n"
            );
            for (path, script) in [(&programs.initdb, initdb), (&programs.postgres, postgres)] {
                fs::write(path, script).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            let port = free_port();
            set_postgres(&data, Some("18"), true, port);
            let probes = Arc::new(probes);
            let mut daemon = Daemon::new(data.clone());
            daemon.postgres.probes = Arc::clone(&probes) as Arc<dyn ProcessProbe>;
            Pg {
                dir,
                daemon: Arc::new(daemon),
                port,
                probes,
                dirs: PostgresDirs::new(&data, 18),
            }
        }

        fn set_postgres(data: &Path, default: Option<&str>, enabled: bool, port: u16) {
            let global = GlobalConfig {
                postgres: ServerSettings {
                    default: default.map(str::to_owned),
                    enabled,
                    port: Some(port),
                },
                ..GlobalConfig::default()
            };
            config::save_global(&ConfigPaths::in_dir(data).global, &global).unwrap();
        }

        impl Pg {
            async fn act(&self, action: ServiceAction) -> Result<ServiceStatus, ActionError> {
                let postgres = &self.daemon.postgres;
                postgres.run_waiting(&self.daemon, action, WAIT).await
            }

            fn calls(&self) -> Vec<String> {
                let text = fs::read_to_string(self.dir.path().join("calls.log"));
                text.unwrap_or_default()
                    .lines()
                    .map(str::to_owned)
                    .collect()
            }

            fn count(&self, call: &str) -> usize {
                let calls = self.calls();
                calls.iter().filter(|line| line.starts_with(call)).count()
            }

            fn texts(&self) -> Vec<String> {
                let lines = self.daemon.postgres.recent_logs();
                lines.into_iter().map(|line| line.text).collect()
            }

            /// Waits until the fake prints `text`.
            async fn wait_for(&self, text: &str) {
                let seen = async {
                    while !self.texts().iter().any(|line| line == text) {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                };
                tokio::time::timeout(WAIT, seen).await.expect(text);
            }
        }

        fn failed(code: &str) -> ServiceState {
            ServiceState::Failed { code: code.into() }
        }

        #[tokio::test]
        async fn a_start_makes_the_cluster_once_and_a_stop_sends_sigint_only() {
            let pg = pg("serve", probes(Liveness::Gone, None));
            let status = pg.act(ServiceAction::Start).await.unwrap();
            assert_eq!(status.state, ServiceState::Running);
            let args = postgres_args(&pg.dirs, pg.port).join(" ");
            assert_eq!(pg.calls(), ["initdb", "single", &format!("server {args}")]);
            assert!(pg.dirs.data.join("PG_VERSION").is_file());
            // It took a connection on the port, and runs the version's
            // program.
            let address = (Ipv4Addr::LOCALHOST, pg.port);
            tokio::net::TcpStream::connect(address).await.unwrap();
            let program = pg.daemon.postgres.running_program(&pg.daemon).unwrap();
            assert!(program.ends_with("18.7.0/bin/postgres"), "{program:?}");
            // A shutdown gives it the time of its whole ladder.
            let limit = pg.daemon.procs.lock().unwrap()[ServerKind::Postgres.id()].stop_limit;
            assert_eq!(limit, postgres_stop().total_wait() + STOP_MARGIN);

            // A start while it runs changes nothing; a restart makes no
            // second cluster.
            pg.act(ServiceAction::Start).await.unwrap();
            assert_eq!(pg.count("server"), 1);
            let status = pg.act(ServiceAction::Restart).await.unwrap();
            assert_eq!(status.state, ServiceState::Running);
            assert_eq!((pg.count("initdb"), pg.count("server")), (1, 2));

            pg.wait_for("fake ready").await;
            let status = pg.act(ServiceAction::Stop).await.unwrap();
            assert_eq!(status.state, ServiceState::Stopped);
            assert_eq!(pg.daemon.postgres.running_program(&pg.daemon), None);
            let texts = pg.texts();
            assert_eq!(texts.iter().filter(|t| *t == "fake got SIGINT").count(), 2);
            assert!(!texts.iter().any(|text| text == "fake got SIGTERM"));
            assert!(
                !texts
                    .iter()
                    .any(|text| text.contains("TCP connections only"))
            );
            let log = fs::read_to_string(pg.daemon.data_dir.join("logs/postgres.log")).unwrap();
            assert!(log.contains("fake got SIGINT"), "{log}");
        }

        /// No socket (a path too long, or a comma in it): TCP only, and the
        /// recent lines say why, and that the superuser cannot connect then.
        #[tokio::test]
        async fn a_data_folder_with_no_socket_says_why_and_that_the_superuser_is_out() {
            let deep = "d".repeat(100);
            for (folder, why) in [(deep.as_str(), "bytes long"), ("a,b", "a comma")] {
                let pg = pg_in(folder, "serve", probes(Liveness::Gone, None));
                let status = pg.act(ServiceAction::Start).await.unwrap();
                assert_eq!(status.state, ServiceState::Running);
                let server = pg.calls().pop().unwrap();
                assert!(server.contains(" -c unix_socket_directories= "), "{server}");
                let texts = pg.texts();
                let said = texts.iter().any(|text| {
                    text.contains(why)
                        && text.contains("TCP connections only")
                        && text.contains("superuser postgres")
                });
                assert!(said, "{texts:?}");
                pg.act(ServiceAction::Stop).await.unwrap();
            }
        }

        #[tokio::test]
        async fn a_busy_port_starts_nothing_and_makes_no_cluster() {
            // Homebrew's PostgreSQL on `127.0.0.1:<port>`.
            let pg = pg("serve", probes(Liveness::Gone, None));
            let held = TcpListener::bind((Ipv4Addr::LOCALHOST, pg.port)).unwrap();
            let err = pg.act(ServiceAction::Start).await.unwrap_err();
            assert_eq!(err.code(), "service_port_busy");
            let status = pg.daemon.postgres.status();
            assert_eq!(status.state, failed("service_port_busy"));
            let why = format!("port {} is in use by another program", pg.port);
            assert!(
                pg.texts().last().unwrap().contains(&why),
                "{:?}",
                pg.texts()
            );
            assert_eq!(pg.calls(), Vec::<String>::new());
            assert!(!pg.dirs.data.exists());
            drop(held);

            // Docker on `*:<port>`: the bind works, lsof names it.
            let pg = pg_with_owner();
            let err = pg.act(ServiceAction::Start).await.unwrap_err();
            assert_eq!(err.code(), "service_port_busy");
            assert!(err.to_string().contains("com.docker.backend(77)"), "{err}");
            assert_eq!(pg.calls(), Vec::<String>::new());
        }

        fn pg_with_owner() -> Pg {
            pg(
                "serve",
                probes(Liveness::Gone, Some("com.docker.backend(77)")),
            )
        }

        /// A data folder that `postmaster.pid` says is in use, by pid 4242.
        fn in_use(pg: &Pg) -> PathBuf {
            fs::create_dir_all(&pg.dirs.data).unwrap();
            let file = pg.dirs.data.join("postmaster.pid");
            fs::write(&file, POSTMASTER_PID).unwrap();
            file
        }

        #[tokio::test]
        async fn our_server_on_the_data_folder_refuses_at_once() {
            let pg = pg("serve", probes(Liveness::Gone, None));
            // Our own PostgreSQL 18.7.0 runs at the pid of the file.
            let data = &pg.daemon.data_dir;
            let platform = Platform::current().unwrap();
            let ours = programs(data, platform, &Version::new(18, 7, 0)).unwrap();
            *pg.probes.now.lock().unwrap() = alive(&ours.postgres);
            let file = in_use(&pg);
            let err = pg.act(ServiceAction::Start).await.unwrap_err();
            assert_eq!(err.code(), "service_data_dir_busy");
            assert!(err.to_string().contains("pid 4242"), "{err}");
            let state = pg.daemon.postgres.status().state;
            assert_eq!(state, failed("service_data_dir_busy"));
            assert_eq!(pg.calls(), Vec::<String>::new());
            assert_eq!(fs::read_to_string(file).unwrap(), POSTMASTER_PID);
        }

        /// The pid in `postmaster.pid` now runs another program: the check
        /// lets the start go, and PostgreSQL refuses by itself. The start
        /// reports it as the check before it would have.
        #[tokio::test]
        async fn a_lock_file_refusal_at_the_start_is_data_dir_busy_without_a_restart() {
            let pg = pg("locked", probes(alive(Path::new(OTHER)), None));
            let file = in_use(&pg);
            let err = pg.act(ServiceAction::Start).await.unwrap_err();
            let busy = "service_data_dir_busy";
            assert_eq!(err.code(), busy);
            assert!(err.to_string().contains("pid 4242"), "{err}");
            assert_eq!(pg.daemon.postgres.status().state, failed(busy));
            let last = pg.texts().pop().unwrap();
            assert!(last.contains("pid 4242"), "{last}");
            assert!(last.contains(OTHER), "{last}");
            // Asked about that pid (besides the run itself, for its
            // record), never signalled it, and its file stays.
            let asked = pg.probes.asked.lock().unwrap().clone();
            let about_4242 = asked.iter().filter(|pid| **pid == 4242).count();
            assert_eq!(about_4242, 2, "{asked:?}");
            assert!(signals(&pg).is_empty());
            assert_eq!(fs::read_to_string(&file).unwrap(), POSTMASTER_PID);
            // Longer than a restart delay would be: none came.
            tokio::time::sleep(Duration::from_millis(1500)).await;
            assert_eq!(pg.count("server"), 1);
            assert!(!pg.daemon.proc_alive(ServerKind::Postgres.id()));
            assert_eq!(pg.daemon.postgres.status().state, failed(busy));
        }

        /// No hint and no `postmaster.pid`: still the data folder, with the
        /// pid unknown.
        #[tokio::test]
        async fn a_lock_file_refusal_without_a_pid_is_still_data_dir_busy() {
            let pg = pg("locked-no-hint", probes(alive(Path::new(OTHER)), None));
            let err = pg.act(ServiceAction::Start).await.unwrap_err();
            assert_eq!(err.code(), "service_data_dir_busy");
            let text = err.to_string();
            assert_eq!(text, "the data folder is in use by a running server");
            let state = pg.daemon.postgres.status().state;
            assert_eq!(state, failed("service_data_dir_busy"));
            let last = pg.texts().pop().unwrap();
            assert!(last.contains("named no pid"), "{last}");
            assert!(pg.probes.asked.lock().unwrap().is_empty());
        }

        /// A shutdown stops the run during its start: its lines are not
        /// read, even with PostgreSQL's refusal among them.
        #[tokio::test]
        async fn a_run_that_a_shutdown_stopped_is_not_read() {
            let pg = pg("locked-hang", probes(alive(Path::new(OTHER)), None));
            let daemon = Arc::clone(&pg.daemon);
            let start = tokio::spawn(async move {
                let postgres = &daemon.postgres;
                postgres
                    .run_waiting(&daemon, ServiceAction::Start, WAIT)
                    .await
            });
            let fatal = "FATAL:  lock file \"postmaster.pid\" already exists";
            let printed = async {
                while !pg.texts().iter().any(|text| text.ends_with(fatal)) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            tokio::time::timeout(WAIT, printed).await.unwrap();
            pg.daemon.stop_procs(WAIT).await;
            let status = start.await.unwrap().unwrap();
            assert_eq!(status.state, ServiceState::Stopped);
            assert!(pg.probes.asked.lock().unwrap().is_empty());
        }

        /// Another server holds the lock of our socket for this port.
        #[tokio::test]
        async fn a_socket_lock_refusal_is_port_busy() {
            let pg = pg("socket-locked", probes(alive(Path::new(OTHER)), None));
            let err = pg.act(ServiceAction::Start).await.unwrap_err();
            assert_eq!(err.code(), "service_port_busy");
            let text = err.to_string();
            assert!(text.contains(&format!("port {}", pg.port)), "{text}");
            assert!(text.contains(&format!("{OTHER} (pid 4242)")), "{text}");
            let state = pg.daemon.postgres.status().state;
            assert_eq!(state, failed("service_port_busy"));
            let last = pg.texts().pop().unwrap();
            assert!(last.contains(".s.PGSQL."), "{last}");
        }

        /// The fake prints PostgreSQL's refusal at its start and again once
        /// it answers, as a client could make it log, but runs on. A line
        /// never stops a server: only a run that ended by itself is read.
        #[tokio::test]
        async fn a_lock_line_from_a_running_server_changes_nothing() {
            let pg = pg("noisy", probes(alive(Path::new(OTHER)), None));
            let status = pg.act(ServiceAction::Start).await.unwrap();
            assert_eq!(status.state, ServiceState::Running);
            let pid = status.pid;
            // The second refusal comes after its first answer, so after
            // the start was ready.
            let fatal = "FATAL:  lock file \"postmaster.pid\" already exists";
            let printed = async {
                let count = || pg.texts().iter().filter(|t| t.ends_with(fatal)).count();
                while count() < 2 {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            tokio::time::timeout(WAIT, printed).await.unwrap();
            tokio::time::sleep(Duration::from_millis(700)).await;
            let status = pg.daemon.postgres.status();
            assert_eq!((status.state, status.pid), (ServiceState::Running, pid));
            assert!(pg.daemon.proc_alive(ServerKind::Postgres.id()));
            // Only about the server itself, for `run/postgres.pid`: the
            // lines named no pid that was looked at.
            let asked = pg.probes.asked.lock().unwrap().clone();
            assert!(asked.iter().all(|asked| Some(*asked) == pid), "{asked:?}");
            let texts = pg.texts();
            let signals = ["fake got SIGINT", "fake got SIGTERM"];
            assert!(!texts.iter().any(|t| signals.contains(&t.as_str())));
            pg.act(ServiceAction::Stop).await.unwrap();
        }

        /// `[postgres] enabled` now.
        fn enabled(pg: &Pg) -> bool {
            let global = config::load_global(&ConfigPaths::in_dir(&pg.daemon.data_dir).global);
            global.unwrap().postgres.enabled
        }

        /// The fake's `initdb` sleeps, as a slow first init.
        fn slow_init(pg: &Pg) {
            let platform = Platform::current().unwrap();
            let programs = programs(&pg.daemon.data_dir, platform, &Version::new(18, 7, 0));
            let calls = pg.dir.path().join("calls.log").display().to_string();
            let slow = format!("#!/bin/sh\necho initdb >> '{calls}'\nexec sleep 60\n");
            fs::write(programs.unwrap().initdb, slow).unwrap();
        }

        /// An action of `marsa services` in a task, as the route runs it.
        fn spawn_saved(
            pg: &Pg,
            action: ServiceAction,
        ) -> JoinHandle<Result<ServiceStatus, ActionError>> {
            let daemon = Arc::clone(&pg.daemon);
            tokio::spawn(async move {
                let postgres = &daemon.postgres;
                postgres.run_and_save(&daemon, action, WAIT).await
            })
        }

        async fn until(what: &str, done: impl Fn() -> bool) {
            let wait = async {
                while !done() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            tokio::time::timeout(WAIT, wait).await.expect(what);
        }

        /// No start left behind: no token, no stop counted.
        fn nothing_left(pg: &Pg) -> bool {
            let starting = lock(&pg.daemon.postgres.starting);
            starting.token.is_none() && starting.stops_waiting == 0
        }

        /// A start of `marsa services` turns it on, a stop turns it off.
        #[tokio::test]
        async fn a_start_turns_it_on_and_a_stop_off() {
            let pg = pg("serve", probes(Liveness::Gone, None));
            set_postgres(&pg.daemon.data_dir, Some("18"), false, pg.port);
            let status = spawn_saved(&pg, ServiceAction::Start)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(status.state, ServiceState::Running);
            assert!(enabled(&pg));
            pg.wait_for("fake ready").await;
            spawn_saved(&pg, ServiceAction::Stop)
                .await
                .unwrap()
                .unwrap();
            assert!(!enabled(&pg));
            let status = spawn_saved(&pg, ServiceAction::Restart)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(status.state, ServiceState::Running);
            assert!(enabled(&pg));
            pg.act(ServiceAction::Stop).await.unwrap();
            // Without `marsa services`, nothing is written.
            assert!(enabled(&pg));
        }

        /// A stop does not wait for a first start: it drops it, `initdb` and
        /// all, nothing runs after, and it stays off.
        #[tokio::test]
        async fn a_stop_drops_a_start_that_runs() {
            let pg = pg("serve", probes(Liveness::Gone, None));
            set_postgres(&pg.daemon.data_dir, Some("18"), false, pg.port);
            slow_init(&pg);
            let start = spawn_saved(&pg, ServiceAction::Start);
            until("initdb", || pg.count("initdb") > 0).await;

            // A stop that writes nothing, so a write of the dropped start
            // would show.
            let begun = std::time::Instant::now();
            let status = pg.act(ServiceAction::Stop).await.unwrap();
            assert!(
                begun.elapsed() < Duration::from_secs(10),
                "{:?}",
                begun.elapsed()
            );
            assert_eq!(status.state, ServiceState::Stopped);
            let started = start.await.unwrap().unwrap();
            assert_eq!(started.state, ServiceState::Stopped);
            assert_eq!(pg.count("server"), 0);
            assert!(!pg.dirs.data.exists());
            assert!(!pg.daemon.proc_alive(ServerKind::Postgres.id()));
            assert!(!enabled(&pg), "the dropped start wrote nothing");
            assert!(nothing_left(&pg));
        }

        /// A daemon that stops drops an API start: `service_busy` at once,
        /// no cluster, and `enabled` as it was.
        #[tokio::test]
        async fn a_shutdown_drops_a_start_of_the_api() {
            let pg = pg("serve", probes(Liveness::Gone, None));
            set_postgres(&pg.daemon.data_dir, Some("18"), false, pg.port);
            slow_init(&pg);
            let start = spawn_saved(&pg, ServiceAction::Start);
            until("initdb", || pg.count("initdb") > 0).await;
            let begun = std::time::Instant::now();
            pg.daemon.shutdown.cancel();
            let err = start.await.unwrap().unwrap_err();
            assert_eq!(err.code(), "service_busy");
            assert!(
                begun.elapsed() < Duration::from_secs(10),
                "{:?}",
                begun.elapsed()
            );
            assert!(!pg.dirs.data.exists());
            assert!(!enabled(&pg));
            assert!(nothing_left(&pg));
        }

        /// A start dropped after its spawn, while it waits for connections:
        /// the stop stops the server it spawned.
        #[tokio::test]
        async fn a_stop_while_the_server_starts_stops_it() {
            let pg = pg("mute", probes(Liveness::Gone, None));
            let start = spawn_saved(&pg, ServiceAction::Start);
            pg.wait_for("fake ready").await;
            let status = spawn_saved(&pg, ServiceAction::Stop)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(status.state, ServiceState::Stopped);
            assert!(pg.texts().iter().any(|text| text == "fake got SIGINT"));
            start.await.unwrap().unwrap();
            assert!(!pg.daemon.proc_alive(ServerKind::Postgres.id()));
            assert!(!enabled(&pg));
            assert!(nothing_left(&pg));
        }

        /// A start in line before a stop never begins, nor does the start of
        /// a restart whose stop part a stop overtook.
        #[tokio::test]
        async fn a_stop_drops_the_starts_before_it_in_line() {
            let pg = pg("serve", probes(Liveness::Gone, None));
            for first in [ServiceAction::Start, ServiceAction::Restart] {
                let held = Arc::clone(&pg.daemon.postgres.busy).lock_owned().await;
                let start = spawn_saved(&pg, first);
                tokio::time::sleep(Duration::from_millis(100)).await;
                let stop = spawn_saved(&pg, ServiceAction::Stop);
                until("the stop in line", || {
                    lock(&pg.daemon.postgres.starting).stops_waiting == 1
                })
                .await;
                drop(held);
                start.await.unwrap().unwrap();
                stop.await.unwrap().unwrap();
                assert_eq!(pg.count("server"), 0, "{first}");
                assert!(!enabled(&pg), "{first}");
                assert!(nothing_left(&pg));
            }
        }

        /// `DELETE /v1/runtimes/postgres/18.7.0` through the routes.
        async fn uninstall(pg: &Pg) -> (u16, marsa_core::types::ErrorBody) {
            use crate::daemon::api::router;
            use crate::daemon::auth::AuthConfig;
            use axum::body::{Body, to_bytes};
            use axum::http::Request;
            use marsa_core::platform::daemon_file::{Token, daemon_authority};
            use tower::ServiceExt as _;

            const PORT: u16 = 51239;
            let token = Token::generate().unwrap();
            let bearer = token.bearer_header();
            let app = router(
                Arc::clone(&pg.daemon),
                AuthConfig::new(Arc::new(token), PORT),
            );
            let request = Request::builder()
                .method("DELETE")
                .uri("/v1/runtimes/postgres/18.7.0")
                .header("host", daemon_authority(PORT))
                .header("authorization", &bearer)
                .body(Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            let status = response.status().as_u16();
            let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
            (status, serde_json::from_slice(&body).unwrap())
        }

        /// The running version, or one a start may pick, cannot be
        /// uninstalled; its folder stays.
        #[tokio::test]
        async fn a_running_or_starting_postgres_keeps_its_version() {
            let pg = pg("serve", probes(Liveness::Gone, None));
            let version_dir = pg.daemon.data_dir.join("runtimes/postgres/18.7.0");
            slow_init(&pg);
            let start = spawn_saved(&pg, ServiceAction::Start);
            until("initdb", || pg.count("initdb") > 0).await;
            let (status, body) = uninstall(&pg).await;
            assert_eq!((status, body.error.code.as_str()), (409, "service_busy"));
            pg.act(ServiceAction::Stop).await.unwrap();
            start.await.unwrap().unwrap();
            assert!(version_dir.is_dir());

            let pg = pg_in("data2", "serve", probes(Liveness::Gone, None));
            pg.act(ServiceAction::Start).await.unwrap();
            let (status, body) = uninstall(&pg).await;
            assert_eq!((status, body.error.code.as_str()), (409, "version_in_use"));
            assert!(body.in_use.unwrap().server);
            assert!(pg.daemon.data_dir.join("runtimes/postgres/18.7.0").is_dir());
            pg.act(ServiceAction::Stop).await.unwrap();
        }

        /// The first install writes `[postgres]` and starts it; a stop asked
        /// while it waits for its turn drops that start.
        #[tokio::test]
        async fn the_first_install_starts_it_unless_a_stop_comes_first() {
            let pg = pg("serve", probes(Liveness::Gone, None));
            set_postgres(&pg.daemon.data_dir, None, false, pg.port);
            let global = ConfigPaths::in_dir(&pg.daemon.data_dir).global;
            start_after_install(&pg.daemon, ServerKind::Postgres, Version::new(18, 7, 0));
            until("running", || {
                pg.daemon.postgres.status().state == ServiceState::Running
            })
            .await;
            let settings = config::load_global(&global).unwrap().postgres;
            assert_eq!(
                (settings.default.as_deref(), settings.enabled),
                (Some("18"), true)
            );
            pg.wait_for("fake ready").await;
            pg.act(ServiceAction::Stop).await.unwrap();

            // Again from no default: a stop waits in line first.
            set_postgres(&pg.daemon.data_dir, None, false, pg.port);
            let held = Arc::clone(&pg.daemon.postgres.busy).lock_owned().await;
            start_after_install(&pg.daemon, ServerKind::Postgres, Version::new(18, 7, 0));
            tokio::time::sleep(Duration::from_millis(100)).await;
            let stop = spawn_saved(&pg, ServiceAction::Stop);
            until("the stop in line", || {
                lock(&pg.daemon.postgres.starting).stops_waiting == 1
            })
            .await;
            drop(held);
            stop.await.unwrap().unwrap();
            assert_eq!(pg.count("server"), 1, "no second server");
            assert!(!enabled(&pg));
        }

        /// A fake server left running, as by a crashed `marsad`. Killed when
        /// dropped, so a failed assert leaves no process behind.
        pub(in crate::daemon::databases) struct Orphan(
            pub(in crate::daemon::databases) std::process::Child,
        );

        impl Orphan {
            fn id(&self) -> u32 {
                self.0.id()
            }
        }

        impl Drop for Orphan {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        /// The fake server in `mode` on `pg.port`, not our child task and not
        /// killed by the daemon. Its own stderr goes to `<dir>/orphan.log`.
        fn orphan_server(pg: &Pg, mode: &str) -> Orphan {
            let log = fs::File::create(pg.dir.path().join("orphan.log")).unwrap();
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "daemon::databases::tests::server::fake_postgres",
                    "--exact",
                    "--nocapture",
                ])
                .env(FAKE, mode)
                .env("MARSA_FAKE_POSTGRES_ARGS", format!("-c port={}", pg.port))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(log)
                .spawn()
                .unwrap();
            Orphan(child)
        }

        /// The installed `postgres` of the fake version.
        fn installed_postgres(pg: &Pg) -> PathBuf {
            pg.daemon
                .data_dir
                .join("runtimes/postgres/18.7.0/bin/postgres")
        }

        /// `run/postgres.pid` for `pid`, as the daemon writes it, with its
        /// real start time unless `started` says another.
        async fn record_orphan(pg: &Pg, pid: u32, started: Option<StartTime>) {
            let exe = installed_postgres(pg);
            pg.probes.orphans.lock().unwrap().push((pid, exe.clone()));
            let until = std::time::Instant::now() + WAIT;
            let real = loop {
                if let Liveness::Alive { started, .. } = process::identity(pid).await {
                    break started;
                }
                assert!(std::time::Instant::now() < until, "the orphan runs");
                tokio::time::sleep(Duration::from_millis(20)).await;
            };
            let started = started.unwrap_or(real);
            let record = PidRecord { pid, exe, started };
            process::write_pid_file(
                &orphan::pid_path(ServerKind::Postgres, &pg.daemon.data_dir),
                &record,
            )
            .unwrap();
        }

        /// Waits until something listens on `port`.
        pub(in crate::daemon::databases) async fn listening(port: u16) {
            let wait = async {
                while tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                    .await
                    .is_err()
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            tokio::time::timeout(WAIT, wait)
                .await
                .expect("the orphan listens");
        }

        fn orphan_log(pg: &Pg) -> String {
            fs::read_to_string(pg.dir.path().join("orphan.log")).unwrap_or_default()
        }

        fn signals(pg: &Pg) -> Vec<(u32, Signal)> {
            pg.probes.signals.lock().unwrap().clone()
        }

        /// What `run/postgres.pid` names now.
        fn recorded(pg: &Pg) -> Option<PidRecord> {
            process::read_pid_file(&orphan::pid_path(ServerKind::Postgres, &pg.daemon.data_dir))
        }

        /// A `marsad` that died left our server running: the next start
        /// stops it with SIGINT, never SIGTERM, then starts a new one and
        /// records it; the record goes when that run ends. A bystander pid is
        /// never signalled.
        #[tokio::test]
        async fn our_orphan_is_stopped_with_sigint_and_a_new_server_starts() {
            let pg = pg("serve", probes(Liveness::Gone, None));
            let mut orphan = orphan_server(&pg, "serve");
            listening(pg.port).await;
            record_orphan(&pg, orphan.id(), None).await;
            let mut bystander = Orphan(
                std::process::Command::new("/bin/sleep")
                    .arg("60")
                    .spawn()
                    .unwrap(),
            );
            // The new server runs as our installed `postgres`.
            *pg.probes.now.lock().unwrap() = alive(&installed_postgres(&pg));

            let status = pg.act(ServiceAction::Start).await.unwrap();
            assert_eq!(status.state, ServiceState::Running);
            let gone = orphan.0.wait().unwrap();
            assert!(gone.success(), "it ended at SIGINT: {gone:?}");
            let log = orphan_log(&pg);
            assert!(
                log.contains("fake got SIGINT") && !log.contains("SIGTERM"),
                "{log}"
            );
            assert_eq!(signals(&pg), [(orphan.id(), Signal::Int)]);
            assert!(
                bystander.0.try_wait().unwrap().is_none(),
                "the bystander runs"
            );
            let said = pg
                .texts()
                .iter()
                .any(|t| t.contains("an earlier marsad left running; it ended after SIGINT"));
            assert!(said, "{:?}", pg.texts());

            let new = recorded(&pg).expect("the new server is recorded");
            assert_eq!(Some(new.pid), status.pid);
            assert_eq!(new.exe, installed_postgres(&pg));
            pg.act(ServiceAction::Stop).await.unwrap();
            assert_eq!(recorded(&pg), None, "the end of the run deletes it");
        }

        /// A process that never runs an installed `postgres` is not
        /// recorded.
        #[tokio::test]
        async fn only_our_postgres_is_recorded() {
            let pg = pg("serve", probes(alive(Path::new("/bin/sleep")), None));
            pg.act(ServiceAction::Start).await.unwrap();
            assert_eq!(recorded(&pg), None);
            pg.act(ServiceAction::Stop).await.unwrap();
        }

        /// An orphan that ignores SIGINT and SIGQUIT gets SIGKILL, and the
        /// line says what that may leave.
        #[tokio::test]
        async fn a_deaf_orphan_gets_sigint_then_sigquit_then_sigkill() {
            let mut pg = pg("serve", probes(Liveness::Gone, None));
            let short = Duration::from_millis(300);
            Arc::get_mut(&mut pg.daemon).unwrap().postgres.orphan_ladder =
                StopMethod::signals([(Signal::Int, short), (Signal::Quit, short)]);
            let mut orphan = orphan_server(&pg, "deaf");
            listening(pg.port).await;
            record_orphan(&pg, orphan.id(), None).await;

            let status = pg.act(ServiceAction::Start).await.unwrap();
            assert_eq!(status.state, ServiceState::Running);
            let gone = orphan.0.wait().unwrap();
            assert!(!gone.success(), "killed: {gone:?}");
            let sent: Vec<Signal> = signals(&pg).iter().map(|(_, s)| *s).collect();
            assert_eq!(sent, [Signal::Int, Signal::Quit, Signal::Kill]);
            assert!(pg.texts().iter().any(|t| t.contains("needed SIGKILL")));
            pg.act(ServiceAction::Stop).await.unwrap();
        }

        /// The check comes again before each signal: a pid that cannot be
        /// told after SIGINT gets no SIGQUIT, and the start is refused.
        #[tokio::test]
        async fn each_signal_needs_its_own_check() {
            let mut pg = pg("serve", probes(Liveness::Gone, None));
            let short = Duration::from_millis(300);
            Arc::get_mut(&mut pg.daemon).unwrap().postgres.orphan_ladder =
                StopMethod::signals([(Signal::Int, short), (Signal::Quit, short)]);
            let orphan = orphan_server(&pg, "deaf");
            listening(pg.port).await;
            record_orphan(&pg, orphan.id(), None).await;
            *pg.probes.unknown_after_signal.lock().unwrap() = true;

            let err = pg.act(ServiceAction::Start).await.unwrap_err();
            assert_eq!(err.code(), "postgres_orphan_stuck");
            assert_eq!(signals(&pg), [(orphan.id(), Signal::Int)]);
            assert_eq!(pg.count("server"), 0);
        }

        /// A stop while this daemon runs nothing stops our orphan, so the
        /// stop never says "stopped" while it serves.
        #[tokio::test]
        async fn a_stop_stops_our_orphan_too() {
            let pg = pg("serve", probes(Liveness::Gone, None));
            let mut orphan = orphan_server(&pg, "serve");
            listening(pg.port).await;
            record_orphan(&pg, orphan.id(), None).await;
            let status = pg.act(ServiceAction::Stop).await.unwrap();
            assert_eq!(status.state, ServiceState::Stopped);
            assert!(orphan.0.wait().unwrap().success());
            assert_eq!(signals(&pg), [(orphan.id(), Signal::Int)]);
            assert_eq!(recorded(&pg), None);
            assert_eq!(pg.count("server"), 0);
        }

        /// The same program at the same pid, started at another time, is not
        /// ours (ADR 0009, R5e): no signal, only the file goes, and the port
        /// it holds refuses the start.
        #[tokio::test]
        async fn another_start_time_is_not_ours_and_gets_no_signal() {
            let pg = pg("serve", probes(Liveness::Gone, None));
            let mut orphan = orphan_server(&pg, "serve");
            listening(pg.port).await;
            record_orphan(&pg, orphan.id(), Some(StartTime::Utc(0))).await;

            let err = pg.act(ServiceAction::Start).await.unwrap_err();
            assert_eq!(err.code(), "service_port_busy");
            assert!(signals(&pg).is_empty());
            assert!(orphan.0.try_wait().unwrap().is_none(), "it still runs");
            assert_eq!(recorded(&pg), None);
        }

        /// A pid the system cannot describe (another user's process): no
        /// signal, and the start goes on to its own checks.
        #[tokio::test]
        async fn a_record_that_cannot_be_told_gets_no_signal() {
            let pg = pg("serve", probes(Liveness::Unknown, None));
            let record = PidRecord {
                pid: 4242,
                exe: installed_postgres(&pg),
                started: StartTime::Ticks(1),
            };
            let path = orphan::pid_path(ServerKind::Postgres, &pg.daemon.data_dir);
            process::write_pid_file(&path, &record).unwrap();
            let status = pg.act(ServiceAction::Start).await.unwrap();
            assert_eq!(status.state, ServiceState::Running);
            assert!(signals(&pg).is_empty());
            assert_eq!(recorded(&pg), Some(record));
            pg.act(ServiceAction::Stop).await.unwrap();
        }

        /// A pid that is gone, or another program now: the file goes, with no
        /// signal, and the start runs.
        #[tokio::test]
        async fn a_stale_record_is_deleted_and_the_start_runs() {
            for now in [Liveness::Gone, alive(Path::new("/bin/sleep"))] {
                let pg = pg("serve", probes(now, None));
                let record = PidRecord {
                    pid: 4242,
                    exe: installed_postgres(&pg),
                    started: StartTime::Ticks(1),
                };
                let path = orphan::pid_path(ServerKind::Postgres, &pg.daemon.data_dir);
                process::write_pid_file(&path, &record).unwrap();
                let status = pg.act(ServiceAction::Start).await.unwrap();
                assert_eq!(status.state, ServiceState::Running);
                assert!(signals(&pg).is_empty());
                assert!(!path.exists(), "deleted, and the new run not ours");
                pg.act(ServiceAction::Stop).await.unwrap();
            }
        }

        #[tokio::test]
        async fn postgres_starts_with_the_daemon_only_when_it_is_on() {
            let pg = pg("serve", probes(Liveness::Gone, None));
            let launched = start_at_launch(&pg.daemon, ServerKind::Postgres)
                .wait(WAIT)
                .await;
            let Some(Launched::Ran(status)) = launched else {
                panic!("{launched:?}");
            };
            assert_eq!(status.state, ServiceState::Running);
            pg.daemon.stop_procs(WAIT).await;

            let data = &pg.daemon.data_dir;
            set_postgres(data, Some("18"), false, pg.port);
            let launched = start_at_launch(&pg.daemon, ServerKind::Postgres)
                .wait(WAIT)
                .await;
            assert_eq!(launched, Some(Launched::Off));
            // On, but its line is not installed: the status stays as it was.
            set_postgres(data, Some("17"), true, pg.port);
            let launched = start_at_launch(&pg.daemon, ServerKind::Postgres)
                .wait(WAIT)
                .await;
            assert_eq!(launched, Some(Launched::NotInstalled));
            assert_eq!(pg.daemon.postgres.status().state, ServiceState::Stopped);
            assert_eq!(pg.count("server"), 1);

            // A failed start: the status says why, and the daemon runs on.
            set_postgres(data, Some("18"), true, pg.port);
            let _held = TcpListener::bind((Ipv4Addr::LOCALHOST, pg.port)).unwrap();
            let launched = start_at_launch(&pg.daemon, ServerKind::Postgres)
                .wait(WAIT)
                .await;
            assert_eq!(launched, Some(Launched::Failed("service_port_busy")));
            let state = pg.daemon.postgres.status().state;
            assert_eq!(state, failed("service_port_busy"));
        }
    }

    /// Each rule of `after_install`.
    #[test]
    fn an_install_sets_the_default_line_and_starts_only_the_first() {
        let v = |text: &str| Version::parse(text).unwrap();
        let settings = |default: Option<&str>, enabled| ServerSettings {
            default: default.map(str::to_owned),
            enabled,
            port: None,
        };
        let cases = [
            // The first install: its line, on, and a start.
            (
                settings(None, false),
                "18.7.0",
                &["18.7.0"][..],
                settings(Some("18"), true),
                true,
            ),
            // Another line ran without a default: pinned to it, no start.
            (
                settings(None, true),
                "18.7.0",
                &["17.12.0", "18.7.0"],
                settings(Some("17"), true),
                false,
            ),
            (
                settings(None, false),
                "17.12.0",
                &["17.12.0", "18.7.0"],
                settings(Some("18"), false),
                false,
            ),
            // A newer patch of the default line: nothing.
            (
                settings(Some("18"), false),
                "18.8.0",
                &["18.7.0", "18.8.0"],
                settings(Some("18"), false),
                false,
            ),
            // Another line beside the default: nothing.
            (
                settings(Some("18"), true),
                "17.12.0",
                &["17.12.0", "18.7.0"],
                settings(Some("18"), true),
                false,
            ),
            // The default line is gone: the new line, started if on.
            (
                settings(Some("18"), true),
                "17.12.0",
                &["17.12.0"],
                settings(Some("17"), true),
                true,
            ),
            (
                settings(Some("18"), false),
                "17.12.0",
                &["17.12.0"],
                settings(Some("17"), false),
                false,
            ),
            // A default that is not a line: left alone.
            (
                settings(Some("x"), true),
                "17.12.0",
                &["17.12.0"],
                settings(Some("x"), true),
                false,
            ),
            // Uninstalled before its turn came: nothing.
            (
                settings(None, false),
                "18.7.0",
                &[],
                settings(None, false),
                false,
            ),
        ];
        for (before, version, installed, after, start) in cases {
            let installed: Vec<Version> = installed.iter().map(|text| v(text)).collect();
            let mut got = before.clone();
            let started = after_install(ServerKind::Postgres, &mut got, &v(version), &installed);
            assert_eq!((got, started), (after, start), "{before:?} + {version}");
        }
    }

    /// Only a change is written.
    #[test]
    fn first_install_writes_global_toml_only_for_a_change() {
        let data = tempfile::tempdir().unwrap();
        let path = ConfigPaths::in_dir(data.path()).global;
        let v18 = Version::new(18, 7, 0);
        assert!(
            first_install(
                ServerKind::Postgres,
                data.path(),
                &v18,
                std::slice::from_ref(&v18)
            )
            .unwrap()
        );
        let settings = config::load_global(&path).unwrap().postgres;
        assert_eq!(
            (settings.default.as_deref(), settings.enabled),
            (Some("18"), true)
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "[postgres]\ndefault = \"18\"\n").unwrap();
        let v17 = Version::new(17, 12, 0);
        assert!(
            !first_install(ServerKind::Postgres, data.path(), &v17, &[v17.clone(), v18]).unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[postgres]\ndefault = \"18\"\n"
        );
    }

    /// A failed sync after the rename does not say that nothing was made.
    #[test]
    fn an_init_error_says_whether_the_cluster_is_in_place() {
        let failed = InitError::new(ServerKind::Postgres, "initdb failed".to_owned());
        let text = failed.to_string();
        assert_eq!(
            text,
            "the PostgreSQL data folder was not made: initdb failed"
        );
        let synced = InitError {
            kind: ServerKind::Postgres,
            reason: "its folder could not be synced to disk: I/O error".to_owned(),
            in_place: true,
        };
        let text = synced.to_string();
        assert!(
            text.contains("was made and is in place, but its folder"),
            "{text}"
        );
        assert!(text.ends_with("the next start uses it"), "{text}");
        assert!(!text.contains("not made"), "{text}");
        assert_eq!(synced.code(), failed.code());
    }

    /// Shell scripts stand in for `initdb` and `postgres`: macOS and Linux
    /// only, the systems of this task (Windows is).
    #[cfg(unix)]
    mod init {
        use super::*;
        use std::fs;
        use std::os::unix::fs::PermissionsExt as _;

        use tempfile::TempDir;

        /// A data folder and the fakes. The fakes write what they were given
        /// to `calls.log` and `stdin.log` in `dir`.
        struct Fakes {
            dir: TempDir,
            programs: Programs,
            dirs: PostgresDirs,
        }

        /// `initdb` makes `-D <folder>` with a `PG_VERSION` and its own
        /// `pg_hba.conf`. `postgres` keeps the `pg_hba.conf` it found and
        /// its stdin, prints `print`, and exits with `exit`.
        fn fakes(print: &str, exit: u8) -> Fakes {
            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("runtimes/postgres/18.7.0/bin");
            fs::create_dir_all(&bin).unwrap();
            let at = |name: &str| dir.path().join(name).display().to_string();
            let initdb = format!(
                "#!/bin/sh\necho \"initdb $*\" >> '{calls}'\n[ \"$1\" = -D ] || exit 2\n\
                 mkdir \"$2\" || exit 3\necho 18 > \"$2/PG_VERSION\"\n\
                 echo 'host all all 127.0.0.1/32 trust' > \"$2/pg_hba.conf\"\n",
                calls = at("calls.log"),
            );
            let postgres = format!(
                "#!/bin/sh\necho \"postgres $*\" >> '{calls}'\n\
                 cp \"$3/pg_hba.conf\" '{hba}'\ncat > '{stdin}'\necho '{print}' >&2\nexit {exit}\n",
                calls = at("calls.log"),
                hba = at("hba-at-single.conf"),
                stdin = at("stdin.log"),
            );
            for (name, script) in [("initdb", initdb), ("postgres", postgres)] {
                let path = bin.join(name);
                fs::write(&path, script).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            let programs = Programs {
                postgres: bin.join("postgres"),
                initdb: bin.join("initdb"),
            };
            let dirs = PostgresDirs::new(&dir.path().join("app"), 18);
            Fakes {
                dir,
                programs,
                dirs,
            }
        }

        impl Fakes {
            fn read(&self, name: &str) -> String {
                fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
            }

            /// The names in `databases/postgres/`.
            fn listing(&self) -> Vec<String> {
                let parent = self.dirs.data.parent().unwrap();
                let mut names: Vec<_> = fs::read_dir(parent)
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                names
            }
        }

        #[tokio::test]
        async fn the_first_init_runs_initdb_then_makes_root_then_moves_the_folder() {
            let fakes = fakes("LOG:  database system is shut down", 0);
            assert_eq!(ensure_cluster(&fakes.programs, &fakes.dirs).await, Ok(true));

            let calls = fakes.read("calls.log");
            let calls: Vec<_> = calls.lines().collect();
            let [initdb, single] = calls[..] else {
                panic!("{calls:?}");
            };
            // The same temp folder for both steps, next to the target.
            let temp = initdb.split(' ').nth(2).unwrap();
            let parent = fakes.dirs.data.parent().unwrap();
            let prefix = parent.join(".18.init-").display().to_string();
            assert!(temp.starts_with(&prefix), "{temp}");
            let in_temp = PostgresDirs {
                data: temp.into(),
                sockets: fakes.dirs.sockets.clone(),
            };
            assert_eq!(
                initdb,
                format!("initdb {}", initdb_args(&in_temp).join(" "))
            );
            let want = format!("postgres {}", single_user_args(Path::new(temp)).join(" "));
            assert_eq!(single, want);
            // Our pg_hba.conf was in place before root was made.
            assert_eq!(fakes.read("hba-at-single.conf"), PG_HBA_CONF);
            assert_eq!(fakes.read("stdin.log"), ROOT_ROLE_SQL);

            // Moved into place whole, and nothing else is left.
            let hba = fs::read_to_string(fakes.dirs.data.join("pg_hba.conf")).unwrap();
            assert_eq!(hba, PG_HBA_CONF);
            assert!(fakes.dirs.data.join("PG_VERSION").is_file());
            assert_eq!(fakes.listing(), ["18"]);

            // The next start makes nothing.
            assert_eq!(
                ensure_cluster(&fakes.programs, &fakes.dirs).await,
                Ok(false)
            );
            assert_eq!(fakes.read("calls.log").lines().count(), 2);
        }

        #[tokio::test]
        async fn anything_at_the_data_folder_is_left_as_it_is() {
            let fakes = fakes("", 0);
            let parent = fakes.dirs.data.parent().unwrap();
            fs::create_dir_all(parent).unwrap();
            // An empty folder, a file, and a link: none is touched or run on.
            let data = &fakes.dirs.data;
            for kind in ["folder", "file", "link"] {
                match kind {
                    "folder" => fs::create_dir(data).unwrap(),
                    "file" => fs::write(data, "x").unwrap(),
                    _ => std::os::unix::fs::symlink("/nowhere", data).unwrap(),
                }
                let before = fs::symlink_metadata(data).unwrap();
                assert_eq!(
                    ensure_cluster(&fakes.programs, &fakes.dirs).await,
                    Ok(false)
                );
                let after = fs::symlink_metadata(data).unwrap();
                assert_eq!(before.file_type(), after.file_type(), "{kind}");
                assert_eq!(before.modified().unwrap(), after.modified().unwrap());
                if kind == "folder" {
                    fs::remove_dir(data).unwrap();
                } else {
                    fs::remove_file(data).unwrap();
                }
            }
            assert_eq!(fakes.read("calls.log"), "", "no program ran");
            assert_eq!(fakes.listing(), Vec::<String>::new());
        }

        #[tokio::test]
        async fn a_failed_step_leaves_no_cluster_and_no_temp_folder() {
            // An error that the session went on after, and an error exit.
            for (print, exit) in [
                ("ERROR:  role \"root\" already exists", 0),
                ("FATAL:  could not open file", 1),
            ] {
                let fakes = fakes(print, exit);
                let err = ensure_cluster(&fakes.programs, &fakes.dirs)
                    .await
                    .unwrap_err();
                assert_eq!(err.code(), "postgres_init_failed");
                assert!(err.reason.starts_with("postgres --single failed"), "{err}");
                assert!(err.reason.contains(print), "{err}");
                assert!(!fakes.dirs.data.exists());
                assert_eq!(fakes.listing(), Vec::<String>::new());
            }
        }

        #[tokio::test]
        async fn a_failed_initdb_runs_nothing_after_it() {
            let fakes = fakes("", 0);
            let script = "#!/bin/sh\necho 'initdb: error: cannot be run as root' >&2\nexit 1\n";
            fs::write(&fakes.programs.initdb, script).unwrap();
            let err = ensure_cluster(&fakes.programs, &fakes.dirs)
                .await
                .unwrap_err();
            assert!(err.reason.contains("initdb failed"), "{err}");
            assert!(err.reason.contains("cannot be run as root"), "{err}");
            assert_eq!(fakes.read("stdin.log"), "", "no single-user step");
            assert!(!fakes.dirs.data.exists());
        }

        #[tokio::test]
        async fn temp_folders_of_earlier_inits_of_this_major_are_deleted() {
            let fakes = fakes("", 0);
            let parent = fakes.dirs.data.parent().unwrap();
            for name in [".18.init-dead", ".17.init-dead"] {
                fs::create_dir_all(parent.join(name).join("base")).unwrap();
            }
            assert_eq!(ensure_cluster(&fakes.programs, &fakes.dirs).await, Ok(true));
            assert_eq!(fakes.listing(), [".17.init-dead", "18"]);
        }
    }
}
