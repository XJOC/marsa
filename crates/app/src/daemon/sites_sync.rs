//! The one task that keeps PHP and Caddy in line with the sites. Task M4-11.
//!
//! It runs a pass at start-up, after the first site list. Then it runs one
//! after each `sites_changed`, each time Caddy starts again, and after each
//! PHP install, 200 ms after the first event of a burst ([`DEBOUNCE`]). Each
//! pass, in order:
//!
//! 1. PHP: the pools the sites need, and `php.ini` of every installed PHP
//!    line ([`PhpManager::sync`]).
//! 2. Caddy: the config of the sites and pools ([`CaddyManager::apply`]).
//! 3. The CA bundle for PHP, once Caddy has made its CA. Caddy makes it at
//!    the first load of a config with a secure site.
//! 4. PHP again if the bundle just appeared: `php.ini` then points to it, so
//!    those pools restart.
//!
//! One task, so two passes never overlap, like a queue with one worker. It
//! owns the [`PhpManager`], so it alone starts and stops the pools. It stops
//! with the daemon's shutdown token.
//!
//! A route that changed a setting the pass reads (`PUT /v1/php/xdebug`, task
//! M5-11) asks for a pass through [`PassRequests`] and waits for it, like a
//! job pushed on the worker's queue with a reply slot. A request is answered
//! after a pass that started after it, so that pass saw the change.
//!
//! [`CaddyManager::apply`]: super::caddy::CaddyManager::apply

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use marsa_core::caddy::{CaddyPaths, PhpPools, system_ca_bundles};
use marsa_core::recipes::Platform;
use marsa_core::runtimes::Runtime;
use marsa_core::sites::Site;
use marsa_core::types::{Event, InstallUpdate, ServiceState};
use tokio::sync::broadcast::{self, error::RecvError, error::TryRecvError};
use tokio::sync::{mpsc, oneshot};
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

/// How many pass requests the queue holds. A route whose request finds it
/// full waits for room.
const REQUESTS: usize = 16;

/// The reply slot of one pass request: the task sends `()` on it after the
/// pass.
type Reply = oneshot::Sender<()>;

/// How a route asks the sync task for a pass. `Daemon::pass_requests`.
///
/// [`start`] sets the sending end once; the task owns the receiving end. No
/// lock: a waiting route only borrows the sending end.
#[derive(Debug, Default)]
pub struct PassRequests(OnceLock<mpsc::Sender<Reply>>);

/// The requested pass did not run to its end: the sync task does not run, or
/// it stopped first because the daemon shuts down. A setting saved before
/// the request is applied by the start-up pass of the next daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassNotRun;

impl PassRequests {
    /// Asks for a pass and waits until a pass that started after this call
    /// has ended. Requests that wait together share one pass.
    ///
    /// # Errors
    ///
    /// [`PassNotRun`] when the sync task does not run, or stops before the
    /// pass ends (it then drops the reply slot, so this never hangs on a
    /// stopped task).
    pub async fn run_pass(&self) -> Result<(), PassNotRun> {
        let requests = self.0.get().ok_or(PassNotRun)?;
        let (reply, done) = oneshot::channel();
        requests.send(reply).await.map_err(|_| PassNotRun)?;
        done.await.map_err(|_| PassNotRun)
    }
}

/// The running sync task.
#[derive(Debug)]
pub struct SitesSync {
    task: JoinHandle<()>,
}

/// Starts the task. It reads the events from now on, so no change after the
/// first site list is missed. It ends when `daemon.shutdown` is cancelled.
pub fn start(daemon: &Arc<Daemon>) -> SitesSync {
    let events = daemon.events.subscribe();
    let (sender, requests) = mpsc::channel(REQUESTS);
    if daemon.pass_requests.0.set(sender).is_err() {
        warn!("the sites sync task started twice; pass requests go to the first");
    }
    SitesSync {
        task: tokio::spawn(run(Arc::clone(daemon), events, requests)),
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

async fn run(
    daemon: Arc<Daemon>,
    mut events: broadcast::Receiver<Event>,
    mut requests: mpsc::Receiver<Reply>,
) {
    let platform = Platform::current();
    let mut php = PhpManager::new(&daemon.data_dir, platform);
    let system = platform.map(system_ca_bundles).unwrap_or_default();
    let shutdown = daemon.shutdown.clone();
    // The requests the coming pass answers. A return drops them unanswered,
    // and with them `requests`, so each waiting route gets `PassNotRun`.
    let mut replies = Vec::new();
    loop {
        // Every request queued before the pass starts: its route saved its
        // change first, so this pass reads it. Requests that come during
        // the pass wait in the queue for the next one.
        take_queued(&mut requests, &mut replies);
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = pass(&daemon, &mut php, &system) => {}
        }
        for reply in replies.drain(..) {
            // `Err` only when the route is gone (its client hung up).
            reply.send(()).ok();
        }
        tokio::select! {
            () = shutdown.cancelled() => return,
            Some(reply) = requests.recv() => {
                // A route waits: no debounce. The pass covers the events so
                // far too.
                replies.push(reply);
                skip_waiting(&mut events);
                continue;
            }
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

/// Moves the queued requests to `replies`.
fn take_queued(requests: &mut mpsc::Receiver<Reply>, replies: &mut Vec<Reply>) {
    while let Ok(reply) = requests.try_recv() {
        replies.push(reply);
    }
}

/// Whether `event` asks for a pass: the sites changed, Caddy started again
/// (it has read `caddy.json`, but it may have just made its CA), or a PHP
/// was installed (its line may have no `php.ini` yet).
pub fn is_trigger(event: &Event) -> bool {
    match event {
        Event::SitesChanged(_) => true,
        Event::Install(install) => {
            matches!(&install.update, InstallUpdate::Done(done) if done.runtime == Runtime::Php)
        }
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
    use marsa_core::types::{InstallDone, InstallProgress, ServiceStatus, SiteList};
    use serde_json::Value;
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

        // A new PHP needs its `php.ini`; a new Node needs nothing here.
        let installed = |runtime| {
            let update = InstallUpdate::Done(InstallDone {
                runtime,
                version: "1.2.3".into(),
                dir: "/data/runtimes/x/1.2.3".into(),
                already: false,
            });
            Event::Install(InstallProgress { id: 1, update })
        };
        assert!(is_trigger(&installed(Runtime::Php)));
        assert!(!is_trigger(&installed(Runtime::Node)));
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

    /// A fake Caddy admin API that holds each `POST /load` until the test
    /// lets it go: the pass that sent it runs on meanwhile. Each load sends
    /// its release on the channel.
    async fn held_admin() -> (u16, mpsc::UnboundedReceiver<oneshot::Sender<()>>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let app = axum::Router::new().route(
            "/load",
            post(move || async move {
                let (release, held) = oneshot::channel();
                tx.send(release).unwrap();
                // The test lets it go, or drops the release.
                held.await.ok();
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (port, rx)
    }

    /// The release of the next held load: a pass is at its Caddy step.
    async fn next_held(
        loads: &mut mpsc::UnboundedReceiver<oneshot::Sender<()>>,
    ) -> oneshot::Sender<()> {
        tokio::time::timeout(Duration::from_secs(10), loads.recv())
            .await
            .expect("a POST /load in time")
            .expect("the fake admin API runs")
    }

    /// A daemon whose Caddy "runs" behind [`held_admin`], with its sync task.
    async fn held_daemon() -> (
        Arc<Daemon>,
        SitesSync,
        mpsc::UnboundedReceiver<oneshot::Sender<()>>,
        tempfile::TempDir,
    ) {
        let (port, loads) = held_admin().await;
        let data = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::new(data.path().to_path_buf());
        daemon.caddy.input.admin_port = port;
        let daemon = Arc::new(daemon);
        pretend_caddy_runs(&daemon);
        let sync = start(&daemon);
        (daemon, sync, loads, data)
    }

    /// Waits until `count` requests are in the task's queue.
    async fn queued(daemon: &Daemon, count: usize) {
        let requests = daemon.pass_requests.0.get().expect("the task started");
        for _ in 0..200 {
            if requests.max_capacity() - requests.capacity() == count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{count} requests were not queued");
    }

    /// A route asks for a pass: what `PUT /v1/php/xdebug` does after it saved
    /// the setting.
    fn ask(daemon: &Arc<Daemon>) -> JoinHandle<Result<(), PassNotRun>> {
        let daemon = Arc::clone(daemon);
        tokio::spawn(async move { daemon.pass_requests.run_pass().await })
    }

    #[tokio::test]
    async fn requests_during_a_pass_are_answered_together_after_the_next_one() {
        let (daemon, sync, mut loads, _data) = held_daemon().await;

        // The start-up pass is at its Caddy step. Two requests come now.
        let first = next_held(&mut loads).await;
        let (one, two) = (ask(&daemon), ask(&daemon));
        queued(&daemon, 2).await;
        first.send(()).ok();

        // The pass that was running did not answer them: it may have read
        // the settings before they were saved. The next pass serves both.
        let second = next_held(&mut loads).await;
        tokio::task::yield_now().await;
        assert!(!one.is_finished() && !two.is_finished());
        second.send(()).ok();
        assert_eq!(one.await.unwrap(), Ok(()));
        assert_eq!(two.await.unwrap(), Ok(()));
        // One pass for both: no third load comes.
        let third = tokio::time::timeout(DEBOUNCE * 2, loads.recv()).await;
        assert!(third.is_err(), "a third pass ran");

        daemon.shutdown.cancel();
        sync.stop().await;
        daemon.stop_procs(Duration::from_secs(5)).await;
    }

    #[tokio::test]
    async fn a_request_waiting_at_shutdown_fails_instead_of_hanging() {
        // No sync task: nothing could answer.
        let dir = tempfile::tempdir().unwrap();
        let alone = Daemon::new(dir.path().to_path_buf());
        assert_eq!(alone.pass_requests.run_pass().await, Err(PassNotRun));

        let (daemon, sync, mut loads, _data) = held_daemon().await;
        let first = next_held(&mut loads).await;
        let waiting = ask(&daemon);
        queued(&daemon, 1).await;

        daemon.shutdown.cancel();
        let answer = tokio::time::timeout(Duration::from_secs(5), waiting).await;
        assert_eq!(answer.expect("no hang").unwrap(), Err(PassNotRun));
        sync.stop().await;
        // After the task ended, a new request fails at once.
        assert_eq!(daemon.pass_requests.run_pass().await, Err(PassNotRun));
        drop(first);
        daemon.stop_procs(Duration::from_secs(5)).await;
    }
}
