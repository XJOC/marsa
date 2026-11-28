//! The process group of a service with `own_group` (a site's worker): its
//! child leads it, so the group's id is the child's pid, and a stop reaches
//! every process in it, such as the `schedule:run` that `schedule:work`
//! started and the tasks that one runs.
//!
//! Before each signal the group must be ours:
//!
//! - while the leader is our child and not yet waited for, its pid, and so
//!   the group's id, cannot be another process's;
//! - once it is waited for, only while no process has that pid: the system
//!   gives a pid to a new process only when no group has it as its id, so
//!   a group with that id and no such process is still ours.
//!
//! Windows has no groups to signal: there the Job Object ends what is left
//! with the daemon (ADR 0017).

use std::io;
use std::time::Duration;

use marsa_core::orphan::Liveness;
use marsa_core::platform::process::{self, Signal};
use marsa_core::types::ServiceId;
use tokio::time::Instant;
use tracing::warn;

/// Sends `signal` to the child `pid`, or to the group it leads. The child is
/// not yet waited for.
pub(super) async fn signal_child(pid: u32, group: Option<Group>, signal: Signal) -> io::Result<()> {
    match group {
        Some(group) => group.signal_with_leader(signal).await,
        None => process::send_signal(pid, signal).await,
    }
}

/// How often a wait for the group looks again.
const POLL: Duration = Duration::from_millis(50);

/// How long the rest of the group may take to end after a SIGKILL.
const KILL_WAIT: Duration = Duration::from_secs(1);

/// The group that a child of the supervisor leads.
#[derive(Debug, Clone, Copy)]
pub(super) struct Group {
    pgid: u32,
    id: ServiceId,
}

impl Group {
    /// The group of the child with `pid`, for service `id`. Call it while
    /// the child is not yet waited for.
    pub(super) fn led_by(pid: u32, id: ServiceId) -> Self {
        Self { pgid: pid, id }
    }

    /// Sends `signal` to the whole group. Only while the leader is our
    /// child and not yet waited for (see the module).
    pub(super) async fn signal_with_leader(self, signal: Signal) -> io::Result<()> {
        process::signal_group(self.pgid, signal).await
    }

    /// SIGKILL to the whole group, before the leader is killed and waited
    /// for. Windows has no group to signal: the kill of the leader follows.
    pub(super) async fn kill_with_leader(self) {
        match self.signal_with_leader(Signal::Kill).await {
            Err(err) if err.kind() != io::ErrorKind::Unsupported => {
                warn!(service = %self.id, %err, "SIGKILL to the group failed");
            }
            _ => {}
        }
    }

    /// After the leader was waited for: waits for the rest of the group to
    /// end, until `deadline`, then sends it SIGKILL if it is still ours, and
    /// waits [`KILL_WAIT`] more.
    pub(super) async fn end_after_leader(self, deadline: Instant) {
        if self.ended_by(deadline).await {
            return;
        }
        match process::identity(self.pgid).await {
            Liveness::Gone => {}
            other => {
                // A process has the leader's pid again, or the system cannot
                // say: the group may not be ours, so it gets no signal.
                warn!(service = %self.id, pgid = self.pgid, ?other, "the group may not be ours now; no SIGKILL");
                return;
            }
        }
        warn!(service = %self.id, pgid = self.pgid, "the rest of the group still runs; SIGKILL");
        if let Err(err) = process::signal_group(self.pgid, Signal::Kill).await {
            // Most often the group ended in the meantime.
            warn!(service = %self.id, %err, "SIGKILL to the group failed");
        }
        if !self.ended_by(Instant::now() + KILL_WAIT).await {
            warn!(service = %self.id, pgid = self.pgid, "the group still runs after SIGKILL");
        }
    }

    /// Whether the group has no process left by `deadline`.
    async fn ended_by(self, deadline: Instant) -> bool {
        loop {
            if !process::group_exists(self.pgid).await {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
    }
}
