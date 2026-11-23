use std::cell::{Cell, RefCell};
use std::ffi::OsStr;
use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::ops::apply;
use crate::ops::testing::code_of;
use crate::os::Os;
use crate::request::Request;
use crate::response::Outcome;
use crate::system::{FakeSystem, Hooked, RunHook};

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
const NETWORKD: &str = "/usr/bin/systemctl is-active systemd-networkd.service \
        systemd-networkd.socket systemd-networkd-wait-online.service";
const NETWORKD_ENABLED_CMD: &str = "/usr/bin/systemctl is-enabled systemd-networkd.service \
        systemd-networkd.socket systemd-networkd-wait-online.service";
/// The mode of files that were there before a test, so a rewrite shows.
const OLD_MODE: u32 = 0o600;
const FOREIGN: &[u8] = b"[Unit]\nDescription=someone else's\n";
const FOREIGN_DROP_IN: &[u8] = b"[Resolve]\nDNS=9.9.9.9\n";

/// The global drop-in, as an older version wrote it.
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
type Linux = Hooked<Machine>;

struct Machine {
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
        let machine = Machine {
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
            networkd_active: "active\nactive\nactive\n",
            networkd_enabled: "enabled\nenabled\nenabled\n",
            wipe_after: Cell::new(None),
            strict_dot: false,
        };
        let linux = Hooked::with_hook(FakeSystem::default(), machine);
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
            "is-active systemd-networkd.service systemd-networkd.socket \
                 systemd-networkd-wait-online.service" => {
                let code = if self.networkd_active.starts_with("active") {
                    0
                } else {
                    3
                };
                exit(code, self.networkd_active)
            }
            "is-enabled systemd-networkd.service systemd-networkd.socket \
                 systemd-networkd-wait-online.service" => {
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

impl RunHook for Machine {
    fn run(
        linux: &Linux,
        program: &'static str,
        args: &[&str],
        _scripted: io::Result<RunOutput>,
    ) -> io::Result<RunOutput> {
        match program {
            "/usr/bin/systemctl" => linux.systemctl(&args.join(" ")),
            "/usr/bin/resolvectl" if args == ["status", "marsa0"] => linux.status(),
            _ => panic!("unexpected command: {program} {args:?}"),
        }
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
    assert_eq!(install(18053, &linux), OpResult::done(INSTALL));
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
    linux.networkd_active = "inactive\ninactive\ninactive\n";
    linux.networkd_enabled = "disabled\nmasked\ndisabled\n";
    assert_eq!(install(18053, &linux), OpResult::done(INSTALL));
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
    older.networkd_active = "inactive\ninactive\ninactive\n";
    older.networkd_enabled = "disabled\ndisabled\ndisabled\n";
    assert_eq!(code_of(&install(18053, &older)), RESOLVED_USE_HOSTS);
    assert_eq!(older.unit_file(), None);
}

/// Before 250, networkd that is not running now but may run later
/// (enabled, its socket enabled, failed, or activating) gets the hosts
/// path: the race would come back at the next boot (review of #125).
#[test]
fn a_systemd_before_250_with_networkd_that_may_run_gets_hosts() {
    for (active, enabled) in [
        (
            "inactive\ninactive\ninactive\n",
            "enabled\ndisabled\ndisabled\n",
        ),
        (
            "inactive\ninactive\ninactive\n",
            "disabled\nenabled\ndisabled\n",
        ),
        (
            "inactive\ninactive\ninactive\n",
            "static\ndisabled\ndisabled\n",
        ),
        (
            "failed\ninactive\ninactive\n",
            "disabled\ndisabled\ndisabled\n",
        ),
        (
            "activating\ninactive\ninactive\n",
            "disabled\ndisabled\ndisabled\n",
        ),
        ("inactive\n", "disabled\ndisabled\ndisabled\n"),
        ("inactive\ninactive\ninactive\n", ""),
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
        assert_eq!(install(18053, &linux), OpResult::done(INSTALL), "{line}");
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
        assert_eq!(install(18053, &linux), OpResult::done(INSTALL), "{line}");
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
    assert_eq!(install(18053, &linux), OpResult::done(INSTALL));
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
    assert_eq!(install(18053, &linux), OpResult::unchanged(INSTALL));
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
    assert_eq!(install(18053, &linux), OpResult::done(INSTALL));
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
    assert_eq!(install(18053, &linux), OpResult::done(INSTALL));
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
    assert_eq!(install(18053, &linux), OpResult::done(INSTALL));
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
    assert_eq!(install(18053, &linux), OpResult::done(INSTALL));
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
    let message = format!("cannot read {UNIT_FILE}: permission denied, so it was left as it is");
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
    assert_eq!(install(18053, &linux), OpResult::done(INSTALL));
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
    assert_eq!(install(18053, &linux), OpResult::done(INSTALL));
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
    assert_eq!(install(18053, &linux), OpResult::done(INSTALL));
}

// --- the old global drop-in --------------------------------------------

#[test]
fn install_takes_out_the_old_drop_in_first() {
    let linux = Linux::new().with_drop_in();
    assert_eq!(install(18053, &linux), OpResult::done(INSTALL));
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
    assert_eq!(install(18053, &linux), OpResult::done(INSTALL));
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
    assert_eq!(install(18053, &linux), OpResult::done(INSTALL));
    assert_eq!(remove(&linux), OpResult::done(REMOVE));
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
    assert_eq!(remove(&linux), OpResult::done(REMOVE));
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
    assert_eq!(remove(&linux), OpResult::done(REMOVE));
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
        assert_eq!(remove(&linux), OpResult::unchanged(REMOVE));
        assert_eq!((linux.unit_file(), linux.file(DROP_IN)), before);
        assert!(linux.commands().is_empty());
    }
}

#[test]
fn remove_stops_disables_and_deletes_the_unit() {
    // Also with another port: the marker makes it ours, not the content.
    for port in [18053, 18054] {
        let linux = Linux::with_unit(&ours(port));
        assert_eq!(remove(&linux), OpResult::done(REMOVE));
        assert_eq!(linux.unit_file(), None);
        assert_eq!(linux.commands(), [DISABLED, RELOAD, RESET_UNIT]);
        assert!(!linux.enabled.get() && linux.server().is_none());
        assert!(linux.loaded.borrow().is_none());
    }
}

#[test]
fn remove_takes_out_the_unit_and_the_drop_in() {
    let linux = Linux::with_unit(&ours(18053)).with_drop_in();
    assert_eq!(remove(&linux), OpResult::done(REMOVE));
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
    let expected = [OpResult::done(INSTALL), OpResult::done(REMOVE)];
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
