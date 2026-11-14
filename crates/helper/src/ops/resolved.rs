//! Linux: names under `.test` go to the DNS server of `marsad`, through a
//! drop-in file of systemd-resolved (ADR 0014). The macOS twin is `resolver`.
//!
//! The file has a port (`DNS=127.0.0.1:18053`), so the DNS server does not
//! need port 53. `Domains=~test` routes `.test` names there, but a global
//! server is also a default route: systemd-resolved sends every other name
//! there too, and takes the first good answer (ADR 0014, finding 2). A
//! program that holds the port while `marsad` does not could answer for any
//! name. So this file must never ship in a release: task M4-18h replaces it
//! with a link of its own (systemd before 258) or a DNS delegate (258 on).
//!
//! systemd-resolved asks one global server at a time: next to another global
//! `DNS=` line, `.test` names never resolve (spike S-02: 0 of 6 tries). And
//! a global server turns off its fallback servers, so a machine whose network
//! gives no DNS server would resolve nothing but `.test`. The helper checks
//! both before it writes, and again after the restart. A check that fails
//! after the write takes Marsa's file out again: a clear error, not a site
//! that does not open.
//!
//! Every restart clears a failed state first: many restarts in a few seconds
//! hit the unit's start limit (S-02, Fedora). After a failed install the
//! helper makes sure systemd-resolved runs again. If it does not, the code is
//! `resolved_stopped`: no name resolves until it runs.
//!
//! The helper changes the file only if Marsa wrote it ([`marked`]).
//!
//! No message holds the content of a file, the output of a program, or the
//! user's own DNS servers.

use std::io;
use std::path::Path;

use crate::ops::marked::{self, Current, MARKER};
use crate::ops::program;
use crate::response::{OpResult, Outcome};
use crate::system::{RunOutput, System};

/// A file at [`FILE`] that Marsa did not write, so it was left as it is.
pub const RESOLVED_FOREIGN: &str = "resolved_foreign";
/// [`FILE`] cannot be read, so it was left as it is.
pub const RESOLVED_READ: &str = "resolved_read";
/// systemd-resolved does not run, or `systemctl` cannot tell. Nothing was
/// changed.
pub const RESOLVED_NOT_RUNNING: &str = "resolved_not_running";
/// Writing [`FILE`] failed.
pub const RESOLVED_WRITE: &str = "resolved_write";
/// `systemctl restart` failed. After an install, Marsa's file is gone again.
pub const RESOLVED_RESTART_FAILED: &str = "resolved_restart_failed";
/// The machine has another global DNS server, so `.test` names would not
/// resolve.
pub const RESOLVED_OTHER_DNS: &str = "resolved_other_dns";
/// No network connection has a DNS server: with Marsa's global server,
/// systemd-resolved would not use its fallback servers.
pub const RESOLVED_NO_LINK_DNS: &str = "resolved_no_link_dns";
/// The servers could not be read, or Marsa's is not among them after the
/// restart.
pub const RESOLVED_CHECK_FAILED: &str = "resolved_check_failed";
/// systemd-resolved does not run after a failed step, and did not start
/// again: no name resolves until it runs.
pub const RESOLVED_STOPPED: &str = "resolved_stopped";
/// Deleting [`FILE`] failed.
pub const RESOLVED_DELETE: &str = "resolved_delete";

/// Marsa's own drop-in. systemd-resolved reads every file in this folder.
const FILE: &str = "/etc/systemd/resolved.conf.d/marsa.conf";
/// Everyone can read it, and only root can change it.
const MODE: u32 = 0o644;
const SYSTEMCTL: &str = "/usr/bin/systemctl";
const RESOLVECTL: &str = "/usr/bin/resolvectl";
const UNIT: &str = "systemd-resolved.service";
const IS_ACTIVE: [&str; 3] = ["is-active", "--quiet", UNIT];
const RESET_FAILED: [&str; 2] = ["reset-failed", UNIT];
/// `restart`, not `reload`: systemd-resolved has no reload before systemd 253.
const RESTART: [&str; 2] = ["restart", UNIT];
const START: [&str; 2] = ["start", UNIT];
/// The end of a message when systemd-resolved stays down.
const STOPPED: &str = "systemd-resolved does not run now, so no name resolves until it starts";
/// The end of a message when a restart failed and the old process runs on.
const OLD_RUNS: &str = "systemd-resolved may use Marsa's server until it restarts";

const INSTALL: &str = "resolved_install";
const REMOVE: &str = "resolved_remove";

/// `resolved_install`: names under `.test` go to `127.0.0.1:<port>`.
pub fn install(port: u16, system: &impl System) -> OpResult {
    let content = content(port);
    let ours = server(port);
    // `None`: no file of Marsa's. `Some(true)`: this very file.
    let in_place = match marked::current(system, Path::new(FILE)) {
        Current::Foreign => {
            // The path only: the content of another tool's file stays out.
            let message = format!("{FILE} was not written by Marsa, so it was left as it is");
            return OpResult::failed(INSTALL, RESOLVED_FOREIGN, message);
        }
        Current::Unreadable(kind) => return unreadable(INSTALL, kind),
        Current::Ours(bytes) => Some(bytes == content.as_bytes()),
        Current::Missing => None,
    };
    if let Err(reason) = running(system) {
        let message = format!("{reason}, so nothing was changed");
        return OpResult::failed(INSTALL, RESOLVED_NOT_RUNNING, message);
    }
    match in_place {
        // The same file, checked again (read only): it may never have been
        // loaded (a crash before the restart), or another global server came
        // later. A restart cannot give a connection a DNS server, so without
        // one the working file stays.
        Some(true) => match check(system, &ours, true) {
            Ok(()) => return result(INSTALL, Outcome::Unchanged),
            Err((code, reason)) if code == RESOLVED_NO_LINK_DNS => {
                let message = format!("{reason}; nothing was changed");
                return OpResult::failed(INSTALL, code, message);
            }
            Err(_) => {}
        },
        // No file of Marsa's yet: what can be told without it is checked
        // first, with nothing written and no restart.
        None => {
            if let Err((code, reason)) = check(system, &ours, false) {
                let message = format!("{reason}; nothing was changed");
                return OpResult::failed(INSTALL, code, message);
            }
        }
        // An older file of Marsa's (another port): its server is listed.
        Some(false) => {}
    }
    if in_place != Some(true)
        && let Err(err) = system.write_atomic(Path::new(FILE), content.as_bytes(), MODE)
    {
        let message = format!("cannot write {FILE}: {}", err.kind());
        return OpResult::failed(INSTALL, RESOLVED_WRITE, message);
    }
    let checked = restart(system)
        .map_err(|reason| (RESOLVED_RESTART_FAILED, reason))
        .and_then(|()| check(system, &ours, true));
    let Err((code, reason)) = checked else {
        return result(INSTALL, Outcome::Done);
    };
    // Marsa's file goes, also one that was there before this call, and
    // systemd-resolved starts again without it.
    let cleanup = match system.remove(Path::new(FILE)) {
        Ok(()) => "was removed again",
        Err(_) => "could not be removed",
    };
    let message = format!("{reason}; {FILE} {cleanup}");
    match settle(system, restart(system)) {
        Restarted::Fine => OpResult::failed(INSTALL, code, message),
        Restarted::OldStillRuns(why) => {
            let message = format!("{message}, but {why}; {OLD_RUNS}");
            OpResult::failed(INSTALL, code, message)
        }
        Restarted::Stopped(_) => {
            OpResult::failed(INSTALL, RESOLVED_STOPPED, format!("{message}; {STOPPED}"))
        }
    }
}

/// `resolved_remove`: deletes the file if Marsa wrote it. A missing or
/// foreign file is `unchanged`: nothing of Marsa's is there to remove.
pub fn remove(system: &impl System) -> OpResult {
    match marked::current(system, Path::new(FILE)) {
        Current::Missing | Current::Foreign => return result(REMOVE, Outcome::Unchanged),
        Current::Unreadable(kind) => return unreadable(REMOVE, kind),
        Current::Ours(_) => {}
    }
    if let Err(err) = system.remove(Path::new(FILE)) {
        let message = format!("cannot delete {FILE}: {}", err.kind());
        return OpResult::failed(REMOVE, RESOLVED_DELETE, message);
    }
    // A systemd-resolved that does not run reads its files when it starts.
    if running(system).is_err() {
        return result(REMOVE, Outcome::Done);
    }
    match settle(system, restart(system)) {
        Restarted::Fine => result(REMOVE, Outcome::Done),
        Restarted::OldStillRuns(why) => {
            let message = format!("{FILE} was deleted, but {why}; {OLD_RUNS}");
            OpResult::failed(REMOVE, RESOLVED_RESTART_FAILED, message)
        }
        Restarted::Stopped(why) => {
            let message = format!("{FILE} was deleted, but {why}; {STOPPED}");
            OpResult::failed(REMOVE, RESOLVED_STOPPED, message)
        }
    }
}

/// LF only, with a final newline.
fn content(port: u16) -> String {
    format!("{MARKER}\n[Resolve]\nDNS={}\nDomains=~test\n", server(port))
}

/// Marsa's DNS server as systemd-resolved writes it.
fn server(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

/// Whether systemd-resolved runs, or why not.
fn running(system: &impl System) -> Result<(), String> {
    let active = system.run(SYSTEMCTL, &IS_ACTIVE);
    match &active {
        Ok(output) if output.code == Some(0) => Ok(()),
        Ok(RunOutput { code: Some(_), .. }) => Err(format!("{UNIT} is not running")),
        _ => Err(format!(
            "cannot check {UNIT}: systemctl {}",
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
            "systemctl restart {UNIT} {}",
            program::failure(&restarted)
        ))
    }
}

/// Where a restart left systemd-resolved.
enum Restarted {
    /// It runs with the files as they are now.
    Fine,
    /// The restart failed, and the old process runs on with the files as
    /// they were. The reason is the restart's.
    OldStillRuns(String),
    /// It does not run, also after one more start: on most machines no name
    /// resolves. The reason is the restart's.
    Stopped(String),
}

/// After a failed restart: whether the old process runs on, or else one
/// more start, which reads the files as they are now.
fn settle(system: &impl System, restarted: Result<(), String>) -> Restarted {
    let Err(reason) = restarted else {
        return Restarted::Fine;
    };
    if running(system).is_ok() {
        return Restarted::OldStillRuns(reason);
    }
    let _ = system.run(SYSTEMCTL, &RESET_FAILED);
    let _ = system.run(SYSTEMCTL, &START);
    if running(system).is_ok() {
        Restarted::Fine
    } else {
        Restarted::Stopped(reason)
    }
}

/// Whether `.test` names reach `ours` and other names still resolve, from
/// `resolvectl dns`. `listed`: whether `ours` must be in the global list;
/// not before Marsa's file is written. Either way it is not another server.
fn check(system: &impl System, ours: &str, listed: bool) -> Result<(), (&'static str, String)> {
    let output = system.run(RESOLVECTL, &["dns"]);
    let Some(dns) = read_dns(&output) else {
        let reason = if program::succeeded(&output) {
            "printed no global line".to_owned()
        } else {
            program::failure(&output)
        };
        return Err((RESOLVED_CHECK_FAILED, format!("resolvectl dns {reason}")));
    };
    let others = dns
        .global
        .iter()
        .filter(|server| server.as_str() != ours)
        .count();
    if others > 0 {
        // How many, never which: the addresses are the user's own.
        let reason = format!(
            "systemd-resolved has {others} other global DNS server(s), so .test names would not \
             resolve"
        );
        return Err((RESOLVED_OTHER_DNS, reason));
    }
    if listed && !dns.global.iter().any(|server| server == ours) {
        let reason = "systemd-resolved does not list Marsa's DNS server".to_owned();
        return Err((RESOLVED_CHECK_FAILED, reason));
    }
    if !dns.link_server {
        let reason = "no network connection has a DNS server, and with Marsa's server \
                      systemd-resolved would not use its fallback servers"
            .to_owned();
        return Err((RESOLVED_NO_LINK_DNS, reason));
    }
    Ok(())
}

/// What `resolvectl dns` shows.
#[derive(Debug, PartialEq, Eq)]
struct Dns {
    /// The global servers, as written (`1.1.1.1#name`, `[::1]:53`).
    global: Vec<String>,
    /// Whether any network connection (link) has a server.
    link_server: bool,
}

/// The servers in the output of `resolvectl dns`, or `None` if the program
/// failed or printed no `Global:` line. A long list goes on, indented, on
/// the next lines:
///
/// ```text
/// Global: 1.1.1.1#cloudflare-dns.com 1.0.0.1#cloudflare-dns.com
///         127.0.0.1:18053
/// Link 2 (eth0): 168.63.129.16
/// Link 3 (docker0):
/// ```
///
/// An interface name never holds `:`, so the first `:` ends the head.
fn read_dns(listed: &io::Result<RunOutput>) -> Option<Dns> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Section {
        Global,
        Link,
        Other,
    }
    let output = listed
        .as_ref()
        .ok()
        .filter(|output| output.code == Some(0))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut global: Option<Vec<String>> = None;
    let mut link_server = false;
    let mut section = Section::Other;
    for line in text.lines() {
        let servers = if line.starts_with(char::is_whitespace) {
            line
        } else {
            let (head, servers) = line.split_once(':').unwrap_or((line, ""));
            section = if head == "Global" {
                global.get_or_insert_with(Vec::new);
                Section::Global
            } else if head.starts_with("Link ") {
                Section::Link
            } else {
                Section::Other
            };
            servers
        };
        let mut servers = servers.split_whitespace().map(str::to_owned).peekable();
        match section {
            Section::Global => global.get_or_insert_with(Vec::new).extend(servers),
            Section::Link => link_server |= servers.peek().is_some(),
            Section::Other => {}
        }
    }
    Some(Dns {
        global: global?,
        link_server,
    })
}

fn unreadable(op: &'static str, kind: io::ErrorKind) -> OpResult {
    let message = format!("cannot read {FILE}: {kind}, so it was left as it is");
    OpResult::failed(op, RESOLVED_READ, message)
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

    // Written out again, not taken from the constants above: a test must
    // fail if one of those changes.
    const CONF: &str = "/etc/systemd/resolved.conf.d/marsa.conf";
    const ACTIVE: &str = "/usr/bin/systemctl is-active --quiet systemd-resolved.service";
    const RESET: &str = "/usr/bin/systemctl reset-failed systemd-resolved.service";
    const RESTARTED: &str = "/usr/bin/systemctl restart systemd-resolved.service";
    const STARTED: &str = "/usr/bin/systemctl start systemd-resolved.service";
    const LISTED: &str = "/usr/bin/resolvectl dns";

    /// The mode of a file a test starts with. Marsa never writes it, so the
    /// mode shows whether an op rewrote the file.
    const OLD_MODE: u32 = 0o600;
    /// Another tool's drop-in at the same path.
    const FOREIGN: &[u8] = b"[Resolve]\nDNS=9.9.9.9\n";

    /// A Linux with systemd-resolved, whose `resolvectl dns` shows what it
    /// read at its last start. Files and the log are in `fake`.
    struct Linux {
        fake: FakeSystem,
        active: Cell<bool>,
        /// Marsa's file as systemd-resolved read it at its last start.
        loaded: RefCell<Option<Vec<u8>>>,
        /// The user's own global servers.
        others: Vec<&'static str>,
        /// Whether the network connection has a DNS server.
        link_server: bool,
        /// That many restarts fail, and leave it stopped.
        failing_restarts: Cell<u32>,
        start_fails: bool,
        no_systemctl: bool,
        resolvectl_fails: bool,
        /// A systemd-resolved that does not read the drop-in folder.
        ignores_file: bool,
        /// Long global lists: one server per line, as when they are wrapped.
        wrapped: bool,
        /// For each command: whether Marsa's file was there.
        file_at: RefCell<Vec<bool>>,
    }

    impl Linux {
        fn new() -> Self {
            Self {
                fake: FakeSystem::default(),
                active: Cell::new(true),
                loaded: RefCell::new(None),
                others: Vec::new(),
                link_server: true,
                failing_restarts: Cell::new(0),
                start_fails: false,
                no_systemctl: false,
                resolvectl_fails: false,
                ignores_file: false,
                wrapped: false,
                file_at: RefCell::new(Vec::new()),
            }
        }

        /// With `bytes` at Marsa's path, and systemd-resolved running with
        /// it loaded.
        fn with_file(bytes: &[u8]) -> Self {
            let linux = Self::new();
            let file = (bytes.to_vec(), OLD_MODE);
            linux
                .fake
                .files
                .borrow_mut()
                .insert(PathBuf::from(CONF), file);
            *linux.loaded.borrow_mut() = Some(bytes.to_vec());
            linux
        }

        /// The file at Marsa's path. No other file may appear.
        fn file(&self) -> Option<(Vec<u8>, u32)> {
            let files = self.fake.files.borrow();
            assert!(files.keys().all(|path| path == Path::new(CONF)));
            files.get(Path::new(CONF)).cloned()
        }

        /// Every command that ran, as `program arguments`.
        fn commands(&self) -> Vec<String> {
            let log = self.fake.log.borrow();
            log.iter()
                .map(|(program, args)| format!("{program} {}", args.join(" ")))
                .collect()
        }

        /// A (re)start: reads Marsa's file, if it is there.
        fn start(&self) {
            self.active.set(true);
            let file = self.file().map(|(bytes, _)| bytes);
            *self.loaded.borrow_mut() = file.filter(|_| !self.ignores_file);
        }

        /// What `resolvectl dns` prints now.
        fn listing(&self) -> String {
            let loaded = self.loaded.borrow();
            let ours = loaded.as_deref().and_then(|bytes| {
                let text = String::from_utf8_lossy(bytes).into_owned();
                text.lines()
                    .find_map(|line| line.strip_prefix("DNS="))
                    .map(str::to_owned)
            });
            let global: Vec<String> = self
                .others
                .iter()
                .map(|server| (*server).to_owned())
                .chain(ours)
                .collect();
            let separator = if self.wrapped { "\n        " } else { " " };
            let link = if self.link_server {
                " 168.63.129.16"
            } else {
                ""
            };
            format!(
                "Global: {}\nLink 2 (eth0):{link}\nLink 3 (docker0):\n",
                global.join(separator)
            )
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
            self.file_at.borrow_mut().push(self.file().is_some());
            match (program, args) {
                ("/usr/bin/systemctl", _) if self.no_systemctl => {
                    Err(io::ErrorKind::NotFound.into())
                }
                ("/usr/bin/systemctl", ["is-active", "--quiet", "systemd-resolved.service"]) => {
                    exit(if self.active.get() { 0 } else { 3 }, "")
                }
                ("/usr/bin/systemctl", ["reset-failed", "systemd-resolved.service"]) => exit(0, ""),
                ("/usr/bin/systemctl", ["restart", "systemd-resolved.service"]) => {
                    if self.failing_restarts.get() > 0 {
                        self.failing_restarts.set(self.failing_restarts.get() - 1);
                        self.active.set(false);
                        return exit(1, "");
                    }
                    self.start();
                    exit(0, "")
                }
                ("/usr/bin/systemctl", ["start", "systemd-resolved.service"]) => {
                    if self.start_fails {
                        return exit(1, "");
                    }
                    self.start();
                    exit(0, "")
                }
                ("/usr/bin/resolvectl", ["dns"]) => {
                    if self.resolvectl_fails || !self.active.get() {
                        return exit(1, "");
                    }
                    exit(0, &self.listing())
                }
                _ => panic!("unexpected command: {program} {args:?}"),
            }
        }

        fn current_exe(&self) -> io::Result<PathBuf> {
            self.fake.current_exe()
        }
    }

    fn ours(port: u16) -> Vec<u8> {
        let text =
            "# Managed by Marsa. Do not edit.\n[Resolve]\nDNS=127.0.0.1:PORT\nDomains=~test\n";
        text.replace("PORT", &port.to_string()).into_bytes()
    }

    fn code_of(result: &OpResult) -> &'static str {
        match result.outcome {
            Outcome::Failed { code, .. } => code,
            Outcome::Done => "done",
            Outcome::Unchanged => "unchanged",
        }
    }

    fn message_of(result: &OpResult) -> &str {
        match &result.outcome {
            Outcome::Failed { message, .. } => message,
            _ => "",
        }
    }

    #[test]
    fn install_checks_writes_restarts_and_checks_again() {
        let linux = Linux::new();
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        assert_eq!(linux.file(), Some((ours(18053), 0o644)));
        assert_eq!(linux.commands(), [ACTIVE, LISTED, RESET, RESTARTED, LISTED]);
        // The first check ran before the file was there, the restart after.
        assert_eq!(*linux.file_at.borrow(), [false, false, true, true, true]);
    }

    /// The snapshot does not show line endings, so they are checked here.
    #[test]
    fn the_written_file() {
        let linux = Linux::new();
        assert_eq!(install(18053, &linux).outcome, Outcome::Done);
        let (bytes, mode) = linux.file().unwrap();
        assert_eq!(mode, 0o644);
        assert!(bytes.ends_with(b"~test\n") && !bytes.contains(&b'\r'));
        let text = String::from_utf8(bytes).unwrap();
        insta::assert_snapshot!(text);
    }

    /// The same file, loaded and working: two read-only commands.
    #[test]
    fn install_over_our_working_file_is_unchanged() {
        let mut linux = Linux::with_file(&ours(18053));
        // Any write fails here, so `unchanged` also shows that none was tried.
        linux.fake.fail_writes.insert(PathBuf::from(CONF));
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Unchanged));
        assert_eq!(linux.file(), Some((ours(18053), OLD_MODE)));
        assert_eq!(linux.commands(), [ACTIVE, LISTED]);
    }

    /// The same file, never loaded (a crash before the restart): restarted
    /// and checked, not rewritten.
    #[test]
    fn install_over_our_file_that_was_not_loaded_restarts() {
        let linux = Linux::with_file(&ours(18053));
        *linux.loaded.borrow_mut() = None;
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        assert_eq!(linux.file(), Some((ours(18053), OLD_MODE)));
        assert_eq!(linux.commands(), [ACTIVE, LISTED, RESET, RESTARTED, LISTED]);
    }

    /// An older file of Marsa's: its server is listed, so the check comes
    /// after the restart only.
    #[test]
    fn install_rewrites_our_file_with_another_port() {
        let linux = Linux::with_file(&ours(18054));
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        assert_eq!(linux.file(), Some((ours(18053), 0o644)));
        assert_eq!(linux.commands(), [ACTIVE, RESET, RESTARTED, LISTED]);
    }

    /// The exact message also shows that it holds the path and no content.
    #[test]
    fn install_fails_on_a_foreign_file_and_keeps_it() {
        let large = {
            let mut bytes = ours(18053);
            bytes.resize(4097, b'#');
            bytes
        };
        for bytes in [FOREIGN.to_vec(), large] {
            let linux = Linux::with_file(&bytes);
            let message = format!("{CONF} was not written by Marsa, so it was left as it is");
            let expected = OpResult::failed(INSTALL, RESOLVED_FOREIGN, message);
            assert_eq!(install(18053, &linux), expected);
            assert_eq!(remove(&linux), result(REMOVE, Outcome::Unchanged));
            assert_eq!(linux.file(), Some((bytes, OLD_MODE)));
            assert!(linux.commands().is_empty());
        }
        // A dangling symlink is someone else's too.
        let mut linux = Linux::new();
        linux.fake.dangling.insert(PathBuf::from(CONF));
        assert_eq!(code_of(&install(18053, &linux)), RESOLVED_FOREIGN);
        assert_eq!(remove(&linux), result(REMOVE, Outcome::Unchanged));
        assert!(linux.commands().is_empty());
    }

    #[test]
    fn a_file_that_cannot_be_read_is_resolved_read_and_kept() {
        let mut linux = Linux::with_file(&ours(18053));
        linux.fake.fail_reads.insert(PathBuf::from(CONF));
        let message = format!("cannot read {CONF}: permission denied, so it was left as it is");
        let failed = |op| OpResult::failed(op, RESOLVED_READ, message.clone());
        assert_eq!(install(18053, &linux), failed(INSTALL));
        assert_eq!(remove(&linux), failed(REMOVE));
        assert_eq!(linux.file(), Some((ours(18053), OLD_MODE)));
        assert!(linux.commands().is_empty());
    }

    #[test]
    fn without_a_running_resolved_nothing_changes() {
        // Not running: `is-active` exits with 3. Also with an older file.
        for start in [None, Some(ours(18054))] {
            let linux = start.as_deref().map_or_else(Linux::new, Linux::with_file);
            linux.active.set(false);
            let message = "systemd-resolved.service is not running, so nothing was changed";
            let expected = OpResult::failed(INSTALL, RESOLVED_NOT_RUNNING, message.to_owned());
            assert_eq!(install(18053, &linux), expected);
            assert_eq!(linux.file().map(|(bytes, _)| bytes), start);
            assert_eq!(linux.commands(), [ACTIVE]);
        }
        // No `systemctl` at all: the message says so.
        let mut linux = Linux::new();
        linux.no_systemctl = true;
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_NOT_RUNNING);
        assert_eq!(
            message_of(&failed),
            "cannot check systemd-resolved.service: systemctl could not run: entity not found, \
             so nothing was changed"
        );
        assert_eq!(linux.file(), None);
    }

    /// Spike S-02, case T2b: next to another global server, `.test` never
    /// resolves. Found before anything is written.
    #[test]
    fn another_global_server_is_refused_before_the_write() {
        for (others, count) in [
            (vec!["1.1.1.1"], 1),
            (
                vec!["1.1.1.1#cloudflare-dns.com", "2606:4700:4700::1111"],
                2,
            ),
        ] {
            let mut linux = Linux::new();
            linux.others = others;
            let failed = install(18053, &linux);
            assert_eq!(code_of(&failed), RESOLVED_OTHER_DNS);
            assert_eq!(
                message_of(&failed),
                format!(
                    "systemd-resolved has {count} other global DNS server(s), so .test names \
                     would not resolve; nothing was changed"
                )
            );
            assert_eq!(linux.file(), None);
            assert_eq!(linux.commands(), [ACTIVE, LISTED]);
        }
    }

    /// systemd prints a long list on more lines. A server on a later line
    /// counts too: both the user's, and Marsa's after the restart.
    #[test]
    fn wrapped_lists_are_read_whole() {
        let mut linux = Linux::new();
        linux.wrapped = true;
        linux.others = vec!["1.1.1.1#cloudflare-dns.com", "1.0.0.1#cloudflare-dns.com"];
        assert_eq!(code_of(&install(18053, &linux)), RESOLVED_OTHER_DNS);

        // An older file of Marsa's, and another server that sorts after it.
        let mut linux = Linux::with_file(&ours(18054));
        linux.wrapped = true;
        linux.others = vec!["2001:db8:abcd:12:3456:7890:abcd:ef01#dns-over-tls.corp.example.com"];
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_OTHER_DNS);
        assert_eq!(linux.file(), None);
    }

    /// A global server turns off the fallback servers, so a machine whose
    /// network gives none would resolve nothing but `.test`.
    #[test]
    fn no_dns_server_on_any_connection_is_refused_before_the_write() {
        let mut linux = Linux::new();
        linux.link_server = false;
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_NO_LINK_DNS);
        assert!(message_of(&failed).ends_with("; nothing was changed"));
        assert_eq!(linux.file(), None);
        assert_eq!(linux.commands(), [ACTIVE, LISTED]);
    }

    #[test]
    fn a_check_that_cannot_run_changes_nothing() {
        let mut linux = Linux::new();
        linux.resolvectl_fails = true;
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_CHECK_FAILED);
        assert_eq!(
            message_of(&failed),
            "resolvectl dns exited with code 1; nothing was changed"
        );
        assert_eq!(linux.file(), None);
    }

    #[test]
    fn a_failed_write_is_resolved_write_and_nothing_restarts() {
        let mut linux = Linux::new();
        linux.fake.fail_writes.insert(PathBuf::from(CONF));
        let message = format!("cannot write {CONF}: permission denied");
        let expected = OpResult::failed(INSTALL, RESOLVED_WRITE, message);
        assert_eq!(install(18053, &linux), expected);
        assert_eq!(linux.file(), None);
        assert_eq!(linux.commands(), [ACTIVE, LISTED]);
    }

    /// The file goes before the second restart, which brings
    /// systemd-resolved back without it.
    #[test]
    fn a_failed_restart_takes_the_file_out_again() {
        let linux = Linux::new();
        linux.failing_restarts.set(1);
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_RESTART_FAILED);
        assert_eq!(
            message_of(&failed),
            format!(
                "systemctl restart systemd-resolved.service exited with code 1; {CONF} was \
                 removed again"
            )
        );
        assert_eq!(linux.file(), None);
        assert_eq!(
            linux.commands(),
            [ACTIVE, LISTED, RESET, RESTARTED, RESET, RESTARTED]
        );
        let file_at = linux.file_at.borrow();
        // The file was there at the first restart, and gone at the second.
        assert!(file_at[3] && !file_at[5]);
        assert!(linux.active.get() && linux.loaded.borrow().is_none());
    }

    /// systemd refuses the restarts, then one more start: no name resolves,
    /// and the code says so.
    #[test]
    fn a_resolved_that_stays_down_is_resolved_stopped() {
        let mut linux = Linux::new();
        linux.failing_restarts.set(2);
        linux.start_fails = true;
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_STOPPED);
        assert!(
            message_of(&failed).ends_with(
                "systemd-resolved does not run now, so no name resolves until it starts"
            )
        );
        assert_eq!(
            linux.commands(),
            [
                ACTIVE, LISTED, RESET, RESTARTED, RESET, RESTARTED, ACTIVE, RESET, STARTED, ACTIVE
            ]
        );
        assert_eq!(linux.file(), None);

        // The start works: the first error is the one reported.
        let linux = Linux::new();
        linux.failing_restarts.set(2);
        assert_eq!(code_of(&install(18053, &linux)), RESOLVED_RESTART_FAILED);
        assert!(linux.active.get());
    }

    /// The other server shows only after the restart (an older file of
    /// Marsa's hid the first check): the file goes, and the older one is
    /// not put back.
    #[test]
    fn a_failed_check_after_the_restart_takes_the_file_out_again() {
        let mut linux = Linux::with_file(&ours(18054));
        linux.others = vec!["1.1.1.1"];
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_OTHER_DNS);
        assert!(message_of(&failed).ends_with(&format!("; {CONF} was removed again")));
        assert!(!message_of(&failed).contains("1.1.1.1"));
        assert_eq!(linux.file(), None);
        assert_eq!(
            linux.commands(),
            [ACTIVE, RESET, RESTARTED, LISTED, RESET, RESTARTED]
        );
        assert!(linux.loaded.borrow().is_none());
    }

    #[test]
    fn a_resolved_that_does_not_list_our_server_is_resolved_check_failed() {
        let mut linux = Linux::new();
        linux.ignores_file = true;
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_CHECK_FAILED);
        assert_eq!(
            message_of(&failed),
            format!("systemd-resolved does not list Marsa's DNS server; {CONF} was removed again")
        );
        assert_eq!(linux.file(), None);
    }

    /// The rollback says when the file stays.
    #[test]
    fn a_file_that_cannot_be_taken_out_again_is_named() {
        let mut linux = Linux::new();
        linux.ignores_file = true;
        linux.fake.fail_removes.insert(PathBuf::from(CONF));
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_CHECK_FAILED);
        assert!(message_of(&failed).ends_with(&format!("; {CONF} could not be removed")));
        assert!(linux.file().is_some());
    }

    /// Setup run again with no network: a restart cannot give a connection
    /// a DNS server, so the working file stays, and nothing restarts.
    #[test]
    fn our_working_file_stays_when_no_connection_has_a_dns_server() {
        let mut linux = Linux::with_file(&ours(18053));
        linux.link_server = false;
        let failed = install(18053, &linux);
        assert_eq!(code_of(&failed), RESOLVED_NO_LINK_DNS);
        assert!(message_of(&failed).ends_with("; nothing was changed"));
        assert_eq!(linux.file(), Some((ours(18053), OLD_MODE)));
        assert_eq!(linux.commands(), [ACTIVE, LISTED]);
    }

    /// Marsa's server still loaded with no file (a removal whose restart
    /// failed): not another server, so the install goes on.
    #[test]
    fn our_own_server_still_loaded_is_not_another_server() {
        let linux = Linux::new();
        *linux.loaded.borrow_mut() = Some(ours(18053));
        assert_eq!(install(18053, &linux), result(INSTALL, Outcome::Done));
        assert_eq!(linux.file(), Some((ours(18053), 0o644)));
    }

    /// Both restarts fail and the old process runs on: the message says it
    /// may still use Marsa's server.
    #[test]
    fn a_failed_rollback_restart_with_the_old_process_running_says_so() {
        let linux = Linux::new();
        linux.failing_restarts.set(2);
        let old = OldProcess(linux);
        let failed = install(18053, &old);
        assert_eq!(code_of(&failed), RESOLVED_RESTART_FAILED);
        assert_eq!(
            message_of(&failed),
            format!(
                "systemctl restart systemd-resolved.service exited with code 1; {CONF} was \
                 removed again, but systemctl restart systemd-resolved.service exited with code \
                 1; systemd-resolved may use Marsa's server until it restarts"
            )
        );
        assert_eq!(
            old.0.commands(),
            [ACTIVE, LISTED, RESET, RESTARTED, RESET, RESTARTED, ACTIVE]
        );
        assert_eq!(old.0.file(), None);
    }

    #[test]
    fn remove_without_our_file_is_unchanged_and_runs_nothing() {
        for linux in [Linux::new(), Linux::with_file(FOREIGN)] {
            let before = linux.file();
            assert_eq!(remove(&linux), result(REMOVE, Outcome::Unchanged));
            assert_eq!(linux.file(), before);
            assert!(linux.commands().is_empty());
        }
    }

    #[test]
    fn remove_deletes_our_file_then_restarts() {
        // Also with another port: the marker makes it ours, not the content.
        for port in [18053, 18054] {
            let linux = Linux::with_file(&ours(port));
            assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
            assert_eq!(linux.file(), None);
            assert_eq!(linux.commands(), [ACTIVE, RESET, RESTARTED]);
            assert_eq!(*linux.file_at.borrow(), [false, false, false]);
            assert!(linux.loaded.borrow().is_none());
        }
    }

    #[test]
    fn remove_does_not_start_a_resolved_that_does_not_run() {
        let linux = Linux::with_file(&ours(18053));
        linux.active.set(false);
        assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
        assert_eq!(linux.file(), None);
        assert_eq!(linux.commands(), [ACTIVE]);
        assert!(!linux.active.get());
    }

    #[test]
    fn a_failed_delete_is_resolved_delete_and_keeps_the_file() {
        let mut linux = Linux::with_file(&ours(18053));
        linux.fake.fail_removes.insert(PathBuf::from(CONF));
        let message = format!("cannot delete {CONF}: permission denied");
        let expected = OpResult::failed(REMOVE, RESOLVED_DELETE, message);
        assert_eq!(remove(&linux), expected);
        assert_eq!(linux.file(), Some((ours(18053), OLD_MODE)));
        assert!(linux.commands().is_empty());
    }

    /// A restart that fails and leaves it stopped: one start reads the
    /// folder without Marsa's file, so the removal is done.
    #[test]
    fn a_failed_restart_after_the_delete_is_mended_by_a_start() {
        let linux = Linux::with_file(&ours(18053));
        linux.failing_restarts.set(1);
        assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
        assert_eq!(
            linux.commands(),
            [ACTIVE, RESET, RESTARTED, ACTIVE, RESET, STARTED, ACTIVE]
        );
        assert!(linux.active.get() && linux.loaded.borrow().is_none());
    }

    #[test]
    fn a_resolved_that_stays_down_after_the_delete_is_resolved_stopped() {
        let mut linux = Linux::with_file(&ours(18053));
        linux.failing_restarts.set(1);
        linux.start_fails = true;
        let failed = remove(&linux);
        assert_eq!(code_of(&failed), RESOLVED_STOPPED);
        assert_eq!(
            message_of(&failed),
            format!(
                "{CONF} was deleted, but systemctl restart systemd-resolved.service exited with \
                 code 1; systemd-resolved does not run now, so no name resolves until it starts"
            )
        );
        assert_eq!(linux.file(), None);
    }

    /// The restart fails while the old process still runs: it may still use
    /// Marsa's server, and the message says so.
    #[test]
    fn a_failed_restart_with_the_old_process_running_is_resolved_restart_failed() {
        let linux = Linux::with_file(&ours(18053));
        linux.failing_restarts.set(1);
        // This restart fails without stopping the old process.
        let still_running = OldProcess(linux);
        let failed = remove(&still_running);
        assert_eq!(code_of(&failed), RESOLVED_RESTART_FAILED);
        assert!(message_of(&failed).ends_with("may use Marsa's server until it restarts"));
        assert_eq!(
            still_running.0.commands(),
            [ACTIVE, RESET, RESTARTED, ACTIVE]
        );
    }

    /// [`Linux`], whose failed restarts leave the old process running.
    struct OldProcess(Linux);

    impl System for OldProcess {
        fn read(&self, path: &Path, max: u64) -> io::Result<Vec<u8>> {
            self.0.read(path, max)
        }

        fn exists(&self, path: &Path) -> bool {
            self.0.exists(path)
        }

        fn write_atomic(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
            self.0.write_atomic(path, bytes, mode)
        }

        fn remove(&self, path: &Path) -> io::Result<()> {
            self.0.remove(path)
        }

        fn remove_empty_dir(&self, path: &Path) -> io::Result<()> {
            self.0.remove_empty_dir(path)
        }

        fn run(&self, program: &'static str, args: &[&str]) -> io::Result<RunOutput> {
            let output = self.0.run(program, args);
            self.0.active.set(true);
            output
        }

        fn current_exe(&self) -> io::Result<PathBuf> {
            self.0.current_exe()
        }
    }

    #[test]
    fn the_output_of_resolvectl_dns() {
        let dns = |stdout: &str| read_dns(&exit(0, stdout));
        let global = |servers: &[&str]| servers.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(
            dns("Global: 127.0.0.1:18053\nLink 2 (eth0): 1.1.1.1\n"),
            Some(Dns {
                global: global(&["127.0.0.1:18053"]),
                link_server: true,
            })
        );
        // A wrapped list, a link without servers, CRLF, and the global line
        // after the links.
        let text = "Link 3 (docker0):\r\nLink 2 (eth0):\r\n        10.0.0.1\r\nGlobal: a\r\n        b c\r\n";
        assert_eq!(
            dns(text),
            Some(Dns {
                global: global(&["a", "b", "c"]),
                link_server: true,
            })
        );
        assert_eq!(
            dns("Global:\nLink 2 (eth0):\nLink 3 (lo):\n"),
            Some(Dns {
                global: Vec::new(),
                link_server: false,
            })
        );
        // Only at the start of a line, and only with exit code 0.
        assert_eq!(dns(" Global: 127.0.0.1:18053\n"), None);
        assert_eq!(dns("Link 2 (Global:): 1.1.1.1\n"), None);
        assert_eq!(dns(""), None);
        assert_eq!(read_dns(&exit(1, "Global: 127.0.0.1:18053\n")), None);
        assert_eq!(read_dns(&Err(io::ErrorKind::NotFound.into())), None);
    }

    /// Through `apply`, as a real request: Linux only.
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
        assert_eq!(linux.file(), None);

        for os in [Os::Macos, Os::Windows] {
            let linux = Linux::new();
            let results = apply(&request, os, &linux);
            let codes: Vec<_> = results.iter().map(code_of).collect();
            assert_eq!(codes, [crate::response::code::OP_NOT_SUPPORTED; 2]);
            assert!(linux.commands().is_empty() && linux.file().is_none());
        }
    }
}
