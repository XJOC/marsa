//! The request: a closed list of typed ops, parsed and fully validated before
//! any op runs.
//!
//! The helper runs as root, so nothing here is lenient. An unknown op, an
//! unknown or duplicate field, a wrong type, or a bad value rejects the whole
//! request, like a strict Laravel Form Request that also fails on extra keys.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf, Prefix};

use serde::Deserialize;
use serde_json::error::Category;

/// The protocol version of requests and responses.
pub const VERSION: u32 = 1;
/// The largest `apply` argument, in bytes. Checked before parsing.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_OPS: usize = 16;
const MAX_NAMES: usize = 1000;
const MAX_CERT_BYTES: usize = 1024;
const MAX_LABEL: usize = 63;
/// Ports below 1024 need root. Caddy and the DNS server never run as root.
const MIN_PORT: u16 = 1024;

/// `{"version": 1, "ops": [...]}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub ops: Vec<Op>,
}

/// One op from the closed list (task M4-05). The helper never writes to a path
/// from the request: every path it writes is a constant in its code.
///
/// Ops without fields are empty struct variants (`TrustRemove {}`), not unit
/// variants: serde ignores extra keys next to the tag of a unit variant, even
/// with `deny_unknown_fields`.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    /// Trusts the CA certificate at `cert`. The helper only reads this path.
    TrustInstall {
        cert: PathBuf,
    },
    TrustRemove {},
    /// macOS: `/etc/resolver/test`, so `.test` names go to `127.0.0.1:<port>`.
    ResolverInstall {
        port: u16,
    },
    ResolverRemove {},
    /// macOS: the `pf` redirect of ports 80 and 443 to Caddy (ADR 0008).
    PfInstall {
        http_port: u16,
        https_port: u16,
    },
    PfRemove {},
    /// Linux: the systemd-resolved config for `.test`.
    ResolvedInstall {
        port: u16,
    },
    ResolvedRemove {},
    /// Windows: the site names in Marsa's block of the hosts file.
    HostsSync {
        names: Vec<String>,
    },
    HostsRemove {},
    /// Every `*_remove` op of the current OS.
    UninstallAll {},
}

impl Op {
    /// The op's name as written in the request and in its result.
    pub fn name(&self) -> &'static str {
        match self {
            Op::TrustInstall { .. } => "trust_install",
            Op::TrustRemove {} => "trust_remove",
            Op::ResolverInstall { .. } => "resolver_install",
            Op::ResolverRemove {} => "resolver_remove",
            Op::PfInstall { .. } => "pf_install",
            Op::PfRemove {} => "pf_remove",
            Op::ResolvedInstall { .. } => "resolved_install",
            Op::ResolvedRemove {} => "resolved_remove",
            Op::HostsSync { .. } => "hosts_sync",
            Op::HostsRemove {} => "hosts_remove",
            Op::UninstallAll {} => "uninstall_all",
        }
    }
}

/// Why a request was rejected. Every case has the code `request_invalid`. The
/// message names the field and the reason, and never quotes the request.
#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    #[error("the request is larger than {MAX_REQUEST_BYTES} bytes")]
    TooLarge,

    #[error("the request is not UTF-8")]
    NotUtf8,

    /// serde's own message is not used: it can quote parts of the request.
    #[error("the request {problem} (line {line}, column {column})")]
    Json {
        problem: &'static str,
        line: usize,
        column: usize,
    },

    #[error("{field}: {reason}")]
    Invalid { field: String, reason: String },
}

impl RequestError {
    /// Stable error name for `marsad`. Never changes once released.
    pub fn code(&self) -> &'static str {
        "request_invalid"
    }
}

impl Request {
    /// Reads the `apply` argument and validates every value.
    ///
    /// # Errors
    ///
    /// `request_invalid` for anything outside the closed list or the limits.
    pub fn from_arg(raw: &OsStr) -> Result<Self, RequestError> {
        let request = Self::parse(raw)?;
        request.validate()?;
        Ok(request)
    }

    /// Checks the size, the encoding, and the JSON shape, in that order. The
    /// size comes first, so a huge argument is never parsed.
    ///
    /// # Errors
    ///
    /// `request_invalid` if the argument is too large, not UTF-8, not JSON, or
    /// not a request of known ops and fields.
    pub fn parse(raw: &OsStr) -> Result<Self, RequestError> {
        if raw.len() > MAX_REQUEST_BYTES {
            return Err(RequestError::TooLarge);
        }
        let text = raw.to_str().ok_or(RequestError::NotUtf8)?;
        serde_json::from_str(text).map_err(|err| RequestError::Json {
            problem: match err.classify() {
                Category::Data => "does not match the request format",
                Category::Io | Category::Syntax | Category::Eof => "is not valid JSON",
            },
            line: err.line(),
            column: err.column(),
        })
    }

    /// Checks every value against the limits of task M4-05.
    ///
    /// # Errors
    ///
    /// `request_invalid` with the first field that is out of bounds.
    pub fn validate(&self) -> Result<(), RequestError> {
        if self.version != VERSION {
            let reason = format!("{} is not supported, only {VERSION} is", self.version);
            return Err(invalid("version".to_owned(), reason));
        }
        if !(1..=MAX_OPS).contains(&self.ops.len()) {
            let reason = format!("{} ops, expected 1 to {MAX_OPS}", self.ops.len());
            return Err(invalid("ops".to_owned(), reason));
        }
        for (i, op) in self.ops.iter().enumerate() {
            // Every variant is listed, so a new op with fields cannot skip
            // validation by accident.
            match op {
                Op::TrustInstall { cert } => check_cert(format!("ops[{i}].cert"), cert)?,
                Op::ResolverInstall { port } | Op::ResolvedInstall { port } => {
                    check_port(format!("ops[{i}].port"), *port)?;
                }
                Op::PfInstall {
                    http_port,
                    https_port,
                } => {
                    check_port(format!("ops[{i}].http_port"), *http_port)?;
                    check_port(format!("ops[{i}].https_port"), *https_port)?;
                    if http_port == https_port {
                        let reason = "http_port and https_port are the same".to_owned();
                        return Err(invalid(format!("ops[{i}]"), reason));
                    }
                }
                Op::HostsSync { names } => check_names(i, names)?,
                Op::TrustRemove {}
                | Op::ResolverRemove {}
                | Op::PfRemove {}
                | Op::ResolvedRemove {}
                | Op::HostsRemove {}
                | Op::UninstallAll {} => {}
            }
        }
        Ok(())
    }
}

fn invalid(field: String, reason: String) -> RequestError {
    RequestError::Invalid { field, reason }
}

fn check_port(field: String, port: u16) -> Result<(), RequestError> {
    if port < MIN_PORT {
        return Err(invalid(field, format!("{port} is below {MIN_PORT}")));
    }
    Ok(())
}

/// An absolute path for this OS, without `..`. On Windows it must start with a
/// drive letter: a `\\server\share` path would make the helper, running as
/// SYSTEM, connect to another machine.
fn check_cert(field: String, cert: &Path) -> Result<(), RequestError> {
    let bytes = cert.as_os_str().as_encoded_bytes();
    let reason = if bytes.len() > MAX_CERT_BYTES {
        "is longer than 1024 bytes"
    } else if bytes.contains(&0) {
        "contains a NUL byte"
    } else if !cert.is_absolute() {
        "is not an absolute path"
    } else if cert.components().any(|part| part == Component::ParentDir) {
        "contains a `..` component"
    } else if let Some(Component::Prefix(prefix)) = cert.components().next()
        && !matches!(prefix.kind(), Prefix::Disk(_))
    {
        "is not on a local drive"
    } else {
        return Ok(());
    };
    Err(invalid(field, reason.to_owned()))
}

fn check_names(i: usize, names: &[String]) -> Result<(), RequestError> {
    if names.len() > MAX_NAMES {
        let reason = format!("{} names, at most {MAX_NAMES}", names.len());
        return Err(invalid(format!("ops[{i}].names"), reason));
    }
    let mut seen = BTreeSet::new();
    for (j, name) in names.iter().enumerate() {
        let reason = if !is_test_name(name) {
            "is not <label>.test, with 1 to 63 of a-z, 0-9, and - (not first or last)"
        } else if !seen.insert(name.as_str()) {
            "is a duplicate"
        } else {
            continue;
        };
        return Err(invalid(format!("ops[{i}].names[{j}]"), reason.to_owned()));
    }
    Ok(())
}

/// `^[a-z0-9-]+\.test$`, by hand (no regex crate), with a label of 1 to 63
/// characters that does not start or end with `-`.
fn is_test_name(name: &str) -> bool {
    let Some(label) = name.strip_suffix(".test") else {
        return false;
    };
    (1..=MAX_LABEL).contains(&label.len())
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_names_match_the_request_tags() {
        let ops = [
            r#"{"op":"trust_install","cert":"/c"}"#,
            r#"{"op":"trust_remove"}"#,
            r#"{"op":"resolver_install","port":1024}"#,
            r#"{"op":"resolver_remove"}"#,
            r#"{"op":"pf_install","http_port":1024,"https_port":1025}"#,
            r#"{"op":"pf_remove"}"#,
            r#"{"op":"resolved_install","port":1024}"#,
            r#"{"op":"resolved_remove"}"#,
            r#"{"op":"hosts_sync","names":[]}"#,
            r#"{"op":"hosts_remove"}"#,
            r#"{"op":"uninstall_all"}"#,
        ];
        for json in ops {
            let op: Op = serde_json::from_str(json).unwrap();
            assert!(
                json.starts_with(&format!(r#"{{"op":"{}""#, op.name())),
                "{json}"
            );
        }
    }

    #[test]
    fn test_names() {
        let long63 = format!("{}.test", "a".repeat(63));
        for good in ["shop.test", "a.test", "my-shop-2.test", long63.as_str()] {
            assert!(is_test_name(good), "{good}");
        }
        let long64 = format!("{}.test", "a".repeat(64));
        let bad = [
            "",
            ".test",
            "a.b.test",
            "shop-.test",
            "shop_1.test",
            "shop.test.",
            "é.test",
            long64.as_str(),
        ];
        for bad in bad {
            assert!(!is_test_name(bad), "{bad}");
        }
    }
}
