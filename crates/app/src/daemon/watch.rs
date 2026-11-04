//! The parked folder watcher (architecture.md section 5, "File watcher").
//! Task M4-10d.
//!
//! A new folder in a parked folder becomes a site without another command,
//! as in Valet: `composer create-project laravel/laravel shop` in a parked
//! `~/Code` is enough for `https://shop.test`.
//!
//! Each parked folder is watched without its sub-folders
//! (`RecursiveMode::NonRecursive`): only a direct child that appears, goes,
//! or gets a new name counts. The thousands of files that Composer or git
//! write inside a new folder are not watched. The events of one burst are
//! joined into one [`Daemon::rebuild_sites`].
//!
//! notify calls back on its own thread. The callback only puts a signal in a
//! channel; one tokio task owns the watcher, waits for the burst to end, and
//! rebuilds. The watched folders follow [`Daemon::watch_parked`]. Calls into
//! the watcher read the disk (and on Linux wait for notify's thread), so they
//! run on a blocking thread.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::state::Daemon;

/// How long the parked folders must stay quiet before the rebuild.
pub const SETTLE: Duration = Duration::from_millis(300);
/// The longest a rebuild waits while events keep coming.
pub const MAX_WAIT: Duration = Duration::from_secs(2);

/// Starts the watcher for `daemon`'s parked folders. It stops when
/// `daemon.shutdown` is cancelled, or at [`ParkedWatcher::stop`].
///
/// `None` if the system gives no watcher (for example, the Linux limit of
/// inotify instances is reached): a warning is logged, and new folders show
/// at the next change through the API.
pub fn start(daemon: &Arc<Daemon>) -> Option<ParkedWatcher> {
    // Capacity 1: a waiting signal already says "something changed". More
    // signals add nothing, and notify's thread never waits.
    let (signal, signals) = mpsc::channel(1);
    let handler = move |event: notify::Result<notify::Event>| {
        if matters(&event) {
            // Full: a signal waits already. Closed: the task has ended.
            let _ = signal.try_send(());
        }
    };
    let watcher = match notify::recommended_watcher(handler) {
        Ok(watcher) => watcher,
        Err(err) => {
            tracing::warn!(error = %err, "cannot watch the parked folders");
            return None;
        }
    };
    // A child, so `stop` does not cancel the daemon's own token.
    let cancel = daemon.shutdown.child_token();
    let rebuilds = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn(run(Task {
        daemon: Arc::clone(daemon),
        watches: Arc::new(Mutex::new(Watches {
            watcher,
            watched: BTreeSet::new(),
        })),
        signals,
        cancel: cancel.clone(),
        rebuilds: Arc::clone(&rebuilds),
    }));
    Some(ParkedWatcher {
        cancel,
        task,
        rebuilds,
    })
}

/// A running watcher, from [`start`].
#[derive(Debug)]
pub struct ParkedWatcher {
    cancel: CancellationToken,
    task: JoinHandle<()>,
    rebuilds: Arc<AtomicUsize>,
}

impl ParkedWatcher {
    /// How many rebuilds the watcher has run.
    pub fn rebuilds(&self) -> usize {
        self.rebuilds.load(Ordering::Relaxed)
    }

    /// Stops the watcher and waits for its task, `limit` at most. After that
    /// the task is aborted.
    pub async fn stop(self, limit: Duration) {
        self.cancel.cancel();
        let abort = self.task.abort_handle();
        match tokio::time::timeout(limit, self.task).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => tracing::warn!(error = %err, "the parked folder watcher failed"),
            Err(_) => {
                tracing::warn!(
                    ?limit,
                    "the parked folder watcher is still running; aborting it"
                );
                abort.abort();
            }
        }
    }
}

/// Whether `event` can change the site list: a direct child that appears,
/// goes, or gets a new name. Writes, reads, and metadata changes do not
/// count. An event that says some were lost, and an error, count: the list
/// is built again to be sure.
fn matters(event: &notify::Result<notify::Event>) -> bool {
    match event {
        Ok(event) => {
            event.need_rescan()
                || matches!(
                    event.kind,
                    EventKind::Any
                        | EventKind::Create(_)
                        | EventKind::Remove(_)
                        | EventKind::Modify(ModifyKind::Name(_))
                )
        }
        Err(err) => {
            tracing::warn!(error = %err, "the parked folder watcher reported an error");
            true
        }
    }
}

/// The watcher and the folders it watches.
struct Watches {
    watcher: RecommendedWatcher,
    watched: BTreeSet<PathBuf>,
}

impl Watches {
    /// Watches each folder of `parked` that is not watched yet, and stops
    /// watching each folder that is no longer parked. A folder that cannot be
    /// watched is logged, and tried again at the next change of the list.
    /// Returns whether a folder was added. Blocks.
    fn sync(&mut self, parked: &BTreeSet<PathBuf>) -> bool {
        let gone: Vec<_> = self.watched.difference(parked).cloned().collect();
        for dir in gone {
            if let Err(err) = self.watcher.unwatch(&dir) {
                // It may be gone from the disk, and its watch with it.
                tracing::debug!(path = %dir.display(), error = %err, "cannot unwatch a folder");
            }
            self.watched.remove(&dir);
        }
        let mut added = false;
        for dir in parked {
            if self.watched.contains(dir) {
                continue;
            }
            match self.watcher.watch(dir, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    tracing::debug!(path = %dir.display(), "watching a parked folder");
                    self.watched.insert(dir.clone());
                    added = true;
                }
                Err(err) => tracing::warn!(
                    path = %dir.display(),
                    error = %err,
                    "cannot watch a parked folder; its new folders show at the next change"
                ),
            }
        }
        added
    }
}

/// What the watcher task owns.
struct Task {
    daemon: Arc<Daemon>,
    /// Locked only inside a blocking job, never across `.await`.
    watches: Arc<Mutex<Watches>>,
    signals: mpsc::Receiver<()>,
    cancel: CancellationToken,
    rebuilds: Arc<AtomicUsize>,
}

/// The events since the last rebuild.
#[derive(Debug, Clone, Copy)]
struct Burst {
    first: Instant,
    last: Instant,
}

impl Burst {
    fn start(now: Instant) -> Self {
        Self {
            first: now,
            last: now,
        }
    }

    /// When to rebuild: once the folders are quiet for [`SETTLE`], and
    /// [`MAX_WAIT`] after the first event at the latest.
    fn due(self) -> Instant {
        (self.last + SETTLE).min(self.first + MAX_WAIT)
    }
}

/// The watcher task: follows the parked list, joins each burst of events,
/// then rebuilds the site list once.
async fn run(mut task: Task) {
    let mut parked = task.daemon.watch_parked();
    // The list as it is now; `changed` then reports each new one.
    let mut wanted = Some(parked.borrow_and_update().iter().cloned().collect());
    let mut burst: Option<Burst> = None;
    loop {
        if let Some(dirs) = wanted.take() {
            let Some(added) = task.sync(dirs).await else {
                break;
            };
            // A folder made between the last build and its new watch is seen
            // by this rebuild.
            if added {
                burst = Some(one_more(burst));
            }
        }
        let due = burst.map_or_else(Instant::now, Burst::due);
        tokio::select! {
            () = task.cancel.cancelled() => break,
            changed = parked.changed() => {
                // An error means the daemon is gone.
                if changed.is_err() {
                    break;
                }
                wanted = Some(parked.borrow_and_update().iter().cloned().collect());
            }
            Some(()) = task.signals.recv() => burst = Some(one_more(burst)),
            () = tokio::time::sleep_until(due), if burst.is_some() => {
                burst = None;
                if !task.rebuild().await {
                    break;
                }
            }
        }
    }
    // The watcher stops its own threads when dropped; on macOS it waits for
    // one of them.
    let watches = task.watches;
    if let Err(err) = tokio::task::spawn_blocking(move || drop(watches)).await {
        tracing::warn!(error = %err, "the parked folder watcher did not stop cleanly");
    }
    tracing::debug!("parked folder watcher stopped");
}

/// `burst` with one more event, now.
fn one_more(burst: Option<Burst>) -> Burst {
    let now = Instant::now();
    match burst {
        Some(burst) => Burst { last: now, ..burst },
        None => Burst::start(now),
    }
}

impl Task {
    /// Makes the watched folders `dirs`, on a blocking thread. Returns
    /// whether a folder was added, or `None` once the token is cancelled.
    async fn sync(&self, dirs: BTreeSet<PathBuf>) -> Option<bool> {
        let watches = Arc::clone(&self.watches);
        let job = tokio::task::spawn_blocking(move || {
            // Each change to `Watches` is one call and one set update, so a
            // panic in an earlier job cannot leave it half changed.
            let mut watches = watches.lock().unwrap_or_else(PoisonError::into_inner);
            if watches.watched == dirs {
                return false;
            }
            watches.sync(&dirs)
        });
        tokio::select! {
            () = self.cancel.cancelled() => None,
            result = job => Some(result.unwrap_or_else(|err| {
                tracing::error!(error = %err, "watching the parked folders failed");
                false
            })),
        }
    }

    /// Builds the site list again. Returns `false` once the token is
    /// cancelled.
    async fn rebuild(&self) -> bool {
        tokio::select! {
            () = self.cancel.cancelled() => false,
            result = self.daemon.rebuild_sites() => {
                let count = self.rebuilds.fetch_add(1, Ordering::Relaxed) + 1;
                // `rebuild_sites` logs its own errors.
                match result {
                    Ok(list) => tracing::debug!(count, sites = list.sites.len(), "site list rebuilt"),
                    Err(err) => tracing::debug!(count, code = err.code, "no site list"),
                }
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use axum::body::Body;
    use axum::extract::Request;
    use axum::http::StatusCode;
    use marsa_core::config::{self, ConfigPaths, SitesConfig};
    use marsa_core::platform::daemon_file::{Token, daemon_authority};
    use marsa_core::types::{Event, SiteList};
    use notify::event::{AccessKind, CreateKind, DataChange, RemoveKind, RenameMode};
    use serde_json::json;
    use tempfile::TempDir;
    use tokio::sync::broadcast;
    use tower::ServiceExt as _;

    use crate::daemon::api::router;
    use crate::daemon::auth::AuthConfig;

    /// Generous: slow CI machines, and macOS FSEvents sends events in
    /// batches. Each wait ends as soon as its condition holds.
    const WAIT: Duration = Duration::from_secs(20);
    const POLL: Duration = Duration::from_millis(20);
    /// The daemon port in the `Host` header. No socket is bound for it.
    const PORT: u16 = 51237;

    /// A daemon on `<tmp>/data` and an empty folder `<tmp>/code` to park.
    fn daemon() -> (TempDir, Arc<Daemon>, PathBuf) {
        let dir = TempDir::new().unwrap();
        let code = dir.path().join("code");
        fs::create_dir(&code).unwrap();
        let daemon = Arc::new(Daemon::new(dir.path().join("data")));
        (dir, daemon, code)
    }

    /// Writes `parked` to `sites.toml` and does the first build, as
    /// `server::run` does at start-up.
    async fn park_on_disk(daemon: &Arc<Daemon>, parked: Vec<PathBuf>) {
        let sites = SitesConfig {
            parked,
            ..SitesConfig::default()
        };
        config::save_sites(&ConfigPaths::in_dir(&daemon.data_dir).sites, &sites).unwrap();
        daemon.rebuild_sites().await.unwrap();
    }

    /// Waits for the rebuild that follows a new watch. From then on the
    /// parked folder is watched, and a site can come from an event only.
    async fn first_rebuild(watcher: &ParkedWatcher) {
        let started = Instant::now();
        while watcher.rebuilds() == 0 {
            assert!(started.elapsed() < WAIT, "no first rebuild in {WAIT:?}");
            tokio::time::sleep(POLL).await;
        }
    }

    /// The next `sites_changed` event that lists `name`.
    async fn site_appears(events: &mut broadcast::Receiver<Event>, name: &str) -> SiteList {
        let wait = async {
            loop {
                match events.recv().await {
                    Ok(Event::SitesChanged(list))
                        if list.sites.iter().any(|site| site.name == name) =>
                    {
                        return list;
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(err) => panic!("the event bus closed: {err}"),
                }
            }
        };
        tokio::time::timeout(WAIT, wait)
            .await
            .unwrap_or_else(|_| panic!("no sites_changed with {name} in {WAIT:?}"))
    }

    #[tokio::test]
    async fn a_new_folder_in_a_folder_parked_through_the_api_becomes_a_site() {
        let (_dir, daemon, code) = daemon();
        let watcher = start(&daemon).expect("a watcher");
        let token = Token::generate().unwrap();
        let bearer = token.bearer_header();
        let router = router(daemon.clone(), AuthConfig::new(Arc::new(token), PORT));
        let request = Request::builder()
            .method("POST")
            .uri("/v1/sites/park")
            .header("host", daemon_authority(PORT))
            .header("authorization", bearer)
            .header("content-type", "application/json")
            .body(Body::from(json!({ "path": code }).to_string()))
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        first_rebuild(&watcher).await;
        let mut events = daemon.events.subscribe();

        // No other request: only an event of the watcher can show it.
        fs::create_dir(code.join("shop")).unwrap();
        let list = site_appears(&mut events, "shop").await;
        assert_eq!(list.parked, std::slice::from_ref(&code));
        let shop = list.sites.iter().find(|site| site.name == "shop").unwrap();
        assert_eq!(shop.root, code.join("shop"));
        assert_eq!(daemon.sites(), Some(list));

        watcher.stop(WAIT).await;
    }

    #[tokio::test]
    async fn fifty_files_in_a_new_folder_cost_one_or_two_rebuilds() {
        let (dir, daemon, code) = daemon();
        // A parked folder that cannot be watched does not stop the others.
        let missing = dir.path().join("missing");
        park_on_disk(&daemon, vec![missing, code.clone()]).await;
        let watcher = start(&daemon).expect("a watcher");
        first_rebuild(&watcher).await;
        let before = watcher.rebuilds();
        let mut events = daemon.events.subscribe();

        let app = code.join("app");
        fs::create_dir(&app).unwrap();
        for n in 0..50 {
            fs::write(app.join(format!("file-{n}.txt")), "x").unwrap();
        }
        site_appears(&mut events, "app").await;
        // Room for a late rebuild, so a third one would be counted.
        tokio::time::sleep(SETTLE + MAX_WAIT).await;
        let rebuilds = watcher.rebuilds() - before;
        assert!((1..=2).contains(&rebuilds), "{rebuilds} rebuilds");

        watcher.stop(WAIT).await;
    }

    #[tokio::test]
    async fn the_daemon_shutdown_token_ends_the_task() {
        let (_dir, daemon, code) = daemon();
        park_on_disk(&daemon, vec![code]).await;
        let watcher = start(&daemon).expect("a watcher");
        daemon.shutdown.cancel();
        tokio::time::timeout(WAIT, watcher.task)
            .await
            .expect("the task ends in time")
            .unwrap();
    }

    #[test]
    fn only_a_child_that_comes_goes_or_moves_matters() {
        let event = |kind| Ok(notify::Event::new(kind));
        for kind in [
            EventKind::Any,
            EventKind::Create(CreateKind::Folder),
            EventKind::Remove(RemoveKind::Any),
            EventKind::Modify(ModifyKind::Name(RenameMode::To)),
        ] {
            assert!(matters(&event(kind)), "{kind:?}");
        }
        for kind in [
            // Windows reports a folder whose content changed this way.
            EventKind::Modify(ModifyKind::Any),
            EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            EventKind::Access(AccessKind::Any),
            EventKind::Other,
        ] {
            assert!(!matters(&event(kind)), "{kind:?}");
        }
        let lost = notify::Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
        assert!(matters(&Ok(lost)));
        assert!(matters(&Err(notify::Error::generic("test"))));
    }

    #[test]
    fn a_burst_waits_for_quiet_but_not_forever() {
        let first = Instant::now();
        let burst = Burst::start(first);
        assert_eq!(burst.due(), first + SETTLE);
        let later = Burst {
            last: first + Duration::from_millis(100),
            ..burst
        };
        assert_eq!(later.due(), first + Duration::from_millis(100) + SETTLE);
        let endless = Burst {
            last: first + MAX_WAIT,
            ..burst
        };
        assert_eq!(endless.due(), first + MAX_WAIT);
    }
}
