//! The runtimes routes. Task M3-12.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/runtimes` | `RuntimeList`: the installed versions of each runtime |
//! | `POST /v1/runtimes/{tool}/install` | 202 `InstallAccepted`, then `install` events on `/v1/events` |
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
use axum::routing::{get, post};
use axum::{Json, Router};
use marsa_core::installer::extract::ExtractError;
use marsa_core::installer::fetch::FetchError;
use marsa_core::installer::{self, InstallEnv, InstallError, InstalledVersion};
use marsa_core::runtimes::{Runtime, RuntimesDir, RuntimesError};
use marsa_core::types::{
    Event, InstallAccepted, InstallDone, InstallEvent, InstallFailure, InstallProgress,
    InstallRequest, InstallUpdate, RuntimeList, RuntimeVersions,
};

use super::api::HttpError;
use super::state::Daemon;

/// Every runtime, in the order `GET /v1/runtimes` lists them.
const ALL: [Runtime; 3] = [Runtime::Php, Runtime::Node, Runtime::Composer];

/// The routes, for `api::router` to merge before its fallbacks.
pub fn routes() -> Router<Arc<Daemon>> {
    Router::new()
        .route("/v1/runtimes", get(runtimes))
        .route("/v1/runtimes/{tool}/install", post(install))
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

    /// Whether an install runs now.
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
    /// `None` while another install runs. No lock: one atomic swap.
    fn take(daemon: &Arc<Daemon>) -> Option<Self> {
        let installs = &daemon.installs;
        installs
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        let id = installs.last_id.fetch_add(1, Ordering::Relaxed) + 1;
        Some(Self {
            daemon: Arc::clone(daemon),
            id,
        })
    }
}

impl Drop for Started {
    fn drop(&mut self) {
        self.daemon.installs.running.store(false, Ordering::Release);
    }
}

async fn runtimes(State(daemon): State<Arc<Daemon>>) -> Result<Json<RuntimeList>, HttpError> {
    // Three small folder reads; not worth a blocking thread.
    list(&daemon.data_dir).map(Json).map_err(|err| {
        tracing::warn!(code = err.code(), error = %err, "cannot list runtimes");
        HttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            err.code(),
            err.to_string(),
        )
    })
}

/// The installed versions of every runtime, from `runtimes/`.
///
/// # Errors
///
/// `runtimes_read` if a runtime folder exists but cannot be read.
pub fn list(data_dir: &Path) -> Result<RuntimeList, RuntimesError> {
    let dir = RuntimesDir::in_dir(data_dir);
    let runtimes = ALL
        .into_iter()
        .map(|runtime| {
            let installed = dir.installed(runtime)?;
            Ok(RuntimeVersions {
                runtime,
                installed: installed.iter().map(ToString::to_string).collect(),
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
        return Err(HttpError::new(
            StatusCode::CONFLICT,
            InstallError::Busy.code(),
            InstallError::Busy.to_string(),
        ));
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

/// The `{tool}` path segment.
fn runtime(tool: &str) -> Result<Runtime, HttpError> {
    ALL.into_iter()
        .find(|runtime| runtime.dir_name() == tool)
        .ok_or_else(|| {
            let message = format!("no tool named {tool:?}; use php, node, or composer");
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

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::installer::manifest::ManifestError;
    use marsa_core::resolver::BadRequest;
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
    fn only_the_three_tools_are_known() {
        for (tool, runtime) in [
            ("php", Runtime::Php),
            ("node", Runtime::Node),
            ("composer", Runtime::Composer),
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
            ".tmp",
        ] {
            std::fs::create_dir_all(tmp.path().join("runtimes").join(dir)).unwrap();
        }
        let got = list(tmp.path()).unwrap();
        let shown: Vec<_> = got
            .runtimes
            .iter()
            .map(|r| (r.runtime, r.installed.join(",")))
            .collect();
        assert_eq!(
            shown,
            [
                (Runtime::Php, "8.3.27,8.4.23".to_owned()),
                (Runtime::Node, "22.11.0".to_owned()),
                (Runtime::Composer, String::new()),
            ]
        );
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
