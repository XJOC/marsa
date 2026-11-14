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
/// Starts the unit if it does not run, and runs a changed one again.
const RESTART_UNIT: [&str; 2] = ["restart", UNIT_NAME];
/// Stops the unit, whose stop deletes the link, and takes it out of the
/// boot.
const DISABLE: [&str; 3] = ["disable", "--now", UNIT_NAME];
const STATUS: [&str; 2] = ["status", LINK];
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
        let message = format!("{reason}, so nothing was changed");
        return OpResult::failed(INSTALL, RESOLVED_NOT_RUNNING, message);
    }
    let Some(ip) = IP.into_iter().find(|path| system.exists(Path::new(path))) else {
        let message = format!(
            "no ip program (iproute2) at {}, so nothing was changed",
            IP.join(", ")
        );
        return OpResult::failed(INSTALL, RESOLVED_TOOL_MISSING, message);
    };
    // The global drop-in first: while it is there, every name reaches the
    // port.
    let dropped = match drop_old(system) {
        Ok(dropped) => dropped,
        Err((code, message)) => return OpResult::failed(INSTALL, code, message),
    };
    let content = unit(ip, port);
    let in_place = current.as_deref() == Some(content.as_bytes());
    if in_place && check(system, port).is_ok() {
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

/// The unit, for `ip` at `ip`. LF only, with a final newline. A `-` before
/// a command lets the unit go on when it fails: the link or its address may
/// be there already.
fn unit(ip: &str, port: u16) -> String {
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
ExecStart={RESOLVECTL} dns {LINK} {server}
ExecStart={RESOLVECTL} domain {LINK} ~test
ExecStart={RESOLVECTL} default-route {LINK} false
ExecStop={ip} link del {LINK}

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

/// Loads the unit file, adds the unit to the boot, and (re)starts it.
fn start(system: &impl System) -> Result<(), String> {
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

/// Stops and disables the unit (its stop deletes the link), deletes its
/// file, and makes systemd forget it. The file is tried even when the stop
/// failed: without it, the unit does not run at the next boot.
fn take_out(system: &impl System) -> Result<(), (&'static str, String)> {
    let disabled = system.run(SYSTEMCTL, &DISABLE);
    let removed = system.remove(Path::new(UNIT_FILE));
    let _ = system.run(SYSTEMCTL, &DAEMON_RELOAD);
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
    } else if !has(&link.domains, "~test") {
        Err(format!("{LINK} does not route .test names"))
    } else if !has(&link.protocols, "-DefaultRoute") {
        // Without it the link would get other names too: the very thing
        // this setup avoids.
        Err(format!("{LINK} is not marked as no default route"))
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
    const LOOKED: &str = "/usr/bin/resolvectl status marsa0";
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
        unit("/usr/sbin/ip", port).into_bytes()
    }

    /// `marsa0` while it is there: what the unit that made it set.
    #[derive(Clone, Debug)]
    struct Link {
        server: String,
        address: bool,
        default_route: bool,
    }

    /// The link a unit makes when it starts, read from what the unit runs.
    fn link_of(unit: &[u8]) -> Option<Link> {
        let text = String::from_utf8_lossy(unit);
        let server = text
            .lines()
            .find_map(|line| line.split_once("resolvectl dns marsa0 "))
            .map(|(_, server)| server.to_owned())?;
        Some(Link {
            server,
            address: text.contains("addr add 169.254.53.53/32 dev marsa0"),
            default_route: !text.contains("default-route marsa0 false"),
        })
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
        link: RefCell<Option<Link>>,
        /// This `systemctl` command line fails.
        failing: Option<&'static str>,
        /// resolved does not use the link, whatever the unit did.
        unused: bool,
        /// `status` says "Current Scopes: none" that many more times first.
        none_for: Cell<u32>,
        resolvectl_fails: bool,
        /// `status` as systemd 262 prints it (Fedora 45).
        fedora: bool,
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
                link: RefCell::new(None),
                failing: None,
                unused: false,
                none_for: Cell::new(0),
                resolvectl_fails: false,
                fedora: false,
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
            *linux.link.borrow_mut() = link_of(bytes);
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
            self.link.borrow().as_ref().map(|link| link.server.clone())
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
                "enable marsa-dns-link.service" => {
                    if self.loaded.borrow().is_none() {
                        return exit(1, "");
                    }
                    self.enabled.set(true);
                    exit(0, "")
                }
                "restart marsa-dns-link.service" => {
                    let link = self.loaded.borrow().as_deref().and_then(link_of);
                    let started = link.is_some();
                    *self.link.borrow_mut() = link;
                    exit(if started { 0 } else { 5 }, "")
                }
                "disable --now marsa-dns-link.service" => {
                    *self.link.borrow_mut() = None;
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
            let mut used = link.address && !self.unused;
            if self.none_for.get() > 0 {
                self.none_for.set(self.none_for.get() - 1);
                used = false;
            }
            let scopes = if used { "DNS" } else { "none" };
            let route = if link.default_route { '+' } else { '-' };
            let server = link.server;
            let text = if self.fedora {
                format!(
                    "Link 3 (marsa0)\n    Current Scopes: {scopes}\n         Protocols: \
                     {route}DefaultRoute LLMNR=resolve -mDNS -DNSOverTLS\n                    \
                     DNSSEC=no/unsupported\nCurrent DNS Server: {server}\n       DNS Servers: \
                     {server}\n        DNS Domain: ~test\n     Default Route: no\n"
                )
            } else {
                format!(
                    "Link 5 (marsa0)\nCurrent Scopes: {scopes}\n     Protocols: \
                     {route}DefaultRoute +LLMNR -mDNS -DNSOverTLS DNSSEC=no/unsupported\n   DNS \
                     Servers: {server}\n    DNS Domain: ~test\n"
                )
            };
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
        assert_eq!(linux.commands(), [ACTIVE, RELOAD, ENABLED, UNIT_UP, LOOKED]);
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
        assert_eq!(bytes, unit("/bin/ip", 18053).into_bytes());
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
        assert_eq!(linux.commands(), [ACTIVE]);
    }

    /// The same unit, running and checked: one look, nothing written.
    #[test]
    fn install_over_our_working_unit_is_unchanged() {
        let mut linux = Linux::with_unit(&ours(18053));
        // Any write fails here, so `unchanged` also shows that none was tried.
        linux.fake.fail_writes.insert(PathBuf::from(UNIT_FILE));
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Unchanged));
        assert_eq!(linux.unit_file(), Some((ours(18053), OLD_MODE)));
        assert_eq!(linux.commands(), [ACTIVE, LOOKED]);
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
            [ACTIVE, LOOKED, RELOAD, ENABLED, UNIT_UP, LOOKED]
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
        assert_eq!(linux.commands(), [ACTIVE, RELOAD, ENABLED, UNIT_UP, LOOKED]);
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
        assert_eq!(linux.commands(), [ACTIVE]);
    }

    /// Each `systemctl` step of the start can fail: the unit goes again.
    #[test]
    fn a_unit_that_does_not_start_is_taken_out_again() {
        let steps = [
            ("daemon-reload", vec![ACTIVE, RELOAD]),
            (
                "enable marsa-dns-link.service",
                vec![ACTIVE, RELOAD, ENABLED],
            ),
            (
                "restart marsa-dns-link.service",
                vec![ACTIVE, RELOAD, ENABLED, UNIT_UP],
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
            commands.extend([DISABLED, RELOAD]);
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
            [ACTIVE, RELOAD, ENABLED, UNIT_UP, LOOKED, LOOKED, LOOKED]
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
        let mut commands = vec![ACTIVE, RELOAD, ENABLED, UNIT_UP];
        commands.extend([LOOKED; 10]);
        commands.extend([DISABLED, RELOAD]);
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
                base.replace(
                    "ExecStart=/usr/bin/resolvectl default-route marsa0 false\n",
                    "",
                ),
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
        for (unit, reason) in cases {
            assert_ne!(unit, base);
            let linux = Linux::with_unit(unit.as_bytes());
            assert_eq!(check(&linux, 18053), Err(reason.to_owned()));
        }
        let linux = Linux::with_unit(base.as_bytes());
        assert_eq!(check(&linux, 18053), Ok(()));
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
                ACTIVE, ACTIVE, RESET, RESTARTED, RELOAD, ENABLED, UNIT_UP, LOOKED
            ]
        );
    }

    /// The unit was fine, but the drop-in was still there: `done`.
    #[test]
    fn install_over_a_working_unit_and_the_drop_in_is_done() {
        let linux = Linux::with_unit(&ours(18053)).with_drop_in();
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        assert_eq!(linux.file(DROP_IN), None);
        assert_eq!(linux.commands(), [ACTIVE, ACTIVE, RESET, RESTARTED, LOOKED]);
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
        assert_eq!(linux.commands(), [ACTIVE]);
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
        assert_eq!(linux.commands(), [ACTIVE, ACTIVE, RESET, RESTARTED, ACTIVE]);
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
            assert_eq!(linux.commands(), [DISABLED, RELOAD]);
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
            [DISABLED, RELOAD, ACTIVE, RESET, RESTARTED]
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
        assert_eq!(linux.commands(), [DISABLED, RELOAD]);
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
