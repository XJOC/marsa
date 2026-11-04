//! The one task that keeps PHP and Caddy in line with the sites. Task M4-11.
//!
//! It runs a pass at start-up, after the first site list. Then it runs one
//! after each `sites_changed`, and each time Caddy starts again, 200 ms after
//! the first event of a burst ([`DEBOUNCE`]). Each pass, in order:
//!
//! 1. PHP: the pools the sites need ([`PhpManager::sync`]).
//! 2. Caddy: the config of the sites and pools ([`CaddyManager::apply`]).
//! 3. The CA bundle for PHP, once Caddy has made its CA. Caddy makes it at
//!    the first load of a config with a secure site.
//! 4. PHP again if the bundle just appeared: `php.ini` then points to it, so
//!    those pools restart.
//!
//! One task, so two passes never overlap, like a queue with one worker. It
//! stops with the daemon's shutdown token.
//!
//! [`CaddyManager::apply`]: super::caddy::CaddyManager::apply

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use marsa_core::caddy::{CaddyPaths, PhpPools, system_ca_bundles};
use marsa_core::recipes::Platform;
use marsa_core::sites::Site;
use marsa_core::types::{Event, ServiceState};
use tokio::sync::broadcast::{self, error::RecvError, error::TryRecvError};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use super::caddy::CADDY_ID;
use super::php::{PhpManager, write_ca_bundle};
use super::state::Daemon;

/// How long a pass waits after the first event, so a burst of changes (a
/// parked folder with many sites, a few patches in a row) makes one pass.
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// How long [`SitesSync::stop`] waits for the task to end.
const STOP_LIMIT: Duration = Duration::from_secs(2);

/// The running sync task.
#[derive(Debug)]
pub struct SitesSync {
    task: JoinHandle<()>,
}

/// Starts the task. It reads the events from now on, so no change after the
/// first site list is missed. It ends when `daemon.shutdown` is cancelled.
pub fn start(daemon: &Arc<Daemon>) -> SitesSync {
    let events = daemon.events.subscribe();
    SitesSync {
        task: tokio::spawn(run(Arc::clone(daemon), events)),
    }
}

impl SitesSync {
    /// Waits for the task to end after the shutdown token, at most
    /// [`STOP_LIMIT`], then aborts it.
    pub async fn stop(self) {
        let abort = self.task.abort_handle();
        match tokio::time::timeout(STOP_LIMIT, self.task).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => warn!(error = %err, "the sites sync task failed"),
            Err(_) => {
                warn!(limit = ?STOP_LIMIT, "the sites sync task still runs; aborting it");
                abort.abort();
            }
        }
    }
}

async fn run(daemon: Arc<Daemon>, mut events: broadcast::Receiver<Event>) {
    let platform = Platform::current();
    let mut php = PhpManager::new(&daemon.data_dir, platform);
    let system = platform.map(system_ca_bundles).unwrap_or_default();
    let shutdown = daemon.shutdown.clone();
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = pass(&daemon, &mut php, &system) => {}
        }
        tokio::select! {
            () = shutdown.cancelled() => return,
            more = next_trigger(&mut events) => if !more {
                return;
            },
        }
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(DEBOUNCE) => skip_waiting(&mut events),
        }
    }
}

/// Whether `event` asks for a pass: the sites changed, or Caddy started
/// again (it has read `caddy.json`, but it may have just made its CA).
pub fn is_trigger(event: &Event) -> bool {
    match event {
        Event::SitesChanged(_) => true,
        Event::ServiceChanged(status) => {
            status.id == CADDY_ID && status.state == ServiceState::Running
        }
        _ => false,
    }
}

/// Waits for the next event that asks for a pass. `false` when the bus is
/// closed, which happens only when the daemon is gone.
async fn next_trigger(events: &mut broadcast::Receiver<Event>) -> bool {
    loop {
        match events.recv().await {
            Ok(event) if is_trigger(&event) => return true,
            Ok(_) => {}
            // Events were lost, maybe a change among them.
            Err(RecvError::Lagged(_)) => return true,
            Err(RecvError::Closed) => return false,
        }
    }
}

/// Drops the events that came during the wait: the coming pass reads the
/// latest sites, so it covers them.
fn skip_waiting(events: &mut broadcast::Receiver<Event>) {
    loop {
        match events.try_recv() {
            Ok(_) | Err(TryRecvError::Lagged(_)) => {}
            Err(TryRecvError::Empty | TryRecvError::Closed) => return,
        }
    }
}

/// One pass: PHP, Caddy, the CA bundle, then PHP again if the bundle is new.
async fn pass(daemon: &Daemon, php: &mut PhpManager, system: &[&'static Path]) {
    let sites = daemon.sites().map(|list| list.sites).unwrap_or_default();
    let bundle = daemon.caddy.paths.ca_bundle();
    let had_bundle = tokio::fs::try_exists(&bundle).await.unwrap_or(false);
    let ca = had_bundle.then_some(bundle.as_path());
    let pools = php.sync(daemon, &sites, ca).await;
    apply_caddy(daemon, &sites, pools.clone()).await;

    if !write_bundle(&daemon.caddy.paths, system).await || had_bundle {
        return;
    }
    let again = php.sync(daemon, &sites, Some(&bundle)).await;
    // The addresses do not depend on `php.ini`, but a pool that failed to
    // start again changes them.
    if again != pools {
        apply_caddy(daemon, &sites, again).await;
    }
}

async fn apply_caddy(daemon: &Daemon, sites: &[Site], pools: PhpPools) {
    match daemon.caddy.apply(daemon, sites.to_vec(), pools).await {
        Ok(applied) => debug!(?applied, sites = sites.len(), "caddy config applied"),
        Err(err) => warn!(code = err.code(), error = %err, "the new Caddy config was not applied"),
    }
}

/// [`write_ca_bundle`] on a blocking thread. `true` when the bundle is
/// written and current.
async fn write_bundle(paths: &CaddyPaths, system: &[&'static Path]) -> bool {
    let paths = paths.clone();
    let system = system.to_vec();
    let job = move || write_ca_bundle(&paths, &system);
    match tokio::task::spawn_blocking(job).await {
        Ok(Ok(written)) => written,
        Ok(Err(err)) => {
            warn!(%err, "cannot write the CA bundle for PHP");
            false
        }
        Err(err) => {
            warn!(error = %err, "the CA bundle job failed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    use axum::Json;
    use axum::routing::post;
    use marsa_core::types::{ServiceStatus, SiteList};
    use serde_json::Value;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use crate::daemon::supervisor::{ServiceHandle, ServiceRuntime};

    fn status(id: &str, state: ServiceState) -> Event {
        Event::ServiceChanged(ServiceStatus {
            id: id.into(),
            state,
            pid: Some(1),
            restarts: 0,
            since_ms: None,
        })
    }

    #[test]
    fn sites_changes_and_a_caddy_start_ask_for_a_pass() {
        assert!(is_trigger(&Event::SitesChanged(SiteList::default())));
        assert!(is_trigger(&status("caddy", ServiceState::Running)));
        assert!(!is_trigger(&status("caddy", ServiceState::Starting)));
        assert!(!is_trigger(&status("php-fpm-8.4", ServiceState::Running)));
        assert!(!is_trigger(&Event::ShuttingDown));
    }

    /// A fake Caddy admin API that sends the body of each `POST /load` on the
    /// channel.
    async fn fake_admin() -> (u16, mpsc::UnboundedReceiver<Value>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let app = axum::Router::new().route(
            "/load",
            post(move |Json(config): Json<Value>| async move {
                tx.send(config).unwrap();
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (port, rx)
    }

    /// Registers a task as the running Caddy, so the config goes to the admin
    /// API.
    fn pretend_caddy_runs(daemon: &Daemon) {
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let cancel = cancel.clone();
            async move { cancel.cancelled().await }
        });
        let runtime = Arc::new(Mutex::new(ServiceRuntime::new(CADDY_ID)));
        let handle = ServiceHandle {
            cancel,
            task,
            runtime,
        };
        daemon.add_proc(CADDY_ID, handle);
    }

    async fn next_load(loads: &mut mpsc::UnboundedReceiver<Value>) -> Value {
        let wait = Duration::from_secs(10);
        tokio::time::timeout(wait, loads.recv())
            .await
            .expect("a POST /load in time")
            .expect("the fake admin API runs")
    }

    #[tokio::test]
    async fn a_parked_site_reaches_caddy_through_post_load() {
        let (port, mut loads) = fake_admin().await;
        let data = tempfile::tempdir().unwrap();
        let parked = tempfile::tempdir().unwrap();
        fs::create_dir(parked.path().join("shop")).unwrap();
        fs::write(parked.path().join("shop").join("index.html"), "hi").unwrap();

        let mut daemon = Daemon::new(data.path().to_path_buf());
        daemon.caddy.input.admin_port = port;
        let daemon = Arc::new(daemon);
        daemon.rebuild_sites().await.unwrap();
        pretend_caddy_runs(&daemon);
        let sync = start(&daemon);

        // The pass at start-up: no site yet, so the greeting only.
        let first = next_load(&mut loads).await;
        assert!(first["apps"]["http"]["servers"].get("marsa-tls").is_none());
        let tag = first["admin"]["@id"].as_str().unwrap();
        assert!(tag.starts_with("marsa-instance-"), "{tag}");
        assert_eq!(
            first["apps"]["http"]["servers"]["marsa"]["@id"],
            "marsa-srv"
        );

        let dir = parked.path().to_path_buf();
        let park = move |sites: &mut marsa_core::config::SitesConfig| {
            sites.parked.push(dir);
            Ok(true)
        };
        daemon.change_sites(park).await.unwrap();
        let loaded = next_load(&mut loads).await;
        let tls = &loaded["apps"]["http"]["servers"]["marsa-tls"];
        assert_eq!(tls["routes"][0]["match"][0]["host"][0], "shop.test");
        assert_eq!(tls["listen"][0], "127.0.0.1:18443");
        // The file holds what Caddy got, for its next start.
        let text = fs::read_to_string(&daemon.caddy.paths.config_file).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), loaded);

        daemon.shutdown.cancel();
        sync.stop().await;
        daemon.stop_procs(Duration::from_secs(5)).await;
    }
}
