//! The error response of every endpoint.

use serde::{Deserialize, Serialize};

use super::VersionInUse;

/// The inside of an error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    /// Stable error name, for example `service_not_found`. The UI shows a
    /// translated text chosen by this code.
    pub code: String,
    /// English, for logs. Never shown to the user as is.
    pub message: String,
}

/// Every error response: `{ "error": { "code": "...", "message": "..." } }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ApiError,
    /// What uses a version, with the codes `version_in_use` and
    /// `sites_lose_php` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_use: Option<VersionInUse>,
}

impl ErrorBody {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: ApiError {
                code: code.into(),
                message: message.into(),
            },
            in_use: None,
        }
    }
}
