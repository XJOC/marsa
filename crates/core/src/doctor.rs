//! `marsa doctor` (task M5-12): eight read-only checks, each with a verdict
//! and, when it fails, a fix. Facts in, verdicts out.
//!
//! The command gathers the facts: the daemon's answers, files in the data
//! folder, and the system's own answers (`platform::probe`, and `netstat`,
//! `ps`, and `security` through the setup runner of `marsad`). Each function
//! here turns the facts of one check into a [`Verdict`] and reads nothing
//! itself, so the tests run no program and open no socket. The text of each
//! verdict is in the CLI's strings module (AGENTS.md section 9).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

use semver::Version;

use crate::caddy::GREETING;
use crate::resolver::best_match;
use crate::types::{Health, PortHolder, ServiceState};

/// The checks, in the order `marsa doctor` runs and prints them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Daemon,
    Runtimes,
    Caddy,
    Dns,
    Ports,
    Trust,
    Path,
    Log,
}

/// What one check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// ✓
    Pass(Pass),
    /// ✗, with a fix: `marsa doctor` exits with code 1.
    Fail(Problem),
    /// Not checked, and why. Not a failure.
    Skip(Skip),
}

/// A check that passed, with what its line shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pass {
    Daemon {
        version: String,
        pid: u32,
        port: u16,
    },
    /// The default PHP as `global.toml` has it (`8.4`), and the installed
    /// version it picks (`8.4.23`).
    Runtimes {
        default: String,
        version: Version,
    },
    Caddy,
    Dns,
    Ports,
    Trust,
    Path,
    Log,
}

/// What is wrong. The CLI shows each with its fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    DaemonNotRunning,
    /// The daemon could not be asked: the code of the client error.
    DaemonError {
        code: String,
    },
    NotInstalled {
        php: bool,
        caddy: bool,
    },
    /// `runtimes/` could not be listed: the error code.
    RuntimesUnreadable {
        code: String,
    },
    /// `global.toml` could not be read: the error code.
    ConfigUnreadable {
        code: String,
    },
    /// `suggest`: the newest installed PHP as `major.minor`, for `marsa use`.
    NoDefaultPhp {
        suggest: String,
    },
    DefaultNotInstalled {
        default: String,
        suggest: String,
    },
    /// Stopped, starting, or stopping.
    CaddyNotRunning {
        state: ServiceState,
    },
    CaddyFailed {
        code: String,
    },
    DnsNoAnswer,
    DnsWrongAddress {
        addresses: Vec<IpAddr>,
    },
    PortsHeld {
        holders: Vec<PortHolder>,
    },
    /// The ports that do not reach Marsa's Caddy: 80, 443, or both.
    PortsNotRouted {
        ports: Vec<u16>,
    },
    CaUntrusted,
    ShimsNotInPath {
        shims: PathBuf,
    },
    /// `php`: the program found in `PATH` before `shims`.
    PhpBeforeShims {
        php: PathBuf,
        shims: PathBuf,
    },
    /// The last error line of the daemon log since its last start.
    LogError {
        line: String,
    },
}

/// Why a check did not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// Marsa has no such check on this system yet (only macOS for now).
    NotOnThisSystem,
    NeedsDaemon,
    NeedsCaddy,
    CaddyNotInstalled,
    /// No CA yet: Caddy makes it when it first starts (task M5-15).
    NoCa,
    NoLog,
    /// The check could not get its facts (`netstat` or `security` failed,
    /// the daemon gave no answer). The log says why.
    NotChecked,
}

/// The facts of the runtimes check, read from the data folder. An error is
/// its code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub php: Result<Vec<Version>, String>,
    pub caddy: Result<Vec<Version>, String>,
    /// `[php] default` in `global.toml`.
    pub default_php: Result<Option<String>, String>,
}

/// The facts of the ports check (macOS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ports {
    /// Who listens on 80 and 443; `None` if `netstat` could not tell.
    pub holders: Option<Vec<PortHolder>>,
    /// `None` when the daemon does not run.
    pub caddy_running: Option<bool>,
    /// `127.0.0.1:80` answered with Marsa's greeting ([`is_marsa_answer`]).
    pub http: bool,
    /// `127.0.0.1:443` took a connection. `None`: not tried, because no
    /// site is secure, so Caddy has no HTTPS listener.
    pub https: Option<bool>,
}

fn fail(problem: Problem) -> Verdict {
    Verdict::Fail(problem)
}

/// `answer`: the health and port of the daemon, or the code of the client
/// error. No daemon is `daemon_not_running`, or `daemon_file_missing` while
/// one starts or stops.
pub fn daemon(answer: Result<(Health, u16), String>) -> Verdict {
    match answer {
        Ok((health, port)) => Verdict::Pass(Pass::Daemon {
            version: health.app_version,
            pid: health.pid,
            port,
        }),
        Err(code) if code == "daemon_not_running" || code == "daemon_file_missing" => {
            fail(Problem::DaemonNotRunning)
        }
        Err(code) => fail(Problem::DaemonError { code }),
    }
}

/// PHP and Caddy installed, and a default PHP that an installed version
/// matches, by the rules of a `.php-version` file.
pub fn runtimes(facts: &Installed) -> Verdict {
    let (php, caddy) = match (&facts.php, &facts.caddy) {
        (Err(code), _) | (_, Err(code)) => {
            return fail(Problem::RuntimesUnreadable { code: code.clone() });
        }
        (Ok(php), Ok(caddy)) => (php, caddy),
    };
    let Some(suggest) = suggestion(php).filter(|_| !caddy.is_empty()) else {
        return fail(Problem::NotInstalled {
            php: php.is_empty(),
            caddy: caddy.is_empty(),
        });
    };
    match &facts.default_php {
        Err(code) => fail(Problem::ConfigUnreadable { code: code.clone() }),
        Ok(None) => fail(Problem::NoDefaultPhp { suggest }),
        Ok(Some(default)) => match best_match(default, php) {
            Ok(Some(version)) => Verdict::Pass(Pass::Runtimes {
                default: default.trim().to_owned(),
                version,
            }),
            // A default that is not a version matches nothing either.
            Ok(None) | Err(_) => fail(Problem::DefaultNotInstalled {
                default: default.trim().to_owned(),
                suggest,
            }),
        },
    }
}

/// What to pass to `marsa use php`: the newest stable version as `8.4`, or
/// the newest pre-release in full, which `8.5` would not pick.
fn suggestion(php: &[Version]) -> Option<String> {
    let stable = php.iter().filter(|version| version.pre.is_empty()).max();
    match stable {
        Some(version) => Some(format!("{}.{}", version.major, version.minor)),
        None => php.iter().max().map(Version::to_string),
    }
}

/// `state`: Caddy's state in `GET /v1/services`, `None` if the daemon gave
/// none. `installed`: a Caddy is in `runtimes/caddy`.
pub fn caddy(daemon_running: bool, state: Option<&ServiceState>, installed: bool) -> Verdict {
    match state {
        _ if !daemon_running => Verdict::Skip(Skip::NeedsDaemon),
        Some(ServiceState::Running) => Verdict::Pass(Pass::Caddy),
        // The runtimes check already says to install it.
        _ if !installed => Verdict::Skip(Skip::CaddyNotInstalled),
        Some(ServiceState::Failed { code }) => fail(Problem::CaddyFailed { code: code.clone() }),
        Some(state) => fail(Problem::CaddyNotRunning {
            state: state.clone(),
        }),
        None => Verdict::Skip(Skip::NotChecked),
    }
}

/// `answer`: what the system resolver gave a random `.test` name, `None` if
/// it gave nothing in time. `marsad`'s DNS server answers `127.0.0.1`, and
/// no address for `AAAA` (task M5-17); `::1` still counts as ours, as older
/// builds answered it. Without the daemon nothing answers, which is no fault
/// of the setup; but a foreign answer is one.
pub fn dns(daemon_running: bool, answer: Option<&[IpAddr]>) -> Verdict {
    let answer = answer.unwrap_or_default();
    if answer.contains(&IpAddr::V4(Ipv4Addr::LOCALHOST))
        && answer.iter().all(|ip| {
            *ip == IpAddr::V4(Ipv4Addr::LOCALHOST) || *ip == IpAddr::V6(Ipv6Addr::LOCALHOST)
        })
    {
        Verdict::Pass(Pass::Dns)
    } else if !answer.is_empty() {
        let mut addresses = answer.to_vec();
        addresses.sort();
        addresses.dedup();
        fail(Problem::DnsWrongAddress { addresses })
    } else if !daemon_running {
        Verdict::Skip(Skip::NeedsDaemon)
    } else {
        fail(Problem::DnsNoAnswer)
    }
}

/// Ports 80 and 443 reach Marsa's Caddy through the `pf` redirect (ADR
/// 0008). A program that listens on either fails the check even if the
/// redirect works: the redirect takes its traffic without a word.
pub fn ports(facts: Ports) -> Verdict {
    let Some(holders) = facts.holders else {
        return Verdict::Skip(Skip::NotChecked);
    };
    if !holders.is_empty() {
        return fail(Problem::PortsHeld { holders });
    }
    match facts.caddy_running {
        None => return Verdict::Skip(Skip::NeedsDaemon),
        Some(false) => return Verdict::Skip(Skip::NeedsCaddy),
        Some(true) => {}
    }
    let mut ports = Vec::new();
    if !facts.http {
        ports.push(80);
    }
    if facts.https == Some(false) {
        ports.push(443);
    }
    if ports.is_empty() {
        Verdict::Pass(Pass::Ports)
    } else {
        fail(Problem::PortsNotRouted { ports })
    }
}

/// `trusted`: what `security verify-cert` said about Marsa's CA root, `None`
/// if it could not run.
pub fn trust(ca_exists: bool, trusted: Option<bool>) -> Verdict {
    match trusted {
        _ if !ca_exists => Verdict::Skip(Skip::NoCa),
        Some(true) => Verdict::Pass(Pass::Trust),
        Some(false) => fail(Problem::CaUntrusted),
        None => Verdict::Skip(Skip::NotChecked),
    }
}

/// `entries`: `PATH`, split. `php_in`: the `php` program in a folder, if
/// there is one. An empty entry is skipped: it means the current folder,
/// which changes with each terminal.
pub fn path(
    entries: &[PathBuf],
    shims: &Path,
    php_in: impl Fn(&Path) -> Option<PathBuf>,
) -> Verdict {
    // `Path` compares by components, so `shims/` is `shims`.
    let Some(at) = entries.iter().position(|entry| entry == shims) else {
        return fail(Problem::ShimsNotInPath {
            shims: shims.to_path_buf(),
        });
    };
    let before = entries[..at]
        .iter()
        .filter(|entry| !entry.as_os_str().is_empty())
        .find_map(|entry| php_in(entry));
    match before {
        Some(php) => fail(Problem::PhpBeforeShims {
            php,
            shims: shims.to_path_buf(),
        }),
        None => Verdict::Pass(Pass::Path),
    }
}

/// `tail`: the end of `logs/marsad.log`, `Ok(None)` if there is no log, an
/// error if it could not be read.
pub fn log(tail: Result<Option<&str>, ()>) -> Verdict {
    match tail {
        Ok(Some(text)) => match last_error(text) {
            Some(line) => fail(Problem::LogError {
                line: line.to_owned(),
            }),
            None => Verdict::Pass(Pass::Log),
        },
        Ok(None) => Verdict::Skip(Skip::NoLog),
        Err(()) => Verdict::Skip(Skip::NotChecked),
    }
}

/// The message `marsad` logs at `info` when it starts (`daemon/server.rs`;
/// a test of `marsa doctor` holds the two together). An error before the
/// last one is from an earlier run.
pub const STARTED: &str = "marsad started";

/// The last `ERROR` line after the last start line. The daemon logs with
/// `tracing`'s default format: the time, then the level, then the rest.
pub fn last_error(tail: &str) -> Option<&str> {
    let mut error = None;
    for line in tail.lines().map(|line| line.trim_end_matches('\r')) {
        match line.split_whitespace().nth(1) {
            Some("INFO") if line.contains(STARTED) => error = None,
            Some("ERROR") => error = Some(line),
            _ => {}
        }
    }
    error
}

/// Whether an HTTP answer is Marsa's Caddy: its `marsa` server answers every
/// host that is not a site with [`GREETING`].
pub fn is_marsa_answer(answer: &[u8]) -> bool {
    answer.starts_with(b"HTTP/1.")
        && answer
            .windows(GREETING.len())
            .any(|window| window == GREETING.as_bytes())
}

/// A `.test` name no site has and no cache holds: `doctor-<16 hex>.test`.
pub fn probe_name(random: u64) -> String {
    format!("doctor-{random:016x}.test")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn installed(php: &[&str], caddy: &[&str], default: Option<&str>) -> Installed {
        Installed {
            php: Ok(php.iter().map(|text| v(text)).collect()),
            caddy: Ok(caddy.iter().map(|text| v(text)).collect()),
            default_php: Ok(default.map(str::to_owned)),
        }
    }

    fn holder(port: u16) -> PortHolder {
        PortHolder {
            port,
            address: "127.0.0.1".into(),
            process: Some("nginx-arm64".into()),
            pid: Some(15464),
            user: None,
            command: None,
            app: Some("Herd".into()),
        }
    }

    fn ports_facts() -> Ports {
        Ports {
            holders: Some(Vec::new()),
            caddy_running: Some(true),
            http: true,
            https: Some(true),
        }
    }

    #[test]
    fn the_daemon_passes_with_its_health_and_fails_with_its_code() {
        let health = Health {
            app_version: "0.1.0".into(),
            api_version: 1,
            pid: 42,
        };
        assert_eq!(
            daemon(Ok((health, 5000))),
            Verdict::Pass(Pass::Daemon {
                version: "0.1.0".into(),
                pid: 42,
                port: 5000
            })
        );
        for code in ["daemon_not_running", "daemon_file_missing"] {
            assert_eq!(daemon(Err(code.into())), fail(Problem::DaemonNotRunning));
        }
        assert_eq!(
            daemon(Err("daemon_unreachable".into())),
            fail(Problem::DaemonError {
                code: "daemon_unreachable".into()
            })
        );
    }

    #[test]
    fn runtimes_need_php_caddy_and_a_default_that_matches() {
        let pass = runtimes(&installed(
            &["8.3.12", "8.4.23"],
            &["2.11.8"],
            Some(" 8.4 "),
        ));
        assert_eq!(
            pass,
            Verdict::Pass(Pass::Runtimes {
                default: "8.4".into(),
                version: v("8.4.23")
            })
        );
        assert_eq!(
            runtimes(&installed(&[], &[], Some("8.4"))),
            fail(Problem::NotInstalled {
                php: true,
                caddy: true
            })
        );
        assert_eq!(
            runtimes(&installed(&["8.4.23"], &[], Some("8.4"))),
            fail(Problem::NotInstalled {
                php: false,
                caddy: true
            })
        );
        // The suggestion is the newest installed minor, in any order.
        assert_eq!(
            runtimes(&installed(&["8.4.23", "8.3.12"], &["2.11.8"], None)),
            fail(Problem::NoDefaultPhp {
                suggest: "8.4".into()
            })
        );
        // A pre-release is suggested in full, and only without a stable one.
        assert_eq!(
            runtimes(&installed(&["8.5.0-rc1"], &["2.11.8"], None)),
            fail(Problem::NoDefaultPhp {
                suggest: "8.5.0-rc1".into()
            })
        );
        assert_eq!(
            runtimes(&installed(&["8.5.0-rc1", "8.3.12"], &["2.11.8"], None)),
            fail(Problem::NoDefaultPhp {
                suggest: "8.3".into()
            })
        );
        for default in ["8.2", "latest", "abc"] {
            assert_eq!(
                runtimes(&installed(&["8.4.23"], &["2.11.8"], Some(default))),
                fail(Problem::DefaultNotInstalled {
                    default: default.into(),
                    suggest: "8.4".into()
                }),
                "{default}"
            );
        }
    }

    #[test]
    fn an_unreadable_folder_or_settings_file_is_its_code() {
        let mut facts = installed(&["8.4.23"], &["2.11.8"], Some("8.4"));
        facts.default_php = Err("config_invalid".into());
        assert_eq!(
            runtimes(&facts),
            fail(Problem::ConfigUnreadable {
                code: "config_invalid".into()
            })
        );
        facts.caddy = Err("runtimes_read".into());
        assert_eq!(
            runtimes(&facts),
            fail(Problem::RuntimesUnreadable {
                code: "runtimes_read".into()
            })
        );
    }

    #[test]
    fn caddy_passes_only_when_running() {
        let failed = ServiceState::Failed {
            code: "port_in_use".into(),
        };
        assert_eq!(
            caddy(true, Some(&ServiceState::Running), true),
            Verdict::Pass(Pass::Caddy)
        );
        assert_eq!(
            caddy(true, Some(&failed), true),
            fail(Problem::CaddyFailed {
                code: "port_in_use".into()
            })
        );
        for state in [
            ServiceState::Stopped,
            ServiceState::Starting,
            ServiceState::Stopping,
        ] {
            assert_eq!(
                caddy(true, Some(&state), true),
                fail(Problem::CaddyNotRunning {
                    state: state.clone()
                })
            );
        }
        assert_eq!(caddy(false, None, true), Verdict::Skip(Skip::NeedsDaemon));
        assert_eq!(
            caddy(true, Some(&ServiceState::Stopped), false),
            Verdict::Skip(Skip::CaddyNotInstalled)
        );
        assert_eq!(caddy(true, None, true), Verdict::Skip(Skip::NotChecked));
    }

    #[test]
    fn test_names_must_resolve_to_loopback_only() {
        let v4 = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let v6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let lan: IpAddr = "192.168.1.5".parse().unwrap();
        assert_eq!(dns(true, Some(&[v4])), Verdict::Pass(Pass::Dns));
        assert_eq!(dns(true, Some(&[v6, v4, v4])), Verdict::Pass(Pass::Dns));
        // `::1` alone is not 127.0.0.1.
        assert_eq!(
            dns(true, Some(&[v6])),
            fail(Problem::DnsWrongAddress {
                addresses: vec![v6]
            })
        );
        assert_eq!(
            dns(false, Some(&[lan, v4, lan])),
            fail(Problem::DnsWrongAddress {
                addresses: vec![v4, lan]
            })
        );
        assert_eq!(dns(true, None), fail(Problem::DnsNoAnswer));
        assert_eq!(dns(true, Some(&[])), fail(Problem::DnsNoAnswer));
        assert_eq!(dns(false, None), Verdict::Skip(Skip::NeedsDaemon));
    }

    #[test]
    fn a_port_holder_fails_before_anything_else() {
        let held = Ports {
            holders: Some(vec![holder(80), holder(443)]),
            caddy_running: None,
            http: false,
            https: None,
        };
        assert_eq!(
            ports(held),
            fail(Problem::PortsHeld {
                holders: vec![holder(80), holder(443)]
            })
        );
        assert_eq!(ports(ports_facts()), Verdict::Pass(Pass::Ports));
        // No secure site: 443 is not tried.
        let plain = Ports {
            https: None,
            ..ports_facts()
        };
        assert_eq!(ports(plain), Verdict::Pass(Pass::Ports));
    }

    #[test]
    fn ports_that_miss_caddy_are_named() {
        let cases = [
            (false, Some(true), vec![80]),
            (true, Some(false), vec![443]),
            (false, Some(false), vec![80, 443]),
            (false, None, vec![80]),
        ];
        for (http, https, want) in cases {
            let facts = Ports {
                http,
                https,
                ..ports_facts()
            };
            assert_eq!(
                ports(facts),
                fail(Problem::PortsNotRouted {
                    ports: want.clone()
                }),
                "{want:?}"
            );
        }
        let skipped = [
            (None, Some(true), Skip::NotChecked),
            (Some(Vec::new()), None, Skip::NeedsDaemon),
            (Some(Vec::new()), Some(false), Skip::NeedsCaddy),
        ];
        for (holders, caddy_running, want) in skipped {
            let facts = Ports {
                holders,
                caddy_running,
                ..ports_facts()
            };
            assert_eq!(ports(facts), Verdict::Skip(want), "{want:?}");
        }
    }

    #[test]
    fn the_ca_is_checked_only_once_it_exists() {
        assert_eq!(trust(true, Some(true)), Verdict::Pass(Pass::Trust));
        assert_eq!(trust(true, Some(false)), fail(Problem::CaUntrusted));
        assert_eq!(trust(true, None), Verdict::Skip(Skip::NotChecked));
        assert_eq!(trust(false, Some(false)), Verdict::Skip(Skip::NoCa));
    }

    #[test]
    fn the_shims_must_come_before_any_other_php() {
        let shims = Path::new("/data/shims");
        let brew = Path::new("/opt/homebrew/bin");
        let php_in = |dir: &Path| (dir == brew).then(|| dir.join("php"));
        let entries = |list: &[&str]| list.iter().map(PathBuf::from).collect::<Vec<_>>();

        let first = entries(&["/usr/bin", "/data/shims/", "/opt/homebrew/bin"]);
        assert_eq!(path(&first, shims, php_in), Verdict::Pass(Pass::Path));
        let after = entries(&["", "/opt/homebrew/bin", "/data/shims"]);
        assert_eq!(
            path(&after, shims, php_in),
            fail(Problem::PhpBeforeShims {
                php: brew.join("php"),
                shims: shims.to_path_buf()
            })
        );
        // Not in PATH at all: that is the problem, not the other PHP.
        let missing = entries(&["/opt/homebrew/bin", "/usr/bin"]);
        assert_eq!(
            path(&missing, shims, php_in),
            fail(Problem::ShimsNotInPath {
                shims: shims.to_path_buf()
            })
        );
        // An empty entry is never looked into.
        let empty = entries(&["", "/data/shims"]);
        let never = |dir: &Path| -> Option<PathBuf> { panic!("looked into {dir:?}") };
        assert_eq!(path(&empty, shims, never), Verdict::Pass(Pass::Path));
    }

    #[test]
    fn the_last_error_since_the_last_start_is_shown() {
        let log = "\
2026-11-14T09:00:00.000001Z ERROR marsa::daemon::caddy: old error
2026-11-14T10:00:00.000001Z  INFO marsa::daemon::server: marsad started port=5000 pid=42
2026-11-14T10:00:01.000001Z ERROR marsa::daemon::caddy: first error\r
2026-11-14T10:00:02.000001Z  WARN marsa::daemon: a warning
2026-11-14T10:00:03.000001Z ERROR marsa::daemon::php: second error\r
2026-11-14T10:00:04.000001Z  INFO marsa::daemon: the message says ERROR
";
        assert_eq!(
            last_error(log),
            Some("2026-11-14T10:00:03.000001Z ERROR marsa::daemon::php: second error")
        );
        let restarted = format!(
            "{log}2026-11-14T11:00:00.000001Z  INFO marsa::daemon::server: marsad started \
             port=5001 pid=43\n"
        );
        assert_eq!(last_error(&restarted), None);
        assert_eq!(last_error(""), None);
        assert_eq!(
            log_verdict(log),
            fail(Problem::LogError {
                line: "2026-11-14T10:00:03.000001Z ERROR marsa::daemon::php: second error".into()
            })
        );
        assert_eq!(log_verdict(&restarted), Verdict::Pass(Pass::Log));
        assert_eq!(super::log(Ok(None)), Verdict::Skip(Skip::NoLog));
        assert_eq!(super::log(Err(())), Verdict::Skip(Skip::NotChecked));
    }

    fn log_verdict(text: &str) -> Verdict {
        super::log(Ok(Some(text)))
    }

    #[test]
    fn only_the_greeting_of_marsas_caddy_counts() {
        let caddy = format!(
            "HTTP/1.1 200 OK\r\nServer: Caddy\r\nContent-Length: {}\r\n\r\n{GREETING}",
            GREETING.len()
        );
        assert!(is_marsa_answer(caddy.as_bytes()));
        assert!(!is_marsa_answer(
            b"HTTP/1.1 200 OK\r\nServer: nginx\r\n\r\nWelcome to nginx!"
        ));
        assert!(!is_marsa_answer(GREETING.as_bytes()));
        assert!(!is_marsa_answer(b""));
    }

    #[test]
    fn the_probe_name_is_a_valid_test_name() {
        assert_eq!(probe_name(0xab), "doctor-00000000000000ab.test");
        let name = probe_name(u64::MAX);
        let label = name.strip_suffix(".test").unwrap();
        assert!(crate::sites::validate_name(label).is_ok(), "{name}");
    }
}
