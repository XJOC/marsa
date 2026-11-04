//! The sites routes. Task M4-10b.
//!
//! | Route | Body | Answer |
//! | --- | --- | --- |
//! | `GET /v1/sites` | – | `SiteList` |
//! | `POST /v1/sites/park` | `ParkRequest` | `SiteList` after the change |
//! | `POST /v1/sites/link` | `LinkRequest` | `SiteList` after the change |
//! | `PATCH /v1/sites/{name}` | `SitePatch` | `SiteList` after the change |
//!
//! Each change goes through [`Daemon::change_sites`]: `sites.toml` is written
//! atomically, the list is built again, and `sites_changed` goes to
//! `/v1/events`. The disk is read on a blocking thread only.
//!
//! A parked site that gets a patch gets its own `[sites.<name>]` table, with
//! its folder as `root`, so its settings are kept. From then on it counts as
//! linked.

use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use marsa_core::config::{self, ConfigPaths, GlobalConfig, SiteSettings, SitesConfig};
use marsa_core::resolver::{self, Tool};
use marsa_core::runtimes::{Runtime, RuntimesDir};
use marsa_core::sites::{self, Found, SiteError, SkipReason, Skipped};
use marsa_core::types::{
    LinkRequest, ParkRequest, Site, SiteKind, SiteList, SitePatch, SkippedFolder,
};

use super::api::HttpError;
use super::state::Daemon;

type JsonBody<T> = Result<Json<T>, JsonRejection>;
type Answer = Result<Json<SiteList>, HttpError>;

/// The routes, for `api::router` to merge before its fallbacks.
pub fn routes() -> Router<Arc<Daemon>> {
    // `park` and `link` are valid site names, and a fixed path wins over
    // `{name}`, so the two fixed paths take `PATCH` too.
    let park_patch = |daemon, body| update(daemon, "park".to_owned(), body);
    let link_patch = |daemon, body| update(daemon, "link".to_owned(), body);
    Router::new()
        .route("/v1/sites", get(list))
        .route("/v1/sites/park", post(park).patch(park_patch))
        .route("/v1/sites/link", post(link).patch(link_patch))
        .route("/v1/sites/{name}", patch(patch_site))
}

async fn list(State(daemon): State<Arc<Daemon>>) -> Answer {
    match daemon.sites() {
        Some(list) => Ok(Json(list)),
        // No build yet: see `Daemon::rebuild_sites`.
        None => daemon.rebuild_sites().await.map(Json),
    }
}

async fn park(State(daemon): State<Arc<Daemon>>, body: JsonBody<ParkRequest>) -> Answer {
    let Json(ParkRequest { path }) = body.map_err(bad_body)?;
    let change = move |sites_toml: &mut SitesConfig| {
        check_folder(&path)?;
        if sites_toml.parked.contains(&path) {
            return Ok(false);
        }
        tracing::info!(path = %path.display(), "folder parked");
        sites_toml.parked.push(path);
        Ok(true)
    };
    daemon.change_sites(change).await.map(Json)
}

async fn link(State(daemon): State<Arc<Daemon>>, body: JsonBody<LinkRequest>) -> Answer {
    let Json(LinkRequest { name, path }) = body.map_err(bad_body)?;
    let change = move |sites_toml: &mut SitesConfig| {
        check_absolute(&path)?;
        sites::link(&name, &path).map_err(|err| site_error(&err))?;
        // The same folder again keeps its settings; another folder is a new
        // project and starts from the defaults.
        if sites_toml
            .sites
            .get(&name)
            .is_some_and(|old| old.root == path)
        {
            return Ok(false);
        }
        tracing::info!(site = %name, path = %path.display(), "folder linked");
        sites_toml.sites.insert(name, new_table(path));
        Ok(true)
    };
    daemon.change_sites(change).await.map(Json)
}

async fn patch_site(
    daemon: State<Arc<Daemon>>,
    path: Result<UrlPath<String>, PathRejection>,
    body: JsonBody<SitePatch>,
) -> Answer {
    let UrlPath(name) =
        path.map_err(|err| HttpError::new(err.status(), "invalid_path", err.body_text()))?;
    update(daemon, name, body).await
}

async fn update(
    State(daemon): State<Arc<Daemon>>,
    name: String,
    body: JsonBody<SitePatch>,
) -> Answer {
    let Json(mut patch) = body.map_err(bad_body)?;
    if let Some(Some(php)) = &mut patch.php {
        check_php(php)?;
    }
    let change = move |sites_toml: &mut SitesConfig| {
        let mut next = match sites_toml.sites.get(&name) {
            Some(table) => table.clone(),
            None => new_table(parked_root(sites_toml, &name).ok_or_else(|| not_found(&name))?),
        };
        if let Some(php) = patch.php {
            next.php = php;
        }
        if let Some(secure) = patch.secure {
            next.secure = secure;
        }
        if let Some(proxy) = patch.proxy {
            next.proxy = proxy.map(NonZeroU16::get);
        }
        if sites_toml.sites.get(&name) == Some(&next) {
            return Ok(false);
        }
        tracing::info!(site = %name, "site settings changed");
        sites_toml.sites.insert(name, next);
        Ok(true)
    };
    daemon.change_sites(change).await.map(Json)
}

/// The site list for `sites_toml`: each linked table, then each parked
/// folder, joined by [`sites::merge`], with the PHP version and the settings
/// of each site.
///
/// Reads the disk (the folders, version files, `global.toml`, and
/// `runtimes/php`), so it runs on a blocking thread. Never fails: a folder
/// that cannot be used is in `skipped` with its code, and a `global.toml` or
/// `runtimes/php` that cannot be read only leaves no PHP version to pick.
pub fn build(data_dir: &Path, sites_toml: &SitesConfig) -> SiteList {
    let mut skipped = Vec::new();
    let mut linked = Vec::new();
    for (name, table) in &sites_toml.sites {
        match sites::link(name, &table.root) {
            Ok(found) => linked.push(found),
            Err(err) => skipped.push(skipped_folder(&table.root, err.code())),
        }
    }
    let mut scans = Vec::new();
    for dir in &sites_toml.parked {
        match sites::scan_parked(dir) {
            Ok(scan) => scans.push(scan),
            Err(err) => skipped.push(skipped_folder(dir, err.code())),
        }
    }
    let merged = sites::merge(linked, scans);
    // A parked folder with its own table is found twice under one name. That
    // is one site, not a clash.
    let itself =
        |s: &Skipped| matches!(&s.reason, SkipReason::NameTaken { by, .. } if *by == s.path);
    let others = merged.skipped.iter().filter(|s| !itself(s));
    skipped.extend(others.map(|s| skipped_folder(&s.path, s.reason.code())));

    let installed = RuntimesDir::in_dir(data_dir)
        .installed(Runtime::Php)
        .unwrap_or_else(|err| {
            tracing::warn!(code = err.code(), error = %err, "cannot list the PHP versions");
            Vec::new()
        });
    let global = config::load_global(&ConfigPaths::in_dir(data_dir).global).unwrap_or_else(|err| {
        tracing::warn!(code = err.code(), error = %err, "global.toml");
        GlobalConfig::default()
    });
    let site = |found: Found| -> Site {
        let own = sites_toml
            .sites
            .get(&found.name)
            .filter(|table| table.root == found.root);
        // The shim's rule (`site_for`: the nearest table at or above the
        // folder), so a terminal in the root runs the same PHP. The site's own
        // table first, for two names on one folder.
        let setting = own.or_else(|| sites_toml.site_for(&found.root).map(|(_, table)| table));
        let setting = setting.and_then(|table| table.php.as_deref());
        let default = global.php.default.as_deref();
        // No PHP at all: skip the version files, the answer is `None`.
        let php = if installed.is_empty() {
            None
        } else {
            match resolver::resolve(Tool::Php, &found.root, setting, &installed, default) {
                Ok(resolution) => Some(resolution.version),
                Err(err) => {
                    let code = err.code();
                    tracing::debug!(site = %found.name, code, error = %err, "no PHP version");
                    None
                }
            }
        };
        let proxy = own.and_then(|table| table.proxy);
        let mut site = found.into_site(php, own.is_none_or(|table| table.secure));
        if let Some(port) = proxy {
            site.kind = SiteKind::NodeProxy { port };
        }
        site
    };
    SiteList {
        parked: sites_toml.parked.clone(),
        sites: merged.found.into_iter().map(site).collect(),
        skipped,
    }
}

/// The folder of the parked site `name`, found as [`build`] finds it: the
/// first parked folder that has it wins.
fn parked_root(sites_toml: &SitesConfig, name: &str) -> Option<PathBuf> {
    sites_toml.parked.iter().find_map(|dir| {
        let scan = sites::scan_parked(dir).ok()?;
        let found = scan.found.into_iter().find(|found| found.name == name)?;
        Some(found.root)
    })
}

/// A new `[sites.<name>]` table, every setting at its default.
fn new_table(root: PathBuf) -> SiteSettings {
    SiteSettings {
        root,
        php: None,
        node: None,
        secure: true,
        proxy: None,
    }
}

fn skipped_folder(path: &Path, code: &str) -> SkippedFolder {
    SkippedFolder {
        // JSON text must be UTF-8, and the path is only shown.
        path: PathBuf::from(path.to_string_lossy().into_owned()),
        code: code.to_owned(),
    }
}

/// `sites.toml` keeps absolute paths only: a relative one would depend on
/// the daemon's working folder.
fn check_absolute(path: &Path) -> Result<(), HttpError> {
    if path.is_absolute() {
        return Ok(());
    }
    let message = format!("{} is not an absolute path", path.display());
    Err(HttpError::new(
        StatusCode::BAD_REQUEST,
        "site_path_relative",
        message,
    ))
}

/// A folder to park: absolute, UTF-8 (for the Caddy config), and a folder.
fn check_folder(path: &Path) -> Result<(), HttpError> {
    check_absolute(path)?;
    let path = path.to_path_buf();
    let err = if path.to_str().is_none() {
        SiteError::PathNotUtf8 { path }
    } else if !path.is_dir() {
        SiteError::NotAFolder { path }
    } else {
        return Ok(());
    };
    Err(site_error(&err))
}

/// Refuses a version the shim would refuse (`latest`, `eight`), and trims it.
fn check_php(php: &mut String) -> Result<(), HttpError> {
    resolver::best_match(php, &[]).map_err(|err| {
        let message = format!("{php:?}: {err}");
        HttpError::new(StatusCode::BAD_REQUEST, err.code(), message)
    })?;
    *php = php.trim().to_owned();
    Ok(())
}

fn site_error(err: &SiteError) -> HttpError {
    let status = match err {
        SiteError::ReadDir { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::BAD_REQUEST,
    };
    HttpError::new(status, err.code(), err.to_string())
}

fn not_found(name: &str) -> HttpError {
    let message = format!("no site named {name:?}");
    HttpError::new(StatusCode::NOT_FOUND, "site_not_found", message)
}

fn bad_body(rejection: JsonRejection) -> HttpError {
    HttpError::new(rejection.status(), "invalid_body", rejection.body_text())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    use axum::body::{Body, to_bytes};
    use axum::extract::Request;
    use axum::response::Response;
    use marsa_core::platform::daemon_file::{Token, daemon_authority};
    use marsa_core::types::{ErrorBody, Event};
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tokio_stream::StreamExt as _;
    use tower::ServiceExt as _;

    use crate::daemon::api::router;
    use crate::daemon::auth::AuthConfig;

    /// The daemon port in the `Host` header. No socket is bound for it.
    const PORT: u16 = 51236;

    /// The real router and auth layer on a temp data folder.
    struct App {
        daemon: Arc<Daemon>,
        router: Router,
        bearer: String,
        dir: TempDir,
    }

    impl App {
        fn new() -> Self {
            Self::on(TempDir::new().unwrap())
        }

        /// A new daemon on the same data folder, as after a restart.
        fn restart(self) -> Self {
            Self::on(self.dir)
        }

        fn on(dir: TempDir) -> Self {
            let token = Token::generate().unwrap();
            let bearer = token.bearer_header();
            let daemon = Arc::new(Daemon::new(dir.path().to_path_buf()));
            let router = router(daemon.clone(), AuthConfig::new(Arc::new(token), PORT));
            Self {
                daemon,
                router,
                bearer,
                dir,
            }
        }

        /// `<tmp>/code`, to park: `shop` (a copy of `fixtures/laravel`, whose
        /// `.php-version` says 8.4), an empty `blog`, a name with no ASCII
        /// letters, a hidden folder, and a file.
        fn code(&self) -> PathBuf {
            let code = self.dir.path().join("code");
            let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("fixtures")
                .join("laravel");
            copy_dir(&fixture, &code.join("shop"));
            for dir in ["blog", "متجر", ".git"] {
                fs::create_dir_all(code.join(dir)).unwrap();
            }
            fs::write(code.join("notes.txt"), "").unwrap();
            code
        }

        fn install_php(&self, versions: &[&str]) {
            let php = RuntimesDir::in_dir(self.dir.path()).runtime_dir(Runtime::Php);
            for version in versions {
                fs::create_dir_all(php.join(version)).unwrap();
            }
        }

        fn sites_toml(&self) -> PathBuf {
            ConfigPaths::in_dir(self.dir.path()).sites
        }

        async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> Response {
            let mut request = Request::builder()
                .method(method)
                .uri(uri)
                .header("host", daemon_authority(PORT))
                .header("authorization", &self.bearer);
            if body.is_some() {
                request = request.header("content-type", "application/json");
            }
            let body = body.map_or_else(Body::empty, |json| Body::from(json.to_string()));
            self.router
                .clone()
                .oneshot(request.body(body).unwrap())
                .await
                .unwrap()
        }

        async fn answer(
            &self,
            method: &str,
            uri: &str,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            let response = self.send(method, uri, body).await;
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
            (status, serde_json::from_slice(&bytes).unwrap())
        }

        async fn list(&self, method: &str, uri: &str, body: Option<Value>) -> SiteList {
            let (status, json) = self.answer(method, uri, body).await;
            assert_eq!(status, StatusCode::OK, "{method} {uri}: {json}");
            serde_json::from_value(json).unwrap()
        }

        /// The status and the error code of a refused request.
        async fn refused(
            &self,
            method: &str,
            uri: &str,
            body: Option<Value>,
        ) -> (StatusCode, String) {
            let (status, json) = self.answer(method, uri, body).await;
            let error: ErrorBody = serde_json::from_value(json).unwrap();
            (status, error.error.code)
        }

        async fn park(&self, path: &Path) -> SiteList {
            let body = json!({ "path": path });
            self.list("POST", "/v1/sites/park", Some(body)).await
        }
    }

    fn copy_dir(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    fn site<'a>(list: &'a SiteList, name: &str) -> &'a Site {
        let site = list.sites.iter().find(|site| site.name == name);
        site.unwrap_or_else(|| panic!("no site {name} in {list:?}"))
    }

    fn php(site: &Site) -> Option<String> {
        site.php.as_ref().map(ToString::to_string)
    }

    #[tokio::test]
    async fn park_then_get_lists_the_sub_folders() {
        let app = App::new();
        let code = app.code();
        app.install_php(&["8.3.27", "8.4.23"]);

        let parked = app.park(&code).await;
        let got = app.list("GET", "/v1/sites", None).await;
        assert_eq!(got, parked);
        assert_eq!(got.parked, std::slice::from_ref(&code));
        let shown: Vec<_> = got
            .sites
            .iter()
            .map(|site| (site.name.as_str(), site.kind, php(site), site.secure))
            .collect();
        assert_eq!(
            shown,
            [
                ("blog", SiteKind::Static, None, true),
                ("shop", SiteKind::Laravel, Some("8.4.23".to_owned()), true),
            ]
        );
        let shop = site(&got, "shop");
        assert_eq!(shop.root, code.join("shop"));
        assert_eq!(shop.docroot, code.join("shop").join("public"));
        let skipped = SkippedFolder {
            path: code.join("متجر"),
            code: "site_name_invalid".into(),
        };
        assert_eq!(got.skipped, [skipped]);

        // Parked twice is parked once.
        assert_eq!(app.park(&code).await, got);
        let file = config::load_sites(&app.sites_toml()).unwrap();
        assert_eq!(file.parked, [code]);
        assert!(file.sites.is_empty());
    }

    #[tokio::test]
    async fn patch_changes_a_site_and_the_file_keeps_it() {
        let app = App::new();
        let code = app.code();
        app.install_php(&["8.3.27", "8.4.23"]);
        app.park(&code).await;

        let list = app
            .list("PATCH", "/v1/sites/shop", Some(json!({ "secure": false })))
            .await;
        assert!(!site(&list, "shop").secure);
        // Its own table now, and not reported as taken by itself.
        assert_eq!(list.skipped.len(), 1, "{list:?}");
        let file = config::load_sites(&app.sites_toml()).unwrap();
        let table = SiteSettings {
            secure: false,
            ..new_table(code.join("shop"))
        };
        assert_eq!(file.sites["shop"], table);

        let body = json!({ "php": " 8.3 ", "proxy": 5173 });
        let list = app.list("PATCH", "/v1/sites/shop", Some(body)).await;
        let shop = site(&list, "shop");
        assert_eq!(shop.kind, SiteKind::NodeProxy { port: 5173 });
        assert_eq!((php(shop).as_deref(), shop.secure), (Some("8.3.27"), false));

        // `null` clears; a missing field keeps.
        let body = json!({ "php": null, "proxy": null });
        let list = app.list("PATCH", "/v1/sites/shop", Some(body)).await;
        let shop = site(&list, "shop");
        assert_eq!(shop.kind, SiteKind::Laravel);
        assert_eq!((php(shop).as_deref(), shop.secure), (Some("8.4.23"), false));
        let file = config::load_sites(&app.sites_toml()).unwrap();
        assert_eq!(file.sites["shop"], table);

        // A new daemon reads it back; its first GET builds the list.
        let app = app.restart();
        assert!(app.daemon.sites().is_none());
        let list = app.list("GET", "/v1/sites", None).await;
        assert!(!site(&list, "shop").secure);
    }

    #[tokio::test]
    async fn link_adds_a_site_that_wins_over_a_parked_one() {
        let app = App::new();
        let code = app.code();
        let api = app.dir.path().join("work").join("api");
        fs::create_dir_all(&api).unwrap();
        app.park(&code).await;

        let body = json!({ "name": "shop", "path": api });
        let list = app.list("POST", "/v1/sites/link", Some(body)).await;
        let shop = site(&list, "shop");
        assert_eq!((&shop.root, shop.kind), (&api, SiteKind::Static));
        let taken = SkippedFolder {
            path: code.join("shop"),
            code: "site_name_taken".into(),
        };
        assert!(list.skipped.contains(&taken), "{list:?}");
        let file = config::load_sites(&app.sites_toml()).unwrap();
        assert_eq!(file.sites["shop"], new_table(api));

        // `park` is a site name too, so its fixed path takes PATCH.
        fs::create_dir(code.join("park")).unwrap();
        let body = json!({ "secure": false });
        let list = app.list("PATCH", "/v1/sites/park", Some(body)).await;
        assert!(!site(&list, "park").secure);
    }

    #[tokio::test]
    async fn sites_changed_reaches_an_events_reader() {
        let app = App::new();
        let code = app.code();
        let events = app.send("GET", "/v1/events", None).await;
        let mut chunks = Body::into_data_stream(events.into_body());

        let parked = app.park(&code).await;
        let mut text = String::new();
        while !text.contains("\n\n") {
            let chunk = tokio::time::timeout(Duration::from_secs(10), chunks.next())
                .await
                .expect("an event in time")
                .expect("the stream stays open")
                .unwrap();
            text.push_str(std::str::from_utf8(&chunk).unwrap());
        }
        let data = text.strip_prefix("data: ").expect("one event").trim_end();
        let event: Event = serde_json::from_str(data).unwrap();
        assert_eq!(event, Event::SitesChanged(parked));
    }

    #[tokio::test]
    async fn a_rebuild_publishes_only_a_changed_list() {
        let app = App::new();
        let code = app.code();
        let sites_toml = SitesConfig {
            parked: vec![code.clone()],
            ..SitesConfig::default()
        };
        config::save_sites(&app.sites_toml(), &sites_toml).unwrap();
        let mut events = app.daemon.events.subscribe();

        let first = app.daemon.rebuild_sites().await.unwrap();
        assert_eq!(first.sites.len(), 2);
        assert_eq!(app.daemon.rebuild_sites().await.unwrap(), first);
        assert!(events.try_recv().is_err(), "the start is not a change");

        fs::create_dir(code.join("new")).unwrap();
        let next = app.daemon.rebuild_sites().await.unwrap();
        assert_eq!(
            events.try_recv().unwrap(),
            Event::SitesChanged(next.clone())
        );
        assert_eq!(app.daemon.sites(), Some(next));
    }

    #[tokio::test]
    async fn bad_requests_get_their_codes_and_change_nothing() {
        let app = App::new();
        let code = app.code();
        let file = code.join("notes.txt");
        let missing = code.join("missing");
        let (bad, unprocessable) = (StatusCode::BAD_REQUEST, StatusCode::UNPROCESSABLE_ENTITY);
        let (park, link, shop) = ("/v1/sites/park", "/v1/sites/link", "/v1/sites/shop");
        for (body, status, want) in [
            (json!({ "path": "code" }), bad, "site_path_relative"),
            (json!({ "path": file }), bad, "site_not_a_folder"),
            (json!({ "path": missing }), bad, "site_not_a_folder"),
            (json!({}), unprocessable, "invalid_body"),
        ] {
            let got = app.refused("POST", park, Some(body.clone())).await;
            assert_eq!(got, (status, want.to_owned()), "{body}");
        }
        for (body, want) in [
            (json!({ "name": "A", "path": code }), "site_name_invalid"),
            (json!({ "name": "n", "path": file }), "site_not_a_folder"),
            (json!({ "name": "a", "path": "a" }), "site_path_relative"),
        ] {
            let got = app.refused("POST", link, Some(body.clone())).await;
            assert_eq!(got, (bad, want.to_owned()), "{body}");
        }
        for (body, status, want) in [
            (json!({ "php": "latest" }), bad, "unsupported_alias"),
            (json!({ "php": "eight" }), bad, "invalid_version"),
            (json!({ "proxy": 0 }), unprocessable, "invalid_body"),
        ] {
            let got = app.refused("PATCH", shop, Some(body.clone())).await;
            assert_eq!(got, (status, want.to_owned()), "{body}");
        }
        let body = json!({ "secure": false });
        let got = app.refused("PATCH", "/v1/sites/nope", Some(body)).await;
        assert_eq!(got, (StatusCode::NOT_FOUND, "site_not_found".to_owned()));
        let got = app.refused("GET", shop, None).await;
        assert_eq!(
            got,
            (
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed".to_owned()
            )
        );
        assert!(!app.sites_toml().exists());

        // A broken `sites.toml` is the daemon's problem, not the request's.
        fs::create_dir_all(app.sites_toml().parent().unwrap()).unwrap();
        fs::write(app.sites_toml(), "parked = 1\n").unwrap();
        let broken = (
            StatusCode::INTERNAL_SERVER_ERROR,
            "config_invalid".to_owned(),
        );
        assert_eq!(app.refused("GET", "/v1/sites", None).await, broken);
        let body = json!({ "path": code });
        assert_eq!(app.refused("POST", park, Some(body)).await, broken);
    }
}
