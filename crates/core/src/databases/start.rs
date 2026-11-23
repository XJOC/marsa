//! The check before a server starts: its lock files, its port, its pid file,
//! and the paths its settings file can hold.

use std::path::{Path, PathBuf};

use super::ServerKind;
use super::postgres::postmaster_pid;
use crate::orphan::Liveness;
use crate::ports::PortLookup;

/// Which of its lock files PostgreSQL found held by a live process at its
/// start, and that process's pid when its hint gave one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockConflict {
    /// `postmaster.pid`: another server runs on the data folder.
    DataDir { pid: Option<u32> },
    /// The lock of its socket, `<socket>.lock`: another server uses the
    /// socket of this port.
    Socket { pid: Option<u32> },
    /// The shared memory block that `postmaster.pid` names: processes of an
    /// earlier server on the data folder still use it. After a SIGKILL of
    /// the postmaster, its backends end only once they see that it is gone.
    SharedMemory,
}

/// PostgreSQL's own refusal to start because a live process holds one of
/// its lock files, or the shared memory of an earlier server on its data
/// folder, from `lines`: the stderr lines, oldest first, of a run that
/// ended by itself before it took connections.
///
/// The guard against text that a client wrote is that timing, not the
/// match: PostgreSQL takes its lock files before it accepts any connection,
/// so no client can write a line in that window, and the daemon reads no
/// line of a run that got further, or that it stopped itself. A line can
/// end with text a client sent (a `STATEMENT:` line does), so never read
/// these lines from a server that runs.
///
/// Only these lines count, matched to their end, so whatever
/// `log_line_prefix` puts first does not matter:
///
/// - `FATAL:  lock file "postmaster.pid" already exists`;
/// - `FATAL:  lock file "<socket>.lock" already exists`, with `socket` from
///   [`postgres_socket`];
/// - the pid only from the very next line, PostgreSQL's hint for that
///   error: `HINT:  Is another postmaster (PID <n>) running in data
///   directory "..."?` or `... using socket file "..."?`, or `postgres`
///   for a single-user server;
/// - `FATAL:  pre-existing shared memory block (key <n>, ID <n>) is still
///   in use`, both numbers digits only.
///
/// A line that starts with a tab is the rest of the line before it (a
/// value with a line break in it), never one of these.
///
/// The text is English: [`postgres_args`] sets `lc_messages=C`.
pub fn startup_lock_conflict(lines: &[&str], socket: Option<&Path>) -> Option<LockConflict> {
    let refusal = |name: &str| format!("FATAL:  lock file \"{name}\" already exists");
    let data_dir = refusal("postmaster.pid");
    let socket_lock = socket.map(|socket| refusal(&format!("{}.lock", socket.display())));
    for (index, line) in lines.iter().enumerate() {
        if line.starts_with('\t') {
            continue;
        }
        if is_shared_memory_refusal(line) {
            return Some(LockConflict::SharedMemory);
        }
        let next = lines.get(index + 1).copied();
        let hint = next.filter(|next| !next.starts_with('\t'));
        if line.ends_with(&data_dir) {
            let pid = hint.and_then(|hint| hint_pid(hint, "running in data directory \""));
            return Some(LockConflict::DataDir { pid });
        }
        if socket_lock
            .as_deref()
            .is_some_and(|lock| line.ends_with(lock))
        {
            let pid = hint.and_then(|hint| hint_pid(hint, "using socket file \""));
            return Some(LockConflict::Socket { pid });
        }
    }
    None
}

/// Whether `line` ends with PostgreSQL's refusal to start while processes
/// of an earlier server still use the shared memory block of its data
/// folder: `FATAL:  pre-existing shared memory block (key <n>, ID <n>) is
/// still in use`, both numbers digits only.
pub(super) fn is_shared_memory_refusal(line: &str) -> bool {
    let Some((_, rest)) = line.rsplit_once("FATAL:  pre-existing shared memory block (key ") else {
        return false;
    };
    let Some(numbers) = rest.strip_suffix(") is still in use") else {
        return false;
    };
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    numbers
        .split_once(", ID ")
        .is_some_and(|(key, id)| digits(key) && digits(id))
}

/// The pid in `HINT:  Is another postmaster (PID <n>) <then>...`.
pub(super) fn hint_pid(line: &str, then: &str) -> Option<u32> {
    let (_, hint) = line.split_once("HINT:  Is another ")?;
    let rest = hint
        .strip_prefix("postmaster (PID ")
        .or_else(|| hint.strip_prefix("postgres (PID "))?;
    let (digits, rest) = rest.split_once(") ")?;
    rest.starts_with(then).then(|| postmaster_pid(digits))?
}

/// Who holds the server's port, from the daemon's bind test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortCheck {
    Free,
    /// `owner` is the program's name, when the OS gives it.
    Busy {
        owner: Option<String>,
    },
    /// The bind worked, but the look for a listener on every address
    /// failed: a program there could be missed, so no start.
    Unknown,
}

/// The pid that the server's own pid file names, and what runs at it now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PidFileCheck {
    /// No file, or junk in it ([`postmaster_pid`] gave `None`).
    NoPid,
    /// Nothing runs at the pid: the file is stale.
    Dead { pid: u32 },
    /// Another program runs at the pid: it was reused, for example after a
    /// reboot.
    Other { pid: u32 },
    /// The server's own program runs at the pid.
    Server { pid: u32 },
    /// The OS could not tell. PostgreSQL may start then (see [`decide_start`]),
    /// but MySQL must refuse it: a second `mysqld` waits about 100 s for the
    /// InnoDB lock before it fails.
    Unknown { pid: u32 },
}

/// The facts the daemon gathers before it starts a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartFacts {
    pub port: u16,
    pub port_check: PortCheck,
    pub pid_file: PidFileCheck,
}

/// What the daemon does before it starts a server: start it, or refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartDecision {
    Start,
    /// Start nothing, stop nothing, and report this.
    Refuse(ServiceBusy),
}

/// Why a server may not start. [`ServiceBusy::code`] gives a stable name
/// that the UI maps to a message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServiceBusy {
    /// Another program (Homebrew, Herd, DBngin, Docker) holds the port.
    #[error("port {port} is in use by {}", .owner.as_deref().unwrap_or("another program"))]
    Port { port: u16, owner: Option<String> },
    /// Who holds the port could not be checked: `lsof` failed or took too
    /// long. Not "in use", which may not be true.
    #[error("could not check who holds port {port}")]
    PortUnknown { port: u16 },
    /// A running server still uses the data folder. `pid` is `None` when
    /// PostgreSQL refused at its start and named no pid.
    #[error(
        "the data folder is in use by a running server{}",
        .pid.map(|pid| format!(" (pid {pid})")).unwrap_or_default()
    )]
    DataDir { pid: Option<u32> },
    /// PostgreSQL: processes of an earlier server on the data folder still
    /// use its shared memory ([`LockConflict::SharedMemory`]). Usually the
    /// backends of a postmaster that SIGKILL ended: they end by themselves
    /// soon, so a start a moment later works.
    #[error("processes of an earlier PostgreSQL still use the shared memory of its data folder")]
    SharedMemory,
}

impl ServiceBusy {
    /// Stable error name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Port { .. } => crate::codes::SERVICE_PORT_BUSY,
            Self::PortUnknown { .. } => crate::codes::SERVICE_PORT_UNKNOWN,
            Self::DataDir { .. } => crate::codes::SERVICE_DATA_DIR_BUSY,
            Self::SharedMemory => crate::codes::POSTGRES_SHARED_MEMORY_BUSY,
        }
    }
}

/// The check before a start (ADR 0023, section 7):
///
/// 1. The pid file names a running server: `service_data_dir_busy`, even if
///    it holds the port too, because that is the exact reason. A later
///    change recovers it when it is our own orphan.
/// 2. Another program holds the port: `service_port_busy`. Or who holds it
///    could not be checked: `service_port_unknown`.
/// 3. Otherwise start. PostgreSQL checks its pid file again: it starts over a
///    stale one, but refuses while the pid is alive and runs as the same
///    user, even as another program ([`PidFileCheck::Other`]).
///
/// [`PidFileCheck::Unknown`] starts too, for PostgreSQL only: a second
/// postmaster cannot run on the folder, because PostgreSQL's
/// `CreateLockFile` refuses while `kill(pid, 0)` finds the pid, and its
/// shared memory check refuses while backends of a dead postmaster still
/// run. MySQL has no such quick refusal and needs its own rule.
pub fn decide_start(facts: &StartFacts) -> StartDecision {
    // Every case is listed, so a new one (such as our own orphan) needs a decision.
    match facts.pid_file {
        PidFileCheck::Server { pid } => {
            let pid = Some(pid);
            return StartDecision::Refuse(ServiceBusy::DataDir { pid });
        }
        PidFileCheck::NoPid
        | PidFileCheck::Dead { .. }
        | PidFileCheck::Other { .. }
        | PidFileCheck::Unknown { .. } => {}
    }
    let port = facts.port;
    match &facts.port_check {
        PortCheck::Free => StartDecision::Start,
        PortCheck::Busy { owner } => StartDecision::Refuse(ServiceBusy::Port {
            port,
            owner: owner.clone(),
        }),
        PortCheck::Unknown => StartDecision::Refuse(ServiceBusy::PortUnknown { port }),
    }
}

/// Whether a start of `kind` is refused, before anything runs, because the
/// daemon runs with an administrator's token (`elevated`, from
/// `platform::is_elevated`, which is `false` off Windows). PostgreSQL only:
/// `postgres.exe` refuses that token, and a start through `pg_ctl start`,
/// which drops the rights, would leave a server that the supervisor does not
/// own (ADR 0023, section 7). MySQL and Valkey run either way.
pub fn refuses_elevation(kind: ServerKind, elevated: bool) -> bool {
    elevated && kind == ServerKind::Postgres
}

/// [`PortCheck`] from the daemon's two looks at the port. Busy if either
/// says so; [`PortCheck::Unknown`], which refuses too, if the bind worked
/// and the look failed:
///
/// - `bound`: a bind to `127.0.0.1:<port>` worked. Alone it misses a program
///   on `*:<port>` on macOS: std's bind sets `SO_REUSEADDR`, and there that
///   lets a bind to one address share the port with a listener on all of
///   them (Docker, for one).
/// - `lookup`: who listens on the port, on any address, as `lsof` names it
///   (macOS). Any owner means busy. A failed look fails closed.
///   Linux does not look, and there the bind already fails next to a
///   listener on all addresses.
pub fn port_check(bound: bool, lookup: PortLookup) -> PortCheck {
    match (bound, lookup) {
        (_, PortLookup::Owner(owner)) => PortCheck::Busy { owner: Some(owner) },
        (true, PortLookup::Nobody | PortLookup::NotChecked) => PortCheck::Free,
        (true, PortLookup::Failed) => PortCheck::Unknown,
        (false, _) => PortCheck::Busy { owner: None },
    }
}

/// [`PidFileCheck`] of `pid`, the pid in `postmaster.pid`, from `now`: what
/// the OS says runs there by the file the process really runs
/// (`process::real_identity`), because postgres rewrites its own argv
/// (`postgres: checkpointer`). `servers` holds `bin/postgres` of every
/// installed PostgreSQL version, and the resolved form of each.
///
/// - Gone: [`PidFileCheck::Dead`], a stale file.
/// - Cannot be told: [`PidFileCheck::Unknown`].
/// - One of `servers`: [`PidFileCheck::Server`].
/// - Any other program: [`PidFileCheck::Other`], a pid reused since.
pub fn classify_postmaster(pid: u32, now: &Liveness, servers: &[PathBuf]) -> PidFileCheck {
    match now {
        Liveness::Gone => PidFileCheck::Dead { pid },
        Liveness::Unknown => PidFileCheck::Unknown { pid },
        Liveness::Alive { exe, .. } if servers.contains(exe) => PidFileCheck::Server { pid },
        Liveness::Alive { .. } => PidFileCheck::Other { pid },
    }
}

/// A path that a server's settings file cannot hold: one with a double quote
/// or a control character, or not UTF-8, and for Valkey on Windows one that
/// is not on a drive. Its own error, since no start can go on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UnwritablePath {
    #[error(
        "{} cannot be written to my.cnf: it has a double quote or a control character",
        .0.display()
    )]
    Mysql(PathBuf),
    #[error(
        "{} cannot be written to valkey.conf: it has a double quote or a control character",
        .0.display()
    )]
    Valkey(PathBuf),
    /// Valkey on Windows reads paths in Cygwin's form, which is made from a
    /// drive letter (`/cygdrive/c/...`).
    #[error(
        "{} cannot be given to Valkey on Windows: it is not a full path on a drive such as C:",
        .0.display()
    )]
    ValkeyNoDrive(PathBuf),
}

impl UnwritablePath {
    /// Stable error name for the UI.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Mysql(_) => crate::codes::MYSQL_PATH_UNSUPPORTED,
            Self::Valkey(_) | Self::ValkeyNoDrive(_) => crate::codes::VALKEY_PATH_UNSUPPORTED,
        }
    }
}

/// Whether `text` has a character that a double-quoted value of `my.cnf` or
/// `valkey.conf` cannot hold: a double quote, or a control character (a line
/// break would start a new setting).
pub(super) fn odd(text: &str) -> bool {
    text.contains(|c: char| c == '"' || c.is_control())
}

/// `path` as a value of `my.cnf` or `valkey.conf`: in double quotes, so a `#`
/// is not a comment and spaces stay part of it, with `\` written `\\` (both
/// read escapes in a quoted value). `refuse` makes the error of the file's
/// server.
pub(super) fn quoted_path(
    path: &Path,
    refuse: fn(PathBuf) -> UnwritablePath,
) -> Result<String, UnwritablePath> {
    let text = path
        .to_str()
        .filter(|text| !odd(text))
        .ok_or_else(|| refuse(path.to_path_buf()))?;
    Ok(format!("\"{}\"", text.replace('\\', "\\\\")))
}
