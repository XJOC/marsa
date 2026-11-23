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
use marsa_core::recipes::Os;
use marsa_core::recipes::Platform;
use marsa_core::setup;
use marsa_core::setup::Facts;
use marsa_core::setup::FollowUp;
use marsa_core::setup::LocalStep;
use marsa_core::setup::Plan;
use marsa_core::types::ApiError;
use marsa_core::types::{OpName, SetupReport};
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
    for left_out in &plan.skips {
        tracing::warn!(op = %left_out.op, code = %left_out.code, "setup leaves this step out");
    }
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
                ports::holders(self.runner(), Os::Mac, stop)
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
        report.skipped.extend(follow_up.skips);
        if follow_up.ops.is_empty() {
            return;
        }
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
mod tests {
    use super::super::ca::CA_WAIT;
    use super::super::helper::INSTALL_BIN;
    use super::super::osascript::OSASCRIPT;

    use marsa_core::platform::ports::NETSTAT;
    use marsa_core::platform::runner::PIPE_GRACE;

    use marsa_core::platform::runner::RunOutput;

    use super::super::test_support::*;
    use super::super::user_trust::SECURITY;
    use super::super::*;
    use super::*;

    use crate::daemon::sites::hosts_sync;
    use crate::daemon::state::Daemon;

    use axum::http::StatusCode;

    use marsa_core::caddy::CaddyInput;

    use marsa_core::helper::HelperOp;
    use marsa_core::helper::HelperRequest;
    use marsa_core::helper::OpResult;
    use marsa_core::platform::daemon_file::daemon_authority;

    use marsa_core::recipes::Platform;
    use marsa_core::types::Event;
    use marsa_core::types::OpStatus;
    use marsa_core::types::SetupReport;
    use serde_json::{Value, json};
    use std::fs;
    use std::io;

    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::Arc;

    use std::time::Duration;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn the_user_trust_commands_are_pinned_and_come_after_the_helper() {
        let dir = Dir::new(true);
        let answer = all_done(&[OpName::ResolverInstall, OpName::PfInstall]);
        let fake = Fake::scripted(netstat_free(), move || ok(&answer));
        let report = dir.run(&fake);

        // The request has no trust_install: on macOS the helper does not
        // trust the CA (ADR 0015), and the user's trust comes after it.
        assert_eq!(fake.request().ops, [resolver(), pf(18080, 18443)]);
        assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT, SECURITY, SECURITY]);
        // `security` gets the checked copy of Caddy's root in the trust
        // folder, not `root.crt` again; it is the record now.
        let checked = dir.dir.path().join("trust").join("pending.pem");
        assert_ne!(checked, dir.ca_root());
        let ca_root = checked.to_str().unwrap();
        let record = dir.dir.path().join("trust").join("user-trusted.pem");
        assert_eq!(fs::read_to_string(record).unwrap(), TEST_CA);
        let keychain = dir
            .home()
            .join("Library")
            .join("Keychains")
            .join("login.keychain-db");
        let keychain = keychain.to_str().unwrap();
        let security = fake.security_calls();
        // The absolute program, written out: `RealRunner` refuses a bare
        // name, so a constant that lost its path would fail only on a Mac.
        assert_eq!(security[0].program, "/usr/bin/security");
        assert_eq!(security[1].program, "/usr/bin/security");
        assert_eq!(security[0].args, ["verify-cert", "-c", ca_root, "-L", "-l"]);
        assert_eq!(security[0].limit, Duration::from_secs(10));
        assert_eq!(
            security[1].args,
            [
                "add-trusted-cert",
                "-r",
                "trustRoot",
                "-k",
                keychain,
                ca_root
            ]
        );
        assert_eq!(security[1].limit, Duration::from_secs(300));
        assert!(Path::new(ca_root).is_absolute() && Path::new(keychain).is_absolute());

        // The trust result comes after the helper's, under the helper's op
        // name, so the CLI prints it like the others.
        assert_eq!(
            report.results,
            [
                done(OpName::ResolverInstall),
                done(OpName::PfInstall),
                done(OpName::TrustInstall)
            ]
        );
        assert!(report.skipped.is_empty());
        assert_eq!(report.error, None);
    }

    #[test]
    fn a_ca_the_user_trusts_already_is_unchanged_and_nothing_is_added() {
        let dir = Dir::new(true);
        let answer = all_done(&[OpName::ResolverInstall, OpName::PfInstall]);
        let fake = Fake::answering(move |program, args| match program {
            NETSTAT => ok(netstat_free()),
            OSASCRIPT => ok(&answer),
            SECURITY if args[0] == "verify-cert" => ok("...certificate verification successful.\n"),
            other => panic!("{other} {args:?} ran"),
        });
        let report = dir.run(&fake);

        assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT, SECURITY]);
        let trust = report.results.last().unwrap();
        assert_eq!(trust.op, OpName::TrustInstall);
        assert_eq!(trust.status, OpStatus::Unchanged);
        assert_eq!(report.results.len(), 3);
        assert_eq!(report.error, None);
    }

    #[test]
    fn a_dialog_refused_or_left_open_is_trust_add_failed_and_the_helper_ops_stay() {
        /// What `add-trusted-cert` prints when the dialog is refused.
        const REFUSED: &str = "SecTrustSettingsSetTrustSettings: The authorization was denied \
                               since no user interaction was possible.\n";
        /// What `add-trusted-cert` gives back.
        type Add = fn() -> io::Result<RunOutput>;
        // The case, the add, and the words the message must hold.
        let cases: [(&str, Add, &str); 4] = [
            ("refused", || exit(1, REFUSED), "exited with code 1"),
            (
                "left open",
                || Err(io::Error::from(io::ErrorKind::TimedOut)),
                "did not finish in 5 minutes",
            ),
            (
                "daemon stopping",
                || Err(io::Error::from(io::ErrorKind::Interrupted)),
                "was stopped with the daemon",
            ),
            (
                "security missing",
                || Err(io::Error::from(io::ErrorKind::NotFound)),
                "could not run",
            ),
        ];
        for (name, add, how) in cases {
            let dir = Dir::new(true);
            let fake = Fake::answering(move |program, args| match program {
                NETSTAT => ok(netstat_free()),
                OSASCRIPT => ok(DONE),
                SECURITY if args[0] == "verify-cert" => exit(1, NOT_TRUSTED),
                SECURITY => add(),
                other => panic!("{other} ran"),
            });
            let report = dir.run(&fake);

            // The helper's results stay, and the setup has no error: the
            // failed step is in the results, like a failed helper op.
            assert_eq!(report.error, None, "{name}");
            assert_eq!(report.results.len(), 3, "{name}");
            assert_eq!(report.results[..2], results(DONE)[..], "{name}");
            let trust = &report.results[2];
            assert_eq!(trust.op, OpName::TrustInstall, "{name}");
            assert_eq!(trust.status, OpStatus::Failed, "{name}");
            assert_eq!(trust.code.as_deref(), Some("trust_add_failed"), "{name}");
            let message = trust.message.as_deref().unwrap().to_ascii_lowercase();
            // What to do: the dialog shows in the daemon's session, so a
            // daemon started over SSH has to stop first.
            for part in [
                how,
                "in terminal on the mac's desktop",
                "approve the dialog",
                "started over ssh",
                "\"marsa daemon stop\" first",
            ] {
                assert!(message.contains(part), "{name}: {message}");
            }
            // Nothing `security` printed is in the message.
            assert!(!message.contains("authorization"), "{name}: {message}");
            assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT, SECURITY, SECURITY]);
        }
    }

    #[test]
    fn no_home_folder_is_trust_add_failed_after_the_helper() {
        let dir = Dir::new(true);
        let answer = all_done(&[OpName::ResolverInstall, OpName::PfInstall]);
        let fake = Fake::scripted(netstat_free(), move || ok(&answer));
        let setup = Setup::with(
            fake.clone(),
            Some(Platform::DarwinArm64),
            Some(dir.helper()),
            None,
        );
        let stop = CancellationToken::new();
        let report = setup.run(
            dir.dir.path(),
            CaddyInput::default(),
            Platform::DarwinArm64,
            dir.caddy.as_ref(),
            Some(&[]),
            None,
            &stop,
        );

        assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT]);
        assert_eq!(report.results.len(), 3);
        let trust = &report.results[2];
        assert_eq!(trust.op, OpName::TrustInstall);
        assert_eq!(trust.code.as_deref(), Some("trust_add_failed"));
        assert_eq!(report.error, None);
    }

    #[test]
    fn a_helper_in_a_protected_folder_is_refused_before_any_prompt() {
        // `desktop`: the usual macOS disk takes it for `Desktop`.
        for folder in ["Desktop", "Documents", "Downloads", "desktop"] {
            let dir = Dir::new(true);
            let helper = dir
                .home()
                .join(folder)
                .join("marsa")
                .join("bin")
                .join("marsa-helper");
            make_helper(&helper);
            let fake = Fake::scripted(netstat_free(), || panic!("osascript ran"));
            let setup = Setup::with(
                fake.clone(),
                Some(Platform::DarwinArm64),
                Some(helper),
                Some(dir.home()),
            );
            let stop = CancellationToken::new();
            let report = setup.run(
                dir.dir.path(),
                CaddyInput::default(),
                Platform::DarwinArm64,
                dir.caddy.as_ref(),
                Some(&[]),
                None,
                &stop,
            );

            let error = report.error.as_ref().expect(folder);
            assert_eq!(error.code, "setup_helper_protected_folder", "{folder}");
            for part in [folder, INSTALL_BIN, "marsa daemon stop", "marsa setup"] {
                assert!(error.message.contains(part), "{folder}: {}", error.message);
            }
            assert!(report.results.is_empty(), "{folder}");
            // No admin prompt, and no trust dialog after it either.
            assert_eq!(fake.programs(), [NETSTAT], "{folder}");
        }

        // By the path alone, before any look at the file: a helper that is
        // not there gets the same answer, not `setup_helper_missing`.
        let dir = Dir::new(true);
        let helper = dir.home().join("Downloads").join("marsa-helper");
        let fake = Fake::scripted(netstat_free(), || panic!("osascript ran"));
        let setup = Setup::with(
            fake.clone(),
            Some(Platform::DarwinArm64),
            Some(helper),
            Some(dir.home()),
        );
        let stop = CancellationToken::new();
        let report = setup.run(
            dir.dir.path(),
            CaddyInput::default(),
            Platform::DarwinArm64,
            dir.caddy.as_ref(),
            Some(&[]),
            None,
            &stop,
        );
        assert_eq!(error_code(&report), Some("setup_helper_protected_folder"));
        assert_eq!(fake.programs(), [NETSTAT]);
    }

    fn linux_setup(fake: &Arc<Fake>, helper: PathBuf, home: PathBuf) -> Setup {
        Setup::with(
            fake.clone(),
            Some(Platform::LinuxX64),
            Some(helper),
            Some(home),
        )
    }

    fn run_linux(setup: &Setup, dir: &Dir) -> SetupReport {
        run_linux_with(setup, dir, &[])
    }

    /// Setup on Linux with the sites `names`.
    fn run_linux_with(setup: &Setup, dir: &Dir, names: &[&str]) -> SetupReport {
        let stop = CancellationToken::new();
        let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
        setup.run(
            dir.dir.path(),
            CaddyInput::default(),
            Platform::LinuxX64,
            dir.caddy.as_ref(),
            Some(&names),
            None,
            &stop,
        )
    }

    /// ADR 0011 stays on Linux: the request carries the certificate. ADR
    /// 0014: `ports_install` instead of a port check and `pf_install`. The
    /// helper runs through `pkexec`, with no shell, and nothing else runs:
    /// no `netstat`, no `security`, and no protected folder check (the
    /// helper here is in `~/Desktop`).
    #[test]
    fn linux_sends_trust_resolved_and_ports_through_pkexec() {
        let dir = Dir::new(true);
        let helper = dir.home().join("Desktop").join("marsa-helper");
        make_helper(&helper);
        let answer = all_done(&[
            OpName::TrustInstall,
            OpName::ResolvedInstall,
            OpName::PortsInstall,
        ]);
        let fake = linux_fake(move || ok(&answer));
        let report = run_linux(&linux_setup(&fake, helper.clone(), dir.home()), &dir);

        let calls = fake.calls();
        assert_eq!(
            fake.programs(),
            [
                "/usr/bin/systemctl",
                "/usr/bin/systemctl",
                "/usr/bin/pkexec"
            ]
        );
        // systemd 255: networkd does not count.
        assert_eq!(calls[0].args, ["--version"]);
        assert_eq!(
            calls[1].args,
            ["is-active", "--quiet", "systemd-resolved.service"]
        );
        assert_eq!(calls[1].limit, Duration::from_secs(10));
        let calls = &calls[2..];
        assert_eq!(calls[0].args.len(), 4);
        assert_eq!(calls[0].args[0], "--disable-internal-agent");
        assert_eq!(calls[0].args[1], helper.to_str().unwrap());
        assert_eq!(calls[0].args[2], "apply");
        assert_eq!(calls[0].limit, Duration::from_secs(300));
        let ops = [
            trust(),
            HelperOp::ResolvedInstall { port: 18053 },
            HelperOp::PortsInstall,
        ];
        assert_eq!(fake.pkexec_request(), HelperRequest::new(ops.to_vec()));
        let names: Vec<_> = report.results.iter().map(|r| r.op).collect();
        assert_eq!(
            names,
            [
                OpName::TrustInstall,
                OpName::ResolvedInstall,
                OpName::PortsInstall
            ]
        );
        assert_eq!((report.error, report.skipped), (None, Vec::new()));
    }

    /// Linux with `certutil` answering `certutil(args)`, and `pkexec`
    /// giving `pkexec()`.
    fn nss_fake(
        pkexec: impl Fn() -> io::Result<RunOutput> + Send + Sync + 'static,
        certutil: impl Fn(&[String]) -> io::Result<RunOutput> + Send + Sync + 'static,
    ) -> Arc<Fake> {
        Fake::answering(move |program, args| match program {
            pkexec::PKEXEC => pkexec(),
            linux_dns::SYSTEMCTL => systemctl(UBUNTU_2404, args),
            nss::CERTUTIL => certutil(args),
            other => panic!("{other} ran on Linux"),
        })
    }

    /// After the helper, the CA root goes into the user's NSS
    /// databases, as the user, with the checked text.
    #[test]
    fn linux_adds_the_ca_to_the_nss_databases_after_the_helper() {
        let dir = Dir::new(true);
        make_helper(&dir.helper());
        fs::create_dir_all(dir.home()).unwrap();
        let answer = all_done(&[
            OpName::TrustInstall,
            OpName::ResolvedInstall,
            OpName::PortsInstall,
        ]);
        let fake = nss_fake(
            move || ok(&answer),
            |args| match args[2].as_str() {
                "-L" => exit(255, "certutil: Could not find cert"),
                _ => ok(""),
            },
        );
        let setup = linux_setup(&fake, dir.helper(), dir.home()).with_nss();
        let report = run_linux(&setup, &dir);

        let programs = fake.programs();
        assert_eq!(programs[2], pkexec::PKEXEC);
        assert!(
            programs[3..]
                .iter()
                .all(|program| *program == nss::CERTUTIL)
        );
        let add = fake.calls().last().unwrap().args.clone();
        assert_eq!(add[2], "-A");
        assert!(add.last().unwrap().ends_with("nss.pem"), "{add:?}");
        let names: Vec<_> = report.results.iter().map(|r| r.op).collect();
        assert_eq!(
            names,
            [
                OpName::TrustInstall,
                OpName::ResolvedInstall,
                OpName::PortsInstall,
                OpName::NssInstall
            ]
        );
        assert_eq!(report.results[3].status, OpStatus::Done);
        assert_eq!((report.error, report.skipped), (None, Vec::new()));
    }

    /// No home folder: the step fails with its code, the rest stands.
    #[test]
    fn nss_without_a_home_folder_is_a_failed_step() {
        let dir = Dir::new(true);
        make_helper(&dir.helper());
        let answer = all_done(&[
            OpName::TrustInstall,
            OpName::ResolvedInstall,
            OpName::PortsInstall,
        ]);
        let fake = nss_fake(move || ok(&answer), |_| panic!("certutil ran"));
        let setup = Setup::with(
            fake.clone(),
            Some(Platform::LinuxX64),
            Some(dir.helper()),
            None,
        )
        .with_nss();
        let report = run_linux(&setup, &dir);
        let last = report.results.last().unwrap();
        assert_eq!(last.op, OpName::NssInstall);
        assert_eq!(last.code.as_deref(), Some("nss_add_failed"));
        assert_eq!(report.error, None);
    }

    /// A cancelled password dialog changes nothing, NSS included; no
    /// `certutil` is a skipped step, not an error.
    #[test]
    fn nss_waits_for_the_helper_and_needs_certutil() {
        let dir = Dir::new(true);
        make_helper(&dir.helper());
        fs::create_dir_all(dir.home()).unwrap();
        let fake = nss_fake(|| exit(126, ""), |_| panic!("certutil ran"));
        let setup = linux_setup(&fake, dir.helper(), dir.home()).with_nss();
        let report = run_linux(&setup, &dir);
        assert_eq!(error_code(&report), Some("setup_cancelled"));

        let answer = all_done(&[
            OpName::TrustInstall,
            OpName::ResolvedInstall,
            OpName::PortsInstall,
        ]);
        let fake = nss_fake(
            move || ok(&answer),
            |_| Err(io::Error::from(io::ErrorKind::NotFound)),
        );
        let setup = linux_setup(&fake, dir.helper(), dir.home()).with_nss();
        let report = run_linux(&setup, &dir);
        assert_eq!(report.error, None);
        assert_eq!(
            report.skipped,
            [skip(OpName::NssInstall, "nss_tool_missing")]
        );
        assert_eq!(report.results.len(), 3);
    }

    /// With the DNS port held by another program, the link
    /// would send `.test` names to it. The other steps still go.
    #[test]
    fn linux_without_the_dns_server_skips_the_link() {
        let dir = Dir::new(true);
        make_helper(&dir.helper());
        let answer = all_done(&[OpName::TrustInstall, OpName::PortsInstall]);
        let fake = linux_fake(move || ok(&answer));
        let setup = linux_setup(&fake, dir.helper(), dir.home());
        let stop = CancellationToken::new();
        let report = setup.run(
            dir.dir.path(),
            CaddyInput::default(),
            Platform::LinuxX64,
            dir.caddy.as_ref(),
            Some(&[]),
            Some(codes::DNS_PORT_IN_USE),
            &stop,
        );
        assert_eq!(requests(&fake), [vec![trust(), HelperOp::PortsInstall]]);
        assert_eq!(
            report.skipped,
            [skip(OpName::ResolvedInstall, codes::DNS_PORT_IN_USE)]
        );
        assert_eq!(report.error, None);
    }

    /// macOS: the same for the resolver file.
    #[test]
    fn macos_without_the_dns_server_skips_the_resolver_file() {
        let dir = Dir::new(false);
        let fake = Fake::scripted(netstat_free(), || ok(DONE));
        let stop = CancellationToken::new();
        let report = dir.setup(&fake).run(
            dir.dir.path(),
            CaddyInput::default(),
            Platform::DarwinArm64,
            dir.caddy.as_ref(),
            Some(&[]),
            Some("dns_not_running"),
            &stop,
        );
        let osascript = fake.calls().last().unwrap().clone();
        assert_eq!(osascript.program, "/usr/bin/osascript");
        assert_eq!(
            serde_json::from_str::<Value>(&osascript.args[7]).unwrap(),
            json!({ "version": 1, "ops": [
                { "op": "pf_install", "http_port": 18080, "https_port": 18443 },
            ] })
        );
        assert!(
            report
                .skipped
                .contains(&skip(OpName::ResolverInstall, "dns_not_running")),
            "{report:?}"
        );
    }

    /// An answer for other ops than sent, through pkexec: no results, so
    /// Caddy's config is not sent again for ports 80 and 443 either.
    #[test]
    fn linux_results_for_other_operations_are_setup_helper_failed() {
        let dir = Dir::new(true);
        let answer = all_done(&[
            OpName::PortsInstall,
            OpName::TrustInstall,
            OpName::ResolvedInstall,
        ]);
        let fake = linux_fake(move || ok(&answer));
        let report = run_linux(&linux_setup(&fake, dir.helper(), dir.home()), &dir);
        assert_eq!(
            report.error.as_ref().map(|error| error.code.as_str()),
            Some("setup_helper_failed")
        );
        assert!(report.results.is_empty());
        assert!(!low_ports_set(&report));
    }

    /// No CA root and no Caddy to make one: the names and the
    /// ports still go to the helper.
    #[test]
    fn linux_without_a_ca_still_sets_up_names_and_ports() {
        let dir = Dir::new(false);
        let answer = all_done(&[OpName::ResolvedInstall, OpName::PortsInstall]);
        let fake = linux_fake(move || ok(&answer));
        let report = run_linux(&linux_setup(&fake, dir.helper(), dir.home()), &dir);
        let ops = vec![
            HelperOp::ResolvedInstall { port: 18053 },
            HelperOp::PortsInstall,
        ];
        assert_eq!(fake.pkexec_request(), HelperRequest::new(ops));
        assert_eq!(
            report.skipped,
            [skip(OpName::TrustInstall, "caddy_not_installed")]
        );
        assert_eq!(report.error, None);
    }

    /// `pkexec`'s own exit codes, with no answer of the helper (spike S-02).
    #[test]
    fn a_dismissed_or_refused_password_dialog_on_linux() {
        for (code, expected) in [(126, "setup_cancelled"), (127, "setup_not_authorized")] {
            let dir = Dir::new(true);
            let fake = linux_fake(move || exit(code, "Error executing command as another user"));
            let report = run_linux(&linux_setup(&fake, dir.helper(), dir.home()), &dir);
            assert_eq!(error_code(&report), Some(expected), "{code}");
            assert!(report.results.is_empty());
        }
    }

    /// The helper's answer comes on stdout also when an op failed (exit 1).
    #[test]
    fn a_failed_op_on_linux_is_in_the_results() {
        let dir = Dir::new(true);
        let answer = json!({ "version": 1, "results": [
            { "op": "trust_install", "status": "done" },
            { "op": "resolved_install", "status": "failed", "code": "resolved_check_failed",
              "message": "x" },
            { "op": "ports_install", "status": "done" },
        ]})
        .to_string();
        let fake = linux_fake(move || {
            Ok(RunOutput {
                code: Some(1),
                stdout: answer.clone().into_bytes(),
                stderr: Vec::new(),
            })
        });
        let report = run_linux(&linux_setup(&fake, dir.helper(), dir.home()), &dir);
        assert_eq!(report.error, None);
        assert_eq!(
            report.results[1].code.as_deref(),
            Some("resolved_check_failed")
        );
        // Not an answer that means the hosts file: no second request.
        assert_eq!(fake.pkexec_calls(), 1);
        assert!(report.skipped.is_empty());
    }

    /// `ops` with the certificate first and `ports_install` last, as setup
    /// sends them on Linux.
    fn linux_request(dns: Vec<HelperOp>) -> HelperRequest {
        let mut ops = vec![trust()];
        ops.extend(dns);
        ops.push(HelperOp::PortsInstall);
        HelperRequest::new(ops)
    }

    fn hosts_sync(names: &[&str]) -> HelperOp {
        HelperOp::HostsSync {
            names: names.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    /// systemd older than 250 with systemd-networkd, or no
    /// systemd-resolved at all: the names of the sites go into the hosts
    /// file, and a link of an earlier setup goes.
    #[test]
    fn linux_without_a_safe_link_puts_the_sites_in_the_hosts_file() {
        for system in [UBUNTU_2204_SERVER, DEBIAN_12] {
            let dir = Dir::new(true);
            let fake = linux_fake_on(system, |request| answer_for(&request, &[]));
            let setup = linux_setup(&fake, dir.helper(), dir.home());
            let report = run_linux_with(&setup, &dir, &["blog.test", "shop.test"]);
            let dns = vec![
                HelperOp::ResolvedRemove,
                hosts_sync(&["blog.test", "shop.test"]),
            ];
            assert_eq!(fake.pkexec_request(), linux_request(dns), "{system:?}");
            let steps: Vec<_> = report.results.iter().map(|r| r.op).collect();
            assert_eq!(
                steps,
                [
                    OpName::TrustInstall,
                    OpName::ResolvedRemove,
                    OpName::HostsSync,
                    OpName::PortsInstall
                ]
            );
            assert_eq!((report.error, report.skipped), (None, Vec::new()));
            assert_eq!(fake.pkexec_calls(), 1, "{system:?}");
            // So `marsa park` and `marsa link` keep the block up to date.
            assert!(linux_dns::marker(dir.dir.path()).is_file(), "{system:?}");
        }
    }

    /// systemd 249 asks about systemd-networkd, and only 249 does. With no
    /// systemd-resolved, nothing more is asked: the hosts file it is.
    #[test]
    fn only_systemd_249_asks_about_networkd() {
        let networkd = |system| {
            let dir = Dir::new(true);
            let fake = linux_fake_on(system, |request| answer_for(&request, &[]));
            run_linux(&linux_setup(&fake, dir.helper(), dir.home()), &dir);
            fake.calls()
                .iter()
                .filter(|call| call.program == linux_dns::SYSTEMCTL)
                .map(|call| call.args[0].clone())
                .collect::<Vec<_>>()
        };
        let asked = ["--version", "is-active", "is-active", "is-enabled"];
        assert_eq!(networkd(UBUNTU_2204_SERVER), asked);
        assert_eq!(networkd(UBUNTU_2204_DESKTOP), asked);
        assert_eq!(networkd(UBUNTU_2404), ["--version", "is-active"]);
        assert_eq!(networkd(DEBIAN_12), ["--version", "is-active"]);
    }

    /// Ubuntu 22.04 on the desktop: networkd is off for good, so the link
    /// (with its local route, in the helper) is safe there too.
    #[test]
    fn systemd_249_without_networkd_keeps_the_link() {
        let dir = Dir::new(true);
        let fake = linux_fake_on(UBUNTU_2204_DESKTOP, |request| answer_for(&request, &[]));
        let setup = linux_setup(&fake, dir.helper(), dir.home());
        run_linux_with(&setup, &dir, &["shop.test"]);
        let dns = vec![HelperOp::ResolvedInstall { port: 18053 }];
        assert_eq!(fake.pkexec_request(), linux_request(dns));
    }

    /// A hosts file with Marsa's block from an earlier setup.
    fn hosts_with_block(dir: &Dir) -> PathBuf {
        let hosts = dir.dir.path().join("hosts");
        let text = "127.0.0.1 localhost\n# Marsa start\n127.0.0.1 shop.test\n# Marsa end\n";
        fs::write(&hosts, text).unwrap();
        hosts
    }

    /// The ops of each `pkexec` request, in order.
    fn requests(fake: &Fake) -> Vec<Vec<HelperOp>> {
        fake.calls()
            .iter()
            .filter(|call| call.program == pkexec::PKEXEC)
            .map(|call| {
                serde_json::from_str::<HelperRequest>(&call.args[3])
                    .unwrap()
                    .ops
            })
            .collect()
    }

    /// Marsa's block from an earlier setup, on a system that now takes the
    /// link (an upgrade to systemd 250, say): it goes in a second request,
    /// once the link is made. The file itself is only read.
    #[test]
    fn the_link_takes_an_old_hosts_block_out_once_it_is_made() {
        for status in ["done", "unchanged"] {
            let dir = Dir::new(true);
            let hosts = hosts_with_block(&dir);
            let text = fs::read_to_string(&hosts).unwrap();
            let fake = linux_fake_on(UBUNTU_2404, move |request| {
                let mut answer = answer_for(&request, &[]);
                if status == "unchanged" {
                    let out = answer.as_mut().unwrap();
                    out.stdout = String::from_utf8(out.stdout.clone())
                        .unwrap()
                        .replace("\"done\"", "\"unchanged\"")
                        .into_bytes();
                }
                answer
            });
            let setup = linux_setup(&fake, dir.helper(), dir.home()).with_hosts_file(hosts.clone());
            fs::write(linux_dns::marker(dir.dir.path()), "").unwrap();
            let report = run_linux_with(&setup, &dir, &["shop.test"]);
            let first = linux_request(vec![HelperOp::ResolvedInstall { port: 18053 }]).ops;
            assert_eq!(
                requests(&fake),
                [first, vec![HelperOp::HostsRemove]],
                "{status}"
            );
            // The link is made: `marsa park` no longer syncs the hosts file.
            assert!(!linux_dns::marker(dir.dir.path()).is_file());
            let steps: Vec<_> = report.results.iter().map(|r| r.op).collect();
            assert_eq!(
                steps,
                [
                    OpName::TrustInstall,
                    OpName::ResolvedInstall,
                    OpName::PortsInstall,
                    OpName::HostsRemove
                ]
            );
            assert_eq!(report.error, None);
            assert_eq!(fs::read_to_string(&hosts).unwrap(), text);
        }

        // No block, or one whose markers do not pair up: nothing to take out.
        for text in [
            "127.0.0.1 localhost\n",
            "# Marsa start\n127.0.0.1 shop.test\n",
        ] {
            let dir = Dir::new(true);
            let hosts = dir.dir.path().join("hosts");
            fs::write(&hosts, text).unwrap();
            let fake = linux_fake_on(UBUNTU_2404, |request| answer_for(&request, &[]));
            let setup = linux_setup(&fake, dir.helper(), dir.home()).with_hosts_file(hosts);
            run_linux_with(&setup, &dir, &["shop.test"]);
            assert_eq!(fake.pkexec_calls(), 1, "{text:?}");
        }
    }

    /// While the block is the only way the sites resolve, it stays: no link,
    /// no removal (review of #130).
    #[test]
    fn a_link_that_failed_keeps_the_block() {
        for code in ["resolved_check_failed", "resolved_tool_missing"] {
            let dir = Dir::new(true);
            let hosts = hosts_with_block(&dir);
            let fake = linux_fake_on(UBUNTU_2404, move |request| {
                answer_for(&request, &[(OpName::ResolvedInstall, code)])
            });
            let setup = linux_setup(&fake, dir.helper(), dir.home()).with_hosts_file(hosts);
            let report = run_linux_with(&setup, &dir, &["shop.test"]);
            assert_eq!(fake.pkexec_calls(), 1, "{code}");
            assert_eq!(report.results[1].code.as_deref(), Some(code));
        }
    }

    /// The removal's prompt dismissed: the link is made, the old block
    /// stays, and the error says so.
    #[test]
    fn a_dismissed_removal_prompt_says_the_block_stays() {
        let dir = Dir::new(true);
        let hosts = hosts_with_block(&dir);
        let fake = linux_fake_on(UBUNTU_2404, |request| {
            if request.ops == [HelperOp::HostsRemove] {
                exit(126, "Request dismissed")
            } else {
                answer_for(&request, &[])
            }
        });
        let setup = linux_setup(&fake, dir.helper(), dir.home()).with_hosts_file(hosts);
        let report = run_linux_with(&setup, &dir, &["shop.test"]);
        assert_eq!(error_code(&report), Some("setup_hosts_not_removed"));
        assert_eq!(report.results.len(), 3);
        assert!(low_ports_set(&report));
    }

    /// The helper is the judge: where marsad expected the
    /// link but the helper chose the hosts file, `resolved_install` becomes
    /// a skipped step with the helper's code, and the operations of the
    /// hosts way go to the helper in a second request. With a block there,
    /// it is not taken out first.
    #[test]
    fn the_hosts_file_when_the_helper_says_so() {
        for code in ["resolved_use_hosts", "resolved_not_running"] {
            let dir = Dir::new(true);
            let hosts = hosts_with_block(&dir);
            let fake = linux_fake_on(UBUNTU_2404, move |request| {
                answer_for(&request, &[(OpName::ResolvedInstall, code)])
            });
            let setup = linux_setup(&fake, dir.helper(), dir.home()).with_hosts_file(hosts);
            let report = run_linux_with(&setup, &dir, &["shop.test"]);

            let first = linux_request(vec![HelperOp::ResolvedInstall { port: 18053 }]).ops;
            let second = vec![HelperOp::ResolvedRemove, hosts_sync(&["shop.test"])];
            assert_eq!(requests(&fake), [first, second], "{code}");
            let steps: Vec<_> = report.results.iter().map(|r| r.op).collect();
            assert_eq!(
                steps,
                [
                    OpName::TrustInstall,
                    OpName::PortsInstall,
                    OpName::ResolvedRemove,
                    OpName::HostsSync
                ]
            );
            assert_eq!(report.skipped, [skip(OpName::ResolvedInstall, code)]);
            assert_eq!(report.error, None);
            assert!(low_ports_set(&report));
        }
    }

    /// The second prompt dismissed: the error says the steps before are
    /// done and the names are not written; the skipped step stays.
    #[test]
    fn a_dismissed_second_prompt_says_the_names_are_not_written() {
        let dir = Dir::new(true);
        let fake = linux_fake_on(UBUNTU_2404, |request| {
            if request.ops.len() == 2 {
                exit(
                    126,
                    "Error executing command as another user: Request dismissed",
                )
            } else {
                answer_for(&request, &[(OpName::ResolvedInstall, "resolved_use_hosts")])
            }
        });
        let report = run_linux(&linux_setup(&fake, dir.helper(), dir.home()), &dir);
        assert_eq!(error_code(&report), Some("setup_hosts_not_written"));
        assert_eq!(
            report.skipped,
            [skip(OpName::ResolvedInstall, "resolved_use_hosts")]
        );
        let steps: Vec<_> = report.results.iter().map(|r| r.op).collect();
        assert_eq!(steps, [OpName::TrustInstall, OpName::PortsInstall]);
    }

    /// A first request that failed as a whole: no second one.
    #[test]
    fn no_second_request_after_a_failed_first_one() {
        let dir = Dir::new(true);
        let hosts = hosts_with_block(&dir);
        let fake = linux_fake_on(UBUNTU_2404, |_| exit(127, "Not authorized"));
        let setup = linux_setup(&fake, dir.helper(), dir.home()).with_hosts_file(hosts);
        let report = run_linux_with(&setup, &dir, &["shop.test"]);
        assert_eq!(error_code(&report), Some("setup_not_authorized"));
        assert_eq!(fake.pkexec_calls(), 1);
    }

    /// The hosts way never falls back the other way: a failed `hosts_sync`
    /// is a failed step, with one prompt.
    #[test]
    fn a_failed_hosts_sync_is_a_failed_step() {
        let dir = Dir::new(true);
        let fake = linux_fake_on(UBUNTU_2204_SERVER, |request| {
            answer_for(&request, &[(OpName::HostsSync, "hosts_block_broken")])
        });
        let report = run_linux(&linux_setup(&fake, dir.helper(), dir.home()), &dir);
        assert_eq!(fake.pkexec_calls(), 1);
        assert_eq!(
            report.results[2].code.as_deref(),
            Some("hosts_block_broken")
        );
        assert_eq!((report.error, report.skipped), (None, Vec::new()));
    }

    /// No site list, or more names than the helper takes: `hosts_sync` is
    /// left out, never sent empty, and never sent so that the helper refuses
    /// the whole request (review of #130). The rest of setup goes on.
    #[test]
    fn hosts_sync_is_left_out_when_it_cannot_be_right() {
        let many: Vec<String> = (0..1001).map(|i| format!("s{i}.test")).collect();
        for (names, code) in [
            (None, "hosts_sites_unknown"),
            (Some(many), "hosts_too_many_names"),
        ] {
            let dir = Dir::new(true);
            let fake = linux_fake_on(UBUNTU_2204_SERVER, |request| answer_for(&request, &[]));
            let setup = linux_setup(&fake, dir.helper(), dir.home());
            let stop = CancellationToken::new();
            let report = setup.run(
                dir.dir.path(),
                CaddyInput::default(),
                Platform::LinuxX64,
                dir.caddy.as_ref(),
                names.as_deref(),
                None,
                &stop,
            );
            assert_eq!(
                fake.pkexec_request(),
                linux_request(vec![HelperOp::ResolvedRemove]),
                "{code}"
            );
            assert_eq!(report.skipped, [skip(OpName::HostsSync, code)]);
            assert_eq!(report.error, None);
        }
    }

    /// `systemctl` that cannot run, or that says resolved is not active in
    /// any way: the helper would refuse the link (`resolved_not_running`),
    /// so the hosts way at once, with one prompt (review of #130).
    #[test]
    fn a_systemctl_that_says_no_means_the_hosts_file() {
        let answers: [fn() -> io::Result<RunOutput>; 2] = [
            || Err(io::Error::from(io::ErrorKind::NotFound)),
            || exit(1, "Failed to connect to bus"),
        ];
        for answer in answers {
            let dir = Dir::new(true);
            let fake = Fake::answering(move |program, args| match program {
                linux_dns::SYSTEMCTL => answer(),
                pkexec::PKEXEC => answer_for(&serde_json::from_str(&args[3]).unwrap(), &[]),
                other => panic!("{other} ran"),
            });
            run_linux_with(
                &linux_setup(&fake, dir.helper(), dir.home()),
                &dir,
                &["a.test"],
            );
            let dns = vec![HelperOp::ResolvedRemove, hosts_sync(&["a.test"])];
            assert_eq!(requests(&fake), [linux_request(dns).ops]);
        }
    }

    /// A read of systemd-resolved that did not end (a timeout, a signal):
    /// the helper is asked, so a slow `systemctl` never takes out a link
    /// that works (review of #130).
    #[test]
    fn a_systemctl_that_did_not_end_leaves_it_to_the_helper() {
        let answers: [fn() -> io::Result<RunOutput>; 2] = [
            || Err(io::Error::from(io::ErrorKind::TimedOut)),
            || {
                Ok(RunOutput {
                    code: None,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                })
            },
        ];
        for answer in answers {
            let dir = Dir::new(true);
            let fake = Fake::answering(move |program, args| match program {
                linux_dns::SYSTEMCTL if args[0] == "--version" => systemctl(UBUNTU_2404, args),
                linux_dns::SYSTEMCTL => answer(),
                pkexec::PKEXEC => answer_for(&serde_json::from_str(&args[3]).unwrap(), &[]),
                other => panic!("{other} ran"),
            });
            run_linux(&linux_setup(&fake, dir.helper(), dir.home()), &dir);
            let dns = vec![HelperOp::ResolvedInstall { port: 18053 }];
            assert_eq!(requests(&fake), [linux_request(dns).ops]);
        }
    }

    /// The helper chose the hosts file, but `hosts_sync` cannot be sent: no
    /// second prompt that would write nothing.
    #[test]
    fn no_second_prompt_without_names_to_write() {
        let dir = Dir::new(true);
        let fake = linux_fake_on(UBUNTU_2404, |request| {
            answer_for(&request, &[(OpName::ResolvedInstall, "resolved_use_hosts")])
        });
        let setup = linux_setup(&fake, dir.helper(), dir.home());
        let stop = CancellationToken::new();
        let report = setup.run(
            dir.dir.path(),
            CaddyInput::default(),
            Platform::LinuxX64,
            dir.caddy.as_ref(),
            None,
            None,
            &stop,
        );
        assert_eq!(fake.pkexec_calls(), 1);
        assert_eq!(
            report.skipped,
            [
                skip(OpName::ResolvedInstall, "resolved_use_hosts"),
                skip(OpName::HostsSync, "hosts_sites_unknown")
            ]
        );
        assert_eq!(report.error, None);
    }

    /// A second request that timed out keeps its own code: the helper may
    /// have run, and that text says so.
    #[test]
    fn a_second_request_that_timed_out_says_it_may_have_run() {
        let dir = Dir::new(true);
        let fake = linux_fake_on(UBUNTU_2404, |request| {
            if request.ops.len() == 2 {
                Err(io::Error::from(io::ErrorKind::TimedOut))
            } else {
                answer_for(&request, &[(OpName::ResolvedInstall, "resolved_use_hosts")])
            }
        });
        let report = run_linux_with(
            &linux_setup(&fake, dir.helper(), dir.home()),
            &dir,
            &["a.test"],
        );
        assert_eq!(error_code(&report), Some("setup_timed_out"));
    }

    /// The slowest Linux setup stays under [`SLOWEST_SETUP`]: four
    /// `systemctl` checks and two prompts, each with its pipes' grace,
    /// after Caddy's start (review of #130), then the NSS step.
    #[test]
    fn the_slowest_linux_setup_fits_in_slowest_setup() {
        let dir = Dir::new(true);
        let fake = linux_fake_on(UBUNTU_2204_DESKTOP, |request| {
            answer_for(&request, &[(OpName::ResolvedInstall, "resolved_use_hosts")])
        });
        run_linux_with(
            &linux_setup(&fake, dir.helper(), dir.home()),
            &dir,
            &["a.test"],
        );
        let calls = fake.calls();
        assert_eq!(calls.len(), 6, "{calls:?}");
        let programs: Duration = calls.iter().map(|call| call.limit + PIPE_GRACE).sum();
        assert!(CA_WAIT + programs + nss::NSS_BUDGET <= SLOWEST_SETUP);
    }

    /// The same checks of the helper file as on macOS, before any dialog.
    #[cfg(unix)]
    #[test]
    fn a_helper_others_could_change_on_linux_means_no_pkexec() {
        let dir = Dir::new(true);
        set_mode(&dir.helper(), 0o777);
        let fake = linux_fake(|| panic!("pkexec ran"));
        let report = run_linux(&linux_setup(&fake, dir.helper(), dir.home()), &dir);
        assert_eq!(error_code(&report), Some("setup_helper_unsafe"));
        assert!(!fake.programs().contains(&pkexec::PKEXEC));
    }

    #[test]
    fn low_ports_are_set_when_ports_install_is_done_or_unchanged() {
        let report = |status| SetupReport {
            results: vec![OpResult {
                op: OpName::PortsInstall,
                status,
                code: None,
                message: None,
            }],
            ..SetupReport::default()
        };
        assert!(low_ports_set(&report(OpStatus::Done)));
        assert!(low_ports_set(&report(OpStatus::Unchanged)));
        assert!(!low_ports_set(&report(OpStatus::Failed)));
        assert!(!low_ports_set(&SetupReport::default()));
        let other = SetupReport {
            results: vec![done(OpName::ResolvedInstall)],
            ..SetupReport::default()
        };
        assert!(!low_ports_set(&other));
    }

    /// After setup on Linux, Caddy's config is written again with ports 80
    /// and 443 (Caddy does not run here, so only the file shows it). Not
    /// after a `ports_install` that failed.
    #[tokio::test]
    async fn linux_setup_sends_caddy_its_config_with_ports_80_and_443() {
        for (status, port) in [("done", Some(80)), ("failed", None)] {
            let dir = Dir::new(true);
            let answer = json!({ "version": 1, "results": [
                { "op": "trust_install", "status": "done" },
                { "op": "resolved_install", "status": "done" },
                { "op": "ports_install", "status": status, "code": "x", "message": "x" },
            ]})
            .to_string();
            let fake = linux_fake(move || ok(&answer));
            let mut daemon = Daemon::new(dir.dir.path().to_path_buf());
            daemon.caddy_mut().low_ports = || true;
            let daemon = Arc::new(daemon);
            let app = App::new(
                Arc::clone(&daemon),
                linux_setup(&fake, dir.helper(), dir.home()),
            );
            assert_eq!(app.post().await.0, StatusCode::OK, "{status}");
            let written = fs::read_to_string(&daemon.caddy().paths.config_file)
                .ok()
                .map(|text| serde_json::from_str::<Value>(&text).unwrap());
            let http_port = written.and_then(|config| config["apps"]["http"]["http_port"].as_u64());
            assert_eq!(http_port, port, "{status}");
        }
    }

    /// The rejection paths of the route (AGENTS.md section 5) on Linux too.
    #[tokio::test]
    async fn on_linux_a_request_without_the_token_with_an_origin_or_another_host_runs_nothing() {
        let dir = Dir::new(true);
        let answer = all_done(&[
            OpName::TrustInstall,
            OpName::ResolvedInstall,
            OpName::PortsInstall,
        ]);
        let fake = linux_fake(move || ok(&answer));
        let daemon = Arc::new(Daemon::new(dir.dir.path().to_path_buf()));
        let app = App::new(daemon, linux_setup(&fake, dir.helper(), dir.home()));
        let host = daemon_authority(PORT);
        let other_host = daemon_authority(PORT + 1);
        let bearer = app.bearer.clone();
        let cases = [
            (vec![("host", host.as_str())], StatusCode::UNAUTHORIZED),
            (
                vec![
                    ("host", host.as_str()),
                    ("authorization", bearer.as_str()),
                    ("origin", "http://evil.test"),
                ],
                StatusCode::FORBIDDEN,
            ),
            (
                vec![
                    ("host", other_host.as_str()),
                    ("authorization", bearer.as_str()),
                ],
                StatusCode::FORBIDDEN,
            ),
        ];
        for (headers, status) in cases {
            assert_eq!(app.send(&headers).await.0, status, "{headers:?}");
            assert!(fake.calls().is_empty());
        }
        assert_eq!(app.post().await.0, StatusCode::OK);
        assert_eq!(
            fake.programs(),
            [
                "/usr/bin/systemctl",
                "/usr/bin/systemctl",
                "/usr/bin/pkexec"
            ]
        );
    }

    #[tokio::test]
    async fn the_whole_setup_over_http() {
        let dir = Dir::new(true);
        let answer = all_done(&[OpName::ResolverInstall, OpName::PfInstall]);
        let fake = Fake::scripted(netstat_free(), move || ok(&answer));
        let mut daemon = Daemon::new(dir.dir.path().to_path_buf());
        // The ports come from the daemon's Caddy settings.
        daemon.caddy_mut().input.http_port = 28080;
        daemon.caddy_mut().input.https_port = 28443;
        let daemon = Arc::new(daemon);
        let mut events = daemon.events().subscribe();
        let app = App::new(daemon.clone(), dir.setup(&fake));

        let (status, json) = app.post().await;
        assert_eq!(status, StatusCode::OK, "{json}");
        let report: SetupReport = serde_json::from_value(json).unwrap();
        let want = SetupReport {
            results: vec![
                done(OpName::ResolverInstall),
                done(OpName::PfInstall),
                done(OpName::TrustInstall),
            ],
            ..SetupReport::default()
        };
        assert_eq!(report, want);
        assert_eq!(
            fake.request(),
            HelperRequest::new(vec![resolver(), pf(28080, 28443)])
        );
        assert_eq!(events.try_recv().unwrap(), Event::SetupFinished(want));
    }

    /// A Windows helper service that is not installed until `install`, and
    /// then answers `done` for every op of a request.
    #[derive(Default)]
    struct WindowsService {
        installed: std::sync::Mutex<Option<(String, String)>>,
        requests: std::sync::Mutex<Vec<Value>>,
    }

    impl windows::Service for WindowsService {
        fn user_sid(&self) -> Result<String, String> {
            Ok("S-1-5-21-1-2-3-1001".to_owned())
        }

        fn send(&self, request: &[u8], _stop: &CancellationToken) -> Result<Vec<u8>, &'static str> {
            let Some((_, approved)) = self.installed.lock().unwrap().clone() else {
                return Err("helper_not_running");
            };
            let request: Value = serde_json::from_slice(request).unwrap();
            // As the service: a CA other than the approved one refuses the
            // whole request.
            for op in request["ops"].as_array().unwrap() {
                if op["op"] == "trust_install" {
                    let pem = op["pem"].as_str().unwrap();
                    if marsa_core::helper::certificate_sha256(pem).as_deref() != Some(&approved) {
                        let refused = json!({
                            "version": 1,
                            "error": { "code": "trust_not_allowed", "message": "no" },
                        });
                        return Ok(refused.to_string().into_bytes());
                    }
                }
            }
            let results: Vec<Value> = request["ops"]
                .as_array()
                .unwrap()
                .iter()
                .map(|op| json!({ "op": op["op"], "status": "done" }))
                .collect();
            self.requests.lock().unwrap().push(request);
            Ok(json!({ "version": 1, "results": results })
                .to_string()
                .into_bytes())
        }

        fn install(&self, _helper: &Path, sid: &str, hash: &str) -> windows::Install {
            *self.installed.lock().unwrap() = Some((sid.to_owned(), hash.to_owned()));
            windows::Install::Exited(0)
        }

        fn uninstall(&self, _helper: &Path) -> windows::Install {
            *self.installed.lock().unwrap() = None;
            windows::Install::Exited(0)
        }
    }

    /// Windows: one install of the service, for this CA, then the CA and the
    /// hosts block through its pipe. No other program runs: no pf, no
    /// `osascript`, no `pkexec`.
    #[tokio::test]
    async fn windows_setup_installs_the_service_for_its_ca_and_sends_trust_and_hosts() {
        let dir = Dir::new(true);
        let fake = Fake::new(|program| panic!("{program} ran"));
        let service = Arc::new(WindowsService::default());
        let daemon = Arc::new(Daemon::new(dir.dir.path().to_path_buf()));
        // The user's folder holds Marsa's programs, as an install puts them.
        let setup = Setup::with(
            fake.clone(),
            Some(Platform::WindowsX64),
            Some(dir.helper()),
            Some(dir.dir.path().to_path_buf()),
        )
        .with_windows(service.clone());
        let app = App::new(daemon, setup);
        let (status, report) = app.post().await;
        assert_eq!(status, StatusCode::OK, "{report}");
        let ops: Vec<&str> = report["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|result| result["op"].as_str().unwrap())
            .collect();
        assert_eq!(ops, ["trust_install", "hosts_sync"], "{report}");
        let hash = marsa_core::helper::certificate_sha256(TEST_CA).unwrap();
        let installed = service.installed.lock().unwrap().clone();
        assert_eq!(installed, Some(("S-1-5-21-1-2-3-1001".to_owned(), hash)));
        assert_eq!(service.requests.lock().unwrap().len(), 1);
        assert!(fake.calls().is_empty());
        // From now on this data folder's daemon sends the names after each
        // pass.
        assert!(dir.dir.path().join(hosts_sync::MARKER).exists());
    }

    /// Windows with no CA root yet and no Caddy to make one: the trust is
    /// skipped, the service is installed for no CA (`-`), and the hosts
    /// block still goes.
    #[tokio::test]
    async fn windows_setup_without_a_ca_installs_for_none_and_sends_the_hosts() {
        let dir = Dir::new(false);
        let fake = Fake::new(|program| panic!("{program} ran"));
        let service = Arc::new(WindowsService::default());
        let daemon = Arc::new(Daemon::new(dir.dir.path().to_path_buf()));
        let setup = Setup::with(
            fake.clone(),
            Some(Platform::WindowsX64),
            Some(dir.helper()),
            Some(dir.dir.path().to_path_buf()),
        )
        .with_windows(service.clone())
        .with_caddy(dir.caddy.clone());
        let app = App::new(daemon, setup);
        let (status, report) = app.post().await;
        assert_eq!(status, StatusCode::OK, "{report}");
        assert_eq!(report["results"][0]["op"], "hosts_sync", "{report}");
        assert_eq!(report["skipped"][0]["op"], "trust_install", "{report}");
        let installed = service.installed.lock().unwrap().clone();
        assert_eq!(
            installed,
            Some(("S-1-5-21-1-2-3-1001".to_owned(), "-".to_owned()))
        );
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn inside_compares_folders_with_no_regard_to_case() {
        let home = Path::new("/Users/Omar");
        assert!(inside(Path::new("/users/omar/bin/marsa-helper"), home));
        assert!(inside(Path::new("/Users/Omar/bin/marsa-helper"), home));
        assert!(!inside(Path::new("/Users/Omarx/bin/marsa-helper"), home));
        assert!(!inside(Path::new("/Users"), home));
        assert!(!inside(Path::new("/tools/marsa-helper"), home));
    }

    /// Windows: a marsa-helper outside the user's folder gets no prompt.
    #[tokio::test]
    async fn windows_setup_refuses_a_helper_outside_the_users_folder() {
        let dir = Dir::new(true);
        let fake = Fake::new(|program| panic!("{program} ran"));
        let service = Arc::new(WindowsService::default());
        let daemon = Arc::new(Daemon::new(dir.dir.path().to_path_buf()));
        let elsewhere = TempDir::new().unwrap();
        let setup = Setup::with(
            fake.clone(),
            Some(Platform::WindowsX64),
            Some(dir.helper()),
            Some(elsewhere.path().to_path_buf()),
        )
        .with_windows(service.clone());
        let app = App::new(daemon, setup);
        let (_, report) = app.post().await;
        assert_eq!(report["error"]["code"], "setup_helper_unsafe", "{report}");
        assert!(service.installed.lock().unwrap().is_none());
        assert!(!dir.dir.path().join(hosts_sync::MARKER).exists());
    }
}
