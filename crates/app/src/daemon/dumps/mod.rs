//! The dump server (ADR 0027): `dump()` in a PHP site reaches the window.
//!
//! PHP's `symfony/var-dumper` sends each dump as one line to
//! `VAR_DUMPER_SERVER` when `VAR_DUMPER_FORMAT=server` (Caddy sets both for
//! every PHP site while this listener is up). `marsad` listens on
//! `127.0.0.1:9912`, turns each line into text with `marsa_core::dumps`, and
//! keeps the newest 500 dumps in memory (a restart clears them).
//!
//! [`DumpsService`] is the one object of it in the [`Daemon`]: the listener
//! task, the store, and the event that tells the window about a new dump,
//! sent at most every 250 ms however many dumps arrive.

pub mod api;
pub mod caddy;
mod listener;
mod store;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use marsa_core::codes;
use marsa_core::types::{DumpList, DumpsStatus, Event};
use tokio::sync::{Notify, broadcast, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use self::listener::Shared;
use self::store::Store;
use crate::daemon::services::supervisor::lock;

/// `DumpsChanged` is sent at most this often.
const EVENT_EVERY: Duration = Duration::from_millis(250);
/// How long a stop waits for the connections to close.
pub const STOP_LIMIT: Duration = Duration::from_secs(2);

/// The listener, while it runs.
struct Running {
    cancel: CancellationToken,
    task: JoinHandle<()>,
    addr: SocketAddr,
}

/// What a state change of the service needs from one place at a time.
#[derive(Default)]
struct Tasks {
    listener: Option<Running>,
    /// The task that sends `DumpsChanged`: started once.
    events: Option<JoinHandle<()>>,
}

pub struct DumpsService {
    /// `127.0.0.1:port`; [`marsa_core::dumps::DUMPS_PORT`], or `0` in a
    /// test, which lets the system pick a free one.
    port: u16,
    idle: Duration,
    shared: Arc<Shared>,
    status: watch::Sender<DumpsStatus>,
    addr: Mutex<Option<SocketAddr>>,
    /// One change at a time. A `tokio::sync::Mutex` held across `.await` as
    /// a serialization token only (AGENTS.md section 6): the data it
    /// guards is the two task handles.
    tasks: tokio::sync::Mutex<Tasks>,
}

impl std::fmt::Debug for DumpsService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DumpsService")
            .field("port", &self.port)
            .field("status", &*self.status.borrow())
            .finish_non_exhaustive()
    }
}

impl DumpsService {
    /// A service that does not listen yet.
    pub fn new(port: u16) -> Self {
        Self::with_idle(port, listener::IDLE)
    }

    /// As [`DumpsService::new`], with the time a silent connection stays
    /// open: a test uses a short one.
    pub fn with_idle(port: u16, idle: Duration) -> Self {
        Self {
            port,
            idle,
            shared: Arc::new(Shared {
                store: Mutex::new(Store::default()),
                changed: Notify::new(),
            }),
            status: watch::Sender::new(DumpsStatus::Disabled),
            addr: Mutex::new(None),
            tasks: tokio::sync::Mutex::default(),
        }
    }

    pub fn status(&self) -> DumpsStatus {
        *self.status.borrow()
    }

    /// The status, and its changes: Caddy's config follows it.
    pub fn watch_status(&self) -> watch::Receiver<DumpsStatus> {
        self.status.subscribe()
    }

    /// Where it listens now.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        *lock(&self.addr)
    }

    /// The dumps with an id over `after` (all, if `None`), newest first.
    pub fn list(&self, after: Option<u64>) -> DumpList {
        let store = lock(&self.shared.store);
        DumpList {
            status: self.status(),
            latest_id: store.latest_id(),
            dumps: store.list(after),
        }
    }

    /// Forgets every dump. The ids go on from where they were.
    pub fn clear(&self) -> DumpList {
        lock(&self.shared.store).clear();
        self.shared.changed.notify_one();
        self.list(None)
    }

    /// Starts the task that sends `DumpsChanged` on `events`, until
    /// `shutdown`. Once: a second call does nothing.
    pub async fn start_events(
        &self,
        events: broadcast::Sender<Event>,
        shutdown: CancellationToken,
    ) {
        let mut tasks = self.tasks.lock().await;
        if tasks.events.is_some() {
            return;
        }
        let shared = Arc::clone(&self.shared);
        tasks.events = Some(tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    () = shared.changed.notified() => {}
                }
                let (latest_id, count) = {
                    let store = lock(&shared.store);
                    (store.latest_id(), store.count() as u64)
                };
                // No reader is not an error.
                let _ = events.send(Event::DumpsChanged { latest_id, count });
                // The dumps that arrive meanwhile are one event after this.
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    () = tokio::time::sleep(EVENT_EVERY) => {}
                }
            }
        }));
    }

    /// Makes the listener match `enabled`: binds the port, or closes it. A
    /// port that another program holds is the status `port_in_use`, not an
    /// error: the next call tries again. Nothing starts once `shutdown` is
    /// cancelled.
    pub async fn apply(&self, enabled: bool, shutdown: &CancellationToken) -> DumpsStatus {
        let mut tasks = self.tasks.lock().await;
        if !enabled {
            if let Some(running) = tasks.listener.take() {
                stop(running, STOP_LIMIT).await;
            }
            *lock(&self.addr) = None;
            self.status.send_replace(DumpsStatus::Disabled);
            return DumpsStatus::Disabled;
        }
        if tasks.listener.is_some() || shutdown.is_cancelled() {
            return self.status();
        }
        match listener::bind(self.port).await {
            Ok((tcp, addr)) => {
                info!(%addr, "the dump server listens");
                let cancel = shutdown.child_token();
                let task = tokio::spawn(listener::serve(
                    tcp,
                    Arc::clone(&self.shared),
                    cancel.clone(),
                    self.idle,
                ));
                tasks.listener = Some(Running { cancel, task, addr });
                *lock(&self.addr) = Some(addr);
                self.status.send_replace(DumpsStatus::Listening);
            }
            Err(err) => {
                // Not only `AddrInUse`: whatever stops the bind, `dump()`
                // stays in the page, and the user is told one reason.
                warn!(code = codes::DUMPS_PORT_IN_USE, %err, port = self.port,
                    "the dump server cannot listen; dump() stays in the page");
                *lock(&self.addr) = None;
                self.status.send_replace(DumpsStatus::PortInUse);
            }
        }
        self.status()
    }

    /// Closes the listener and the events task at the daemon's end.
    pub async fn shutdown(&self, limit: Duration) {
        let mut tasks = self.tasks.lock().await;
        if let Some(running) = tasks.listener.take() {
            stop(running, limit).await;
        }
        if let Some(events) = tasks.events.take() {
            // It ends on the daemon's `shutdown` token; this only waits.
            let _ = tokio::time::timeout(limit, events).await;
        }
        *lock(&self.addr) = None;
    }
}

async fn stop(running: Running, limit: Duration) {
    running.cancel.cancel();
    if tokio::time::timeout(limit, running.task).await.is_err() {
        warn!(addr = %running.addr, "the dump server did not stop in time");
    }
}

/// Starts the events task, and the listener unless `[dumps] enabled` is
/// `false` in `global.toml`. Before the API serves. A `global.toml` that
/// cannot be read leaves the default, which is on: its own routes report it.
pub async fn start_at_launch(daemon: &Arc<crate::daemon::state::Daemon>) {
    daemon
        .dumps()
        .start_events(daemon.events().clone(), daemon.shutdown().clone())
        .await;
    let worker = Arc::clone(daemon);
    let enabled = tokio::task::spawn_blocking(move || enabled_in_config(worker.data_dir()))
        .await
        .unwrap_or(true);
    daemon.dumps().apply(enabled, daemon.shutdown()).await;
}

/// `[dumps] enabled` of `global.toml`.
pub(crate) fn enabled_in_config(data_dir: &std::path::Path) -> bool {
    let path = marsa_core::config::ConfigPaths::in_dir(data_dir).global;
    match marsa_core::config::load_global(&path) {
        Ok(global) => global.dumps.enabled,
        Err(err) => {
            warn!(code = err.code(), %err, "cannot read [dumps]; dump capture is on");
            true
        }
    }
}

#[cfg(test)]
mod tests;
