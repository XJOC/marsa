//! Start-up and shutdown: lock file, bind, `run/daemon.json`, serve. Task M3-06.

use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use axum::Router;
use marsa_core::platform::daemon_file::{self, DaemonFile, Token};
use marsa_core::platform::signals::shutdown_signal;
use marsa_core::types::Event;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use super::api;
use super::auth::AuthConfig;
use super::state::Daemon;

/// How long shutdown waits for the managed processes to stop.
const PROCS_LIMIT: Duration = Duration::from_secs(10);
/// How long shutdown then waits for open connections to finish.
const DRAIN_LIMIT: Duration = Duration::from_secs(5);

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
/// stopped, `run/daemon.json` deleted, the lock released.
///
/// # Errors
///
/// [`AlreadyRunning`] if another daemon holds the lock. Any other error if the
/// lock, the port, or `run/daemon.json` cannot be set up.
pub async fn run(data_dir: PathBuf) -> anyhow::Result<()> {
    let run_dir = data_dir.join("run");
    let lock = lock_instance(&run_dir)?;
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
    let app = api::router(daemon.clone(), auth);
    let result = serve(listener, app, daemon, signal).await;

    // The file goes before the lock, so a daemon that takes the lock next
    // never finds our file.
    if let Err(err) = daemon_file::remove_daemon_file(&run_dir) {
        tracing::warn!(code = err.code(), error = %err, "cannot delete daemon.json");
    }
    // Closing the file releases the lock.
    drop(lock);
    tracing::info!("marsad stopped");
    result
}

/// Takes `run_dir/daemon.lock` for this process, or fails with
/// [`AlreadyRunning`]. The OS drops the lock if the process dies.
fn lock_instance(run_dir: &Path) -> anyhow::Result<File> {
    // `write_daemon_file` closes the folder to other users right after.
    fs::create_dir_all(run_dir).with_context(|| format!("cannot create {}", run_dir.display()))?;
    let path = run_dir.join("daemon.lock");
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(AlreadyRunning { lock: path }.into()),
        Err(TryLockError::Error(err)) => {
            Err(anyhow::Error::new(err).context(format!("cannot lock {}", path.display())))
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
