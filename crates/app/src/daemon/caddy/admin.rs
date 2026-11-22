//! Caddy's admin API on `127.0.0.1:<admin port>`, and the errors of a refused config.

use marsa_core::caddy::CaddyError;
use marsa_core::codes;
use reqwest::redirect;
use serde_json::Value;
use std::fmt;
use std::io;
use std::time::Duration;
use tracing::{debug, warn};

/// For each admin API call, `/stop` included.
const ADMIN_TIMEOUT: Duration = Duration::from_secs(1);
/// For `POST /load`: Caddy answers once the new config runs.
const LOAD_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a new config did not reach Caddy. Caddy keeps its old config then.
#[derive(Debug)]
pub enum LoadError {
    /// The config cannot be built.
    Config(CaddyError),
    /// `caddy.json` or the instance id cannot be written.
    Write(io::Error),
    /// Caddy cannot listen on an address of the new config: another program
    /// holds it, or it is not allowed (ADR 0008, finding 11). `address` comes
    /// from `listening on <addr>:` in Caddy's answer.
    PortInUse {
        address: Option<String>,
        message: String,
    },
    /// Caddy refused the config for another reason.
    Rejected { status: u16, message: String },
    /// The admin API did not answer.
    Unreachable(String),
}

impl LoadError {
    /// Stable error name for the logs and the UI.
    pub fn code(&self) -> &'static str {
        match self {
            LoadError::Config(err) => err.code(),
            LoadError::Write(_) => codes::CADDY_CONFIG_WRITE,
            LoadError::PortInUse { .. } => codes::PORT_IN_USE,
            LoadError::Rejected { .. } => codes::CADDY_CONFIG_REJECTED,
            LoadError::Unreachable(_) => codes::CADDY_ADMIN_UNREACHABLE,
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Config(err) => err.fmt(f),
            LoadError::Write(err) => write!(f, "cannot write caddy.json: {err}"),
            LoadError::PortInUse {
                address: Some(address),
                ..
            } => write!(
                f,
                "Caddy cannot listen on {address}: it is in use or not allowed"
            ),
            LoadError::PortInUse { address: None, .. } => {
                f.write_str("Caddy cannot listen on an address: it is in use or not allowed")
            }
            LoadError::Rejected { status, message } => {
                write!(f, "Caddy refused the config ({status}): {message}")
            }
            LoadError::Unreachable(err) => write!(f, "Caddy's admin API did not answer: {err}"),
        }
    }
}

/// Caddy's admin API on `127.0.0.1:<admin port>`.
pub(super) struct Admin {
    /// `None` if the client cannot be built: every call then fails.
    pub(super) http: Option<reqwest::Client>,
    pub(super) base: String,
}

impl Admin {
    pub(super) fn new(port: u16) -> Self {
        // `no_proxy`: a proxy from `HTTP_PROXY` must never get these calls.
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(redirect::Policy::none())
            .timeout(ADMIN_TIMEOUT)
            .build()
            .inspect_err(|err| warn!(%err, "cannot build the Caddy admin client"))
            .ok();
        Self {
            http,
            base: format!("http://127.0.0.1:{port}"),
        }
    }

    /// `POST /stop`: Caddy stops its servers and exits.
    pub(super) async fn stop(&self) {
        let Some(http) = &self.http else {
            return;
        };
        match http.post(format!("{}/stop", self.base)).send().await {
            Ok(response) => debug!(status = %response.status(), "caddy admin /stop"),
            Err(err) => debug!(%err, "caddy admin /stop failed"),
        }
    }

    /// `POST /load` with `config`: Caddy runs it from now on, or keeps its
    /// old config and says why.
    pub(super) async fn load(&self, config: &Value) -> Result<(), LoadError> {
        let Some(http) = &self.http else {
            return Err(LoadError::Unreachable("no HTTP client".to_owned()));
        };
        let response = http
            .post(format!("{}/load", self.base))
            .json(config)
            .timeout(LOAD_TIMEOUT)
            .send()
            .await
            .map_err(|err| LoadError::Unreachable(err.to_string()))?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        // A body that cannot be read leaves the status to say what happened.
        let body = response.text().await.unwrap_or_default();
        Err(load_error(status.as_u16(), &body))
    }
}

/// What Caddy says, in lower case, when it cannot listen on an address of a
/// new config: Go's texts on macOS and Linux (ADR 0008, finding 11), and the
/// two English Windows texts (the texts of `PORT_IN_USE` in `watch.rs`).
const LOAD_PORT_ERRORS: [&str; 4] = [
    "bind: address already in use",
    "bind: permission denied",
    "only one usage of each socket address",
    "forbidden by its access permissions",
];

/// The error of a refused `POST /load`. Caddy answers `{"error": "..."}`; a
/// 400 that says an address is taken or not allowed is `port_in_use`.
pub(super) fn load_error(status: u16, body: &str) -> LoadError {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|body| body.get("error")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| body.trim().to_owned());
    let lower = message.to_ascii_lowercase();
    if status == 400 && LOAD_PORT_ERRORS.iter().any(|text| lower.contains(text)) {
        let address = listen_address(&message);
        return LoadError::PortInUse { address, message };
    }
    LoadError::Rejected { status, message }
}

/// The `<addr>` of `listening on <addr>: ...` in Caddy's error text, for
/// example `127.0.0.1:18443`.
fn listen_address(message: &str) -> Option<String> {
    let (_, rest) = message.split_once("listening on ")?;
    let (address, _) = rest.split_once(": ")?;
    (!address.is_empty()).then(|| address.to_owned())
}
