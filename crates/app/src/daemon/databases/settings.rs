//! `[postgres]` in `global.toml`, the work after an install, and the start
//! with the daemon.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use marsa_core::config::{self, ConfigError, ConfigPaths, ServerSettings};
use marsa_core::runtimes::{Runtime, RuntimesDir};
use marsa_core::types::{ServiceAction, ServiceState};
use semver::Version;
use tracing::{info, warn};

use super::POSTGRES_ID;
use super::manager::ActionError;
use crate::daemon::caddy::{Launch, Launched};
use crate::daemon::state::Daemon;

/// [`set_enabled`] on a blocking thread, under `Daemon::lock_global`.
pub(super) async fn save_enabled(daemon: &Arc<Daemon>, enabled: bool) -> Result<(), ActionError> {
    let worker = Arc::clone(daemon);
    let job = tokio::task::spawn_blocking(move || {
        let _one = worker.lock_global();
        set_enabled(&worker.data_dir, enabled)
    });
    match job.await {
        Ok(saved) => saved.map_err(|err| {
            warn!(code = err.code(), %err, enabled, "cannot save [postgres] enabled");
            ActionError::Config(err)
        }),
        Err(err) => {
            warn!(%err, "the [postgres] enabled job failed");
            Err(ActionError::Internal)
        }
    }
}

/// Turns PostgreSQL on or off in `global.toml` (`[postgres] enabled`), for
/// the start with the daemon. Writes only a change. The caller holds
/// `Daemon::lock_global`.
///
/// # Errors
///
/// The `config_*` codes when `global.toml` cannot be read or written.
fn set_enabled(data_dir: &Path, enabled: bool) -> Result<(), ConfigError> {
    let path = ConfigPaths::in_dir(data_dir).global;
    let mut global = config::load_global(&path)?;
    if global.postgres.enabled == enabled {
        return Ok(());
    }
    global.postgres.enabled = enabled;
    config::save_global(&path, &global)
}

/// What an install of PostgreSQL `version` changes in `[postgres]`, now
/// that `installed` holds it, and whether PostgreSQL then starts:
///
/// - no `default`, and no other version installed: the first install. Its
///   line becomes the default, it is turned on, and it starts (ADR 0023,
///   section 5: the install is the user's request);
/// - no `default`, and other versions installed: the default becomes the
///   line that ran until now, the newest of the others, so the next start
///   does not move to the new line's empty cluster. `enabled` stays;
/// - a `default` whose line has no installed version any more (its last
///   one was uninstalled): the new line becomes the default, and it starts
///   if it is on. Otherwise no start could ever run again without a hand
///   edit of `global.toml`;
/// - any other `default`, or one that is not a number: nothing changes.
///
/// A `version` that is no longer in `installed` (an uninstall came first)
/// changes nothing.
pub(super) fn after_install(
    settings: &mut ServerSettings,
    version: &Version,
    installed: &[Version],
) -> bool {
    // An uninstall took PostgreSQL's turn first and removed it.
    if !installed.contains(version) {
        return false;
    }
    let others = installed.iter().filter(|other| *other != version);
    match settings.default.as_deref() {
        None => match others.max() {
            None => {
                settings.default = Some(version.major.to_string());
                settings.enabled = true;
                true
            }
            Some(ran) => {
                settings.default = Some(ran.major.to_string());
                false
            }
        },
        Some(line) => match line.trim().parse::<u64>() {
            Ok(major) if !installed.iter().any(|v| v.major == major) => {
                settings.default = Some(version.major.to_string());
                settings.enabled
            }
            _ => false,
        },
    }
}

/// [`after_install`] on `global.toml`, written only when it changes. Returns
/// whether PostgreSQL starts. The caller holds `Daemon::lock_global`.
///
/// # Errors
///
/// The `config_*` codes when `global.toml` cannot be read or written.
pub(super) fn first_install(
    data_dir: &Path,
    version: &Version,
    installed: &[Version],
) -> Result<bool, ConfigError> {
    let path = ConfigPaths::in_dir(data_dir).global;
    let mut global = config::load_global(&path)?;
    let before = global.postgres.clone();
    let start = after_install(&mut global.postgres, version, installed);
    if global.postgres != before {
        config::save_global(&path, &global)?;
    }
    Ok(start)
}

/// After an install of PostgreSQL `version`, in the background: takes
/// PostgreSQL's turn, writes `[postgres]` ([`first_install`]), and starts it
/// in that same turn when it says so. A stop asked meanwhile waits for the
/// turn and drops the start ([`super::PostgresManager::run_waiting`]); a daemon
/// that stops starts nothing. A failure is only logged, or sets the status:
/// the install has succeeded either way.
pub fn start_after_install(daemon: &Arc<Daemon>, version: Version) {
    let daemon = Arc::clone(daemon);
    tokio::spawn(async move {
        let postgres = &daemon.postgres;
        let turn = tokio::time::timeout(START_WAIT, Arc::clone(&postgres.busy).lock_owned());
        let turn = tokio::select! {
            biased;
            () = daemon.shutdown.cancelled() => return,
            turn = turn => turn,
        };
        let Ok(turn) = turn else {
            warn!(%version, "another action on postgres ran too long; [postgres] is as it was");
            return;
        };
        let worker = Arc::clone(&daemon);
        let installed = version.clone();
        let job = tokio::task::spawn_blocking(move || {
            let _one = worker.lock_global();
            // A list that cannot be read changes nothing: it could hide
            // another line.
            let versions = RuntimesDir::in_dir(&worker.data_dir)
                .installed(Runtime::Postgres)
                .map_err(|err| {
                    warn!(code = err.code(), %err, "cannot list the PostgreSQL versions");
                })
                .ok()?;
            Some(first_install(&worker.data_dir, &installed, &versions))
        });
        match job.await {
            Ok(Some(Ok(true))) => info!(%version, "the first PostgreSQL: on, and starting it"),
            Ok(None | Some(Ok(false))) => return,
            Ok(Some(Err(err))) => {
                warn!(code = err.code(), %err, "cannot write [postgres] after the install");
                return;
            }
            Err(err) => {
                warn!(%err, "cannot write [postgres] after the install");
                return;
            }
        }
        let start = postgres.act(&daemon, ServiceAction::Start, &turn);
        let started = tokio::select! {
            biased;
            () = daemon.shutdown.cancelled() => return,
            started = start => started,
        };
        drop(turn);
        match started {
            Ok(_) => info!(state = ?postgres.status().state, "postgres after its first install"),
            Err(err) => warn!(code = err.code(), %err, "postgres did not start after its install"),
        }
    });
}

/// How long the work after an install waits for another action on
/// PostgreSQL, such as a start the user asked for meanwhile.
const START_WAIT: Duration = Duration::from_secs(30);

/// `[postgres]` of `global.toml`.
pub(super) fn read_settings(data_dir: &Path) -> Result<ServerSettings, ActionError> {
    let path = ConfigPaths::in_dir(data_dir).global;
    let global = config::load_global(&path).map_err(ActionError::Config)?;
    Ok(global.postgres)
}

/// Starts PostgreSQL in the background when `[postgres] enabled = true` and
/// a version of its `default` line is installed (ADR 0023, section 5), the
/// way Caddy starts with the daemon (task M5-16): the start of
/// `marsa services start postgres`, with the check before it. Its turn is
/// taken here, before the API serves, so a request waits for it. A failure
/// only sets the status `Failed` with its code: the daemon runs on.
///
/// A shutdown drops the start at its next `.await`. The spawn and
/// `add_proc` have no `.await` between them, so a server it spawned is in
/// `Daemon::procs`, which the shutdown stops. A dropped init kills its
/// program (`kill_on_drop`) and leaves a temp folder, which the next init
/// deletes.
pub fn start_at_launch(daemon: &Arc<Daemon>) -> Launch {
    let postgres = &daemon.postgres;
    let settings = match read_settings(&daemon.data_dir) {
        Ok(settings) => settings,
        // Without PostgreSQL, a broken `global.toml` is none of its business:
        // its status and lines stay as they are.
        Err(err) if !any_installed(&daemon.data_dir) => {
            info!(
                code = err.code(),
                "no postgres is installed; it does not start with the daemon"
            );
            return Launch::ended(POSTGRES_ID, Launched::NotInstalled);
        }
        Err(err) => {
            postgres.fail(&daemon.events, err.code(), &err.to_string());
            return Launch::ended(POSTGRES_ID, Launched::Failed(err.code()));
        }
    };
    if !settings.enabled {
        info!("postgres is off; it does not start with the daemon");
        return Launch::ended(POSTGRES_ID, Launched::Off);
    }
    // First, so a missing version does not mark the status failed.
    if let Err(err) = postgres.installed(&daemon.data_dir, &settings) {
        warn!(code = err.code(), %err, "postgres is on, but does not start with the daemon");
        return Launch::ended(POSTGRES_ID, Launched::NotInstalled);
    }
    let Ok(busy) = Arc::clone(&postgres.busy).try_lock_owned() else {
        info!("another action on postgres runs; it decides instead of the start with the daemon");
        return Launch::ended(POSTGRES_ID, Launched::Busy);
    };
    info!("starting postgres with the daemon");
    let daemon = Arc::clone(daemon);
    let task = tokio::spawn(async move {
        tokio::select! {
            // A daemon that already stops starts nothing.
            biased;
            () = daemon.shutdown.cancelled() => Launched::ShuttingDown,
            launched = launch(&daemon, busy) => launched,
        }
    });
    Launch::running(POSTGRES_ID, task)
}

/// Whether any PostgreSQL version is installed. A folder that cannot be read
/// counts as one: then the error is reported.
pub(super) fn any_installed(data_dir: &Path) -> bool {
    let installed = RuntimesDir::in_dir(data_dir).installed(Runtime::Postgres);
    installed.map_or(true, |versions| !versions.is_empty())
}

async fn launch(daemon: &Daemon, busy: tokio::sync::OwnedMutexGuard<()>) -> Launched {
    let started = daemon
        .postgres
        .act(daemon, ServiceAction::Start, &busy)
        .await;
    drop(busy);
    match started.map(|_| daemon.postgres.status()) {
        Ok(status) => {
            match &status.state {
                ServiceState::Running => info!("postgres started with the daemon"),
                ServiceState::Failed { code } => {
                    warn!(%code, "postgres did not start with the daemon");
                }
                state => warn!(?state, "postgres is not running after its start"),
            }
            Launched::Ran(status)
        }
        Err(err) => {
            warn!(code = err.code(), %err, "postgres did not start with the daemon");
            Launched::Failed(err.code())
        }
    }
}
