//! `AppState` and `Daemon` (architecture.md section 5). Task M3-06.
//!
//! The locks here are `std::sync` locks: they are taken for short reads and
//! writes and never held across `.await` (`clippy::await_holding_lock`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::Duration;

use marsa_core::types::Event;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::caddy::CaddyManager;
use super::runtimes_api::Installs;
use super::supervisor::ServiceHandle;

/// How many events the bus keeps for a slow reader. A reader that falls
/// further behind loses the oldest ones (`Lagged`).
pub const EVENT_CAPACITY: usize = 1024;

/// What the daemon knows. The sites come in M4. A service's status is not
/// copied here: it lives in one place, the runtime its manager shares with
/// the supervisor (`Daemon::caddy`).
#[derive(Debug, Default)]
pub struct AppState {}

/// Everything the routes share. The router holds it in an `Arc`, like a
/// singleton in Laravel's service container.
#[derive(Debug)]
pub struct Daemon {
    /// The data folder (`--data-dir`), absolute.
    pub data_dir: PathBuf,
    pub state: Arc<RwLock<AppState>>,
    /// Every event, for `GET /v1/events`. Use [`Daemon::publish`].
    pub events: broadcast::Sender<Event>,
    /// The task of each managed process, by id. Use [`Daemon::add_proc`] and
    /// [`Daemon::take_proc`]. A task that has ended may stay until the next
    /// start replaces it.
    pub procs: Mutex<BTreeMap<String, ServiceHandle>>,
    /// Cancelled once, when the daemon starts to stop.
    pub shutdown: CancellationToken,
    /// Caddy: how it runs, its status, and its recent lines.
    pub caddy: CaddyManager,
    /// The runtime installer, and whether an install runs (task M3-12).
    pub installs: Installs,
}

impl Daemon {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            caddy: CaddyManager::new(&data_dir),
            data_dir,
            state: Arc::default(),
            events: broadcast::channel(EVENT_CAPACITY).0,
            procs: Mutex::default(),
            shutdown: CancellationToken::new(),
            installs: Installs::new(),
        }
    }

    /// Sends `event` to every `/v1/events` reader.
    pub fn publish(&self, event: Event) {
        // The only error is "no reader right now", which is fine.
        let _ = self.events.send(event);
    }

    /// Registers a managed process under `id` and gives back the handle it
    /// replaces, if any. During shutdown the process is told to stop at once.
    pub fn add_proc(&self, id: impl Into<String>, handle: ServiceHandle) -> Option<ServiceHandle> {
        let mut procs = self.lock_procs();
        // Checked under the lock: `stop_procs` empties the map after the token
        // is cancelled, so no process slips in between.
        if self.shutdown.is_cancelled() {
            handle.cancel.cancel();
        }
        procs.insert(id.into(), handle)
    }

    /// Takes the handle of `id` out, so the caller can stop it.
    pub fn take_proc(&self, id: &str) -> Option<ServiceHandle> {
        self.lock_procs().remove(id)
    }

    /// Whether the task of `id` still runs.
    pub fn proc_alive(&self, id: &str) -> bool {
        let procs = self.lock_procs();
        procs
            .get(id)
            .is_some_and(|handle| !handle.task.is_finished())
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

    fn lock_procs(&self) -> MutexGuard<'_, BTreeMap<String, ServiceHandle>> {
        // Each change is one insert or one take, so a panic elsewhere while
        // the lock was held cannot leave the map half changed.
        self.procs.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::supervisor::ServiceRuntime;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;
    use tokio::task::JoinHandle;

    fn handle(cancel: CancellationToken, task: JoinHandle<()>) -> ServiceHandle {
        let runtime = Arc::new(Mutex::new(ServiceRuntime::new("test")));
        ServiceHandle {
            cancel,
            task,
            runtime,
        }
    }

    fn polite(stopped: &Arc<AtomicBool>) -> ServiceHandle {
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let cancel = cancel.clone();
            let stopped = stopped.clone();
            async move {
                cancel.cancelled().await;
                stopped.store(true, Ordering::SeqCst);
            }
        });
        handle(cancel, task)
    }

    #[tokio::test]
    async fn stop_procs_cancels_waits_and_aborts_after_the_limit() {
        let daemon = Daemon::new(PathBuf::new());
        let stopped = Arc::new(AtomicBool::new(false));
        daemon.add_proc("a-polite", polite(&stopped));
        let stuck = tokio::spawn(std::future::pending::<()>());
        let stuck_abort = stuck.abort_handle();
        daemon.add_proc("b-stuck", handle(CancellationToken::new(), stuck));

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

    #[tokio::test]
    async fn take_proc_and_proc_alive() {
        let daemon = Daemon::new(PathBuf::new());
        assert!(!daemon.proc_alive("a"));
        let stopped = Arc::new(AtomicBool::new(false));
        daemon.add_proc("a", polite(&stopped));
        assert!(daemon.proc_alive("a"));
        let taken = daemon.take_proc("a").unwrap();
        assert!(!daemon.proc_alive("a"));
        taken.stop().await.unwrap();
        assert!(stopped.load(Ordering::SeqCst));
    }
}
