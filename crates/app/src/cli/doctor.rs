//! `marsa doctor`: checks what Marsa needs, one line each, and says how to
//! fix what is wrong. Task M5-12.
//!
//! | Check | ✗ when |
//! | --- | --- |
//! | `marsad` answers `/v1/health` | it does not |
//! | PHP and Caddy are installed, with a default PHP | one is missing |
//! | Caddy runs | it does not |
//! | `doctor-<hex>.test` resolves to `127.0.0.1` (macOS) | it does not, or to another address |
//! | ports 80 and 443 reach Caddy (macOS) | another program holds one, or one misses Caddy |
//! | Marsa's CA is trusted (macOS) | it is not |
//! | `shims/` is in `PATH` before any other `php` | it is not |
//! | the last error in `logs/marsad.log` since `marsad` started | there is one |
//!
//! A line starts with ✓, ✗ (with a `Fix:` line under it), or `-` for a check
//! that did not run: not on this system yet, or it needs `marsad` or Caddy
//! first. Exit code 1 if a check failed.
//!
//! Read-only: it never starts `marsad` (`DaemonClient::connect`, not
//! `ensure_running`), never asks for admin rights, and writes no file.
//! `marsa_core::doctor` judges the facts; this file gathers them and prints.
//!
//! Ports 80 and 443 "reach Caddy" when no program listens on them (`netstat`
//! and `ps`, as `marsa setup` asks), `127.0.0.1:80` answers a random host
//! with the greeting of Marsa's Caddy, and, once a site is secure,
//! `127.0.0.1:443` takes a connection. Nothing listens on 80 or 443 then, so
//! only the `pf` redirect (ADR 0008) can answer there.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use marsa_core::caddy::CaddyPaths;
use marsa_core::client::{ClientError, DaemonClient};
use marsa_core::config::{self, ConfigPaths};
use marsa_core::doctor::{self, Check, Installed, Pass, Ports, Problem, Skip, Verdict};
use marsa_core::platform::probe;
use marsa_core::recipes::Platform;
use marsa_core::runtimes::{Runtime, RuntimesDir};
use marsa_core::types::{Health, ServiceState};
use tokio_util::sync::CancellationToken;

use super::daemon::{ClientFailure, failure};
use super::setup::port_held;
use super::sites::printable;
use super::strings::{self, Details};
use crate::daemon::caddy::CADDY_ID;
use crate::daemon::setup_api::{RealRunner, Setup, user_trust};

/// How long the system resolver may take for the `.test` name.
const DNS_LIMIT: Duration = Duration::from_secs(5);
/// How long a connection to port 80 or 443, and the answer on 80, may take.
const PORT_LIMIT: Duration = Duration::from_secs(2);
/// How much of the end of `logs/marsad.log` is read.
const LOG_TAIL: u64 = 64 * 1024;
/// The longest log line shown.
const MAX_LINE: usize = 300;

/// What the daemon said, if it runs.
struct FromDaemon {
    /// Its health and port, or the code of the client error.
    health: Result<(Health, u16), String>,
    caddy: Option<ServiceState>,
    /// Whether any site is secure. `None` if not known.
    secure_site: Option<bool>,
}

/// Runs the checks in order and prints each line as soon as it is known.
pub async fn run(data_dir: &Path) -> ExitCode {
    let platform = Platform::current();
    let macos = matches!(platform, Some(Platform::DarwinArm64 | Platform::DarwinX64));
    let windows = platform == Some(Platform::WindowsX64);
    let mut failed = false;
    let mut show = |check: Check, verdict: Verdict| {
        failed |= matches!(verdict, Verdict::Fail(_));
        println!("{}", render(check, &verdict, data_dir, windows));
    };

    let daemon = ask_daemon(data_dir).await;
    let running = daemon.health.is_ok();
    show(Check::Daemon, doctor::daemon(daemon.health.clone()));
    let installed = installed(data_dir);
    let caddy_installed = installed
        .caddy
        .as_ref()
        .is_ok_and(|found| !found.is_empty());
    show(Check::Runtimes, doctor::runtimes(&installed));
    let caddy = daemon.caddy.as_ref();
    show(Check::Caddy, doctor::caddy(running, caddy, caddy_installed));
    // The checks of the redirect, the resolver file, and the user's trust
    // are macOS ones (ADR 0008, ADR 0015). Linux and Windows set these up
    // in other ways, which have no check yet.
    if macos {
        let name = doctor::probe_name(probe::random());
        show(Check::Dns, dns(&name, running));
        let caddy_running = running.then(|| caddy == Some(&ServiceState::Running));
        let facts = ports(&name, caddy_running, daemon.secure_site);
        show(Check::Ports, doctor::ports(facts));
        show(Check::Trust, trust(data_dir));
    } else {
        for check in [Check::Dns, Check::Ports, Check::Trust] {
            show(check, Verdict::Skip(Skip::NotOnThisSystem));
        }
    }
    show(Check::Path, path(data_dir));
    show(Check::Log, log(data_dir));
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Asks a running daemon, never one this starts: its health, Caddy's
/// state, and whether a site is secure.
async fn ask_daemon(data_dir: &Path) -> FromDaemon {
    let down = |err: ClientError| FromDaemon {
        health: Err(failure(&err, data_dir, None).code),
        caddy: None,
        secure_site: None,
    };
    let client = match DaemonClient::connect(data_dir) {
        Ok(client) => client,
        Err(err) => return down(err),
    };
    let health = match client.health().await {
        Ok(health) => health,
        Err(err) => return down(err),
    };
    let caddy = answer(client.services().await, data_dir)
        .and_then(|services| services.into_iter().find(|service| service.id == CADDY_ID))
        .map(|service| service.state);
    let secure_site = answer(client.sites().await, data_dir)
        .map(|list| list.sites.iter().any(|site| site.secure));
    FromDaemon {
        health: Ok((health, client.port())),
        caddy,
        secure_site,
    }
}

/// The value of a call, or `None` with the error in the log.
fn answer<T>(result: Result<T, ClientError>, data_dir: &Path) -> Option<T> {
    result.map_err(|err| failure(&err, data_dir, None)).ok()
}

/// What `runtimes/` and `global.toml` hold, read like the shim reads them.
fn installed(data_dir: &Path) -> Installed {
    let runtimes = RuntimesDir::in_dir(data_dir);
    let list = |runtime| {
        runtimes
            .installed(runtime)
            .map_err(|err| err.code().to_owned())
    };
    let global = config::load_global(&ConfigPaths::in_dir(data_dir).global);
    Installed {
        php: list(Runtime::Php),
        caddy: list(Runtime::Caddy),
        default_php: global
            .map(|global| global.php.default)
            .map_err(|err| err.code().to_owned()),
    }
}

fn dns(name: &str, running: bool) -> Verdict {
    let answer = probe::resolve(name, DNS_LIMIT)
        .map_err(|err| tracing::debug!(name, error = %err, "the .test name did not resolve"))
        .ok();
    doctor::dns(running, answer.as_deref())
}

/// The holders of 80 and 443 from `netstat` and `ps`, as `marsa setup`
/// finds them. The ports are tried only when nobody holds them and Caddy
/// runs: the verdict needs them only then, and another program's server is
/// left alone.
fn ports(name: &str, caddy_running: Option<bool>, secure_site: Option<bool>) -> Ports {
    let setup = Setup::with(Arc::new(RealRunner), Platform::current(), None, None);
    let holders = setup.port_holders(&CancellationToken::new());
    let clear = holders.as_ref().is_some_and(Vec::is_empty) && caddy_running == Some(true);
    let http = clear
        && probe::http_get(80, name, PORT_LIMIT)
            .is_ok_and(|answer| doctor::is_marsa_answer(&answer));
    let https = (clear && secure_site == Some(true)).then(|| probe::accepts(443, PORT_LIMIT));
    Ports {
        holders,
        caddy_running,
        http,
        https,
    }
}

/// `security verify-cert` on Marsa's CA root, as `marsa setup` asks it.
fn trust(data_dir: &Path) -> Verdict {
    // Absolute, so `security` reads this file whatever the folder it runs in.
    let Ok(ca_root) = std::path::absolute(CaddyPaths::in_dir(data_dir).ca_root()) else {
        return Verdict::Skip(Skip::NotChecked);
    };
    let exists = ca_root.is_file();
    let trusted = exists
        .then(|| user_trust::trusted(&RealRunner, &ca_root, &CancellationToken::new()))
        .flatten();
    doctor::trust(exists, trusted)
}

/// `PATH` as this terminal has it, against the shims of `data_dir`.
fn path(data_dir: &Path) -> Verdict {
    let shims = shims_dir(data_dir);
    let entries: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    doctor::path(&entries, &shims, |dir| probe::program_in(dir, "php"))
}

fn log(data_dir: &Path) -> Verdict {
    let tail = probe::read_tail(&log_path(data_dir), LOG_TAIL)
        .map_err(|err| tracing::debug!(error = %err, "cannot read the daemon log"));
    doctor::log(tail.as_ref().map(Option::as_deref).map_err(|_| ()))
}

/// The folder of the shims, absolute: `PATH` holds absolute folders.
fn shims_dir(data_dir: &Path) -> PathBuf {
    let shims = data_dir.join("shims");
    std::path::absolute(&shims).unwrap_or(shims)
}

fn log_path(data_dir: &Path) -> PathBuf {
    data_dir.join("logs").join("marsad.log")
}

/// The line of one check. `windows`: the PATH fixes speak of the user's
/// PATH setting, not of `~/.zshrc`.
pub fn render(check: Check, verdict: &Verdict, data_dir: &Path, windows: bool) -> String {
    match verdict {
        Verdict::Pass(pass) => strings::doctor_pass(&passed(pass, data_dir)),
        Verdict::Fail(problem) => {
            let (text, fix) = problem_text(problem, data_dir, windows);
            strings::doctor_fail(&text, &fix)
        }
        Verdict::Skip(skip) => strings::doctor_skip(check, *skip),
    }
}

fn passed(pass: &Pass, data_dir: &Path) -> String {
    match pass {
        Pass::Daemon { version, pid, port } => {
            strings::daemon_running(&printable(version), *pid, *port)
        }
        Pass::Runtimes { default, version } => {
            strings::doctor_runtimes_ok(&printable(default), &version.to_string())
        }
        Pass::Caddy => strings::DOCTOR_CADDY_OK.to_owned(),
        Pass::Dns => strings::DOCTOR_DNS_OK.to_owned(),
        Pass::Ports => strings::DOCTOR_PORTS_OK.to_owned(),
        Pass::Trust => strings::DOCTOR_TRUST_OK.to_owned(),
        Pass::Path => strings::DOCTOR_PATH_OK.to_owned(),
        Pass::Log => strings::doctor_log_ok(&log_path(data_dir)),
    }
}

/// What is wrong, and the fix.
fn problem_text(problem: &Problem, data_dir: &Path, windows: bool) -> (String, String) {
    let pair = |text: &str, fix: &str| (text.to_owned(), fix.to_owned());
    // The sentence of an error code, which says what to do.
    let code_fix = |code: &str, path: Option<&Path>| {
        let details = Details {
            path,
            ..Details::default()
        };
        strings::message(code, &details)
    };
    match problem {
        Problem::DaemonNotRunning => {
            pair(strings::DAEMON_NOT_RUNNING, strings::DOCTOR_START_DAEMON)
        }
        Problem::DaemonError { code } => (
            strings::doctor_daemon_error(&printable(code)),
            ClientFailure::new(code, data_dir, None).message,
        ),
        Problem::NotInstalled { php, caddy } => strings::doctor_not_installed(*php, *caddy),
        Problem::RuntimesUnreadable { code } => (
            strings::doctor_runtimes_unreadable(code),
            code_fix(code, Some(RuntimesDir::in_dir(data_dir).root())),
        ),
        Problem::ConfigUnreadable { code } => (
            strings::doctor_config_unreadable(code),
            code_fix(code, Some(&ConfigPaths::in_dir(data_dir).global)),
        ),
        Problem::NoDefaultPhp { suggest } => (
            strings::DOCTOR_NO_DEFAULT.to_owned(),
            strings::doctor_use_php(suggest),
        ),
        Problem::DefaultNotInstalled { default, suggest } => (
            strings::doctor_default_not_installed(&printable(default)),
            strings::doctor_use_php(suggest),
        ),
        Problem::CaddyNotRunning { state } => (
            strings::doctor_caddy_state(state),
            strings::DOCTOR_START_CADDY.to_owned(),
        ),
        Problem::CaddyFailed { code } => (
            strings::doctor_caddy_failed(&printable(code)),
            ClientFailure::new(code, data_dir, Some(CADDY_ID)).message,
        ),
        Problem::DnsNoAnswer => pair(strings::DOCTOR_DNS_NO_ANSWER, strings::DOCTOR_DNS_SETUP),
        Problem::DnsWrongAddress { addresses } => {
            let list: Vec<String> = addresses.iter().map(ToString::to_string).collect();
            (
                strings::doctor_dns_wrong(&list.join(", ")),
                strings::DOCTOR_DNS_FOREIGN.to_owned(),
            )
        }
        Problem::PortsHeld { holders } => {
            let mut text = strings::DOCTOR_PORTS_HELD.to_owned();
            for holder in holders {
                text.push_str("\n  ");
                text.push_str(&port_held(holder));
            }
            (text, strings::DOCTOR_FREE_PORTS.to_owned())
        }
        Problem::PortsNotRouted { ports } => (
            strings::doctor_ports_not_routed(ports),
            strings::DOCTOR_PORTS_SETUP.to_owned(),
        ),
        Problem::CaUntrusted => pair(strings::DOCTOR_UNTRUSTED, strings::DOCTOR_TRUST_SETUP),
        Problem::ShimsNotInPath { shims } => (
            strings::doctor_shims_missing(shims),
            strings::doctor_add_shims(shims, windows),
        ),
        Problem::PhpBeforeShims { php, shims } => (
            strings::doctor_php_first(php),
            strings::doctor_move_shims(shims, windows),
        ),
        Problem::LogError { line } => {
            let log = log_path(data_dir);
            let line: String = printable(line).chars().take(MAX_LINE).collect();
            (
                strings::doctor_log_error(&log, &line),
                strings::doctor_read_log(&log),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::types::PortHolder;

    fn data() -> PathBuf {
        std::env::temp_dir().join("marsa-cli-doctor")
    }

    fn fail(problem: Problem) -> String {
        render(Check::Daemon, &Verdict::Fail(problem), &data(), false)
    }

    #[test]
    fn a_pass_is_one_line_with_a_tick() {
        let data = data();
        let pass = |pass: Pass| render(Check::Daemon, &Verdict::Pass(pass), &data, false);
        assert_eq!(
            pass(Pass::Daemon {
                version: "0.1\u{1b}[2J.0".into(),
                pid: 42,
                port: 5000
            }),
            "✓ marsad is running: version 0.1[2J.0, PID 42, port 5000."
        );
        assert_eq!(
            pass(Pass::Runtimes {
                default: "8.4".into(),
                version: semver::Version::new(8, 4, 23)
            }),
            "✓ PHP and Caddy are installed, and PHP 8.4 is the default (8.4.23)."
        );
        assert_eq!(pass(Pass::Ports), "✓ Ports 80 and 443 reach Caddy.");
        assert_eq!(
            pass(Pass::Log),
            format!(
                "✓ No errors in {} since marsad last started.",
                log_path(&data).display()
            )
        );
    }

    #[test]
    fn a_failure_has_its_fix_on_the_next_line() {
        assert_eq!(
            fail(Problem::DaemonNotRunning),
            "✗ marsad is not running.\n  Fix: Run \"marsa daemon start\"."
        );
        assert_eq!(
            fail(Problem::NotInstalled {
                php: true,
                caddy: true
            }),
            "✗ PHP and Caddy are not installed.\n  Fix: Run \"marsa install php\", then \
             \"marsa install caddy\"."
        );
        assert_eq!(
            fail(Problem::DefaultNotInstalled {
                default: "8.2".into(),
                suggest: "8.4".into()
            }),
            "✗ The default PHP is 8.2, but no installed PHP matches it.\n  Fix: Run \"marsa \
             use php 8.4\"."
        );
        assert_eq!(
            fail(Problem::CaddyNotRunning {
                state: ServiceState::Stopped
            }),
            "✗ Caddy is stopped.\n  Fix: Run \"marsa services start caddy\"."
        );
        assert_eq!(
            fail(Problem::PortsNotRouted { ports: vec![443] }),
            format!(
                "✗ Port 443 does not reach Caddy.\n  Fix: {}",
                strings::DOCTOR_PORTS_SETUP
            )
        );
    }

    #[test]
    fn an_error_code_gives_its_own_fix() {
        assert_eq!(
            fail(Problem::CaddyFailed {
                code: "port_in_use".into()
            }),
            "✗ Caddy stopped with an error. [port_in_use]\n  Fix: A port Caddy needs is used \
             by another program. Run \"marsa logs caddy\" to see which one."
        );
        let global = ConfigPaths::in_dir(&data()).global;
        let text = fail(Problem::ConfigUnreadable {
            code: "config_invalid".into(),
        });
        assert!(
            text.contains(&format!("  Fix: {} has an error", global.display())),
            "{text}"
        );
        let text = fail(Problem::DaemonError {
            code: "daemon_unreachable".into(),
        });
        assert!(
            text.starts_with("✗ Cannot reach marsad. [daemon_unreachable]\n  Fix: Cannot reach"),
            "{text}"
        );
    }

    #[test]
    fn port_holders_are_named_under_the_line() {
        let holder = PortHolder {
            port: 80,
            address: "127.0.0.1".into(),
            process: Some("nginx-arm64".into()),
            pid: Some(15464),
            user: None,
            command: None,
            app: Some("Herd".into()),
        };
        assert_eq!(
            fail(Problem::PortsHeld {
                holders: vec![holder]
            }),
            "✗ Another program uses port 80 or 443:\n    Port 80 is used by nginx-arm64 (PID \
             15464, Herd).\n  Fix: Stop that program, or turn off its web server, then run \
             \"marsa setup\"."
        );
        let addresses = vec!["192.168.1.5".parse().unwrap(), "::1".parse().unwrap()];
        assert!(
            fail(Problem::DnsWrongAddress { addresses })
                .starts_with("✗ .test names resolve to 192.168.1.5, ::1, not to 127.0.0.1.")
        );
    }

    #[test]
    fn the_path_fix_fits_the_system() {
        let shims = PathBuf::from("/data/shims");
        let missing = Verdict::Fail(Problem::ShimsNotInPath {
            shims: shims.clone(),
        });
        let unix = render(Check::Path, &missing, &data(), false);
        // Every interactive zsh reads ~/.zshrc; a terminal that is not a
        // login shell never reads ~/.zprofile (task M5-18).
        assert!(
            unix.contains("~/.zshrc") && !unix.contains(".zprofile"),
            "{unix}"
        );
        assert!(
            unix.ends_with(&format!(
                "then open a new terminal: export PATH=\"{}:$PATH\"",
                shims.display()
            )),
            "{unix}"
        );
        let windows = render(Check::Path, &missing, &data(), true);
        assert!(
            windows.ends_with(&format!(
                "Fix: Add {} to the start of your user PATH, then open a new terminal.",
                shims.display()
            )),
            "{windows}"
        );
        let first = Verdict::Fail(Problem::PhpBeforeShims {
            php: PathBuf::from("/opt/homebrew/bin/php"),
            shims,
        });
        let text = render(Check::Path, &first, &data(), false);
        assert!(
            text.starts_with("✗ /opt/homebrew/bin/php comes before Marsa's shims in PATH"),
            "{text}"
        );
        assert!(
            text.contains("~/.zshrc") && text.contains("after any other line that changes PATH"),
            "{text}"
        );
    }

    #[test]
    fn the_log_line_is_cleaned_and_cut() {
        let line = format!("2026 ERROR x: \u{1b}[2J{}", "y".repeat(500));
        let text = fail(Problem::LogError { line });
        let log = log_path(&data());
        let shown = text.lines().nth(1).unwrap();
        assert!(shown.starts_with("    2026 ERROR x: [2Jyyy"), "{text}");
        assert_eq!(shown.chars().count(), 4 + MAX_LINE);
        assert!(!text.contains('\u{1b}'));
        assert!(
            text.starts_with(&format!(
                "✗ The last error in {} since marsad started:\n",
                log.display()
            )),
            "{text}"
        );
    }

    #[test]
    fn the_daemon_logs_the_start_line_doctor_looks_for() {
        let server = include_str!("../daemon/server.rs");
        let message = format!("\"{}\");", doctor::STARTED);
        assert!(server.contains(&message), "{message}");
    }

    #[test]
    fn a_skipped_check_says_why() {
        let skip = |check, skip| render(check, &Verdict::Skip(skip), &data(), false);
        assert_eq!(
            skip(Check::Dns, Skip::NotOnThisSystem),
            "- .test names: not checked. Marsa checks this on macOS only for now."
        );
        assert_eq!(
            skip(Check::Ports, Skip::NeedsCaddy),
            "- Ports 80 and 443: not checked. It needs Caddy, which is not running."
        );
        assert_eq!(
            skip(Check::Trust, Skip::NoCa),
            "- Marsa's certificate authority: not checked. It does not exist yet: Caddy makes \
             it when it first starts. Run \"marsa setup\" to make it and trust it."
        );
    }
}
