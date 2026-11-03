//! `AppState` and `Daemon` (architecture.md section 5). Task M3-06.
//!
//! The locks here are `std::sync` locks: they are taken for short reads and
//! writes and never held across `.await` (`clippy::await_holding_lock`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::Duration;

use marsa_core::types::{Event, ServiceStatus};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// How many events the bus keeps for a slow reader. A reader that falls
/// further behind loses the oldest ones (`Lagged`).
pub const EVENT_CAPACITY: usize = 1024;

/// What the daemon knows. More fields come with later tasks.
#[derive(Debug, Default)]
pub struct AppState {
    pub services: BTreeMap<String, ServiceStatus>,
}

/// One managed process: the task that owns its `Child`, and the token that
/// tells the task to stop it.
#[derive(Debug)]
pub struct ProcHandle {
    pub cancel: CancellationToken,
    pub task: JoinHandle<()>,
}

/// Everything the routes share. The router holds it in an `Arc`, like a
/// singleton in Laravel's service container.
#[derive(Debug)]
pub struct Daemon {
    /// The data folder (`--data-dir`), absolute.
    pub data_dir: PathBuf,
    pub state: Arc<RwLock<AppState>>,
    /// Every event, for `GET /v1/events`. Use [`Daemon::publish`].
    pub events: broadcast::Sender<Event>,
    /// Managed processes by id. Use [`Daemon::add_proc`].
    pub procs: Mutex<BTreeMap<String, ProcHandle>>,
    /// Cancelled once, when the daemon starts to stop.
    pub shutdown: CancellationToken,
}

impl Daemon {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            state: Arc::default(),
            events: broadcast::channel(EVENT_CAPACITY).0,
            procs: Mutex::default(),
            shutdown: CancellationToken::new(),
        }
    }

    /// Sends `event` to every `/v1/events` reader.
    pub fn publish(&self, event: Event) {
        // The only error is "no reader right now", which is fine.
        let _ = self.events.send(event);
    }

    /// Registers a managed process under `id` and gives back the handle it
    /// replaces, if any. During shutdown the process is told to stop at once.
    pub fn add_proc(&self, id: impl Into<String>, handle: ProcHandle) -> Option<ProcHandle> {
        let mut procs = self.lock_procs();
        // Checked under the lock: `stop_procs` empties the map after the token
        // is cancelled, so no process slips in between.
        if self.shutdown.is_cancelled() {
            handle.cancel.cancel();
        }
        procs.insert(id.into(), handle)
    }

    /// Stops every managed process: cancels all tokens in id order, then waits
    /// for the tasks, at most `limit` in total. Tasks still running then are
    /// aborted, which drops their `Child`.
    pub async fn stop_procs(&self, limit: Duration) {
        let procs = std::mem::take(&mut *self.lock_procs());
        for handle in procs.values() {
            handle.cancel.cancel();
        }
        let aborts: Vec<_> = procs.values().map(|h| h.task.abort_handle()).collect();
        let wait = async {
            for (id, handle) in procs {
                if let Err(err) = handle.task.await {
                    tracing::warn!(service = %id, error = %err, "process task failed");
                }
            }
        };
        if tokio::time::timeout(limit, wait).await.is_err() {
            tracing::warn!(?limit, "processes still running; aborting their tasks");
            for abort in aborts {
                abort.abort();
            }
        }
    }

    fn lock_procs(&self) -> MutexGuard<'_, BTreeMap<String, ProcHandle>> {
        // Each change is one insert or one take, so a panic elsewhere while
        // the lock was held cannot leave the map half changed.
        self.procs.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    fn polite(stopped: &Arc<AtomicBool>) -> ProcHandle {
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let cancel = cancel.clone();
            let stopped = stopped.clone();
            async move {
                cancel.cancelled().await;
                stopped.store(true, Ordering::SeqCst);
            }
        });
        ProcHandle { cancel, task }
    }

    #[tokio::test]
    async fn stop_procs_cancels_waits_and_aborts_after_the_limit() {
        let daemon = Daemon::new(PathBuf::new());
        let stopped = Arc::new(AtomicBool::new(false));
        daemon.add_proc("a-polite", polite(&stopped));
        let stuck = tokio::spawn(std::future::pending::<()>());
        let stuck_abort = stuck.abort_handle();
        let stuck = ProcHandle {
            cancel: CancellationToken::new(),
            task: stuck,
        };
        daemon.add_proc("b-stuck", stuck);

        let started = Instant::now();
        daemon.stop_procs(Duration::from_millis(200)).await;
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(stopped.load(Ordering::SeqCst));
        assert!(daemon.procs.lock().unwrap().is_empty());
        // The abort takes effect at the task's next turn.
        tokio::time::timeout(Duration::from_secs(5), async {
            while !stuck_abort.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_proc_added_during_shutdown_is_told_to_stop() {
        let daemon = Daemon::new(PathBuf::new());
        daemon.shutdown.cancel();
        let stopped = Arc::new(AtomicBool::new(false));
        let handle = polite(&stopped);
        let cancel = handle.cancel.clone();
        assert!(daemon.add_proc("late", handle).is_none());
        assert!(cancel.is_cancelled());
    }
}
