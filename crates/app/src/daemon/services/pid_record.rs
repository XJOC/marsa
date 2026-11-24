//! One process that a crashed `marsad` left running, named by its pid record
//! `run/<id>.pid` (the rules of ADR 0009; for the database servers ADR
//! 0023, section 8). Before a start, [`clear`] reads the record and checks
//! the process at that pid again ([`marsa_core::orphan::classify_pidfile`]):
//!
//! - ours (one of the accepted programs of this data folder, started at
//!   that time): stopped by the signals of its stop method, then SIGKILL,
//!   with the check again before each signal;
//! - gone, or another program now: the record is deleted, nothing is
//!   signalled;
//! - cannot be told: no signal, and the record stays.
//!
//! The database servers and the sites' workers write their records their
//! own way; this module only reads and acts on one.

use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use marsa_core::orphan::{PidRecord, PidfileState, classify_pidfile};
use marsa_core::platform::process::{self, Signal};
use marsa_core::services::{StopAction, StopMethod};
use marsa_core::types::ServiceId;
use tracing::{debug, info, warn};

use crate::daemon::services::probe::ProcessProbe;

/// How often a wait looks again.
const POLL: Duration = Duration::from_millis(50);
/// How long, after the orphan is gone, its port may take to be free.
const PORT_WAIT: Duration = Duration::from_secs(5);
/// How long a SIGKILL may take to end the process.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// Whose pid record [`clear`] reads.
#[derive(Debug, Clone)]
pub(crate) struct Owner<'a> {
    /// For the daemon's log: `postgres`, `queue-shop`.
    pub(crate) id: ServiceId,
    /// In the reasons of [`Stuck`]: `PostgreSQL`, `queue worker`.
    pub(crate) name: &'a str,
    /// `run/<id>.pid`.
    pub(crate) pid_file: PathBuf,
    /// The port it listens on, waited for once it is gone. `None`: no port.
    pub(crate) port: Option<u16>,
    /// The system recovers orphans of this kind
    /// (`process::DATABASE_ORPHAN_RECOVERY`): else nothing is read.
    pub(crate) recovery: bool,
}

/// The steps of `ladder` that are signals, with their waits. A request to the
/// server's port and a stop program are left out: they find the server by its
/// port or its data folder, which does not prove that the process of the pid
/// record is the one that holds them.
pub(crate) fn signal_steps(ladder: &StopMethod) -> Vec<(Signal, Duration)> {
    ladder
        .steps
        .iter()
        .filter_map(|step| match step.action {
            StopAction::Signal(signal) => Some((signal, step.wait)),
            StopAction::Send(_) | StopAction::Run(_) => None,
        })
        .collect()
}

/// Why a start cannot go on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stuck(pub(crate) String);

/// How many times a start looks at the pid of the record. It goes on to its
/// own checks after a look that cannot tell: a server of ours would still
/// hold its port and its own pid file, which refuse it.
pub(crate) const START_LOOKS: u32 = 1;
/// How many times a stop looks, when the system cannot tell who runs the
/// pid, before it goes on without a signal: a `ps` or `lsof` may fail once,
/// and a stop has no later check to fall back on.
pub(crate) const STOP_LOOKS: u32 = 3;
/// How long between two of those looks.
const LOOK_AGAIN: Duration = Duration::from_millis(250);

/// What [`clear`] did, for the recent lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Cleared {
    /// No file: nothing was signalled.
    Nothing,
    /// The system could not tell whether the pid of the file is our old
    /// server, at any look: nothing was signalled, and the file stays.
    Unknown { pid: u32 },
    /// The file named a process that is gone or another program's now: only
    /// the file was deleted.
    Forgotten,
    /// Our old server ended after `signal`, the last one sent (`None`: it
    /// ended before any).
    Stopped { pid: u32, signal: Option<Signal> },
}

/// Finds the process that `owner`'s pid record names, and stops it when it
/// is ours, by the signals of `ladder` and a final SIGKILL, then waits for
/// `owner`'s port, if it has one. A first look that cannot tell is made again, up to `looks`
/// looks in all ([`START_LOOKS`], [`STOP_LOOKS`]).
///
/// # Errors
///
/// [`Stuck`], with the reason, when our server still runs after SIGKILL, or
/// cannot be told from another program after a signal. Nothing else is
/// signalled then.
pub(crate) async fn clear(
    owner: &Owner<'_>,
    probes: &dyn ProcessProbe,
    servers: &[PathBuf],
    ladder: &StopMethod,
    looks: u32,
) -> Result<Cleared, Stuck> {
    if !owner.recovery {
        return Ok(Cleared::Nothing);
    }
    let Some(record) = process::read_pid_file(&owner.pid_file) else {
        return Ok(Cleared::Nothing);
    };
    let pid = record.pid;
    let mut state = check(probes, &record, servers).await;
    for _ in 1..looks {
        if !matches!(state, PidfileState::Unknown { .. }) {
            break;
        }
        debug!(
            pid,
            service = %owner.id,
            "cannot tell who runs the pid yet; looking again"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
        state = check(probes, &record, servers).await;
    }
    info!(pid, %state, service = %owner.id, "checked the pid record");
    match state {
        PidfileState::Missing | PidfileState::Stale { .. } | PidfileState::Mismatch { .. } => {
            forget(owner);
            return Ok(Cleared::Forgotten);
        }
        // Often another user's process at a reused pid, after a reboot.
        PidfileState::Unknown { .. } => {
            warn!(
                pid,
                service = %owner.id,
                "cannot tell whether the pid record names our old process; no signal"
            );
            return Ok(Cleared::Unknown { pid });
        }
        PidfileState::Ours { .. } => {}
    }
    // Signals only. A request to the server's port (Valkey's `SHUTDOWN
    // SAVE`), or a stop program that finds the server by its port or its
    // data folder, would reach whatever is there, which the check of the pid
    // does not prove is this process (ADR 0023, section 8).
    let signals = signal_steps(ladder)
        .into_iter()
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
                    owner.name,
                    signal.name()
                );
                return Err(Stuck(reason));
            }
            // Gone, or someone else's now: it ended between two checks.
            _ => return Ok(done(owner, pid, sent).await),
        }
        warn!(
            pid,
            signal = signal.name(),
            service = %owner.id,
            "stopping our old process"
        );
        if let Err(err) = probes.signal(pid, signal).await {
            warn!(pid, signal = signal.name(), %err, "the signal failed");
        }
        sent = Some(signal);
        if gone(probes, &record, servers, wait).await {
            return Ok(done(owner, pid, sent).await);
        }
    }
    let reason = format!(
        "our old {} (pid {pid}) still runs after SIGKILL",
        owner.name
    );
    Err(Stuck(reason))
}

/// The file against what the system says now: the file the process really
/// runs, and its start time.
pub(crate) async fn check(
    probes: &dyn ProcessProbe,
    record: &PidRecord,
    servers: &[PathBuf],
) -> PidfileState {
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

/// After the orphan ended: the file goes, then a short wait for the port, if
/// there is one, so the check before a start does not find it busy with a
/// listener that is closing.
async fn done(owner: &Owner<'_>, pid: u32, signal: Option<Signal>) -> Cleared {
    info!(
        pid,
        signal = signal.map(Signal::name),
        service = %owner.id,
        "our old process stopped"
    );
    forget(owner);
    if let Some(port) = owner.port {
        let started = Instant::now();
        while TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err()
            && started.elapsed() < PORT_WAIT
        {
            tokio::time::sleep(POLL).await;
        }
    }
    Cleared::Stopped { pid, signal }
}

/// Deletes the pid record of `owner`.
fn forget(owner: &Owner<'_>) {
    if let Err(err) = process::remove_pid_file(&owner.pid_file) {
        warn!(%err, service = %owner.id, "cannot delete the pid record");
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use marsa_core::services::{StopCommand, StopStep, TcpRequest};

    use super::*;

    /// An orphan is stopped by signals only: neither a request to the port
    /// nor a stop program, which would reach whatever holds the port or the
    /// data folder, goes in its ladder.
    #[test]
    fn an_orphans_ladder_has_signals_only() {
        let wait = Duration::from_secs(3);
        let ladder = StopMethod {
            steps: vec![
                StopStep {
                    action: StopAction::Send(TcpRequest {
                        port: 6379,
                        bytes: b"x".to_vec(),
                        name: "request",
                        limit: Duration::from_secs(1),
                    }),
                    wait,
                },
                StopStep {
                    action: StopAction::Run(StopCommand {
                        program: PathBuf::from("/bin/stop"),
                        args: Vec::new(),
                        name: "stop program",
                        on_failure: "",
                    }),
                    wait,
                },
                StopStep {
                    action: StopAction::Signal(Signal::Term),
                    wait,
                },
            ],
        };
        assert_eq!(signal_steps(&ladder), [(Signal::Term, wait)]);
        assert!(signal_steps(&StopMethod { steps: Vec::new() }).is_empty());
    }
}
