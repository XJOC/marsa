//! The request and response of `marsa-helper`, the privileged helper.
//!
//! The helper runs as root (or as a Windows service) and accepts only a closed
//! list of typed operations (architecture.md section 7). It does not depend on
//! `marsa-core`, so these types are written twice: here, and in
//! `crates/helper/src/request.rs`. Both sides are tested against the same JSON
//! files in `fixtures/helper/`, so they cannot drift apart silently.
//!
//! The daemon builds a [`HelperRequest`], and the setup flow sends it to the
//! helper with one admin prompt. This module only describes the data.

use serde::{Deserialize, Serialize};

// `sha2` comes with the `full` feature; the shim needs only the types here.
#[cfg(feature = "full")]
pub(crate) mod ca;
#[cfg(feature = "full")]
pub use ca::certificate_sha256;
/// The strict parser, for the round trip of `caddy::pem_certificates`.
#[cfg(all(test, feature = "full"))]
pub(crate) use ca::parse_pem as certificate_der;

/// The protocol version in every request and response.
pub const HELPER_PROTOCOL_VERSION: u32 = 1;

/// One request: the operations to run, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperRequest {
    pub version: u32,
    pub ops: Vec<HelperOp>,
}

impl HelperRequest {
    /// A request at the current protocol version.
    pub fn new(ops: Vec<HelperOp>) -> Self {
        Self {
            version: HELPER_PROTOCOL_VERSION,
            ops,
        }
    }
}

/// The closed list of helper operations. Nothing else can be asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum HelperOp {
    /// Trust the local CA root certificate (macOS and Linux). `pem` is the
    /// certificate itself, at most 16 KiB, not a path: the helper runs as
    /// root and never opens a path from the request, so a link planted by
    /// another program cannot make it read something else.
    TrustInstall {
        pem: String,
    },
    TrustRemove,
    /// macOS: write `/etc/resolver/test` so `.test` names go to the DNS
    /// server of `marsad` on `127.0.0.1:<port>`.
    ResolverInstall {
        port: u16,
    },
    ResolverRemove,
    /// macOS: redirect `127.0.0.1:80` and `:443` to Caddy's high ports with
    /// `pf` (ADR 0008).
    PfInstall {
        http_port: u16,
        https_port: u16,
    },
    PfRemove,
    /// Linux: send `~test` to the DNS server of `marsad` through
    /// systemd-resolved.
    ResolvedInstall {
        port: u16,
    },
    ResolvedRemove,
    /// Linux: let a normal process bind port 80 and up, with the sysctl
    /// `net.ipv4.ip_unprivileged_port_start` (ADR 0014). Caddy then binds
    /// ports 80 and 443 itself. No field: the helper holds the file, the
    /// key, and the value as constants.
    PortsInstall,
    PortsRemove,
    /// Windows: one hosts file line per name, inside Marsa's own marked
    /// block. Every name is `<site>.test`, sorted, each once: the helper
    /// rejects a duplicate.
    HostsSync {
        names: Vec<String>,
    },
    HostsRemove,
    /// Every `*_remove` operation of the current system.
    UninstallAll,
}

impl HelperOp {
    /// The name of this operation: its `op` tag in JSON, and the `op` of its
    /// result.
    pub fn name(&self) -> OpName {
        match self {
            Self::TrustInstall { .. } => OpName::TrustInstall,
            Self::TrustRemove => OpName::TrustRemove,
            Self::ResolverInstall { .. } => OpName::ResolverInstall,
            Self::ResolverRemove => OpName::ResolverRemove,
            Self::PfInstall { .. } => OpName::PfInstall,
            Self::PfRemove => OpName::PfRemove,
            Self::ResolvedInstall { .. } => OpName::ResolvedInstall,
            Self::ResolvedRemove => OpName::ResolvedRemove,
            Self::PortsInstall => OpName::PortsInstall,
            Self::PortsRemove => OpName::PortsRemove,
            Self::HostsSync { .. } => OpName::HostsSync,
            Self::HostsRemove => OpName::HostsRemove,
            Self::UninstallAll => OpName::UninstallAll,
        }
    }
}

/// The name of a step in a setup or uninstall report: a helper operation,
/// or one of the daemon's own steps, which have no [`HelperOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpName {
    TrustInstall,
    TrustRemove,
    ResolverInstall,
    ResolverRemove,
    PfInstall,
    PfRemove,
    ResolvedInstall,
    ResolvedRemove,
    PortsInstall,
    PortsRemove,
    HostsSync,
    HostsRemove,
    UninstallAll,
    /// The daemon's own: the CA root out of the current user's trust
    /// (macOS, ADR 0015). Not the helper's `trust_remove`, which the same
    /// uninstall reports too, for a trust added as root before that ADR.
    UserTrustRemove,
    /// The daemon's own: the CA root in, or out of, the user's NSS
    /// databases, for Chrome and Firefox (Linux).
    NssInstall,
    NssRemove,
    /// The daemon's own: no more start at login.
    AutostartRemove,
}

impl OpName {
    /// Every name, for the tests that check the JSON text.
    pub const ALL: [Self; 17] = [
        Self::TrustInstall,
        Self::TrustRemove,
        Self::ResolverInstall,
        Self::ResolverRemove,
        Self::PfInstall,
        Self::PfRemove,
        Self::ResolvedInstall,
        Self::ResolvedRemove,
        Self::PortsInstall,
        Self::PortsRemove,
        Self::HostsSync,
        Self::HostsRemove,
        Self::UninstallAll,
        Self::UserTrustRemove,
        Self::NssInstall,
        Self::NssRemove,
        Self::AutostartRemove,
    ];

    /// The JSON text, for logs and for a message that shows the name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TrustInstall => "trust_install",
            Self::TrustRemove => "trust_remove",
            Self::ResolverInstall => "resolver_install",
            Self::ResolverRemove => "resolver_remove",
            Self::PfInstall => "pf_install",
            Self::PfRemove => "pf_remove",
            Self::ResolvedInstall => "resolved_install",
            Self::ResolvedRemove => "resolved_remove",
            Self::PortsInstall => "ports_install",
            Self::PortsRemove => "ports_remove",
            Self::HostsSync => "hosts_sync",
            Self::HostsRemove => "hosts_remove",
            Self::UninstallAll => "uninstall_all",
            Self::UserTrustRemove => "user_trust_remove",
            Self::NssInstall => "nss_install",
            Self::NssRemove => "nss_remove",
            Self::AutostartRemove => "autostart_remove",
        }
    }
}

impl std::fmt::Display for OpName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the helper prints on stdout.
///
/// A request that runs has `results`, one per operation. A rejected request
/// (bad JSON, an unknown field, a port below 1024) has `error` and runs
/// nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelperResponse {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<OpResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<HelperFailure>,
}

/// The result of one operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpResult {
    /// The operation, for example `resolver_install`. `uninstall_all`
    /// reports one result per operation it ran.
    pub op: OpName,
    pub status: OpStatus,
    /// Set when `status` is `failed`: a stable name such as
    /// `resolver_foreign`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// English text for the logs. The UI shows a text chosen by `code`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpStatus {
    /// The system was changed.
    Done,
    /// Nothing to do: the system was already as asked.
    Unchanged,
    Failed,
}

/// Why the helper rejected the whole request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelperFailure {
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn fixture_files(kind: &str) -> Vec<PathBuf> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
            .join("helper")
            .join(kind);
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
            .expect("read fixtures/helper")
            .map(|entry| entry.expect("entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        files.sort();
        assert!(!files.is_empty(), "no fixtures in {}", dir.display());
        files
    }

    fn read_json(path: &Path) -> serde_json::Value {
        let text = fs::read_to_string(path).expect("read fixture");
        serde_json::from_str(&text).expect("fixture is JSON")
    }

    /// The helper parses the same files (crates/helper), so a field renamed
    /// on one side only breaks one of the two test suites.
    #[test]
    fn requests_match_the_helper_fixtures() {
        for path in fixture_files("requests") {
            let json = read_json(&path);
            let request: HelperRequest = serde_json::from_value(json.clone())
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_eq!(request.version, HELPER_PROTOCOL_VERSION);
            let back = serde_json::to_value(&request).expect("serialize");
            assert_eq!(back, json, "{}", path.display());
        }
    }

    #[test]
    fn responses_match_the_helper_fixtures() {
        for path in fixture_files("responses") {
            let json = read_json(&path);
            let response: HelperResponse = serde_json::from_value(json.clone())
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let back = serde_json::to_value(&response).expect("serialize");
            assert_eq!(back, json, "{}", path.display());
        }
    }

    #[test]
    fn request_json_shape() {
        let request = HelperRequest::new(vec![
            HelperOp::PfInstall {
                http_port: 18080,
                https_port: 18443,
            },
            HelperOp::UninstallAll,
        ]);
        assert_eq!(
            serde_json::to_value(&request).expect("serialize"),
            serde_json::json!({
                "version": 1,
                "ops": [
                    { "op": "pf_install", "http_port": 18080, "https_port": 18443 },
                    { "op": "uninstall_all" },
                ],
            })
        );
    }

    #[test]
    fn op_names_are_their_json_text() {
        for name in OpName::ALL {
            let json = serde_json::to_value(name).expect("serialize");
            assert_eq!(json, serde_json::json!(name.as_str()));
            let back: OpName = serde_json::from_value(json).expect("parse");
            assert_eq!(back, name);
        }
    }

    /// `name` gives the `op` tag that serde writes, for every operation.
    #[test]
    fn helper_op_names_are_their_tags() {
        let ops = [
            HelperOp::TrustInstall { pem: String::new() },
            HelperOp::TrustRemove,
            HelperOp::ResolverInstall { port: 1053 },
            HelperOp::ResolverRemove,
            HelperOp::PfInstall {
                http_port: 18080,
                https_port: 18443,
            },
            HelperOp::PfRemove,
            HelperOp::ResolvedInstall { port: 1053 },
            HelperOp::ResolvedRemove,
            HelperOp::PortsInstall,
            HelperOp::PortsRemove,
            HelperOp::HostsSync { names: Vec::new() },
            HelperOp::HostsRemove,
            HelperOp::UninstallAll,
        ];
        for op in ops {
            let json = serde_json::to_value(&op).expect("serialize");
            assert_eq!(json["op"], serde_json::json!(op.name().as_str()), "{op:?}");
        }
    }

    #[test]
    fn unknown_op_is_rejected() {
        let json = serde_json::json!({ "version": 1, "ops": [{ "op": "run", "cmd": "id" }] });
        assert!(serde_json::from_value::<HelperRequest>(json).is_err());
    }
}
