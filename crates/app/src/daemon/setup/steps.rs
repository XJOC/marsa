//! What setup does after the checks, on a blocking thread. The daemon
//! gathers the facts, [`marsa_core::setup::plan`] decides (ADR 0026), and
//! this file runs the plan: the helper's requests, then the steps `marsad`
//! does itself (the user's trust on macOS, the NSS databases on Linux), and
//! the Windows service. On Linux, the way of [`super::linux_dns`] that the
//! user's system allows: the `marsa0` link, or the hosts file where
//! systemd-resolved does not run (ADR 0014); the helper stays the judge and
//! setup sends the hosts operations in a second request if it says so.

use super::Flow;
use super::Setup;
use super::ca::StartCaddy;
use super::failure;
use super::linux_dns;
use super::nss;
use super::skip;
use super::user_trust;
use super::windows;
use marsa_core::caddy::CaddyInput;
use marsa_core::caddy::CaddyPaths;
use marsa_core::codes;
use marsa_core::dns;
use marsa_core::helper::HelperOp;
use marsa_core::helper::OpResult;
use marsa_core::helper::OpStatus;
use marsa_core::platform::ports;
use marsa_core::recipes::Platform;
use marsa_core::setup;
use marsa_core::setup::Facts;
use marsa_core::setup::FollowUp;
use marsa_core::setup::LocalStep;
use marsa_core::setup::Plan;
use marsa_core::types::ApiError;
use marsa_core::types::{OpName, SetupReport, SetupSkip};
use std::path::Path;
use tokio_util::sync::CancellationToken;

/// Whether the helper's `ports_install` is done, now or before.
pub(super) fn low_ports_set(report: &SetupReport) -> bool {
    report.results.iter().any(|result| {
        result.op == OpName::PortsInstall
            && matches!(result.status, OpStatus::Done | OpStatus::Unchanged)
    })
}

/// Whether `path` is inside `dir`, folder by folder, with no regard to case,
/// as Windows compares names (`C:\Users\Omar` and `c:\users\omar`).
pub(super) fn inside(path: &Path, dir: &Path) -> bool {
    let mut path = path.components();
    dir.components().all(|part| {
        path.next().is_some_and(|other| {
            part.as_os_str().to_string_lossy().to_lowercase()
                == other.as_os_str().to_string_lossy().to_lowercase()
        })
    })
}

/// What the plan leaves out and why, for the log. Not the facts as a
/// whole: they hold the CA text.
fn log_plan(facts: &Facts, plan: &Plan) {
    if let Some(linux) = facts.linux_dns {
        tracing::info!(
            path = ?dns::linux_path(linux),
            block = facts.hosts_block,
            "the way of .test names here"
        );
    }
    log_skips(&plan.skips);
    for holder in &plan.port_holders {
        tracing::warn!(
            port = holder.port,
            process = ?holder.process,
            pid = ?holder.pid,
            app = ?holder.app,
            "a program listens on a port the pf redirect would take; no pf_install"
        );
    }
}

/// One warning for each step the setup leaves out, of either request.
fn log_skips(skips: &[SetupSkip]) {
    for left_out in skips {
        tracing::warn!(op = %left_out.op, code = %left_out.code, "setup leaves this step out");
    }
}

impl Setup {
    /// The setup itself. Blocking: it runs programs and reads a file.
    /// `starter` starts Caddy when its CA root is not there yet. `names`:
    /// the host names of the sites, sorted, for the hosts file on Linux and
    /// Windows; `None` if the site list is not known. `dns_down`: why the
    /// daemon's DNS server does not answer (`DnsService::not_serving`); the
    /// DNS operation is then skipped with that code.
    #[expect(
        clippy::too_many_arguments,
        reason = "the inputs of one setup; a struct would only rename them"
    )]
    pub(super) fn run(
        &self,
        data_dir: &Path,
        caddy: CaddyInput,
        platform: Platform,
        starter: &dyn StartCaddy,
        names: Option<&[String]>,
        dns_down: Option<&str>,
        stop: &CancellationToken,
    ) -> SetupReport {
        let facts = self.facts(data_dir, caddy, platform, starter, names, dns_down, stop);
        let plan = setup::plan(&facts);
        log_plan(&facts, &plan);
        let Plan {
            ops,
            skips,
            port_holders,
            local,
        } = plan;
        let mut report = SetupReport {
            skipped: skips,
            port_holders,
            ..SetupReport::default()
        };
        match self.apply(ops, platform, stop) {
            Ok(results) => report.results = results,
            Err(error) => report.error = Some(error),
        }
        // Linux: a second request, if the helper's answer asks for one.
        if report.error.is_none()
            && let Some(follow_up) = setup::after_helper(&facts, &report.results)
        {
            self.follow_up(&mut report, follow_up, platform, stop);
        }
        for step in local {
            match step {
                // Only if the helper ran as asked, or was not needed: a
                // cancelled or refused admin prompt ends setup with nothing
                // changed in the system, and no second dialog. Setup may have
                // started Caddy and made its CA by then: Marsa's own process
                // and data.
                LocalStep::UserTrust { pem } if report.error.is_none() => {
                    let files = user_trust::TrustFiles::in_dir(data_dir);
                    report.results.push(self.trust_for_user(&pem, &files, stop));
                }
                LocalStep::NssInstall { pem } if report.error.is_none() => {
                    self.nss_install(&mut report, &pem, data_dir, stop);
                }
                LocalStep::UserTrust { .. } | LocalStep::NssInstall { .. } => {}
                LocalStep::RecordDnsWay => linux_dns::record_way(data_dir, &report.results),
            }
        }
        report
    }

    /// The facts [`setup::plan`] needs, as far as [`setup::needs`] asks for
    /// them on this system. The CA comes first: it may start Caddy.
    #[expect(
        clippy::too_many_arguments,
        reason = "the inputs of one setup; a struct would only rename them"
    )]
    fn facts(
        &self,
        data_dir: &Path,
        caddy: CaddyInput,
        platform: Platform,
        starter: &dyn StartCaddy,
        names: Option<&[String]>,
        dns_down: Option<&str>,
        stop: &CancellationToken,
    ) -> Facts {
        let needs = setup::needs(platform.os());
        // First, and on its own line: it may start Caddy, and the order of
        // the other facts must not move before it.
        let ca = self.ca(&CaddyPaths::in_dir(data_dir).ca_root(), starter, stop);
        Facts {
            platform,
            ca,
            caddy_ports: (caddy.http_port, caddy.https_port),
            port_holders: if needs.port_holders {
                ports::holders(self.runner(), platform.os(), stop)
            } else {
                None
            },
            linux_dns: needs
                .linux_dns
                .then(|| linux_dns::facts(self.runner.as_ref(), stop)),
            hosts_block: needs.hosts_block
                && self
                    .hosts_file
                    .as_deref()
                    .and_then(linux_dns::read_block)
                    .is_some(),
            names: names.map(<[String]>::to_vec),
            dns_down: dns_down.map(str::to_owned),
            nss: self.nss,
        }
    }

    /// Linux: the second request that [`setup::after_helper`] asks for. If
    /// the helper chose the hosts file, `resolved_install` moves from the
    /// results to the skipped steps with the helper's code. When the request
    /// does not run, the error says that the steps before it are done:
    /// [`codes::SETUP_HOSTS_NOT_WRITTEN`] or [`codes::SETUP_HOSTS_NOT_REMOVED`].
    fn follow_up(
        &self,
        report: &mut SetupReport,
        follow_up: FollowUp,
        platform: Platform,
        stop: &CancellationToken,
    ) {
        if let Some((op, code)) = &follow_up.retire {
            tracing::warn!(
                code,
                "marsad expected the marsa0 link, but marsa-helper chose the hosts file; \
                 sending the operations of the hosts way"
            );
            report.results.retain(|result| result.op != *op);
            report.skipped.push(skip(*op, code));
        }
        log_skips(&follow_up.skips);
        report.skipped.extend(follow_up.skips);
        if follow_up.ops.is_empty() {
            return;
        }
        let ops: Vec<OpName> = follow_up.ops.iter().map(HelperOp::name).collect();
        tracing::info!(
            ?ops,
            "a second request to marsa-helper, after the first one's answer"
        );
        match self.apply(follow_up.ops, platform, stop) {
            Ok(results) => report.results.extend(results),
            // The helper may have run: their texts say so.
            Err(error)
                if matches!(
                    error.code.as_str(),
                    codes::SETUP_TIMED_OUT | codes::SETUP_INTERRUPTED
                ) =>
            {
                report.error = Some(error);
            }
            Err(error) => {
                tracing::warn!(
                    code = %error.code,
                    message = %error.message,
                    "the second request of setup did not run"
                );
                let message = format!(
                    "the steps before are done, but the second request did not run: {}",
                    error.code
                );
                report.error = Some(failure(follow_up.not_run, message));
            }
        }
    }

    /// The user's trust of the CA root whose checked text is `pem` (ADR
    /// 0015), through the trust folder `files`; or `trust_add_failed` when
    /// the home folder, and so the login keychain, is not known.
    fn trust_for_user(
        &self,
        pem: &str,
        files: &user_trust::TrustFiles,
        stop: &CancellationToken,
    ) -> OpResult {
        match &self.home {
            Some(home) => user_trust::install(self.runner.as_ref(), home, pem, files, stop),
            None => {
                tracing::warn!(
                    "the home folder is not known, so the login keychain cannot be found"
                );
                let message = "the home folder of the current user is not known, so the \
                               login keychain cannot be found";
                user_trust::failed(OpName::TrustInstall, codes::TRUST_ADD_FAILED, message)
            }
        }
    }

    /// Linux: the CA root whose checked text is `pem` into the user's NSS
    /// databases ([`nss`]), as a result or a skipped step of `report`.
    pub(super) fn nss_install(
        &self,
        report: &mut SetupReport,
        pem: &str,
        data_dir: &Path,
        stop: &CancellationToken,
    ) {
        let Some(home) = &self.home else {
            tracing::warn!("the home folder is not known, so the NSS databases cannot be found");
            let message = "the home folder of the current user is not known";
            let failed = user_trust::failed(OpName::NssInstall, codes::NSS_ADD_FAILED, message);
            report.results.push(failed);
            return;
        };
        let files = user_trust::TrustFiles::in_dir(data_dir);
        match nss::install(self.runner.as_ref(), home, pem, &files, stop) {
            nss::Outcome::Ran(result) => report.results.push(result),
            nss::Outcome::Skipped(code) => report.skipped.push(skip(OpName::NssInstall, code)),
        }
    }

    /// [`Setup::launch`] for the setup.
    pub(super) fn apply(
        &self,
        ops: Vec<HelperOp>,
        platform: Platform,
        stop: &CancellationToken,
    ) -> Result<Vec<OpResult>, ApiError> {
        self.launch(Flow::Setup, ops, platform, stop)
    }

    /// `marsa uninstall --all` on Windows: the checked helper's `uninstall`
    /// behind the UAC prompt ([`windows::uninstall`]).
    ///
    /// # Errors
    ///
    /// The `uninstall_*` codes of the preflight and of the helper.
    pub(in crate::daemon) fn uninstall_windows(
        &self,
        stop: &CancellationToken,
    ) -> Result<OpResult, ApiError> {
        let helper = self.checked_helper(Flow::Uninstall, Platform::WindowsX64)?;
        let Some(service) = self.windows.as_deref() else {
            tracing::error!("no helper service on this system");
            return Err(failure(
                codes::UNINSTALL_HELPER_FAILED,
                "no helper service here",
            ));
        };
        windows::uninstall(service, helper, stop)
    }
}

#[cfg(test)]
mod tests;
