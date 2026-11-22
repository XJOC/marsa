//! What only MySQL needs: its `my.cnf` on disk, the first init of its data
//! folder, how it runs, and how it says it is ready.
//!
//! The first start of a line makes its data folder in
//! `databases/mysql/<line>/` ([`ensure_data_dir`]): `mysqld
//! --initialize-insecure`, which makes `root@localhost` with no password,
//! and in the same run `root@127.0.0.1` for TCP ([`root_tcp_sql`]).

use marsa_core::types::ServiceId;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use marsa_core::databases::ServerKind;
use marsa_core::databases::mysql::{
    MysqlDirs, is_handshake, mysql_init_args, mysqld_args, root_tcp_sql,
};
use marsa_core::platform::daemon_file::create_private_dir;
use marsa_core::recipes::Os;
use marsa_core::services::{RestartPolicy, ServiceSpec, StopMethod};
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;

use super::init::{InitError, Step, ensure_data, run_step};

const KIND: ServerKind = ServerKind::Mysql;
/// How long the first init may run. 1 to 5 s on a laptop; a slow disk or a
/// busy CI machine needs more.
const INIT_LIMIT: Duration = Duration::from_secs(120);
/// MySQL's error lines: `<time> <thread> [ERROR] [MY-...] ...`.
const SEVERE: [&str; 1] = ["[ERROR]"];
/// For each attempt of the readiness check.
const CONNECT_LIMIT: Duration = Duration::from_millis(500);
/// Owner-only: `my.cnf` names the socket that reaches `root@localhost`.
const PRIVATE: u32 = 0o600;

/// How the supervisor runs MySQL: `mysqld --defaults-file=run/mysql.cnf`
/// ([`mysqld_args`] on `os`), stopped by `stop`: SIGTERM on macOS and Linux
/// (`mysql_stop`), `mysqladmin shutdown` on Windows (`mysql_windows_stop`),
/// then a kill after 30 s. MySQL logs to stderr (its `my.cnf` sets no
/// `log-error`), which the supervisor keeps in `logs/mysql.log`.
///
/// Never restarted after it exits, as PostgreSQL: a refusal at its start
/// would only come again, and the status must keep the run's own exit.
pub(super) fn spec(program: PathBuf, dirs: &MysqlDirs, os: Os, stop: StopMethod) -> ServiceSpec {
    ServiceSpec {
        id: ServiceId::Database(KIND),
        program,
        args: mysqld_args(&dirs.config(), os),
        env: std::collections::BTreeMap::new(),
        cwd: None,
        restart: RestartPolicy::Never,
        stop,
    }
}

/// Writes `text`, Marsa's `my.cnf`, to `run/mysql.cnf`, owner-only, and
/// makes the folder of `secure_file_priv` and the folder that holds the data
/// folder owner-only too; `mysqld` needs the first at its start. Before each start, so a new port or version takes effect.
pub(super) async fn write_config(dirs: &MysqlDirs, text: String) -> io::Result<()> {
    let dirs = dirs.clone();
    let job = tokio::task::spawn_blocking(move || {
        create_private_dir(&dirs.run)?;
        create_private_dir(&dirs.files)?;
        // `mysqld` makes its data folder 0750 with group-readable files: the
        // folder above it keeps other users out.
        if let Some(parent) = dirs.data.parent() {
            create_private_dir(parent)?;
        }
        marsa_core::fs::write_atomic(&dirs.config(), text.as_bytes(), Some(PRIVATE))
    });
    job.await.map_err(io::Error::other)?
}

/// Makes the data folder in `dirs.data` if nothing is there yet
/// ([`ensure_data`]), and returns whether it made one: `mysqld` with
/// [`mysql_init_args`] into the temp folder, with [`root_tcp_sql`] of `os`
/// in `run/mysql-init.sql` for that run only. `run/mysql.cnf` is written
/// first ([`write_config`]).
///
/// # Errors
///
/// `mysql_init_failed`, with the end of the output.
pub(super) async fn ensure_data_dir(
    mysqld: &Path,
    dirs: &MysqlDirs,
    os: Os,
) -> Result<bool, InitError> {
    ensure_data(KIND, &dirs.data, async |temp: &Path| {
        let sql = dirs.init_sql();
        let text = root_tcp_sql(os);
        marsa_core::fs::write_atomic(&sql, text.as_bytes(), Some(PRIVATE))
            .map_err(|err| InitError::io(KIND, "cannot write the init SQL", &err))?;
        let mut init = Command::new(mysqld);
        init.args(mysql_init_args(&dirs.config(), &sql, temp, os));
        let step = Step {
            kind: KIND,
            what: "mysqld --initialize-insecure",
            input: None,
            limit: INIT_LIMIT,
            severe: &SEVERE,
        };
        let ran = run_step(init, step).await;
        if let Err(err) = std::fs::remove_file(&sql)
            && err.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(%err, "cannot delete run/mysql-init.sql");
        }
        ran
    })
    .await
}

/// Whether MySQL takes connections on `127.0.0.1:<port>`: its first packet
/// is the handshake ([`is_handshake`]). The connection then closes before
/// any user name; `host_cache_size=0` in `my.cnf` keeps that from ever
/// blocking `127.0.0.1`.
pub(super) async fn answers(port: u16) -> bool {
    let ask = async {
        let address = (Ipv4Addr::LOCALHOST, port);
        let mut stream = tokio::net::TcpStream::connect(address).await.ok()?;
        let mut greeting = [0_u8; 5];
        stream.read_exact(&mut greeting).await.ok()?;
        Some(is_handshake(&greeting))
    };
    matches!(
        tokio::time::timeout(CONNECT_LIMIT, ask).await,
        Ok(Some(true))
    )
}

/// The test binary stands in for `mysqld`, run by a script at `bin/mysqld`
/// that does the first init itself. macOS only until the recipe has Linux.
#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::sync::Arc;

    use marsa_core::config::{self, ConfigPaths, GlobalConfig, ServerSettings};
    use marsa_core::orphan::Liveness;
    use marsa_core::types::{ServiceAction, ServiceState, ServiceStatus};
    use tempfile::TempDir;

    use marsa_core::databases::mysql::{ROOT_TCP_SQL, mysql_stop};

    use super::*;
    use crate::daemon::services::databases::ActionError;
    use crate::daemon::services::databases::tests::{FakeProbes, alive, free_port, probes};
    use crate::daemon::services::probe::ProcessProbe;
    use crate::daemon::state::Daemon;

    const FAKE: &str = "MARSA_FAKE_MYSQLD";
    const WAIT: Duration = Duration::from_secs(30);

    /// Not a check: the fake server's entry point. In a normal test run the
    /// variable is unset and this returns at once.
    ///
    /// It reads `port=` and `pid-file=` from the `my.cnf` it was given,
    /// writes its pid there as `mysqld` does, greets each client with a
    /// handshake (`mute`: never), and at SIGTERM deletes its pid file and
    /// exits 0.
    #[test]
    fn fake_mysqld() {
        let Ok(mode) = std::env::var(FAKE) else {
            return;
        };
        let cnf = fs::read_to_string(std::env::var("MARSA_FAKE_MYSQLD_CNF").unwrap()).unwrap();
        let setting = |name: &str| {
            let line = cnf
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .unwrap();
            line.trim_matches('"').to_owned()
        };
        let port: u16 = setting("port=").parse().unwrap();
        let pid_file = setting("pid-file=");
        fs::write(&pid_file, format!("{}\n", std::process::id())).unwrap();
        let serve = async {
            use tokio::io::AsyncWriteExt as _;
            use tokio::signal::unix::{SignalKind, signal};
            let mut term = signal(SignalKind::terminate()).unwrap();
            let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            eprintln!("fake ready");
            loop {
                tokio::select! {
                    _ = term.recv() => {
                        eprintln!("fake got SIGTERM");
                        let _ = fs::remove_file(&pid_file);
                        eprintln!("mysqld: Shutdown complete");
                        std::process::exit(0);
                    }
                    Ok((mut stream, _)) = listener.accept(), if mode != "mute" => {
                        let _ = stream.write_all(&[0x4a, 0, 0, 0, 10, b'8']).await;
                    }
                }
            }
        };
        tokio::runtime::Runtime::new().unwrap().block_on(serve);
    }

    struct My {
        dir: TempDir,
        daemon: Arc<Daemon>,
        port: u16,
        probes: Arc<FakeProbes>,
        dirs: MysqlDirs,
        mysqld: PathBuf,
    }

    /// A daemon whose MySQL 8.4.13 is the fake in `mode`, with `[mysql]` on,
    /// on a free port. The script writes to `calls.log`.
    fn my(mode: &str, probes: FakeProbes) -> My {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let mysqld = data.join("runtimes/mysql/8.4.13/bin/mysqld");
        fs::create_dir_all(mysqld.parent().unwrap()).unwrap();
        let calls = dir.path().join("calls.log").display().to_string();
        let exe = std::env::current_exe().unwrap().display().to_string();
        let script = format!(
            "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in\n\
             --initialize-insecure) init=1;;\n--datadir=*) dir=\"${{a#--datadir=}}\";;\n\
             --init-file=*) sql=\"${{a#--init-file=}}\";;\n\
             --defaults-file=*) cnf=\"${{a#--defaults-file=}}\";;\nesac; done\n\
             if [ -n \"$init\" ]; then echo \"init $*\" >> '{calls}'; mkdir \"$dir\" || exit 3; \
             cp \"$sql\" \"$dir/init.sql\"; echo '[System] [Server] init end' >&2; exit 0; fi\n\
             echo \"server $*\" >> '{calls}'\n\
             {FAKE}='{mode}' MARSA_FAKE_MYSQLD_CNF=\"$cnf\" exec '{exe}' \
             daemon::services::databases::mysql::tests::fake_mysqld --exact --nocapture\n"
        );
        fs::write(&mysqld, script).unwrap();
        fs::set_permissions(&mysqld, fs::Permissions::from_mode(0o755)).unwrap();
        let port = free_port();
        let global = GlobalConfig {
            mysql: ServerSettings {
                default: Some("8.4".to_owned()),
                enabled: true,
                port: Some(port),
            },
            ..GlobalConfig::default()
        };
        config::save_global(&ConfigPaths::in_dir(&data).global, &global).unwrap();
        let probes = Arc::new(probes);
        let mut daemon = Daemon::new(data.clone());
        daemon.server_mut(ServerKind::Mysql).probes = Arc::clone(&probes) as Arc<dyn ProcessProbe>;
        My {
            dir,
            daemon: Arc::new(daemon),
            port,
            probes,
            dirs: MysqlDirs::new(&data, "8.4"),
            mysqld,
        }
    }

    impl My {
        async fn act(&self, action: ServiceAction) -> Result<ServiceStatus, ActionError> {
            let mysql = &self.daemon.server(ServerKind::Mysql);
            mysql.run_waiting(&self.daemon, action, WAIT).await
        }

        fn calls(&self) -> Vec<String> {
            let text = fs::read_to_string(self.dir.path().join("calls.log"));
            text.unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn texts(&self) -> Vec<String> {
            let lines = self.daemon.server(ServerKind::Mysql).recent_logs();
            lines.into_iter().map(|line| line.text).collect()
        }
    }

    #[tokio::test]
    async fn a_start_makes_the_data_folder_once_and_a_stop_sends_sigterm() {
        let my = my("serve", probes(Liveness::Gone, None));
        let status = my.act(ServiceAction::Start).await.unwrap();
        assert_eq!(status.state, ServiceState::Running);
        let calls = my.calls();
        let config = my.dirs.config().display().to_string();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0].starts_with(&format!("init --defaults-file={config} ")));
        assert_eq!(calls[1], format!("server --defaults-file={config}"));
        // The init ran with root@127.0.0.1's SQL, which is gone after it.
        let sql = fs::read_to_string(my.dirs.data.join("init.sql")).unwrap();
        assert_eq!(sql, ROOT_TCP_SQL);
        assert!(!my.dirs.init_sql().exists());
        let mode = fs::metadata(&my.dirs.files).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        let cnf = fs::read_to_string(my.dirs.config()).unwrap();
        assert!(cnf.contains(&format!("\nport={}\n", my.port)), "{cnf}");
        let mode = fs::metadata(my.dirs.config()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let program = my
            .daemon
            .server(ServerKind::Mysql)
            .running_program(&my.daemon)
            .unwrap();
        assert_eq!(program, my.mysqld);
        // A restart makes no second data folder.
        let status = my.act(ServiceAction::Restart).await.unwrap();
        assert_eq!(status.state, ServiceState::Running);
        let inits = my.calls().iter().filter(|c| c.starts_with("init")).count();
        assert_eq!(inits, 1);

        let status = my.act(ServiceAction::Stop).await.unwrap();
        assert_eq!(status.state, ServiceState::Stopped);
        let texts = my.texts();
        assert_eq!(texts.iter().filter(|t| *t == "fake got SIGTERM").count(), 2);
        assert!(texts.iter().any(|t| t.ends_with("Shutdown complete")));
        let log = fs::read_to_string(my.daemon.data_dir().join("logs/mysql.log")).unwrap();
        assert!(log.contains("fake got SIGTERM"), "{log}");
    }

    /// `mysqld.pid` names a live installed `mysqld`, or a pid the system
    /// cannot tell: refused at once, without the 100 s of InnoDB's lock.
    #[tokio::test]
    async fn a_live_or_unknown_mysqld_pid_refuses_at_once() {
        for now in [None, Some(Liveness::Unknown)] {
            let my = my("serve", probes(Liveness::Gone, None));
            let now = now.unwrap_or_else(|| alive(&my.mysqld));
            *my.probes.now.lock().unwrap() = now;
            fs::create_dir_all(&my.dirs.run).unwrap();
            fs::write(my.dirs.pid_file(), "4242\n").unwrap();
            let begun = std::time::Instant::now();
            let err = my.act(ServiceAction::Start).await.unwrap_err();
            assert!(
                begun.elapsed() < Duration::from_secs(1),
                "{:?}",
                begun.elapsed()
            );
            assert_eq!(err.code(), "service_data_dir_busy");
            assert!(err.to_string().contains("pid 4242"), "{err}");
            assert!(my.calls().is_empty());
            assert!(my.probes.signals.lock().unwrap().is_empty());
            assert_eq!(fs::read_to_string(my.dirs.pid_file()).unwrap(), "4242\n");
        }
    }

    /// A stale `mysqld.pid`, or one whose pid is another program now: the
    /// start goes on, and `mysqld` writes its own.
    #[tokio::test]
    async fn a_stale_mysqld_pid_starts() {
        for now in [Liveness::Gone, alive(Path::new("/bin/sleep"))] {
            let my = my("serve", probes(now, None));
            fs::create_dir_all(&my.dirs.run).unwrap();
            fs::write(my.dirs.pid_file(), "4242\n").unwrap();
            let status = my.act(ServiceAction::Start).await.unwrap();
            assert_eq!(status.state, ServiceState::Running);
            my.act(ServiceAction::Stop).await.unwrap();
        }
    }

    #[tokio::test]
    async fn a_busy_port_starts_nothing_and_makes_no_data_folder() {
        let my = my("serve", probes(Liveness::Gone, Some("mysqld(901)")));
        let err = my.act(ServiceAction::Start).await.unwrap_err();
        assert_eq!(err.code(), "service_port_busy");
        assert!(err.to_string().contains("mysqld(901)"), "{err}");
        let failed = ServiceState::Failed {
            code: "service_port_busy".into(),
        };
        assert_eq!(my.daemon.server(ServerKind::Mysql).status().state, failed);
        assert!(my.calls().is_empty());
        assert!(!my.dirs.data.exists());
    }

    /// A listener that does not greet is not MySQL yet.
    #[tokio::test]
    async fn readiness_is_the_handshake_not_the_port() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!answers(port).await);
        let my = my("serve", probes(Liveness::Gone, None));
        my.act(ServiceAction::Start).await.unwrap();
        assert!(answers(my.port).await);
        my.act(ServiceAction::Stop).await.unwrap();
    }

    #[test]
    fn mysql_runs_with_its_defaults_file_and_stops_with_sigterm() {
        let dirs = MysqlDirs::new(Path::new("/m"), "8.4");
        let program = PathBuf::from("/m/runtimes/mysql/8.4.13/bin/mysqld");
        let spec = spec(program.clone(), &dirs, Os::Mac, mysql_stop());
        assert_eq!(spec.id, ServiceId::Database(ServerKind::Mysql));
        assert_eq!(spec.program, program);
        assert_eq!(spec.args, ["--defaults-file=/m/run/mysql.cnf"]);
        assert_eq!(spec.stop, mysql_stop());
        assert_eq!(spec.stop.total_wait(), Duration::from_secs(30));
        assert_eq!(spec.restart, RestartPolicy::Never);
    }
}
