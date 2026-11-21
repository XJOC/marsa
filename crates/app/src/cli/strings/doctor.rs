//! `marsa doctor`.

use std::path::Path;

use marsa_core::doctor::{Check, Skip};
use marsa_core::types::ServiceState;

use super::*;

// `marsa doctor`.

/// A check that passed.
pub fn doctor_pass(text: &str) -> String {
    format!("✓ {text}")
}

/// A check that failed, with its fix on the next line.
pub fn doctor_fail(text: &str, fix: &str) -> String {
    format!("✗ {text}\n  Fix: {fix}")
}

/// A check that did not run, and why.
pub fn doctor_skip(check: Check, skip: Skip) -> String {
    let check = match check {
        Check::Daemon => "marsad",
        Check::Runtimes => "PHP and Caddy",
        Check::Xz => "xz",
        Check::VcRuntime => "The Microsoft Visual C++ runtime",
        Check::SystemRoots => "PHP's CA bundle",
        Check::Caddy => "Caddy",
        Check::Dns => ".test names",
        Check::Ports => "Ports 80 and 443",
        Check::Trust => "Marsa's certificate authority",
        Check::Path => "PATH",
        Check::Log => "The daemon log",
    };
    let reason = match skip {
        Skip::NotOnThisSystem => "Marsa checks this on macOS only for now.",
        Skip::NeedsDaemon => "It needs marsad, which is not running.",
        Skip::NeedsCaddy => "It needs Caddy, which is not running.",
        Skip::CaddyNotInstalled => "Caddy is not installed.",
        Skip::NoCa => {
            "It does not exist yet: Caddy makes it when it first starts. Run \"marsa setup\" to \
             make it and trust it."
        }
        Skip::NoLog => "marsad has not written it yet.",
        Skip::NoBundle => "marsad has not written it yet: it does when it starts.",
        Skip::NotChecked => "The check could not run. Run again with MARSA_LOG=debug for details.",
    };
    format!("- {check}: not checked. {reason}")
}

/// Linux: the program that unpacks MySQL's `tar.xz`.
pub const DOCTOR_XZ_OK: &str = "/usr/bin/xz is installed (MySQL needs it to unpack).";

pub const DOCTOR_XZ_MISSING: &str = "/usr/bin/xz is not installed. MySQL comes as a .tar.xz \
     archive, so Marsa cannot install it without xz.";

pub const DOCTOR_XZ_INSTALL: &str = "Install the package xz-utils (Debian, Ubuntu) or xz \
     (Fedora, Arch), then run \"marsa doctor\" again.";

/// Windows: Microsoft's runtime that the official PHP needs (ADR 0019).
pub fn doctor_vc_runtime_ok(version: &str) -> String {
    format!("The Microsoft Visual C++ runtime {version} is installed.")
}

pub const DOCTOR_VC_MISSING: &str = "Windows does not report the Microsoft Visual C++ runtime \
     (x64), which PHP needs.";

pub fn doctor_vc_old(found: &str) -> String {
    format!("The Microsoft Visual C++ runtime {found} is too old: PHP needs 14.44 or newer.")
}

pub const DOCTOR_VC_INSTALL: &str = "Install the latest Microsoft Visual C++ Redistributable \
     (x64) from https://aka.ms/vs/17/release/vc_redist.x64.exe, then run \"marsa doctor\" again.";

/// Windows: PHP's CA bundle against the roots Windows trusts (task M4-19n,
/// ADR 0020).
pub fn doctor_system_roots_ok(count: usize, left_out: usize, marsa: bool, age_days: u64) -> String {
    let written = match age_days {
        0 => "today".to_owned(),
        1 => "1 day ago".to_owned(),
        days => format!("{days} days ago"),
    };
    let marsa = if marsa {
        ", and Marsa's CA"
    } else {
        " (Marsa's CA is not made yet)"
    };
    format!(
        "PHP trusts the {count} roots Windows trusts for HTTPS{marsa}; {left_out} other roots of \
         Windows were left out (not for HTTPS, expired, or disallowed). The bundle was written \
         {written}."
    )
}

pub fn doctor_system_roots_unreadable(error: &str) -> String {
    format!(
        "Marsa cannot read the root certificates of Windows ({error}), so PHP trusts only \
         Marsa's CA: HTTPS calls from PHP to the internet fail."
    )
}

pub const DOCTOR_SYSTEM_ROOTS_EMPTY: &str = "Windows' certificate store gave no roots for HTTPS, \
     so PHP trusts only Marsa's CA: HTTPS calls from PHP to the internet fail.";

pub const DOCTOR_SYSTEM_ROOTS_FIX: &str = "Check that Windows' date and time are right (roots \
     outside their dates are left out), and that \"Trusted Root Certification Authorities\" in \
     \"Manage user certificates\" (certmgr.msc) lists roots. Then run \"marsa doctor\" again.";

pub fn doctor_system_roots_missing(missing: usize) -> String {
    let roots = if missing == 1 { "root" } else { "roots" };
    format!("{missing} {roots} that Windows trusts now are not in PHP's CA bundle yet.")
}

pub const DOCTOR_SYSTEM_ROOTS_RESTART: &str = "marsad reads Windows' roots again within an \
     hour. To do it now, run \"marsa daemon stop\", then \"marsa daemon start\".";

pub fn doctor_runtimes_ok(default: &str, version: &str) -> String {
    if default == version {
        format!("PHP and Caddy are installed, and PHP {default} is the default.")
    } else {
        format!("PHP and Caddy are installed, and PHP {default} is the default ({version}).")
    }
}

pub const DOCTOR_CADDY_OK: &str = "Caddy is running.";
pub const DOCTOR_DNS_OK: &str = ".test names resolve to 127.0.0.1.";
/// Linux on the `marsa0` link (task M4-18k3).
pub const DOCTOR_LINK_SETUP: &str = "Run \"marsa setup\": it sends .test names to Marsa through \
     systemd-resolved. If they still do not resolve, \"resolvectl status marsa0\" shows whether \
     systemd-resolved sends .test names to Marsa.";
/// Linux on the hosts way (task M4-18k3).
pub const DOCTOR_HOSTS_OK: &str = "Every site has its line in /etc/hosts.";
/// Windows (task M4-19o): the hosts file is Windows' own, and marsad adds the
/// names through the helper service with no prompt.
pub const DOCTOR_HOSTS_OK_WINDOWS: &str = "Every site has its line in the hosts file.";
pub const DOCTOR_HOSTS_SYNC_WINDOWS: &str = "marsad adds new sites through Marsa's helper service \
     within a minute. If they stay missing, run \"marsa setup\"; marsad's log says why the \
     service refused them.";
pub const DOCTOR_HOSTS_GONE_WINDOWS: &str = "Marsa's lines are not in the hosts file, so .test \
     names do not resolve: \"marsa setup\" has not run, or another program wrote the file again.";
pub const DOCTOR_HOSTS_BACK_WINDOWS: &str = "Run \"marsa setup\" to put them back.";
pub const DOCTOR_HOSTS_SETUP: &str = "Run \"marsa setup\" to add them, with one password \
     prompt. From then on \"marsa park\" and \"marsa link\" add new sites themselves; a site \
     that appears in a parked folder waits for one of these commands.";
pub const DOCTOR_HOSTS_GONE: &str = "Marsa's lines are gone from /etc/hosts, so .test names do \
     not resolve. Another program may have written the file again, as WSL and cloud-init do at \
     boot.";
pub const DOCTOR_HOSTS_BACK: &str = "Run \"marsa setup\" to put them back. If they go again \
     after each restart, stop the program that rewrites /etc/hosts (in WSL, \
     generateHosts=false in /etc/wsl.conf).";

/// The sites whose names are not in `/etc/hosts`; at most five are named.
pub fn doctor_hosts_missing(names: &[String]) -> String {
    let mut list = names.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > 5 {
        list.push_str(&format!(", and {} more", names.len() - 5));
    }
    match names.len() {
        1 => format!("{list} is not in /etc/hosts yet, so it does not open."),
        _ => format!("{list} are not in /etc/hosts yet, so they do not open."),
    }
}
pub const DOCTOR_PORTS_OK: &str = "Ports 80 and 443 reach Caddy.";
pub const DOCTOR_TRUST_OK: &str = "Marsa's certificate authority is trusted.";
pub const DOCTOR_PATH_OK: &str = "Marsa's shims come first in PATH.";

pub fn doctor_log_ok(log: &Path) -> String {
    format!("No errors in {} since marsad last started.", log.display())
}

pub const DOCTOR_START_DAEMON: &str = "Run \"marsa daemon start\".";

pub fn doctor_daemon_error(code: &str) -> String {
    format!("Cannot reach marsad. [{code}]")
}

/// The line and the fix when PHP, Caddy, or both are missing.
pub fn doctor_not_installed(php: bool, caddy: bool) -> (String, String) {
    let (missing, fix) = match (php, caddy) {
        (true, true) => (
            "PHP and Caddy are",
            "Run \"marsa install php\", then \"marsa install caddy\".",
        ),
        (true, false) => ("PHP is", "Run \"marsa install php\"."),
        _ => ("Caddy is", "Run \"marsa install caddy\"."),
    };
    (format!("{missing} not installed."), fix.to_owned())
}

pub fn doctor_runtimes_unreadable(code: &str) -> String {
    format!("Cannot list the installed versions. [{code}]")
}

pub fn doctor_config_unreadable(code: &str) -> String {
    format!("Cannot read the default PHP version. [{code}]")
}

pub const DOCTOR_NO_DEFAULT: &str =
    "No PHP version is the default, so a folder without a version of its own runs no PHP.";

pub fn doctor_default_not_installed(default: &str) -> String {
    format!("The default PHP is {default}, but no installed PHP matches it.")
}

pub fn doctor_use_php(version: &str) -> String {
    format!("Run \"marsa use php {version}\".")
}

pub fn doctor_caddy_state(state: &ServiceState) -> String {
    format!("Caddy is {}.", state_name(state))
}

pub const DOCTOR_START_CADDY: &str = "Run \"marsa services start caddy\".";

pub fn doctor_caddy_failed(code: &str) -> String {
    format!("Caddy stopped with an error. [{code}]")
}

pub const DOCTOR_DNS_NO_ANSWER: &str =
    ".test names do not resolve, so browsers cannot find Marsa's sites.";
pub const DOCTOR_DNS_SETUP: &str = "Run \"marsa setup\": it sends .test names to Marsa. If it \
     says /etc/resolver/test belongs to another tool, remove that tool's .test setting first.";

pub fn doctor_dns_wrong(addresses: &str) -> String {
    format!(".test names resolve to {addresses}, not to 127.0.0.1.")
}

pub const DOCTOR_DNS_FOREIGN: &str = "Another tool answers .test names, such as the dnsmasq of \
     Valet or Herd, or a VPN. Turn off its .test domain, then run \"marsa setup\".";
/// Above the [`port_held`] lines.
pub const DOCTOR_PORTS_HELD: &str = "Another program uses port 80 or 443:";
pub const DOCTOR_FREE_PORTS: &str = "Stop that program, or turn off its web server, then run \
     \"marsa setup\".";
/// Windows: Caddy listens on 80 and 443 itself; IIS and other HTTP.sys
/// servers show as `System` (PID 4).
pub const DOCTOR_FREE_PORTS_WINDOWS: &str = "Stop that program. For IIS, run \"Stop-Service \
     W3SVC\" in PowerShell as administrator, and set the \"World Wide Web Publishing Service\" to \
     Manual. For \"System\" (PID 4), another HTTP.sys server holds the port: \"netsh http show \
     servicestate\" names it.";
pub const DOCTOR_CADDY_PORTS_WINDOWS: &str = "Caddy runs but does not listen on port 80. Its log \
     says why: run \"marsa logs caddy\".";

pub fn doctor_ports_not_routed(ports: &[u16]) -> String {
    match ports {
        [port] => format!("Port {port} does not reach Caddy."),
        _ => "Ports 80 and 443 do not reach Caddy.".to_owned(),
    }
}

pub const DOCTOR_PORTS_SETUP: &str = "Run \"marsa setup\" to route ports 80 and 443 to Caddy. If \
     you already did, run it again: a macOS update or another tool may have removed the route.";
pub const DOCTOR_UNTRUSTED: &str =
    "Marsa's certificate authority is not trusted, so browsers warn on https://*.test sites.";
pub const DOCTOR_TRUST_SETUP: &str = "Run \"marsa setup\" in Terminal on the Mac's desktop and \
     approve the dialog macOS shows.";
pub const DOCTOR_TRUST_SETUP_WINDOWS: &str = "Run \"marsa setup\" and answer Yes to the Windows \
     prompt (UAC).";

pub fn doctor_shims_missing(shims: &Path) -> String {
    format!(
        "Marsa's shims ({}) are not in PATH, so php does not run Marsa's PHP.",
        shims.display()
    )
}

/// `windows`: the user's PATH is a setting there, not a line in a file.
pub fn doctor_add_shims(shims: &Path, windows: bool) -> String {
    if windows {
        format!(
            "Add {} to the start of your user PATH, then open a new terminal.",
            shims.display()
        )
    } else {
        format!(
            "Add this line at the end of ~/.zshrc (or your shell's startup file), then open a \
             new terminal: export PATH=\"{}:$PATH\"",
            shims.display()
        )
    }
}

pub fn doctor_php_first(php: &Path) -> String {
    format!(
        "{} comes before Marsa's shims in PATH, so php runs that PHP.",
        php.display()
    )
}

pub fn doctor_move_shims(shims: &Path, windows: bool) -> String {
    if windows {
        format!(
            "Move {} to the start of your user PATH, then open a new terminal.",
            shims.display()
        )
    } else {
        format!(
            "Move the line that adds {} to PATH to the end of ~/.zshrc (or your shell's startup \
             file), after any other line that changes PATH, then open a new terminal.",
            shims.display()
        )
    }
}

pub fn doctor_log_error(log: &Path, line: &str) -> String {
    format!(
        "The last error in {} since marsad started:\n    {line}",
        log.display()
    )
}

pub fn doctor_read_log(log: &Path) -> String {
    format!(
        "Read {} around that line. If the error repeats, report it with that file.",
        log.display()
    )
}
