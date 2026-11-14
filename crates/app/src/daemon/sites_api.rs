//! The sites routes. Task M4-10b.
//!
//! | Route | Body | Answer |
//! | --- | --- | --- |
//! | `GET /v1/sites` | – | `SiteList` |
//! | `POST /v1/sites/park` | `ParkRequest` | `SiteList` after the change |
//! | `POST /v1/sites/link` | `LinkRequest` | `SiteList` after the change |
//! | `PATCH /v1/sites/{name}` | `SitePatch` | `SiteList` after the change |
//! | `DELETE /v1/sites/{name}` | – | `SiteList` after the change |
//! | `DELETE /v1/sites/park` | `ParkRequest` | `SiteList` after the change |
//!
//! Each change goes through [`Daemon::change_sites`]: `sites.toml` is written
//! atomically, the list is built again, and `sites_changed` goes to
//! `/v1/events`. The disk is read on a blocking thread only.
//!
//! A parked site that gets a patch gets its own `[sites.<name>]` table, with
//! its folder as `root`, so its settings are kept. From then on it counts as
//! linked.
//!
//! `DELETE` (task M5-09) removes a linked site's table (`unlink`), or a parked
//! folder with the tables of its sites (`forget`). A site from a parked folder
//! cannot be unlinked alone: `site_is_parked` (409).

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
    // `{name}`, so the two fixed paths take `PATCH` and `DELETE` too. A
    // `DELETE /v1/sites/park` with a JSON body forgets a folder instead.
    let park_patch = |daemon, body| update(daemon, "park".to_owned(), body);
    let link_patch = |daemon, body| update(daemon, "link".to_owned(), body);
    let link_delete = |daemon| unlink(daemon, "link".to_owned());
    Router::new()
        .route("/v1/sites", get(list))
        .route(
            "/v1/sites/park",
            post(park).patch(park_patch).delete(park_delete),
        )
        .route(
            "/v1/sites/link",
            post(link).patch(link_patch).delete(link_delete),
        )
        .route("/v1/sites/{name}", patch(patch_site).delete(unlink_site))
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

/// `DELETE /v1/sites/{name}`.
async fn unlink_site(
    daemon: State<Arc<Daemon>>,
    path: Result<UrlPath<String>, PathRejection>,
) -> Answer {
    let UrlPath(name) =
        path.map_err(|err| HttpError::new(err.status(), "invalid_path", err.body_text()))?;
    unlink(daemon, name).await
}

/// Removes the table of the linked site `name`. A site from a parked folder
/// is refused, also when it has a table of its own: the table only holds its
/// settings, and the site would stay.
async fn unlink(State(daemon): State<Arc<Daemon>>, name: String) -> Answer {
    let change = move |sites_toml: &mut SitesConfig| {
        let parked = parked_site(sites_toml, &name);
        let own_root = sites_toml.sites.get(&name).map(|table| table.root.clone());
        match (own_root, parked) {
            (Some(root), Some((dir, parked_root))) if root == parked_root => {
                Err(site_is_parked(&name, &dir))
            }
            (Some(_), _) => {
                tracing::info!(site = %name, "site unlinked");
                sites_toml.sites.remove(&name);
                Ok(true)
            }
            (None, Some((dir, _))) => Err(site_is_parked(&name, &dir)),
            (None, None) => Err(not_found(&name)),
        }
    };
    daemon.change_sites(change).await.map(Json)
}

/// `DELETE /v1/sites/park`: with a JSON body, forgets the folder in it;
/// without one (no `Content-Type`), unlinks the site named `park`.
async fn park_delete(
    daemon: State<Arc<Daemon>>,
    body: Result<Option<Json<ParkRequest>>, JsonRejection>,
) -> Answer {
    match body.map_err(bad_body)? {
        Some(Json(ParkRequest { path })) => forget(daemon, path).await,
        None => unlink(daemon, "park".to_owned()).await,
    }
}

/// Removes the parked folder `path`, and the tables of its sites: a table
/// named as the folder's scan names the sub-folder it points at. A table
/// under another name was linked by hand, and stays.
async fn forget(State(daemon): State<Arc<Daemon>>, path: PathBuf) -> Answer {
    check_absolute(&path)?;
    let change = move |sites_toml: &mut SitesConfig| {
        let index = parked_index(&sites_toml.parked, &path).ok_or_else(|| not_parked(&path))?;
        let dir = sites_toml.parked.remove(index);
        sites_toml.sites.retain(|name, table| {
            let folder = table.root.file_name().and_then(sites::name_from_folder);
            let parked = table.root.parent() == Some(dir.as_path())
                && folder.as_deref() == Some(name.as_str());
            !parked
        });
        tracing::info!(path = %dir.display(), "parked folder forgotten");
        Ok(true)
    };
    daemon.change_sites(change).await.map(Json)
}

/// Where `path` is in `parked`, matched as `park` stored it. `park` keeps the
/// path it was given (the CLI and the window make it absolute and drop `.`
/// parts first), so:
///
/// 1. By component, as [`PathBuf`] compares: `/code/`, `/code//`, and
///    `/code/.` are `/code`.
/// 2. The same, with `\r` and `\n` cut from the end: a script that read the
///    path from a file with Windows line ends.
/// 3. For a folder that still exists, by the folder the system finds for
///    both: another letter case (macOS and Windows), a link, or `..`. Only
///    compared, never passed on.
fn parked_index(parked: &[PathBuf], path: &Path) -> Option<usize> {
    let trimmed = match path.to_str() {
        Some(text) => PathBuf::from(text.trim_end_matches(['\r', '\n'])),
        None => path.to_path_buf(),
    };
    let same = |dir: &PathBuf| *dir == path || *dir == trimmed;
    parked.iter().position(same).or_else(|| {
        let real = std::fs::canonicalize(&trimmed).ok()?;
        parked
            .iter()
            .position(|dir| std::fs::canonicalize(dir).is_ok_and(|dir| dir == real))
    })
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
        // No PHP at all: skip the version files, the answer is `None`. The
        // resolution keeps where the version came from, for the window.
        let php = if installed.is_empty() {
            None
        } else {
            match resolver::resolve(Tool::Php, &found.root, setting, &installed, default) {
                Ok(resolution) => Some(resolution),
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
    parked_site(sites_toml, name).map(|(_, root)| root)
}

/// The parked folder that has the site `name`, and the site's folder.
fn parked_site(sites_toml: &SitesConfig, name: &str) -> Option<(PathBuf, PathBuf)> {
    sites_toml.parked.iter().find_map(|dir| {
        let scan = sites::scan_parked(dir).ok()?;
        let found = scan.found.into_iter().find(|found| found.name == name)?;
        Some((dir.clone(), found.root))
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

fn site_is_parked(name: &str, dir: &Path) -> HttpError {
    let message = format!(
        "{name:?} comes from the parked folder {}; forget that folder to remove its sites",
        dir.display()
    );
    HttpError::new(StatusCode::CONFLICT, "site_is_parked", message)
}

fn not_parked(path: &Path) -> HttpError {
    let message = format!("{} is not a parked folder", path.display());
    HttpError::new(StatusCode::NOT_FOUND, "folder_not_parked", message)
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
    use marsa_core::types::{ErrorBody, Event, Source};
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

    #[tokio::test]
    async fn each_site_says_where_its_php_version_came_from() {
        let app = App::new();
        let code = app.code();
        app.install_php(&["8.3.27", "8.4.23"]);
        // `api` has only a constraint, `^8.3`: the newest match wins.
        fs::create_dir(code.join("api")).unwrap();
        let composer = code.join("api").join("composer.json");
        fs::write(&composer, r#"{ "require": { "php": "^8.3" } }"#).unwrap();
        let mut global = GlobalConfig::default();
        global.php.default = Some("8.3".into());
        config::save_global(&ConfigPaths::in_dir(app.dir.path()).global, &global).unwrap();
        let source = |list: &SiteList, name: &str| {
            let site = site(list, name);
            (php(site), site.php_source.clone())
        };
        let version_file = Source::VersionFile(code.join("shop").join(".php-version"));
        let shop = (Some("8.4.23".to_owned()), Some(version_file.clone()));

        let list = app.park(&code).await;
        assert_eq!(source(&list, "shop"), shop);
        let api = (
            Some("8.4.23".to_owned()),
            Some(Source::Constraint(composer)),
        );
        assert_eq!(source(&list, "api"), api);
        let blog = (Some("8.3.27".to_owned()), Some(Source::GlobalDefault));
        assert_eq!(source(&list, "blog"), blog);

        // The site's own setting wins over its `.php-version`; clearing it
        // gives the file back.
        let body = json!({ "php": "8.3" });
        let list = app.list("PATCH", "/v1/sites/shop", Some(body)).await;
        let own = (Some("8.3.27".to_owned()), Some(Source::ProjectSetting));
        assert_eq!(source(&list, "shop"), own);
        let body = json!({ "php": null });
        let list = app.list("PATCH", "/v1/sites/shop", Some(body)).await;
        assert_eq!(source(&list, "shop"), shop);

        // No match: no version, and so no source.
        let body = json!({ "php": "7.4" });
        let list = app.list("PATCH", "/v1/sites/shop", Some(body)).await;
        assert_eq!(source(&list, "shop"), (None, None));
    }

    // M5-09: `DELETE /v1/sites/{name}` and `DELETE /v1/sites/park`.

    fn link_body(name: &str, path: &Path) -> Value {
        json!({ "name": name, "path": path })
    }

    #[tokio::test]
    async fn unlink_removes_a_linked_site_and_its_own_settings() {
        let app = App::new();
        let code = app.code();
        app.park(&code).await;
        let api = app.dir.path().join("work").join("api");
        fs::create_dir_all(&api).unwrap();
        app.list("POST", "/v1/sites/link", Some(link_body("api", &api)))
            .await;
        let body = json!({ "php": "8.3", "secure": false });
        app.list("PATCH", "/v1/sites/api", Some(body)).await;
        // A linked `shop` hides the parked one.
        app.list("POST", "/v1/sites/link", Some(link_body("shop", &api)))
            .await;
        let mut events = app.daemon.events.subscribe();

        let list = app.list("DELETE", "/v1/sites/api", None).await;
        assert!(list.sites.iter().all(|site| site.name != "api"), "{list:?}");
        assert_eq!(events.try_recv().unwrap(), Event::SitesChanged(list));
        // The parked `shop` is served again.
        let list = app.list("DELETE", "/v1/sites/shop", None).await;
        assert_eq!(site(&list, "shop").root, code.join("shop"));
        let file = config::load_sites(&app.sites_toml()).unwrap();
        assert!(file.sites.is_empty(), "{file:?}");
        assert_eq!(file.parked, [code]);

        // A linked folder that is gone can still be unlinked.
        let gone = app.dir.path().join("gone");
        fs::create_dir(&gone).unwrap();
        app.list("POST", "/v1/sites/link", Some(link_body("gone", &gone)))
            .await;
        fs::remove_dir(&gone).unwrap();
        let list = app.daemon.rebuild_sites().await.unwrap();
        assert!(list.skipped.iter().any(|folder| folder.path == gone));
        let list = app.list("DELETE", "/v1/sites/gone", None).await;
        assert!(list.skipped.iter().all(|folder| folder.path != gone));
    }

    #[tokio::test]
    async fn unlink_refuses_a_parked_site_and_an_unknown_name() {
        let app = App::new();
        let code = app.code();
        app.park(&code).await;
        // `shop` gets a table of its own: its settings, not a link.
        let body = json!({ "secure": false });
        app.list("PATCH", "/v1/sites/shop", Some(body)).await;
        let before = fs::read_to_string(app.sites_toml()).unwrap();
        let mut events = app.daemon.events.subscribe();

        let conflict = (StatusCode::CONFLICT, "site_is_parked".to_owned());
        for name in ["blog", "shop"] {
            let got = app
                .refused("DELETE", &format!("/v1/sites/{name}"), None)
                .await;
            assert_eq!(got, conflict, "{name}");
        }
        let (_, json) = app.answer("DELETE", "/v1/sites/blog", None).await;
        let message = json["error"]["message"].as_str().unwrap();
        let named = message.contains("forget") && message.contains(&code.display().to_string());
        assert!(named, "{message}");
        let got = app.refused("DELETE", "/v1/sites/nope", None).await;
        assert_eq!(got, (StatusCode::NOT_FOUND, "site_not_found".to_owned()));

        assert_eq!(fs::read_to_string(app.sites_toml()).unwrap(), before);
        assert!(events.try_recv().is_err(), "a refusal changes nothing");
    }

    #[tokio::test]
    async fn park_and_link_are_site_names_for_delete_too() {
        let app = App::new();
        let code = app.code();
        fs::create_dir(code.join("park")).unwrap();
        app.park(&code).await;
        // No body: the site `park`, which is parked.
        let got = app.refused("DELETE", "/v1/sites/park", None).await;
        assert_eq!(got, (StatusCode::CONFLICT, "site_is_parked".to_owned()));

        let work = app.dir.path().join("work");
        fs::create_dir(&work).unwrap();
        for name in ["park", "link"] {
            app.list("POST", "/v1/sites/link", Some(link_body(name, &work)))
                .await;
            let list = app.list("DELETE", &format!("/v1/sites/{name}"), None).await;
            assert!(
                list.sites.iter().all(|s| s.root != work),
                "{name}: {list:?}"
            );
        }
        assert_eq!(app.list("GET", "/v1/sites", None).await.parked, [code]);
        assert!(
            config::load_sites(&app.sites_toml())
                .unwrap()
                .sites
                .is_empty()
        );
    }

    #[tokio::test]
    async fn forget_removes_the_folder_its_sites_and_their_settings() {
        let app = App::new();
        let code = app.code();
        let other = app.dir.path().join("other");
        fs::create_dir_all(other.join("tools")).unwrap();
        app.park(&code).await;
        app.park(&other).await;
        let body = json!({ "secure": false });
        app.list("PATCH", "/v1/sites/shop", Some(body)).await;
        // Linked by hand under another name: it stays.
        let admin = link_body("admin", &code.join("blog"));
        app.list("POST", "/v1/sites/link", Some(admin)).await;
        let mut events = app.daemon.events.subscribe();

        let body = json!({ "path": code });
        let list = app.list("DELETE", "/v1/sites/park", Some(body)).await;
        assert_eq!(
            events.try_recv().unwrap(),
            Event::SitesChanged(list.clone())
        );
        assert_eq!(list.parked, [other]);
        let names: Vec<_> = list.sites.iter().map(|site| site.name.as_str()).collect();
        assert_eq!(names, ["admin", "tools"]);
        assert!(list.skipped.is_empty(), "{list:?}");
        let file = config::load_sites(&app.sites_toml()).unwrap();
        assert_eq!(file.sites.keys().collect::<Vec<_>>(), ["admin"]);
    }

    #[tokio::test]
    async fn forget_matches_the_folder_as_park_stored_it() {
        let app = App::new();
        let code = app.code();
        let text = code.to_str().unwrap();
        let sep = std::path::MAIN_SEPARATOR;
        let mut given = vec![
            format!("{text}{sep}"),
            format!("{text}{sep}{sep}"),
            format!("{text}{sep}."),
            format!("{text}\r\n"),
            // `..` is resolved by the system, for a folder that exists.
            format!("{text}{sep}shop{sep}.."),
        ];
        // Another letter case, where the file system ignores case (macOS
        // and Windows by default).
        let upper = code.with_file_name("CODE");
        if upper.is_dir() {
            given.push(upper.to_str().unwrap().to_owned());
        }
        for path in given {
            app.park(&code).await;
            let body = json!({ "path": path });
            let list = app.list("DELETE", "/v1/sites/park", Some(body)).await;
            assert!(list.parked.is_empty(), "{path:?}");
        }

        // A folder that is gone is matched as it was stored.
        app.park(&code).await;
        fs::remove_dir_all(&code).unwrap();
        let body = json!({ "path": code });
        let list = app.list("DELETE", "/v1/sites/park", Some(body)).await;
        assert!(list.parked.is_empty());
    }

    #[tokio::test]
    async fn forget_refuses_a_folder_that_is_not_parked() {
        let app = App::new();
        let code = app.code();
        app.park(&code).await;
        let before = fs::read_to_string(app.sites_toml()).unwrap();
        let mut events = app.daemon.events.subscribe();

        let (bad, missing) = (StatusCode::BAD_REQUEST, StatusCode::NOT_FOUND);
        for (body, status, want) in [
            (
                json!({ "path": code.join("shop") }),
                missing,
                "folder_not_parked",
            ),
            (
                json!({ "path": app.dir.path() }),
                missing,
                "folder_not_parked",
            ),
            (json!({ "path": "code" }), bad, "site_path_relative"),
            (json!({}), StatusCode::UNPROCESSABLE_ENTITY, "invalid_body"),
        ] {
            let got = app
                .refused("DELETE", "/v1/sites/park", Some(body.clone()))
                .await;
            assert_eq!(got, (status, want.to_owned()), "{body}");
        }
        // A body that is not JSON.
        let request = Request::builder()
            .method("DELETE")
            .uri("/v1/sites/park")
            .header("host", daemon_authority(PORT))
            .header("authorization", &app.bearer)
            .header("content-type", "text/plain")
            .body(Body::from("x"))
            .unwrap();
        let response = app.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        assert_eq!(fs::read_to_string(app.sites_toml()).unwrap(), before);
        assert!(events.try_recv().is_err(), "a refusal changes nothing");
    }
}
