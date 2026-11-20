//! What only PostgreSQL needs: its programs, the first init of its
//! cluster, how it runs, how it says it is ready, and how it says that a
//! lock file is held.
//!
//! The first start of a major line makes its cluster in
//! `databases/postgres/<major>/` ([`ensure_cluster`]): `initdb`, then Marsa's
//! `pg_hba.conf`, then the role `root` in single-user mode.

use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use marsa_core::databases::{
    LockConflict, PG_HBA_CONF, PostgresDirs, ROOT_ROLE_SQL, ServerKind, ServiceBusy, initdb_args,
    postgres_args, postgres_stop, postmaster_pid, single_user_args, startup_lock_conflict,
};
use marsa_core::orphan::Liveness;
use marsa_core::platform::process;
use marsa_core::recipes::Platform;
use marsa_core::services::{RestartPolicy, ServiceSpec};
use semver::Version;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;

use super::facts::Probes;
use super::init::{self, InitError, Step, ensure_data, run_step};

const KIND: ServerKind = ServerKind::Postgres;
/// How long `initdb` may run. About a second on a laptop (S-12); a slow disk
/// or a busy CI machine needs more.
const INITDB_LIMIT: Duration = Duration::from_secs(120);
/// How long the single-user `postgres` that makes `root` may run.
const SINGLE_USER_LIMIT: Duration = Duration::from_secs(60);
/// PostgreSQL's severities: a line with one fails a step of the init.
const SEVERE: [&str; 3] = ["ERROR:", "FATAL:", "PANIC:"];
/// For each attempt of the readiness check.
const CONNECT_LIMIT: Duration = Duration::from_millis(500);
/// An `SSLRequest`: its length, 8, then the code 80877103. The first message
/// a client may send, before any user name.
pub(super) const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f];

/// The programs of one installed PostgreSQL version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Programs {
    pub postgres: PathBuf,
    pub initdb: PathBuf,
}

/// Where the recipe puts the programs of the installed PostgreSQL `version`:
/// `runtimes/postgres/<version>/bin/`. `None` if the recipe has none for
/// `platform`.
pub(super) fn programs(data_dir: &Path, platform: Platform, version: &Version) -> Option<Programs> {
    Some(Programs {
        postgres: init::server(KIND, data_dir, platform, version)?,
        initdb: init::program(KIND, data_dir, platform, version, "initdb")?,
    })
}

/// How the supervisor runs PostgreSQL: `postgres` with [`postgres_args`],
/// so on `127.0.0.1:<port>` only and with its socket in `run/`, stopped with
/// SIGINT, then SIGQUIT, never SIGTERM ([`postgres_stop`]). PostgreSQL logs
/// to stderr, which the supervisor keeps in `logs/postgres.log`, as for
/// every service.
///
/// Never restarted after it exits, unlike Caddy. A refusal at its start
/// (another server on the folder, the port, or the socket) would only come
/// again, the status must keep the run's own exit, and after a crash of the
/// postmaster its backends may still hold the shared memory, so a restart
/// fails too. PostgreSQL restarts its own crashed backends itself.
pub(super) fn spec(program: PathBuf, dirs: &PostgresDirs, port: u16) -> ServiceSpec {
    ServiceSpec {
        id: KIND.id().to_owned(),
        program,
        args: postgres_args(dirs, port),
        env: std::collections::BTreeMap::new(),
        cwd: None,
        restart: RestartPolicy::Never,
        stop: postgres_stop(),
    }
}

/// Makes the cluster in `dirs.data` if nothing is there yet
/// ([`ensure_data`]), and returns whether it made one. The steps, in the
/// temp folder:
///
/// 1. `initdb` with [`initdb_args`];
/// 2. [`PG_HBA_CONF`] over initdb's `pg_hba.conf`;
/// 3. `postgres --single` with [`ROOT_ROLE_SQL`] on stdin.
///
/// # Errors
///
/// `postgres_init_failed`, with the step and the end of its output.
pub(super) async fn ensure_cluster(
    programs: &Programs,
    dirs: &PostgresDirs,
) -> Result<bool, InitError> {
    ensure_data(KIND, &dirs.data, async |temp: &Path| {
        init_in(programs, dirs, temp).await
    })
    .await
}

/// Steps 1 to 3 of [`ensure_cluster`] in `temp`.
async fn init_in(programs: &Programs, dirs: &PostgresDirs, temp: &Path) -> Result<(), InitError> {
    let in_temp = PostgresDirs {
        data: temp.to_path_buf(),
        sockets: dirs.sockets.clone(),
    };
    let mut initdb = Command::new(&programs.initdb);
    initdb.args(initdb_args(&in_temp));
    let step = |what, input, limit| Step {
        kind: KIND,
        what,
        input,
        limit,
        severe: &SEVERE,
    };
    run_step(initdb, step("initdb", None, INITDB_LIMIT)).await?;

    write_hba(&temp.join("pg_hba.conf"))
        .await
        .map_err(|err| InitError::io(KIND, "cannot write pg_hba.conf", &err))?;

    let mut single = Command::new(&programs.postgres);
    single.args(single_user_args(temp));
    let sql = Some(ROOT_ROLE_SQL);
    run_step(single, step("postgres --single", sql, SINGLE_USER_LIMIT)).await
}

/// Writes Marsa's `pg_hba.conf` over initdb's, which keeps its mode (`0600`),
/// and syncs it: the cluster is renamed into place next.
async fn write_hba(path: &Path) -> io::Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .await?;
    file.write_all(PG_HBA_CONF.as_bytes()).await?;
    file.sync_all().await
}

/// Whether PostgreSQL takes connections on `127.0.0.1:<port>`: it answers
/// an [`SSL_REQUEST`] with one byte, `N` or `S`. Only a postmaster whose
/// start is done answers: it has taken its lock files and made all its
/// sockets. A bare TCP connection is not enough, because the TCP listener
/// is made before the socket and its lock file. PostgreSQL logs nothing for
/// this: the connection closes before any startup message.
pub(super) async fn answers(port: u16) -> bool {
    let ask = async {
        let address = (Ipv4Addr::LOCALHOST, port);
        let mut stream = tokio::net::TcpStream::connect(address).await.ok()?;
        stream.write_all(&SSL_REQUEST).await.ok()?;
        let mut answer = [0_u8; 1];
        stream.read_exact(&mut answer).await.ok()?;
        Some(matches!(&answer, b"N" | b"S"))
    };
    matches!(
        tokio::time::timeout(CONNECT_LIMIT, ask).await,
        Ok(Some(true))
    )
}

/// Why a run that ended by itself before it took connections did not
/// start, when PostgreSQL refused because a live process holds one of its
/// lock files ([`startup_lock_conflict`] on `stderr`, this run's stderr
/// lines), with the program at that pid now:
///
/// - `postmaster.pid`: `service_data_dir_busy`, as the check before the
///   start reports it. The pid comes from PostgreSQL's hint, else from
///   `postmaster.pid` (read only), and may stay unknown;
/// - the lock of the socket: `service_port_busy`, the server that uses the
///   socket of this port.
///
/// Only read once the run has ended: a line never stops a server. That
/// process gets no signal, and `postmaster.pid` stays: the pid may be
/// another program's now (`orphan::clear` handles our own).
pub(super) async fn lock_refusal(
    probes: &dyn Probes,
    stderr: &[&str],
    data: &Path,
    socket: Option<&Path>,
    port: u16,
) -> Option<(ServiceBusy, String)> {
    let conflict = startup_lock_conflict(stderr, socket)?;
    let no_signal = "and Marsa sends it no signal";
    match conflict {
        LockConflict::DataDir { pid } => {
            let pid = pid.or_else(|| {
                let text = process::read_small_file(&data.join("postmaster.pid"))?;
                postmaster_pid(&text)
            });
            let busy = ServiceBusy::DataDir { pid };
            let reason = match pid {
                Some(pid) => {
                    let program = program_at(probes, pid).await;
                    format!("{busy}; pid {pid} runs {program} now, {no_signal}")
                }
                None => format!("{busy}; PostgreSQL named no pid, {no_signal}"),
            };
            Some((busy, reason))
        }
        LockConflict::Socket { pid } => {
            let owner = match pid {
                Some(pid) => Some(format!("{} (pid {pid})", program_at(probes, pid).await)),
                None => None,
            };
            let busy = ServiceBusy::Port { port, owner };
            let lock = socket.map(|socket| socket.display().to_string());
            let lock = lock.unwrap_or_default();
            let reason = format!("{busy}: it holds the lock of the socket {lock}, {no_signal}");
            Some((busy, reason))
        }
    }
}

/// The program at `pid` now, in words, for a message.
async fn program_at(probes: &dyn Probes, pid: u32) -> String {
    match probes.real_identity(pid).await {
        Liveness::Alive { exe, .. } => exe.display().to_string(),
        Liveness::Gone => "nothing (it has ended since)".to_owned(),
        Liveness::Unknown => "a program that cannot be told".to_owned(),
    }
}
