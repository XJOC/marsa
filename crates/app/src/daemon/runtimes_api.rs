//! The runtimes routes. Task M3-12.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/runtimes` | `RuntimeList`: the installed versions and the default of each runtime |
//! | `POST /v1/runtimes/{tool}/install` | 202 `InstallAccepted`, then `install` events on `/v1/events` |
//! | `PUT /v1/runtimes/{tool}/default` | `DefaultVersion`: the new default of PHP or Node (task M5-01) |
//! | `DELETE /v1/runtimes/{tool}/{version}` | `Uninstalled`: the version whose folder is gone (task M5-07) |
//!
//! The daemon runs the installer (`marsa_core::installer::install`) in a task
//! of its own and sends each `InstallEvent` on the event bus. The last event
//! of an install is `done` or `failed { code }`. In M2 `marsa install` called
//! the installer itself; now only the daemon writes to `runtimes/`.
//!
//! One install at a time: a second request while one runs gets 409
//! `install_busy`. The installer's own file lock (`runtimes/.tmp/install.lock`)
//! still guards against another process; that case ends as `failed` with the
//! same code.
//!
//! `PUT .../default` takes `{"version": "8.3"}`. The text must match an
//! installed version, by the rules of a `.php-version` file; then it goes
//! into `global.toml` as written, `default_changed` goes to `/v1/events`, and
//! the site list is built again. A site that follows the default gets the
//! new PHP, and `sites_changed` makes Caddy and php-fpm follow. A refused
//! request leaves `global.toml` as it was.
//!
//! `DELETE .../{version}` takes `8.3.12`, or `8.3` when one installed version
//! matches. It takes the install's guard, so it never runs with an install,
//! and refuses (409 `version_in_use`, nothing changed) a version that the
//! default in `global.toml` or a site's own setting in `sites.toml` picks
//! now, or that the running Caddy runs from. Then the folder is moved aside
//! (`installer::uninstall`), each php-fpm pool that runs the version is
//! stopped, the folder is deleted, and the site list is built again.
//!
//! The default of the only installed PHP or Node version has no other
//! version to pick, so it does not keep the version: it is cleared from
//! `global.toml` with the uninstall (task M5-19). The PHP sites that follow
//! that default would have no PHP, so they still keep it: 409
//! `sites_follow_default`, with their names.

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use marsa_core::config::{
    self, ConfigError, ConfigPaths, GlobalConfig, SiteSettings, SitesConfig, ToolDefaults,
};
use marsa_core::installer::extract::ExtractError;
use marsa_core::installer::fetch::FetchError;
use marsa_core::installer::uninstall::{self, Retired, UninstallError};
use marsa_core::installer::{self, InstallEnv, InstallError, InstalledVersion};
use marsa_core::resolver::{self, BadRequest, Source, Tool};
use marsa_core::runtimes::{Runtime, RuntimesDir, RuntimesError};
use marsa_core::types::{
    DefaultRequest, DefaultVersion, Event, InstallAccepted, InstallDone, InstallEvent,
    InstallFailure, InstallProgress, InstallRequest, InstallUpdate, RuntimeList, RuntimeVersions,
    SiteKind, Uninstalled, VersionInUse,
};
use semver::Version;

use super::api::HttpError;
use super::php::{fpm_id, wanted_pools};
use super::sites_api;
use super::state::Daemon;

/// The routes, for `api::router` to merge before its fallbacks.
pub fn routes() -> Router<Arc<Daemon>> {
    Router::new()
        .route("/v1/runtimes", get(runtimes))
        .route("/v1/runtimes/{tool}/install", post(install))
        .route("/v1/runtimes/{tool}/default", put(set_default))
        .route("/v1/runtimes/{tool}/{version}", delete(uninstall))
}

/// What [`RunInstall::run`] returns.
pub type InstallFuture<'a> =
    Pin<Box<dyn Future<Output = Result<InstalledVersion, InstallError>> + Send + 'a>>;

/// Runs one install. The daemon uses [`ProductionInstall`]. Tests put a fake
/// in `Daemon::installs.runner`, as `tests/services.rs` puts a fake Caddy in
/// `Daemon::caddy.spec`: the real installer needs the network and the
/// manifest key, and neither can be swapped (task M2-05).
pub trait RunInstall: Send + Sync {
    fn run<'a>(
        &'a self,
        data_dir: &'a Path,
        request: installer::InstallRequest<'a>,
        events: &'a mut (dyn FnMut(InstallEvent) + Send),
    ) -> InstallFuture<'a>;
}

/// `marsa_core::installer::install` with [`InstallEnv::production`]: the
/// published manifest, the key built into the binary, and every check.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProductionInstall;

impl RunInstall for ProductionInstall {
    fn run<'a>(
        &'a self,
        data_dir: &'a Path,
        request: installer::InstallRequest<'a>,
        events: &'a mut (dyn FnMut(InstallEvent) + Send),
    ) -> InstallFuture<'a> {
        Box::pin(async move {
            let env = InstallEnv::production(data_dir)?;
            installer::install(&request, &env, events).await
        })
    }
}

/// The install side of the daemon: who runs it, and whether one runs now.
pub struct Installs {
    /// Runs each install: [`ProductionInstall`], unless a test put a fake
    /// here before the daemon went into its `Arc`.
    pub runner: Arc<dyn RunInstall>,
    running: AtomicBool,
    last_id: AtomicU64,
}

impl Installs {
    /// With [`ProductionInstall`].
    pub fn new() -> Self {
        Self {
            runner: Arc::new(ProductionInstall),
            running: AtomicBool::new(false),
            last_id: AtomicU64::new(0),
        }
    }

    /// Whether an install, or an uninstall (task M5-07), runs now.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
}

impl Default for Installs {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Installs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Installs")
            .field("running", &self.is_running())
            .field("last_id", &self.last_id.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// The right to run the one install. Dropping it lets the next one start,
/// also if the install task panics.
struct Started {
    daemon: Arc<Daemon>,
    id: u64,
}

impl Started {
    /// `None` while another install or an uninstall runs.
    fn take(daemon: &Arc<Daemon>) -> Option<Self> {
        let mut started = Self::claim(daemon)?;
        started.id = daemon.installs.last_id.fetch_add(1, Ordering::Relaxed) + 1;
        Some(started)
    }

    /// The same right for an uninstall, which has no id. No lock: one atomic
    /// swap.
    fn claim(daemon: &Arc<Daemon>) -> Option<Self> {
        let running = &daemon.installs.running;
        running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(Self {
            daemon: Arc::clone(daemon),
            id: 0,
        })
    }
}

impl Drop for Started {
    fn drop(&mut self) {
        self.daemon.installs.running.store(false, Ordering::Release);
    }
}

async fn runtimes(State(daemon): State<Arc<Daemon>>) -> Result<Json<RuntimeList>, HttpError> {
    // Four small folder reads and one small file; not worth a blocking thread.
    list(&daemon.data_dir).map(Json).map_err(|err| {
        tracing::warn!(code = err.code(), error = %err, "cannot list runtimes");
        HttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            err.code(),
            err.to_string(),
        )
    })
}

/// The installed versions of every runtime, from `runtimes/`, and the
/// defaults of PHP and Node, from `global.toml`. A `global.toml` that cannot
/// be read gives no default, as in the site list.
///
/// # Errors
///
/// `runtimes_read` if a runtime folder exists but cannot be read.
pub fn list(data_dir: &Path) -> Result<RuntimeList, RuntimesError> {
    let dir = RuntimesDir::in_dir(data_dir);
    let global = config::load_global(&ConfigPaths::in_dir(data_dir).global).unwrap_or_else(|err| {
        tracing::warn!(code = err.code(), error = %err, "global.toml");
        GlobalConfig::default()
    });
    let runtimes = Runtime::ALL
        .into_iter()
        .map(|runtime| {
            let installed = dir.installed(runtime)?;
            let default = match runtime {
                Runtime::Php => global.php.default.clone(),
                Runtime::Node => global.node.default.clone(),
                Runtime::Composer | Runtime::Caddy => None,
            };
            Ok(RuntimeVersions {
                runtime,
                installed: installed.iter().map(ToString::to_string).collect(),
                default,
            })
        })
        .collect::<Result<_, RuntimesError>>()?;
    Ok(RuntimeList { runtimes })
}

async fn install(
    State(daemon): State<Arc<Daemon>>,
    path: Result<UrlPath<String>, PathRejection>,
    body: Result<Json<InstallRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<InstallAccepted>), HttpError> {
    let UrlPath(tool) =
        path.map_err(|err| HttpError::new(err.status(), "invalid_path", err.body_text()))?;
    let runtime = runtime(&tool)?;
    let Json(request) =
        body.map_err(|err| HttpError::new(err.status(), "invalid_body", err.body_text()))?;
    let Some(started) = Started::take(&daemon) else {
        return Err(busy());
    };
    let id = started.id;
    tracing::info!(
        id,
        runtime = runtime.dir_name(),
        version = request.version.as_deref().unwrap_or("newest"),
        force = request.force,
        "install started"
    );
    let task = tokio::spawn(run(started, runtime, request));
    // A panic in the install task sends no final event, and `marsa install`
    // waits for one. This watcher sends `failed` instead.
    tokio::spawn(async move {
        if let Err(err) = task.await
            && err.is_panic()
        {
            tracing::error!(id, "the install task panicked");
            daemon.publish(Event::Install(InstallProgress {
                id,
                update: InstallUpdate::Failed(InstallFailure {
                    code: "install_crashed".to_owned(),
                    message: "the install task panicked".to_owned(),
                    ..InstallFailure::default()
                }),
            }));
        }
    });
    Ok((StatusCode::ACCEPTED, Json(InstallAccepted { id })))
}

fn busy() -> HttpError {
    let err = InstallError::Busy;
    HttpError::new(StatusCode::CONFLICT, err.code(), err.to_string())
}

/// The `{tool}` path segment.
fn runtime(tool: &str) -> Result<Runtime, HttpError> {
    Runtime::ALL
        .into_iter()
        .find(|runtime| runtime.dir_name() == tool)
        .ok_or_else(|| {
            let message = format!("no tool named {tool:?}; use php, node, composer, or caddy");
            HttpError::new(StatusCode::NOT_FOUND, "unknown_tool", message)
        })
}

/// The install task: runs the installer, sends each step, then `done` or
/// `failed`. Stops when the daemon does; the installer is safe to drop at
/// any `.await`, and its disk steps finish on their own thread.
async fn run(started: Started, runtime: Runtime, body: InstallRequest) {
    let daemon = Arc::clone(&started.daemon);
    let id = started.id;
    let request = installer::InstallRequest {
        runtime,
        version: body.version.as_deref(),
        force: body.force,
    };
    let mut events = {
        let daemon = Arc::clone(&daemon);
        move |event| {
            daemon.publish(Event::Install(InstallProgress {
                id,
                update: InstallUpdate::Running(event),
            }));
        }
    };
    let result = tokio::select! {
        result = daemon.installs.runner.run(&daemon.data_dir, request, &mut events) => result,
        () = daemon.shutdown.cancelled() => {
            tracing::info!(id, "install stopped: the daemon is stopping");
            return;
        }
    };
    // Free before the last event, so a client that reads it can start the
    // next install at once.
    drop(started);
    let update = match result {
        Ok(done) => {
            tracing::info!(id, version = %done.version, already = done.already, "install done");
            // The first PHP or Node becomes the default, so a site or a
            // folder with no version file works at once (task M4-22).
            let worker = Arc::clone(&daemon);
            let version = done.version.clone();
            let job = tokio::task::spawn_blocking(move || {
                let _one = worker.lock_global();
                first_default(&worker.data_dir, runtime, &version)
            });
            match job.await {
                Ok(Ok(Some(line))) => {
                    tracing::info!(id, runtime = ?runtime, default = %line, "set the default version");
                    daemon.publish(Event::DefaultChanged(DefaultVersion {
                        runtime,
                        default: line,
                        version: done.version.to_string(),
                    }));
                }
                Ok(Ok(None)) => {}
                Ok(Err(err)) => {
                    tracing::warn!(id, code = err.code(), error = %err, "cannot set the default version")
                }
                Err(err) => tracing::warn!(id, error = %err, "cannot set the default version"),
            }
            // Before `done`, so a client that reads it lists the sites with
            // their new PHP (task M4-10d). `rebuild_sites` logs its errors.
            if runtime == Runtime::Php {
                let _ = daemon.rebuild_sites().await;
            }
            InstallUpdate::Done(InstallDone {
                runtime: done.runtime,
                version: done.version.to_string(),
                dir: done.dir,
                already: done.already,
            })
        }
        Err(err) => {
            let failure = failure(&err, request.version);
            // `?`: the message may hold a program's output, line breaks too.
            tracing::warn!(id, code = %failure.code, error = ?failure.message, "install failed");
            InstallUpdate::Failed(failure)
        }
    };
    daemon.publish(Event::Install(InstallProgress { id, update }));
}

/// The `failed` event for `err`: its code, its text with every cause, and
/// the facts a message may show. `requested` is the version text asked for.
pub fn failure(err: &InstallError, requested: Option<&str>) -> InstallFailure {
    let mut failure = InstallFailure {
        code: err.code().to_owned(),
        message: chain(err),
        ..InstallFailure::default()
    };
    match err {
        InstallError::UnsupportedPlatform { platform, .. } => {
            failure.platform = Some(platform.clone());
        }
        InstallError::NoSuchVersion {
            requested,
            available,
            ..
        } => {
            failure.requested.clone_from(requested);
            failure.available.clone_from(available);
        }
        InstallError::CheckFailed { output, .. } => failure.output = Some(output.clone()),
        InstallError::SerialCorrupt { path } | InstallError::Io { path, .. } => {
            failure.path = Some(path.clone());
        }
        InstallError::Fetch(fetch) => {
            failure.url = url_of(fetch).map(str::to_owned);
            failure.cause = fetch.last_code().map(str::to_owned);
            if let FetchError::Io { path, .. } = fetch {
                failure.path = Some(path.clone());
            }
        }
        InstallError::Extract(ExtractError::Io(path, _))
        | InstallError::Runtimes(RuntimesError::Read { path, .. }) => {
            failure.path = Some(path.clone());
        }
        InstallError::Version(_) => failure.requested = requested.map(str::to_owned),
        _ => {}
    }
    failure
}

/// The URL a download error is about. For `download_failed`, the one of the
/// last mirror tried.
fn url_of(err: &FetchError) -> Option<&str> {
    match err {
        FetchError::InsecureUrl { url }
        | FetchError::HttpStatus { url, .. }
        | FetchError::Network { url, .. }
        | FetchError::TooLong { url, .. }
        | FetchError::TooShort { url, .. }
        | FetchError::ChecksumMismatch { url, .. }
        | FetchError::TooLarge { url, .. } => Some(url),
        FetchError::Failed {
            last: Some(last), ..
        } => url_of(last),
        FetchError::Failed { last: None, .. } | FetchError::Io { .. } => None,
    }
}

/// The message of `err` and of each cause under it, for the log.
fn chain(err: &dyn Error) -> String {
    let mut text = err.to_string();
    let mut cause = err.source();
    while let Some(current) = cause {
        text.push_str(": ");
        text.push_str(&current.to_string());
        cause = current.source();
    }
    text
}

/// Makes `version` the default of `runtime` in `global.toml` when it has
/// none: `major.minor` for PHP (`8.4`), the major for Node (`22`), so later
/// updates of that line follow. Returns the new
/// default, or `None` when one was already set or the runtime has no default
/// (Composer). The caller holds [`Daemon::lock_global`].
///
/// # Errors
///
/// The `config_*` codes when `global.toml` cannot be read or written.
fn first_default(
    data_dir: &Path,
    runtime: Runtime,
    version: &semver::Version,
) -> Result<Option<String>, ConfigError> {
    let Some(tool) = default_tool(runtime) else {
        return Ok(None);
    };
    // PHP lines differ by minor (8.3, 8.4); Node lines by major (22), as
    // in `.nvmrc`.
    let line = match tool {
        Tool::Php => format!("{}.{}", version.major, version.minor),
        Tool::Node => version.major.to_string(),
    };
    let path = ConfigPaths::in_dir(data_dir).global;
    let mut global = config::load_global(&path)?;
    let defaults = defaults_mut(&mut global, tool);
    if defaults.default.is_some() {
        return Ok(None);
    }
    defaults.default = Some(line.clone());
    config::save_global(&path, &global)?;
    Ok(Some(line))
}

async fn set_default(
    State(daemon): State<Arc<Daemon>>,
    path: Result<UrlPath<String>, PathRejection>,
    body: Result<Json<DefaultRequest>, JsonRejection>,
) -> Result<Json<DefaultVersion>, HttpError> {
    let UrlPath(tool) =
        path.map_err(|err| HttpError::new(err.status(), "invalid_path", err.body_text()))?;
    let runtime = runtime(&tool)?;
    let tool = default_tool(runtime).ok_or_else(|| no_default_version(runtime))?;
    let Json(DefaultRequest { version }) =
        body.map_err(|err| HttpError::new(err.status(), "invalid_body", err.body_text()))?;
    // The disk on a blocking thread; the lock is dropped there, before any
    // `.await`.
    let worker = Arc::clone(&daemon);
    let job = tokio::task::spawn_blocking(move || {
        let _one = worker.lock_global();
        save_default(&worker.data_dir, tool, version.trim())
    });
    let answer = job.await.unwrap_or_else(|err| {
        tracing::error!(error = %err, "the default version job failed");
        let message = "the default version job failed";
        Err(HttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            message,
        ))
    })?;
    tracing::info!(
        runtime = runtime.dir_name(),
        default = %answer.default,
        version = %answer.version,
        "set the default version"
    );
    // For the window's runtimes list, also when the CLI made the change.
    daemon.publish(Event::DefaultChanged(answer.clone()));
    // The sites that follow the default now get the new PHP, and
    // `sites_changed` makes Caddy and php-fpm follow (task M4-11). Before
    // the answer, so a client that reads it then lists the new versions.
    // Node is in no site yet; then the list stays the same and nothing is
    // sent. `rebuild_sites` logs its errors; the default is saved either way.
    let _ = daemon.rebuild_sites().await;
    Ok(Json(answer))
}

/// Makes `text` the default of `tool` in `global.toml` if it matches an
/// installed version, and returns it with the version it picks now. Writes
/// nothing when it is the default already, and nothing on any error. The
/// caller holds [`Daemon::lock_global`].
///
/// # Errors
///
/// `invalid_version` or `unsupported_alias` (400) for text that is not a
/// version, `version_not_installed` (409) when no installed version matches,
/// `runtimes_read` (500) when the runtime's folder cannot be read, and a
/// `config_*` code (500) when `global.toml` cannot be read or written.
fn save_default(data_dir: &Path, tool: Tool, text: &str) -> Result<DefaultVersion, HttpError> {
    let runtime = Runtime::from(tool);
    // First alone, so text that is not a version is refused whatever is
    // installed, as `PATCH /v1/sites/{name}` refuses it.
    resolver::best_match(text, &[]).map_err(|err| bad_version(text, err))?;
    let installed = RuntimesDir::in_dir(data_dir)
        .installed(runtime)
        .map_err(|err| {
            tracing::warn!(code = err.code(), error = %err, "cannot list the installed versions");
            HttpError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                err.code(),
                err.to_string(),
            )
        })?;
    let version = match resolver::best_match(text, &installed) {
        Ok(Some(version)) => version,
        Ok(None) => {
            let message = format!(
                "no installed {} version matches {text:?}",
                runtime.dir_name()
            );
            return Err(HttpError::new(
                StatusCode::CONFLICT,
                "version_not_installed",
                message,
            ));
        }
        Err(err) => return Err(bad_version(text, err)),
    };
    let path = ConfigPaths::in_dir(data_dir).global;
    let mut global = config::load_global(&path).map_err(|err| global_error(&err))?;
    let defaults = defaults_mut(&mut global, tool);
    if defaults.default.as_deref() != Some(text) {
        defaults.default = Some(text.to_owned());
        config::save_global(&path, &global).map_err(|err| global_error(&err))?;
    }
    Ok(DefaultVersion {
        runtime,
        default: text.to_owned(),
        version: version.to_string(),
    })
}

async fn uninstall(
    State(daemon): State<Arc<Daemon>>,
    path: Result<UrlPath<(String, String)>, PathRejection>,
) -> Result<Json<Uninstalled>, HttpError> {
    let UrlPath((tool, text)) =
        path.map_err(|err| HttpError::new(err.status(), "invalid_path", err.body_text()))?;
    let runtime = runtime(&tool)?;
    let started = Started::claim(&daemon).ok_or_else(busy)?;
    // A task of its own, so a client that hangs up cannot stop it between
    // the rename and the delete.
    let task = tokio::spawn(remove_version(started, runtime, text));
    task.await.unwrap_or_else(|err| {
        tracing::error!(error = %err, "the uninstall task failed");
        let message = "the uninstall task failed";
        Err(HttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            message,
        ))
    })
}

/// The uninstall, from the checks to the new site list. `started` keeps
/// installs out until the end.
async fn remove_version(
    started: Started,
    runtime: Runtime,
    text: String,
) -> Result<Json<Uninstalled>, HttpError> {
    let daemon = Arc::clone(&started.daemon);
    let caddy = daemon.caddy.running_program(&daemon);
    let worker = Arc::clone(&daemon);
    let job = tokio::task::spawn_blocking(move || {
        // `marsa use` waits, so the version cannot become the default
        // between the check and the rename.
        let _one = worker.lock_global();
        retire_unused(&worker.data_dir, runtime, text.trim(), caddy.as_deref())
    });
    let (version, retired, cleared) = job.await.map_err(|err| {
        tracing::error!(error = %err, "the uninstall job failed");
        HttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "the uninstall job failed",
        )
    })??;
    if let Some(default) = cleared {
        let tool = runtime.dir_name();
        tracing::info!(runtime = tool, %default, "cleared the default of the last version");
    }
    // The version is hidden now, so no pool can start with it again.
    if runtime == Runtime::Php {
        stop_pools(&daemon, &version).await;
    }
    let path = retired.path().to_path_buf();
    match tokio::task::spawn_blocking(move || retired.remove()).await {
        Ok(Ok(())) => {}
        // The version is gone either way; `marsad` deletes the rest at its
        // next start.
        Ok(Err(err)) => tracing::warn!(code = err.code(), error = %err, "the delete stopped"),
        Err(err) => tracing::warn!(path = %path.display(), error = %err, "the delete stopped"),
    }
    tracing::info!(runtime = runtime.dir_name(), %version, "uninstalled");
    // Sites that ran this version now resolve to another one, or to none,
    // and `sites_changed` makes Caddy and php-fpm follow. `rebuild_sites`
    // logs its errors.
    let _ = daemon.rebuild_sites().await;
    drop(started);
    Ok(Json(Uninstalled {
        runtime,
        version: version.to_string(),
    }))
}

/// Picks the installed version `text` names, checks that nothing uses it,
/// and moves its folder aside. Also returns the default it cleared, if any:
/// the default of the only installed version goes with it (task M5-19). A
/// refusal changes nothing on disk. `caddy` is the program the running Caddy
/// runs from. The caller holds [`Daemon::lock_global`].
fn retire_unused(
    data_dir: &Path,
    runtime: Runtime,
    text: &str,
    caddy: Option<&Path>,
) -> Result<(Version, Retired, Option<String>), HttpError> {
    resolver::best_match(text, &[]).map_err(|err| bad_version(text, err))?;
    let runtimes = RuntimesDir::in_dir(data_dir);
    let installed = runtimes.installed(runtime).map_err(|err| {
        let status = StatusCode::INTERNAL_SERVER_ERROR;
        HttpError::new(status, err.code(), err.to_string())
    })?;
    let matching: Vec<&Version> = installed
        .iter()
        .filter(|version| picks(text, std::slice::from_ref(*version), version))
        .collect();
    let version = match matching[..] {
        [one] => one.clone(),
        [] => {
            let message = format!(
                "no installed {} version matches {text:?}",
                runtime.dir_name()
            );
            let status = StatusCode::NOT_FOUND;
            return Err(HttpError::new(status, "version_not_installed", message));
        }
        ref many => {
            let many: Vec<String> = many.iter().map(ToString::to_string).collect();
            let message = format!("{text:?} matches {}; name one", many.join(", "));
            let status = StatusCode::CONFLICT;
            return Err(HttpError::new(status, "version_ambiguous", message));
        }
    };
    let paths = ConfigPaths::in_dir(data_dir);
    let global = config::load_global(&paths.global).map_err(|err| global_error(&err))?;
    let sites = config::load_sites(&paths.sites).map_err(|err| {
        tracing::warn!(code = err.code(), error = %err, "sites.toml");
        HttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            err.code(),
            err.to_string(),
        )
    })?;
    let mut used = in_use(runtime, &version, &installed, &global, &sites);
    let dir = runtimes.version_dir(runtime, &version);
    used.caddy = caddy.is_some_and(|program| program.starts_with(&dir));
    // The default of the only installed version has no other version to
    // pick, so it goes with this one. The PHP sites that follow it would
    // have no PHP, so they keep the version instead.
    let clear = match (default_tool(runtime), installed.len()) {
        (Some(tool), 1) => used.default.take().map(|default| (tool, default)),
        _ => None,
    };
    if clear.is_some() && runtime == Runtime::Php {
        used.default_sites = default_sites(data_dir, &sites, &version);
    }
    if used.any() {
        let message = format!("{} {version} is in use: {used:?}", runtime.dir_name());
        let mut err = HttpError::new(StatusCode::CONFLICT, used.code(), message);
        err.in_use = Some(Box::new(used));
        return Err(err);
    }
    // The default first, then the move. The move fails more often (on
    // Windows: a program from the folder runs), and then the default is
    // written back, so a failed uninstall keeps it. In no order of failures
    // does a default name a version that is gone: at worst the version
    // stays without its default, and the log says so.
    if let Some((tool, _)) = &clear {
        let mut cleared = global.clone();
        defaults_mut(&mut cleared, *tool).default = None;
        config::save_global(&paths.global, &cleared).map_err(|err| global_error(&err))?;
    }
    let retired = match uninstall::retire(&runtimes, runtime, &version) {
        Ok(retired) => retired,
        Err(err) => {
            tracing::warn!(code = err.code(), error = %err, "cannot uninstall");
            let mut answer = uninstall_error(&err);
            if clear.is_some()
                && let Err(put_back) = config::save_global(&paths.global, &global)
            {
                let code = put_back.code();
                tracing::error!(code, error = %put_back, "cannot put the default back");
                answer
                    .message
                    .push_str("; the default it had could not be put back");
            }
            return Err(answer);
        }
    };
    Ok((version, retired, clear.map(|(_, default)| default)))
}

/// The answer for an uninstall that could not move the folder aside.
fn uninstall_error(err: &UninstallError) -> HttpError {
    let status = match err {
        UninstallError::Busy => StatusCode::CONFLICT,
        UninstallError::NotInstalled { .. } => StatusCode::NOT_FOUND,
        UninstallError::Io { .. } => StatusCode::INTERNAL_SERVER_ERROR,
    };
    HttpError::new(status, err.code(), err.to_string())
}

/// The PHP sites that follow the default and run `version` through it now,
/// by name: no setting in `sites.toml`, no version file, no `composer.json`
/// constraint. Only Laravel and PHP sites count, as in `wanted_pools`: a
/// static site or a Node proxy runs no PHP. Builds the site list as the
/// daemon does, so it reads the disk.
fn default_sites(data_dir: &Path, sites: &SitesConfig, version: &Version) -> Vec<String> {
    let list = sites_api::build(data_dir, sites);
    list.sites
        .into_iter()
        .filter(|site| matches!(site.kind, SiteKind::Laravel | SiteKind::Php))
        .filter(|site| site.php_source == Some(Source::GlobalDefault))
        .filter(|site| site.php.as_ref() == Some(version))
        .map(|site| site.name)
        .collect()
}

/// Whether the version text `text` picks `version` among `installed`, as the
/// resolver would.
fn picks(text: &str, installed: &[Version], version: &Version) -> bool {
    resolver::best_match(text, installed).is_ok_and(|picked| picked.as_ref() == Some(version))
}

/// The default and the sites' own settings that pick `version` now. Composer
/// and Caddy have neither.
fn in_use(
    runtime: Runtime,
    version: &Version,
    installed: &[Version],
    global: &GlobalConfig,
    sites: &SitesConfig,
) -> VersionInUse {
    let setting: fn(&SiteSettings) -> Option<&String> = match runtime {
        Runtime::Php => |site| site.php.as_ref(),
        Runtime::Node => |site| site.node.as_ref(),
        Runtime::Composer | Runtime::Caddy => |_| None,
    };
    let default = default_tool(runtime).and_then(|tool| match tool {
        Tool::Php => global.php.default.as_ref(),
        Tool::Node => global.node.default.as_ref(),
    });
    let uses = |text: &&String| picks(text, installed, version);
    VersionInUse {
        version: version.to_string(),
        default: default.filter(uses).cloned(),
        sites: sites
            .sites
            .iter()
            .filter(|(_, site)| setting(site).is_some_and(|text| uses(&text)))
            .map(|(name, _)| name.clone())
            .collect(),
        default_sites: Vec::new(),
        caddy: false,
    }
}

/// Stops, through the supervisor, each php-fpm pool that the sites run with
/// `version`. The sync task starts the line again after `sites_changed`, if
/// a site still needs it.
async fn stop_pools(daemon: &Daemon, version: &Version) {
    let sites = daemon.sites().map(|list| list.sites).unwrap_or_default();
    for (minor, picked) in wanted_pools(&sites) {
        if picked == *version
            && let Some(handle) = daemon.take_proc(&fpm_id(minor))
        {
            tracing::info!(php = %minor, "stopping php-fpm before its uninstall");
            if let Err(err) = handle.stop().await {
                tracing::warn!(php = %minor, %err, "the php-fpm task failed");
            }
        }
    }
}

/// The resolver's tool for `runtime`, or `None` for a runtime with no default
/// version (Composer, Caddy): the newest installed one is used.
fn default_tool(runtime: Runtime) -> Option<Tool> {
    match runtime {
        Runtime::Php => Some(Tool::Php),
        Runtime::Node => Some(Tool::Node),
        Runtime::Composer | Runtime::Caddy => None,
    }
}

/// The `[php]` or `[node]` table of `global.toml`.
fn defaults_mut(global: &mut GlobalConfig, tool: Tool) -> &mut ToolDefaults {
    match tool {
        Tool::Php => &mut global.php,
        Tool::Node => &mut global.node,
    }
}

fn no_default_version(runtime: Runtime) -> HttpError {
    let message = format!(
        "{} has no default version; the newest installed one is used",
        runtime.dir_name()
    );
    HttpError::new(StatusCode::BAD_REQUEST, "no_default_version", message)
}

/// The same code and status as `PATCH /v1/sites/{name}` gives for this text.
fn bad_version(text: &str, err: BadRequest) -> HttpError {
    let message = format!("{text:?}: {err}");
    HttpError::new(StatusCode::BAD_REQUEST, err.code(), message)
}

/// `global.toml` cannot be read or written: the request was fine, the file or
/// the disk is not.
fn global_error(err: &ConfigError) -> HttpError {
    tracing::warn!(code = err.code(), error = %err, "global.toml");
    HttpError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        err.code(),
        err.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_php_and_node_become_the_default() {
        let dir = tempfile::TempDir::new().unwrap();
        let v = |text: &str| semver::Version::parse(text).unwrap();
        let path = ConfigPaths::in_dir(dir.path()).global;

        assert_eq!(
            first_default(dir.path(), Runtime::Php, &v("8.4.27"))
                .unwrap()
                .as_deref(),
            Some("8.4")
        );
        // A default that is set stays: a second install does not move it.
        assert_eq!(
            first_default(dir.path(), Runtime::Php, &v("8.3.20")).unwrap(),
            None
        );
        assert_eq!(
            first_default(dir.path(), Runtime::Node, &v("22.11.0"))
                .unwrap()
                .as_deref(),
            Some("22")
        );
        assert_eq!(
            first_default(dir.path(), Runtime::Composer, &v("2.8.3")).unwrap(),
            None
        );

        let global = config::load_global(&path).unwrap();
        assert_eq!(global.php.default.as_deref(), Some("8.4"));
        assert_eq!(global.node.default.as_deref(), Some("22"));
    }
    use marsa_core::installer::manifest::ManifestError;
    use std::path::PathBuf;

    #[test]
    fn no_such_version_carries_what_was_asked_and_what_exists() {
        let err = InstallError::NoSuchVersion {
            runtime: "php",
            requested: Some("8.9".into()),
            available: vec!["8.3.27".into(), "8.4.23".into()],
        };
        let got = failure(&err, Some("8.9"));
        assert_eq!(got.code, "install_no_such_version");
        assert_eq!(got.requested.as_deref(), Some("8.9"));
        assert_eq!(got.available, ["8.3.27", "8.4.23"]);
        assert_eq!(got.message, err.to_string());
    }

    #[test]
    fn download_failed_carries_the_last_url_and_its_cause() {
        let err = InstallError::Fetch(FetchError::Failed {
            tried: 2,
            last: Some(Box::new(FetchError::HttpStatus {
                url: "https://mirror.example/php.tar.gz".into(),
                status: 503,
            })),
        });
        let got = failure(&err, None);
        assert_eq!(got.code, "download_failed");
        assert_eq!(
            got.url.as_deref(),
            Some("https://mirror.example/php.tar.gz")
        );
        assert_eq!(got.cause.as_deref(), Some("download_http_status"));
    }

    #[test]
    fn the_other_facts_come_from_the_error() {
        let serial = InstallError::SerialCorrupt {
            path: PathBuf::from("/d/runtimes/.manifest-serial"),
        };
        assert_eq!(
            failure(&serial, None).path,
            Some(PathBuf::from("/d/runtimes/.manifest-serial"))
        );
        // The resolver's errors do not hold the text; the request does.
        let alias = failure(&InstallError::Version(BadRequest::Alias), Some("latest"));
        assert_eq!(
            (alias.code.as_str(), alias.requested.as_deref()),
            ("unsupported_alias", Some("latest"))
        );
        let check = InstallError::CheckFailed {
            output: "PHP 8.4.22 (cli)".into(),
            exit_code: Some(0),
        };
        assert_eq!(
            failure(&check, None).output.as_deref(),
            Some("PHP 8.4.22 (cli)")
        );
        let key = failure(&InstallError::Manifest(ManifestError::NoTrustedKey), None);
        assert_eq!(key.code, "manifest_no_trusted_key");
        assert_eq!(
            key,
            InstallFailure {
                code: key.code.clone(),
                message: key.message.clone(),
                ..InstallFailure::default()
            }
        );
    }

    #[test]
    fn the_message_holds_every_cause() {
        let err = InstallError::Io {
            path: PathBuf::from("/d"),
            source: std::io::Error::other("disk full"),
        };
        assert_eq!(failure(&err, None).message, "disk error at /d: disk full");
    }

    #[test]
    fn only_the_four_tools_are_known() {
        for (tool, runtime) in [
            ("php", Runtime::Php),
            ("node", Runtime::Node),
            ("composer", Runtime::Composer),
            ("caddy", Runtime::Caddy),
        ] {
            assert_eq!(super::runtime(tool).unwrap(), runtime);
        }
        for bad in ["ruby", "PHP", "", "php "] {
            let err = super::runtime(bad).unwrap_err();
            assert_eq!(
                (err.status, err.code),
                (StatusCode::NOT_FOUND, "unknown_tool")
            );
        }
    }

    #[test]
    fn list_reads_every_runtime_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        for dir in [
            "php/8.4.23",
            "php/8.3.27",
            "php/8.4",
            "node/22.11.0",
            "caddy/2.11.7",
            ".tmp",
        ] {
            std::fs::create_dir_all(tmp.path().join("runtimes").join(dir)).unwrap();
        }
        let shown = |got: RuntimeList| -> Vec<_> {
            let row = |r: RuntimeVersions| (r.runtime, r.installed.join(","), r.default);
            got.runtimes.into_iter().map(row).collect()
        };
        let want = [
            (Runtime::Php, "8.3.27,8.4.23".to_owned(), None),
            (Runtime::Node, "22.11.0".to_owned(), None),
            (Runtime::Composer, String::new(), None),
            (Runtime::Caddy, "2.11.7".to_owned(), None),
        ];
        assert_eq!(shown(list(tmp.path()).unwrap()), want);

        // The defaults as written in `global.toml`, for PHP and Node only.
        let mut global = GlobalConfig::default();
        global.php.default = Some("8.4".into());
        global.node.default = Some("22".into());
        config::save_global(&ConfigPaths::in_dir(tmp.path()).global, &global).unwrap();
        let [php, node, composer, caddy] = want;
        let want = [
            (php.0, php.1, Some("8.4".to_owned())),
            (node.0, node.1, Some("22".to_owned())),
            composer,
            caddy,
        ];
        assert_eq!(shown(list(tmp.path()).unwrap()), want);

        // A broken `global.toml` gives no default; the versions still show.
        std::fs::write(ConfigPaths::in_dir(tmp.path()).global, "php = 1\n").unwrap();
        let got = list(tmp.path()).unwrap();
        assert!(got.runtimes.iter().all(|r| r.default.is_none()), "{got:?}");
    }

    // `cfg(unix)` outside `platform`: Unix file modes are the simplest way to
    // make a folder unreadable in a test.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_is_runtimes_read() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let php = tmp.path().join("runtimes").join("php");
        std::fs::create_dir_all(&php).unwrap();
        std::fs::set_permissions(&php, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = list(tmp.path());
        std::fs::set_permissions(&php, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Root can read anything; then there is no error to check.
        if let Err(err) = result {
            assert_eq!(err.code(), "runtimes_read");
        }
    }
}
