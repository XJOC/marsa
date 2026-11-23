//! The report of `marsa setup`: what the setup changed and what it skipped.

use serde::{Deserialize, Serialize};

use super::{ApiError, OpName, OpResult};

/// The answer to `POST /v1/setup`, and the data of `setup_finished`.
///
/// Setup sends one request to `marsa-helper`, behind the admin prompt. On
/// macOS the daemon then asks the user to approve the trust in Marsa's
/// certificate authority for this user, in the dialog macOS shows (ADR
/// 0015). An operation it cannot do safely is left out of that request and
/// listed in `skipped`; the others still run. On Linux a second request may
/// follow, behind a second prompt: the operations of the
/// hosts file when the helper refused the `marsa0` link, or `hosts_remove`
/// once the link is made.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupReport {
    /// The helper's result for each operation it ran, in order, then, on
    /// macOS, the daemon's own `trust_install` result: the trust for the
    /// current user (ADR 0015). Empty if nothing ran: no operation was left,
    /// or `error` says why. On Linux the results of a second request follow
    /// those of the first, then the daemon's own `nss_install`: the CA root
    /// in the user's NSS databases, for Chrome and Firefox.
    #[serde(default)]
    pub results: Vec<OpResult>,
    /// The operations left out of the request, and why.
    #[serde(default)]
    pub skipped: Vec<SetupSkip>,
    /// The programs that listen on port 80 or 443 and take loopback
    /// connections. Any of them means no `pf_install`: the redirect would
    /// take their traffic without a word (ADR 0008, "Herd conflict").
    #[serde(default)]
    pub port_holders: Vec<PortHolder>,
    /// Why the helper did not run, or why its answer cannot be used:
    /// `setup_cancelled`, `setup_not_authorized`, `setup_timed_out`,
    /// `setup_interrupted`, `setup_helper_missing`, `setup_helper_unsafe`,
    /// `setup_helper_protected_folder`, `setup_prompt_failed`,
    /// `setup_helper_failed`, or `setup_helper_rejected`; on Linux also
    /// `setup_pkexec_missing`, `setup_caddy_ports_failed` when the helper
    /// allowed ports 80 and 443 but Caddy did not take them, and
    /// `setup_hosts_not_written` or `setup_hosts_not_removed` when the
    /// second request did not run (the results of the first stand). `None`
    /// if the helper answered, or was not needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

/// The answer to `GET /v1/hosts`: whether this system puts
/// `.test` names in its hosts file, and which sites Marsa's block there
/// lacks. `POST /v1/jobs/hosts-sync` adds them, behind the password prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostsStatus {
    /// `true` on Linux where this data folder's `marsa setup` put Marsa's
    /// block in the hosts file, and the block is still there. `false`
    /// before that setup, on the `marsa0` link, on macOS, and on Windows
    /// (its helper service keeps the block up to date by itself).
    pub used: bool,
    /// The host names of the sites that are not in the block, sorted. Such
    /// a site does not open until they are added. Empty when `used` is
    /// `false`.
    #[serde(default)]
    pub missing: Vec<String>,
}

/// An operation setup left out of the helper request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupSkip {
    /// The operation, for example `pf_install`.
    pub op: OpName,
    /// Why: `redirect_port_held` and `ports_not_checked` for `pf_install`,
    /// `ca_not_ready`, `ca_unreadable`, `caddy_not_installed`, and
    /// `caddy_not_started` for `trust_install`; on Linux
    /// `resolved_use_hosts` and `resolved_not_running` for
    /// `resolved_install`, `hosts_sites_unknown` and
    /// `hosts_too_many_names` for `hosts_sync`, and `nss_tool_missing` for
    /// `nss_install`.
    pub code: String,
}

/// A program that listens on port 80 or 443, from `netstat` and `ps`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortHolder {
    pub port: u16,
    /// As netstat prints it: `127.0.0.1`, `::1`, or `*` for every address.
    pub address: String,
    /// The process name netstat gives, at most 16 characters
    /// (`nginx-arm64`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// The user of the process, from `ps`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// The program from `ps`: a path, or the title the process set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The app it seems to belong to (`Herd`, `Docker`), a hint only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
}
