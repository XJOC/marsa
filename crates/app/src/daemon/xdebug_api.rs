//! Xdebug on or off, for every installed PHP.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/php/xdebug` | `XdebugState`: the setting, the PHP versions that load Xdebug, and those that have none |
//! | `PUT /v1/php/xdebug` | `{"enabled": bool}`: saves `[php] xdebug` in `global.toml`, runs a sync pass, then answers the new `XdebugState` |
//!
//! | Error | When |
//! | --- | --- |
//! | 500 `config_read`, `config_invalid`, `config_write` | `global.toml` cannot be read or written |
//! | 500 `runtimes_read` | the folder of the PHP versions cannot be read |
//! | 500 `xdebug_interrupted` | `PUT` only: the daemon stopped before the pass ended; the setting is saved |
//! | 4xx `invalid_body` | the body is not `{"enabled": bool}` |
//!
//! One switch for every PHP, off by default: Xdebug makes PHP two to four
//! times slower (ADR 0010). The route only saves the setting, then asks the
//! sites sync task for a pass (`Daemon::pass_requests`) and waits for it. The
//! pass does the work: it writes `php.ini` of every installed PHP line with
//! or without the Xdebug lines, and restarts the php-fpm pools whose
//! `php.ini` changed. `PUT` answers after the pass, so `php` in the terminal
//! already runs with the new setting. The pass runs in the sync task, so a
//! client that goes away does not stop it halfway.
//!
//! A PHP with no `ext/xdebug.so` (the official Windows build, an older
//! install) is skipped: the answer names it in `missing`. On Windows,
//! php-cgi reads the same `php.ini` (ADR 0016), so the switch works the
//! same there for a PHP that has the file.

use marsa_core::codes;
use std::sync::Arc;

use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use marsa_core::config::{self, ConfigPaths};
use marsa_core::recipes::Platform;
use marsa_core::types::{XdebugRequest, XdebugState};

use super::api::HttpError;
use super::http::{self, Rejected as _, global_error};
use super::services::php;
use super::sites_sync::PassNotRun;
use super::state::Daemon;

/// The routes, for `api::router` to merge before its fallbacks.
pub fn routes() -> Router<Arc<Daemon>> {
    Router::new().route("/v1/php/xdebug", get(current).put(set))
}

async fn current(State(daemon): State<Arc<Daemon>>) -> Result<Json<XdebugState>, HttpError> {
    http::blocking("Xdebug", move || state(&daemon))
        .await
        .map(Json)
}

async fn set(
    State(daemon): State<Arc<Daemon>>,
    body: Result<Json<XdebugRequest>, JsonRejection>,
) -> Result<Json<XdebugState>, HttpError> {
    let Json(XdebugRequest { enabled }) = body.map_err(|err| err.reject(codes::INVALID_BODY))?;
    // The save and the request in a task of their own: a client that hangs
    // up drops this handler, and must not leave the setting saved but not
    // applied. The pass itself runs in the sync task.
    let job = tokio::spawn({
        let daemon = Arc::clone(&daemon);
        async move {
            let worker = Arc::clone(&daemon);
            http::blocking("Xdebug", move || save(&worker, enabled)).await?;
            // Also when nothing changed: the pass then repairs a `php.ini`
            // that is missing or old.
            daemon
                .pass_requests
                .run_pass()
                .await
                .map_err(|PassNotRun| pass_not_run())
        }
    });
    job.await
        .unwrap_or_else(|err| Err(http::job_failed("Xdebug", &err)))?;
    http::blocking("Xdebug", move || state(&daemon))
        .await
        .map(Json)
}

/// The daemon stops (or its sync task does not run), so no pass applied the
/// saved setting. The start-up pass of the next daemon applies it. A code of
/// its own, like `setup_interrupted`; 500 like the daemon's other failures.
fn pass_not_run() -> HttpError {
    tracing::warn!("the Xdebug setting is saved, but no pass applied it: the daemon stops");
    let message = "the daemon stopped before every php.ini was written; the setting is saved";
    HttpError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        codes::XDEBUG_INTERRUPTED,
        message,
    )
}

/// Saves `[php] xdebug` if it differs. Blocking: it takes the lock of
/// `global.toml` and writes the file.
fn save(daemon: &Daemon, enabled: bool) -> Result<(), HttpError> {
    let _one = daemon.lock_global();
    let path = ConfigPaths::in_dir(&daemon.data_dir).global;
    let mut global = config::load_global(&path).map_err(|err| global_error(&err))?;
    if global.php.xdebug != enabled {
        global.php.xdebug = enabled;
        config::save_global(&path, &global).map_err(|err| global_error(&err))?;
        tracing::info!(enabled, "the Xdebug setting changed");
    }
    Ok(())
}

/// The setting, and what it means for each installed PHP line, decided as
/// the sync pass decides it ([`php::line_versions`], [`php::line_files`]).
/// Blocking: it reads `global.toml` and the folder of the PHP versions.
fn state(daemon: &Daemon) -> Result<XdebugState, HttpError> {
    let data_dir = &daemon.data_dir;
    let enabled = php::xdebug_enabled(data_dir).map_err(|err| global_error(&err))?;
    let sites = daemon.sites().map(|list| list.sites).unwrap_or_default();
    let lines = php::line_versions(data_dir, &php::wanted_pools(&sites)).map_err(|err| {
        tracing::warn!(code = err.code(), error = %err, "cannot list the installed PHP versions");
        HttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            err.code(),
            err.to_string(),
        )
    })?;
    // The pass writes `php.ini` on every system Marsa has PHP for.
    let written = Platform::current().is_some_and(php::runs_pools);
    let mut state = XdebugState {
        enabled,
        ..XdebugState::default()
    };
    for version in lines.into_values() {
        let has_xdebug = php::line_files(data_dir, &version, None, true)
            .xdebug
            .is_some();
        match (has_xdebug, enabled && written) {
            (false, _) => state.missing.push(version.to_string()),
            (true, true) => state.loaded.push(version.to_string()),
            (true, false) => {}
        }
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    use axum::body::Body;

    use marsa_core::php::PhpFiles;

    use semver::Version;
    use serde_json::{Value, json};
    use tempfile::TempDir;

    use crate::daemon::sites_sync::{self, SitesSync};
    use crate::daemon::test_router::TestRouter;

    /// The real router and auth layer, and the real sites sync task, on a
    /// temp data folder with three PHP versions and no site: 8.3.1 without
    /// Xdebug, and 8.4.1 and 8.4.3 with `ext/xdebug.so`. Caddy does not run,
    /// so a pass only writes files.
    struct App {
        router: TestRouter,
        daemon: Arc<Daemon>,
        sync: Option<SitesSync>,
        dir: TempDir,
    }

    impl App {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            for (version, xdebug) in [("8.3.1", false), ("8.4.1", true), ("8.4.3", true)] {
                let files = PhpFiles::new(dir.path(), &Version::parse(version).unwrap());
                fs::create_dir_all(&files.ext_dir).unwrap();
                if xdebug {
                    fs::write(files.xdebug_so(), "").unwrap();
                }
            }
            let daemon = Arc::new(Daemon::new(dir.path().to_path_buf()));
            let sync = Some(sites_sync::start(&daemon));
            Self {
                router: TestRouter::new(Arc::clone(&daemon)),
                daemon,
                sync,
                dir,
            }
        }

        /// The daemon's shutdown: the sync task ends.
        async fn stop(&mut self) {
            self.daemon.shutdown.cancel();
            if let Some(sync) = self.sync.take() {
                sync.stop().await;
            }
            self.daemon
                .stop_procs(std::time::Duration::from_secs(5))
                .await;
        }

        /// The status and the JSON body of one request with the token.
        async fn send(&self, method: &str, body: Option<Value>) -> (StatusCode, Value) {
            self.router.answer(method, "/v1/php/xdebug", body).await
        }

        fn global_toml(&self) -> PathBuf {
            ConfigPaths::in_dir(self.dir.path()).global
        }

        /// `php/<line>/php.ini`, or `None` if the pass wrote none.
        fn ini(&self, line: &str) -> Option<String> {
            let file = self.dir.path().join("php").join(line).join("php.ini");
            fs::read_to_string(file).ok()
        }
    }

    /// The answer while Xdebug is on: the newest 8.4 loads it, and 8.3.1
    /// has none. The same on every system: php-fpm and php-cgi (Windows)
    /// both read the `php.ini` the pass writes.
    fn on_state() -> Value {
        json!({ "enabled": true, "loaded": ["8.4.3"], "missing": ["8.3.1"] })
    }

    /// The `zend_extension` line of `php.ini` that loads the Xdebug of
    /// `version`.
    fn loads(dir: &Path, version: &str) -> String {
        let so = PhpFiles::new(dir, &Version::parse(version).unwrap()).xdebug_so();
        format!("\nzend_extension = \"{}\"\n", so.display())
    }

    #[tokio::test]
    async fn put_saves_the_setting_and_every_php_ini_follows() {
        let mut app = App::new();
        let off = json!({ "enabled": false, "loaded": [], "missing": ["8.3.1"] });
        let on = on_state();
        assert_eq!(app.send("GET", None).await, (StatusCode::OK, off.clone()));

        let put_on = app.send("PUT", Some(json!({ "enabled": true }))).await;
        assert_eq!(put_on, (StatusCode::OK, on.clone()));
        assert_eq!(app.send("GET", None).await, (StatusCode::OK, on));
        let text = fs::read_to_string(app.global_toml()).unwrap();
        assert!(text.contains("[php]\nxdebug = true\n"), "{text}");
        // The path is the one of the newest 8.4, under the data folder as it
        // was given: on macOS the temp folder is behind a symlink, so a
        // canonicalized path would not match.
        assert!(
            app.ini("8.4")
                .unwrap()
                .contains(&loads(app.dir.path(), "8.4.3"))
        );
        assert!(!app.ini("8.3").unwrap().contains("xdebug"));

        let put_off = app.send("PUT", Some(json!({ "enabled": false }))).await;
        assert_eq!(put_off, (StatusCode::OK, off));
        let text = fs::read_to_string(app.global_toml()).unwrap();
        assert!(!text.contains("xdebug"), "{text}");
        assert!(!app.ini("8.4").unwrap().contains("xdebug"));
        app.stop().await;
    }

    /// Both wait for the sync task; the pass that answers each one started
    /// after its setting was saved.
    #[tokio::test]
    async fn two_puts_at_once_both_get_the_on_state() {
        let mut app = App::new();
        let on = || Some(json!({ "enabled": true }));
        let (first, second) = tokio::join!(app.send("PUT", on()), app.send("PUT", on()));
        assert_eq!(first, (StatusCode::OK, on_state()));
        assert_eq!(second, (StatusCode::OK, on_state()));
        assert!(
            app.ini("8.4")
                .unwrap()
                .contains(&loads(app.dir.path(), "8.4.3"))
        );
        app.stop().await;
    }

    /// No pass can apply the setting once the daemon stops: the route says
    /// so instead of waiting, and the setting stays saved for the next
    /// daemon.
    #[tokio::test]
    async fn a_put_while_the_daemon_stops_fails_and_keeps_the_setting() {
        let mut app = App::new();
        app.stop().await;
        let (status, body) = app.send("PUT", Some(json!({ "enabled": true }))).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["code"], "xdebug_interrupted");
        let text = fs::read_to_string(app.global_toml()).unwrap();
        assert!(text.contains("[php]\nxdebug = true\n"), "{text}");
    }

    #[tokio::test]
    async fn a_bad_body_and_a_broken_global_toml_are_refused() {
        let mut app = App::new();
        for bad in [json!({}), json!({ "enabled": "on" })] {
            let (status, body) = app.send("PUT", Some(bad)).await;
            assert!(status.is_client_error(), "{status}");
            assert_eq!(body["error"]["code"], "invalid_body");
        }
        assert!(!app.global_toml().exists());

        fs::create_dir_all(app.global_toml().parent().unwrap()).unwrap();
        fs::write(app.global_toml(), "[php]\nxdebug = 1\n").unwrap();
        for (method, body) in [("GET", None), ("PUT", Some(json!({ "enabled": true })))] {
            let (status, body) = app.send(method, body).await;
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(body["error"]["code"], "config_invalid");
        }
        app.stop().await;
    }

    #[tokio::test]
    async fn a_request_without_the_token_is_refused() {
        let mut app = App::new();
        let request = app
            .router
            .builder("PUT", "/v1/php/xdebug")
            .header("content-type", "application/json")
            .body(Body::from(json!({ "enabled": true }).to_string()))
            .unwrap();
        let response = app.router.call(request).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(!app.global_toml().exists());
        app.stop().await;
    }
}
