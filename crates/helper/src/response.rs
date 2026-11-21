//! The response: one JSON line on stdout, which `marsad` reads.
//!
//! A message never holds the content of a file the helper read, and never
//! quotes the request.

use std::io;

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
    /// Writing a file of the redirect failed, or a folder of the redirect is one
    /// that not only root can change, so nothing was written.
    pub const PF_WRITE: &str = "pf_write";
    /// The running helper could not be found or read, so it was not copied for
    /// the boot job, and nothing was written: its path is not
    /// known, the file is gone or larger than 64 MiB, or the read was refused.
    /// A refused read is what the folder protection of macOS gives: root cannot
    /// read the user's Desktop, Documents, and Downloads folders until the user
    /// allows it (Privacy & Security). Only then does the message say to run
    /// Marsa from its own folder,
    /// `~/Library/Application Support/dev.marsa.Marsa/bin`.
    pub const PF_HELPER_UNREADABLE: &str = "pf_helper_unreadable";
    /// Deleting a file of the redirect failed.
    pub const PF_DELETE: &str = "pf_delete";
    /// `pfctl -f` did not load the rules file into the anchor, or it was not
    /// asked to: the rules file is a link, or its folder is one that not only
    /// root can change.
    pub const PF_LOAD_FAILED: &str = "pf_load_failed";
    /// `pfctl -E` printed no reference token.
    pub const PF_TOKEN_MISSING: &str = "pf_token_missing";
    /// After the load, the anchor does not show the four rules.
    pub const PF_RULES_MISSING: &str = "pf_rules_missing";
    /// `pfctl -F all` on the anchor failed, so nothing else was removed.
    pub const PF_FLUSH_FAILED: &str = "pf_flush_failed";
    /// `pf-load` found no rules file: `pf_install` has not run.
    pub const PF_NOT_INSTALLED: &str = "pf_not_installed";
    /// A file at `FILE` that Marsa did not write, so it was left as it is.
    pub const PORTS_FOREIGN: &str = "ports_foreign";
    /// `FILE` cannot be read, or `ports_install` cannot read the current
    /// value. Nothing was changed.
    pub const PORTS_READ: &str = "ports_read";
    /// Writing `FILE` failed.
    pub const PORTS_WRITE: &str = "ports_write";
    /// `sysctl -p` failed, or port 80 is still not allowed after it. Marsa's
    /// file is removed again, so it cannot take effect at a later boot.
    pub const PORTS_APPLY_FAILED: &str = "ports_apply_failed";
    /// Deleting `FILE` failed. Nothing was changed.
    pub const PORTS_DELETE: &str = "ports_delete";
    /// `FILE` is deleted, but the value could not be set back to the default.
    /// It stays as it is until the next boot.
    pub const PORTS_RESTORE_FAILED: &str = "ports_restore_failed";
    /// A file at `UNIT_FILE` that Marsa did not write, so it was left as it
    /// is.
    pub const RESOLVED_FOREIGN: &str = "resolved_foreign";
    /// `UNIT_FILE` or `DROP_IN` cannot be read, so it was left as it is.
    pub const RESOLVED_READ: &str = "resolved_read";
    /// systemd-resolved does not run, or `systemctl` cannot tell. Nothing was
    /// changed.
    pub const RESOLVED_NOT_RUNNING: &str = "resolved_not_running";
    /// There is no `ip` program (iproute2) at the usual paths. Nothing was
    /// changed.
    pub const RESOLVED_TOOL_MISSING: &str = "resolved_tool_missing";
    /// Writing `UNIT_FILE` failed.
    pub const RESOLVED_WRITE: &str = "resolved_write";
    /// `systemctl` could not load, enable, start, or stop Marsa's unit. After
    /// an install, the unit is gone again.
    pub const RESOLVED_UNIT_FAILED: &str = "resolved_unit_failed";
    /// `marsa0` does not send `.test` names, and only them, to Marsa's server.
    /// The unit is gone again.
    pub const RESOLVED_CHECK_FAILED: &str = "resolved_check_failed";
    /// The restart that drops `DROP_IN` failed, or `systemctl` cannot tell
    /// whether systemd-resolved runs. The message says that the old process
    /// may still use Marsa's server.
    pub const RESOLVED_RESTART_FAILED: &str = "resolved_restart_failed";
    /// systemd-resolved does not run after the restart that drops `DROP_IN`,
    /// and did not start again: no name resolves until it runs.
    pub const RESOLVED_STOPPED: &str = "resolved_stopped";
    /// Deleting `UNIT_FILE` or `DROP_IN` failed.
    pub const RESOLVED_DELETE: &str = "resolved_delete";
    /// The version of systemd cannot be read, so Marsa makes no link. Nothing
    /// was changed but the removal of an old drop-in (and of an earlier unit).
    pub const RESOLVED_SYSTEMD_UNKNOWN: &str = "resolved_systemd_unknown";
    /// systemd is older than 250 and systemd-networkd runs (or `systemctl`
    /// cannot tell): that resolved counts a link networkd calls "initialized"
    /// as managed and clears what was set on it (ADR 0014, finding 30), so
    /// Marsa makes no link. The daemon writes `/etc/hosts` lines instead
    /// (`hosts_sync`). Nothing was changed but the removal of an
    /// old drop-in (and of an earlier unit).
    pub const RESOLVED_USE_HOSTS: &str = "resolved_use_hosts";
    /// The request's `pem` is not exactly one certificate. On Windows also: its
    /// SHA-1 has no letter, which `certutil` would read as an index.
    pub const TRUST_CERT_INVALID: &str = "trust_cert_invalid";
    /// Marsa's file (`CA_PATH` on macOS) is a link, is in a folder that not
    /// only root can change, or cannot be read as one certificate, so it was
    /// left as it is and no command ran.
    pub const TRUST_READ: &str = "trust_read";
    /// Writing Marsa's file failed.
    pub const TRUST_WRITE: &str = "trust_write";
    /// `security add-trusted-cert` (macOS) or `certutil -addstore` (Windows)
    /// failed or timed out. The helper then ran the steps of `trust_remove`. If
    /// they failed too, the file stays so a later `trust_remove` can finish, and
    /// until then `trust_install` with the same certificate answers `unchanged`:
    /// run `trust_remove` before a retry.
    pub const TRUST_ADD_FAILED: &str = "trust_add_failed";
    /// `security` or `certutil` could not remove the trust or the certificate.
    /// The file stays, so a retry still knows the hash.
    pub const TRUST_REMOVE_FAILED: &str = "trust_remove_failed";
    /// Deleting Marsa's file failed.
    pub const TRUST_DELETE: &str = "trust_delete";
    /// This system has neither family's tool with its folder. An install
    /// changed nothing; a removal may have deleted a file of Marsa's, but
    /// nothing could rebuild the store.
    pub const TRUST_TOOL_MISSING: &str = "trust_tool_missing";
    /// The store's tool failed, in an install or a removal. Not the codes of
    /// the macOS op (`trust_add_failed`, `trust_remove_failed`): the UI explains
    /// those with macOS steps (a dialog, Keychain Access).
    pub const TRUST_STORE_UPDATE_FAILED: &str = "trust_store_update_failed";
}

/// The message of a failure to read `path`, for ops that build their result
/// later.
pub fn unreadable_message(path: &str, kind: io::ErrorKind) -> String {
    format!("cannot read {path}: {kind}, so it was left as it is")
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
    /// An op that changed something.
    pub fn done(op: &'static str) -> Self {
        Self {
            op,
            outcome: Outcome::Done,
        }
    }

    /// An op that found nothing to change.
    pub fn unchanged(op: &'static str) -> Self {
        Self {
            op,
            outcome: Outcome::Unchanged,
        }
    }

    /// A file that could not be read, so it was left as it is.
    pub fn unreadable(
        op: &'static str,
        code: &'static str,
        path: &str,
        kind: io::ErrorKind,
    ) -> Self {
        Self::failed(op, code, unreadable_message(path, kind))
    }

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
