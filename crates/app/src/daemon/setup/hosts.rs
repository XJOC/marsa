//! `GET /v1/hosts` and `POST /v1/hosts/sync`: Linux puts `.test` names in
//! the hosts file where the `marsa0` link cannot be used (ADR 0014).
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/hosts` | `HostsStatus`: whether the hosts file is used, and the sites missing from Marsa's block |
//! | `POST /v1/hosts/sync` | `SetupReport`: the helper's `hosts_sync` result, or in `error` why it did not run |
//!
//! The hosts file has no wildcards, so each new site needs a line, and only
//! root writes it. **Only a command of the user calls the sync**: `marsa
//! park`, `marsa link`, or a click in the window. The daemon never calls it
//! by itself, so no password prompt shows that the user did not cause
//! (manager decision). A site that the folder watcher finds
//! waits in `missing` until then, and `marsa doctor` and the window say so.
//!
//! The hosts file counts as used only where **this data folder's setup**
//! wrote Marsa's block, and the block is still there ([`linux_dns::status`],
//! [`linux_dns::marker`]). Before the first `marsa setup`, on the link way,
//! and in a test's or a scratch data folder, `used` is `false`: nothing
//! syncs and no prompt shows (review of #132). Setup then writes every name.
//!
//! The sync shares [`Setup`]: its runner, its checks of the helper file,
//! and its one-at-a-time flag, so it never runs next to a setup or an
//! uninstall (409 `setup_running` or `uninstall_running`). The marker and
//! the block are read again for each request; no program runs to read
//! them. With no site missing, nothing runs and no prompt shows. The sync
//! only adds ([`linux_dns::names_to_add`]): a site whose folder cannot be
//! read for a moment keeps its line; `marsa setup` takes out the names of
//! removed sites. The error codes are setup's (`setup_cancelled`, and so
//! on).
//!
//! On macOS and Windows `used` is `false`, and the sync answers 409
//! `hosts_not_used`; so does Linux where the block is not this folder's.

use marsa_core::codes;
use marsa_core::types::OpName;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use marsa_core::dns;
use marsa_core::helper::HelperOp;
use marsa_core::recipes::Os;
use marsa_core::types::{HostsStatus, SetupReport, SetupSkip};

use super::super::http;
use super::super::http::routes::HttpError;
use super::super::state::Daemon;
use super::{Flow, Running, Setup, linux_dns, names_of};

/// The routes on `setup`, merged into setup's own.
pub fn routes(setup: Arc<Setup>) -> Router<Arc<Daemon>> {
    let read = {
        let setup = Arc::clone(&setup);
        move |State(daemon): State<Arc<Daemon>>| {
            let setup = Arc::clone(&setup);
            async move { status(daemon, setup).await }
        }
    };
    let sync = move |State(daemon): State<Arc<Daemon>>| {
        let setup = Arc::clone(&setup);
        async move { sync(daemon, setup).await }
    };
    Router::new()
        .route("/v1/hosts", get(read))
        .route("/v1/hosts/sync", post(sync))
}

async fn status(daemon: Arc<Daemon>, setup: Arc<Setup>) -> Result<Json<HostsStatus>, HttpError> {
    if !setup
        .platform()
        .is_some_and(|platform| platform.os() == Os::Linux)
    {
        return Ok(Json(HostsStatus::default()));
    }
    let names = site_names(&daemon).await?;
    http::blocking("hosts", move || Ok(read_status(&daemon, &setup, &names)))
        .await
        .map(Json)
}

async fn sync(daemon: Arc<Daemon>, setup: Arc<Setup>) -> Result<Json<SetupReport>, HttpError> {
    let platform = setup
        .platform()
        .filter(|platform| platform.os() == Os::Linux)
        .ok_or_else(not_used)?;
    let running = Running::take(&setup, Flow::Setup).map_err(Flow::busy)?;
    let names = site_names(&daemon).await?;
    let report = http::blocking("hosts", move || {
        // Moved in, so a setup waits for this sync to end even if the client
        // goes away.
        let _running = running;
        let status = read_status(&daemon, &setup, &names);
        if !status.used {
            return Err(not_used());
        }
        let mut report = SetupReport::default();
        if status.missing.is_empty() {
            return Ok(report);
        }
        let names = linux_dns::names_to_add(setup.hosts_file(), &names);
        // A request the helper refuses as a whole would change nothing
        // either, after a password for nothing.
        if !dns::hosts_names_fit(&names) {
            let skipped = SetupSkip {
                op: OpName::HostsSync.to_owned(),
                code: codes::HOSTS_TOO_MANY_NAMES.to_owned(),
            };
            report.skipped.push(skipped);
            return Ok(report);
        }
        tracing::info!(missing = ?status.missing, "adding sites to the hosts file");
        let ops = vec![HelperOp::HostsSync { names }];
        match setup.launch(Flow::Setup, ops, platform, &daemon.shutdown) {
            Ok(results) => report.results = results,
            Err(error) => report.error = Some(error),
        }
        Ok(report)
    })
    .await?;
    Ok(Json(report))
}

/// The host names of the daemon's sites, from the list `GET /v1/sites`
/// gives: built now if there is none yet, with that route's errors.
async fn site_names(daemon: &Arc<Daemon>) -> Result<Vec<String>, HttpError> {
    let list = match daemon.sites() {
        Some(list) => list,
        None => daemon.rebuild_sites().await?,
    };
    Ok(names_of(&list))
}

/// [`linux_dns::status`] for `names`. Blocking: it reads the hosts file.
fn read_status(daemon: &Daemon, setup: &Setup, names: &[String]) -> HostsStatus {
    linux_dns::status(&daemon.data_dir, setup.hosts_file(), names)
}

fn not_used() -> HttpError {
    let message = "this system does not put .test names in the hosts file";
    HttpError::new(StatusCode::CONFLICT, codes::HOSTS_NOT_USED, message)
}

#[cfg(test)]
mod tests {
    use marsa_core::data_dir::DataDir;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::PoisonError;

    use axum::body::{Body, to_bytes};
    use axum::extract::Request;
    use axum::middleware::from_fn_with_state;
    use marsa_core::helper::HelperRequest;
    use marsa_core::platform::daemon_file::{Token, daemon_authority};
    use marsa_core::recipes::Platform;
    use marsa_core::sites::{Site, SiteKind};
    use marsa_core::types::{ErrorBody, SiteList};
    use serde_json::Value;
    use tempfile::TempDir;
    use tower::ServiceExt as _;

    use super::super::test_support::{
        Fake, Systemd, UBUNTU_2204_SERVER, answer_for, exit, linux_fake_on, make_helper,
    };
    use super::*;
    use crate::daemon::http::auth::{AuthConfig, require_auth};

    const PORT: u16 = 49_321;

    /// A data folder with a helper, a hosts file, and a daemon whose sites
    /// are `names` (without `.test`).
    struct Place {
        dir: TempDir,
        daemon: Arc<Daemon>,
    }

    impl Place {
        fn new(names: &[&str], hosts: &str) -> Self {
            let dir = TempDir::new().unwrap();
            make_helper(&DataDir::new(dir.path()).bin().join("marsa-helper"));
            fs::write(dir.path().join("hosts"), hosts).unwrap();
            let daemon = Arc::new(Daemon::new(dir.path().to_path_buf()));
            let sites = names.iter().map(|name| site(name)).collect();
            let list = SiteList {
                parked: Vec::new(),
                sites,
                skipped: Vec::new(),
            };
            daemon
                .state
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .sites = Some(list);
            Self { dir, daemon }
        }

        fn setup(&self, fake: &Arc<Fake>, platform: Platform) -> Setup {
            Setup::with(
                fake.clone(),
                Some(platform),
                Some(DataDir::new(self.dir.path()).bin().join("marsa-helper")),
                Some(self.dir.path().join("home")),
            )
            .with_hosts_file(self.hosts())
        }

        fn hosts(&self) -> PathBuf {
            self.dir.path().join("hosts")
        }

        /// This data folder's setup wrote the block.
        fn marked(self) -> Self {
            fs::write(linux_dns::marker(self.dir.path()), "").unwrap();
            self
        }

        fn app(&self, fake: &Arc<Fake>, platform: Platform) -> App {
            App::new(Arc::clone(&self.daemon), self.setup(fake, platform))
        }
    }

    fn site(name: &str) -> Site {
        Site {
            name: name.to_owned(),
            root: Path::new("/code").join(name),
            docroot: Path::new("/code").join(name),
            kind: SiteKind::Static,
            php: None,
            php_source: None,
            secure: true,
        }
    }

    /// The routes behind the real auth layer.
    struct App {
        router: Router,
        bearer: String,
    }

    impl App {
        fn new(daemon: Arc<Daemon>, setup: Setup) -> Self {
            let token = Token::generate().unwrap();
            let bearer = token.bearer_header();
            let auth = AuthConfig::new(Arc::new(token), PORT);
            let router = routes(Arc::new(setup))
                .with_state(daemon)
                .layer(from_fn_with_state(auth, require_auth));
            Self { router, bearer }
        }

        async fn call(&self, method: &str, uri: &str) -> (StatusCode, Value) {
            let host = daemon_authority(PORT);
            let headers = [("host", host.as_str()), ("authorization", &self.bearer)];
            self.send(method, uri, &headers).await
        }

        async fn send(
            &self,
            method: &str,
            uri: &str,
            headers: &[(&str, &str)],
        ) -> (StatusCode, Value) {
            let mut request = Request::builder().method(method).uri(uri);
            for &(name, value) in headers {
                request = request.header(name, value);
            }
            let request = request.body(Body::empty()).unwrap();
            let response = self.router.clone().oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
            (status, serde_json::from_slice(&bytes).unwrap())
        }

        async fn status(&self) -> HostsStatus {
            let (status, json) = self.call("GET", "/v1/hosts").await;
            assert_eq!(status, StatusCode::OK, "{json}");
            serde_json::from_value(json).unwrap()
        }

        async fn sync(&self) -> (StatusCode, Value) {
            self.call("POST", "/v1/hosts/sync").await
        }
    }

    fn code(json: &Value) -> String {
        serde_json::from_value::<ErrorBody>(json.clone())
            .unwrap()
            .error
            .code
    }

    fn missing(list: &[&str]) -> HostsStatus {
        HostsStatus {
            used: true,
            missing: list.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    const BLOCK: &str = "127.0.0.1 localhost\n# Marsa start\n127.0.0.1 blog.test\n# Marsa end\n";
    /// What setup leaves with no site.
    const EMPTY_BLOCK: &str = "127.0.0.1 localhost\n# Marsa start\n# Marsa end\n";

    fn done_fake(system: Systemd) -> Arc<Fake> {
        linux_fake_on(system, |request| answer_for(&request, &[]))
    }

    /// A runner that must not be used.
    fn no_programs() -> Arc<Fake> {
        Fake::answering(|program, _| panic!("{program} ran"))
    }

    /// Setup put the block there for this data folder: the sites the block
    /// lacks. Reading runs no program.
    #[tokio::test]
    async fn the_status_lists_the_sites_the_block_lacks() {
        let place = Place::new(&["blog", "shop"], BLOCK).marked();
        let app = place.app(&no_programs(), Platform::LinuxX64);
        assert_eq!(app.status().await, missing(&["shop.test"]));
    }

    /// No marker (before the first setup, on the link way, a test's or a
    /// scratch data folder), or no block (another tool rewrote the file):
    /// not used, and nothing syncs (review of #132).
    #[tokio::test]
    async fn without_this_folders_block_nothing_syncs() {
        for (hosts, marked) in [(BLOCK, false), ("127.0.0.1 localhost\n", true), ("", false)] {
            let mut place = Place::new(&["blog", "shop"], hosts);
            if marked {
                place = place.marked();
            }
            let fake = no_programs();
            let app = place.app(&fake, Platform::LinuxX64);
            assert_eq!(app.status().await, HostsStatus::default(), "{hosts:?}");
            let (status, json) = app.sync().await;
            assert_eq!(
                (status, code(&json)),
                (StatusCode::CONFLICT, "hosts_not_used".to_owned()),
                "{hosts:?}"
            );
            assert!(fake.calls().is_empty());
        }
    }

    /// macOS and Windows run nothing.
    #[tokio::test]
    async fn other_systems_do_not_use_it() {
        for platform in [Platform::DarwinArm64, Platform::WindowsX64] {
            let place = Place::new(&["shop"], BLOCK).marked();
            let fake = no_programs();
            let app = place.app(&fake, platform);
            assert_eq!(app.status().await, HostsStatus::default());
            let (status, json) = app.sync().await;
            assert_eq!(
                (status, code(&json)),
                (StatusCode::CONFLICT, "hosts_not_used".to_owned())
            );
            assert!(fake.calls().is_empty());
        }
    }

    /// One prompt. The sync only adds: a name in the block that is no site
    /// now (a folder that cannot be read for a moment, say) keeps its line.
    /// The file itself is the helper's.
    #[tokio::test]
    async fn the_sync_adds_and_never_takes_out() {
        let hosts = "# Marsa start\n127.0.0.1 blog.test\n127.0.0.1 gone.test\n# Marsa end\n";
        let place = Place::new(&["shop", "blog"], hosts).marked();
        let fake = done_fake(UBUNTU_2204_SERVER);
        let app = place.app(&fake, Platform::LinuxX64);
        let (status, json) = app.sync().await;
        assert_eq!(status, StatusCode::OK, "{json}");
        let report: SetupReport = serde_json::from_value(json).unwrap();
        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].op, OpName::HostsSync);
        assert_eq!(report.error, None);
        assert_eq!(fake.programs(), ["/usr/bin/pkexec"]);
        let names = ["blog.test", "gone.test", "shop.test"]
            .map(str::to_owned)
            .to_vec();
        assert_eq!(
            fake.pkexec_request(),
            HelperRequest::new(vec![HelperOp::HostsSync { names }])
        );
        assert_eq!(fs::read_to_string(place.hosts()).unwrap(), hosts);
    }

    /// Nothing missing: no prompt.
    #[tokio::test]
    async fn no_missing_site_means_no_prompt() {
        let place = Place::new(&["blog"], BLOCK).marked();
        let fake = no_programs();
        let (status, json) = place.app(&fake, Platform::LinuxX64).sync().await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(
            serde_json::from_value::<SetupReport>(json).unwrap(),
            SetupReport::default()
        );
        assert!(fake.calls().is_empty());
    }

    /// More names than the helper takes: no prompt for a request it would
    /// refuse, and the report says why.
    #[tokio::test]
    async fn too_many_names_are_skipped_with_no_prompt() {
        let names: Vec<String> = (0..1001).map(|i| format!("s{i}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let place = Place::new(&names, EMPTY_BLOCK).marked();
        let fake = no_programs();
        let (status, json) = place.app(&fake, Platform::LinuxX64).sync().await;
        assert_eq!(status, StatusCode::OK, "{json}");
        let report: SetupReport = serde_json::from_value(json).unwrap();
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].code, "hosts_too_many_names");
        assert!(fake.calls().is_empty());
    }

    /// A dismissed prompt is in the report, as for setup.
    #[tokio::test]
    async fn a_dismissed_prompt_is_the_error_of_the_report() {
        let place = Place::new(&["shop"], EMPTY_BLOCK).marked();
        let fake = linux_fake_on(UBUNTU_2204_SERVER, |_| exit(126, "Request dismissed"));
        let (status, json) = place.app(&fake, Platform::LinuxX64).sync().await;
        assert_eq!(status, StatusCode::OK, "{json}");
        let report: SetupReport = serde_json::from_value(json).unwrap();
        let error = report.error.map(|error| error.code);
        assert_eq!(error.as_deref(), Some("setup_cancelled"));
        assert!(report.results.is_empty());
    }

    /// One at a time with setup and the uninstall.
    #[tokio::test]
    async fn the_sync_waits_for_no_setup_and_no_uninstall() {
        for (flow, busy) in [
            (Flow::Setup, "setup_running"),
            (Flow::Uninstall, "uninstall_running"),
        ] {
            let place = Place::new(&["shop"], EMPTY_BLOCK).marked();
            let fake = done_fake(UBUNTU_2204_SERVER);
            let setup = Arc::new(place.setup(&fake, Platform::LinuxX64));
            let running = Running::take(&setup, flow).unwrap();
            let token = Token::generate().unwrap();
            let bearer = token.bearer_header();
            let auth = AuthConfig::new(Arc::new(token), PORT);
            let router: Router = routes(Arc::clone(&setup))
                .with_state(Arc::clone(&place.daemon))
                .layer(from_fn_with_state(auth, require_auth));
            let app = App { router, bearer };
            let (status, json) = app.sync().await;
            assert_eq!(
                (status, code(&json)),
                (StatusCode::CONFLICT, busy.to_owned())
            );
            drop(running);
            assert_eq!(app.sync().await.0, StatusCode::OK);
            assert_eq!(fake.pkexec_calls(), 1);
        }
    }

    /// The client waits longer than the slowest sync: the prompt and the
    /// helper's pipes. No other program runs.
    #[test]
    fn the_client_waits_longer_than_the_slowest_sync() {
        let slowest = super::super::PROMPT_LIMIT + super::super::PIPE_GRACE;
        assert!(marsa_core::client::HOSTS_SYNC_TIMEOUT > slowest);
    }

    /// The rejection paths (AGENTS.md section 5): nothing runs.
    #[tokio::test]
    async fn a_request_without_the_token_with_an_origin_or_another_host_runs_nothing() {
        let place = Place::new(&["shop"], EMPTY_BLOCK).marked();
        let fake = done_fake(UBUNTU_2204_SERVER);
        let app = place.app(&fake, Platform::LinuxX64);
        let host = daemon_authority(PORT);
        let other_host = daemon_authority(PORT + 1);
        let bearer = app.bearer.clone();
        let wrong = "Bearer 0000000000000000000000000000000000000000000000000000000000000000";
        let cases = [
            (vec![("host", host.as_str())], StatusCode::UNAUTHORIZED),
            (
                vec![("host", host.as_str()), ("authorization", wrong)],
                StatusCode::UNAUTHORIZED,
            ),
            (
                vec![
                    ("host", host.as_str()),
                    ("authorization", bearer.as_str()),
                    ("origin", "http://evil.test"),
                ],
                StatusCode::FORBIDDEN,
            ),
            (
                vec![
                    ("host", other_host.as_str()),
                    ("authorization", bearer.as_str()),
                ],
                StatusCode::FORBIDDEN,
            ),
        ];
        for (method, uri) in [("GET", "/v1/hosts"), ("POST", "/v1/hosts/sync")] {
            for (headers, status) in &cases {
                let (got, _) = app.send(method, uri, headers).await;
                assert_eq!(got, *status, "{method} {uri} {headers:?}");
            }
        }
        assert!(fake.calls().is_empty());
    }
}
