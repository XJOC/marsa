//! `marsa doctor`: eight read-only checks, each with a verdict
//! and, when it fails, a fix. Facts in, verdicts out.
//!
//! The command gathers the facts: the daemon's answers, files in the data
//! folder, and the system's own answers (`platform::probe`, and `netstat`,
//! `ps`, and `security` through the setup runner of `marsad`). Each function
//! here turns the facts of one check into a [`Verdict`] and reads nothing
//! itself, so the tests run no program and open no socket. The text of each
//! verdict is in the CLI's strings module (AGENTS.md section 9).

use std::collections::BTreeSet;
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
    /// Windows only: the Microsoft Visual C++ runtime PHP needs (ADR 0019).
    VcRuntime,
    /// Linux only: `/usr/bin/xz`, which unpacks MySQL's `tar.xz`.
    Xz,
    /// Windows only: the roots PHP's curl trusts, from Windows' store.
    SystemRoots,
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
    /// The installed runtime's version, `14.44.35211`.
    VcRuntime {
        version: String,
    },
    /// Linux: `/usr/bin/xz` is there.
    Xz,
    /// PHP's CA bundle holds every root Windows trusts for HTTPS now:
    /// `count` of them, with `left_out` others of the store left out, and
    /// Marsa's CA when `marsa`; the bundle was written `age_days` ago.
    SystemRoots {
        count: usize,
        left_out: usize,
        marsa: bool,
        age_days: u64,
    },
    Caddy,
    Dns,
    /// Linux on the hosts way: every site has its line in the hosts file.
    Hosts,
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
    /// Linux on the `marsa0` link: a `.test` name did not resolve. Its fix
    /// names the link, not macOS's resolver file.
    LinkNoAnswer,
    DnsWrongAddress {
        addresses: Vec<IpAddr>,
    },
    /// Linux on the hosts way: the sites whose names are not in Marsa's
    /// block yet, sorted.
    HostsMissing {
        names: Vec<String>,
    },
    /// Linux: this data folder's setup wrote Marsa's block in the hosts
    /// file, and it is gone. Another program may have written the file
    /// again (WSL and cloud-init do at boot).
    HostsBlockGone,
    PortsHeld {
        holders: Vec<PortHolder>,
    },
    /// The ports that do not reach Marsa's Caddy: 80, 443, or both.
    PortsNotRouted {
        ports: Vec<u16>,
    },
    CaUntrusted,
    /// Linux: no `/usr/bin/xz`, so MySQL, which comes as `tar.xz`, cannot be
    /// installed.
    XzMissing,
    /// Windows: no Microsoft Visual C++ runtime (x64), so the official PHP
    /// does not start.
    VcRuntimeMissing,
    /// Windows: a runtime older than [`VC_RUNTIME_MIN`]: PHP 8.4 and 8.5
    /// need a newer one.
    VcRuntimeOld {
        found: String,
    },
    /// Windows: Marsa cannot read the roots Windows trusts, so PHP trusts
    /// only Marsa's CA, and its HTTPS calls to the internet fail.
    SystemRootsUnreadable {
        error: String,
    },
    /// Windows: the store gave no root for HTTPS, with the same effect.
    SystemRootsEmpty,
    /// Windows: roots Windows trusts now are not in PHP's CA bundle yet.
    SystemRootsMissing {
        missing: usize,
    },
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
    /// Marsa has no such check on this system yet: ports and trust on
    /// Linux.
    NotOnThisSystem,
    NeedsDaemon,
    NeedsCaddy,
    CaddyNotInstalled,
    /// No CA yet: Caddy makes it when it first starts.
    NoCa,
    /// Windows: no CA bundle for PHP yet; marsad writes it when it starts.
    NoBundle,
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
        Err(code)
            if code == crate::codes::DAEMON_NOT_RUNNING
                || code == crate::codes::DAEMON_FILE_MISSING =>
        {
            fail(Problem::DaemonNotRunning)
        }
        Err(code) => fail(Problem::DaemonError { code }),
    }
}

/// The oldest Visual C++ runtime the official PHP builds run on: 8.4 and
/// 8.5 are built with MSVC 14.44, and Microsoft asks for a runtime of the
/// same version or later (ADR 0019). Raise it when `recipes/pins.toml`
/// moves to PHP built with a newer MSVC (vs18 is 14.5x): `doctor` would
/// pass on an older runtime that PHP fails on.
pub const VC_RUNTIME_MIN: (u32, u32) = (14, 44);

/// Windows: the facts of the check of PHP's CA bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootsFacts {
    /// The DER of each root Windows trusts for HTTPS now, and how many other
    /// roots the store holds; or why the store could not be read.
    pub windows: Result<(Vec<Vec<u8>>, usize), String>,
    /// The bundle marsad wrote, and its age in days; `None` when there is
    /// none.
    pub bundle: Option<(String, u64)>,
    /// Marsa's root certificate, once Caddy has made it.
    pub marsa_root: Option<String>,
}

/// Windows: whether PHP's CA bundle holds every root that Windows trusts for
/// HTTPS now (ADR 0020). The roots are read here again, the same way marsad
/// reads them, and each must be in the bundle, so an old bundle that is
/// still complete passes. An empty store fails: every Windows has roots.
pub fn system_roots(facts: RootsFacts) -> Verdict {
    let (roots, left_out) = match facts.windows {
        Err(error) => return Verdict::Fail(Problem::SystemRootsUnreadable { error }),
        Ok((roots, _)) if roots.is_empty() => return Verdict::Fail(Problem::SystemRootsEmpty),
        Ok(found) => found,
    };
    let Some((bundle, age_days)) = facts.bundle else {
        return Verdict::Skip(Skip::NoBundle);
    };
    let missing = roots
        .iter()
        .filter(|der| !bundle.contains(&crate::caddy::pem_certificates(std::slice::from_ref(der))))
        .count();
    if missing > 0 {
        return Verdict::Fail(Problem::SystemRootsMissing { missing });
    }
    let marsa = facts
        .marsa_root
        .is_some_and(|root| !root.trim().is_empty() && bundle.contains(root.trim()));
    Verdict::Pass(Pass::SystemRoots {
        count: roots.len(),
        left_out,
        marsa,
        age_days,
    })
}

/// Linux: whether `/usr/bin/xz` is there.
#[must_use]
pub fn xz(available: bool) -> Verdict {
    if available {
        Verdict::Pass(Pass::Xz)
    } else {
        Verdict::Fail(Problem::XzMissing)
    }
}

/// Windows: the Visual C++ runtime that Windows reports, `(major, minor,
/// build)`, or `None` when it reports none.
pub fn vc_runtime(found: Option<(u32, u32, u32)>) -> Verdict {
    match found {
        None => Verdict::Fail(Problem::VcRuntimeMissing),
        Some((major, minor, build)) => {
            let version = format!("{major}.{minor}.{build}");
            if (major, minor) >= VC_RUNTIME_MIN {
                Verdict::Pass(Pass::VcRuntime { version })
            } else {
                Verdict::Fail(Problem::VcRuntimeOld { found: version })
            }
        }
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
/// no address for `AAAA`; `::1` still counts as ours, as older
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

/// Linux on the `marsa0` link: [`dns`], with the link's own fix.
pub fn link(daemon_running: bool, answer: Option<&[IpAddr]>) -> Verdict {
    match dns(daemon_running, answer) {
        Verdict::Fail(Problem::DnsNoAnswer) => fail(Problem::LinkNoAnswer),
        verdict => verdict,
    }
}

/// Linux on the hosts way (ADR 0014): `block`, the names in
/// Marsa's block of the hosts file (`None`: no block); `sites`, the host
/// names of the sites (`None`: the daemon gave no list). Every site needs
/// its line; a name in the block that is no site does no harm.
pub fn hosts(block: Option<&BTreeSet<String>>, sites: Option<&[String]>) -> Verdict {
    let Some(block) = block else {
        return fail(Problem::HostsBlockGone);
    };
    let Some(sites) = sites else {
        return Verdict::Skip(Skip::NeedsDaemon);
    };
    let mut names: Vec<String> = sites
        .iter()
        .filter(|name| !block.contains(*name))
        .cloned()
        .collect();
    if names.is_empty() {
        return Verdict::Pass(Pass::Hosts);
    }
    names.sort();
    names.dedup();
    fail(Problem::HostsMissing { names })
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

/// Windows: Caddy listens on 80 and 443 itself, so a holder
/// fails only when it is not Caddy: IIS, or `System` (PID 4) for another
/// HTTP.sys server. `caddy`: `None` when the daemon does not run,
/// `Some(None)` when Caddy does not, else Caddy's PID.
pub fn windows_ports(holders: Option<Vec<PortHolder>>, caddy: Option<Option<u32>>) -> Verdict {
    let Some(holders) = holders else {
        return Verdict::Skip(Skip::NotChecked);
    };
    let caddy_pid = caddy.flatten();
    let (caddys, others): (Vec<PortHolder>, Vec<PortHolder>) = holders
        .into_iter()
        .partition(|holder| caddy_pid.is_some() && holder.pid == caddy_pid);
    if !others.is_empty() {
        return fail(Problem::PortsHeld { holders: others });
    }
    match caddy {
        None => Verdict::Skip(Skip::NeedsDaemon),
        Some(None) => Verdict::Skip(Skip::NeedsCaddy),
        // Port 80 always: Caddy answers there for every site, secure or not.
        Some(Some(_)) if caddys.iter().any(|holder| holder.port == 80) => {
            Verdict::Pass(Pass::Ports)
        }
        Some(Some(_)) => fail(Problem::PortsNotRouted { ports: vec![80] }),
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

    #[test]
    fn the_windows_roots_must_all_be_in_phps_bundle() {
        let roots = vec![vec![1, 2, 3], vec![4, 5, 6]];
        let all = crate::caddy::ca_bundle_text(&crate::caddy::pem_certificates(&roots), "root");
        let facts = |windows, bundle| {
            system_roots(RootsFacts {
                windows,
                bundle,
                marsa_root: Some("root".to_owned()),
            })
        };
        assert_eq!(
            facts(Ok((roots.clone(), 7)), Some((all.clone(), 3))),
            Verdict::Pass(Pass::SystemRoots {
                count: 2,
                left_out: 7,
                marsa: true,
                age_days: 3,
            })
        );
        // Before Caddy makes Marsa's root, the bundle has Windows' alone.
        let before = system_roots(RootsFacts {
            windows: Ok((roots.clone(), 0)),
            bundle: Some((crate::caddy::pem_certificates(&roots), 0)),
            marsa_root: None,
        });
        assert!(
            matches!(
                before,
                Verdict::Pass(Pass::SystemRoots { marsa: false, .. })
            ),
            "{before:?}"
        );
        let one = crate::caddy::pem_certificates(&roots[..1]);
        assert_eq!(
            facts(Ok((roots.clone(), 0)), Some((one, 0))),
            Verdict::Fail(Problem::SystemRootsMissing { missing: 1 })
        );
        assert_eq!(facts(Ok((roots, 0)), None), Verdict::Skip(Skip::NoBundle));
        assert_eq!(
            facts(Ok((Vec::new(), 3)), Some((all, 0))),
            Verdict::Fail(Problem::SystemRootsEmpty)
        );
        assert_eq!(
            facts(Err("access denied".to_owned()), None),
            Verdict::Fail(Problem::SystemRootsUnreadable {
                error: "access denied".to_owned()
            })
        );
    }

    #[test]
    fn windows_ports_fail_only_for_a_holder_that_is_not_caddy() {
        let held = |port, pid, name: &str| PortHolder {
            port,
            address: "0.0.0.0".to_owned(),
            process: Some(name.to_owned()),
            pid: Some(pid),
            user: None,
            command: None,
            app: None,
        };
        let caddy = vec![held(80, 5244, "caddy.exe"), held(443, 5244, "caddy.exe")];
        assert_eq!(
            windows_ports(Some(caddy.clone()), Some(Some(5244))),
            Verdict::Pass(Pass::Ports)
        );
        let mut with_iis = caddy.clone();
        with_iis.push(held(80, 4, "System"));
        assert_eq!(
            windows_ports(Some(with_iis), Some(Some(5244))),
            Verdict::Fail(Problem::PortsHeld {
                holders: vec![held(80, 4, "System")]
            })
        );
        // Caddy stopped: its old PID means nothing, and IIS still fails.
        assert_eq!(
            windows_ports(Some(vec![held(80, 4, "System")]), Some(None)),
            Verdict::Fail(Problem::PortsHeld {
                holders: vec![held(80, 4, "System")]
            })
        );
        assert_eq!(
            windows_ports(Some(Vec::new()), Some(None)),
            Verdict::Skip(Skip::NeedsCaddy)
        );
        assert_eq!(
            windows_ports(Some(Vec::new()), None),
            Verdict::Skip(Skip::NeedsDaemon)
        );
        assert_eq!(
            windows_ports(Some(vec![held(443, 5244, "caddy.exe")]), Some(Some(5244))),
            Verdict::Fail(Problem::PortsNotRouted { ports: vec![80] })
        );
        assert_eq!(
            windows_ports(None, Some(Some(5244))),
            Verdict::Skip(Skip::NotChecked)
        );
    }

    #[test]
    fn xz_must_be_there() {
        assert_eq!(xz(true), Verdict::Pass(Pass::Xz));
        assert_eq!(xz(false), Verdict::Fail(Problem::XzMissing));
    }

    #[test]
    fn the_vc_runtime_must_be_there_and_new_enough() {
        assert_eq!(vc_runtime(None), Verdict::Fail(Problem::VcRuntimeMissing));
        let old = Verdict::Fail(Problem::VcRuntimeOld {
            found: "14.29.30133".to_owned(),
        });
        assert_eq!(vc_runtime(Some((14, 29, 30133))), old);
        let just_below = Verdict::Fail(Problem::VcRuntimeOld {
            found: "14.43.99".to_owned(),
        });
        assert_eq!(vc_runtime(Some((14, 43, 99))), just_below);
        for (found, shown) in [
            ((14, 44, 0), "14.44.0"),
            ((14, 44, 35211), "14.44.35211"),
            ((14, 52, 0), "14.52.0"),
        ] {
            let pass = Verdict::Pass(Pass::VcRuntime {
                version: shown.to_owned(),
            });
            assert_eq!(vc_runtime(Some(found)), pass);
        }
    }

    /// The hosts way on Linux.
    #[test]
    fn every_site_needs_its_line_in_the_hosts_file() {
        let set = |names: &[&str]| -> BTreeSet<String> {
            names.iter().map(|name| (*name).to_owned()).collect()
        };
        let list = |names: &[&str]| -> Vec<String> {
            names.iter().map(|name| (*name).to_owned()).collect()
        };
        let block = set(&["blog.test", "old.test"]);
        assert_eq!(
            hosts(Some(&block), Some(&list(&["blog.test"]))),
            Verdict::Pass(Pass::Hosts)
        );
        assert_eq!(hosts(Some(&block), Some(&[])), Verdict::Pass(Pass::Hosts));
        assert_eq!(
            hosts(
                Some(&block),
                Some(&list(&["shop.test", "blog.test", "api.test"]))
            ),
            Verdict::Fail(Problem::HostsMissing {
                names: list(&["api.test", "shop.test"])
            })
        );
        assert_eq!(
            hosts(None, Some(&list(&["blog.test"]))),
            Verdict::Fail(Problem::HostsBlockGone)
        );
        assert_eq!(hosts(None, None), Verdict::Fail(Problem::HostsBlockGone));
        assert_eq!(hosts(Some(&block), None), Verdict::Skip(Skip::NeedsDaemon));
    }

    /// The `marsa0` link on Linux has its own fix, and is
    /// otherwise the probe of macOS.
    #[test]
    fn the_link_gives_its_own_fix() {
        let local = [IpAddr::V4(Ipv4Addr::LOCALHOST)];
        assert_eq!(link(true, Some(&local)), Verdict::Pass(Pass::Dns));
        assert_eq!(link(true, None), Verdict::Fail(Problem::LinkNoAnswer));
        assert_eq!(link(false, None), Verdict::Skip(Skip::NeedsDaemon));
        let other = [IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))];
        assert!(matches!(
            link(true, Some(&other)),
            Verdict::Fail(Problem::DnsWrongAddress { .. })
        ));
    }
}
