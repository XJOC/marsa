//! Why a call to the daemon failed. Task M3-08.

use std::error::Error as StdError;
use std::io;
use std::path::PathBuf;

use crate::platform::daemon_file::DaemonFileError;
use crate::types::{API_VERSION, ErrorBody, VersionInUse};

/// The longest text from the daemon kept in an error or a log line.
const MAX_DAEMON_TEXT: usize = 200;

/// Why a call to the daemon failed. [`ClientError::code`] gives a stable name
/// that the UI maps to a message. No message holds the token: the token lives
/// only in the `Authorization` header, and response bodies are never quoted
/// except the `ErrorBody` the daemon wrote for us, cleaned by
/// [`daemon_text`].
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// No daemon holds `run/daemon.lock`, there is no `run/daemon.json`, or
    /// nothing accepts a connection on its port.
    #[error("the daemon is not running")]
    NotRunning,

    /// `marsad` could not be started, stopped early, or did not answer in
    /// time.
    #[error("cannot start the daemon: {reason}")]
    StartFailed {
        reason: String,
        #[source]
        source: Option<io::Error>,
    },

    /// A running daemon did not stop in time.
    #[error("the daemon did not stop: {reason}")]
    StopFailed { reason: String },

    /// The connection worked but the call did not, for example it timed out.
    #[error("cannot reach the daemon on port {port}")]
    Unreachable {
        port: u16,
        #[source]
        source: Box<dyn StdError + Send + Sync>,
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

    /// A folder for a site whose path is not UTF-8. JSON text cannot hold
    /// it, and the daemon would refuse it with the same code. Refused before
    /// any request.
    #[error("{} is not valid UTF-8", .path.display())]
    PathNotUtf8 { path: PathBuf },

    /// The daemon answered with an `ErrorBody`. `code` is passed on as sent,
    /// after [`daemon_text`].
    #[error("the daemon answered {status} {code}: {message}")]
    Api {
        status: u16,
        code: String,
        message: String,
    },

    /// The daemon's 409 `version_in_use` (task M5-07) or
    /// `sites_follow_default` (task M5-19) from `DELETE
    /// /v1/runtimes/{tool}/{version}`, with what uses the version. Its text
    /// went through [`daemon_text`]. Boxed, so every `Result<_,
    /// ClientError>` stays small.
    #[error("the daemon answered 409 {}: {message}", .in_use.code())]
    InUse {
        message: String,
        in_use: Box<VersionInUse>,
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
            ClientError::StopFailed { .. } => "daemon_stop_failed",
            ClientError::Unreachable { .. } => "daemon_unreachable",
            ClientError::ApiTooNew { .. } => "daemon_api_too_new",
            ClientError::BadResponse { .. } => "daemon_bad_response",
            ClientError::InvalidId { .. } => "invalid_id",
            ClientError::PathNotUtf8 { .. } => "site_path_not_utf8",
            ClientError::Api { code, .. } => code,
            ClientError::InUse { in_use, .. } => in_use.code(),
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

    pub(super) fn api(status: u16, body: &ErrorBody) -> Self {
        if let Some(in_use) = &body.in_use
            && matches!(
                body.error.code.as_str(),
                "version_in_use" | "sites_follow_default"
            )
        {
            let names = |names: &[String]| -> Vec<String> {
                names.iter().map(|name| daemon_text(name)).collect()
            };
            return ClientError::InUse {
                message: daemon_text(&body.error.message),
                in_use: Box::new(VersionInUse {
                    version: daemon_text(&in_use.version),
                    default: in_use.default.as_deref().map(daemon_text),
                    sites: names(&in_use.sites),
                    default_sites: names(&in_use.default_sites),
                    caddy: in_use.caddy,
                }),
            };
        }
        ClientError::Api {
            status,
            code: daemon_text(&body.error.code),
            message: daemon_text(&body.error.message),
        }
    }

    /// No answer within `waited`.
    pub(super) fn timed_out(port: u16, what: &str, waited: std::time::Duration) -> Self {
        ClientError::Unreachable {
            port,
            source: Box::new(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{what} within {} seconds", waited.as_secs()),
            )),
        }
    }

    /// Whether waiting may fix it: the daemon may be starting, stopping, or
    /// renaming its file into place (a sharing violation on Windows).
    pub(super) fn is_transient(&self) -> bool {
        matches!(
            self,
            ClientError::NotRunning
                | ClientError::Unreachable { .. }
                | ClientError::BadResponse { .. }
                | ClientError::Api { status: 401, .. }
                | ClientError::DaemonFile(
                    DaemonFileError::Invalid { .. } | DaemonFileError::Unreadable { .. }
                )
        )
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

/// Text from the daemon (an error code or message, a version) made safe for
/// an error message or a log line: no control characters, which could move
/// the cursor or forge a log line, and at most 200 characters.
pub(super) fn daemon_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_DAEMON_TEXT)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_text_drops_control_characters_and_is_short() {
        assert_eq!(daemon_text("service_not_found"), "service_not_found");
        assert_eq!(daemon_text("a\nb\r\u{1b}[31mc\u{7f}\u{85}"), "ab[31mc");
        let long = "م".repeat(300);
        assert_eq!(daemon_text(&long).chars().count(), MAX_DAEMON_TEXT);
    }
}
