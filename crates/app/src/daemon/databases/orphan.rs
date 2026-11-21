//! A server that a crashed `marsad` left running (ADR 0023, section 8; the
//! rules of ADR 0009). macOS and Linux only
//! ([`process::DATABASE_ORPHAN_RECOVERY`]): on Windows the Job Object ends
//! the servers with the daemon (ADR 0017).
//!
//! `run/<id>.pid` (`run/postgres.pid`) holds the pid, the file it really
//! runs, and its start time, written as soon as the spawned process runs our
//! server program, and deleted when that run ends. Before each start, and at a stop when this
//! daemon runs no server, the file is read and the process at that pid is
//! checked again ([`marsa_core::orphan::classify_pidfile`]):
//!
//! - ours (one of the installed server programs of this data folder,
//!   started at that time): stopped with the server's own ladder
//!   ([`ServerKind::stop`]; for PostgreSQL SIGINT, then SIGQUIT, never
//!   SIGTERM), then SIGKILL, with the check again before each signal;
//! - gone, or another program now: the file is deleted, nothing is
//!   signalled;
//! - cannot be told (another user's process, which `lsof` does not show):
//!   no signal, and the file stays. A start goes on to its own checks: our
//!   server would still hold the port and its own pid file, which refuse it.
//!
//! A port, a protocol answer, or the server's own pid file
//! (`postmaster.pid`) never authorizes a signal, and that file is never
//! deleted here.

use marsa_core::codes;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use marsa_core::databases::ServerKind;
use marsa_core::orphan::{Liveness, PidRecord, PidfileState, classify_pidfile};
use marsa_core::platform::process::{self, Signal};
use marsa_core::services::{StopAction, StopMethod};
use tracing::{debug, info, warn};

use crate::daemon::services::probe::ProcessProbe;

/// Our old server could not be stopped: nothing was started.
pub(super) fn stuck_code(kind: ServerKind) -> &'static str {
    match kind {
        ServerKind::Postgres => codes::POSTGRES_ORPHAN_STUCK,
    }
}

/// How often a wait looks again.
const POLL: Duration = Duration::from_millis(50);
/// How long, after the orphan is gone, the port may take to be free.
const PORT_WAIT: Duration = Duration::from_secs(5);
/// How long a SIGKILL may take to end the process.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// `run/<id>.pid` of `data_dir`.
pub(super) fn pid_path(kind: ServerKind, data_dir: &Path) -> PathBuf {
    data_dir.join("run").join(format!("{}.pid", kind.id()))
}

/// Writes `run/<id>.pid` for our server at `pid`, only when the file it
/// really runs is one of `servers`. `false` when it was not written: right
/// after the spawn the process may not run the server yet (before its
/// `exec`), or the system could not tell this time, so the caller asks
/// again.
pub(super) async fn record(
    kind: ServerKind,
    probes: &dyn ProcessProbe,
    data_dir: &Path,
    servers: &[PathBuf],
    pid: u32,
) -> bool {
    if !process::DATABASE_ORPHAN_RECOVERY {
        return true;
    }
    match probes.real_identity(pid).await {
        Liveness::Alive { exe, started } if servers.contains(&exe) => {
            let record = PidRecord { pid, exe, started };
            match process::write_pid_file(&pid_path(kind, data_dir), &record) {
                Ok(()) => debug!(pid, server = kind.id(), "the pid record is written"),
                Err(err) => warn!(%err, server = kind.id(), "cannot write the pid record"),
            }
            true
        }
        other => {
            debug!(
                pid,
                ?other,
                server = kind.id(),
                "the pid record is not written yet"
            );
            false
        }
    }
}

/// Deletes `run/<id>.pid` when it names `pid`, a run that has ended. A
/// record of another pid (a newer run, or one that could not be told) stays.
pub(super) fn forget_run(kind: ServerKind, data_dir: &Path, pid: u32) {
    let path = pid_path(kind, data_dir);
    if process::read_pid_file(&path).is_some_and(|record| record.pid == pid) {
        forget(kind, data_dir);
    }
}

/// Deletes `run/<id>.pid`.
fn forget(kind: ServerKind, data_dir: &Path) {
    if let Err(err) = process::remove_pid_file(&pid_path(kind, data_dir)) {
        warn!(%err, server = kind.id(), "cannot delete the pid record");
    }
}

/// Why the start cannot go on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Stuck(pub(super) String);

/// What [`clear`] did, for the recent lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Cleared {
    /// No file, or one that cannot be told: nothing was signalled.
    Nothing,
    /// The file named a process that is gone or another program's now: only
    /// the file was deleted.
    Forgotten,
    /// Our old server ended after `signal`, the last one sent (`None`: it
    /// ended before any).
    Stopped { pid: u32, signal: Option<Signal> },
}

/// Finds the server that `run/<id>.pid` names, and stops it when it is
/// ours, by `ladder` ([`ServerKind::stop`]) and a final SIGKILL, then waits
/// for `port`.
///
/// # Errors
///
/// [`Stuck`], with the reason, when our server still runs after SIGKILL, or
/// cannot be told from another program after a signal. Nothing else is
/// signalled then.
pub(super) async fn clear(
    kind: ServerKind,
    probes: &dyn ProcessProbe,
    data_dir: &Path,
    servers: &[PathBuf],
    ladder: &StopMethod,
    port: u16,
) -> Result<Cleared, Stuck> {
    if !process::DATABASE_ORPHAN_RECOVERY {
        return Ok(Cleared::Nothing);
    }
    let Some(record) = process::read_pid_file(&pid_path(kind, data_dir)) else {
        return Ok(Cleared::Nothing);
    };
    let pid = record.pid;
    let state = check(probes, &record, servers).await;
    info!(pid, %state, server = kind.id(), "checked the pid record");
    match state {
        PidfileState::Missing | PidfileState::Stale { .. } | PidfileState::Mismatch { .. } => {
            forget(kind, data_dir);
            return Ok(Cleared::Forgotten);
        }
        // Often another user's process at a reused pid, after a reboot.
        PidfileState::Unknown { .. } => {
            warn!(
                pid,
                server = kind.id(),
                "cannot tell whether the pid record names our old server; no signal"
            );
            return Ok(Cleared::Nothing);
        }
        PidfileState::Ours { .. } => {}
    }
    let signals = ladder
        .steps
        .iter()
        .map(|step| match step.action {
            StopAction::Signal(signal) => (signal, step.wait),
        })
        .chain([(Signal::Kill, KILL_WAIT)]);
    // The last signal sent, for the line that says how it stopped.
    let mut sent = None;
    for (signal, wait) in signals {
        match check(probes, &record, servers).await {
            PidfileState::Ours { .. } => {}
            PidfileState::Unknown { .. } => {
                let reason = format!(
                    "our old {} (pid {pid}) did not stop, and the system cannot tell whether \
                     that pid is still it, so it gets no {}",
                    kind.name(),
                    signal.name()
                );
                return Err(Stuck(reason));
            }
            // Gone, or someone else's now: it ended between two checks.
            _ => return Ok(done(kind, data_dir, pid, sent, port).await),
        }
        warn!(
            pid,
            signal = signal.name(),
            server = kind.id(),
            "stopping our old server"
        );
        if let Err(err) = probes.signal(pid, signal).await {
            warn!(pid, signal = signal.name(), %err, "the signal failed");
        }
        sent = Some(signal);
        if gone(probes, &record, servers, wait).await {
            return Ok(done(kind, data_dir, pid, sent, port).await);
        }
    }
    let reason = format!(
        "our old {} (pid {pid}) still runs after SIGKILL",
        kind.name()
    );
    Err(Stuck(reason))
}

/// The file against what the system says now: the file the process really
/// runs, and its start time.
async fn check(probes: &dyn ProcessProbe, record: &PidRecord, servers: &[PathBuf]) -> PidfileState {
    let now = probes.real_identity(record.pid).await;
    classify_pidfile(record, servers, &now)
}

/// Polls until the process is not our server any more, at most `limit`. An
/// answer that cannot be told waits on: it may be a `ps` that failed once.
async fn gone(
    probes: &dyn ProcessProbe,
    record: &PidRecord,
    servers: &[PathBuf],
    limit: Duration,
) -> bool {
    let started = Instant::now();
    loop {
        match check(probes, record, servers).await {
            PidfileState::Ours { .. } | PidfileState::Unknown { .. } => {}
            _ => return true,
        }
        if started.elapsed() >= limit {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// After the orphan ended: the file goes, then a short wait for the port, so
/// the check before a start does not find it busy with a listener that is
/// closing.
async fn done(
    kind: ServerKind,
    data_dir: &Path,
    pid: u32,
    signal: Option<Signal>,
    port: u16,
) -> Cleared {
    info!(
        pid,
        signal = signal.map(Signal::name),
        server = kind.id(),
        "our old server stopped"
    );
    forget(kind, data_dir);
    let started = Instant::now();
    while TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err() && started.elapsed() < PORT_WAIT {
        tokio::time::sleep(POLL).await;
    }
    Cleared::Stopped { pid, signal }
}

/// The line for `marsa logs <id>` after [`clear`] stopped our old server.
pub(super) fn stopped_line(kind: ServerKind, pid: u32, signal: Option<Signal>) -> String {
    let how = match (kind, signal) {
        (_, None) => "it ended before a signal".to_owned(),
        (ServerKind::Postgres, Some(Signal::Kill)) => "it needed SIGKILL, so its backends may \
                                                       still hold its shared memory for a \
                                                       moment; if PostgreSQL then does not \
                                                       start, start it again"
            .to_owned(),
        (_, Some(signal)) => format!("it ended after {}", signal.name()),
    };
    format!(
        "marsad: stopped the {} (pid {pid}) that an earlier marsad left running; {how}.",
        kind.name()
    )
}
