//! Xdebug on or off, for every installed PHP. Task M5-11.
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
//! | 4xx `invalid_body` | the body is not `{"enabled": bool}` |
//!
//! One switch for every PHP, off by default: Xdebug makes PHP two to four
//! times slower (ADR 0010). The route only saves the setting. The sync pass
//! (`sites_sync::pass`) does the work: it writes `php.ini` of every installed
//! PHP line with or without the Xdebug lines, and restarts the php-fpm pools
//! whose `php.ini` changed. `PUT` answers after the pass, so `php` in the
//! terminal already runs with the new setting.
//!
//! A PHP with no `ext/xdebug.so` (the official Windows build, an older
//! install) is skipped: the answer names it in `missing`. Where the daemon
//! writes no `php.ini` yet (Windows, until its php-cgi pools come), the
//! setting is saved and no PHP loads Xdebug.

use std::sync::Arc;

use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use marsa_core::config::{self, ConfigError, ConfigPaths};
use marsa_core::recipes::Platform;
use marsa_core::types::{XdebugRequest, XdebugState};

use super::api::HttpError;
use super::state::Daemon;
use super::{php, sites_sync};

/// The routes, for `api::router` to merge before its fallbacks.
pub fn routes() -> Router<Arc<Daemon>> {
    Router::new().route("/v1/php/xdebug", get(current).put(set))
}

async fn current(State(daemon): State<Arc<Daemon>>) -> Result<Json<XdebugState>, HttpError> {
    blocking(&daemon, state).await.map(Json)
}

async fn set(
    State(daemon): State<Arc<Daemon>>,
    body: Result<Json<XdebugRequest>, JsonRejection>,
) -> Result<Json<XdebugState>, HttpError> {
    let Json(XdebugRequest { enabled }) =
        body.map_err(|err| HttpError::new(err.status(), "invalid_body", err.body_text()))?;
    blocking(&daemon, move |daemon| save(daemon, enabled)).await?;
    // Also when nothing changed: the pass then repairs a `php.ini` that is
    // missing or old. In a task of its own, so a client that goes away does
    // not stop the pass halfway.
    let pass = tokio::spawn({
        let daemon = Arc::clone(&daemon);
        async move { sites_sync::pass(&daemon).await }
    });
    pass.await.map_err(|err| job_failed(&err))?;
    blocking(&daemon, state).await.map(Json)
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
    // The pass writes `php.ini` only where php-fpm runs.
    let written = Platform::current().is_some_and(php::runs_fpm);
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

/// Runs `job` on a blocking thread: it touches the disk, and may take a
/// `std::sync` lock that must not be held across `.await`.
async fn blocking<T, F>(daemon: &Arc<Daemon>, job: F) -> Result<T, HttpError>
where
    T: Send + 'static,
    F: FnOnce(&Daemon) -> Result<T, HttpError> + Send + 'static,
{
    let daemon = Arc::clone(daemon);
    let job = tokio::task::spawn_blocking(move || job(&daemon));
    job.await.unwrap_or_else(|err| Err(job_failed(&err)))
}

fn job_failed(err: &tokio::task::JoinError) -> HttpError {
    tracing::error!(error = %err, "the Xdebug job failed");
    let message = "the Xdebug job failed";
    HttpError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
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
    use std::fs;
    use std::path::PathBuf;

    use axum::body::{Body, to_bytes};
    use axum::extract::Request;
    use marsa_core::php::PhpFiles;
    use marsa_core::platform::daemon_file::{Token, daemon_authority};
    use semver::Version;
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tower::ServiceExt as _;

    use crate::daemon::api::router;
    use crate::daemon::auth::AuthConfig;

    /// The daemon port in the `Host` header. No socket is bound for it.
    const PORT: u16 = 51238;

    /// The real router and auth layer on a temp data folder with three PHP
    /// versions and no site: 8.3.1 without Xdebug, and 8.4.1 and 8.4.3 with
    /// `ext/xdebug.so`. Caddy does not run, so a pass only writes files.
    struct App {
        router: Router,
        bearer: String,
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
            let token = Token::generate().unwrap();
            let bearer = token.bearer_header();
            let daemon = Arc::new(Daemon::new(dir.path().to_path_buf()));
            let router = router(daemon, AuthConfig::new(Arc::new(token), PORT));
            Self {
                router,
                bearer,
                dir,
            }
        }

        /// The status and the JSON body of one request with the token.
        async fn send(&self, method: &str, body: Option<Value>) -> (StatusCode, Value) {
            let request = Request::builder()
                .method(method)
                .uri("/v1/php/xdebug")
                .header("host", daemon_authority(PORT))
                .header("authorization", &self.bearer)
                .header("content-type", "application/json");
            let body = body.map_or(Body::empty(), |json| Body::from(json.to_string()));
            let response = self.router.clone().oneshot(request.body(body).unwrap());
            let response = response.await.unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
            (status, serde_json::from_slice(&bytes).unwrap())
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

    #[tokio::test]
    async fn put_saves_the_setting_and_every_php_ini_follows() {
        let app = App::new();
        // `php.ini` is written where php-fpm runs: macOS and Linux. On
        // Windows the setting is saved, and no PHP loads Xdebug yet.
        let loaded = if cfg!(unix) {
            json!(["8.4.3"])
        } else {
            json!([])
        };
        let off = json!({ "enabled": false, "loaded": [], "missing": ["8.3.1"] });
        let on = json!({ "enabled": true, "loaded": loaded, "missing": ["8.3.1"] });
        assert_eq!(app.send("GET", None).await, (StatusCode::OK, off.clone()));

        let put_on = app.send("PUT", Some(json!({ "enabled": true }))).await;
        assert_eq!(put_on, (StatusCode::OK, on.clone()));
        assert_eq!(app.send("GET", None).await, (StatusCode::OK, on));
        let text = fs::read_to_string(app.global_toml()).unwrap();
        assert!(text.contains("[php]\nxdebug = true\n"), "{text}");
        // The path is the one of the newest 8.4, under the data folder as it
        // was given: on macOS the temp folder is behind a symlink, so a
        // canonicalized path would not match.
        if cfg!(unix) {
            let so = PhpFiles::new(app.dir.path(), &Version::parse("8.4.3").unwrap()).xdebug_so();
            let line = format!("\nzend_extension = \"{}\"\n", so.display());
            assert!(app.ini("8.4").unwrap().contains(&line));
            assert!(!app.ini("8.3").unwrap().contains("xdebug"));
        }

        let put_off = app.send("PUT", Some(json!({ "enabled": false }))).await;
        assert_eq!(put_off, (StatusCode::OK, off));
        let text = fs::read_to_string(app.global_toml()).unwrap();
        assert!(!text.contains("xdebug"), "{text}");
        if cfg!(unix) {
            assert!(!app.ini("8.4").unwrap().contains("xdebug"));
        }
    }

    #[tokio::test]
    async fn a_bad_body_and_a_broken_global_toml_are_refused() {
        let app = App::new();
        for bad in [json!({}), json!({ "enabled": "on" })] {
            let (status, body) = app.send("PUT", Some(bad)).await;
            assert!(status.is_client_error(), "{status}");
            assert_eq!(body["error"]["code"], "invalid_body");
        }
        assert!(!app.global_toml().exists());
        assert_eq!(app.ini("8.4"), None, "no pass ran");

        fs::create_dir_all(app.global_toml().parent().unwrap()).unwrap();
        fs::write(app.global_toml(), "[php]\nxdebug = 1\n").unwrap();
        for (method, body) in [("GET", None), ("PUT", Some(json!({ "enabled": true })))] {
            let (status, body) = app.send(method, body).await;
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(body["error"]["code"], "config_invalid");
        }
    }

    #[tokio::test]
    async fn a_request_without_the_token_is_refused() {
        let app = App::new();
        let request = Request::builder()
            .method("PUT")
            .uri("/v1/php/xdebug")
            .header("host", daemon_authority(PORT))
            .header("content-type", "application/json")
            .body(Body::from(json!({ "enabled": true }).to_string()))
            .unwrap();
        let response = app.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(!app.global_toml().exists());
    }
}
