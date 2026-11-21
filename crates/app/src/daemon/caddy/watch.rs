//! Watches one run of Caddy: the pid file, and a port that is taken.

use super::CADDY_ID;
use super::orphan::PidFile;
use crate::daemon::services::probe::ProcessProbe;
use crate::daemon::services::runtime::AbortOnDrop;
use crate::daemon::supervisor::{ServiceRuntime, lock};
use marsa_core::codes;
use marsa_core::types::{Event, LogLine, LogStream, ServiceState};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// What Caddy prints when an address is taken: Go's text on macOS and Linux,
/// and the two English Windows texts. Windows says "forbidden by its access
/// permissions" when the holder took the port for itself alone, as IIS does
/// with port 80 (spike S-11). Compared in lower case.
const PORT_IN_USE: [&str; 3] = [
    "address already in use",
    "only one usage of each socket address",
    "forbidden by its access permissions",
];

/// Watches one run of Caddy. It owns the supervisor's task, and its own task
/// is the one in `Daemon::procs`.
pub(super) struct Watcher {
    /// The event bus, subscribed before the process started.
    pub(super) lines: broadcast::Receiver<Event>,
    /// The `seq` of this run's first line.
    pub(super) first_seq: u64,
    pub(super) cancel: CancellationToken,
    pub(super) runtime: Arc<Mutex<ServiceRuntime>>,
    pub(super) events: broadcast::Sender<Event>,
    /// `run/caddy.pid`: written after each spawn, removed after the exit.
    pub(super) pid_file: PidFile,
    pub(super) processes: Arc<dyn ProcessProbe>,
}

impl Watcher {
    /// Waits for the supervisor's task. If Caddy first says that a port is
    /// taken, stops it and sets `Failed { port_in_use }`: a restart would
    /// fail the same way (ADR 0008).
    pub(super) async fn run(self, mut supervisor: JoinHandle<()>) {
        // `stop_procs` aborts this task after its time limit; the abort must
        // reach the supervisor too, so the `Child` is dropped and killed.
        let _abort = AbortOnDrop(supervisor.abort_handle());
        let Watcher {
            lines,
            first_seq,
            cancel,
            runtime,
            events,
            pid_file,
            processes,
        } = self;
        let conflict = tokio::select! {
            result = &mut supervisor => {
                report(result);
                false
            }
            () = watch(lines, first_seq, &runtime, &pid_file, &*processes) => true,
        };
        if conflict {
            warn!("a port caddy needs is in use; stopping it without a restart");
            cancel.cancel();
            report(supervisor.await);
        }
        // Caddy has exited, so its pid may be anyone's from now on.
        pid_file.remove();
        if conflict {
            // After the supervisor's last change, so this one stays.
            lock(&runtime).set_failed(&events, codes::PORT_IN_USE);
        }
    }
}

/// Writes `run/caddy.pid` after each spawn of this run. Ends when a stderr
/// line of this run says that a port is taken; never ends otherwise.
async fn watch(
    mut lines: broadcast::Receiver<Event>,
    first_seq: u64,
    runtime: &Mutex<ServiceRuntime>,
    pid_file: &PidFile,
    processes: &dyn ProcessProbe,
) {
    let mut recorded = None;
    loop {
        let status = match lines.recv().await {
            Ok(Event::Log(line)) if line.service == CADDY_ID && is_port_conflict(&line) => return,
            Ok(Event::ServiceChanged(status)) if status.id == CADDY_ID => status,
            Ok(_) => continue,
            // Too far behind: the skipped lines are still in the buffer, and
            // the status is current.
            Err(RecvError::Lagged(_)) => {
                let (recent, status) = {
                    let runtime = lock(runtime);
                    (runtime.logs.recent(), runtime.status.clone())
                };
                let mut this_run = recent.iter().filter(|line| line.seq >= first_seq);
                if this_run.any(is_port_conflict) {
                    return;
                }
                status
            }
            Err(RecvError::Closed) => std::future::pending().await,
        };
        // The supervisor says `Running` right after each spawn.
        if status.state == ServiceState::Running
            && let Some(pid) = status.pid
            && recorded != Some(pid)
        {
            let running = || {
                let status = &lock(runtime).status;
                status.state == ServiceState::Running && status.pid == Some(pid)
            };
            pid_file.record(processes, pid, &running).await;
            recorded = Some(pid);
        }
    }
}

pub(super) fn is_port_conflict(line: &LogLine) -> bool {
    if line.stream != LogStream::Stderr {
        return false;
    }
    let text = line.text.to_ascii_lowercase();
    PORT_IN_USE.iter().any(|needle| text.contains(needle))
}

fn report(result: Result<(), JoinError>) {
    if let Err(err) = result {
        warn!(%err, "the caddy supervisor task failed");
    }
}
