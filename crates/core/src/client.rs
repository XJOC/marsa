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

mod db_browser;
mod error;
mod jobs;
mod lifecycle;
mod sse;

pub use error::ClientError;
pub use lifecycle::{
    START_WAIT, ensure_running, ensure_running_with, is_daemon_running, stop_daemon,
};
pub use sse::{EventStream, StreamItem};

use crate::data_dir::DataDir;
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
    AutostartRequest, AutostartState, DefaultRequest, DefaultVersion, DoctorReport, DumpList,
    DumpsCapture, ErrorBody, Event, Health, HostsStatus, InstallAccepted, InstallRequest,
    LinkRequest, LogLine, MailList, MailText, ParkRequest, RuntimeList, ServiceStatus, SiteList,
    SiteLog, SiteLogLine, SitePatch, WorkerKind, XdebugRequest, XdebugState,
};
use lifecycle::lock_is_held;

/// This build's version. An older daemon is replaced.
pub(super) const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// `GET /v1/health` answers at once; this also bounds each handshake step.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
/// For one call that is not a stream, and for the start of a stream.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
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
        let request = self.http.get(self.url(&["v1", "health"])?);
        self.answer(request.timeout(HEALTH_TIMEOUT)).await
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
        self.call(Method::GET, &["v1", "services"]).await
    }

    /// `GET /v1/logs/{id}`: the recent lines.
    pub async fn logs(&self, id: &str) -> Result<Vec<LogLine>, ClientError> {
        self.call(Method::GET, &["v1", "logs", id]).await
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
        self.call(Method::GET, &["v1", "runtimes"]).await
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

    /// `GET /v1/sites`: every site, sorted by name, and the folders that did
    /// not become one.
    pub async fn sites(&self) -> Result<SiteList, ClientError> {
        self.call(Method::GET, &["v1", "sites"]).await
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

    /// `PATCH /v1/sites/{name}` with `{"queue": on}` or `{"schedule": on}`.
    /// Returns once the setting is saved: the daemon starts or stops the
    /// worker after, and `GET /v1/services` shows it. The list after the
    /// change.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; the daemon's `site_not_found` (404), and
    /// `site_not_laravel` (409) to turn a worker on for a site that is not
    /// Laravel.
    pub async fn set_worker(
        &self,
        name: &str,
        kind: WorkerKind,
        on: bool,
    ) -> Result<SiteList, ClientError> {
        self.update_site(name, &SitePatch::worker(kind, on)).await
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
        self.call(Method::DELETE, &["v1", "sites", name]).await
    }

    /// `GET /v1/sites/{name}/log`: the end of the Laravel site's newest log
    /// (`storage/logs/laravel*.log`), read by the daemon.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; the daemon's `site_not_found` (404),
    /// `site_not_laravel` (409), and `site_log_unsafe` (409).
    pub async fn site_log(&self, name: &str) -> Result<SiteLog, ClientError> {
        self.call(Method::GET, &["v1", "sites", name, "log"]).await
    }

    /// `GET /v1/sites/{name}/log?follow=true`: the same lines, then each new
    /// one as it is written. The stream ends when the daemon stops, or when
    /// the log became unsafe to read; a new request then says which (409
    /// `site_log_unsafe`).
    ///
    /// # Errors
    ///
    /// As [`DaemonClient::site_log`].
    pub async fn follow_site_log(
        &self,
        name: &str,
    ) -> Result<EventStream<SiteLogLine>, ClientError> {
        let mut url = self.url(&["v1", "sites", name, "log"])?;
        url.set_query(Some("follow=true"));
        self.stream(url).await
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

    /// `GET /v1/hosts`: whether this system puts `.test`
    /// names in its hosts file, and which sites are not there yet. Shows no
    /// prompt.
    pub async fn hosts(&self) -> Result<HostsStatus, ClientError> {
        self.call(Method::GET, &["v1", "hosts"]).await
    }

    /// `GET /v1/doctor`: the checks of `marsa doctor`, as the daemon runs
    /// them. Read-only. Without the `PATH` check, which only a terminal can
    /// make.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `not_found` (404) from a daemon of an older build.
    pub async fn doctor(&self) -> Result<DoctorReport, ClientError> {
        self.call(Method::GET, &["v1", "doctor"]).await
    }

    /// `GET /v1/autostart`: whether the tray icon starts at login.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `autostart_unsupported` (501) on Windows for now.
    pub async fn autostart(&self) -> Result<AutostartState, ClientError> {
        self.call(Method::GET, &["v1", "autostart"]).await
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

    /// `GET /v1/mail`: the newest mails Mailpit caught, newest first.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `mail_not_running` (409) while Mailpit does not
    /// run, and `mail_unavailable` (502) when it did not answer.
    pub async fn mail(&self) -> Result<MailList, ClientError> {
        self.call(Method::GET, &["v1", "mail"]).await
    }

    /// `GET /v1/mail/{id}`: one mail's plain text, and Mailpit's page for
    /// its HTML. Opening it marks it read.
    ///
    /// # Errors
    ///
    /// As [`Self::mail`]; `mail_id_invalid` (400) and `mail_not_found` (404).
    pub async fn mail_text(&self, id: &str) -> Result<MailText, ClientError> {
        self.call(Method::GET, &["v1", "mail", id]).await
    }

    /// `DELETE /v1/mail`: deletes every mail Mailpit caught. The list after.
    ///
    /// # Errors
    ///
    /// As [`Self::mail`].
    pub async fn clear_mail(&self) -> Result<MailList, ClientError> {
        self.call(Method::DELETE, &["v1", "mail"]).await
    }

    /// `GET /v1/dumps`: the dumps with an id over `after` (all, if `None`),
    /// newest first, and what the dump server is doing.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `invalid_query` (400) cannot happen from here.
    pub async fn dumps(&self, after: Option<u64>) -> Result<DumpList, ClientError> {
        let mut url = self.url(&["v1", "dumps"])?;
        if let Some(after) = after {
            url.set_query(Some(&format!("after={after}")));
        }
        let request = self.http.get(url);
        self.answer(request.timeout(REQUEST_TIMEOUT)).await
    }

    /// `DELETE /v1/dumps`: forgets every dump. The list after.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`].
    pub async fn clear_dumps(&self) -> Result<DumpList, ClientError> {
        self.call(Method::DELETE, &["v1", "dumps"]).await
    }

    /// `GET /v1/dumps/capture`: whether `dump()` goes to Marsa.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; a `config_*` code (500) when `global.toml`
    /// cannot be read.
    pub async fn dumps_capture(&self) -> Result<DumpsCapture, ClientError> {
        self.call(Method::GET, &["v1", "dumps", "capture"]).await
    }

    /// `PUT /v1/dumps/capture`: turns the capture on or off. On, the dump
    /// server tries the port again; see [`DumpList::status`] afterwards.
    ///
    /// # Errors
    ///
    /// As [`Self::dumps_capture`], when `global.toml` cannot be written.
    pub async fn set_dumps_capture(&self, enabled: bool) -> Result<DumpsCapture, ClientError> {
        let body = DumpsCapture { enabled };
        self.call_with(Method::PUT, &["v1", "dumps", "capture"], &body)
            .await
    }

    /// `GET /v1/php/xdebug`: whether PHP loads Xdebug, and which versions.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`].
    pub async fn xdebug(&self) -> Result<XdebugState, ClientError> {
        self.call(Method::GET, &["v1", "php", "xdebug"]).await
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

    /// A call without a body and with a JSON answer, within
    /// `REQUEST_TIMEOUT`: a long operation is a job, not a long request
    /// (ADR 0026).
    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        segments: &[&str],
    ) -> Result<T, ClientError> {
        let request = self.http.request(method, self.url(segments)?);
        self.answer(request.timeout(REQUEST_TIMEOUT)).await
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
    DataDir::new(data_dir).run()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// No request of this client waits more than 30 seconds, except the
    /// event stream, which watches for silence instead (ADR 0026): a long
    /// operation is a job, and the client follows it with events. A new
    /// timeout constant in the client must be named here, and fit.
    #[test]
    fn no_request_waits_longer_than_thirty_seconds() {
        let limit = Duration::from_secs(30);
        for (name, value) in [
            ("CONNECT_TIMEOUT", CONNECT_TIMEOUT),
            ("HEALTH_TIMEOUT", HEALTH_TIMEOUT),
            ("REQUEST_TIMEOUT", REQUEST_TIMEOUT),
        ] {
            assert!(value <= limit, "{name} is {value:?}");
        }
        // The client's sources name no other timeout, and every limit they
        // set is one of these constants: `.timeout(Duration::from_secs(300))`
        // or `.timeout(wait)` fails here.
        let sources = [
            include_str!("client.rs"),
            include_str!("client/db_browser.rs"),
            include_str!("client/jobs.rs"),
            include_str!("client/lifecycle.rs"),
            include_str!("client/sse.rs"),
        ];
        let known = BTreeSet::from(["CONNECT_TIMEOUT", "HEALTH_TIMEOUT", "REQUEST_TIMEOUT"]);
        for source in sources {
            // Only the code before the tests.
            let code = source.split("#[cfg(test)]").next().unwrap_or(source);
            for word in code.split(|c: char| !(c.is_ascii_uppercase() || c == '_')) {
                if word.ends_with("_TIMEOUT") {
                    assert!(
                        known.contains(word),
                        "{word}: a timeout the test does not know"
                    );
                }
            }
            let lines = code
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"));
            for line in lines {
                for (at, call) in line.match_indices("timeout(") {
                    let argument = line[at + call.len()..]
                        .split([',', ')'])
                        .next()
                        .unwrap_or_default()
                        .trim();
                    assert!(
                        known.contains(argument),
                        "{line}: the limit is not a known constant"
                    );
                }
            }
        }
    }
}
