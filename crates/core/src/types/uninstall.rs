//! The report of `marsa uninstall --all`, with its JSON tests.

use serde::{Deserialize, Serialize};

use super::{ApiError, OpResult};
// Only the tests below build these.
#[cfg(test)]
use super::{OpName, OpStatus};

/// What `marsa uninstall --all` removed from the system: the `done` of the
/// uninstall job, and the answer of the old `POST /v1/uninstall`.
///
/// One admin prompt runs the helper's `uninstall_all`. Only if the helper
/// ran does the daemon go on: it takes away the current user's trust in
/// Marsa's certificate authority (macOS, ADR 0015) or deletes this data
/// folder's roots from the user's NSS databases (Linux, ADR 0014), deletes
/// the login file where the system has one (no `autostart_remove`
/// otherwise), answers, and then stops, with Caddy, the PHP pools, and the
/// DNS server. The data folder stays: the CLI asks about it on its own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UninstallReport {
    /// One result per step, in order: the helper's removals (`pf_remove`,
    /// `resolver_remove`, `trust_remove` on macOS; `ports_remove`,
    /// `resolved_remove`, `hosts_remove`, `trust_remove` on Linux), then the
    /// daemon's own `user_trust_remove` (macOS) or `nss_remove` (Linux), and
    /// `autostart_remove`. Empty when `error` says
    /// why nothing ran.
    #[serde(default)]
    pub results: Vec<OpResult>,
    /// Why the helper did not run, or why its answer cannot be used:
    /// `uninstall_cancelled`, `uninstall_not_authorized`,
    /// `uninstall_timed_out`, `uninstall_interrupted`,
    /// `uninstall_helper_missing`, `uninstall_helper_unsafe`,
    /// `uninstall_prompt_failed`, `uninstall_helper_failed`, or
    /// `uninstall_helper_rejected`; on Linux also `uninstall_pkexec_missing`.
    /// Nothing after the helper ran then, and the
    /// daemon keeps running. `None` if the helper answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
    /// The daemon stops right after this answer: `true` exactly when `error`
    /// is `None`.
    #[serde(default)]
    pub stopping: bool,
}

/// The JSON of [`UninstallReport`], next to it, like the autostart tests.
#[cfg(test)]
mod uninstall_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_uninstall_report_and_a_cancelled_one() {
        let report = UninstallReport {
            results: vec![OpResult {
                op: OpName::UserTrustRemove,
                status: OpStatus::Unchanged,
                code: None,
                message: None,
            }],
            error: None,
            stopping: true,
        };
        let value = json!({
            "results": [{ "op": "user_trust_remove", "status": "unchanged" }],
            "stopping": true,
        });
        assert_eq!(serde_json::to_value(&report).unwrap(), value);
        assert_eq!(
            serde_json::from_value::<UninstallReport>(value).unwrap(),
            report
        );

        let cancelled = UninstallReport {
            error: Some(ApiError {
                code: "uninstall_cancelled".into(),
                message: "the admin prompt was cancelled".into(),
            }),
            ..UninstallReport::default()
        };
        let value = json!({
            "results": [],
            "error": { "code": "uninstall_cancelled", "message": "the admin prompt was cancelled" },
            "stopping": false,
        });
        assert_eq!(serde_json::to_value(&cancelled).unwrap(), value);
        // Every field may be left out.
        assert_eq!(
            serde_json::from_value::<UninstallReport>(json!({})).unwrap(),
            UninstallReport::default()
        );
    }
}
