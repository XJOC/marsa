//! Typed client for the daemon API, used by the CLI and the window
//! (architecture.md section 6).
//!
//! [`ensure_running`] finds the daemon, starts `marsad` if needed, and checks
//! its version. Every call then goes to `http://127.0.0.1:<port>` with the
//! bearer token from `run/daemon.json`.
//!
//! `run/daemon.lock` is the source of truth: the running daemon holds it, and
//! the OS frees it when the daemon exits, even after a crash. While the lock
//! is free, a `daemon.json` left by a crash is never contacted. One short
//! window remains: a new daemon holds the lock but has not replaced the old
//! file yet. A probe then may reach the old port with the old token, which
//! is dead (every start makes a new one).
//!
//! The token leaves this module only in the `Authorization` header, which is
//! marked sensitive. [`DaemonClient`] prints its port only, no proxy is ever
//! used (`HTTP_PROXY` would receive the header), redirects are not followed,
//! and no error message holds the token.

mod error;
mod lifecycle;
mod sse;

pub use error::ClientError;
pub use lifecycle::{
    START_WAIT, ensure_running, ensure_running_with, is_daemon_running, stop_daemon,
};
pub use sse::{EventStream, StreamItem};

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::{self, HeaderMap, HeaderValue};
use reqwest::{Method, RequestBuilder, Response, Url, redirect};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::time::timeout;

use crate::platform::daemon_file::{
    DaemonFile, daemon_authority, daemon_lock_path, read_daemon_file,
};
use crate::runtimes::Runtime;
use crate::types::{
    AutostartRequest, AutostartState, DefaultRequest, DefaultVersion, ErrorBody, Event, Health,
    HostsStatus, InstallAccepted, InstallRequest, LinkRequest, LogLine, ParkRequest, RuntimeList,
    ServiceAction, ServiceStatus, SetupReport, SiteList, SitePatch, UninstallReport, Uninstalled,
    XdebugRequest, XdebugState,
};
use lifecycle::lock_is_held;

/// This build's version. An older daemon is replaced.
pub(super) const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// `GET /v1/health` answers at once; this also bounds each handshake step.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
/// For one call that is not a stream, and for the start of a stream.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// For `POST /v1/services/{id}/{action}`. The daemon waits up to 20 s for
/// another action on the service, then runs this one; a restart that stops
/// an old Caddy first takes about 23 s. About 43 s in all.
const SERVICE_ACTION_TIMEOUT: Duration = Duration::from_secs(60);
/// The same for PostgreSQL (`postgres`). The daemon waits up to
/// 30 s for another action, a restart stops it first (SIGINT and SIGQUIT,
/// up to about 23 s), and a first start reads the port (lsof, up to 8 s),
/// runs `initdb` (up to 120 s) and single-user mode (up to 60 s), waits up
/// to 15 s for the server, and stops one that never answered (23 s): about
/// 279 s in all. MySQL (`mysql`) takes less: its first init (up to 120 s)
/// is one step, and its stop is SIGTERM, then a kill after 30 s. Valkey
/// (`valkey`) has no init, but its stop (`SHUTDOWN SAVE`, then SIGTERM) may
/// take 29 s, twice in a restart whose start never answered: more than the
/// 60 s of the other services.
const SERVER_ACTION_TIMEOUT: Duration = Duration::from_secs(300);
/// For `DELETE /v1/runtimes/{tool}/{version}`: a Node.js folder holds
/// thousands of files, and a virus scanner may slow their delete.
const UNINSTALL_TIMEOUT: Duration = Duration::from_secs(120);
/// For `POST /v1/setup`: longer than the daemon's slowest setup, which is
/// about 12.5 minutes when every step runs to its limit. The daemon gives
/// the user 5 minutes at the admin prompt and 5 more at the trust dialog
/// (macOS), each check (`netstat`, `ps` for at most 8 processes, `security
/// verify-cert`) up to 10 seconds, and Caddy 30 seconds to start and make
/// its CA when there is none yet. A test in the daemon's `setup_api/`
/// holds this above the sum of the daemon's own limits.
///
/// On Windows the slowest setup is longer: a request to the helper service,
/// the install after the UAC prompt, and the request again, up to 35.5
/// minutes, and the prompt has no limit of its own.
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(40 * 60);
/// For `POST /v1/hosts/sync`: longer than the daemon's slowest sync, 5
/// minutes at the password prompt and 2 seconds for the helper's pipes. A
/// test in the daemon's `setup_api/hosts.rs` holds this above the sum of
/// the daemon's own limits.
pub const HOSTS_SYNC_TIMEOUT: Duration = Duration::from_secs(6 * 60);
/// For `POST /v1/uninstall`: longer than the daemon's slowest uninstall,
/// about 15 minutes and 20 seconds when every program runs to its limit. The
/// user has 5 minutes at the admin prompt, then up to 5 at each of the two
/// dialogs macOS may show to take the user's trust away and to delete the
/// certificate; the keychain listing gets 10 seconds. A test in the daemon's
/// `uninstall_api.rs` holds this above the sum of the daemon's own limits.
///
/// On Windows the slowest uninstall is the helper's run after the UAC
/// prompt, up to 15 minutes, and the prompt has no limit of its own: 40
/// minutes leaves the user time to find it, as for setup.
pub const UNINSTALL_ALL_TIMEOUT: Duration = Duration::from_secs(40 * 60);
/// The largest JSON body read. Log lines are the biggest answer.
const MAX_BODY: usize = 16 * 1024 * 1024;
const MIB: usize = 1024 * 1024;

/// A connection to a running `marsad`. Cheap to clone: clones share one
/// connection pool.
#[derive(Clone)]
pub struct DaemonClient {
    http: reqwest::Client,
    /// `http://127.0.0.1:<port>/`.
    base: Url,
    port: u16,
}

/// The port only. The token sits in the `reqwest::Client` headers, which this
/// never prints.
impl fmt::Debug for DaemonClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DaemonClient")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl DaemonClient {
    /// A client for the daemon that holds `<data_dir>/run/daemon.lock`, from
    /// its `run/daemon.json`. Sends nothing: call [`DaemonClient::health`] to
    /// see if it answers.
    ///
    /// # Errors
    ///
    /// `daemon_not_running` if no daemon holds the lock or there is no file,
    /// a `daemon_file_*` code if the file cannot be used, and
    /// `daemon_unreachable` if the HTTP client cannot be built.
    pub fn connect(data_dir: &Path) -> Result<Self, ClientError> {
        let run = run_dir(data_dir);
        if !lock_is_held(&daemon_lock_path(&run)) {
            return Err(ClientError::NotRunning);
        }
        Self::from_file(&read_daemon_file(&run)?)
    }

    fn from_file(file: &DaemonFile) -> Result<Self, ClientError> {
        let port = file.port;
        let mut auth = HeaderValue::try_from(file.token.bearer_header())
            .expect("`Bearer ` and 64 hex digits form a valid header value");
        // Keeps the value out of `Debug` output, in `http` and in `reqwest`.
        auth.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, auth);
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .default_headers(headers)
            .build()
            .map_err(|source| ClientError::Unreachable {
                port,
                source: Box::new(source),
            })?;
        // The same authority the auth middleware wants in `Host`; reqwest
        // sends it from this URL.
        let base = Url::parse(&format!("http://{}/", daemon_authority(port)))
            .expect("a loopback address with a u16 port is a valid URL");
        Ok(Self { http, base, port })
    }

    /// The daemon's port on `127.0.0.1`.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `GET /v1/health`, with a 5 second limit.
    pub async fn health(&self) -> Result<Health, ClientError> {
        self.call(Method::GET, &["v1", "health"], HEALTH_TIMEOUT)
            .await
    }

    /// `POST /v1/shutdown`. Returns once the daemon accepted; it stops soon
    /// after. [`stop_daemon`] also waits until it has stopped.
    pub async fn shutdown(&self) -> Result<(), ClientError> {
        let request = self.http.post(self.url(&["v1", "shutdown"])?);
        self.send(request.timeout(REQUEST_TIMEOUT)).await?;
        Ok(())
    }

    /// `GET /v1/services`.
    pub async fn services(&self) -> Result<Vec<ServiceStatus>, ClientError> {
        self.call(Method::GET, &["v1", "services"], REQUEST_TIMEOUT)
            .await
    }

    /// `POST /v1/services/{id}/{action}`. The status after the action.
    pub async fn service_action(
        &self,
        id: &str,
        action: ServiceAction,
    ) -> Result<ServiceStatus, ClientError> {
        let path = ["v1", "services", id, action.as_str()];
        self.call(Method::POST, &path, action_timeout(id)).await
    }

    /// `GET /v1/logs/{id}`: the recent lines.
    pub async fn logs(&self, id: &str) -> Result<Vec<LogLine>, ClientError> {
        self.call(Method::GET, &["v1", "logs", id], REQUEST_TIMEOUT)
            .await
    }

    /// `GET /v1/logs/{id}?follow=true`: the recent lines, then new ones as
    /// they come.
    pub async fn follow_logs(&self, id: &str) -> Result<EventStream<LogLine>, ClientError> {
        let mut url = self.url(&["v1", "logs", id])?;
        url.set_query(Some("follow=true"));
        self.stream(url).await
    }

    /// `GET /v1/events`: every change, as it happens.
    pub async fn events(&self) -> Result<EventStream<Event>, ClientError> {
        self.stream(self.url(&["v1", "events"])?).await
    }

    /// `GET /v1/runtimes`: the installed versions of each runtime.
    pub async fn runtimes(&self) -> Result<RuntimeList, ClientError> {
        self.call(Method::GET, &["v1", "runtimes"], REQUEST_TIMEOUT)
            .await
    }

    /// `POST /v1/runtimes/{tool}/install`. Returns once the daemon has
    /// accepted; the progress then comes on [`DaemonClient::events`] as
    /// `Event::Install` with the id returned here. Open that stream first,
    /// so no event of this install is missed.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `install_busy` (409) while another install runs.
    pub async fn install(
        &self,
        runtime: Runtime,
        version: Option<&str>,
        force: bool,
    ) -> Result<InstallAccepted, ClientError> {
        let url = self.url(&["v1", "runtimes", runtime.dir_name(), "install"])?;
        let body = InstallRequest {
            version: version.map(str::to_owned),
            force,
        };
        let request = self.http.post(url).json(&body);
        self.answer(request.timeout(REQUEST_TIMEOUT)).await
    }

    /// `PUT /v1/runtimes/{tool}/default`: makes `version` (`8.3`, `22`) the
    /// default of PHP or Node. Returns once `global.toml` is saved and the
    /// site list is built again. The new default also comes on
    /// [`DaemonClient::events`] as `Event::DefaultChanged`, and a changed list
    /// as `Event::SitesChanged`.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; the daemon's `no_default_version` (400) for
    /// Composer and Caddy, `invalid_version` or `unsupported_alias` (400) for
    /// text that is not a version, and `version_not_installed` (409) when no
    /// installed version matches.
    pub async fn set_default(
        &self,
        runtime: Runtime,
        version: &str,
    ) -> Result<DefaultVersion, ClientError> {
        let path = ["v1", "runtimes", runtime.dir_name(), "default"];
        let body = DefaultRequest {
            version: version.to_owned(),
        };
        self.call_with(Method::PUT, &path, &body).await
    }

    /// `DELETE /v1/runtimes/{tool}/{version}`: deletes the
    /// installed version that `version` names: `8.3.12`, or `8.3` when one
    /// installed version matches it. A php-fpm pool that runs it is stopped
    /// first. Returns once the folder is gone and the site list is built
    /// again; a changed list also comes on [`DaemonClient::events`] as
    /// `Event::SitesChanged`. A refusal changes nothing. The only installed
    /// PHP or Node version goes with its default in `global.toml`.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`], and these codes from the daemon:
    ///
    /// - `version_in_use` (409) as [`ClientError::InUse`], with what uses
    ///   it: the default in `global.toml` while another version is
    ///   installed, sites set to it in `sites.toml`, or the running Caddy;
    /// - `sites_lose_php` (409) as [`ClientError::InUse`] too: it is the
    ///   only PHP installed, and the sites that run it, which would
    ///   have no PHP after it, are in `sites_left_without_php`;
    /// - `install_busy` (409) while an install or another uninstall runs;
    /// - `version_not_installed` (404) when no installed version matches;
    /// - `version_ambiguous` (409) when more than one does;
    /// - `invalid_version` or `unsupported_alias` (400) for text that is not
    ///   a version, and `unknown_tool` (404);
    /// - `uninstall_io` (500) when the folder cannot be moved aside (on
    ///   Windows: a program from it still runs);
    /// - `runtimes_read`, `config_read`, or `config_invalid` (500).
    pub async fn uninstall(
        &self,
        runtime: Runtime,
        version: &str,
    ) -> Result<Uninstalled, ClientError> {
        let path = ["v1", "runtimes", runtime.dir_name(), version];
        self.call(Method::DELETE, &path, UNINSTALL_TIMEOUT).await
    }

    /// `GET /v1/sites`: every site, sorted by name, and the folders that did
    /// not become one.
    pub async fn sites(&self) -> Result<SiteList, ClientError> {
        self.call(Method::GET, &["v1", "sites"], REQUEST_TIMEOUT)
            .await
    }

    /// `POST /v1/sites/park`: every sub-folder of `path` becomes a site. The
    /// list after the change.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `site_path_not_utf8` before any request, and the
    /// daemon's `site_path_relative` or `site_not_a_folder`.
    pub async fn park(&self, path: &Path) -> Result<SiteList, ClientError> {
        let body = ParkRequest {
            path: utf8_path(path)?,
        };
        self.call_with(Method::POST, &["v1", "sites", "park"], &body)
            .await
    }

    /// `POST /v1/sites/link`: `path` becomes the site `name`. The list after
    /// the change.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `site_path_not_utf8` before any request, and the
    /// daemon's `site_name_invalid`, `site_path_relative`, or
    /// `site_not_a_folder`.
    pub async fn link(&self, name: &str, path: &Path) -> Result<SiteList, ClientError> {
        let body = LinkRequest {
            name: name.to_owned(),
            path: utf8_path(path)?,
        };
        self.call_with(Method::POST, &["v1", "sites", "link"], &body)
            .await
    }

    /// `PATCH /v1/sites/{name}`: changes the settings in `patch`, keeps the
    /// others. The list after the change.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `site_not_found` (404) for a name with no site.
    pub async fn update_site(
        &self,
        name: &str,
        patch: &SitePatch,
    ) -> Result<SiteList, ClientError> {
        self.call_with(Method::PATCH, &["v1", "sites", name], patch)
            .await
    }

    /// `DELETE /v1/sites/{name}`: the linked site `name` is no longer served,
    /// and its own settings (PHP version, HTTPS, proxy) go with it. The list
    /// after the change. Sent without a body, so `park` is unlinked too.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; the daemon's `site_not_found` (404) for a name
    /// with no site, and `site_is_parked` (409) for a site from a parked
    /// folder: [`DaemonClient::forget`] removes those.
    pub async fn unlink(&self, name: &str) -> Result<SiteList, ClientError> {
        self.call(Method::DELETE, &["v1", "sites", name], REQUEST_TIMEOUT)
            .await
    }

    /// `DELETE /v1/sites/park` with `{"path": ...}`: `path` is no longer
    /// parked, so its sites are no longer served, and their own settings go
    /// with them. The list after the change.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `site_path_not_utf8` before any request, and the
    /// daemon's `site_path_relative` (400) and `folder_not_parked` (404).
    pub async fn forget(&self, path: &Path) -> Result<SiteList, ClientError> {
        let body = ParkRequest {
            path: utf8_path(path)?,
        };
        self.call_with(Method::DELETE, &["v1", "sites", "park"], &body)
            .await
    }

    /// `POST /v1/setup`: the first setup. On macOS the user is asked twice:
    /// for an administrator password, then to approve the trust in Marsa's
    /// certificate authority for this user. On Windows UAC may ask once.
    /// Waits until it ends, up to 40 minutes ([`SETUP_TIMEOUT`]): the user
    /// may take a while at each prompt. The
    /// same report also comes on [`DaemonClient::events`] as
    /// `Event::SetupFinished`.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `setup_running` (409) while another setup runs,
    /// and `setup_not_supported` (501) on a system Marsa has no setup for. A
    /// cancelled prompt is not an error: it is in [`SetupReport::error`].
    pub async fn setup(&self) -> Result<SetupReport, ClientError> {
        self.call(Method::POST, &["v1", "setup"], SETUP_TIMEOUT)
            .await
    }

    /// `GET /v1/hosts`: whether this system puts `.test`
    /// names in its hosts file, and which sites are not there yet. Shows no
    /// prompt.
    pub async fn hosts(&self) -> Result<HostsStatus, ClientError> {
        self.call(Method::GET, &["v1", "hosts"], REQUEST_TIMEOUT)
            .await
    }

    /// `POST /v1/hosts/sync`: the names of every site go into
    /// Marsa's block in the hosts file, behind the password prompt, if any
    /// is missing; with none missing, nothing runs. Only for a command of
    /// the user: never call it from a background task. Waits up to
    /// [`HOSTS_SYNC_TIMEOUT`]. The helper's result, or why it did not run,
    /// is in the report, as for setup.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `hosts_not_used` (409) where this system does
    /// not use the hosts file, and `setup_running` or `uninstall_running`
    /// (409) while one of those runs.
    pub async fn sync_hosts(&self) -> Result<SetupReport, ClientError> {
        self.call(Method::POST, &["v1", "hosts", "sync"], HOSTS_SYNC_TIMEOUT)
            .await
    }

    /// `POST /v1/uninstall`: one admin prompt for the helper's
    /// removals, then the user's trust and the login file. Waits until it
    /// ends, up to 40 minutes ([`UNINSTALL_ALL_TIMEOUT`]). When
    /// [`UninstallReport::stopping`] is `true` the daemon stops right after
    /// this answer: [`stop_daemon`] waits until it has.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `uninstall_running` or `setup_running` (409)
    /// while one of them runs, and `uninstall_not_supported` (501) on Linux
    /// and Windows for now. A cancelled prompt is not an error: it is in
    /// [`UninstallReport::error`].
    pub async fn uninstall_all(&self) -> Result<UninstallReport, ClientError> {
        self.call(Method::POST, &["v1", "uninstall"], UNINSTALL_ALL_TIMEOUT)
            .await
    }

    /// `GET /v1/autostart`: whether the tray icon starts at login.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `autostart_unsupported` (501) on Windows for now.
    pub async fn autostart(&self) -> Result<AutostartState, ClientError> {
        self.call(Method::GET, &["v1", "autostart"], REQUEST_TIMEOUT)
            .await
    }

    /// `PUT /v1/autostart`: the daemon writes (`true`) or deletes (`false`)
    /// the login file that starts the tray icon. It takes effect at the next
    /// login. The state after the change.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `tray_not_found` (409) when `marsa-tray` is not
    /// next to `marsad`, and `autostart_unsupported` (501) on Windows for now.
    pub async fn set_autostart(&self, enabled: bool) -> Result<AutostartState, ClientError> {
        let body = AutostartRequest { enabled };
        self.call_with(Method::PUT, &["v1", "autostart"], &body)
            .await
    }

    /// `GET /v1/php/xdebug`: whether PHP loads Xdebug, and which versions.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`].
    pub async fn xdebug(&self) -> Result<XdebugState, ClientError> {
        self.call(Method::GET, &["v1", "php", "xdebug"], REQUEST_TIMEOUT)
            .await
    }

    /// `PUT /v1/php/xdebug`: turns Xdebug on or off for every installed PHP.
    /// Returns once every `php.ini` is written and the php-fpm pools it
    /// changed have started again, so the next `php` in the terminal already
    /// runs with the new setting. The state after the change.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; a `config_*` code (500) when `global.toml` cannot
    /// be read or written.
    pub async fn set_xdebug(&self, enabled: bool) -> Result<XdebugState, ClientError> {
        let body = XdebugRequest { enabled };
        self.call_with(Method::PUT, &["v1", "php", "xdebug"], &body)
            .await
    }

    /// The URL of the path made of `segments`, so an id can never reach
    /// another route: each segment is percent-encoded (`/`, `?`, `#`, and `%`
    /// too), and an empty, `.`, or `..` segment is refused here, because the
    /// URL rules would drop it and shorten the path.
    fn url(&self, segments: &[&str]) -> Result<Url, ClientError> {
        if let Some(bad) = segments.iter().find(|s| matches!(**s, "" | "." | "..")) {
            return Err(ClientError::InvalidId {
                id: (*bad).to_owned(),
            });
        }
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("an http URL has a path")
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }

    /// A call without a body and with a JSON answer, within `limit`.
    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        segments: &[&str],
        limit: Duration,
    ) -> Result<T, ClientError> {
        let request = self.http.request(method, self.url(segments)?);
        self.answer(request.timeout(limit)).await
    }

    /// A call with a JSON body and a JSON answer, within `REQUEST_TIMEOUT`.
    async fn call_with<B: Serialize, T: DeserializeOwned>(
        &self,
        method: Method,
        segments: &[&str],
        body: &B,
    ) -> Result<T, ClientError> {
        let request = self.http.request(method, self.url(segments)?).json(body);
        self.answer(request.timeout(REQUEST_TIMEOUT)).await
    }

    /// Sends `request` and reads its JSON answer.
    async fn answer<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T, ClientError> {
        let response = self.send(request).await?;
        let body = self.read_body(response).await?;
        serde_json::from_slice(&body).map_err(|err| ClientError::bad_json("the answer", &err))
    }

    /// A call with an SSE answer. The answer must start within
    /// `REQUEST_TIMEOUT`; after that, [`EventStream`] watches for silence.
    async fn stream<T: DeserializeOwned>(&self, url: Url) -> Result<EventStream<T>, ClientError> {
        let request = self
            .http
            .get(url)
            .header(header::ACCEPT, "text/event-stream");
        let response = timeout(REQUEST_TIMEOUT, self.send(request))
            .await
            .map_err(|_| ClientError::timed_out(self.port, "no answer", REQUEST_TIMEOUT))??;
        let is_sse = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"));
        if !is_sse {
            return Err(ClientError::bad_response(
                "the answer is not an event stream",
            ));
        }
        Ok(EventStream::new(response, self.port))
    }

    /// Sends the request. A status other than 2xx becomes `Api` with the
    /// daemon's code, or `daemon_bad_response` if there is no `ErrorBody`.
    /// A redirect is such a status too: it is never followed.
    async fn send(&self, request: RequestBuilder) -> Result<Response, ClientError> {
        let response = request.send().await.map_err(|err| self.transport(err))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = self.read_body(response).await?;
        let body: ErrorBody = serde_json::from_slice(&body).map_err(|_| {
            ClientError::bad_response(format!("status {} without an error body", status.as_u16()))
        })?;
        Err(ClientError::api(status.as_u16(), &body))
    }

    async fn read_body(&self, mut response: Response) -> Result<Vec<u8>, ClientError> {
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|err| self.transport(err))? {
            if body.len() + chunk.len() > MAX_BODY {
                return Err(ClientError::bad_response(format!(
                    "the answer is larger than {} MiB",
                    MAX_BODY / MIB
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// A failed connection means no daemon on that port: it is stopping, or
    /// it has not started serving yet.
    fn transport(&self, err: reqwest::Error) -> ClientError {
        if err.is_connect() {
            tracing::debug!(port = self.port, error = %err, "no daemon answers");
            ClientError::NotRunning
        } else {
            ClientError::Unreachable {
                port: self.port,
                source: Box::new(err),
            }
        }
    }
}

/// How long the client waits for an action on the service `id`.
fn action_timeout(id: &str) -> Duration {
    match id {
        "postgres" | "mysql" | "valkey" => SERVER_ACTION_TIMEOUT,
        _ => SERVICE_ACTION_TIMEOUT,
    }
}

/// `path` as it goes into a JSON body. JSON text is UTF-8, so a path that is
/// not fails here, with its own code, before any request.
fn utf8_path(path: &Path) -> Result<PathBuf, ClientError> {
    if path.to_str().is_none() {
        return Err(ClientError::PathNotUtf8 {
            path: path.to_path_buf(),
        });
    }
    Ok(path.to_path_buf())
}

pub(super) fn run_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("run")
}

#[cfg(test)]
mod tests {

    use super::*;

    /// PostgreSQL's first start runs `initdb`, so its actions get longer.
    #[test]
    fn a_postgres_action_waits_longer_than_its_worst_case() {
        assert_eq!(action_timeout("caddy"), SERVICE_ACTION_TIMEOUT);
        // Waiting 30, stopping 23, then lsof 8, initdb 120, single-user 60,
        // ready 15, and the stop of a server that never answered, 23.
        let worst = Duration::from_secs(30 + 23 + 8 + 120 + 60 + 15 + 23);
        assert!(action_timeout("postgres") > worst);
        // Waiting 30, stopping 35, lsof 8, init 120, ready 15, stop 35.
        let worst = Duration::from_secs(30 + 35 + 8 + 120 + 15 + 35);
        assert!(action_timeout("mysql") > worst);
        // Waiting 30, stopping 29 (the request, SIGTERM, and the margin),
        // lsof 8, ready 15, stop 29.
        let worst = Duration::from_secs(30 + 29 + 8 + 15 + 29);
        assert!(action_timeout("valkey") > worst);
        assert!(
            SERVICE_ACTION_TIMEOUT < worst,
            "valkey needs the longer one"
        );
    }
}
