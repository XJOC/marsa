//! A server's table in `global.toml` (`[postgres]`), the work after an
//! install, and the start with the daemon.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use marsa_core::config::{self, ConfigError, ConfigPaths, ServerSettings};
use marsa_core::databases::ServerKind;
use marsa_core::runtimes::RuntimesDir;
use marsa_core::types::{ServiceAction, ServiceState};
use semver::Version;
use tracing::{info, warn};

use super::manager::{ActionError, ServerManager};
use crate::daemon::caddy::{Launch, Launched};
use crate::daemon::state::Daemon;

/// [`set_enabled`] on a blocking thread, under `Daemon::lock_global`.
pub(super) async fn save_enabled(
    kind: ServerKind,
    daemon: &Arc<Daemon>,
    enabled: bool,
) -> Result<(), ActionError> {
    let worker = Arc::clone(daemon);
    let job = tokio::task::spawn_blocking(move || {
        let _one = worker.lock_global();
        set_enabled(kind, &worker.data_dir, enabled)
    });
    let server = kind.id();
    match job.await {
        Ok(saved) => saved.map_err(|err| {
            warn!(code = err.code(), %err, server, enabled, "cannot save enabled");
            ActionError::Config(err)
        }),
        Err(err) => {
            warn!(%err, server, "the job that saves enabled failed");
            Err(ActionError::Internal)
        }
    }
}

/// Turns the server on or off in `global.toml` (`[postgres] enabled`), for
/// the start with the daemon. Writes only a change. The caller holds
/// `Daemon::lock_global`.
///
/// # Errors
///
/// The `config_*` codes when `global.toml` cannot be read or written.
fn set_enabled(kind: ServerKind, data_dir: &Path, enabled: bool) -> Result<(), ConfigError> {
    let path = ConfigPaths::in_dir(data_dir).global;
    let mut global = config::load_global(&path)?;
    let settings = kind.settings_mut(&mut global);
    if settings.enabled == enabled {
        return Ok(());
    }
    settings.enabled = enabled;
    config::save_global(&path, &global)
}

/// What an install of `version` changes in the server's table, now that
/// `installed` holds it, and whether the server then starts:
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
/// - any other `default`, or one that names no line: nothing changes.
///
/// A `version` that is no longer in `installed` (an uninstall came first)
/// changes nothing.
pub(super) fn after_install(
    kind: ServerKind,
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
                settings.default = Some(kind.line(version));
                settings.enabled = true;
                true
            }
            Some(ran) => {
                settings.default = Some(kind.line(ran));
                false
            }
        },
        Some(line) => {
            let gone = installed
                .iter()
                .all(|other| kind.in_line(line, other) == Some(false));
            if kind.in_line(line, version).is_some() && gone {
                settings.default = Some(kind.line(version));
                settings.enabled
            } else {
                false
            }
        }
    }
}

/// [`after_install`] on `global.toml`, written only when it changes. Returns
/// whether the server starts. The caller holds `Daemon::lock_global`.
///
/// # Errors
///
/// The `config_*` codes when `global.toml` cannot be read or written.
pub(super) fn first_install(
    kind: ServerKind,
    data_dir: &Path,
    version: &Version,
    installed: &[Version],
) -> Result<bool, ConfigError> {
    let path = ConfigPaths::in_dir(data_dir).global;
    let mut global = config::load_global(&path)?;
    let settings = kind.settings_mut(&mut global);
    let before = settings.clone();
    let start = after_install(kind, settings, version, installed);
    if *settings != before {
        config::save_global(&path, &global)?;
    }
    Ok(start)
}

/// After an install of `version` of `kind`, in the background: takes the
/// server's turn, writes its table ([`first_install`]), and starts it in
/// that same turn when it says so. A stop asked meanwhile waits for the turn
/// and drops the start ([`ServerManager::run_waiting`]); a daemon that stops
/// starts nothing. A failure is only logged, or sets the status: the install
/// has succeeded either way.
pub fn start_after_install(daemon: &Arc<Daemon>, kind: ServerKind, version: Version) {
    let daemon = Arc::clone(daemon);
    tokio::spawn(async move {
        let manager = daemon.server(kind);
        let server = kind.id();
        let turn = tokio::time::timeout(START_WAIT, Arc::clone(&manager.busy).lock_owned());
        let turn = tokio::select! {
            biased;
            () = daemon.shutdown.cancelled() => return,
            turn = turn => turn,
        };
        let Ok(turn) = turn else {
            warn!(%version, server, "another action ran too long; the settings are as they were");
            return;
        };
        let worker = Arc::clone(&daemon);
        let installed = version.clone();
        let job = tokio::task::spawn_blocking(move || {
            let _one = worker.lock_global();
            // A list that cannot be read changes nothing: it could hide
            // another line.
            let versions = RuntimesDir::in_dir(&worker.data_dir)
                .installed(kind.runtime())
                .map_err(|err| {
                    warn!(code = err.code(), %err, server, "cannot list the versions");
                })
                .ok()?;
            Some(first_install(kind, &worker.data_dir, &installed, &versions))
        });
        match job.await {
            Ok(Some(Ok(true))) => info!(%version, server, "the first install: on, and starting it"),
            Ok(None | Some(Ok(false))) => return,
            Ok(Some(Err(err))) => {
                warn!(code = err.code(), %err, server, "cannot write the settings after the install");
                return;
            }
            Err(err) => {
                warn!(%err, server, "cannot write the settings after the install");
                return;
            }
        }
        let start = manager.act(&daemon, ServiceAction::Start, &turn);
        let started = tokio::select! {
            biased;
            () = daemon.shutdown.cancelled() => return,
            started = start => started,
        };
        drop(turn);
        match started {
            Ok(_) => info!(state = ?manager.status().state, server, "after its first install"),
            Err(err) => warn!(code = err.code(), %err, server, "no start after its install"),
        }
    });
}

/// How long the work after an install waits for another action on the
/// server, such as a start the user asked for meanwhile.
const START_WAIT: Duration = Duration::from_secs(30);

/// The server's table in `global.toml`.
pub(super) fn read_settings(
    kind: ServerKind,
    data_dir: &Path,
) -> Result<ServerSettings, ActionError> {
    let path = ConfigPaths::in_dir(data_dir).global;
    let global = config::load_global(&path).map_err(ActionError::Config)?;
    Ok(kind.settings(&global).clone())
}

/// Starts the server in the background when its table says `enabled = true`
/// and a version of its `default` line is installed (ADR 0023, section 5),
/// the way Caddy starts with the daemon: the start of
/// `marsa services start <id>`, with the check before it. Its turn is taken
/// here, before the API serves, so a request waits for it. A failure only
/// sets the status `Failed` with its code: the daemon runs on.
///
/// A shutdown drops the start at its next `.await`. The spawn and
/// `add_proc` have no `.await` between them, so a server it spawned is in
/// `Daemon::procs`, which the shutdown stops. A dropped init kills its
/// program (`kill_on_drop`) and leaves a temp folder, which the next init
/// deletes.
pub fn start_at_launch(daemon: &Arc<Daemon>, kind: ServerKind) -> Launch {
    let manager = daemon.server(kind);
    let server = kind.id();
    let settings = match read_settings(kind, &daemon.data_dir) {
        Ok(settings) => settings,
        // Without the server, a broken `global.toml` is none of its
        // business: its status and lines stay as they are.
        Err(err) if !any_installed(kind, &daemon.data_dir) => {
            info!(
                code = err.code(),
                server, "it is not installed; it does not start with the daemon"
            );
            return Launch::ended(server, Launched::NotInstalled);
        }
        Err(err) => {
            manager.fail(&daemon.events, err.code(), &err.to_string());
            return Launch::ended(server, Launched::Failed(err.code()));
        }
    };
    if !settings.enabled {
        info!(server, "it is off; it does not start with the daemon");
        return Launch::ended(server, Launched::Off);
    }
    // First, so a missing version does not mark the status failed.
    if let Err(err) = manager.installed(&daemon.data_dir, &settings) {
        warn!(code = err.code(), %err, server, "it is on, but does not start with the daemon");
        return Launch::ended(server, Launched::NotInstalled);
    }
    let Ok(busy) = Arc::clone(&manager.busy).try_lock_owned() else {
        info!(
            server,
            "another action runs; it decides instead of the start with the daemon"
        );
        return Launch::ended(server, Launched::Busy);
    };
    info!(server, "starting it with the daemon");
    let daemon = Arc::clone(daemon);
    let task = tokio::spawn(async move {
        tokio::select! {
            // A daemon that already stops starts nothing.
            biased;
            () = daemon.shutdown.cancelled() => Launched::ShuttingDown,
            launched = launch(&daemon, daemon.server(kind), busy) => launched,
        }
    });
    Launch::running(server, task)
}

/// Whether any version of `kind` is installed. A folder that cannot be read
/// counts as one: then the error is reported.
pub(super) fn any_installed(kind: ServerKind, data_dir: &Path) -> bool {
    let installed = RuntimesDir::in_dir(data_dir).installed(kind.runtime());
    installed.map_or(true, |versions| !versions.is_empty())
}

async fn launch(
    daemon: &Daemon,
    manager: &ServerManager,
    busy: tokio::sync::OwnedMutexGuard<()>,
) -> Launched {
    let started = manager.act(daemon, ServiceAction::Start, &busy).await;
    drop(busy);
    let server = manager.kind().id();
    match started.map(|_| manager.status()) {
        Ok(status) => {
            match &status.state {
                ServiceState::Running => info!(server, "started with the daemon"),
                ServiceState::Failed { code } => {
                    warn!(%code, server, "it did not start with the daemon");
                }
                state => warn!(?state, server, "it is not running after its start"),
            }
            Launched::Ran(status)
        }
        Err(err) => {
            warn!(code = err.code(), %err, server, "it did not start with the daemon");
            Launched::Failed(err.code())
        }
    }
}
