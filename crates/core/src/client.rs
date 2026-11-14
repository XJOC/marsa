//! Typed client for the daemon API, used by the CLI and the window
//! (architecture.md section 6). Task M3-08.
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
mod sse;

pub use error::ClientError;
pub use sse::{EventStream, StreamItem};

use std::cmp::Ordering;
use std::fmt;
use std::fs::{File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::{self, HeaderMap, HeaderValue};
use reqwest::{Method, RequestBuilder, Response, Url, redirect};
use semver::Version;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::time::{Instant, sleep, timeout, timeout_at};

use crate::platform::daemon_file::{
    DaemonFile, DaemonFileError, daemon_authority, daemon_lock_path, read_daemon_file,
};
use crate::platform::spawn::{EXIT_ALREADY_RUNNING, marsad_path, spawn_marsad};
use crate::runtimes::Runtime;
use crate::types::{
    API_VERSION, AutostartRequest, AutostartState, DefaultRequest, DefaultVersion, ErrorBody,
    Event, Health, InstallAccepted, InstallRequest, LinkRequest, LogLine, ParkRequest, RuntimeList,
    ServiceAction, ServiceStatus, SetupReport, SiteList, SitePatch, Uninstalled, XdebugRequest,
    XdebugState,
};
use error::daemon_text;

/// This build's version. An older daemon is replaced.
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// `GET /v1/health` answers at once; this also bounds each handshake step.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
/// For one call that is not a stream, and for the start of a stream. A
/// service restart fits well inside.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// For `DELETE /v1/runtimes/{tool}/{version}`: a Node.js folder holds
/// thousands of files, and a virus scanner may slow their delete.
const UNINSTALL_TIMEOUT: Duration = Duration::from_secs(120);
/// For `POST /v1/setup`: longer than the daemon's slowest setup, which is
/// about 12 minutes when every program runs to its limit. The daemon gives
/// the user 5 minutes at the admin prompt and 5 more at the trust dialog
/// (macOS), and each check (`netstat`, `ps` for at most 8 processes,
/// `security verify-cert`) up to 10 seconds. A test in the daemon's
/// `setup_api.rs` holds this above the sum of the daemon's own limits.
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(13 * 60);
/// How long a new daemon has to answer. The wait ends as soon as it does, so
/// this only bounds a real failure. macOS checks a program the first time it
/// runs after an install or an update: about 2 s when the machine is idle,
/// more under load, so 5 s was too tight (task M4-20).
pub const START_WAIT: Duration = Duration::from_secs(15);
/// How long an old daemon has to stop. `marsad` gives its processes up to
/// 10 s and its open connections up to 5 s before it deletes `daemon.json`
/// (task M3-06), so this is longer than both together.
const STOP_WAIT: Duration = Duration::from_secs(17);
const POLL: Duration = Duration::from_millis(50);
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
        self.call(Method::POST, &path, REQUEST_TIMEOUT).await
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

    /// `DELETE /v1/runtimes/{tool}/{version}` (task M5-07): deletes the
    /// installed version that `version` names: `8.3.12`, or `8.3` when one
    /// installed version matches it. A php-fpm pool that runs it is stopped
    /// first. Returns once the folder is gone and the site list is built
    /// again; a changed list also comes on [`DaemonClient::events`] as
    /// `Event::SitesChanged`. A refusal changes nothing.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`], and these codes from the daemon:
    ///
    /// - `version_in_use` (409) as [`ClientError::InUse`], with what uses
    ///   it: the default in `global.toml`, sites set to it in `sites.toml`,
    ///   or the running Caddy;
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
    /// certificate authority for this user. Waits until it ends, up to 13
    /// minutes ([`SETUP_TIMEOUT`]): the user may take a while at each. The
    /// same report also comes on [`DaemonClient::events`] as
    /// `Event::SetupFinished`.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `setup_running` (409) while another setup runs,
    /// and `setup_not_supported` (501) on Linux and Windows for now. A
    /// cancelled prompt is not an error: it is in [`SetupReport::error`].
    pub async fn setup(&self) -> Result<SetupReport, ClientError> {
        self.call(Method::POST, &["v1", "setup"], SETUP_TIMEOUT)
            .await
    }

    /// `GET /v1/autostart`: whether the tray icon starts at login (task
    /// M5-03).
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

    /// `GET /v1/php/xdebug`: whether PHP loads Xdebug, and which versions
    /// (task M5-11).
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

/// A client for a daemon that fits this build, started if needed, with
/// `marsad` from the folder of the running program (version handshake,
/// architecture.md section 6).
///
/// 1. A daemon of this version or newer, with an API this build knows: use it.
/// 2. No daemon holds the lock: start `marsad`, then wait up to 5 s for it to
///    answer.
/// 3. A daemon with a newer API: `daemon_api_too_new`, and it keeps running.
/// 4. An older daemon: ask it to stop, wait up to 17 s for it to let go of
///    the lock, then step 2. If another client has replaced it meanwhile, the
///    new daemon is used.
///
/// A daemon that holds the lock but does not answer yet (starting or
/// stopping) is waited for, up to 17 s.
///
/// # Errors
///
/// Any [`ClientError`]; `daemon_start_failed` if `marsad` cannot be started,
/// stops early, or does not answer, and `daemon_stop_failed` if an old
/// daemon does not stop.
pub async fn ensure_running(data_dir: &Path) -> Result<DaemonClient, ClientError> {
    ensure(data_dir, marsad_path).await
}

/// [`ensure_running`] with the `marsad` program given. Public so the M3-09
/// tests can pass `env!("CARGO_BIN_EXE_marsad")`.
///
/// # Errors
///
/// As [`ensure_running`].
pub async fn ensure_running_with(
    data_dir: &Path,
    marsad: &Path,
) -> Result<DaemonClient, ClientError> {
    ensure(data_dir, || Ok(marsad.to_path_buf())).await
}

/// Stops the running daemon (`marsa daemon stop`): asks it to shut down,
/// then waits up to 17 s until it has let go of `run/daemon.lock`.
/// `Ok(false)` if no daemon was running.
///
/// # Errors
///
/// `daemon_stop_failed` if it still runs after 17 s, or the error of the
/// shutdown call if waiting cannot fix it.
pub async fn stop_daemon(data_dir: &Path) -> Result<bool, ClientError> {
    let lock = daemon_lock_path(&run_dir(data_dir));
    if !lock_is_held(&lock) {
        return Ok(false);
    }
    let deadline = Instant::now() + STOP_WAIT;
    let mut asked = false;
    let mut last_error = None;
    while lock_is_held(&lock) {
        if !asked {
            // No file or no answer yet: the daemon is starting or already
            // stopping. Asked again at the next poll.
            let ask = async { DaemonClient::connect(data_dir)?.shutdown().await };
            match timeout_at(deadline, ask).await {
                Ok(Ok(())) => asked = true,
                Ok(Err(err)) if err.is_transient() => last_error = Some(err),
                Ok(Err(err)) => return Err(err),
                Err(_) => {}
            }
        }
        if Instant::now() + POLL >= deadline {
            return Err(still_running(last_error.as_ref()));
        }
        sleep(POLL).await;
    }
    Ok(true)
}

/// `marsad` is looked up only when it must be started, so a running daemon
/// is found even if `current_exe` fails.
async fn ensure(
    data_dir: &Path,
    marsad: impl FnOnce() -> io::Result<PathBuf>,
) -> Result<DaemonClient, ClientError> {
    match current_daemon(data_dir).await? {
        Some(client) => Ok(client),
        None => start(data_dir, marsad).await,
    }
}

enum Probe {
    /// No daemon holds the lock.
    NotRunning,
    /// A daemon holds the lock but does not answer yet: it is starting or
    /// stopping.
    Busy,
    /// A daemon of this version or newer, with an API this build knows.
    Ready(DaemonClient),
    /// A daemon older than this build, and its version.
    Older(DaemonClient, String),
}

/// Steps 1, 3, and 4 of [`ensure_running`]: a client for the running daemon
/// if it fits, or `None` once no daemon holds the lock. An older daemon is
/// asked to stop.
async fn current_daemon(data_dir: &Path) -> Result<Option<DaemonClient>, ClientError> {
    let deadline = Instant::now() + STOP_WAIT;
    let mut asked_to_stop = false;
    let mut last_error = None;
    loop {
        match timeout_at(deadline, probe(data_dir)).await {
            Ok(Ok(Probe::Ready(client))) => return Ok(Some(client)),
            Ok(Ok(Probe::NotRunning)) => return Ok(None),
            Ok(Ok(Probe::Older(client, version))) if !asked_to_stop => {
                tracing::info!(daemon = %version, client = APP_VERSION, "stopping an older daemon");
                match timeout_at(deadline, client.shutdown()).await {
                    Ok(Ok(())) => asked_to_stop = true,
                    Ok(Err(err)) if err.is_transient() => last_error = Some(err),
                    Ok(Err(err)) => return Err(err),
                    Err(_) => {}
                }
            }
            Ok(Ok(Probe::Older(..) | Probe::Busy)) | Err(_) => {}
            Ok(Err(err)) if err.is_transient() => last_error = Some(err),
            Ok(Err(err)) => return Err(err),
        }
        if Instant::now() + POLL >= deadline {
            return Err(still_running(last_error.as_ref()));
        }
        sleep(POLL).await;
    }
}

/// Who holds the lock, and which version it is.
async fn probe(data_dir: &Path) -> Result<Probe, ClientError> {
    let run = run_dir(data_dir);
    // The lock first: a file left by a crash is never contacted.
    if !lock_is_held(&daemon_lock_path(&run)) {
        return Ok(Probe::NotRunning);
    }
    let file = match read_daemon_file(&run) {
        Ok(file) => file,
        Err(DaemonFileError::Missing { .. }) => return Ok(Probe::Busy),
        Err(err) => return Err(err.into()),
    };
    let client = DaemonClient::from_file(&file)?;
    let health = match client.health().await {
        Err(ClientError::NotRunning) => return Ok(Probe::Busy),
        other => other?,
    };
    if is_older(&health)? {
        return Ok(Probe::Older(client, daemon_text(&health.app_version)));
    }
    Ok(Probe::Ready(client))
}

/// Starts `marsad` and waits up to 5 s for it to answer.
///
/// If it exits at once with code 3, another daemon held the lock: if the
/// lock is free now (that daemon was stopping), `marsad` is started once
/// more; if not, that daemon is starting, and this waits for it. Any other
/// early exit fails at once.
async fn start(
    data_dir: &Path,
    marsad: impl FnOnce() -> io::Result<PathBuf>,
) -> Result<DaemonClient, ClientError> {
    let run = run_dir(data_dir);
    let lock = daemon_lock_path(&run);
    let log = data_dir.join("logs").join("marsad.log");
    // A file left by a crash stays until the new daemon replaces it. Its
    // daemon is gone, so it must not be contacted.
    let stale = read_daemon_file(&run)
        .ok()
        .map(|file| (file.pid, file.port))
        // Another client's daemon may have taken the lock and written this
        // file since our probe. Then the file is live: our `marsad` exits
        // with code 3 and the wait below uses that daemon.
        .filter(|_| !lock_is_held(&lock));
    let marsad = marsad().map_err(|err| {
        ClientError::start_failed("cannot find marsad next to this program", Some(err))
    })?;
    let spawn = || {
        let child = spawn_marsad(&marsad, data_dir).map_err(|err| {
            ClientError::start_failed(format!("cannot run {}", marsad.display()), Some(err))
        })?;
        tracing::info!(pid = child.pid(), path = %marsad.display(), "started marsad");
        Ok::<_, ClientError>(child)
    };
    let mut child = spawn()?;
    let mut respawned = false;
    let deadline = Instant::now() + START_WAIT;
    let mut last_error = None;
    loop {
        if let Some(status) = child.exit_status() {
            if status.code() != Some(EXIT_ALREADY_RUNNING) {
                return Err(ClientError::start_failed(
                    format!(
                        "marsad stopped during start-up ({status}); see {}",
                        log.display()
                    ),
                    None,
                ));
            }
            if !respawned && !lock_is_held(&lock) {
                respawned = true;
                child = spawn()?;
            }
        }
        match timeout_at(deadline, probe_new(data_dir, stale)).await {
            Ok(Ok(Some(client))) => return Ok(client),
            Ok(Ok(None)) | Err(_) => {}
            Ok(Err(err)) if err.is_transient() => last_error = Some(err),
            Ok(Err(err)) => return Err(err),
        }
        if Instant::now() + POLL >= deadline {
            let last = last_error
                .map(|err| format!(" (last error: {})", err.code()))
                .unwrap_or_default();
            return Err(ClientError::start_failed(
                format!(
                    "marsad did not answer within {} seconds{last}; see {}",
                    START_WAIT.as_secs(),
                    log.display()
                ),
                None,
            ));
        }
        sleep(POLL).await;
    }
}

/// A client for the new daemon once it holds the lock and answers. A file
/// whose `(pid, port)` is `stale` belongs to the crashed daemon and is not
/// contacted.
async fn probe_new(
    data_dir: &Path,
    stale: Option<(u32, u16)>,
) -> Result<Option<DaemonClient>, ClientError> {
    let run = run_dir(data_dir);
    if !lock_is_held(&daemon_lock_path(&run)) {
        return Ok(None);
    }
    let file = match read_daemon_file(&run) {
        Ok(file) if Some((file.pid, file.port)) != stale => file,
        Ok(_) | Err(DaemonFileError::Missing { .. }) => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let client = DaemonClient::from_file(&file)?;
    let health = client.health().await?;
    if is_older(&health)? {
        return Err(ClientError::start_failed(
            format!(
                "the daemon that answered is version {}, older than {APP_VERSION}",
                daemon_text(&health.app_version)
            ),
            None,
        ));
    }
    Ok(Some(client))
}

/// Steps 3 and 4 of the handshake for one answer: `daemon_api_too_new` for a
/// newer API, else whether the daemon is older than this build.
fn is_older(health: &Health) -> Result<bool, ClientError> {
    if health.api_version > API_VERSION {
        return Err(ClientError::ApiTooNew {
            daemon: health.api_version,
        });
    }
    Ok(older_than_this_build(&health.app_version))
}

/// Compared by semver precedence, so build metadata does not count. A
/// version that does not parse is replaced, unless it is exactly ours.
fn older_than_this_build(version: &str) -> bool {
    match (Version::parse(version), Version::parse(APP_VERSION)) {
        (Ok(theirs), Ok(ours)) => theirs.cmp_precedence(&ours) == Ordering::Less,
        _ => version != APP_VERSION,
    }
}

fn still_running(last_error: Option<&ClientError>) -> ClientError {
    let last = last_error
        .map(|err| format!(" (last error: {})", err.code()))
        .unwrap_or_default();
    ClientError::StopFailed {
        reason: format!(
            "it still holds its lock after {} seconds{last}",
            STOP_WAIT.as_secs()
        ),
    }
}

/// Whether a daemon holds `lock`. A missing lock file, or one this process
/// cannot lock or open, means no daemon could hold it either.
fn lock_is_held(lock: &Path) -> bool {
    let Ok(file) = File::open(lock) else {
        return false;
    };
    match file.try_lock_shared() {
        Ok(()) => {
            // Released at once and on purpose: on Windows a lock released
            // only by closing the handle may stay a little longer, and a
            // daemon starting now would find it taken.
            if let Err(err) = file.unlock() {
                tracing::debug!(error = %err, "cannot release the probe lock");
            }
            false
        }
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(err)) => {
            tracing::debug!(error = %err, "cannot test the daemon lock");
            false
        }
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

fn run_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("run")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_older_daemon_is_replaced() {
        let ours = Version::parse(APP_VERSION).unwrap();
        let newer = Version::new(ours.major, ours.minor, ours.patch + 1).to_string();
        let build = format!("{APP_VERSION}+other-build");
        for version in [APP_VERSION, newer.as_str(), build.as_str()] {
            assert!(!older_than_this_build(version), "{version}");
        }
        let pre = format!("{APP_VERSION}-rc.1");
        for version in [pre.as_str(), "not a version", ""] {
            assert!(older_than_this_build(version), "{version}");
        }
    }

    #[test]
    fn a_newer_api_is_refused() {
        let health = |api_version| Health {
            app_version: APP_VERSION.to_owned(),
            api_version,
            pid: 1,
        };
        assert!(!is_older(&health(API_VERSION)).unwrap());
        let err = is_older(&health(API_VERSION + 1)).unwrap_err();
        assert_eq!(err.code(), "daemon_api_too_new");
    }
}
