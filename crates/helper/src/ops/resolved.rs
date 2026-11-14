//! Linux: names under `.test` go to the DNS server of `marsad`, and no other
//! name does (ADR 0014, spike S-02b). The macOS twin is `resolver`.
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
//! were lost: the first `.test` lookup took 21 s on Ubuntu 22.04 (spike
//! S-02c). With the route, the kernel delivers them locally: 0.01 s. From
//! 253 on, resolved makes an exception for localhost, so the unit changes
//! no routing there. `marsad` still listens on `127.0.0.1` only, and `lo`
//! keeps its own route.
//!
//! Before systemd 250, resolved can clear a new link's settings while
//! systemd-networkd looks at it (ADR 0014, finding 30). There the link is
//! made only when networkd cannot run: its service and its socket are both
//! inactive, and both disabled or masked. Anything else, or an answer that
//! cannot be read, gives [`RESOLVED_USE_HOSTS`]: the daemon writes
//! `/etc/hosts` lines instead (task M4-18k).
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
use crate::response::{OpResult, Outcome};
use crate::system::{RunOutput, System};

/// A file at [`UNIT_FILE`] that Marsa did not write, so it was left as it
/// is.
pub const RESOLVED_FOREIGN: &str = "resolved_foreign";
/// [`UNIT_FILE`] or [`DROP_IN`] cannot be read, so it was left as it is.
pub const RESOLVED_READ: &str = "resolved_read";
/// systemd-resolved does not run, or `systemctl` cannot tell. Nothing was
/// changed.
pub const RESOLVED_NOT_RUNNING: &str = "resolved_not_running";
/// There is no `ip` program (iproute2) at the usual paths. Nothing was
/// changed.
pub const RESOLVED_TOOL_MISSING: &str = "resolved_tool_missing";
/// Writing [`UNIT_FILE`] failed.
pub const RESOLVED_WRITE: &str = "resolved_write";
/// `systemctl` could not load, enable, start, or stop Marsa's unit. After
/// an install, the unit is gone again.
pub const RESOLVED_UNIT_FAILED: &str = "resolved_unit_failed";
/// `marsa0` does not send `.test` names, and only them, to Marsa's server.
/// The unit is gone again.
pub const RESOLVED_CHECK_FAILED: &str = "resolved_check_failed";
/// The restart that drops [`DROP_IN`] failed, or `systemctl` cannot tell
/// whether systemd-resolved runs. The message says that the old process
/// may still use Marsa's server.
pub const RESOLVED_RESTART_FAILED: &str = "resolved_restart_failed";
/// systemd-resolved does not run after the restart that drops [`DROP_IN`],
/// and did not start again: no name resolves until it runs.
pub const RESOLVED_STOPPED: &str = "resolved_stopped";
/// Deleting [`UNIT_FILE`] or [`DROP_IN`] failed.
pub const RESOLVED_DELETE: &str = "resolved_delete";
/// The version of systemd cannot be read, so Marsa makes no link. Nothing
/// was changed but the removal of an old drop-in (and of an earlier unit).
pub const RESOLVED_SYSTEMD_UNKNOWN: &str = "resolved_systemd_unknown";
/// systemd is older than 250 and systemd-networkd runs (or `systemctl`
/// cannot tell): that resolved counts a link networkd calls "initialized"
/// as managed and clears what was set on it (ADR 0014, finding 30), so
/// Marsa makes no link. The daemon writes `/etc/hosts` lines instead
/// (`hosts_sync`, task M4-18k). Nothing was changed but the removal of an
/// old drop-in (and of an earlier unit).
pub const RESOLVED_USE_HOSTS: &str = "resolved_use_hosts";

/// Marsa's unit. `enable` links it into `multi-user.target`, so it runs at
/// every boot.
const UNIT_FILE: &str = "/etc/systemd/system/marsa-dns-link.service";
const UNIT_NAME: &str = "marsa-dns-link.service";
/// The global drop-in of earlier builds (task M4-18a).
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
const NETWORKD_ACTIVE: [&str; 3] = [
    "is-active",
    "systemd-networkd.service",
    "systemd-networkd.socket",
];
const NETWORKD_ENABLED: [&str; 3] = [
    "is-enabled",
    "systemd-networkd.service",
    "systemd-networkd.socket",
];
/// The first systemd whose resolved leaves a new link's settings alone
/// while systemd-networkd looks at it.
const FIRST_SYSTEMD: u32 = 250;
/// The first systemd whose resolved does not bind its socket to the link
/// for a server on localhost; before it, the unit adds the local route.
const NO_BIND_BUG: u32 = 253;
/// Before 250 the link is made on 249 only, the version tested (S-02c);
/// older ones lack the `ip:port` server syntax or the `Protocols:` line.
const LINK_BEFORE_250: u32 = 249;
/// How many times the link is checked after the start, with a pause in
/// between: resolved may take a moment to use a new link (S-02b).
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
pub fn install(port: u16, system: &impl System) -> OpResult {
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
            return OpResult::failed(INSTALL, RESOLVED_READ, unreadable(UNIT_FILE, kind));
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
    let version = match systemd_version(system) {
        Ok(version) if version >= FIRST_SYSTEMD => version,
        // Before 250 the race needs systemd-networkd (finding 30): without
        // it running, the link is safe there too.
        Ok(version) if version >= LINK_BEFORE_250 && !networkd_may_run(system) => version,
        other => {
            let (code, reason) = match other {
                Ok(version) => (
                    RESOLVED_USE_HOSTS,
                    format!(
                        "systemd {version} is older than {FIRST_SYSTEMD} and systemd-networkd \
                         may run: its resolved can drop the settings of Marsa's link, and .test \
                         names go into /etc/hosts here instead"
                    ),
                ),
                Err(reason) => (RESOLVED_SYSTEMD_UNKNOWN, reason),
            };
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
        let outcome = if dropped {
            Outcome::Done
        } else {
            Outcome::Unchanged
        };
        return result(INSTALL, outcome);
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
        return result(INSTALL, Outcome::Done);
    };
    // The unit goes, also one that was there before this call: a unit that
    // does not work must not run again at the next boot.
    let cleanup = match take_out(system) {
        Ok(()) => format!("{UNIT_FILE} was removed again"),
        Err((_, why)) => why,
    };
    OpResult::failed(INSTALL, code, format!("{reason}; {cleanup}"))
}

/// `resolved_remove`: takes out Marsa's unit and the old drop-in, each only
/// if Marsa wrote it. Both are tried; the first failure is the result.
/// Nothing of Marsa's there is `unchanged`.
pub fn remove(system: &impl System) -> OpResult {
    let taken = match marked::current(system, Path::new(UNIT_FILE)) {
        Current::Missing | Current::Foreign => Ok(false),
        Current::Unreadable(kind) => Err((RESOLVED_READ, unreadable(UNIT_FILE, kind))),
        Current::Ours(_) => take_out(system).map(|()| true),
    };
    let dropped = drop_old(system);
    match (taken, dropped) {
        (Err((code, message)), _) | (_, Err((code, message))) => {
            OpResult::failed(REMOVE, code, message)
        }
        (Ok(false), Ok(false)) => result(REMOVE, Outcome::Unchanged),
        (Ok(_), Ok(_)) => result(REMOVE, Outcome::Done),
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
/// service and its socket both print `inactive`, and both print `disabled`
/// or `masked` (review of #125). Exit code 3 alone is not enough: it also
/// covers `activating` and `failed`, and an inactive networkd that is
/// enabled starts again at boot, or through its socket. Output that cannot
/// be read counts as "may run".
fn networkd_may_run(system: &impl System) -> bool {
    let words = |args: &[&str]| -> Option<Vec<String>> {
        let output = system.run(SYSTEMCTL, args).ok()?;
        output.code?;
        let text = String::from_utf8_lossy(&output.stdout);
        let words: Vec<String> = text.lines().map(|line| line.trim().to_owned()).collect();
        (words.len() == 2).then_some(words)
    };
    let inactive = words(&NETWORKD_ACTIVE)
        .is_some_and(|states| states.iter().all(|state| state == "inactive"));
    if !inactive {
        return true;
    }
    let off = words(&NETWORKD_ENABLED).is_some_and(|states| {
        states
            .iter()
            .all(|state| matches!(state.as_str(), "disabled" | "masked" | "masked-runtime"))
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
/// seconds (spike S-02c).
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
        Current::Unreadable(kind) => return Err((RESOLVED_READ, unreadable(DROP_IN, kind))),
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
/// and then systemd refuses the next one (spike S-02, Fedora).
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

/// The message for a file at `path` that cannot be read.
fn unreadable(path: &str, kind: io::ErrorKind) -> String {
    format!("cannot read {path}: {kind}, so it was left as it is")
}

fn result(op: &'static str, outcome: Outcome) -> OpResult {
    OpResult { op, outcome }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::ops::apply;
    use crate::os::Os;
    use crate::request::Request;
    use crate::system::FakeSystem;

    const ACTIVE: &str = "/usr/bin/systemctl is-active --quiet systemd-resolved.service";
    const RESET: &str = "/usr/bin/systemctl reset-failed systemd-resolved.service";
    const RESTARTED: &str = "/usr/bin/systemctl restart systemd-resolved.service";
    const STARTED: &str = "/usr/bin/systemctl start systemd-resolved.service";
    const RELOAD: &str = "/usr/bin/systemctl daemon-reload";
    const ENABLED: &str = "/usr/bin/systemctl enable marsa-dns-link.service";
    const UNIT_UP: &str = "/usr/bin/systemctl restart marsa-dns-link.service";
    const DISABLED: &str = "/usr/bin/systemctl disable --now marsa-dns-link.service";
    const IS_ENABLED_CMD: &str = "/usr/bin/systemctl is-enabled --quiet marsa-dns-link.service";
    const RESET_UNIT: &str = "/usr/bin/systemctl reset-failed marsa-dns-link.service";
    const LOOKED: &str = "/usr/bin/resolvectl status marsa0";
    const VERSIONED: &str = "/usr/bin/systemctl --version";
    const NETWORKD: &str =
        "/usr/bin/systemctl is-active systemd-networkd.service systemd-networkd.socket";
    const NETWORKD_ENABLED_CMD: &str =
        "/usr/bin/systemctl is-enabled systemd-networkd.service systemd-networkd.socket";
    /// The mode of files that were there before a test, so a rewrite shows.
    const OLD_MODE: u32 = 0o600;
    const FOREIGN: &[u8] = b"[Unit]\nDescription=someone else's\n";
    const FOREIGN_DROP_IN: &[u8] = b"[Resolve]\nDNS=9.9.9.9\n";

    /// The global drop-in of task M4-18a, as it wrote it.
    fn drop_in() -> Vec<u8> {
        format!("{MARKER}\n[Resolve]\nDNS=127.0.0.1:18053\nDomains=~test\n").into_bytes()
    }

    /// Marsa's unit for `port`, with `ip` at its first path.
    fn ours(port: u16) -> Vec<u8> {
        unit("/usr/sbin/ip", port, false).into_bytes()
    }

    /// `marsa0` while it is there, as the lines of the unit left it.
    #[derive(Clone, Debug, Default)]
    struct Link {
        address: bool,
        /// Set by `default-route`; `None` is resolved's automatic mode.
        default_route: Option<bool>,
        domains: Vec<String>,
        server: Option<String>,
        /// Set by `dnsovertls no`.
        no_dot: bool,
        /// The local route for `127.0.0.1` on the link.
        loopback_route: bool,
    }

    impl Link {
        /// The default route in effect: in the automatic mode, a link is one
        /// unless it has a routing-only domain (`~x`), and `~.` makes it one.
        fn default_route(&self) -> bool {
            let only = self.domains.iter().any(|domain| domain.starts_with('~'));
            let all = self.domains.iter().any(|domain| domain == "~.");
            self.default_route.unwrap_or(!only || all)
        }

        /// Whether names outside `.test` could reach the server now.
        fn exposed(&self) -> bool {
            self.server.is_some() && (self.default_route() || self.domains != ["~test"])
        }
    }

    /// A Linux with systemd-resolved and `ip`, whose systemd runs Marsa's
    /// unit as written. Files and the log are in `fake`.
    struct Linux {
        fake: FakeSystem,
        active: Cell<bool>,
        /// `systemctl is-active` exits with this instead, as on a bus error.
        active_code: Option<i32>,
        no_systemctl: bool,
        /// That many restarts of systemd-resolved fail.
        failing_restarts: Cell<u32>,
        /// A failed restart leaves the old process running.
        old_survives: bool,
        start_fails: bool,
        /// systemd-resolved read the drop-in at its last start.
        drop_in_loaded: Cell<bool>,
        /// The unit as systemd read it at the last `daemon-reload`.
        loaded: RefCell<Option<Vec<u8>>>,
        enabled: Cell<bool>,
        /// The unit's start worked, and it has not stopped since.
        unit_active: Cell<bool>,
        link: RefCell<Option<Link>>,
        /// At any moment, `marsa0` had a server for more than `.test`.
        exposed: Cell<bool>,
        /// This `systemctl` command line fails.
        failing: Option<&'static str>,
        /// The unit's command that has this text fails.
        failing_line: Option<&'static str>,
        /// resolved does not use the link, whatever the unit did.
        unused: bool,
        /// `status` says "Current Scopes: none" that many more times first.
        none_for: Cell<u32>,
        resolvectl_fails: bool,
        /// `status` as systemd 262 prints it (Fedora 45).
        fedora: bool,
        /// What `systemctl --version` says on its first line.
        version_line: &'static str,
        /// What `systemctl is-active` and `is-enabled` print for networkd's
        /// service and socket.
        networkd_active: &'static str,
        networkd_enabled: &'static str,
        /// After the unit's line with this text, resolved clears the link's
        /// domain and default route, as in the race of finding 30.
        wipe_after: Cell<Option<&'static str>>,
        /// A global `DNSOverTLS=yes`, which a link inherits.
        strict_dot: bool,
    }

    impl Linux {
        fn new() -> Self {
            let linux = Self {
                fake: FakeSystem::default(),
                active: Cell::new(true),
                active_code: None,
                no_systemctl: false,
                failing_restarts: Cell::new(0),
                old_survives: false,
                start_fails: false,
                drop_in_loaded: Cell::new(false),
                loaded: RefCell::new(None),
                enabled: Cell::new(false),
                unit_active: Cell::new(false),
                link: RefCell::new(None),
                exposed: Cell::new(false),
                failing: None,
                failing_line: None,
                unused: false,
                none_for: Cell::new(0),
                resolvectl_fails: false,
                fedora: false,
                version_line: "systemd 255 (255.4-1ubuntu8)",
                networkd_active: "active\nactive\n",
                networkd_enabled: "enabled\nenabled\n",
                wipe_after: Cell::new(None),
                strict_dot: false,
            };
            linux.put("/usr/sbin/ip", b"");
            linux
        }

        /// With `bytes` as the unit, loaded, enabled, and started.
        fn with_unit(bytes: &[u8]) -> Self {
            let linux = Self::new();
            linux.put(UNIT_FILE, bytes);
            *linux.loaded.borrow_mut() = Some(bytes.to_vec());
            linux.enabled.set(true);
            linux.start_unit();
            linux
        }

        /// With the old drop-in, loaded.
        fn with_drop_in(self) -> Self {
            self.put(DROP_IN, &drop_in());
            self.drop_in_loaded.set(true);
            self
        }

        fn put(&self, path: &str, bytes: &[u8]) {
            let file = (bytes.to_vec(), OLD_MODE);
            self.fake
                .files
                .borrow_mut()
                .insert(PathBuf::from(path), file);
        }

        fn file(&self, path: &str) -> Option<(Vec<u8>, u32)> {
            self.fake.files.borrow().get(Path::new(path)).cloned()
        }

        fn unit_file(&self) -> Option<(Vec<u8>, u32)> {
            self.file(UNIT_FILE)
        }

        /// Every command that ran, as `program arguments`.
        fn commands(&self) -> Vec<String> {
            let log = self.fake.log.borrow();
            log.iter()
                .map(|(program, args)| format!("{program} {}", args.join(" ")))
                .collect()
        }

        /// The server of `marsa0`, while it is there.
        fn server(&self) -> Option<String> {
            self.link
                .borrow()
                .as_ref()
                .and_then(|link| link.server.clone())
        }

        /// The lines of the loaded unit that start with `key=`, without the
        /// `-` that lets one fail: `(may_fail, command)`.
        fn lines(&self, key: &str) -> Vec<(bool, String)> {
            let loaded = self.loaded.borrow();
            let text = String::from_utf8_lossy(loaded.as_deref().unwrap_or_default());
            text.lines()
                .filter_map(|line| line.strip_prefix(key)?.strip_prefix('='))
                .map(|command| match command.strip_prefix('-') {
                    Some(command) => (true, command.to_owned()),
                    None => (false, command.to_owned()),
                })
                .collect()
        }

        /// systemd starts the loaded unit: its `ExecStart=` lines in order.
        /// A line that fails without a `-` fails the start, and then the
        /// `ExecStopPost=` lines run.
        fn start_unit(&self) -> bool {
            for (may_fail, command) in self.lines("ExecStart") {
                let failed = self.failing_line.is_some_and(|part| command.contains(part))
                    || !self.apply(&command);
                if failed && !may_fail {
                    self.stop_unit(false);
                    return false;
                }
            }
            self.unit_active.set(true);
            true
        }

        /// systemd stops the unit: `ExecStop=` only after a start that
        /// worked, `ExecStopPost=` always.
        fn stop_unit(&self, active: bool) {
            let mut commands = if active {
                self.lines("ExecStop")
            } else {
                Vec::new()
            };
            commands.extend(self.lines("ExecStopPost"));
            for (_, command) in commands {
                self.apply(&command);
            }
            self.unit_active.set(false);
        }

        /// One command of the unit, on the link. `false`: it failed, as
        /// with no link to work on.
        fn apply(&self, command: &str) -> bool {
            let words: Vec<&str> = command.split_whitespace().collect();
            let mut link = self.link.borrow_mut();
            if let ["link", "add", "marsa0", "type", "dummy"] = &words[1..] {
                let made = link.is_none();
                link.get_or_insert_with(Link::default);
                return made;
            }
            let Some(there) = link.as_mut() else {
                return false;
            };
            match &words[1..] {
                ["link", "del", "marsa0"] => *link = None,
                ["link", "set", "marsa0", "up"] | ["llmnr" | "mdns", "marsa0", "no"] => {}
                ["addr", "add", "169.254.53.53/32", "dev", "marsa0"] => there.address = true,
                [
                    "route",
                    "add",
                    "local",
                    "127.0.0.1/32",
                    "dev",
                    "marsa0",
                    "table",
                    "local",
                    "metric",
                    "1024",
                ] => {
                    there.loopback_route = true;
                }
                ["default-route", "marsa0", value] => there.default_route = Some(*value == "true"),
                ["dnsovertls", "marsa0", "no"] => there.no_dot = true,
                ["domain", "marsa0", domains @ ..] => {
                    there.domains = domains.iter().map(|word| (*word).to_owned()).collect();
                }
                ["dns", "marsa0", server] => there.server = Some((*server).to_owned()),
                _ => panic!("the fake does not know: {command}"),
            }
            if self
                .wipe_after
                .get()
                .is_some_and(|part| command.contains(part))
            {
                self.wipe_after.set(None);
                if let Some(there) = link.as_mut() {
                    there.domains.clear();
                    there.default_route = None;
                }
            }
            if link.as_ref().is_some_and(Link::exposed) {
                self.exposed.set(true);
            }
            true
        }

        /// A (re)start of systemd-resolved: it reads the drop-in if it is
        /// there.
        fn start_resolved(&self) {
            self.active.set(true);
            self.drop_in_loaded.set(self.file(DROP_IN).is_some());
        }

        fn systemctl(&self, line: &str) -> io::Result<RunOutput> {
            if self.no_systemctl {
                return Err(io::ErrorKind::NotFound.into());
            }
            if self.failing == Some(line) {
                return exit(1, "");
            }
            match line {
                "--version" => exit(0, &format!("{}\n+PAM +AUDIT +SELINUX\n", self.version_line)),
                "is-active systemd-networkd.service systemd-networkd.socket" => {
                    let code = if self.networkd_active.starts_with("active") {
                        0
                    } else {
                        3
                    };
                    exit(code, self.networkd_active)
                }
                "is-enabled systemd-networkd.service systemd-networkd.socket" => {
                    let code = if self.networkd_enabled.contains("enabled") {
                        0
                    } else {
                        1
                    };
                    exit(code, self.networkd_enabled)
                }
                "is-active --quiet systemd-resolved.service" => {
                    let code = if self.active.get() { 0 } else { 3 };
                    exit(self.active_code.unwrap_or(code), "")
                }
                "reset-failed systemd-resolved.service" => exit(0, ""),
                "restart systemd-resolved.service" => {
                    if self.failing_restarts.get() > 0 {
                        self.failing_restarts.set(self.failing_restarts.get() - 1);
                        self.active.set(self.old_survives);
                        return exit(1, "");
                    }
                    self.start_resolved();
                    exit(0, "")
                }
                "start systemd-resolved.service" => {
                    if self.start_fails {
                        return exit(1, "");
                    }
                    self.start_resolved();
                    exit(0, "")
                }
                "daemon-reload" => {
                    *self.loaded.borrow_mut() = self.unit_file().map(|(bytes, _)| bytes);
                    exit(0, "")
                }
                "is-enabled --quiet marsa-dns-link.service" => {
                    exit(if self.enabled.get() { 0 } else { 1 }, "")
                }
                "reset-failed marsa-dns-link.service" => exit(0, ""),
                "enable marsa-dns-link.service" => {
                    if self.loaded.borrow().is_none() {
                        return exit(1, "");
                    }
                    self.enabled.set(true);
                    exit(0, "")
                }
                "restart marsa-dns-link.service" => {
                    if self.loaded.borrow().is_none() {
                        return exit(5, "");
                    }
                    if self.unit_active.get() {
                        self.stop_unit(true);
                    }
                    exit(if self.start_unit() { 0 } else { 1 }, "")
                }
                // A unit whose start failed is not active: nothing runs.
                "disable --now marsa-dns-link.service" => {
                    if self.unit_active.get() {
                        self.stop_unit(true);
                    }
                    self.enabled.set(false);
                    exit(0, "")
                }
                _ => panic!("unexpected systemctl {line}"),
            }
        }

        /// What `resolvectl status marsa0` prints now.
        fn status(&self) -> io::Result<RunOutput> {
            let link = self.link.borrow().clone();
            let Some(link) = link.filter(|_| !self.resolvectl_fails && self.active.get()) else {
                return exit(1, "");
            };
            let mut used = link.address && link.server.is_some() && !self.unused;
            if self.none_for.get() > 0 {
                self.none_for.set(self.none_for.get() - 1);
                used = false;
            }
            let scopes = if used { "DNS" } else { "none" };
            let route = if link.default_route() { '+' } else { '-' };
            let dot = if self.strict_dot && !link.no_dot {
                '+'
            } else {
                '-'
            };
            let domains = link.domains.join(" ");
            let mut text = if self.fedora {
                format!(
                    "Link 3 (marsa0)\n    Current Scopes: {scopes}\n         Protocols: \
                     {route}DefaultRoute LLMNR=resolve -mDNS {dot}DNSOverTLS\n                    \
                     DNSSEC=no/unsupported\n"
                )
            } else {
                format!(
                    "Link 5 (marsa0)\nCurrent Scopes: {scopes}\n     Protocols: \
                     {route}DefaultRoute +LLMNR -mDNS {dot}DNSOverTLS DNSSEC=no/unsupported\n"
                )
            };
            if let Some(server) = &link.server {
                if self.fedora {
                    text.push_str(&format!("Current DNS Server: {server}\n"));
                }
                text.push_str(&format!("       DNS Servers: {server}\n"));
            }
            if !domains.is_empty() {
                text.push_str(&format!("        DNS Domain: {domains}\n"));
            }
            exit(0, &text)
        }
    }

    fn exit(code: i32, stdout: &str) -> io::Result<RunOutput> {
        Ok(RunOutput {
            code: Some(code),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }

    impl System for Linux {
        fn read(&self, path: &Path, max: u64) -> io::Result<Vec<u8>> {
            self.fake.read(path, max)
        }

        fn exists(&self, path: &Path) -> bool {
            self.fake.exists(path)
        }

        fn is_link(&self, path: &Path) -> bool {
            self.fake.is_link(path)
        }

        fn write_atomic(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
            self.fake.write_atomic(path, bytes, mode)
        }

        fn remove(&self, path: &Path) -> io::Result<()> {
            self.fake.remove(path)
        }

        fn remove_empty_dir(&self, path: &Path) -> io::Result<()> {
            self.fake.remove_empty_dir(path)
        }

        fn run(&self, program: &'static str, args: &[&str]) -> io::Result<RunOutput> {
            let strings = args.iter().map(|arg| (*arg).to_owned()).collect();
            self.fake
                .log
                .borrow_mut()
                .push((program.to_owned(), strings));
            match program {
                "/usr/bin/systemctl" => self.systemctl(&args.join(" ")),
                "/usr/bin/resolvectl" if args == ["status", "marsa0"] => self.status(),
                _ => panic!("unexpected command: {program} {args:?}"),
            }
        }

        fn current_exe(&self) -> io::Result<PathBuf> {
            self.fake.current_exe()
        }
    }

    fn code_of(result: &OpResult) -> &'static str {
        match &result.outcome {
            Outcome::Failed { code, .. } => code,
            other => panic!("not a failure: {other:?}"),
        }
    }

    fn message_of(result: &OpResult) -> &str {
        match &result.outcome {
            Outcome::Failed { message, .. } => message,
            other => panic!("not a failure: {other:?}"),
        }
    }

    // --- install ---------------------------------------------------------

    #[test]
    fn install_writes_starts_and_checks_the_unit() {
        let linux = Linux::new();
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        assert_eq!(linux.unit_file(), Some((ours(18053), 0o644)));
        assert_eq!(
            linux.commands(),
            [
                ACTIVE, VERSIONED, RESET_UNIT, RELOAD, ENABLED, UNIT_UP, LOOKED
            ]
        );
        assert!(linux.enabled.get());
        assert_eq!(linux.server().as_deref(), Some("127.0.0.1:18053"));
    }

    #[test]
    fn the_written_unit() {
        let linux = Linux::new();
        install(18053, &linux);
        let (bytes, mode) = linux.unit_file().unwrap();
        assert_eq!(mode, 0o644);
        assert!(bytes.ends_with(b"multi-user.target\n") && !bytes.contains(&b'\r'));
        let text = String::from_utf8(bytes).unwrap();
        insta::assert_snapshot!(text);
    }

    /// systemd 249 (Ubuntu 22.04) and an unreadable version: no link, and
    /// the old drop-in still goes.
    #[test]
    fn a_systemd_before_250_gets_no_link() {
        let mut old = Linux::new().with_drop_in();
        old.version_line = "systemd 249 (249.11-0ubuntu3.12)";
        let failed = install(18053, &old);
        assert_eq!(code_of(&failed), RESOLVED_USE_HOSTS);
        assert_eq!(
            message_of(&failed),
            "systemd 249 is older than 250 and systemd-networkd may run: its resolved can drop \
             the settings of Marsa's link, and .test names go into /etc/hosts here instead, so \
             only Marsa's old global setting was removed"
        );
        assert_eq!((old.unit_file(), old.file(DROP_IN)), (None, None));
        assert_eq!(
            old.commands(),
            [ACTIVE, RESET, RESTARTED, ACTIVE, VERSIONED, NETWORKD]
        );
        // `systemctl` cannot tell whether networkd runs: the same.
        let mut unsure = Linux::new();
        unsure.version_line = "systemd 249 (249.11-0ubuntu3.12)";
        unsure.networkd_active = "";
        assert_eq!(code_of(&install(18053, &unsure)), RESOLVED_USE_HOSTS);
        assert_eq!(unsure.unit_file(), None);
        let mut unknown = Linux::new();
        unknown.version_line = "something else";
        let failed = install(18053, &unknown);
        assert_eq!(code_of(&failed), RESOLVED_SYSTEMD_UNKNOWN);
        assert_eq!(
            message_of(&failed),
            "cannot read the version of systemd: systemctl --version printed no version, so \
             nothing was changed"
        );
        assert_eq!(unknown.unit_file(), None);
    }

    /// A unit of an earlier install on a system before 250 goes: it would
    /// meet the race of resolved 249 at every boot.
    #[test]
    fn a_systemd_before_250_takes_out_an_earlier_unit() {
        let mut linux = Linux::with_unit(&ours(18053));
        linux.version_line = "systemd 249 (249.11-0ubuntu3.12)";
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_USE_HOSTS);
        assert_eq!(
            message_of(&failed),
            format!(
                "systemd 249 is older than 250 and systemd-networkd may run: its resolved can \
                 drop the settings of Marsa's link, and .test names go into /etc/hosts here \
                 instead; {UNIT_FILE} of an earlier install was taken out"
            )
        );
        assert_eq!(linux.unit_file(), None);
        assert!(linux.link.borrow().is_none() && !linux.enabled.get());
        assert_eq!(
            linux.commands(),
            [ACTIVE, VERSIONED, NETWORKD, DISABLED, RELOAD, RESET_UNIT]
        );
    }

    /// Before 250 without systemd-networkd (an Ubuntu 22.04 desktop with
    /// NetworkManager) the race cannot happen: the unit, with the route.
    #[test]
    fn a_systemd_before_250_without_networkd_gets_the_link() {
        let mut linux = Linux::new();
        linux.version_line = "systemd 249 (249.11-0ubuntu3.12)";
        linux.networkd_active = "inactive\ninactive\n";
        linux.networkd_enabled = "disabled\nmasked\n";
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        let (bytes, _) = linux.unit_file().unwrap();
        assert_eq!(bytes, unit("/usr/sbin/ip", 18053, true).into_bytes());
        assert_eq!(
            linux.commands(),
            [
                ACTIVE,
                VERSIONED,
                NETWORKD,
                NETWORKD_ENABLED_CMD,
                RESET_UNIT,
                RELOAD,
                ENABLED,
                UNIT_UP,
                LOOKED
            ]
        );
        // Before 249 (untested there) the hosts path, whatever networkd does.
        let mut older = Linux::new();
        older.version_line = "systemd 247 (247.3-7+deb11u5)";
        older.networkd_active = "inactive\ninactive\n";
        older.networkd_enabled = "disabled\ndisabled\n";
        assert_eq!(code_of(&install(18053, &older)), RESOLVED_USE_HOSTS);
        assert_eq!(older.unit_file(), None);
    }

    /// Before 250, networkd that is not running now but may run later
    /// (enabled, its socket enabled, failed, or activating) gets the hosts
    /// path: the race would come back at the next boot (review of #125).
    #[test]
    fn a_systemd_before_250_with_networkd_that_may_run_gets_hosts() {
        for (active, enabled) in [
            ("inactive\ninactive\n", "enabled\ndisabled\n"),
            ("inactive\ninactive\n", "disabled\nenabled\n"),
            ("inactive\ninactive\n", "static\ndisabled\n"),
            ("failed\ninactive\n", "disabled\ndisabled\n"),
            ("activating\ninactive\n", "disabled\ndisabled\n"),
            ("inactive\n", "disabled\ndisabled\n"),
            ("inactive\ninactive\n", ""),
        ] {
            let mut linux = Linux::new();
            linux.version_line = "systemd 249 (249.11-0ubuntu3.12)";
            linux.networkd_active = active;
            linux.networkd_enabled = enabled;
            let failed = install(18053, &linux);
            assert_eq!(
                code_of(&failed),
                RESOLVED_USE_HOSTS,
                "{active:?} {enabled:?}"
            );
            assert_eq!(linux.unit_file(), None);
        }
    }

    /// Whatever clears the link's domain and default route after a line of
    /// the unit (finding 30), the lines after `dns` set them again: the link
    /// never ends with Marsa's server and no domain.
    #[test]
    fn the_unit_sets_the_domain_again_after_the_server() {
        for line in [
            "default-route marsa0 false",
            "domain marsa0 ~test",
            "dnsovertls marsa0",
        ] {
            let linux = Linux::new();
            linux.wipe_after.set(Some(line));
            assert_eq!(
                install(18053, &linux),
                result(INSTALL, Outcome::Done),
                "{line}"
            );
            let link = linux.link.borrow().clone().unwrap();
            assert!(!link.exposed(), "{line}");
            assert_eq!(link.domains, ["~test"], "{line}");
        }
        // Without the lines after `dns`, the same wipe leaves a default route.
        let base = String::from_utf8(ours(18053)).unwrap();
        let tail = "ExecStart=/usr/bin/resolvectl default-route marsa0 false\n\
                    ExecStart=/usr/bin/resolvectl domain marsa0 ~test\n\
                    ExecStopPost";
        let short = base.replace(tail, "ExecStopPost");
        assert_ne!(short, base);
        let linux = Linux::new();
        linux.put(UNIT_FILE, short.as_bytes());
        *linux.loaded.borrow_mut() = Some(short.into_bytes());
        linux.wipe_after.set(Some("domain marsa0 ~test"));
        assert!(linux.start_unit());
        assert!(linux.link.borrow().as_ref().unwrap().exposed());
    }

    /// The unit for systemd 250 to 252, with the local route.
    #[test]
    fn the_written_unit_before_253() {
        let mut linux = Linux::new();
        linux.version_line = "systemd 252 (252.36-1~deb12u1)";
        install(18053, &linux);
        let (bytes, _) = linux.unit_file().unwrap();
        let text = String::from_utf8(bytes).unwrap();
        insta::assert_snapshot!(text);
    }

    /// Before 253 the unit adds the local route for `127.0.0.1`; from 253
    /// on it changes no routing (see the module doc).
    #[test]
    fn the_loopback_route_comes_only_before_253() {
        for (line, route) in [
            ("systemd 250 (250.3)", true),
            ("systemd 252 (252.36-1~deb12u1)", true),
            ("systemd 253 (253.1)", false),
            ("systemd 262 (262-1.fc45)", false),
        ] {
            let mut linux = Linux::new();
            linux.version_line = line;
            assert_eq!(
                install(18053, &linux),
                result(INSTALL, Outcome::Done),
                "{line}"
            );
            let (bytes, _) = linux.unit_file().unwrap();
            let text = String::from_utf8(bytes).unwrap();
            assert_eq!(
                text.contains("route add local 127.0.0.1/32 dev marsa0"),
                route,
                "{line}"
            );
            assert_eq!(
                linux.link.borrow().as_ref().unwrap().loopback_route,
                route,
                "{line}"
            );
        }
    }

    /// The first `ip` that is there goes into the unit.
    #[test]
    fn the_unit_uses_the_ip_that_is_there() {
        let linux = Linux::new();
        linux
            .fake
            .files
            .borrow_mut()
            .remove(Path::new("/usr/sbin/ip"));
        linux.put("/bin/ip", b"");
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        let (bytes, _) = linux.unit_file().unwrap();
        assert_eq!(bytes, unit("/bin/ip", 18053, false).into_bytes());
    }

    #[test]
    fn without_ip_nothing_changes() {
        let linux = Linux::new();
        linux
            .fake
            .files
            .borrow_mut()
            .remove(Path::new("/usr/sbin/ip"));
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_TOOL_MISSING);
        assert_eq!(
            message_of(&failed),
            "no ip program (iproute2) at /usr/sbin/ip, /usr/bin/ip, /sbin/ip, /bin/ip, so \
             nothing was changed"
        );
        assert_eq!(linux.unit_file(), None);
        assert_eq!(linux.commands(), [ACTIVE, VERSIONED]);
    }

    /// The same unit, running and checked: one look, nothing written.
    #[test]
    fn install_over_our_working_unit_is_unchanged() {
        let mut linux = Linux::with_unit(&ours(18053));
        // Any write fails here, so `unchanged` also shows that none was tried.
        linux.fake.fail_writes.insert(PathBuf::from(UNIT_FILE));
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Unchanged));
        assert_eq!(linux.unit_file(), Some((ours(18053), OLD_MODE)));
        assert_eq!(
            linux.commands(),
            [ACTIVE, VERSIONED, LOOKED, IS_ENABLED_CMD]
        );
    }

    /// The same unit, but the link is gone (deleted by hand): started again,
    /// not rewritten.
    #[test]
    fn install_over_our_unit_without_its_link_starts_it_again() {
        let linux = Linux::with_unit(&ours(18053));
        *linux.link.borrow_mut() = None;
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        assert_eq!(linux.unit_file(), Some((ours(18053), OLD_MODE)));
        assert_eq!(
            linux.commands(),
            [
                ACTIVE, VERSIONED, LOOKED, RESET_UNIT, RELOAD, ENABLED, UNIT_UP, LOOKED
            ]
        );
    }

    /// The unit works but was disabled by hand: it would not run at the next
    /// boot, so it is enabled and started again.
    #[test]
    fn install_over_our_disabled_unit_enables_it() {
        let linux = Linux::with_unit(&ours(18053));
        linux.enabled.set(false);
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        assert!(linux.enabled.get());
        assert_eq!(
            linux.commands(),
            [
                ACTIVE,
                VERSIONED,
                LOOKED,
                IS_ENABLED_CMD,
                RESET_UNIT,
                RELOAD,
                ENABLED,
                UNIT_UP,
                LOOKED
            ]
        );
    }

    /// A global `DNSOverTLS=yes`: the unit turns it off on its link, as
    /// `marsad` does not speak TLS; without that line the check fails.
    #[test]
    fn a_global_dns_over_tls_is_turned_off_on_the_link() {
        let mut linux = Linux::new();
        linux.strict_dot = true;
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        let base = String::from_utf8(ours(18053)).unwrap();
        let without = base.replace("ExecStart=-/usr/bin/resolvectl dnsovertls marsa0 no\n", "");
        assert_ne!(without, base);
        let mut linux = Linux::with_unit(without.as_bytes());
        linux.strict_dot = true;
        assert_eq!(
            check(&linux, 18053),
            Err("marsa0 would use DNS over TLS".to_owned())
        );
    }

    /// An older unit of Marsa's, with another port: rewritten, and checked
    /// only after the start.
    #[test]
    fn install_rewrites_our_unit_with_another_port() {
        let linux = Linux::with_unit(&ours(18054));
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        assert_eq!(linux.unit_file(), Some((ours(18053), 0o644)));
        assert_eq!(linux.server().as_deref(), Some("127.0.0.1:18053"));
        assert_eq!(
            linux.commands(),
            [
                ACTIVE, VERSIONED, RESET_UNIT, RELOAD, ENABLED, UNIT_UP, LOOKED
            ]
        );
    }

    /// A foreign file, and a link to Marsa's very unit: both left alone,
    /// with nothing run.
    #[test]
    fn install_fails_on_a_foreign_unit_and_keeps_it() {
        let mut link = Linux::with_unit(&ours(18053));
        link.fake.links.insert(PathBuf::from(UNIT_FILE));
        for (linux, bytes) in [
            (Linux::with_unit(FOREIGN), FOREIGN.to_vec()),
            (link, ours(18053)),
        ] {
            let failed = install(18053, &linux);
            assert_eq!(code_of(&failed), RESOLVED_FOREIGN);
            assert_eq!(
                message_of(&failed),
                format!("{UNIT_FILE} was not written by Marsa, so it was left as it is")
            );
            assert_eq!(linux.unit_file(), Some((bytes, OLD_MODE)));
            assert!(linux.commands().is_empty());
        }
    }

    /// A foreign unit stops the install, but the old drop-in goes first.
    #[test]
    fn a_foreign_unit_still_lets_the_old_drop_in_go() {
        let linux = Linux::with_unit(FOREIGN).with_drop_in();
        assert_eq!(code_of(&install(18053, &linux)), RESOLVED_FOREIGN);
        assert_eq!(linux.file(DROP_IN), None);
        assert_eq!(linux.unit_file(), Some((FOREIGN.to_vec(), OLD_MODE)));
        assert_eq!(linux.commands(), [ACTIVE, RESET, RESTARTED]);
    }

    #[test]
    fn a_unit_that_cannot_be_read_is_resolved_read_and_kept() {
        let mut linux = Linux::with_unit(&ours(18053));
        linux.fake.fail_reads.insert(PathBuf::from(UNIT_FILE));
        let message =
            format!("cannot read {UNIT_FILE}: permission denied, so it was left as it is");
        for failed in [install(18053, &linux), remove(&linux)] {
            assert_eq!(code_of(&failed), RESOLVED_READ);
            assert_eq!(message_of(&failed), message);
        }
        assert_eq!(linux.unit_file(), Some((ours(18053), OLD_MODE)));
        assert!(linux.server().is_some());
    }

    /// Stopped (exit 3), `systemctl` missing, or `systemctl` that cannot
    /// tell (exit 1): nothing is written.
    #[test]
    fn without_a_running_resolved_nothing_changes() {
        let stopped = Linux::new();
        stopped.active.set(false);
        let mut missing = Linux::new();
        missing.no_systemctl = true;
        let mut unknown = Linux::new();
        unknown.active_code = Some(1);
        let cases = [
            (stopped, "systemd-resolved.service is not running"),
            (
                missing,
                "cannot check systemd-resolved.service: systemctl could not run: entity not found",
            ),
            (
                unknown,
                "cannot check systemd-resolved.service: systemctl exited with code 1",
            ),
        ];
        for (linux, reason) in cases {
            let failed = install(18053, &linux);
            assert_eq!(code_of(&failed), RESOLVED_NOT_RUNNING);
            assert_eq!(
                message_of(&failed),
                format!("{reason}, so nothing was changed")
            );
            assert_eq!(linux.unit_file(), None);
            assert_eq!(linux.commands(), [ACTIVE]);
        }
    }

    #[test]
    fn a_failed_write_is_resolved_write_and_nothing_starts() {
        let mut linux = Linux::new();
        linux.fake.fail_writes.insert(PathBuf::from(UNIT_FILE));
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_WRITE);
        assert_eq!(
            message_of(&failed),
            format!("cannot write {UNIT_FILE}: permission denied")
        );
        assert_eq!(linux.commands(), [ACTIVE, VERSIONED]);
    }

    /// Each `systemctl` step of the start can fail: the unit goes again.
    #[test]
    fn a_unit_that_does_not_start_is_taken_out_again() {
        let steps = [
            ("daemon-reload", vec![ACTIVE, VERSIONED, RESET_UNIT, RELOAD]),
            (
                "enable marsa-dns-link.service",
                vec![ACTIVE, VERSIONED, RESET_UNIT, RELOAD, ENABLED],
            ),
            (
                "restart marsa-dns-link.service",
                vec![ACTIVE, VERSIONED, RESET_UNIT, RELOAD, ENABLED, UNIT_UP],
            ),
        ];
        for (step, mut commands) in steps {
            let mut linux = Linux::new();
            linux.failing = Some(step);
            let failed = install(18053, &linux);
            assert_eq!(code_of(&failed), RESOLVED_UNIT_FAILED, "{step}");
            assert_eq!(
                message_of(&failed),
                format!("systemctl {step} exited with code 1; {UNIT_FILE} was removed again"),
            );
            commands.extend([DISABLED, RELOAD, RESET_UNIT]);
            assert_eq!(linux.commands(), commands, "{step}");
            assert_eq!(linux.unit_file(), None);
            assert!(!linux.enabled.get() && linux.server().is_none());
        }
    }

    /// resolved needs a moment for a new link: the check is tried again.
    #[test]
    fn a_link_that_needs_a_moment_is_checked_again() {
        let linux = Linux::new();
        linux.none_for.set(2);
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        assert_eq!(
            linux.commands(),
            [
                ACTIVE, VERSIONED, RESET_UNIT, RELOAD, ENABLED, UNIT_UP, LOOKED, LOOKED, LOOKED
            ]
        );
    }

    /// A link resolved never uses: ten looks, then the unit goes.
    #[test]
    fn a_link_resolved_does_not_use_is_resolved_check_failed() {
        let mut linux = Linux::new();
        linux.unused = true;
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_CHECK_FAILED);
        assert_eq!(
            message_of(&failed),
            format!("systemd-resolved does not use marsa0 for DNS; {UNIT_FILE} was removed again")
        );
        let mut commands = vec![ACTIVE, VERSIONED, RESET_UNIT, RELOAD, ENABLED, UNIT_UP];
        commands.extend([LOOKED; 10]);
        commands.extend([DISABLED, RELOAD, RESET_UNIT]);
        assert_eq!(linux.commands(), commands);
        assert_eq!(linux.unit_file(), None);
        assert!(linux.server().is_none());
    }

    /// Each part of the check, with a unit that misses it: the fake runs the
    /// unit as written, so these are units Marsa must never accept.
    #[test]
    fn the_check_needs_each_setting_of_the_link() {
        let base = String::from_utf8(ours(18053)).unwrap();
        let cases = [
            (
                base.replace("127.0.0.1:18053", "127.0.0.1:18054"),
                "marsa0 does not have Marsa's DNS server alone",
            ),
            (
                base.replace("default-route marsa0 false", "default-route marsa0 true"),
                "marsa0 is not marked as no default route",
            ),
            (
                base.replace(
                    "ExecStart=-/usr/sbin/ip addr add 169.254.53.53/32 dev marsa0\n",
                    "",
                ),
                "systemd-resolved does not use marsa0 for DNS",
            ),
        ];
        let more = [
            (
                base.replace("ExecStart=/usr/bin/resolvectl domain marsa0 ~test\n", ""),
                "marsa0 does not route .test names alone",
            ),
            (
                base.replace("domain marsa0 ~test", "domain marsa0 ~test ~."),
                "marsa0 does not route .test names alone",
            ),
            (
                base.replace("domain marsa0 ~test", "domain marsa0 ~test example.com"),
                "marsa0 does not route .test names alone",
            ),
        ];
        for (unit, reason) in cases.into_iter().chain(more) {
            assert_ne!(unit, base);
            let linux = Linux::with_unit(unit.as_bytes());
            assert_eq!(check(&linux, 18053), Err(reason.to_owned()));
        }
        let linux = Linux::with_unit(base.as_bytes());
        assert_eq!(check(&linux, 18053), Ok(()));
    }

    /// At no moment of the start does `marsa0` have Marsa's server for more
    /// than `.test`. The fake would see it: with `dns` first, it does.
    #[test]
    fn the_unit_never_routes_other_names_to_the_port() {
        let linux = Linux::new();
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        assert!(!linux.exposed.get());
        let base = String::from_utf8(ours(18053)).unwrap();
        let dns = "ExecStart=/usr/bin/resolvectl dns marsa0 127.0.0.1:18053\n";
        let dns_first = base.replace(dns, "").replace(
            "ExecStart=/usr/bin/resolvectl default-route",
            &format!("{dns}ExecStart=/usr/bin/resolvectl default-route"),
        );
        assert!(Linux::with_unit(dns_first.as_bytes()).exposed.get());
    }

    /// A line of the unit fails half way: its start fails, and the link
    /// goes with `ExecStopPost=`, never left with a server for every name.
    #[test]
    fn a_start_that_fails_half_way_leaves_no_link() {
        for line in [
            "default-route marsa0",
            "domain marsa0",
            "dns marsa0",
            "link set",
        ] {
            let mut linux = Linux::new();
            linux.failing_line = Some(line);
            let failed = install(18053, &linux);
            assert_eq!(code_of(&failed), RESOLVED_UNIT_FAILED, "{line}");
            assert!(linux.link.borrow().is_none(), "{line}");
            assert!(!linux.exposed.get(), "{line}");
            assert_eq!(linux.unit_file(), None, "{line}");
        }
        // With `ExecStop=` instead, a failed start would leave the link.
        let base = String::from_utf8(ours(18053)).unwrap();
        let mut linux = Linux::new();
        linux.put(
            UNIT_FILE,
            base.replace("ExecStopPost=-", "ExecStop=").as_bytes(),
        );
        *linux.loaded.borrow_mut() = linux.unit_file().map(|(bytes, _)| bytes);
        linux.failing_line = Some("dns marsa0");
        assert!(!linux.start_unit());
        assert!(linux.link.borrow().is_some());
    }

    #[test]
    fn a_status_that_cannot_be_read_fails_the_check() {
        let mut linux = Linux::with_unit(&ours(18053));
        linux.resolvectl_fails = true;
        assert_eq!(
            check(&linux, 18053),
            Err("resolvectl status marsa0 exited with code 1".to_owned())
        );
    }

    /// After a failed check, the message tells what the cleanup did.
    #[test]
    fn a_unit_that_cannot_be_taken_out_again_is_named() {
        let mut linux = Linux::new();
        linux.unused = true;
        linux.fake.fail_removes.insert(PathBuf::from(UNIT_FILE));
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_CHECK_FAILED);
        assert_eq!(
            message_of(&failed),
            format!(
                "systemd-resolved does not use marsa0 for DNS; cannot delete {UNIT_FILE}: \
                 permission denied"
            )
        );
        assert!(!linux.enabled.get());
    }

    /// systemd 262 wraps the protocols onto a second line.
    #[test]
    fn the_status_of_systemd_262_is_read_whole() {
        let mut linux = Linux::new();
        linux.fedora = true;
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
    }

    // --- the old global drop-in --------------------------------------------

    #[test]
    fn install_takes_out_the_old_drop_in_first() {
        let linux = Linux::new().with_drop_in();
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        assert_eq!(linux.file(DROP_IN), None);
        assert!(!linux.drop_in_loaded.get());
        assert_eq!(
            linux.commands(),
            [
                ACTIVE, RESET, RESTARTED, ACTIVE, VERSIONED, RESET_UNIT, RELOAD, ENABLED, UNIT_UP,
                LOOKED
            ]
        );
    }

    /// The unit was fine, but the drop-in was still there: `done`.
    #[test]
    fn install_over_a_working_unit_and_the_drop_in_is_done() {
        let linux = Linux::with_unit(&ours(18053)).with_drop_in();
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        assert_eq!(linux.file(DROP_IN), None);
        assert_eq!(
            linux.commands(),
            [
                ACTIVE,
                RESET,
                RESTARTED,
                ACTIVE,
                VERSIONED,
                LOOKED,
                IS_ENABLED_CMD
            ]
        );
    }

    #[test]
    fn a_foreign_drop_in_is_left_alone() {
        let linux = Linux::new();
        linux.put(DROP_IN, FOREIGN_DROP_IN);
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
        assert_eq!(
            linux.file(DROP_IN),
            Some((FOREIGN_DROP_IN.to_vec(), OLD_MODE))
        );
    }

    /// A drop-in that cannot go stops the install before the unit.
    #[test]
    fn a_drop_in_that_cannot_be_deleted_stops_the_install() {
        let mut linux = Linux::new().with_drop_in();
        linux.fake.fail_removes.insert(PathBuf::from(DROP_IN));
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_DELETE);
        assert_eq!(
            message_of(&failed),
            format!("cannot delete {DROP_IN}: permission denied")
        );
        assert_eq!(linux.unit_file(), None);
        assert!(linux.commands().is_empty());
    }

    /// The restart without the drop-in fails while the old process runs:
    /// it may still use Marsa's server for every name.
    #[test]
    fn a_failed_restart_with_the_old_process_running_is_resolved_restart_failed() {
        let mut linux = Linux::new().with_drop_in();
        linux.failing_restarts.set(1);
        linux.old_survives = true;
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_RESTART_FAILED);
        assert_eq!(
            message_of(&failed),
            format!(
                "{DROP_IN} was deleted, but systemctl restart systemd-resolved.service exited \
                 with code 1; {OLD_RUNS}"
            )
        );
        assert_eq!(linux.commands(), [ACTIVE, RESET, RESTARTED, ACTIVE]);
        assert_eq!(linux.unit_file(), None);
    }

    /// A restart that fails and leaves it stopped: one start reads the
    /// folder without the drop-in.
    #[test]
    fn a_failed_restart_after_the_delete_is_mended_by_a_start() {
        let linux = Linux::new().with_drop_in();
        linux.failing_restarts.set(1);
        assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
        assert_eq!(
            linux.commands(),
            [ACTIVE, RESET, RESTARTED, ACTIVE, RESET, STARTED, ACTIVE]
        );
        assert!(linux.active.get() && !linux.drop_in_loaded.get());
    }

    #[test]
    fn a_resolved_that_stays_down_after_the_delete_is_resolved_stopped() {
        let mut linux = Linux::new().with_drop_in();
        linux.failing_restarts.set(1);
        linux.start_fails = true;
        let failed = remove(&linux);
        assert_eq!(code_of(&failed), RESOLVED_STOPPED);
        assert_eq!(
            message_of(&failed),
            format!(
                "{DROP_IN} was deleted, but systemctl restart systemd-resolved.service exited \
                 with code 1; {STOPPED}"
            )
        );
        assert_eq!(linux.file(DROP_IN), None);
    }

    /// resolved does not run: the old drop-in still goes, and the message
    /// says that this is all that changed.
    #[test]
    fn install_with_a_stopped_resolved_still_takes_out_the_drop_in() {
        let linux = Linux::new().with_drop_in();
        linux.active.set(false);
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_NOT_RUNNING);
        assert_eq!(
            message_of(&failed),
            "systemd-resolved.service is not running, so only Marsa's old global setting was \
             removed"
        );
        assert_eq!(linux.file(DROP_IN), None);
        assert_eq!(linux.unit_file(), None);
    }

    /// A stopped resolved reads its files when it starts: no restart.
    #[test]
    fn remove_does_not_start_a_resolved_that_does_not_run() {
        let linux = Linux::new().with_drop_in();
        linux.active.set(false);
        assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
        assert_eq!(linux.file(DROP_IN), None);
        assert_eq!(linux.commands(), [ACTIVE]);
        assert!(!linux.active.get());
    }

    /// `systemctl` cannot tell whether it runs: it may run with the
    /// drop-in, so the removal is not `done`.
    #[test]
    fn remove_when_systemctl_cannot_tell_is_resolved_restart_failed() {
        let mut linux = Linux::new().with_drop_in();
        linux.active_code = Some(1);
        let failed = remove(&linux);
        assert_eq!(code_of(&failed), RESOLVED_RESTART_FAILED);
        assert_eq!(
            message_of(&failed),
            format!(
                "{DROP_IN} was deleted, but cannot check systemd-resolved.service: systemctl \
                 exited with code 1; {OLD_RUNS}"
            )
        );
        assert_eq!(linux.file(DROP_IN), None);
        assert_eq!(linux.commands(), [ACTIVE]);
    }

    // --- remove --------------------------------------------------------------

    #[test]
    fn remove_without_anything_of_marsa_is_unchanged_and_runs_nothing() {
        let foreign_drop_in = Linux::new();
        foreign_drop_in.put(DROP_IN, FOREIGN_DROP_IN);
        for linux in [Linux::new(), Linux::with_unit(FOREIGN), foreign_drop_in] {
            let before = (linux.unit_file(), linux.file(DROP_IN));
            assert_eq!(remove(&linux), result(REMOVE, Outcome::Unchanged));
            assert_eq!((linux.unit_file(), linux.file(DROP_IN)), before);
            assert!(linux.commands().is_empty());
        }
    }

    #[test]
    fn remove_stops_disables_and_deletes_the_unit() {
        // Also with another port: the marker makes it ours, not the content.
        for port in [18053, 18054] {
            let linux = Linux::with_unit(&ours(port));
            assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
            assert_eq!(linux.unit_file(), None);
            assert_eq!(linux.commands(), [DISABLED, RELOAD, RESET_UNIT]);
            assert!(!linux.enabled.get() && linux.server().is_none());
            assert!(linux.loaded.borrow().is_none());
        }
    }

    #[test]
    fn remove_takes_out_the_unit_and_the_drop_in() {
        let linux = Linux::with_unit(&ours(18053)).with_drop_in();
        assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
        assert_eq!((linux.unit_file(), linux.file(DROP_IN)), (None, None));
        assert_eq!(
            linux.commands(),
            [DISABLED, RELOAD, RESET_UNIT, ACTIVE, RESET, RESTARTED]
        );
    }

    /// The stop fails: the file still goes, so the link does not come back
    /// at the next boot, and the message says it may stay until then.
    #[test]
    fn a_failed_stop_still_deletes_the_unit() {
        let mut linux = Linux::with_unit(&ours(18053));
        linux.failing = Some("disable --now marsa-dns-link.service");
        let failed = remove(&linux);
        assert_eq!(code_of(&failed), RESOLVED_UNIT_FAILED);
        assert_eq!(
            message_of(&failed),
            format!(
                "{UNIT_FILE} was deleted, but systemctl disable --now marsa-dns-link.service \
                 exited with code 1; marsa0 may stay until the next boot"
            )
        );
        assert_eq!(linux.unit_file(), None);
        assert_eq!(linux.commands(), [DISABLED, RELOAD, RESET_UNIT]);
        assert!(linux.server().is_some());
    }

    /// A unit that cannot be deleted: the drop-in is still tried, and the
    /// first failure is the result.
    #[test]
    fn a_failed_delete_is_resolved_delete_and_the_drop_in_still_goes() {
        let mut linux = Linux::with_unit(&ours(18053)).with_drop_in();
        linux.fake.fail_removes.insert(PathBuf::from(UNIT_FILE));
        let failed = remove(&linux);
        assert_eq!(code_of(&failed), RESOLVED_DELETE);
        assert_eq!(
            message_of(&failed),
            format!("cannot delete {UNIT_FILE}: permission denied")
        );
        assert_eq!(linux.unit_file(), Some((ours(18053), OLD_MODE)));
        assert_eq!(linux.file(DROP_IN), None);
    }

    // --- resolvectl status ---------------------------------------------------

    #[test]
    fn the_output_of_resolvectl_status() {
        let read = |text: &str| read_status(&exit(0, text)).unwrap();
        let words = |list: &[&str]| {
            list.iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>()
        };
        // systemd 249 (Ubuntu 22.04), from spike S-02b.
        let ubuntu = read(
            "Link 5 (marsa0)\nCurrent Scopes: DNS\n     Protocols: -DefaultRoute +LLMNR -mDNS \
             -DNSOverTLS DNSSEC=no/unsupported\n   DNS Servers: 127.0.0.1:18053\n    DNS \
             Domain: ~test\n",
        );
        assert_eq!(
            ubuntu,
            LinkStatus {
                scopes: words(&["DNS"]),
                protocols: words(&[
                    "-DefaultRoute",
                    "+LLMNR",
                    "-mDNS",
                    "-DNSOverTLS",
                    "DNSSEC=no/unsupported"
                ]),
                servers: words(&["127.0.0.1:18053"]),
                domains: words(&["~test"]),
            }
        );
        // systemd 262 (Fedora 45): wrapped protocols, and keys of its own.
        let fedora = read(
            "Link 3 (marsa0)\n    Current Scopes: none\n         Protocols: -DefaultRoute \
             LLMNR=resolve -mDNS -DNSOverTLS\n                    DNSSEC=no/unsupported\nCurrent \
             DNS Server: 127.0.0.1:18053\n       DNS Servers: 127.0.0.1:18053\n        DNS \
             Domain: ~test\n     Default Route: no\n",
        );
        assert_eq!(fedora.scopes, ["none"]);
        assert_eq!(fedora.protocols.last().unwrap(), "DNSSEC=no/unsupported");
        assert_eq!(fedora.servers, ["127.0.0.1:18053"]);
        // A wrapped list of servers, with a `:` in each, stays one list.
        let wrapped = read("       DNS Servers: 127.0.0.1:18053\n                    fd00::1\n");
        assert_eq!(wrapped.servers, ["127.0.0.1:18053", "fd00::1"]);
        // No settings yet.
        let bare = read("Link 5 (marsa0)\nCurrent Scopes: none\n");
        assert!(bare.servers.is_empty() && bare.domains.is_empty());
        assert_eq!(read_status(&exit(1, "")), None);
        assert_eq!(read_status(&Err(io::ErrorKind::NotFound.into())), None);
    }

    /// Through a request, as `main` runs it.
    #[test]
    fn the_ops_run_from_a_request_on_linux_only() {
        let ops = json!([
            { "op": "resolved_install", "port": 18053 },
            { "op": "resolved_remove" },
        ]);
        let raw = json!({ "version": 1, "ops": ops }).to_string();
        let request = Request::from_arg(OsStr::new(&raw)).unwrap();
        let linux = Linux::new();
        let expected = [
            result(INSTALL, Outcome::Done),
            result(REMOVE, Outcome::Done),
        ];
        assert_eq!(apply(&request, Os::Linux, &linux), expected);
        assert_eq!(linux.unit_file(), None);

        for os in [Os::Macos, Os::Windows] {
            let linux = Linux::new();
            let results = apply(&request, os, &linux);
            let codes: Vec<_> = results.iter().map(code_of).collect();
            assert_eq!(codes, [crate::response::code::OP_NOT_SUPPORTED; 2]);
            assert!(linux.commands().is_empty() && linux.unit_file().is_none());
        }
    }
}
