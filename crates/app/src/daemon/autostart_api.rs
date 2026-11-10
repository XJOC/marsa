//! Start the tray icon at login, or not. Task M5-03.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/autostart` | `AutostartState`: `enabled`, or `stale` for a login file this Marsa would not write |
//! | `PUT /v1/autostart` | `{"enabled": bool}`: writes or deletes the login file, then answers the new `AutostartState` |
//!
//! | Error | When |
//! | --- | --- |
//! | 501 `autostart_unsupported` | Windows, for now (task M4-19) |
//! | 409 `tray_not_found` | `marsa-tray` is not next to `marsad` (enable); its path is not known (any) |
//! | 409 `autostart_path_invalid` | the login file cannot hold the `marsa-tray` path (enable) |
//! | 500 `autostart_no_home` | the system gives no home folder |
//! | 500 `autostart_read`, `autostart_write` | the login file or its folder cannot be read or written |
//! | 4xx `invalid_body` | the body is not `{"enabled": bool}` |
//!
//! The daemon writes the file because only the daemon changes state
//! (AGENTS.md section 6). The file is the only change: no `launchctl`, no
//! admin rights. It takes effect at the next login. The file itself is
//! `marsa_core::platform::autostart`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use marsa_core::platform::autostart::{
    self, AutostartError, AutostartFile, AutostartKind, tray_path,
};
use marsa_core::types::{AutostartRequest, AutostartState};

use super::api::HttpError;
use super::state::Daemon;

/// The routes, for `api::router` to merge before its fallbacks.
pub fn routes() -> Router<Arc<Daemon>> {
    Router::new().route("/v1/autostart", get(current).put(set))
}

/// Where the login file goes and what it starts. It lives in
/// `Daemon::autostart`. `Daemon::new` gives [`Autostart::unset`], so no test
/// reaches the user's real folders; `server::run` puts
/// [`Autostart::this_user`] in. Tests put a temp folder in with
/// [`Autostart::with`].
#[derive(Debug)]
pub struct Autostart {
    /// `None`: no login start on this system yet (Windows).
    kind: Option<AutostartKind>,
    /// The folder of the login file. `None`: the system gives no home folder.
    dir: Option<PathBuf>,
    /// `marsa-tray`. `None`: the daemon's own path is not known.
    tray: Option<PathBuf>,
    /// Held while the file is read, written, or deleted, so two requests
    /// never share the temp file. Taken on a blocking thread only.
    lock: Mutex<()>,
}

impl Autostart {
    /// This system's kind, with no folder and no `marsa-tray`: every request
    /// gets `autostart_no_home` (`autostart_unsupported` on Windows).
    pub fn unset() -> Self {
        Self::with(AutostartKind::current(), None, None)
    }

    /// The daemon's: this system's kind, the folder of this user (from
    /// `directories`), and `marsa-tray` next to the running `marsad`.
    pub fn this_user() -> Self {
        let kind = AutostartKind::current();
        let dir = kind.and_then(autostart::user_dir);
        let tray = tray_path()
            .inspect_err(|err| {
                tracing::warn!(error = %err, "cannot find the running program, so not marsa-tray");
            })
            .ok();
        Self::with(kind, dir, tray)
    }

    /// For tests: any kind, folder, and `marsa-tray` path.
    pub fn with(kind: Option<AutostartKind>, dir: Option<PathBuf>, tray: Option<PathBuf>) -> Self {
        Self {
            kind,
            dir,
            tray,
            lock: Mutex::default(),
        }
    }

    /// What the login file says now. Blocking: it reads the file.
    fn state(&self) -> Result<AutostartState, HttpError> {
        let (kind, dir) = self.target()?;
        let tray = self.tray.as_deref().ok_or_else(tray_unknown)?;
        let _one = self.lock();
        let file = autostart::read(kind, dir, tray).map_err(file_error)?;
        Ok(match file {
            AutostartFile::Absent => AutostartState::default(),
            AutostartFile::Current => AutostartState {
                enabled: true,
                stale: false,
            },
            AutostartFile::Stale => AutostartState {
                enabled: false,
                stale: true,
            },
        })
    }

    /// Writes (`true`) or deletes (`false`) the login file. Blocking.
    fn set(&self, enabled: bool) -> Result<AutostartState, HttpError> {
        let (kind, dir) = self.target()?;
        if !enabled {
            let _one = self.lock();
            autostart::remove(kind, dir).map_err(file_error)?;
            tracing::info!(dir = %dir.display(), "Marsa no longer starts at login");
            return Ok(AutostartState::default());
        }
        let tray = self.tray.as_deref().ok_or_else(tray_unknown)?;
        if !tray.is_file() {
            let message = format!("{} is not there", tray.display());
            return Err(HttpError::new(
                StatusCode::CONFLICT,
                "tray_not_found",
                message,
            ));
        }
        let _one = self.lock();
        let path = autostart::write(kind, dir, tray).map_err(file_error)?;
        tracing::info!(file = %path.display(), "Marsa starts at login");
        Ok(AutostartState {
            enabled: true,
            stale: false,
        })
    }

    fn target(&self) -> Result<(AutostartKind, &Path), HttpError> {
        let kind = self.kind.ok_or_else(|| {
            let message = "start at login works on macOS and Linux only for now";
            HttpError::new(
                StatusCode::NOT_IMPLEMENTED,
                "autostart_unsupported",
                message,
            )
        })?;
        let dir = self.dir.as_deref().ok_or_else(|| {
            let message = "the system gives no home folder, so the login file has no place";
            HttpError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "autostart_no_home",
                message,
            )
        })?;
        Ok((kind, dir))
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        // The lock guards no data, so a poisoned one is still good.
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

async fn current(State(daemon): State<Arc<Daemon>>) -> Result<Json<AutostartState>, HttpError> {
    blocking(daemon, Autostart::state).await
}

async fn set(
    State(daemon): State<Arc<Daemon>>,
    body: Result<Json<AutostartRequest>, JsonRejection>,
) -> Result<Json<AutostartState>, HttpError> {
    let Json(AutostartRequest { enabled }) =
        body.map_err(|err| HttpError::new(err.status(), "invalid_body", err.body_text()))?;
    blocking(daemon, move |autostart| autostart.set(enabled)).await
}

/// Runs `job` on a blocking thread: it touches the disk, and takes a
/// `std::sync` lock that must not be held across `.await`.
async fn blocking<F>(daemon: Arc<Daemon>, job: F) -> Result<Json<AutostartState>, HttpError>
where
    F: FnOnce(&Autostart) -> Result<AutostartState, HttpError> + Send + 'static,
{
    let job = tokio::task::spawn_blocking(move || job(&daemon.autostart));
    let answer = job.await.unwrap_or_else(|err| {
        tracing::error!(error = %err, "the autostart job failed");
        let message = "the autostart job failed";
        Err(HttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            message,
        ))
    });
    answer.map(Json)
}

fn tray_unknown() -> HttpError {
    let message = "the daemon's own path is not known, so neither is marsa-tray's";
    HttpError::new(StatusCode::CONFLICT, "tray_not_found", message)
}

/// The login file or its folder failed. A path the file cannot hold is the
/// install's state (409); the disk is a server error (500).
fn file_error(err: AutostartError) -> HttpError {
    let status = match err {
        AutostartError::InvalidPath { .. } => StatusCode::CONFLICT,
        AutostartError::Read { .. } | AutostartError::Write { .. } => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    let message = match std::error::Error::source(&err) {
        Some(source) => format!("{err}: {source}"),
        None => err.to_string(),
    };
    tracing::warn!(code = err.code(), error = %message, "login file");
    HttpError::new(status, err.code(), message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use axum::body::{Body, to_bytes};
    use axum::extract::Request;
    use marsa_core::platform::daemon_file::{Token, daemon_authority};
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tower::ServiceExt as _;

    use crate::daemon::api::router;
    use crate::daemon::auth::AuthConfig;

    use AutostartKind::{LaunchAgent, XdgAutostart};

    /// The daemon port in the `Host` header. No socket is bound for it.
    const PORT: u16 = 51237;
    const KINDS: [AutostartKind; 2] = [LaunchAgent, XdgAutostart];

    /// The real router and auth layer, with the login folder and
    /// `marsa-tray` in a temp folder: `<tmp>/bin/marsa-tray` (made only if
    /// `tray` is true) and `<tmp>/login` (not made: the route makes it).
    struct App {
        router: Router,
        bearer: String,
        dir: TempDir,
    }

    impl App {
        fn new(kind: AutostartKind, tray: bool) -> Self {
            let dir = TempDir::new().unwrap();
            let bin = dir.path().join("bin");
            fs::create_dir_all(&bin).unwrap();
            if tray {
                fs::write(bin.join("marsa-tray"), "").unwrap();
            }
            let autostart = Autostart::with(
                Some(kind),
                Some(dir.path().join("login")),
                Some(bin.join("marsa-tray")),
            );
            Self::with(dir, autostart)
        }

        fn with(dir: TempDir, autostart: Autostart) -> Self {
            let token = Token::generate().unwrap();
            let bearer = token.bearer_header();
            let mut daemon = Daemon::new(dir.path().join("data"));
            daemon.autostart = autostart;
            let router = router(Arc::new(daemon), AuthConfig::new(Arc::new(token), PORT));
            Self {
                router,
                bearer,
                dir,
            }
        }

        fn tray(&self) -> PathBuf {
            self.dir.path().join("bin").join("marsa-tray")
        }

        fn login(&self) -> PathBuf {
            self.dir.path().join("login")
        }

        async fn send(&self, method: &str, body: Option<Value>) -> (StatusCode, Value) {
            let mut request = Request::builder()
                .method(method)
                .uri("/v1/autostart")
                .header("host", daemon_authority(PORT))
                .header("authorization", &self.bearer);
            if body.is_some() {
                request = request.header("content-type", "application/json");
            }
            let body = body.map_or_else(Body::empty, |json| Body::from(json.to_string()));
            let response = self
                .router
                .clone()
                .oneshot(request.body(body).unwrap())
                .await
                .unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
            (status, serde_json::from_slice(&bytes).unwrap())
        }

        async fn get(&self) -> (StatusCode, Value) {
            self.send("GET", None).await
        }

        async fn put(&self, enabled: bool) -> (StatusCode, Value) {
            self.send("PUT", Some(json!({ "enabled": enabled }))).await
        }
    }

    fn ok(enabled: bool, stale: bool) -> (StatusCode, Value) {
        (
            StatusCode::OK,
            json!({ "enabled": enabled, "stale": stale }),
        )
    }

    fn code(answer: &(StatusCode, Value)) -> (StatusCode, &str) {
        let code = answer.1["error"]["code"].as_str().unwrap_or_default();
        (answer.0, code)
    }

    #[tokio::test]
    async fn enable_writes_the_file_and_disable_deletes_it() {
        for kind in KINDS {
            let app = App::new(kind, true);
            let file = kind.file_in(&app.login());
            assert_eq!(app.get().await, ok(false, false), "{kind:?}");

            assert_eq!(app.put(true).await, ok(true, false), "{kind:?}");
            assert_eq!(
                fs::read_to_string(&file).unwrap(),
                kind.render(&app.tray()).unwrap()
            );
            assert_eq!(app.get().await, ok(true, false), "{kind:?}");
            // Again: the same file, no error.
            assert_eq!(app.put(true).await, ok(true, false), "{kind:?}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&file).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o644);
            }

            assert_eq!(app.put(false).await, ok(false, false), "{kind:?}");
            assert!(!file.exists());
            assert_eq!(app.get().await, ok(false, false), "{kind:?}");
            // No file to delete is fine.
            assert_eq!(app.put(false).await, ok(false, false), "{kind:?}");
            let left: Vec<_> = fs::read_dir(app.login()).unwrap().collect();
            assert!(left.is_empty(), "{left:?}");
        }
    }

    #[tokio::test]
    async fn a_file_for_a_moved_app_is_stale_until_enabled_again() {
        for kind in KINDS {
            let app = App::new(kind, true);
            let old = app.dir.path().join("old").join("marsa-tray");
            autostart::write(kind, &app.login(), &old).unwrap();
            assert_eq!(app.get().await, ok(false, true), "{kind:?}");

            assert_eq!(app.put(true).await, ok(true, false), "{kind:?}");
            assert_eq!(app.get().await, ok(true, false), "{kind:?}");

            // A file changed by hand is stale too, and disable deletes it.
            let file = kind.file_in(&app.login());
            fs::write(&file, "changed").unwrap();
            assert_eq!(app.get().await, ok(false, true), "{kind:?}");
            assert_eq!(app.put(false).await, ok(false, false), "{kind:?}");
            assert!(!file.exists());
        }
    }

    #[tokio::test]
    async fn no_tray_next_to_marsad_is_tray_not_found() {
        for kind in KINDS {
            let app = App::new(kind, false);
            let refused = app.put(true).await;
            assert_eq!(
                code(&refused),
                (StatusCode::CONFLICT, "tray_not_found"),
                "{kind:?}"
            );
            assert!(!app.login().exists(), "{kind:?}");
            // Reading and disabling still work.
            assert_eq!(app.get().await, ok(false, false), "{kind:?}");
            assert_eq!(app.put(false).await, ok(false, false), "{kind:?}");

            // A folder where the program should be is not a program either.
            fs::create_dir(app.tray()).unwrap();
            let refused = app.put(true).await;
            assert_eq!(code(&refused), (StatusCode::CONFLICT, "tray_not_found"));
        }

        // The daemon's own path not known.
        let dir = TempDir::new().unwrap();
        let login = Some(dir.path().join("login"));
        let app = App::with(dir, Autostart::with(Some(LaunchAgent), login, None));
        for answer in [app.get().await, app.put(true).await] {
            assert_eq!(code(&answer), (StatusCode::CONFLICT, "tray_not_found"));
        }
        assert_eq!(app.put(false).await, ok(false, false));
    }

    #[tokio::test]
    async fn windows_gets_autostart_unsupported() {
        let dir = TempDir::new().unwrap();
        let login = Some(dir.path().join("login"));
        let tray = Some(dir.path().join("marsa-tray"));
        fs::write(dir.path().join("marsa-tray"), "").unwrap();
        let app = App::with(dir, Autostart::with(None, login, tray));
        for answer in [app.get().await, app.put(true).await, app.put(false).await] {
            assert_eq!(
                code(&answer),
                (StatusCode::NOT_IMPLEMENTED, "autostart_unsupported")
            );
        }
        assert!(!app.login().exists());
    }

    #[tokio::test]
    async fn a_new_daemon_never_reaches_the_users_folders() {
        let app = App::with(
            TempDir::new().unwrap(),
            Daemon::new(PathBuf::new()).autostart,
        );
        let expected = match AutostartKind::current() {
            Some(_) => (StatusCode::INTERNAL_SERVER_ERROR, "autostart_no_home"),
            None => (StatusCode::NOT_IMPLEMENTED, "autostart_unsupported"),
        };
        for answer in [app.get().await, app.put(true).await, app.put(false).await] {
            assert_eq!(code(&answer), expected);
        }
    }

    #[tokio::test]
    async fn a_path_the_file_cannot_hold_writes_nothing() {
        let dir = TempDir::new().unwrap();
        let bin = dir.path().join("100%");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("marsa-tray"), "").unwrap();
        let login = dir.path().join("login");
        let autostart = Autostart::with(
            Some(XdgAutostart),
            Some(login.clone()),
            Some(bin.join("marsa-tray")),
        );
        let app = App::with(dir, autostart);
        let refused = app.put(true).await;
        assert_eq!(
            code(&refused),
            (StatusCode::CONFLICT, "autostart_path_invalid")
        );
        assert!(!login.exists());
    }

    #[tokio::test]
    async fn a_folder_that_cannot_be_written_is_autostart_write() {
        let app = App::new(LaunchAgent, true);
        fs::write(app.login(), "a file, not a folder").unwrap();
        let refused = app.put(true).await;
        assert_eq!(
            code(&refused),
            (StatusCode::INTERNAL_SERVER_ERROR, "autostart_write")
        );
    }

    #[tokio::test]
    async fn a_bad_body_or_method_is_refused() {
        let app = App::new(LaunchAgent, true);
        for body in [
            json!({}),
            json!({ "enabled": "yes" }),
            json!({ "enabled": null }),
        ] {
            let refused = app.send("PUT", Some(body.clone())).await;
            assert_eq!(code(&refused).1, "invalid_body", "{body}");
            assert!(refused.0.is_client_error(), "{body}");
        }
        // No JSON content type.
        let refused = app.send("PUT", None).await;
        assert_eq!(code(&refused).1, "invalid_body");
        let refused = app.send("POST", None).await;
        assert_eq!(
            code(&refused),
            (StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
        );
        assert!(!app.login().exists());
    }
}
