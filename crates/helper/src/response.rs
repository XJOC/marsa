//! The response: one JSON line on stdout, which `marsad` reads.
//!
//! A message never holds the content of a file the helper read, and never
//! quotes the request.

use serde::Serialize;

use crate::request::VERSION;

/// The stable error codes. `marsad` and the UI match on them, so they never
/// change once released.
pub mod code {
    /// The pipe's client is not the user who installed the helper service
    /// (Windows).
    // Used by the Windows service only.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub const CLIENT_NOT_ALLOWED: &str = "client_not_allowed";
    /// The marker lines of Marsa's block in the hosts file do not pair up,
    /// so the file was left as it is (Windows).
    pub const HOSTS_BLOCK_BROKEN: &str = "hosts_block_broken";
    /// The hosts file could not be read, so it was left as it is (Windows).
    pub const HOSTS_READ: &str = "hosts_read";
    /// Writing the hosts file failed (Windows).
    pub const HOSTS_WRITE: &str = "hosts_write";
    pub const OP_NOT_IMPLEMENTED: &str = "op_not_implemented";
    pub const OP_NOT_SUPPORTED: &str = "op_not_supported";
    pub const REQUEST_INVALID: &str = "request_invalid";
    /// `/etc/resolver/test` is Marsa's, but deleting it failed.
    pub const RESOLVER_DELETE: &str = "resolver_delete";
    /// `/etc/resolver/test` belongs to another tool, so it was left as it is.
    pub const RESOLVER_FOREIGN: &str = "resolver_foreign";
    /// `/etc/resolver/test` could not be read, so it was left as it is.
    pub const RESOLVER_READ: &str = "resolver_read";
    /// Writing `/etc/resolver/test` failed.
    pub const RESOLVER_WRITE: &str = "resolver_write";
    /// `marsa-helper service` was not started by the service manager
    /// (Windows).
    // Used by the Windows service only.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub const SERVICE_FAILED: &str = "service_failed";
    /// A `trust_install` through the service carries a certificate other
    /// than the one approved at install (Windows).
    // Used by the Windows service only.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub const TRUST_NOT_ALLOWED: &str = "trust_not_allowed";
    pub const USAGE: &str = "usage";
}

/// `{"version":1,"results":[...]}` or `{"version":1,"error":{...}}`.
#[derive(Debug, Serialize)]
pub struct Response {
    version: u32,
    #[serde(flatten)]
    body: Body,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Body {
    Results(Vec<OpResult>),
    Error { code: &'static str, message: String },
}

/// The result of one op: `{"op":"pf_install","status":"done"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpResult {
    pub op: &'static str,
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// `status`, plus `code` and `message` when the op failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Done,
    Unchanged,
    /// `code` is stable, so the UI can map it to a message.
    Failed {
        code: &'static str,
        message: String,
    },
}

impl OpResult {
    /// A failed op. `message` must not hold the content of a file.
    pub fn failed(op: &'static str, code: &'static str, message: String) -> Self {
        Self {
            op,
            outcome: Outcome::Failed { code, message },
        }
    }
}

impl Response {
    /// The results of the ops of a valid request, in order.
    pub fn results(results: Vec<OpResult>) -> Self {
        Self {
            version: VERSION,
            body: Body::Results(results),
        }
    }

    /// A rejected request (`request_invalid`) or a bad command line (`usage`).
    pub fn error(code: &'static str, message: String) -> Self {
        Self {
            version: VERSION,
            body: Body::Error { code, message },
        }
    }

    /// `0` if every op is done or unchanged, `1` if one failed, `2` for an
    /// error.
    pub fn exit_code(&self) -> u8 {
        match &self.body {
            Body::Results(results) => {
                let failed = |r: &OpResult| matches!(r.outcome, Outcome::Failed { .. });
                u8::from(results.iter().any(failed))
            }
            Body::Error { .. } => 2,
        }
    }

    /// The response as one line of JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self)
            .expect("a response has only string keys, strings, and numbers, so it serializes")
    }
}
