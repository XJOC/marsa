//! What only PostgreSQL needs: its programs, the first init of its
//! cluster, how it runs, how it says it is ready, and how it says that a
//! lock file is held.
//!
//! The first start of a major line makes its cluster in
//! `databases/postgres/<major>/` ([`ensure_cluster`]): `initdb`, then Marsa's
//! `pg_hba.conf`, then the role `root` in single-user mode.

use marsa_core::types::ServiceId;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use marsa_core::databases::{
    LockConflict, PG_HBA_CONF, PostgresDirs, ROOT_ROLE_SQL, ServerKind, ServiceBusy, initdb_args,
    postgres_args, postmaster_pid, single_user_args, startup_lock_conflict,
};
use marsa_core::orphan::Liveness;
use marsa_core::platform::process;
use marsa_core::recipes::{Os, Platform};
use marsa_core::services::{RestartPolicy, ServiceSpec, StopMethod};
use semver::Version;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;

use super::init::{self, InitError, Step, ensure_data, run_step};
use crate::daemon::services::probe::ProcessProbe;

const KIND: ServerKind = ServerKind::Postgres;
/// How long `initdb` may run. About a second on a laptop (measured); a slow disk
/// or a busy CI machine needs more.
const INITDB_LIMIT: Duration = Duration::from_secs(120);
/// How long the single-user `postgres` that makes `root` may run.
const SINGLE_USER_LIMIT: Duration = Duration::from_secs(60);
/// PostgreSQL's severities: a line with one fails a step of the init.
const SEVERE: [&str; 3] = ["ERROR:", "FATAL:", "PANIC:"];
/// For each attempt of the readiness check, a whole session start: a busy
/// Windows machine can take more than a few hundred milliseconds to make the
/// backend process.
const CONNECT_LIMIT: Duration = Duration::from_secs(2);
/// The longest message of a session start that the check reads.
const REPLY_LIMIT: usize = 8 * 1024;
/// A `Terminate` message: the client ends the session.
const TERMINATE: [u8; 5] = [b'X', 0, 0, 0, 4];
/// `cannot_connect_now`: the server starts up, recovers, or shuts down.
const CANNOT_CONNECT_NOW: &[u8] = b"57P03";

/// The programs of one installed PostgreSQL version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Programs {
    pub postgres: PathBuf,
    pub initdb: PathBuf,
    /// The stop on Windows ([`marsa_core::databases::postgres_windows_stop`]).
    pub pg_ctl: PathBuf,
}

/// Where the recipe puts the programs of the installed PostgreSQL `version`:
/// `runtimes/postgres/<version>/bin/`. `None` if the recipe has none for
/// `platform`.
pub(super) fn programs(data_dir: &Path, platform: Platform, version: &Version) -> Option<Programs> {
    Some(Programs {
        postgres: init::server(KIND, data_dir, platform, version)?,
        initdb: init::program(KIND, data_dir, platform, version, "initdb")?,
        pg_ctl: init::program(KIND, data_dir, platform, version, "pg_ctl")?,
    })
}

/// How the supervisor runs PostgreSQL: `postgres` with [`postgres_args`],
/// so on `127.0.0.1:<port>` only and with its socket in `run/`, stopped by
/// `stop`: on macOS and Linux SIGINT, then SIGQUIT, never SIGTERM
/// ([`marsa_core::databases::postgres_stop`]); on Windows `pg_ctl stop -m fast`
/// ([`marsa_core::databases::postgres_windows_stop`]). PostgreSQL logs to stderr, which the
/// supervisor keeps in `logs/postgres.log`, as for every service.
///
/// Never restarted after it exits, unlike Caddy. A refusal at its start
/// (another server on the folder, the port, or the socket) would only come
/// again, the status must keep the run's own exit, and after a crash of the
/// postmaster its backends may still hold the shared memory, so a restart
/// fails too. PostgreSQL restarts its own crashed backends itself.
pub(super) fn spec(
    program: PathBuf,
    dirs: &PostgresDirs,
    port: u16,
    stop: StopMethod,
) -> ServiceSpec {
    ServiceSpec {
        id: ServiceId::Database(KIND),
        program,
        args: postgres_args(dirs, port),
        env: std::collections::BTreeMap::new(),
        env_remove: Vec::new(),
        cwd: None,
        restart: RestartPolicy::Never,
        stop,
        own_group: false,
    }
}

/// Makes the cluster in `dirs.data` if nothing is there yet
/// ([`ensure_data`]), and returns whether it made one. The steps, in the
/// temp folder:
///
/// 1. `initdb` with [`initdb_args`] for `os`;
/// 2. [`PG_HBA_CONF`] over initdb's `pg_hba.conf`;
/// 3. `postgres --single` with [`ROOT_ROLE_SQL`] on stdin.
///
/// # Errors
///
/// `postgres_init_failed`, with the step and the end of its output.
pub(super) async fn ensure_cluster(
    programs: &Programs,
    dirs: &PostgresDirs,
    os: Os,
) -> Result<bool, InitError> {
    ensure_data(KIND, &dirs.data, async |temp: &Path| {
        init_in(programs, dirs, temp, os).await
    })
    .await
}

/// Steps 1 to 3 of [`ensure_cluster`] in `temp`.
async fn init_in(
    programs: &Programs,
    dirs: &PostgresDirs,
    temp: &Path,
    os: Os,
) -> Result<(), InitError> {
    let in_temp = PostgresDirs {
        data: temp.to_path_buf(),
        sockets: dirs.sockets.clone(),
    };
    let mut initdb = Command::new(&programs.initdb);
    initdb.args(initdb_args(&in_temp, os));
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

/// The `StartupMessage` of protocol 3.0 for the role `root`, which Marsa's
/// `pg_hba.conf` lets in over TCP, and the database `postgres`, which every
/// cluster has.
pub(super) fn startup_message() -> Vec<u8> {
    const PROTOCOL_3_0: u32 = 3 << 16;
    const PARAMETERS: &[u8] = b"user\0root\0database\0postgres\0\0";
    // The length counts itself and the protocol number.
    let length = 8 + PARAMETERS.len() as u32;
    let mut message = Vec::with_capacity(length as usize);
    message.extend_from_slice(&length.to_be_bytes());
    message.extend_from_slice(&PROTOCOL_3_0.to_be_bytes());
    message.extend_from_slice(PARAMETERS);
    message
}

/// Whether PostgreSQL takes connections on `127.0.0.1:<port>`: it takes a
/// [`startup_message`] up to `ReadyForQuery`. An open port or an answer to
/// an `SSLRequest` is not enough: PostgreSQL gives both while it still
/// refuses every session with "the database system is starting up".
///
/// As `pg_isready` does, a server that asks for a password, or refuses this
/// session for any reason but [`CANNOT_CONNECT_NOW`], also counts: the user
/// may have changed `pg_hba.conf` or dropped the role, and that server runs.
pub(super) async fn answers(port: u16) -> bool {
    let ask = async {
        let address = (Ipv4Addr::LOCALHOST, port);
        let mut stream = tokio::net::TcpStream::connect(address).await.ok()?;
        stream.write_all(&startup_message()).await.ok()?;
        takes_session(&mut stream).await
    };
    matches!(
        tokio::time::timeout(CONNECT_LIMIT, ask).await,
        Ok(Some(true))
    )
}

/// Reads the server's messages to a [`startup_message`] until they say
/// whether it takes sessions ([`answers`]); `None` when they cannot be read.
async fn takes_session(stream: &mut tokio::net::TcpStream) -> Option<bool> {
    loop {
        let kind = stream.read_u8().await.ok()?;
        // The length counts its own four bytes.
        let length = usize::try_from(stream.read_u32().await.ok()?).ok()?;
        let length = length.checked_sub(4).filter(|n| *n <= REPLY_LIMIT)?;
        let mut body = vec![0; length];
        stream.read_exact(&mut body).await.ok()?;
        match kind {
            // `Authentication`: any code but 0 asks for a password.
            b'R' if body.get(..4)? != [0; 4] => return Some(true),
            // Accepted, then the settings, the cancel key, and notices.
            b'R' | b'S' | b'K' | b'N' | b'v' => {}
            b'Z' => {
                // Ended properly, so PostgreSQL logs nothing. The answer is
                // known, whether this arrives or not.
                let _ = stream.write_all(&TERMINATE).await;
                return Some(true);
            }
            b'E' => return Some(sqlstate(&body)? != CANNOT_CONNECT_NOW),
            _ => return None,
        }
    }
}

/// The SQLSTATE of an `ErrorResponse`: its field `C`. Each field is one
/// letter, its text, and a zero byte.
fn sqlstate(fields: &[u8]) -> Option<&[u8]> {
    let code = fields
        .split(|byte| *byte == 0)
        .find_map(|field| field.strip_prefix(b"C"))?;
    (code.len() == 5).then_some(code)
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
///   socket of this port;
/// - the shared memory of the data folder, still used by processes of an
///   earlier server (the backends of a postmaster that SIGKILL ended):
///   `postgres_shared_memory_busy`. They end by themselves, so a later
///   start works; nothing is signalled, because no record names them.
///
/// Only read once the run has ended: a line never stops a server. That
/// process gets no signal, and `postmaster.pid` stays: the pid may be
/// another program's now (`pid_record::clear` handles our own).
pub(super) async fn lock_refusal(
    probes: &dyn ProcessProbe,
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
        // No pid is named, and none is looked for: the backends of a
        // postmaster that SIGKILL ended have no record of ours.
        LockConflict::SharedMemory => {
            let busy = ServiceBusy::SharedMemory;
            let reason = format!(
                "{busy}; they end by themselves once they see that their server is gone, so \
                 start it again in a moment; Marsa sends them no signal"
            );
            Some((busy, reason))
        }
    }
}

/// The program at `pid` now, in words, for a message.
async fn program_at(probes: &dyn ProcessProbe, pid: u32) -> String {
    match probes.real_identity(pid).await {
        Liveness::Alive { exe, .. } => exe.display().to_string(),
        Liveness::Gone => "nothing (it has ended since)".to_owned(),
        Liveness::Unknown => "a program that cannot be told".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    /// One message of the server: its kind, its length, and its body.
    fn message(kind: u8, body: &[u8]) -> Vec<u8> {
        let length = u32::try_from(body.len() + 4).unwrap();
        [&[kind][..], &length.to_be_bytes(), body].concat()
    }

    fn error(code: &str, text: &str) -> Vec<u8> {
        let fields = format!("SFATAL\0VFATAL\0C{code}\0M{text}\0\0");
        message(b'E', fields.as_bytes())
    }

    fn authentication(code: u32) -> Vec<u8> {
        message(b'R', &code.to_be_bytes())
    }

    /// A server on `127.0.0.1` that takes one connection, reads the startup
    /// message, sends `reply`, and closes once the client has. It returns what
    /// the client sent after the startup message.
    async fn fake(reply: Vec<u8>) -> (u16, JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut startup = vec![0; startup_message().len()];
            stream.read_exact(&mut startup).await.unwrap();
            assert_eq!(startup, startup_message());
            stream.write_all(&reply).await.unwrap();
            // It fails on macOS when the client has closed already.
            let _ = stream.shutdown().await;
            let mut rest = Vec::new();
            // A client that closes with replies unread resets the connection.
            let _ = stream.read_to_end(&mut rest).await;
            rest
        });
        (port, served)
    }

    async fn answer_to(reply: Vec<u8>) -> bool {
        let (port, served) = fake(reply).await;
        let answer = answers(port).await;
        served.await.unwrap();
        answer
    }

    #[test]
    fn the_startup_message_is_protocol_3_for_root() {
        let message = startup_message();
        assert_eq!(message[..4], (message.len() as u32).to_be_bytes());
        assert_eq!(message[4..8], [0, 3, 0, 0]);
        assert_eq!(message[8..], *b"user\0root\0database\0postgres\0\0");
    }

    #[tokio::test]
    async fn a_whole_session_start_is_ready_and_is_ended() {
        let reply = [
            authentication(0),
            message(b'S', b"server_version\x0018.7\0"),
            message(b'N', b"SNOTICE\0\0"),
            message(b'K', &[0; 8]),
            message(b'Z', b"I"),
        ]
        .concat();
        let (port, served) = fake(reply).await;
        assert!(answers(port).await);
        assert_eq!(served.await.unwrap(), TERMINATE);
    }

    #[tokio::test]
    async fn an_open_port_that_says_starting_up_is_not_ready() {
        for text in [
            "the database system is starting up",
            "the database system is in recovery mode",
            "the database system is shutting down",
        ] {
            assert!(!answer_to(error("57P03", text)).await, "{text}");
        }
    }

    /// The user changed `pg_hba.conf` or dropped the role: the server runs.
    #[tokio::test]
    async fn a_server_that_refuses_this_session_for_another_reason_is_ready() {
        assert!(answer_to(error("28000", "pg_hba.conf rejects connection")).await);
        assert!(answer_to(error("53300", "sorry, too many clients already")).await);
        let after_auth = [authentication(0), error("3D000", "no database")].concat();
        assert!(answer_to(after_auth).await);
        // `AuthenticationSASL`.
        assert!(answer_to(authentication(10)).await);
    }

    #[tokio::test]
    async fn a_reply_that_cannot_be_read_is_not_ready() {
        let unreadable = [
            Vec::new(),
            // The answer to an `SSLRequest`, not to a startup message.
            b"N".to_vec(),
            // Accepted, then closed before `ReadyForQuery`.
            authentication(0),
            message(b'R', &[0, 0]),
            // No SQLSTATE.
            message(b'E', b"SFATAL\0\0"),
            // The postmaster's failed fork: no length, only words.
            b"Ecould not fork new process for connection\0".to_vec(),
            [&b"E"[..], &3_u32.to_be_bytes()].concat(),
            message(b'?', b""),
        ];
        for reply in unreadable {
            assert!(!answer_to(reply.clone()).await, "{reply:?}");
        }
    }

    #[tokio::test]
    async fn a_silent_port_and_a_closed_one_are_not_ready() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // The system takes the connection; nothing ever answers.
        assert!(!answers(port).await);
        drop(listener);
        assert!(!answers(port).await);
    }
}
