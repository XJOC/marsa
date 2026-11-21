//! Starting Caddy (and a database server) in the background when the daemon starts.

use super::CADDY_ID;
use crate::daemon::state::Daemon;
use marsa_core::types::{ServiceAction, ServiceState, ServiceStatus};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::{info, warn};

/// How the start of a service with the daemon ended: Caddy (task M5-16),
/// and PostgreSQL (`databases::start_at_launch`, task M6-03).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launched {
    /// The service is off in `global.toml` (PostgreSQL): nothing was tried,
    /// and the status is unchanged.
    Off,
    /// Nothing to run is installed: nothing was tried, and the status is
    /// unchanged.
    NotInstalled,
    /// The start ran. Caddy's status after it: `Running`, or `Failed` with
    /// the reason.
    Ran(ServiceStatus),
    /// Another action on Caddy ran first, a request for example. It decides.
    Busy,
    /// The start failed before Caddy ran, with this code. The status is
    /// `Failed` with the same code.
    Failed(&'static str),
    /// The daemon began to stop first, and the start was dropped. The
    /// shutdown stops a Caddy it had spawned (see [`start_at_launch`]).
    ShuttingDown,
}

/// The start of a service with the daemon. Its task ends after the start, or
/// at once when the daemon's shutdown token is cancelled.
#[derive(Debug)]
pub struct Launch {
    /// The service id, for the logs.
    service: &'static str,
    state: LaunchState,
}

#[derive(Debug)]
enum LaunchState {
    /// Decided before any task: no Caddy, or another action runs.
    Ended(Launched),
    Running(JoinHandle<Launched>),
}

impl Launch {
    /// A start decided before any task.
    pub(in crate::daemon) fn ended(service: &'static str, launched: Launched) -> Self {
        let state = LaunchState::Ended(launched);
        Self { service, state }
    }

    /// A start that runs in `task`.
    pub(in crate::daemon) fn running(service: &'static str, task: JoinHandle<Launched>) -> Self {
        let state = LaunchState::Running(task);
        Self { service, state }
    }

    /// Waits for the start to end, at most `limit`, then aborts it. `None`
    /// when it did not end in time or its task failed.
    pub async fn wait(self, limit: Duration) -> Option<Launched> {
        let service = self.service;
        let task = match self.state {
            LaunchState::Ended(launched) => return Some(launched),
            LaunchState::Running(task) => task,
        };
        let abort = task.abort_handle();
        match tokio::time::timeout(limit, task).await {
            Ok(Ok(launched)) => Some(launched),
            Ok(Err(err)) => {
                warn!(%service, error = %err, "the start with the daemon failed");
                None
            }
            Err(_) => {
                warn!(%service, ?limit, "the start with the daemon still runs; aborting it");
                abort.abort();
                None
            }
        }
    }
}

/// Starts Caddy in the background when it is installed (task M5-16), so the
/// sites answer after a reboot or a `marsa daemon stop` with no step by hand.
/// The path of `marsa services start caddy`, with the orphan check (ADR 0009)
/// and one action at a time. Its turn is taken here, before the task and
/// before the API serves, so a request always comes after it: it waits for
/// the start ([`CaddyManager::run_waiting`]) and never starts a second Caddy.
/// The API serves meanwhile.
///
/// The config is the latest that the sites sync gave
/// [`CaddyManager::apply`]: `start` and `apply` take turns on `pushing`, so a
/// pass that comes after the start sends its config with `POST /load`. Caddy's
/// `Running` also asks for a pass.
///
/// A shutdown drops the start at its next `.await`. The spawn of Caddy and
/// `add_proc` have no `.await` between them, so a Caddy it spawned is in
/// `Daemon::procs`, which the shutdown stops; `server::run` sweeps `procs`
/// again at its end for one added after the first sweep. A Caddy that the
/// start was stopping after a time-out was told to stop before that `.await`.
/// `kill_on_drop` is the last guard, as for every Caddy.
pub fn start_at_launch(daemon: &Arc<Daemon>) -> Launch {
    let caddy = &daemon.caddy;
    // First, so a missing Caddy does not mark Caddy's status failed.
    if !caddy.installed() {
        info!("no caddy is installed; it does not start with the daemon");
        return Launch::ended(CADDY_ID, Launched::NotInstalled);
    }
    let Ok(busy) = Arc::clone(&caddy.busy).try_lock_owned() else {
        info!("another action on caddy runs; it decides instead of the start with the daemon");
        return Launch::ended(CADDY_ID, Launched::Busy);
    };
    info!("starting caddy with the daemon");
    let daemon = Arc::clone(daemon);
    let task = tokio::spawn(async move {
        tokio::select! {
            // A daemon that already stops starts nothing. Dropping the start
            // lets go of its turn.
            biased;
            () = daemon.shutdown.cancelled() => Launched::ShuttingDown,
            launched = launch(&daemon, busy) => launched,
        }
    });
    Launch::running(CADDY_ID, task)
}

async fn launch(daemon: &Daemon, busy: tokio::sync::OwnedMutexGuard<()>) -> Launched {
    let caddy = &daemon.caddy;
    match caddy.act(daemon, ServiceAction::Start, busy).await {
        Ok(status) => {
            match &status.state {
                ServiceState::Running => info!("caddy started with the daemon"),
                // `marsa services` and `marsa doctor` show the code too.
                ServiceState::Failed { code } => {
                    warn!(%code, "caddy did not start with the daemon");
                }
                state => warn!(
                    ?state,
                    "caddy is not running after its start with the daemon"
                ),
            }
            Launched::Ran(status)
        }
        Err(err) => {
            warn!(code = err.code(), %err, "caddy did not start with the daemon");
            Launched::Failed(err.code())
        }
    }
}
