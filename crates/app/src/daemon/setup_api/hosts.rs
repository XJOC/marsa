//! `GET /v1/hosts` and `POST /v1/hosts/sync`: Linux puts `.test` names in
//! the hosts file where the `marsa0` link cannot be used (ADR 0014, task
//! M4-18k2).
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
//! (manager decision, task M4-18k2). A site that the folder watcher finds
//! waits in `missing` until then, and `marsa doctor` and the window say so.
//!
//! The sync shares [`Setup`]: its runner, its checks of the helper file,
//! and its one-at-a-time flag, so it never runs next to a setup or an
//! uninstall (409 `setup_running` or `uninstall_running`). The way and the
//! block are read again for each request ([`linux_dns::status`]). With no
//! site missing, nothing runs and no prompt shows. The request carries the
//! name of every site, so names of removed sites leave the block too. The
//! error codes are setup's (`setup_cancelled`, and so on).
//!
//! On macOS and Windows `used` is `false`, and the sync answers 409
//! `hosts_not_used`; so does Linux with the link.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use marsa_core::helper::HelperOp;
use marsa_core::types::{HostsStatus, SetupReport};

use super::super::api::HttpError;
use super::super::state::Daemon;
use super::{Flow, Running, Setup, host_names, is_linux, linux_dns};

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
    if !setup.platform().is_some_and(is_linux) {
        return Ok(Json(HostsStatus::default()));
    }
    let job = tokio::task::spawn_blocking(move || read_status(&daemon, &setup));
    job.await.map(Json).map_err(|err| {
        tracing::error!(error = %err, "the hosts job failed");
        internal_error()
    })
}

async fn sync(daemon: Arc<Daemon>, setup: Arc<Setup>) -> Result<Json<SetupReport>, HttpError> {
    let platform = setup
        .platform()
        .filter(|platform| is_linux(*platform))
        .ok_or_else(not_used)?;
    let running = Running::take(&setup, Flow::Setup).map_err(Flow::busy)?;
    let job = tokio::task::spawn_blocking(move || {
        // Moved in, so a setup waits for this sync to end even if the client
        // goes away.
        let _running = running;
        let status = read_status(&daemon, &setup);
        if !status.used {
            return Err(not_used());
        }
        let mut report = SetupReport::default();
        if status.missing.is_empty() {
            return Ok(report);
        }
        tracing::info!(missing = ?status.missing, "adding sites to the hosts file");
        let names = host_names(&daemon);
        let ops = vec![HelperOp::HostsSync { names }];
        match setup.launch(Flow::Setup, ops, platform, &daemon.shutdown) {
            Ok(results) => report.results = results,
            Err(error) => report.error = Some(error),
        }
        Ok(report)
    });
    let report = job.await.map_err(|err| {
        tracing::error!(error = %err, "the hosts job failed");
        internal_error()
    })??;
    Ok(Json(report))
}

/// [`linux_dns::status`] for the daemon's sites. Blocking.
fn read_status(daemon: &Daemon, setup: &Setup) -> HostsStatus {
    let names = host_names(daemon);
    linux_dns::status(setup.runner(), setup.hosts_file(), &names, &daemon.shutdown)
}

fn not_used() -> HttpError {
    let message = "this system does not put .test names in the hosts file";
    HttpError::new(StatusCode::CONFLICT, "hosts_not_used", message)
}

fn internal_error() -> HttpError {
    let message = "the hosts job failed";
    HttpError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
}

#[cfg(test)]
mod tests {
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

    use super::super::tests::{
        DEBIAN_12, Fake, Systemd, UBUNTU_2204_SERVER, UBUNTU_2404, answer_for, exit, linux_fake_on,
        make_helper,
    };
    use super::*;
    use crate::daemon::auth::{AuthConfig, require_auth};

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
            make_helper(&dir.path().join("bin").join("marsa-helper"));
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
                Some(self.dir.path().join("bin").join("marsa-helper")),
                Some(self.dir.path().join("home")),
            )
            .with_hosts_file(self.hosts())
        }

        fn hosts(&self) -> PathBuf {
            self.dir.path().join("hosts")
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

    fn done_fake(system: Systemd) -> Arc<Fake> {
        linux_fake_on(system, |request| answer_for(&request, &[]))
    }

    /// The hosts way: the sites the block lacks. Reading shows no prompt.
    #[tokio::test]
    async fn the_status_lists_the_sites_the_block_lacks() {
        for system in [UBUNTU_2204_SERVER, DEBIAN_12] {
            let place = Place::new(&["blog", "shop"], BLOCK);
            let fake = done_fake(system);
            let app = place.app(&fake, Platform::LinuxX64);
            assert_eq!(app.status().await, missing(&["shop.test"]), "{system:?}");
            assert_eq!(fake.pkexec_calls(), 0);
        }
        // No block yet: every site is missing.
        let place = Place::new(&["blog", "shop"], "127.0.0.1 localhost\n");
        let app = place.app(&done_fake(UBUNTU_2204_SERVER), Platform::LinuxX64);
        assert_eq!(app.status().await, missing(&["blog.test", "shop.test"]));
    }

    /// The link: nothing to do, unless the helper chose the hosts file at
    /// the last setup and left a block (task M4-18k2a).
    #[tokio::test]
    async fn the_link_uses_the_hosts_file_only_if_a_block_is_there() {
        let place = Place::new(&["blog", "shop"], "127.0.0.1 localhost\n");
        let app = place.app(&done_fake(UBUNTU_2404), Platform::LinuxX64);
        assert_eq!(app.status().await, HostsStatus::default());

        let place = Place::new(&["blog", "shop"], BLOCK);
        let app = place.app(&done_fake(UBUNTU_2404), Platform::LinuxX64);
        assert_eq!(app.status().await, missing(&["shop.test"]));
    }

    /// macOS and Windows run nothing.
    #[tokio::test]
    async fn other_systems_do_not_use_it() {
        for platform in [Platform::DarwinArm64, Platform::WindowsX64] {
            let place = Place::new(&["shop"], "");
            let fake = Fake::answering(|program, _| panic!("{program} ran"));
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

    /// One prompt, with every site's name; the file itself is the helper's.
    #[tokio::test]
    async fn the_sync_sends_every_name_once() {
        let place = Place::new(&["shop", "blog"], BLOCK);
        let fake = done_fake(UBUNTU_2204_SERVER);
        let app = place.app(&fake, Platform::LinuxX64);
        let (status, json) = app.sync().await;
        assert_eq!(status, StatusCode::OK, "{json}");
        let report: SetupReport = serde_json::from_value(json).unwrap();
        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].op, "hosts_sync");
        assert_eq!(report.error, None);
        assert_eq!(fake.pkexec_calls(), 1);
        let names = vec!["blog.test".to_owned(), "shop.test".to_owned()];
        assert_eq!(
            fake.pkexec_request(),
            HelperRequest::new(vec![HelperOp::HostsSync { names }])
        );
        assert_eq!(fs::read_to_string(place.hosts()).unwrap(), BLOCK);
    }

    /// Nothing missing, or the link: no prompt.
    #[tokio::test]
    async fn no_missing_site_means_no_prompt() {
        let place = Place::new(&["blog"], BLOCK);
        let fake = done_fake(UBUNTU_2204_SERVER);
        let (status, json) = place.app(&fake, Platform::LinuxX64).sync().await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(
            serde_json::from_value::<SetupReport>(json).unwrap(),
            SetupReport::default()
        );
        assert_eq!(fake.pkexec_calls(), 0);

        let place = Place::new(&["blog"], "");
        let fake = done_fake(UBUNTU_2404);
        let (status, json) = place.app(&fake, Platform::LinuxX64).sync().await;
        assert_eq!(
            (status, code(&json)),
            (StatusCode::CONFLICT, "hosts_not_used".to_owned())
        );
        assert_eq!(fake.pkexec_calls(), 0);
    }

    /// A dismissed prompt is in the report, as for setup.
    #[tokio::test]
    async fn a_dismissed_prompt_is_the_error_of_the_report() {
        let place = Place::new(&["shop"], "");
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
    async fn the_sync_waits_for_no_setup() {
        let place = Place::new(&["shop"], "");
        let fake = done_fake(UBUNTU_2204_SERVER);
        let setup = Arc::new(place.setup(&fake, Platform::LinuxX64));
        let running = Running::take(&setup, Flow::Setup).unwrap();
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
            (StatusCode::CONFLICT, "setup_running".to_owned())
        );
        drop(running);
        assert_eq!(app.sync().await.0, StatusCode::OK);
        assert_eq!(fake.pkexec_calls(), 1);
    }

    /// The client waits longer than the slowest sync: four `systemctl`
    /// checks, the prompt, and the helper's pipes.
    #[test]
    fn the_client_waits_longer_than_the_slowest_sync() {
        let slowest = 4 * super::super::CHECK_LIMIT
            + super::super::PROMPT_LIMIT
            + 5 * super::super::PIPE_GRACE;
        assert!(marsa_core::client::HOSTS_SYNC_TIMEOUT > slowest);
    }

    /// The rejection paths (AGENTS.md section 5): nothing runs, not even
    /// `systemctl`.
    #[tokio::test]
    async fn a_request_without_the_token_with_an_origin_or_another_host_runs_nothing() {
        let place = Place::new(&["shop"], "");
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
