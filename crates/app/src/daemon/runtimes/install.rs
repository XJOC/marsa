//! `POST /v1/runtimes/{tool}/install`: the route, and the task that runs one install.

use marsa_core::codes;
use std::error::Error;
use std::num::NonZeroU8;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use marsa_core::config::{self, ConfigError, ConfigPaths};
use marsa_core::databases::ServerKind;
use marsa_core::installer::extract::ExtractError;
use marsa_core::installer::fetch::FetchError;
use marsa_core::installer::{self, InstallError};
use marsa_core::resolver::Tool;
use marsa_core::runtimes::{Runtime, RuntimesError};
use marsa_core::types::{
    DefaultVersion, Event, InstallAccepted, InstallDone, InstallFailure, InstallProgress,
    InstallRequest, InstallUpdate, OsMismatch,
};

use super::api::{default_tool, defaults_mut, runtime};
use crate::daemon::http::Rejected as _;
use crate::daemon::http::routes::HttpError;
use crate::daemon::services::databases;
use crate::daemon::services::databases::client_shims;
use crate::daemon::setup::one_at_a_time::{OneAtATime, Turn};
use crate::daemon::state::Daemon;

/// The right to run the one install. Dropping it lets the next one start,
/// also if the install task panics.
pub(super) struct Started {
    pub(super) daemon: Arc<Daemon>,
    _turn: Turn,
    pub(super) id: u64,
}

impl Started {
    /// `None` while another install or an uninstall runs.
    pub(super) fn take(daemon: &Arc<Daemon>) -> Option<Self> {
        let mut started = Self::claim(daemon)?;
        started.id = daemon.installs.last_id.fetch_add(1, Ordering::Relaxed) + 1;
        Some(started)
    }

    /// The same right for an uninstall, which has no id. No lock: one atomic
    /// swap.
    pub(super) fn claim(daemon: &Arc<Daemon>) -> Option<Self> {
        let turn = OneAtATime::take(&daemon.installs.running, NonZeroU8::MIN).ok()?;
        Some(Self {
            daemon: Arc::clone(daemon),
            _turn: turn,
            id: 0,
        })
    }
}

pub(super) async fn install(
    State(daemon): State<Arc<Daemon>>,
    path: Result<UrlPath<String>, PathRejection>,
    body: Result<Json<InstallRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<InstallAccepted>), HttpError> {
    let UrlPath(tool) = path.map_err(|err| err.reject(codes::INVALID_PATH))?;
    let runtime = runtime(&tool)?;
    let Json(request) = body.map_err(|err| err.reject(codes::INVALID_BODY))?;
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
                    code: codes::INSTALL_CRASHED.to_owned(),
                    message: "the install task panicked".to_owned(),
                    ..InstallFailure::default()
                }),
            }));
        }
    });
    Ok((StatusCode::ACCEPTED, Json(InstallAccepted { id })))
}

pub(super) fn busy() -> HttpError {
    let err = InstallError::Busy;
    HttpError::new(StatusCode::CONFLICT, err.code(), err.to_string())
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
            // folder with no version file works at once.
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
            // their new PHP. `rebuild_sites` logs its errors.
            if runtime == Runtime::Php {
                let _ = daemon.rebuild_sites().await;
            }
            if let Some(kind) = ServerKind::of_runtime(runtime) {
                // Before `done`, so `psql` is in `shims/` when `marsa
                // install postgres` ends.
                client_shims::sync(&daemon, kind).await;
                databases::start_after_install(&daemon, kind, done.version.clone());
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
        InstallError::OsTooOld { needs, found, .. } => {
            failure.os = Some(Box::new(OsMismatch {
                needs: needs.clone(),
                found: found.clone(),
            }));
        }
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
        InstallError::Extract(ExtractError::Io { path, .. })
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
        let old = InstallError::OsTooOld {
            what: "postgres".into(),
            needs: "macOS 15.0".into(),
            found: "macOS 14.6".into(),
        };
        let got = failure(&old, None);
        assert_eq!(got.code, "install_os_too_old");
        let os = got.os.expect("the versions are in the event");
        assert_eq!(
            (os.needs.as_str(), os.found.as_str()),
            ("macOS 15.0", "macOS 14.6")
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
}
