//! The browser's open connections to Marsa's servers (ADR 0028, section 6).
//!
//! No pool: one connection for each source, used by one request at a time.
//! At most [`MAX_OPEN`] sources stay open, and the least recently used
//! closes first. A connection closes when no request used it for the idle
//! time, on `POST /v1/db/close`, before its server stops, and at shutdown.
//!
//! Closing a source drops its slot. An idle connection is dropped at once,
//! which closes its socket, so a server's stop never waits for it. A request
//! that runs keeps its own handle to the slot, and its connection is dropped
//! when it ends.
//!
//! Generic over the connection, so the tests count closes with a fake.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use marsa_core::types::DbServer;
use tokio_util::sync::CancellationToken;

/// The sources that stay open at once.
pub(super) const MAX_OPEN: usize = 4;

/// A server source: the server, and the database (`None` for the server's
/// own catalog connection).
pub(super) type Key = (DbServer, Option<String>);

/// One source's connection: `None` until the first request opens it, while
/// a request uses it, and after a request failed on it. A `tokio` lock held across `.await`
/// as the one-request-at-a-time token of the source, not as a guard of
/// shared data (AGENTS.md section 6).
pub(super) type Slot<C> = Arc<tokio::sync::Mutex<Option<C>>>;

pub(super) struct Store<C> {
    inner: Mutex<Inner<C>>,
    /// How long a connection stays open with no request.
    idle: Duration,
}

struct Inner<C> {
    /// Most recently used last.
    slots: Vec<(Key, Slot<C>, Instant)>,
    /// A task that closes idle connections runs: one while any slot is open.
    reaping: bool,
}

impl<C: Send + 'static> Store<C> {
    pub(super) fn new(idle: Duration) -> Self {
        Self {
            inner: Mutex::new(Inner {
                slots: Vec::new(),
                reaping: false,
            }),
            idle,
        }
    }

    /// The slot of `key`, made if missing, and marked used now. The least
    /// recently used source closes when more than [`MAX_OPEN`] would be
    /// open. `true` with it when a task to close idle connections must
    /// start ([`Self::spawn_reaper`]).
    pub(super) fn slot(&self, key: &Key) -> (Slot<C>, bool) {
        let mut inner = self.lock();
        let now = Instant::now();
        let slot = match inner.slots.iter().position(|(k, _, _)| k == key) {
            Some(at) => {
                let (key, slot, _) = inner.slots.remove(at);
                inner.slots.push((key, Arc::clone(&slot), now));
                slot
            }
            None => {
                let slot = Arc::new(tokio::sync::Mutex::new(None));
                inner.slots.push((key.clone(), Arc::clone(&slot), now));
                if inner.slots.len() > MAX_OPEN {
                    inner.slots.remove(0);
                }
                slot
            }
        };
        let start_reaper = !inner.reaping;
        inner.reaping = true;
        (slot, start_reaper)
    }

    /// Closes every source of `server`.
    pub(super) fn close_server(&self, server: DbServer) {
        self.lock().slots.retain(|((s, _), _, _)| *s != server);
    }

    pub(super) fn close_all(&self) {
        self.lock().slots.clear();
    }

    /// Closes the sources no request used since `now - idle`. `false` when
    /// none is left: the caller's task then ends, and the next slot starts a
    /// new one.
    pub(super) fn close_idle(&self, now: Instant) -> bool {
        let mut inner = self.lock();
        let idle = self.idle;
        inner
            .slots
            .retain(|(_, _, used)| now.saturating_duration_since(*used) < idle);
        inner.reaping = !inner.slots.is_empty();
        inner.reaping
    }

    #[cfg(test)]
    pub(super) fn open(&self) -> Vec<Key> {
        self.lock()
            .slots
            .iter()
            .map(|(key, _, _)| key.clone())
            .collect()
    }

    /// The task that closes idle connections, until none is left or the
    /// daemon shuts down. It looks every twelfth of the idle time, so a
    /// connection closes at most that late: 65 s after its last use for an
    /// idle time of 60 s.
    pub(super) fn spawn_reaper(self: &Arc<Self>, shutdown: CancellationToken) {
        let store = Arc::clone(self);
        let every = store.idle / 12;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => {
                        store.close_all();
                        return;
                    }
                    () = tokio::time::sleep(every) => {}
                }
                if !store.close_idle(Instant::now()) {
                    return;
                }
            }
        });
    }

    /// A panic while the lock was held leaves the list whole: each change is
    /// one `Vec` call.
    fn lock(&self) -> MutexGuard<'_, Inner<C>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A connection that counts its closes.
    struct Fake(Arc<AtomicUsize>);

    impl Drop for Fake {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn key(server: DbServer, database: &str) -> Key {
        (server, Some(database.to_owned()))
    }

    /// Opens `key`'s connection in its slot, as a request would.
    async fn open(store: &Store<Fake>, key: &Key, closed: &Arc<AtomicUsize>) {
        let (slot, _) = store.slot(key);
        *slot.lock().await = Some(Fake(Arc::clone(closed)));
    }

    #[tokio::test]
    async fn the_least_recently_used_source_closes_past_four() {
        let closed = Arc::new(AtomicUsize::new(0));
        let store = Store::new(Duration::from_secs(60));
        let keys: Vec<Key> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|db| key(DbServer::Mysql, db))
            .collect();
        for key in &keys[..4] {
            open(&store, key, &closed).await;
        }
        // `a` is used again, so `b` is the oldest.
        store.slot(&keys[0]);
        open(&store, &keys[4], &closed).await;
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.open(),
            [&keys[2], &keys[3], &keys[0], &keys[4]].map(Clone::clone)
        );
    }

    #[tokio::test]
    async fn a_server_that_stops_closes_its_sources_at_once() {
        let closed = Arc::new(AtomicUsize::new(0));
        let store = Store::new(Duration::from_secs(60));
        open(&store, &key(DbServer::Mysql, "shop"), &closed).await;
        open(&store, &(DbServer::Mysql, None), &closed).await;
        open(&store, &key(DbServer::Postgres, "app"), &closed).await;
        store.close_server(DbServer::Mysql);
        assert_eq!(closed.load(Ordering::SeqCst), 2);
        assert_eq!(store.open(), [key(DbServer::Postgres, "app")]);
        store.close_all();
        assert_eq!(closed.load(Ordering::SeqCst), 3);
        assert!(store.open().is_empty());
    }

    /// A request that runs keeps its connection until it ends, even when its
    /// source is closed meanwhile.
    #[tokio::test]
    async fn a_running_request_keeps_its_connection_until_it_ends() {
        let closed = Arc::new(AtomicUsize::new(0));
        let store = Store::new(Duration::from_secs(60));
        let shop = key(DbServer::Mysql, "shop");
        open(&store, &shop, &closed).await;
        let (slot, _) = store.slot(&shop);
        let request = slot.lock().await;
        store.close_all();
        assert_eq!(closed.load(Ordering::SeqCst), 0);
        drop(request);
        drop(slot);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_idle_connection_closes_after_the_idle_time() {
        let closed = Arc::new(AtomicUsize::new(0));
        let store = Store::new(Duration::from_secs(60));
        let shop = key(DbServer::Postgres, "shop");
        open(&store, &shop, &closed).await;
        let start = Instant::now();
        assert!(store.close_idle(start + Duration::from_secs(59)));
        assert_eq!(closed.load(Ordering::SeqCst), 0);
        assert!(!store.close_idle(start + Duration::from_secs(61)));
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }

    /// The task starts with the first slot, closes the idle ones, and ends
    /// when none is left; the next slot asks for a new one.
    #[tokio::test]
    async fn the_reaper_runs_only_while_a_source_is_open() {
        let closed = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(Store::new(Duration::from_millis(80)));
        let shop = key(DbServer::Mysql, "shop");
        let (slot, start) = store.slot(&shop);
        assert!(start);
        *slot.lock().await = Some(Fake(Arc::clone(&closed)));
        drop(slot);
        store.spawn_reaper(CancellationToken::new());
        assert!(!store.slot(&shop).1, "a second reaper");
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert!(store.open().is_empty());
        assert!(store.slot(&shop).1, "no reaper after the last close");
    }

    #[tokio::test]
    async fn a_shutdown_closes_every_source() {
        let closed = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(Store::new(Duration::from_secs(60)));
        open(&store, &key(DbServer::Mysql, "shop"), &closed).await;
        let shutdown = CancellationToken::new();
        store.spawn_reaper(shutdown.clone());
        shutdown.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
}
