//! Why a call to the daemon failed. Task M3-08.

use std::io;

use crate::platform::daemon_file::DaemonFileError;
use crate::types::API_VERSION;

/// Why a call to the daemon failed. [`ClientError::code`] gives a stable name
/// that the UI maps to a message. No message holds the token: the token lives
/// only in the `Authorization` header, and response bodies are never quoted
/// except the `ErrorBody` the daemon wrote for us.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// No `run/daemon.json`, or nothing accepts a connection on its port.
    #[error("the daemon is not running")]
    NotRunning,

    /// `marsad` could not be started, did not answer in time, or an old
    /// daemon did not stop.
    #[error("cannot start the daemon: {reason}")]
    StartFailed {
        reason: String,
        #[source]
        source: Option<io::Error>,
    },

    /// The connection worked but the call did not, for example it timed out.
    #[error("cannot reach the daemon on port {port}")]
    Unreachable {
        port: u16,
        #[source]
        source: reqwest::Error,
    },

    /// The daemon is newer than this build. Guessing is unsafe, and stopping
    /// it would undo an update, so the client stops here.
    #[error("the daemon speaks API version {daemon}; this build knows version {known} at most", known = API_VERSION)]
    ApiTooNew { daemon: u32 },

    /// `reason` is fixed text and places only, never the body itself.
    #[error("the daemon sent a response this client cannot read: {reason}")]
    BadResponse { reason: String },

    /// An id that cannot be one path segment: empty, `.`, or `..`. Refused
    /// before any request.
    #[error("{id:?} is not a valid name")]
    InvalidId { id: String },

    /// The daemon answered with an `ErrorBody`. `code` is passed on as sent.
    #[error("the daemon answered {status} {code}: {message}")]
    Api {
        status: u16,
        code: String,
        message: String,
    },

    /// `run/daemon.json` is there but cannot be used.
    #[error(transparent)]
    DaemonFile(DaemonFileError),
}

impl ClientError {
    /// Stable error name for the UI. Never changes once released. For `Api`
    /// it is the daemon's own code, such as `service_not_found`.
    pub fn code(&self) -> &str {
        match self {
            ClientError::NotRunning => "daemon_not_running",
            ClientError::StartFailed { .. } => "daemon_start_failed",
            ClientError::Unreachable { .. } => "daemon_unreachable",
            ClientError::ApiTooNew { .. } => "daemon_api_too_new",
            ClientError::BadResponse { .. } => "daemon_bad_response",
            ClientError::InvalidId { .. } => "invalid_id",
            ClientError::Api { code, .. } => code,
            ClientError::DaemonFile(err) => err.code(),
        }
    }

    pub(super) fn start_failed(reason: impl Into<String>, source: Option<io::Error>) -> Self {
        ClientError::StartFailed {
            reason: reason.into(),
            source,
        }
    }

    pub(super) fn bad_response(reason: impl Into<String>) -> Self {
        ClientError::BadResponse {
            reason: reason.into(),
        }
    }

    /// `serde_json`'s own message can quote the body, so only the place of
    /// the problem is kept.
    pub(super) fn bad_json(what: &str, err: &serde_json::Error) -> Self {
        Self::bad_response(format!(
            "{what} is not the expected JSON (line {}, column {})",
            err.line(),
            err.column()
        ))
    }
}

/// A missing file means no daemon. Every other file error keeps its code.
impl From<DaemonFileError> for ClientError {
    fn from(err: DaemonFileError) -> Self {
        match err {
            DaemonFileError::Missing { .. } => ClientError::NotRunning,
            other => ClientError::DaemonFile(other),
        }
    }
}
