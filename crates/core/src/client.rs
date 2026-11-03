//! Typed client for the daemon API, used by the CLI and the window
//! (architecture.md section 6). Task M3-08.
//!
//! [`ensure_running`] finds the daemon through `run/daemon.json`, starts
//! `marsad` if needed, and checks its version. Every call then goes to
//! `http://127.0.0.1:<port>` with the bearer token.
//!
//! The token leaves this module only in the `Authorization` header, which is
//! marked sensitive. [`DaemonClient`] prints its port only, no proxy is ever
//! used (`HTTP_PROXY` would receive the header), redirects are not followed,
//! and no error message holds the token.

mod error;
mod sse;

pub use error::ClientError;
pub use sse::EventStream;

use std::fmt;
use std::fs::{File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::{self, HeaderMap, HeaderValue};
use reqwest::{Method, RequestBuilder, Response, Url, redirect};
use serde::de::DeserializeOwned;
use tokio::time::{Instant, sleep, timeout_at};

use crate::platform::daemon_file::{
    DaemonFileError, daemon_authority, daemon_file_path, read_daemon_file,
};
use crate::platform::spawn::{marsad_path, spawn_marsad};
use crate::types::{API_VERSION, ErrorBody, Event, Health, LogLine, ServiceAction, ServiceStatus};

/// This build's version. A daemon with another one is replaced.
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// For one call that is not a stream. A service restart fits well inside.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a new daemon has to answer.
const START_WAIT: Duration = Duration::from_secs(5);
/// How long an old daemon has to stop. `marsad` gives its processes up to
/// 10 s and its open connections up to 5 s before it deletes `daemon.json`
/// (task M3-06), so this is longer than both together.
const STOP_WAIT: Duration = Duration::from_secs(17);
const POLL: Duration = Duration::from_millis(50);
/// The largest JSON body read. Log lines are the biggest answer.
const MAX_BODY: usize = 16 * 1024 * 1024;

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
    /// Reads `<data_dir>/run/daemon.json` and prepares the client. Sends
    /// nothing: call [`DaemonClient::health`] to see if the daemon answers.
    ///
    /// # Errors
    ///
    /// `daemon_not_running` if there is no file, a `daemon_file_*` code if it
    /// cannot be used, and `daemon_unreachable` if the HTTP client cannot be
    /// built.
    pub fn connect(data_dir: &Path) -> Result<Self, ClientError> {
        let file = read_daemon_file(&run_dir(data_dir))?;
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
            .map_err(|source| ClientError::Unreachable { port, source })?;
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

    /// `GET /v1/health`.
    pub async fn health(&self) -> Result<Health, ClientError> {
        self.call(Method::GET, &["v1", "health"]).await
    }

    /// `POST /v1/shutdown`. Returns once the daemon accepted; it stops soon
    /// after and deletes `run/daemon.json` last.
    pub async fn shutdown(&self) -> Result<(), ClientError> {
        let request = self.http.post(self.url(&["v1", "shutdown"])?);
        self.send(request.timeout(REQUEST_TIMEOUT)).await?;
        Ok(())
    }

    /// `GET /v1/services`.
    pub async fn services(&self) -> Result<Vec<ServiceStatus>, ClientError> {
        self.call(Method::GET, &["v1", "services"]).await
    }

    /// `POST /v1/services/{id}/{action}`. The status after the action.
    pub async fn service_action(
        &self,
        id: &str,
        action: ServiceAction,
    ) -> Result<ServiceStatus, ClientError> {
        self.call(Method::POST, &["v1", "services", id, action.as_str()])
            .await
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

    /// A call with a JSON answer.
    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        segments: &[&str],
    ) -> Result<T, ClientError> {
        let request = self.http.request(method, self.url(segments)?);
        let request = request.timeout(REQUEST_TIMEOUT);
        let response = self.send(request).await?;
        let body = self.read_body(response).await?;
        serde_json::from_slice(&body).map_err(|err| ClientError::bad_json("the answer", &err))
    }

    /// A call with an SSE answer. No timeout: a stream stays open.
    async fn stream<T: DeserializeOwned>(&self, url: Url) -> Result<EventStream<T>, ClientError> {
        let request = self
            .http
            .get(url)
            .header(header::ACCEPT, "text/event-stream");
        let response = self.send(request).await?;
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
        Err(ClientError::Api {
            status: status.as_u16(),
            code: body.error.code,
            message: body.error.message,
        })
    }

    async fn read_body(&self, mut response: Response) -> Result<Vec<u8>, ClientError> {
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|err| self.transport(err))? {
            if body.len() + chunk.len() > MAX_BODY {
                return Err(ClientError::bad_response(
                    "the answer is larger than 16 MiB",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// A failed connection means no daemon on that port, as after a crash
    /// that left `run/daemon.json` behind.
    fn transport(&self, err: reqwest::Error) -> ClientError {
        if err.is_connect() {
            tracing::debug!(port = self.port, error = %err, "no daemon answers");
            ClientError::NotRunning
        } else {
            ClientError::Unreachable {
                port: self.port,
                source: err,
            }
        }
    }
}

/// A client for a daemon of this version, started if needed, with `marsad`
/// from the folder of the running program (version handshake, architecture.md
/// section 6).
///
/// 1. A daemon that answers with this version: done.
/// 2. No daemon: start `marsad`, then wait up to 5 seconds for it to answer.
/// 3. A daemon with a newer API: `daemon_api_too_new`, and it keeps running.
/// 4. A daemon with another version: stop it, wait up to 5 seconds for it to
///    go, then step 2.
///
/// # Errors
///
/// Any [`ClientError`]; `daemon_start_failed` if `marsad` cannot be started,
/// does not answer, or the old daemon does not stop.
pub async fn ensure_running(data_dir: &Path) -> Result<DaemonClient, ClientError> {
    ensure(data_dir, marsad_path).await
}

/// [`ensure_running`] with the `marsad` program given, for tests.
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

/// `marsad` is looked up only when it must be started, so a running daemon
/// is found even if `current_exe` fails.
async fn ensure(
    data_dir: &Path,
    marsad: impl FnOnce() -> io::Result<PathBuf>,
) -> Result<DaemonClient, ClientError> {
    match probe(data_dir).await? {
        Probe::Ready(client) => return Ok(client),
        Probe::NotRunning => {}
        Probe::OtherVersion(client, version) => {
            tracing::info!(daemon = %version, client = APP_VERSION, "replacing a daemon of another version");
            match client.shutdown().await {
                Ok(()) | Err(ClientError::NotRunning) => {}
                Err(err) => return Err(err),
            }
            wait_until_stopped(&run_dir(data_dir)).await?;
        }
    }
    let marsad = marsad().map_err(|err| {
        ClientError::start_failed("cannot find marsad next to this program", Some(err))
    })?;
    let pid = spawn_marsad(&marsad, data_dir).map_err(|err| {
        ClientError::start_failed(format!("cannot run {}", marsad.display()), Some(err))
    })?;
    tracing::info!(pid, path = %marsad.display(), "started marsad");
    wait_until_up(data_dir).await
}

enum Probe {
    Ready(DaemonClient),
    NotRunning,
    /// A daemon of this API version or older, but another app version.
    OtherVersion(DaemonClient, String),
}

/// Steps 1, 3, and 4 of [`ensure_running`]: who answers, and which version.
async fn probe(data_dir: &Path) -> Result<Probe, ClientError> {
    let found = DaemonClient::connect(data_dir);
    let client = match found {
        Err(ClientError::NotRunning) => return Ok(Probe::NotRunning),
        other => other?,
    };
    let health = match client.health().await {
        Err(ClientError::NotRunning) => return Ok(Probe::NotRunning),
        other => other?,
    };
    if health.api_version > API_VERSION {
        return Err(ClientError::ApiTooNew {
            daemon: health.api_version,
        });
    }
    if health.app_version != APP_VERSION {
        return Ok(Probe::OtherVersion(client, health.app_version));
    }
    Ok(Probe::Ready(client))
}

/// Polls until the new daemon answers. Until it has written its own file, a
/// file left by a crash can point at a dead or foreign port, so connection
/// errors and unreadable answers mean "not yet". So does a file that cannot
/// be read: on Windows that can be a sharing violation while the daemon
/// renames it into place.
async fn wait_until_up(data_dir: &Path) -> Result<DaemonClient, ClientError> {
    let deadline = Instant::now() + START_WAIT;
    loop {
        match timeout_at(deadline, probe(data_dir)).await {
            Ok(Ok(Probe::Ready(client))) => return Ok(client),
            Ok(Ok(Probe::OtherVersion(_, version))) => {
                return Err(ClientError::start_failed(
                    format!("the new daemon reports version {version}, not {APP_VERSION}"),
                    None,
                ));
            }
            Ok(Ok(Probe::NotRunning))
            | Ok(Err(
                ClientError::Unreachable { .. }
                | ClientError::BadResponse { .. }
                | ClientError::DaemonFile(
                    DaemonFileError::Invalid { .. } | DaemonFileError::Unreadable { .. },
                ),
            ))
            | Err(_) => {}
            Ok(Err(err)) => return Err(err),
        }
        if Instant::now() + POLL >= deadline {
            return Err(ClientError::start_failed(
                "marsad did not answer within 5 seconds",
                None,
            ));
        }
        sleep(POLL).await;
    }
}

/// Polls until the old daemon has deleted `run/daemon.json` and released
/// `run/daemon.lock`. It deletes the file just before it releases the lock;
/// a new daemon started in between would find the lock taken and exit.
async fn wait_until_stopped(run_dir: &Path) -> Result<(), ClientError> {
    let deadline = Instant::now() + STOP_WAIT;
    let file = daemon_file_path(run_dir);
    let lock = run_dir.join("daemon.lock");
    while file.exists() || lock_is_held(&lock) {
        if Instant::now() >= deadline {
            return Err(ClientError::start_failed(
                "the old daemon did not stop within 5 seconds",
                None,
            ));
        }
        sleep(POLL).await;
    }
    Ok(())
}

/// Whether another process holds `lock`. A shared lock taken here would
/// fail; if it works, it is released at once when the file closes.
fn lock_is_held(lock: &Path) -> bool {
    File::open(lock)
        .is_ok_and(|file| matches!(file.try_lock_shared(), Err(TryLockError::WouldBlock)))
}

fn run_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("run")
}
