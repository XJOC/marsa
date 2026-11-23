//! The check before a server starts (ADR 0023, section 7): who holds its
//! port, what its pid file names, and whether the start goes on.

use std::path::PathBuf;

use super::ServerKind;
use crate::orphan::Liveness;
use crate::ports::PortLookup;

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
    /// No file, or junk in it ([`super::postmaster_pid`] gave `None`).
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
    /// use its shared memory ([`super::LockConflict::SharedMemory`]). Usually the
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
