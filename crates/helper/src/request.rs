//! The request: a closed list of typed ops, parsed and fully validated before
//! any op runs.
//!
//! The helper runs as root, so nothing here is lenient. An unknown op, an
//! unknown or duplicate field, a wrong type, or a bad value rejects the whole
//! request, like a strict Laravel Form Request that also fails on extra keys.
//! The only way to get a [`ValidRequest`] is [`Request::from_arg`], so an op
//! can never run from a request that skipped a check.

use std::collections::BTreeSet;
use std::ffi::OsStr;

use serde::Deserialize;
use serde_json::Value;
use serde_json::error::Category;

use crate::response::code;

/// The protocol version of requests and responses.
pub const VERSION: u32 = 1;
/// The largest `apply` argument, in bytes. Checked before parsing.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_OPS: usize = 16;
const MAX_NAMES: usize = 1000;
const MAX_PEM_BYTES: usize = 16 * 1024;
const MAX_LABEL: usize = 63;
/// Ports below 1024 need root. Caddy and the DNS server never run as root.
const MIN_PORT: u16 = 1024;

/// `{"version": 1, "ops": [...]}`, before validation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    version: u32,
    ops: Vec<Op>,
}

/// A request that passed every check. The field is private, so only this
/// module can build one.
#[derive(Debug)]
pub struct ValidRequest(Request);

impl ValidRequest {
    /// The ops, in the order of the request.
    pub fn ops(&self) -> &[Op] {
        &self.0.ops
    }
}

/// One op from the closed list (task M4-05). The helper never opens or writes
/// a path from the request: every path it touches is a constant in its code.
///
/// Ops without fields are empty struct variants (`TrustRemove {}`), not unit
/// variants: serde ignores extra keys next to the tag of a unit variant, even
/// with `deny_unknown_fields`.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    /// Trusts a CA root certificate, sent as PEM text. The text, not a path:
    /// root must never open a path the caller chose (a symlink, a FIFO, a
    /// network share). Task M4-09 parses the PEM.
    TrustInstall {
        pem: String,
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
    /// Linux: lets a normal process bind port 80 and up, through a sysctl
    /// (ADR 0014). No field: the file, the key, and the value are constants
    /// in the helper.
    PortsInstall {},
    PortsRemove {},
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
            Op::PortsInstall {} => "ports_install",
            Op::PortsRemove {} => "ports_remove",
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
        code::REQUEST_INVALID
    }
}

impl Request {
    /// Reads the `apply` argument and checks every value, in this order: the
    /// size (so a huge argument is never parsed), UTF-8, JSON syntax, the
    /// shape and the version, the typed parse, and the values.
    ///
    /// # Errors
    ///
    /// `request_invalid` for anything outside the closed list or the limits.
    pub fn from_arg(raw: &OsStr) -> Result<ValidRequest, RequestError> {
        if raw.len() > MAX_REQUEST_BYTES {
            return Err(RequestError::TooLarge);
        }
        let text = raw.to_str().ok_or(RequestError::NotUtf8)?;
        let value: Value = serde_json::from_str(text).map_err(json_error)?;
        check_shape(&value)?;
        // From the text, not `from_value`: a `Value` keeps only the last of
        // two equal keys, and the typed parse must see both to reject them.
        let request: Request = serde_json::from_str(text).map_err(json_error)?;
        request.validate()?;
        Ok(ValidRequest(request))
    }

    fn validate(&self) -> Result<(), RequestError> {
        // Checked on the JSON value already; again here on the typed value.
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
                Op::TrustInstall { pem } => check_pem(format!("ops[{i}].pem"), pem)?,
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
                | Op::PortsInstall {}
                | Op::PortsRemove {}
                | Op::HostsRemove {}
                | Op::UninstallAll {} => {}
            }
        }
        Ok(())
    }
}

fn json_error(err: serde_json::Error) -> RequestError {
    RequestError::Json {
        problem: match err.classify() {
            Category::Data => "does not match the request format",
            Category::Io | Category::Syntax | Category::Eof => "is not valid JSON",
        },
        line: err.line(),
        column: err.column(),
    }
}

/// An object whose `version` is 1 and whose `ops` is an array of objects.
/// serde would also accept arrays in place of objects (`[1, [["pf_remove"]]]`),
/// and the version comes first, so a newer request with new ops gets a clear
/// error.
fn check_shape(value: &Value) -> Result<(), RequestError> {
    let Some(request) = value.as_object() else {
        return Err(invalid("request".to_owned(), "is not an object".to_owned()));
    };
    match request.get("version").and_then(Value::as_u64) {
        Some(version) if version == u64::from(VERSION) => {}
        Some(version) => {
            let reason = format!("{version} is not supported, only {VERSION} is");
            return Err(invalid("version".to_owned(), reason));
        }
        None => {
            let reason = "is missing or not a whole number".to_owned();
            return Err(invalid("version".to_owned(), reason));
        }
    }
    let Some(ops) = request.get("ops").and_then(Value::as_array) else {
        let reason = "is missing or not an array".to_owned();
        return Err(invalid("ops".to_owned(), reason));
    };
    match ops.iter().position(|op| !op.is_object()) {
        Some(i) => Err(invalid(format!("ops[{i}]"), "is not an object".to_owned())),
        None => Ok(()),
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

/// Only the size and the bytes. Task M4-09 parses the PEM itself.
fn check_pem(field: String, pem: &str) -> Result<(), RequestError> {
    let reason = if pem.is_empty() {
        "is empty"
    } else if pem.len() > MAX_PEM_BYTES {
        "is longer than 16384 bytes"
    } else if !pem.is_ascii() {
        "is not ASCII"
    } else if pem.contains('\0') {
        "contains a NUL byte"
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
            r#"{"op":"trust_install","pem":"x"}"#,
            r#"{"op":"trust_remove"}"#,
            r#"{"op":"resolver_install","port":1024}"#,
            r#"{"op":"resolver_remove"}"#,
            r#"{"op":"pf_install","http_port":1024,"https_port":1025}"#,
            r#"{"op":"pf_remove"}"#,
            r#"{"op":"resolved_install","port":1024}"#,
            r#"{"op":"resolved_remove"}"#,
            r#"{"op":"ports_install"}"#,
            r#"{"op":"ports_remove"}"#,
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

    /// Why the typed parse reads the text and not the `Value`.
    #[test]
    fn a_value_hides_duplicate_keys() {
        let json = r#"{"version":1,"ops":[{"op":"resolver_install","port":18053,"port":18054}]}"#;
        let value: Value = serde_json::from_str(json).unwrap();
        assert!(serde_json::from_value::<Request>(value).is_ok());
        assert!(Request::from_arg(OsStr::new(json)).is_err());
    }

    #[test]
    fn a_pem_of_exactly_the_limit_is_valid() {
        let op = Op::TrustInstall {
            pem: "A".repeat(MAX_PEM_BYTES),
        };
        let request = Request {
            version: 1,
            ops: vec![op],
        };
        assert!(request.validate().is_ok());
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
