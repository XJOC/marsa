//! Start-up and shutdown: lock file, bind, `run/daemon.json`, serve. Task M3-06.
//! The DNS server for `.test` starts and stops with it (task M4-12), and so
//! does the parked folder watcher (task M4-10d).

use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use axum::Router;
use marsa_core::platform::daemon_file::{self, DaemonFile, Token};
use marsa_core::platform::signals::shutdown_signal;
use marsa_core::types::Event;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use super::api;
use super::auth::AuthConfig;
use super::dns;
use super::sites_sync;
use super::state::Daemon;
use super::watch;

/// How long shutdown waits for the managed processes to stop.
const PROCS_LIMIT: Duration = Duration::from_secs(10);
/// How long shutdown then waits for open connections to finish.
const DRAIN_LIMIT: Duration = Duration::from_secs(5);
/// How long shutdown waits for the DNS server to close its sockets.
const DNS_STOP_LIMIT: Duration = Duration::from_secs(2);
/// How long shutdown waits for the parked folder watcher.
const WATCH_STOP_LIMIT: Duration = Duration::from_secs(2);
/// How long a busy `run/daemon.lock` is tried again before giving up. A
/// client probes it with a short shared lock every 50 ms; a probe must not
/// make a starting daemon exit.
const LOCK_RETRY_LIMIT: Duration = Duration::from_millis(300);
const LOCK_RETRY_EVERY: Duration = Duration::from_millis(20);

/// Another `marsad` holds `run/daemon.lock` for this data folder.
#[derive(Debug)]
pub struct AlreadyRunning {
    pub lock: PathBuf,
}

impl AlreadyRunning {
    /// Stable error name.
    pub fn code(&self) -> &'static str {
        "daemon_already_running"
    }
}

impl fmt::Display for AlreadyRunning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "another marsad holds {}", self.lock.display())
    }
}

impl std::error::Error for AlreadyRunning {}

/// Runs the daemon on `data_dir` until a stop request, then stops cleanly:
/// `shutting_down` to every reader, the processes stopped, the server
/// stopped, the DNS server stopped, `run/daemon.json` deleted, the lock
/// released.
///
/// # Errors
///
/// [`AlreadyRunning`] if another daemon holds the lock. Any other error if the
/// lock, the port, or `run/daemon.json` cannot be set up.
pub async fn run(data_dir: PathBuf) -> anyhow::Result<()> {
    let run_dir = data_dir.join("run");
    let lock = lock_instance(&run_dir).await?;
    // A file left by a crash goes as soon as this daemon owns the lock, so no
    // client reads its stale port and token.
    if let Err(err) = daemon_file::remove_daemon_file(&run_dir) {
        tracing::warn!(code = err.code(), error = %err, "cannot delete an old daemon.json");
    }
    // Before `daemon.json` exists, so a client that found the daemon can
    // already stop it with a signal.
    let signal = shutdown_signal();

    // Loopback only, never `0.0.0.0` or `::` (AGENTS.md section 6).
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .context("cannot bind 127.0.0.1")?;
    let port = listener.local_addr().context("no local address")?.port();

    let file = DaemonFile {
        pid: std::process::id(),
        port,
        token: Token::generate()?,
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
    };
    daemon_file::write_daemon_file(&run_dir, &file)?;
    tracing::info!(port, pid = file.pid, version = %file.app_version, "marsad started");
    let auth = AuthConfig::new(Arc::new(file.token), port);

    let daemon = Arc::new(Daemon::new(data_dir));
    // The first site list, so the first client gets it at once. A broken
    // sites.toml is logged; the routes report it again with its code.
    if let Err(err) = daemon.rebuild_sites().await {
        tracing::warn!(code = err.code, error = %err.message, "cannot build the site list");
    }
    // From here on, PHP pools and Caddy's config follow the sites (task M4-11).
    let sync = sites_sync::start(&daemon);
    // After the first build, so it starts from its parked folders. It stops
    // when `daemon.shutdown` is cancelled.
    let watcher = watch::start(&daemon);
    // After the API listener. It stops when `daemon.shutdown` is cancelled. A
    // port in use is only a warning: the daemon runs on without it.
    let dns = dns::start_for_daemon(&daemon.shutdown).await;
    let app = api::router(daemon.clone(), auth);
    let result = serve(listener, app, daemon, signal).await;
    // The watcher first: it is what asks the sync task for passes.
    if let Some(watcher) = watcher {
        watcher.stop(WATCH_STOP_LIMIT).await;
    }
    sync.stop().await;
    // Before the lock goes, so the next daemon finds the DNS port free.
    if let Some(dns) = dns {
        dns.stop(DNS_STOP_LIMIT).await;
    }

    // The file goes before the lock, so a daemon that takes the lock next
    // never finds our file.
    if let Err(err) = daemon_file::remove_daemon_file(&run_dir) {
        tracing::warn!(code = err.code(), error = %err, "cannot delete daemon.json");
    }
    // Released before the close: Windows may release a lock some time after
    // the handle closes.
    if let Err(err) = lock.unlock() {
        tracing::warn!(error = %err, "cannot unlock daemon.lock");
    }
    drop(lock);
    tracing::info!("marsad stopped");
    result
}

/// Takes `run_dir/daemon.lock` for this process, or fails with
/// [`AlreadyRunning`] if the lock stays busy for [`LOCK_RETRY_LIMIT`]. The OS
/// drops the lock if the process dies.
async fn lock_instance(run_dir: &Path) -> anyhow::Result<File> {
    // `write_daemon_file` closes the folder to other users right after.
    fs::create_dir_all(run_dir).with_context(|| format!("cannot create {}", run_dir.display()))?;
    let path = run_dir.join("daemon.lock");
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if started.elapsed() < LOCK_RETRY_LIMIT => {
                tokio::time::sleep(LOCK_RETRY_EVERY).await;
            }
            Err(TryLockError::WouldBlock) => return Err(AlreadyRunning { lock: path }.into()),
            Err(TryLockError::Error(err)) => {
                let context = format!("cannot lock {}", path.display());
                return Err(anyhow::Error::new(err).context(context));
            }
        }
    }
}

/// Serves until `POST /v1/shutdown` or `signal`, then stops in order.
async fn serve(
    listener: TcpListener,
    app: Router,
    daemon: Arc<Daemon>,
    signal: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let stopped = CancellationToken::new();
    let stop = {
        let stopped = stopped.clone();
        async move {
            tokio::select! {
                () = daemon.shutdown.cancelled() => {}
                () = signal => daemon.shutdown.cancel(),
            }
            // `/v1/events` streams end after this event, so their
            // connections can close.
            daemon.publish(Event::ShuttingDown);
            daemon.stop_procs(PROCS_LIMIT).await;
            // Now the server stops taking connections and waits for the open ones.
            stopped.cancel();
        }
    };
    let server = axum::serve(listener, app).with_graceful_shutdown(stop);
    let drain_limit = async {
        stopped.cancelled().await;
        tokio::time::sleep(DRAIN_LIMIT).await;
    };
    tokio::select! {
        result = server.into_future() => result.context("the API server failed"),
        () = drain_limit => {
            tracing::warn!(limit = ?DRAIN_LIMIT, "connections still open; closing them");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(run_dir: &Path) -> File {
        let probe = File::open(run_dir.join("daemon.lock")).unwrap();
        probe.try_lock_shared().unwrap();
        probe
    }

    #[tokio::test]
    async fn a_short_shared_probe_does_not_stop_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let first = lock_instance(dir.path()).await.unwrap();
        first.unlock().unwrap();
        drop(first);

        // A client's probe, released 100 ms later.
        let probe = probe(dir.path());
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            probe.unlock().unwrap();
        });
        let started = Instant::now();
        let lock = lock_instance(dir.path()).await;
        release.await.unwrap();
        assert!(lock.is_ok(), "{lock:?}");
        // It waited for the probe, and did not give up.
        assert!(started.elapsed() >= Duration::from_millis(100));
    }

    #[tokio::test]
    async fn a_lock_held_longer_is_already_running() {
        let dir = tempfile::tempdir().unwrap();
        let _held = lock_instance(dir.path()).await.unwrap();
        let started = Instant::now();
        let err = lock_instance(dir.path()).await.unwrap_err();
        assert!(started.elapsed() >= LOCK_RETRY_LIMIT);
        assert!(err.downcast_ref::<AlreadyRunning>().is_some(), "{err:#}");
    }
}
