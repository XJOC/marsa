//! Start the tray icon at login, or not.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/autostart` | `AutostartState`: `enabled`, or `stale` for a login file this Marsa would not write |
//! | `PUT /v1/autostart` | `{"enabled": bool}`: writes or deletes the login file, then answers the new `AutostartState` |
//!
//! | Error | When |
//! | --- | --- |
//! | 501 `autostart_unsupported` | Windows, for now |
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

use marsa_core::codes;
use std::sync::Arc;

use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::routing::get;
use axum::{Json, Router};
use marsa_core::types::{AutostartRequest, AutostartState};

use super::api::HttpError;
use super::autostart::Autostart;
use super::http::{self, Rejected as _};
use super::state::Daemon;

/// The routes, for `api::router` to merge before its fallbacks.
pub fn routes() -> Router<Arc<Daemon>> {
    Router::new().route("/v1/autostart", get(current).put(set))
}

async fn current(State(daemon): State<Arc<Daemon>>) -> Result<Json<AutostartState>, HttpError> {
    http::blocking("autostart", move || Autostart::state(&daemon.autostart))
        .await
        .map(Json)
}

async fn set(
    State(daemon): State<Arc<Daemon>>,
    body: Result<Json<AutostartRequest>, JsonRejection>,
) -> Result<Json<AutostartState>, HttpError> {
    let Json(AutostartRequest { enabled }) = body.map_err(|err| err.reject(codes::INVALID_BODY))?;
    http::blocking("autostart", move || daemon.autostart.set(enabled))
        .await
        .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::data_dir::DataDir;
    use std::fs;
    use std::path::PathBuf;

    use axum::http::StatusCode;
    use marsa_core::platform::autostart::{self, AutostartKind};

    use serde_json::{Value, json};
    use tempfile::TempDir;

    use crate::daemon::test_router::TestRouter;

    use AutostartKind::{LaunchAgent, XdgAutostart};

    const KINDS: [AutostartKind; 2] = [LaunchAgent, XdgAutostart];

    /// The real router and auth layer, with the login folder and
    /// `marsa-tray` in a temp folder: `<tmp>/bin/marsa-tray` (made only if
    /// `tray` is true) and `<tmp>/login` (not made: the route makes it).
    struct App {
        router: TestRouter,
        dir: TempDir,
    }

    impl App {
        fn new(kind: AutostartKind, tray: bool) -> Self {
            let dir = TempDir::new().unwrap();
            let bin = DataDir::new(dir.path()).bin();
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
            let mut daemon = Daemon::new(dir.path().join("data"));
            daemon.autostart = autostart;
            Self {
                router: TestRouter::new(Arc::new(daemon)),
                dir,
            }
        }

        fn tray(&self) -> PathBuf {
            DataDir::new(self.dir.path()).bin().join("marsa-tray")
        }

        fn login(&self) -> PathBuf {
            self.dir.path().join("login")
        }

        async fn send(&self, method: &str, body: Option<Value>) -> (StatusCode, Value) {
            self.router.answer(method, "/v1/autostart", body).await
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
