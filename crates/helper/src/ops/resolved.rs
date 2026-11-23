//! Linux: names under `.test` go to the DNS server of `marsad`, and no other
//! name does (ADR 0014). The macOS twin is `resolver`.
//!
//! A root unit of Marsa's, [`UNIT_FILE`], makes a dummy link `marsa0` with
//! the address `169.254.53.53/32`. Then it tells systemd-resolved, through
//! `resolvectl`, that this link has one DNS server, `127.0.0.1:<port>`, the
//! routing domain `~test`, and no default route. So resolved sends `.test`
//! names there and nothing else: a program that holds the port while
//! `marsad` does not can answer for `.test` names only, as on macOS. The
//! unit runs again at every boot, and resolved keeps the link's settings
//! when it restarts. The address is needed: resolved ignores a link without
//! one.
//!
//! The unit gives the link its domain and takes away its default route
//! before it gives it Marsa's server: a link with a server and no routing
//! domain is a default route, so the other order would send every name to
//! the port for a moment, and for good if a later line failed. A start that
//! fails half way still deletes the link (`ExecStopPost=`). A `marsa0` that
//! is there already is taken over: only root can make a link.
//!
//! Before systemd 253 the unit also adds a local route for `127.0.0.1` on
//! `marsa0`. Those versions of resolved bind a link's DNS socket to the
//! link while it connects, unless they know the server's address as a link
//! address, so the packets to `127.0.0.1` left through the dummy link and
//! were lost: the first `.test` lookup took 21 s on Ubuntu 22.04 (measured).
//! With the route, the kernel delivers them locally: 0.01 s. From
//! 253 on, resolved makes an exception for localhost, so the unit changes
//! no routing there. `marsad` still listens on `127.0.0.1` only, and `lo`
//! keeps its own route.
//!
//! Before systemd 250, resolved can clear a new link's settings while
//! systemd-networkd looks at it (ADR 0014, finding 30). There the link is
//! made only on 249, and only when networkd cannot run: its service, its
//! socket, and `systemd-networkd-wait-online` are all inactive, and all
//! disabled or masked. Anything else, or an answer that
//! cannot be read, gives [`RESOLVED_USE_HOSTS`]: the daemon writes
//! `/etc/hosts` lines instead.
//!
//! On every version the unit sets `default-route false` and `domain ~test`
//! again after `dns`: whatever clears the link's settings at any moment,
//! the link never ends with Marsa's server and no domain, which would make
//! it a default route.
//!
//! The helper writes the unit only over a file Marsa wrote ([`marked`]),
//! starts it, and checks the link with `resolvectl status marsa0`. A failed
//! start or check takes the unit out again, so it does not run at the next
//! boot.
//!
//! An earlier build wrote a global drop-in instead ([`DROP_IN`]). A global
//! server is a default route, so resolved sent every name there too. Both
//! ops delete it if Marsa wrote it, and restart systemd-resolved without it.
//! After a failed restart the helper makes sure systemd-resolved runs again;
//! if it does not, the code is `resolved_stopped`.
//!
//! No message holds the content of a file or the output of a program.

use std::io;
use std::path::Path;
use std::time::Duration;

use crate::ops::marked::{self, Current, MARKER};
use crate::ops::program;
use crate::response::code::{
    RESOLVED_CHECK_FAILED, RESOLVED_DELETE, RESOLVED_FOREIGN, RESOLVED_NOT_RUNNING, RESOLVED_READ,
    RESOLVED_RESTART_FAILED, RESOLVED_STOPPED, RESOLVED_SYSTEMD_UNKNOWN, RESOLVED_TOOL_MISSING,
    RESOLVED_UNIT_FAILED, RESOLVED_USE_HOSTS, RESOLVED_WRITE,
};
use crate::response::{OpResult, unreadable_message};
use crate::system::{RunOutput, System};

/// Marsa's unit. `enable` links it into `multi-user.target`, so it runs at
/// every boot.
const UNIT_FILE: &str = "/etc/systemd/system/marsa-dns-link.service";
const UNIT_NAME: &str = "marsa-dns-link.service";
/// The global drop-in of earlier builds.
const DROP_IN: &str = "/etc/systemd/resolved.conf.d/marsa.conf";
/// Everyone can read it, and only root can change it.
const MODE: u32 = 0o644;
const LINK: &str = "marsa0";
/// A link-local address (RFC 3927): it adds a route on this link only.
const ADDRESS: &str = "169.254.53.53/32";
/// Where iproute2 puts `ip`. The first one there goes into the unit.
const IP: [&str; 4] = ["/usr/sbin/ip", "/usr/bin/ip", "/sbin/ip", "/bin/ip"];
const SYSTEMCTL: &str = "/usr/bin/systemctl";
const RESOLVECTL: &str = "/usr/bin/resolvectl";
const RESOLVED: &str = "systemd-resolved.service";
const IS_ACTIVE: [&str; 3] = ["is-active", "--quiet", RESOLVED];
/// `systemctl is-active` for a unit that is not active (LSB "not
/// running"). Any other failure means `systemctl` cannot tell.
const INACTIVE: i32 = 3;
const RESET_FAILED: [&str; 2] = ["reset-failed", RESOLVED];
/// `restart`, not `reload`: systemd-resolved has no reload before systemd
/// 253.
const RESTART: [&str; 2] = ["restart", RESOLVED];
const START: [&str; 2] = ["start", RESOLVED];
const DAEMON_RELOAD: [&str; 1] = ["daemon-reload"];
const ENABLE: [&str; 2] = ["enable", UNIT_NAME];
const IS_ENABLED: [&str; 3] = ["is-enabled", "--quiet", UNIT_NAME];
/// After the unit is gone: a failed state would keep it listed, and the
/// system "degraded", until the next boot.
const RESET_FAILED_UNIT: [&str; 2] = ["reset-failed", UNIT_NAME];
/// Starts the unit if it does not run, and runs a changed one again.
const RESTART_UNIT: [&str; 2] = ["restart", UNIT_NAME];
/// Stops the unit, whose stop deletes the link, and takes it out of the
/// boot.
const DISABLE: [&str; 3] = ["disable", "--now", UNIT_NAME];
const STATUS: [&str; 2] = ["status", LINK];
const VERSION: [&str; 1] = ["--version"];
/// Each prints one line per unit, whatever its exit code.
/// `systemd-networkd-wait-online.service` requires networkd: enabled, it
/// starts networkd at boot (review of #125).
const NETWORKD_ACTIVE: [&str; 4] = [
    "is-active",
    "systemd-networkd.service",
    "systemd-networkd.socket",
    "systemd-networkd-wait-online.service",
];
const NETWORKD_ENABLED: [&str; 4] = [
    "is-enabled",
    "systemd-networkd.service",
    "systemd-networkd.socket",
    "systemd-networkd-wait-online.service",
];
/// The first systemd whose resolved leaves a new link's settings alone
/// while systemd-networkd looks at it.
const FIRST_SYSTEMD: u32 = 250;
/// The first systemd whose resolved does not bind its socket to the link
/// for a server on localhost; before it, the unit adds the local route.
const NO_BIND_BUG: u32 = 253;
/// Before 250 the link is made on 249 only, the version tested;
/// older ones lack the `ip:port` server syntax or the `Protocols:` line.
const LINK_BEFORE_250: u32 = 249;
/// How many times the link is checked after the start, with a pause in
/// between: resolved may take a moment to use a new link.
const CHECKS: u32 = 10;
const CHECK_PAUSE: Duration = Duration::from_millis(500);
/// The end of a message when systemd-resolved stays down.
const STOPPED: &str = "systemd-resolved does not run now, so no name resolves until it starts";
/// The end of a message when the old process may run on.
const OLD_RUNS: &str = "systemd-resolved may use Marsa's server until it restarts (systemctl restart systemd-resolved)";

const INSTALL: &str = "resolved_install";
const REMOVE: &str = "resolved_remove";

/// `resolved_install`: names under `.test`, and only them, go to
/// `127.0.0.1:<port>`.
pub(super) fn install(port: u16, system: &impl System) -> OpResult {
    // The global drop-in first, whatever else fails: while it is there,
    // every name reaches the port.
    let dropped = match drop_old(system) {
        Ok(dropped) => dropped,
        Err((code, message)) => return OpResult::failed(INSTALL, code, message),
    };
    let nothing_else = if dropped {
        "so only Marsa's old global setting was removed"
    } else {
        "so nothing was changed"
    };
    let current = match marked::current(system, Path::new(UNIT_FILE)) {
        Current::Foreign => {
            // The path only: the content of another tool's file stays out.
            let message = format!("{UNIT_FILE} was not written by Marsa, so it was left as it is");
            return OpResult::failed(INSTALL, RESOLVED_FOREIGN, message);
        }
        Current::Unreadable(kind) => {
            return OpResult::failed(INSTALL, RESOLVED_READ, unreadable_message(UNIT_FILE, kind));
        }
        Current::Ours(bytes) => Some(bytes),
        Current::Missing => None,
    };
    let not_running = match running(system) {
        Ok(true) => None,
        Ok(false) => Some(format!("{RESOLVED} is not running")),
        Err(reason) => Some(reason),
    };
    if let Some(reason) = not_running {
        let message = format!("{reason}, {nothing_else}");
        return OpResult::failed(INSTALL, RESOLVED_NOT_RUNNING, message);
    }
    let version = match link_allowed(system) {
        Ok(version) => version,
        Err((code, reason)) => {
            // A unit of an earlier install would meet that race at every
            // boot, so it goes.
            let message = if current.is_some() {
                let cleanup = match take_out(system) {
                    Ok(()) => format!("{UNIT_FILE} of an earlier install was taken out"),
                    Err((_, why)) => why,
                };
                format!("{reason}; {cleanup}")
            } else {
                format!("{reason}, {nothing_else}")
            };
            return OpResult::failed(INSTALL, code, message);
        }
    };
    let Some(ip) = IP.into_iter().find(|path| system.exists(Path::new(path))) else {
        let message = format!(
            "no ip program (iproute2) at {}, {nothing_else}",
            IP.join(", ")
        );
        return OpResult::failed(INSTALL, RESOLVED_TOOL_MISSING, message);
    };
    let content = unit(ip, port, version < NO_BIND_BUG);
    let in_place = current.as_deref() == Some(content.as_bytes());
    // In place, working, and enabled for the next boot: nothing to do.
    if in_place
        && check(system, port).is_ok()
        && program::succeeded(&system.run(SYSTEMCTL, &IS_ENABLED))
    {
        return if dropped {
            OpResult::done(INSTALL)
        } else {
            OpResult::unchanged(INSTALL)
        };
    }
    if !in_place
        && let Err(err) = system.write_atomic(Path::new(UNIT_FILE), content.as_bytes(), MODE)
    {
        let message = format!("cannot write {UNIT_FILE}: {}", err.kind());
        return OpResult::failed(INSTALL, RESOLVED_WRITE, message);
    }
    let checked = start(system)
        .map_err(|reason| (RESOLVED_UNIT_FAILED, reason))
        .and_then(|()| {
            wait_for_link(system, port).map_err(|reason| (RESOLVED_CHECK_FAILED, reason))
        });
    let Err((code, reason)) = checked else {
        return OpResult::done(INSTALL);
    };
    // The unit goes, also one that was there before this call: a unit that
    // does not work must not run again at the next boot.
    let cleanup = match take_out(system) {
        Ok(()) => format!("{UNIT_FILE} was removed again"),
        Err((_, why)) => why,
    };
    OpResult::failed(INSTALL, code, format!("{reason}; {cleanup}"))
}

/// The systemd version if Marsa's link may be used on it, or the code and the
/// reason why not.
fn link_allowed(system: &impl System) -> Result<u32, (&'static str, String)> {
    match systemd_version(system) {
        Ok(version) if version >= FIRST_SYSTEMD => Ok(version),
        // Before 250 the race needs systemd-networkd (finding 30): without
        // it running, the link is safe there too.
        Ok(version) if version >= LINK_BEFORE_250 && !networkd_may_run(system) => Ok(version),
        Ok(version) if version < LINK_BEFORE_250 => Err((
            RESOLVED_USE_HOSTS,
            format!(
                "systemd {version} is older than {LINK_BEFORE_250}, where Marsa's link \
                 is not tested, and .test names go into /etc/hosts here instead"
            ),
        )),
        Ok(version) => Err((
            RESOLVED_USE_HOSTS,
            format!(
                "systemd {version} is older than {FIRST_SYSTEMD} and systemd-networkd \
                 may run: its resolved can drop the settings of Marsa's link, and .test \
                 names go into /etc/hosts here instead"
            ),
        )),
        Err(reason) => Err((RESOLVED_SYSTEMD_UNKNOWN, reason)),
    }
}

/// `resolved_remove`: takes out Marsa's unit and the old drop-in, each only
/// if Marsa wrote it. Both are tried; the first failure is the result.
/// Nothing of Marsa's there is `unchanged`.
pub(super) fn remove(system: &impl System) -> OpResult {
    let taken = match marked::current(system, Path::new(UNIT_FILE)) {
        Current::Missing | Current::Foreign => Ok(false),
        Current::Unreadable(kind) => Err((RESOLVED_READ, unreadable_message(UNIT_FILE, kind))),
        Current::Ours(_) => take_out(system).map(|()| true),
    };
    let dropped = drop_old(system);
    match (taken, dropped) {
        (Err((code, message)), _) | (_, Err((code, message))) => {
            OpResult::failed(REMOVE, code, message)
        }
        (Ok(false), Ok(false)) => OpResult::unchanged(REMOVE),
        (Ok(_), Ok(_)) => OpResult::done(REMOVE),
    }
}

/// The unit, for `ip` at `ip`. LF only, with a final newline. The domain
/// and the default route come before the server (see the module doc). A
/// `-` before a command lets the unit go on when it fails: the link or its
/// address may be there already, and LLMNR, mDNS, or DNS over TLS may be
/// built out. Without `dnsovertls no`, a global `DNSOverTLS=yes` would make
/// resolved speak TLS to `marsad`, which does not.
/// `ExecStopPost=` runs after a stop, and also after a start that failed.
/// `loopback_route`: before systemd 253 (see the module doc).
fn unit(ip: &str, port: u16, loopback_route: bool) -> String {
    let route = if loopback_route {
        format!("ExecStart={ip} route add local 127.0.0.1/32 dev {LINK} table local metric 1024\n")
    } else {
        String::new()
    };
    format!(
        "{MARKER}
[Unit]
Description=Marsa: send .test names to marsad
After={RESOLVED}

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=-{ip} link add {LINK} type dummy
ExecStart={ip} link set {LINK} up
ExecStart=-{ip} addr add {ADDRESS} dev {LINK}
{route}ExecStart={RESOLVECTL} default-route {LINK} false
ExecStart={RESOLVECTL} domain {LINK} ~test
ExecStart=-{RESOLVECTL} llmnr {LINK} no
ExecStart=-{RESOLVECTL} mdns {LINK} no
ExecStart=-{RESOLVECTL} dnsovertls {LINK} no
ExecStart={RESOLVECTL} dns {LINK} {server}
ExecStart={RESOLVECTL} default-route {LINK} false
ExecStart={RESOLVECTL} domain {LINK} ~test
ExecStopPost=-{ip} link del {LINK}

[Install]
WantedBy=multi-user.target
",
        server = server(port)
    )
}

/// Marsa's DNS server as systemd-resolved writes it.
fn server(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

/// Whether systemd-networkd may run, now or later: `false` only when its
/// service, its socket, and `systemd-networkd-wait-online` all print
/// `inactive`, and all print `disabled` or `masked` (review of #125). Exit code 3 alone is not enough: it also
/// covers `activating` and `failed`, and an inactive networkd that is
/// enabled starts again at boot, or through its socket. Output that cannot
/// be read counts as "may run".
fn networkd_may_run(system: &impl System) -> bool {
    let words = |args: &[&str]| -> Option<Vec<String>> {
        let output = system.run(SYSTEMCTL, args).ok()?;
        output.code?;
        let text = String::from_utf8_lossy(&output.stdout);
        let words: Vec<String> = text.lines().map(|line| line.trim().to_owned()).collect();
        (words.len() == 3).then_some(words)
    };
    let inactive = words(&NETWORKD_ACTIVE)
        .is_some_and(|states| states.iter().all(|state| state == "inactive"));
    if !inactive {
        return true;
    }
    let off = words(&NETWORKD_ENABLED).is_some_and(|states| {
        states
            .iter()
            // Not `masked-runtime`: it is gone after a reboot.
            .all(|state| matches!(state.as_str(), "disabled" | "masked"))
    });
    !off
}

/// The version of systemd, from the first line of `systemctl --version`
/// (`systemd 255 (255.4-1ubuntu8)`), or why it cannot be read.
fn systemd_version(system: &impl System) -> Result<u32, String> {
    let output = system.run(SYSTEMCTL, &VERSION);
    let version = output
        .as_ref()
        .ok()
        .filter(|output| output.code == Some(0))
        .and_then(|output| {
            let text = String::from_utf8_lossy(&output.stdout);
            let first = text.lines().next()?.to_owned();
            first
                .strip_prefix("systemd ")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        });
    version.ok_or_else(|| {
        let reason = if program::succeeded(&output) {
            "printed no version".to_owned()
        } else {
            program::failure(&output)
        };
        format!("cannot read the version of systemd: systemctl --version {reason}")
    })
}

/// Loads the unit file, adds the unit to the boot, and (re)starts it. A
/// failed state is cleared first: systemd refuses a sixth start in ten
/// seconds.
fn start(system: &impl System) -> Result<(), String> {
    let _ = system.run(SYSTEMCTL, &RESET_FAILED_UNIT);
    for args in [&DAEMON_RELOAD[..], &ENABLE, &RESTART_UNIT] {
        let output = system.run(SYSTEMCTL, args);
        if !program::succeeded(&output) {
            return Err(format!(
                "systemctl {} {}",
                args.join(" "),
                program::failure(&output)
            ));
        }
    }
    Ok(())
}

/// Stops and disables the unit (its stop deletes the link; after a failed
/// start, `ExecStopPost=` already did), deletes its
/// file, and makes systemd forget it. The file is tried even when the stop
/// failed: without it, the unit does not run at the next boot.
fn take_out(system: &impl System) -> Result<(), (&'static str, String)> {
    let disabled = system.run(SYSTEMCTL, &DISABLE);
    let removed = system.remove(Path::new(UNIT_FILE));
    let _ = system.run(SYSTEMCTL, &DAEMON_RELOAD);
    let _ = system.run(SYSTEMCTL, &RESET_FAILED_UNIT);
    if let Err(err) = removed {
        let message = format!("cannot delete {UNIT_FILE}: {}", err.kind());
        return Err((RESOLVED_DELETE, message));
    }
    if !program::succeeded(&disabled) {
        let message = format!(
            "{UNIT_FILE} was deleted, but systemctl {} {}; {LINK} may stay until the next boot",
            DISABLE.join(" "),
            program::failure(&disabled)
        );
        return Err((RESOLVED_UNIT_FAILED, message));
    }
    Ok(())
}

/// [`check`] until it passes, up to [`CHECKS`] times. The last reason if it
/// never does.
fn wait_for_link(system: &impl System, port: u16) -> Result<(), String> {
    let mut checked = check(system, port);
    for _ in 1..CHECKS {
        if checked.is_ok() {
            break;
        }
        system.pause(CHECK_PAUSE);
        checked = check(system, port);
    }
    checked
}

/// Whether resolved sends `.test` names, and only them, to Marsa's server
/// through `marsa0`, from `resolvectl status marsa0`.
fn check(system: &impl System, port: u16) -> Result<(), String> {
    let output = system.run(RESOLVECTL, &STATUS);
    let Some(link) = read_status(&output) else {
        return Err(format!(
            "resolvectl status {LINK} {}",
            program::failure(&output)
        ));
    };
    let has = |list: &[String], word: &str| list.iter().any(|item| item == word);
    if link.servers != [server(port)] {
        Err(format!("{LINK} does not have Marsa's DNS server alone"))
    } else if link.domains != ["~test"] {
        // Another domain would send its names there too; `~.` every name.
        Err(format!("{LINK} does not route .test names alone"))
    } else if !has(&link.protocols, "-DefaultRoute") {
        // Without it the link would get other names too: the very thing
        // this setup avoids.
        Err(format!("{LINK} is not marked as no default route"))
    } else if !has(&link.protocols, "-DNSOverTLS") {
        // `marsad` does not speak TLS: every `.test` lookup would fail.
        Err(format!("{LINK} would use DNS over TLS"))
    } else if !has(&link.scopes, "DNS") {
        Err(format!("systemd-resolved does not use {LINK} for DNS"))
    } else {
        Ok(())
    }
}

/// What `resolvectl status <link>` shows for one link, as lists of words.
#[derive(Debug, Default, PartialEq, Eq)]
struct LinkStatus {
    scopes: Vec<String>,
    protocols: Vec<String>,
    servers: Vec<String>,
    domains: Vec<String>,
}

/// The fields of [`LinkStatus`], and the others.
#[derive(Clone, Copy)]
enum Field {
    Scopes,
    Protocols,
    Servers,
    Domains,
    Other,
}

/// The status in the output of `resolvectl status <link>`, or `None` if the
/// program failed (also for a link that is not there). A line without a
/// key goes on the value before it, as a wrapped value does:
///
/// ```text
/// Link 3 (marsa0)
///     Current Scopes: DNS
///          Protocols: -DefaultRoute LLMNR=resolve -mDNS -DNSOverTLS
///                     DNSSEC=no/unsupported
/// Current DNS Server: 127.0.0.1:18053
///        DNS Servers: 127.0.0.1:18053
///         DNS Domain: ~test
///      Default Route: no
/// ```
fn read_status(output: &io::Result<RunOutput>) -> Option<LinkStatus> {
    let output = output
        .as_ref()
        .ok()
        .filter(|output| output.code == Some(0))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut status = LinkStatus::default();
    let mut field = Field::Other;
    for line in text.lines() {
        let words = match line.split_once(':') {
            Some((key, words)) if is_key(key.trim()) => {
                field = match key.trim() {
                    "Current Scopes" => Field::Scopes,
                    "Protocols" => Field::Protocols,
                    "DNS Servers" => Field::Servers,
                    "DNS Domain" => Field::Domains,
                    _ => Field::Other,
                };
                words
            }
            _ => line,
        };
        let list = match field {
            Field::Scopes => &mut status.scopes,
            Field::Protocols => &mut status.protocols,
            Field::Servers => &mut status.servers,
            Field::Domains => &mut status.domains,
            Field::Other => continue,
        };
        list.extend(words.split_whitespace().map(str::to_owned));
    }
    Some(status)
}

/// A key of `resolvectl status`: words that start with a capital letter.
/// So a wrapped value with a `:`, as `127.0.0.1:53` or `fd00::1`, is no key.
fn is_key(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_uppercase())
        && text.chars().all(|c| c.is_ascii_alphabetic() || c == ' ')
}

/// Deletes [`DROP_IN`] if Marsa wrote it, and restarts systemd-resolved
/// without it. `Ok(true)`: it was there and is gone.
fn drop_old(system: &impl System) -> Result<bool, (&'static str, String)> {
    match marked::current(system, Path::new(DROP_IN)) {
        Current::Missing | Current::Foreign => return Ok(false),
        Current::Unreadable(kind) => {
            return Err((RESOLVED_READ, unreadable_message(DROP_IN, kind)));
        }
        Current::Ours(_) => {}
    }
    if let Err(err) = system.remove(Path::new(DROP_IN)) {
        let message = format!("cannot delete {DROP_IN}: {}", err.kind());
        return Err((RESOLVED_DELETE, message));
    }
    match running(system) {
        Ok(true) => {}
        // A systemd-resolved that does not run reads its files when it
        // starts.
        Ok(false) => return Ok(true),
        Err(reason) => {
            let message = format!("{DROP_IN} was deleted, but {reason}; {OLD_RUNS}");
            return Err((RESOLVED_RESTART_FAILED, message));
        }
    }
    match settle(system, restart(system)) {
        Restarted::Fine => Ok(true),
        Restarted::OldMayRun(why) => {
            let message = format!("{DROP_IN} was deleted, but {why}; {OLD_RUNS}");
            Err((RESOLVED_RESTART_FAILED, message))
        }
        Restarted::Stopped(why) => {
            let message = format!("{DROP_IN} was deleted, but {why}; {STOPPED}");
            Err((RESOLVED_STOPPED, message))
        }
    }
}

/// Whether systemd-resolved runs. `Err` with the reason when `systemctl`
/// cannot tell: only exit code 3 means "not active".
fn running(system: &impl System) -> Result<bool, String> {
    let active = system.run(SYSTEMCTL, &IS_ACTIVE);
    match &active {
        Ok(RunOutput { code: Some(0), .. }) => Ok(true),
        Ok(RunOutput {
            code: Some(INACTIVE),
            ..
        }) => Ok(false),
        _ => Err(format!(
            "cannot check {RESOLVED}: systemctl {}",
            program::failure(&active)
        )),
    }
}

/// Restarts systemd-resolved, so it reads its files again. A failed state is
/// cleared first: many restarts in a few seconds hit the unit's start limit,
/// and then systemd refuses the next one (seen on Fedora).
fn restart(system: &impl System) -> Result<(), String> {
    let _ = system.run(SYSTEMCTL, &RESET_FAILED);
    let restarted = system.run(SYSTEMCTL, &RESTART);
    if program::succeeded(&restarted) {
        Ok(())
    } else {
        Err(format!(
            "systemctl restart {RESOLVED} {}",
            program::failure(&restarted)
        ))
    }
}

/// Where a restart left systemd-resolved.
enum Restarted {
    /// It runs with the files as they are now.
    Fine,
    /// The restart failed, and the old process may run on with the files as
    /// they were: it runs, or `systemctl` cannot tell. The reason is the
    /// restart's.
    OldMayRun(String),
    /// It does not run, also after one more start: on most machines no name
    /// resolves. The reason is the restart's.
    Stopped(String),
}

/// After a failed restart: whether the old process may run on, or else one
/// more start, which reads the files as they are now.
fn settle(system: &impl System, restarted: Result<(), String>) -> Restarted {
    let Err(reason) = restarted else {
        return Restarted::Fine;
    };
    if running(system) != Ok(false) {
        return Restarted::OldMayRun(reason);
    }
    let _ = system.run(SYSTEMCTL, &RESET_FAILED);
    let _ = system.run(SYSTEMCTL, &START);
    match running(system) {
        Ok(true) => Restarted::Fine,
        Ok(false) => Restarted::Stopped(reason),
        Err(_) => Restarted::OldMayRun(reason),
    }
}

#[cfg(test)]
mod tests;
