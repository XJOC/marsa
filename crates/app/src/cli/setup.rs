//! `marsa setup`: the first setup (macOS and Linux). macOS may ask for an
//! administrator password, and may then ask the user to approve trusting
//! Marsa's certificate authority for this user. Linux asks for the password
//! once. Task M4-13, task M4-26 for the trust, and task M4-18e for Linux.
//!
//! One call to `marsad` (`POST /v1/setup`, in `daemon/setup_api.rs`),
//! started if needed. On macOS the daemon checks ports 80 and 443, asks for
//! the administrator password and runs `marsa-helper`, then trusts the
//! certificate authority for the user behind the dialog macOS shows. On
//! Linux it runs `marsa-helper` through `pkexec`. This prints one line per
//! step: done, already done, failed, or skipped and why.
//!
//! Exit code 1 if a step failed or the helper did not run (the prompt was
//! cancelled, for example). A skipped step alone, such as
//! `caddy_not_installed` before Caddy is installed, is not a failure: its
//! line says what to do.

use std::path::Path;
use std::process::ExitCode;

use marsa_core::caddy::CaddyPaths;
use marsa_core::recipes::Platform;
use marsa_core::types::{OpStatus, PortHolder, SetupReport};

use super::daemon::{ClientFailure, connect, failure};
use super::sites::printable;
use super::strings::{self, Details};
use crate::daemon::setup_api::user_trust::{self, TrustFiles};

/// Starts the daemon if needed, runs the setup, and prints its report.
pub async fn run(data_dir: &Path) -> ExitCode {
    let client = match connect(data_dir).await {
        Ok(client) => client,
        Err(failure) => return failure.report(),
    };
    if let Some(line) = started(Platform::current()) {
        eprintln!("{line}");
    }
    let report = match client.setup().await {
        Ok(report) => report,
        Err(err) => return failure(&err, data_dir, None).report(),
    };
    print!("{}", render(&report, data_dir));
    match error(&report) {
        Some(failure) => failure.report(),
        None if report
            .results
            .iter()
            .any(|result| result.status == OpStatus::Failed) =>
        {
            ExitCode::FAILURE
        }
        None => ExitCode::SUCCESS,
    }
}

/// The line before the call, which may wait for the password prompt. Only
/// on macOS and Linux, where the daemon runs setup (the platform check of
/// `daemon/setup_api.rs`); the CLI and the daemon run on the same machine.
/// Elsewhere the daemon answers `setup_not_supported` at once, and a
/// password line first would be wrong.
fn started(platform: Option<Platform>) -> Option<&'static str> {
    match platform? {
        Platform::DarwinArm64 | Platform::DarwinX64 => Some(strings::SETUP_STARTED),
        Platform::LinuxX64 | Platform::LinuxArm64 => Some(strings::SETUP_STARTED_LINUX),
        Platform::WindowsX64 | Platform::Any => None,
    }
}

/// One line per helper result, then one per skipped step, with the programs
/// that hold port 80 or 443 under the `pf_install` line.
pub fn render(report: &SetupReport, data_dir: &Path) -> String {
    let mut text = String::new();
    let mut push = |line: String| {
        text.push_str(&line);
        text.push('\n');
    };
    for result in &report.results {
        let step = strings::setup_step(&printable(&result.op));
        push(match result.status {
            OpStatus::Done => strings::setup_done(&step),
            OpStatus::Unchanged => strings::setup_unchanged(&step),
            OpStatus::Failed => {
                let code = printable(result.code.as_deref().unwrap_or_default());
                // The uninstall's text for this code is about its record.
                let reason = if code == user_trust::TRUST_RECORD_UNUSABLE {
                    strings::setup_trust_dir_unusable(TrustFiles::in_dir(data_dir).dir())
                } else {
                    strings::message(&code, &Details::default())
                };
                strings::setup_failed(&step, &reason, &code)
            }
        });
    }
    let ca_root = CaddyPaths::in_dir(data_dir).ca_root();
    for skipped in &report.skipped {
        let step = strings::setup_step(&printable(&skipped.op));
        let code = printable(&skipped.code);
        let details = Details {
            path: Some(&ca_root),
            ..Details::default()
        };
        // The general text of this code is for `marsa services start caddy`.
        let reason = if code == "caddy_not_installed" {
            strings::SETUP_CADDY_NOT_INSTALLED.to_owned()
        } else {
            strings::message(&code, &details)
        };
        push(strings::setup_skipped(&step, &reason, &code));
        if code == "redirect_port_held" {
            for holder in &report.port_holders {
                push(port_held(holder));
            }
        }
    }
    if text.is_empty() && report.error.is_none() {
        text.push_str(strings::SETUP_NOTHING);
        text.push('\n');
    }
    text
}

/// Why the helper did not run, or (Linux) why Caddy did not take ports 80
/// and 443 after it, ready for stderr.
pub fn error(report: &SetupReport) -> Option<ClientFailure> {
    let error = report.error.as_ref()?;
    let code = printable(&error.code);
    Some(ClientFailure {
        message: strings::message(&code, &Details::default()),
        code,
    })
}

/// `  Port 80 is used by nginx-arm64 (PID 15464, Herd).` `marsa doctor`
/// shows the same lines.
pub(super) fn port_held(holder: &PortHolder) -> String {
    let name = holder
        .process
        .as_deref()
        .or(holder.command.as_deref())
        .map(printable);
    let app = holder.app.as_deref().map(printable);
    strings::port_held(holder.port, name.as_deref(), holder.pid, app.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::types::{ApiError, OpResult, SetupSkip};

    fn data() -> std::path::PathBuf {
        std::env::temp_dir().join("marsa-cli-setup")
    }

    fn result(op: &str, status: OpStatus, code: Option<&str>) -> OpResult {
        OpResult {
            op: op.into(),
            status,
            code: code.map(str::to_owned),
            message: code.map(|_| "x".to_owned()),
        }
    }

    fn skip(op: &str, code: &str) -> SetupSkip {
        SetupSkip {
            op: op.into(),
            code: code.into(),
        }
    }

    #[test]
    fn a_report_shows_each_step_and_the_port_holders() {
        let report = SetupReport {
            results: vec![
                result("resolver_install", OpStatus::Done, None),
                result("trust_install", OpStatus::Unchanged, None),
            ],
            skipped: vec![skip("pf_install", "redirect_port_held")],
            port_holders: vec![
                PortHolder {
                    port: 80,
                    address: "127.0.0.1".into(),
                    process: Some("nginx-arm64".into()),
                    pid: Some(15464),
                    user: Some("dev".into()),
                    command: Some("nginx: worker process".into()),
                    app: Some("Herd".into()),
                },
                PortHolder {
                    port: 443,
                    address: "*".into(),
                    process: None,
                    pid: None,
                    user: None,
                    command: None,
                    app: None,
                },
            ],
            error: None,
        };
        assert_eq!(
            render(&report, &data()),
            "Send .test names to Marsa: done.\n\
             Trust Marsa's HTTPS certificates: already done.\n\
             Route ports 80 and 443 to Marsa: skipped. Another program uses port 80 or 443, \
             and the redirect would take its traffic. Stop that program, or turn off its web \
             server, then run \"marsa setup\" again. [redirect_port_held]\n  \
             Port 80 is used by nginx-arm64 (PID 15464, Herd).\n  \
             Port 443 is used by an unknown program.\n"
        );
        assert!(error(&report).is_none());
    }

    #[test]
    fn a_failed_step_and_no_ca_say_why() {
        let report = SetupReport {
            results: vec![result(
                "resolver_install",
                OpStatus::Failed,
                Some("resolver_foreign"),
            )],
            skipped: vec![skip("trust_install", "ca_not_ready")],
            ..SetupReport::default()
        };
        let text = render(&report, &data());
        assert!(
            text.starts_with(
                "Send .test names to Marsa: failed. /etc/resolver/test belongs to another tool"
            ),
            "{text}"
        );
        assert!(text.contains("[resolver_foreign]\n"), "{text}");
        assert!(
            text.contains(
                "Trust Marsa's HTTPS certificates: skipped. Caddy did not make Marsa's \
                 certificate authority in time"
            ),
            "{text}"
        );
        assert!(
            text.contains("Run \"marsa logs caddy\" to see why"),
            "{text}"
        );
        assert!(text.ends_with("[ca_not_ready]\n"), "{text}");
    }

    /// Task M5-15b: Caddy did not start. The reason is in its recent lines,
    /// which `marsa logs caddy` shows.
    #[test]
    fn a_caddy_that_did_not_start_says_to_look_at_its_logs() {
        let report = SetupReport {
            skipped: vec![skip("trust_install", "caddy_not_started")],
            ..SetupReport::default()
        };
        assert_eq!(
            render(&report, &data()),
            "Trust Marsa's HTTPS certificates: skipped. Caddy did not start, so Marsa's \
             certificate authority is not there yet. Run \"marsa logs caddy\" to see why, then \
             run \"marsa setup\" again. [caddy_not_started]\n"
        );
    }

    /// Task M5-15: no Caddy, so no CA. The line says to install Caddy, then
    /// to run setup again, and names no path.
    #[test]
    fn no_caddy_says_to_install_it_then_to_run_setup_again() {
        let report = SetupReport {
            skipped: vec![skip("trust_install", "caddy_not_installed")],
            ..SetupReport::default()
        };
        assert_eq!(
            render(&report, &data()),
            "Trust Marsa's HTTPS certificates: skipped. Caddy is not installed, so Marsa's \
             certificate authority does not exist yet. Run \"marsa install caddy\", then run \
             \"marsa setup\" again. [caddy_not_installed]\n"
        );
    }

    #[test]
    fn an_unusable_trust_folder_names_it_and_says_to_run_setup_again() {
        let report = SetupReport {
            results: vec![result(
                "trust_install",
                OpStatus::Failed,
                Some("trust_record_unusable"),
            )],
            ..SetupReport::default()
        };
        let text = render(&report, &data());
        let trust = data().join("trust");
        assert!(text.contains(&trust.display().to_string()), "{text}");
        assert!(text.contains("not a link"), "{text}");
        assert!(text.contains("run \"marsa setup\" again"), "{text}");
        assert!(!text.contains("Keychain Access"), "{text}");
        assert!(text.ends_with("[trust_record_unusable]\n"), "{text}");
    }

    #[test]
    fn a_cancelled_prompt_is_an_error_line() {
        let report = SetupReport {
            skipped: vec![skip("trust_install", "ca_not_ready")],
            error: Some(ApiError {
                code: "setup_cancelled".into(),
                message: "x".into(),
            }),
            ..SetupReport::default()
        };
        let failure = error(&report).unwrap();
        assert_eq!(
            failure.line(),
            "marsa: The password prompt was cancelled, so nothing was changed. Run \"marsa \
             setup\" again when you are ready. [setup_cancelled]"
        );
        assert_eq!(
            render(&SetupReport::default(), &data()),
            "Nothing to set up.\n"
        );
    }

    #[test]
    fn macos_and_linux_are_told_about_the_password() {
        for platform in [Platform::DarwinArm64, Platform::DarwinX64] {
            assert_eq!(started(Some(platform)), Some(strings::SETUP_STARTED));
        }
        for platform in [Platform::LinuxX64, Platform::LinuxArm64] {
            assert_eq!(started(Some(platform)), Some(strings::SETUP_STARTED_LINUX));
        }
        // ADR 0014: the one change for every program, before the password.
        assert!(strings::SETUP_STARTED_LINUX.contains("ports 80 to 1023"));
        for platform in [Some(Platform::WindowsX64), Some(Platform::Any), None] {
            assert_eq!(started(platform), None, "{platform:?}");
        }
    }

    #[test]
    fn stopping_to_wait_says_steps_may_have_run_and_a_rerun_is_safe() {
        for code in ["setup_timed_out", "setup_interrupted"] {
            let report = SetupReport {
                error: Some(ApiError {
                    code: code.into(),
                    message: "x".into(),
                }),
                ..SetupReport::default()
            };
            let line = error(&report).unwrap().line();
            assert!(line.contains("stopped waiting"), "{line}");
            assert!(
                line.contains("Some steps may already have run with admin rights"),
                "{line}"
            );
            assert!(line.contains("again is safe"), "{line}");
        }
    }

    #[test]
    fn text_from_the_daemon_has_no_control_characters() {
        let report = SetupReport {
            results: vec![result("x\u{1b}[2J", OpStatus::Failed, Some("new\ncode"))],
            ..SetupReport::default()
        };
        let text = render(&report, &data());
        assert_eq!(text.matches('\n').count(), 1, "{text}");
        assert!(!text.contains('\u{1b}'), "{text}");
    }
}
