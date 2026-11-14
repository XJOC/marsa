//! `AppState` and `Daemon` (architecture.md section 5). Task M3-06.
//!
//! The locks here are `std::sync` locks: they are taken for short reads and
//! writes and never held across `.await` (`clippy::await_holding_lock`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockWriteGuard};
use std::time::Duration;

use axum::http::StatusCode;
use marsa_core::config::{self, ConfigError, ConfigPaths, SitesConfig};
use marsa_core::types::{Event, SiteList};
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;

use super::api::HttpError;
use super::autostart_api::Autostart;
use super::caddy::CaddyManager;
use super::php::Pools;
use super::runtimes_api::Installs;
use super::sites_api;
use super::sites_sync::PassRequests;
use super::supervisor::ServiceHandle;

/// How many events the bus keeps for a slow reader. A reader that falls
/// further behind loses the oldest ones (`Lagged`).
pub const EVENT_CAPACITY: usize = 1024;

/// What the daemon knows. A service's status is not copied here: it lives in
/// one place, the runtime its manager shares with the supervisor
/// (`Daemon::caddy`).
#[derive(Debug, Default)]
pub struct AppState {
    /// The site list of the last build; `None` before the first one. Use
    /// [`Daemon::sites`], [`Daemon::change_sites`], and
    /// [`Daemon::rebuild_sites`].
    pub sites: Option<SiteList>,
}

/// When a sites job sends `sites_changed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Publish {
    /// After every change request, so its client sees the result.
    Always,
    /// Only if the new list differs from the stored one. Never for the first
    /// build: that is the start, not a change.
    IfChanged,
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
    /// The task of each managed process, by id. Use [`Daemon::add_proc`] and
    /// [`Daemon::take_proc`]. A task that has ended may stay until the next
    /// start replaces it.
    pub procs: Mutex<BTreeMap<String, ServiceHandle>>,
    /// Cancelled once, when the daemon starts to stop.
    pub shutdown: CancellationToken,
    /// Caddy: how it runs, its status, and its recent lines.
    pub caddy: CaddyManager,
    /// The status and recent lines of each php-fpm pool (task M4-23). The
    /// sites sync starts and stops the pools.
    pub pools: Pools,
    /// Asks the sites sync task for a pass, and waits for it (task M5-11).
    /// The task alone runs passes and owns the pools.
    pub pass_requests: PassRequests,
    /// The runtime installer, and whether an install runs (task M3-12).
    pub installs: Installs,
    /// Where the login file that starts the tray icon goes (task M5-03).
    /// [`Autostart::unset`] here, so tests never reach the user's real
    /// folders; `server::run` sets [`Autostart::this_user`].
    pub autostart: Autostart,
    /// Held for the whole of each sites job, from reading `sites.toml` to
    /// publishing the list, so no change is lost and the events keep order.
    sites_lock: Mutex<()>,
    /// Held while `global.toml` is read, changed, and written. Use
    /// [`Daemon::lock_global`].
    global_lock: Mutex<()>,
    /// The parked folders of the last build. Use [`Daemon::watch_parked`].
    parked: watch::Sender<Vec<PathBuf>>,
}

impl Daemon {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            caddy: CaddyManager::new(&data_dir),
            pools: Pools::default(),
            pass_requests: PassRequests::default(),
            data_dir,
            state: Arc::default(),
            events: broadcast::channel(EVENT_CAPACITY).0,
            procs: Mutex::default(),
            shutdown: CancellationToken::new(),
            installs: Installs::new(),
            autostart: Autostart::unset(),
            sites_lock: Mutex::default(),
            global_lock: Mutex::default(),
            parked: watch::Sender::new(Vec::new()),
        }
    }

    /// The site list of the last build, or `None` before the first one.
    pub fn sites(&self) -> Option<SiteList> {
        // A write is one assignment, so a panic elsewhere while the lock was
        // held cannot leave the state half changed.
        let state = self.state.read().unwrap_or_else(PoisonError::into_inner);
        state.sites.clone()
    }

    /// Changes `sites.toml` and builds the site list again (task M4-10b).
    ///
    /// On a blocking thread, one job at a time: reads the file, lets `edit`
    /// change it, writes it back atomically if `edit` returns `true`, builds
    /// the list, stores it, and publishes `sites_changed`. `edit` sees the
    /// file as it is on disk now, so it may read the disk too.
    ///
    /// # Errors
    ///
    /// The error of `edit`, or a `config_*` code (500) if `sites.toml` cannot
    /// be read or written.
    pub async fn change_sites<F>(self: &Arc<Self>, edit: F) -> Result<SiteList, HttpError>
    where
        F: FnOnce(&mut SitesConfig) -> Result<bool, HttpError> + Send + 'static,
    {
        self.sites_job(edit, Publish::Always).await
    }

    /// Builds the site list again from `sites.toml`, the parked folders, and
    /// the installed PHP versions. Publishes `sites_changed` only if the list
    /// changed; the first build publishes nothing.
    ///
    /// `server::run` calls this once at start-up; `GET /v1/sites` also builds
    /// the list if it finds none (for tests that skip the server).
    ///
    /// # Errors
    ///
    /// A `config_*` code (500) if `sites.toml` cannot be read.
    pub async fn rebuild_sites(self: &Arc<Self>) -> Result<SiteList, HttpError> {
        self.sites_job(|_| Ok(false), Publish::IfChanged).await
    }

    async fn sites_job<F>(
        self: &Arc<Self>,
        edit: F,
        publish: Publish,
    ) -> Result<SiteList, HttpError>
    where
        F: FnOnce(&mut SitesConfig) -> Result<bool, HttpError> + Send + 'static,
    {
        let daemon = Arc::clone(self);
        let job = tokio::task::spawn_blocking(move || {
            // The lock guards no data, so a poisoned one is still good.
            let _one = daemon
                .sites_lock
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let path = ConfigPaths::in_dir(&daemon.data_dir).sites;
            let mut sites = config::load_sites(&path).map_err(config_error)?;
            if edit(&mut sites)? {
                config::save_sites(&path, &sites).map_err(config_error)?;
            }
            let list = sites_api::build(&daemon.data_dir, &sites);
            // Wakes the folder watcher only when the parked list changes.
            daemon.parked.send_if_modified(|parked| {
                let changed = *parked != list.parked;
                parked.clone_from(&list.parked);
                changed
            });
            let before = daemon.write_state().sites.replace(list.clone());
            let changed = before.is_some_and(|before| before != list);
            if publish == Publish::Always || changed {
                daemon.publish(Event::SitesChanged(list.clone()));
            }
            Ok(list)
        });
        job.await.unwrap_or_else(|err| {
            tracing::error!(error = %err, "the sites job failed");
            let message = "the sites job failed";
            Err(HttpError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                message,
            ))
        })
    }

    /// The parked folders of the last build, for the folder watcher (task
    /// M4-10d). It reports a change only when the list changes.
    pub fn watch_parked(&self) -> watch::Receiver<Vec<PathBuf>> {
        self.parked.subscribe()
    }

    /// One change of `global.toml` at a time: the first install's default
    /// and `PUT /v1/runtimes/{tool}/default` both read the whole file and
    /// write it back, so without this one could undo the other. Take it on a
    /// blocking thread only, and drop it before any `.await`.
    pub fn lock_global(&self) -> MutexGuard<'_, ()> {
        // The lock guards no data, so a poisoned one is still good.
        self.global_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn write_state(&self) -> RwLockWriteGuard<'_, AppState> {
        // As in `sites`: each write is one assignment.
        self.state.write().unwrap_or_else(PoisonError::into_inner)
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

/// `sites.toml` cannot be read or written: the request was fine, the file or
/// the disk is not.
fn config_error(err: ConfigError) -> HttpError {
    tracing::warn!(code = err.code(), error = %err, "sites.toml");
    HttpError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        err.code(),
        err.to_string(),
    )
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
