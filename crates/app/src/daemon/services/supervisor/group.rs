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
//!   a group with that id and no such process is still ours. Between that
//!   look and the signal, our group could end and its id go round to a new
//!   group whose leader already exited: that needs every pid to be handed
//!   out again within a few milliseconds.
//!
//! Windows has no groups to signal: there the Job Object ends what is left
//! with the daemon (ADR 0017).

use std::io;
use std::time::Duration;

use marsa_core::orphan::Liveness;
use marsa_core::platform::process::{self, Signal};
use marsa_core::services::ServiceSpec;
use marsa_core::types::ServiceId;
use tokio::time::Instant;
use tracing::warn;

/// What a stop with `own_group` may take on top of `super::stop_limit`:
/// the wait for the rest of the group, its SIGKILL, [`KILL_WAIT`], and the
/// `/bin/kill` and `ps` runs in between (up to 2 s each when one hangs).
const GROUP_MARGIN: Duration = Duration::from_secs(10);

/// How long the stop of `spec` may take: `super::stop_limit` of its stop
/// method, plus [`GROUP_MARGIN`] when it leads a group.
pub(super) fn stop_limit(spec: &ServiceSpec) -> Duration {
    let limit = super::stop_limit(&spec.stop);
    if spec.own_group {
        limit.saturating_add(GROUP_MARGIN)
    } else {
        limit
    }
}

/// Sends `signal` to the child `pid`, or to the group it leads. The child is
/// not yet waited for. A child that left its group (`setsid`) gets it
/// alone, so it still hears the stop.
pub(super) async fn signal_child(pid: u32, group: Option<Group>, signal: Signal) -> io::Result<()> {
    let Some(group) = group else {
        return process::send_signal(pid, signal).await;
    };
    match group.signal_with_leader(signal).await {
        Err(err) if err.kind() != io::ErrorKind::Unsupported => {
            warn!(service = %group.id, %err, "{} to the group failed; to the child alone", signal.name());
            process::send_signal(pid, signal).await
        }
        sent => sent,
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

    /// After the SIGKILL of the whole group and the wait for the leader: the
    /// others may still be on their way out (a zombie until its new parent
    /// collects it), so they get [`KILL_WAIT`] before a second look.
    pub(super) async fn end_after_kill(self) {
        self.end_after_leader(Instant::now() + KILL_WAIT).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::types::{SiteKey, WorkerKind};
    use marsa_core::workers::{WorkerPaths, worker_spec};
    use std::path::Path;

    #[test]
    fn only_a_spec_that_leads_a_group_gets_the_group_margin() {
        let paths = WorkerPaths {
            php: Path::new("/data/runtimes/php/8.4.1/bin/php"),
            php_ini: Path::new("/data/config/php/8.4/php.ini"),
            root: Path::new("/sites/shop"),
        };
        let site = SiteKey::new("shop").expect("a valid site name");
        let worker = worker_spec(WorkerKind::Schedule, site, paths);
        let alone = ServiceSpec {
            own_group: false,
            ..worker.clone()
        };
        let base = super::super::stop_limit(&worker.stop);
        assert_eq!(stop_limit(&alone), base);
        assert_eq!(stop_limit(&worker), base + GROUP_MARGIN);
    }
}
