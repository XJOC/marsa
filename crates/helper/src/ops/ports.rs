//! Linux: ports 80 and 443 for Caddy, through the sysctl
//! `net.ipv4.ip_unprivileged_port_start` (ADR 0014).
//!
//! Linux lets only root bind a port below this value, 1024 by default. The
//! helper sets it to 80 with its own file in `/etc/sysctl.d`, which the
//! system loads again at every boot. Caddy then binds `127.0.0.1:80` and
//! `:443` itself, as a normal process. Not `setcap` on the Caddy file: the
//! daemon could then no longer read `/proc/<pid>/exe` of its own Caddy, and
//! every Caddy update would need the helper again.
//!
//! The cost: every local program may then bind ports 80 to 1023, not only
//! Caddy. `marsa setup` says so before it asks for the password.
//!
//! Nothing comes from the request: the file, the key, and the value are
//! constants here. The helper changes the file only if Marsa wrote it
//! ([`marked`]), and it writes none on a machine whose owner already allows
//! port 80.
//!
//! No message holds the content of a file or the output of `sysctl`. One
//! message names the port number that the kernel's value file holds.

use std::io;
use std::path::Path;

use crate::ops::marked::{self, Current, MARKER};
use crate::ops::program;
use crate::response::{OpResult, Outcome};
use crate::system::System;

/// A file at [`FILE`] that Marsa did not write, so it was left as it is.
pub const PORTS_FOREIGN: &str = "ports_foreign";
/// [`FILE`] cannot be read, or `ports_install` cannot read the current
/// value. Nothing was changed.
pub const PORTS_READ: &str = "ports_read";
/// Writing [`FILE`] failed.
pub const PORTS_WRITE: &str = "ports_write";
/// `sysctl -p` failed, or port 80 is still not allowed after it. Marsa's
/// file is removed again, so it cannot take effect at a later boot.
pub const PORTS_APPLY_FAILED: &str = "ports_apply_failed";
/// Deleting [`FILE`] failed. Nothing was changed.
pub const PORTS_DELETE: &str = "ports_delete";
/// [`FILE`] is deleted, but the value could not be set back to the default.
/// It stays as it is until the next boot.
pub const PORTS_RESTORE_FAILED: &str = "ports_restore_failed";

/// Marsa's own sysctl file. The system loads this folder at every boot.
const FILE: &str = "/etc/sysctl.d/60-marsa.conf";
/// Everyone can read it, and only root can change it.
const MODE: u32 = 0o644;
const KEY: &str = "net.ipv4.ip_unprivileged_port_start";
/// Where the kernel shows the value of [`KEY`].
const VALUE_FILE: &str = "/proc/sys/net/ipv4/ip_unprivileged_port_start";
/// The value Marsa sets: port 80 for HTTP, so port 443 too.
const WANTED: u32 = 80;
/// The kernel's own value.
const DEFAULT: u32 = 1024;
const SYSCTL: &str = "/usr/sbin/sysctl";
/// For `sysctl --pattern`: only [`KEY`] is loaded again from the other
/// files, not every setting that someone changed by hand since the boot.
///
/// procps 3.3 (Ubuntu 22.04) matches the pattern against the key alone;
/// procps 4 (Ubuntu 24.04, Fedora) against the whole line, `key = value`.
/// So the key may be followed by its end or by spaces and `=`, and may start
/// with `-` (a line whose errors are ignored). A file may write the key with
/// `/` in place of `.`.
const ONLY_KEY: &str = r"^-?net[./]ipv4[./]ip_unprivileged_port_start[[:space:]]*(=|$)";
/// The largest value file read: a port number and a line break.
const MAX_VALUE_BYTES: u64 = 16;

const INSTALL: &str = "ports_install";
const REMOVE: &str = "ports_remove";

/// `ports_install`: a normal process may bind port 80 and up.
pub fn install(system: &impl System) -> OpResult {
    let content = content();
    let ours = match marked::current(system, Path::new(FILE)) {
        Current::Missing => None,
        Current::Ours(bytes) => Some(bytes),
        Current::Foreign => {
            // The path only: the content of another tool's file stays out.
            let message = format!("{FILE} was not written by Marsa, so it was left as it is");
            return OpResult::failed(INSTALL, PORTS_FOREIGN, message);
        }
        Current::Unreadable(kind) => return unreadable(INSTALL, kind),
    };
    let first_allowed = match first_allowed_port(system) {
        Ok(port) => port,
        Err(message) => return OpResult::failed(INSTALL, PORTS_READ, message),
    };
    let in_place = ours.as_deref() == Some(content.as_bytes());
    let allowed = first_allowed <= WANTED;
    // Port 80 is allowed already: by Marsa's file, or by the owner of the
    // machine. Then Marsa writes no file, so it has nothing to undo later.
    if allowed && (in_place || ours.is_none()) {
        return result(INSTALL, Outcome::Unchanged);
    }
    if !in_place && let Err(err) = system.write_atomic(Path::new(FILE), content.as_bytes(), MODE) {
        let message = format!("cannot write {FILE}: {}", err.kind());
        return OpResult::failed(INSTALL, PORTS_WRITE, message);
    }
    // Marsa's file had other content, and is right again for the next boot.
    // No load now: it could only raise a lower value that someone else set.
    if allowed {
        return result(INSTALL, Outcome::Done);
    }
    let loaded = system.run(SYSCTL, &["-p", FILE]);
    let reason = if program::succeeded(&loaded) {
        // Exit code 0 alone is not trusted: the daemon picks Caddy's ports
        // by this value.
        match first_allowed_port(system) {
            Ok(port) if port <= WANTED => return result(INSTALL, Outcome::Done),
            Ok(port) => format!("sysctl -p {FILE} ran, but {KEY} is {port}"),
            Err(message) => message,
        }
    } else {
        format!("sysctl -p {FILE} {}", program::failure(&loaded))
    };
    // A file that is not loaded now would still take effect at the next
    // boot, without a word. So Marsa's file goes, also one that was there
    // before this call.
    let removed = system.remove(Path::new(FILE)).is_ok();
    let cleanup = if removed {
        "was removed again"
    } else {
        "could not be removed"
    };
    // A `sysctl` that failed may still have set the key. With the file
    // still there, it is set again at every boot too.
    let value = match first_allowed_port(system) {
        Ok(port) if port <= WANTED && removed => "; port 80 stays allowed until the next boot",
        Ok(port) if port <= WANTED => "; port 80 stays allowed",
        _ => "",
    };
    let message = format!("{reason}; {FILE} {cleanup}{value}");
    OpResult::failed(INSTALL, PORTS_APPLY_FAILED, message)
}

/// `ports_remove`: deletes Marsa's file and puts the kernel's value back.
/// Without the file, nothing here is Marsa's: the value is not touched.
pub fn remove(system: &impl System) -> OpResult {
    match marked::current(system, Path::new(FILE)) {
        Current::Missing | Current::Foreign => return result(REMOVE, Outcome::Unchanged),
        Current::Unreadable(kind) => return unreadable(REMOVE, kind),
        Current::Ours(_) => {}
    }
    // The file first: if this fails, nothing has changed and a retry starts
    // from the same state.
    if let Err(err) = system.remove(Path::new(FILE)) {
        let message = format!("cannot delete {FILE}: {}", err.kind());
        return OpResult::failed(REMOVE, PORTS_DELETE, message);
    }
    // Then the kernel's own value. `sysctl --system` alone would not do it:
    // it sets only the keys that a file names.
    let reset = format!("{KEY}={DEFAULT}");
    let back = system.run(SYSCTL, &["-w", &reset]);
    if !program::succeeded(&back) {
        let message = format!(
            "{FILE} was deleted, but sysctl -w {reset} {}; the value stays until the next boot",
            program::failure(&back)
        );
        return OpResult::failed(REMOVE, PORTS_RESTORE_FAILED, message);
    }
    // Then this key from every other sysctl file again: one of them may set
    // it too, and that setting is not Marsa's to drop. The result is not
    // checked: `sysctl --system` also fails for a bad line in another
    // program's file, which is not Marsa's to report.
    let _ = system.run(SYSCTL, &["--pattern", ONLY_KEY, "--system"]);
    result(REMOVE, Outcome::Done)
}

/// LF only, with a final newline.
fn content() -> String {
    format!("{MARKER}\n{KEY}={WANTED}\n")
}

/// The lowest port a normal process may bind now, or why that is not known.
fn first_allowed_port(system: &impl System) -> Result<u32, String> {
    let bytes = system
        .read(Path::new(VALUE_FILE), MAX_VALUE_BYTES)
        .map_err(|err| format!("cannot read {VALUE_FILE}: {}", err.kind()))?;
    std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| text.trim_ascii().parse().ok())
        .ok_or_else(|| format!("{VALUE_FILE} does not hold a port number"))
}

fn unreadable(op: &'static str, kind: io::ErrorKind) -> OpResult {
    let message = format!("cannot read {FILE}: {kind}, so it was left as it is");
    OpResult::failed(op, PORTS_READ, message)
}

fn result(op: &'static str, outcome: Outcome) -> OpResult {
    OpResult { op, outcome }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::ops::apply;
    use crate::os::Os;
    use crate::request::Request;
    use crate::system::{FakeSystem, RunOutput};

    // Written out again, not taken from the constants above: a test must
    // fail if one of those changes.
    const PROGRAM: &str = "/usr/sbin/sysctl";
    const CONF: &str = "/etc/sysctl.d/60-marsa.conf";
    const PROC: &str = "/proc/sys/net/ipv4/ip_unprivileged_port_start";
    const RESET: &str = "net.ipv4.ip_unprivileged_port_start=1024";
    const RELOAD: [&str; 3] = [
        "--pattern",
        "^-?net[./]ipv4[./]ip_unprivileged_port_start[[:space:]]*(=|$)",
        "--system",
    ];
    const OURS: &[u8] =
        b"# Managed by Marsa. Do not edit.\nnet.ipv4.ip_unprivileged_port_start=80\n";

    /// The mode of a file a test starts with. Marsa never writes it, so the
    /// mode shows whether an op rewrote the file.
    const OLD_MODE: u32 = 0o600;
    /// Another tool's file at the same path.
    const FOREIGN: &[u8] = b"net.ipv4.ip_unprivileged_port_start=443\n";

    /// A Linux whose `sysctl` changes the value file, as the real one does.
    /// Everything else goes straight to the [`FakeSystem`] inside.
    struct Linux {
        fake: FakeSystem,
        /// What the value file holds after `sysctl -p <our file>`.
        after_load: &'static str,
        /// The value in another file of `/etc/sysctl.d`, if one sets the key.
        other_file: Option<u32>,
        /// A `sysctl -p` that sets the key and then fails all the same.
        load_sets_then_fails: bool,
    }

    impl Linux {
        /// A machine where normal processes may bind from port `first` up,
        /// with a working `sysctl` and nothing at Marsa's path.
        fn new(first: u32) -> Self {
            let mut fake = FakeSystem::default();
            fake.defaults.insert(PROGRAM, exit(0));
            let linux = Self {
                fake,
                after_load: "80\n",
                other_file: None,
                load_sets_then_fails: false,
            };
            linux.set(PROC, format!("{first}\n").as_bytes());
            linux
        }

        fn with_file(first: u32, bytes: &[u8]) -> Self {
            let linux = Self::new(first);
            linux.set(CONF, bytes);
            linux
        }

        fn set(&self, path: &str, bytes: &[u8]) {
            let file = (bytes.to_vec(), OLD_MODE);
            self.fake
                .files
                .borrow_mut()
                .insert(PathBuf::from(path), file);
        }

        /// The file at Marsa's path, or `None`.
        fn file(&self) -> Option<(Vec<u8>, u32)> {
            self.fake.files.borrow().get(Path::new(CONF)).cloned()
        }

        /// The value file, as text.
        fn value(&self) -> String {
            let files = self.fake.files.borrow();
            String::from_utf8(files[Path::new(PROC)].0.clone()).unwrap()
        }

        /// The arguments of every command, which must all be `sysctl`.
        fn commands(&self) -> Vec<Vec<String>> {
            let log = self.fake.log.borrow();
            assert!(log.iter().all(|(program, _)| program == PROGRAM));
            log.iter().map(|(_, args)| args.clone()).collect()
        }
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
            let output = self.fake.run(program, args)?;
            let loads = matches!(args, ["-p", CONF]);
            if output.code != Some(0) && !(loads && self.load_sets_then_fails) {
                return Ok(output);
            }
            let value = match args {
                // Both load the file at Marsa's path only while it is there.
                ["-p", CONF] if self.file().is_some() => Some(self.after_load.to_owned()),
                [..] if args == RELOAD && self.file().is_some() => Some("80\n".to_owned()),
                [..] if args == RELOAD => self.other_file.map(|value| format!("{value}\n")),
                ["-w", RESET] => Some("1024\n".to_owned()),
                _ => None,
            };
            if let Some(value) = value {
                self.set(PROC, value.as_bytes());
            }
            Ok(output)
        }

        fn current_exe(&self) -> io::Result<PathBuf> {
            self.fake.current_exe()
        }
    }

    fn exit(code: i32) -> RunOutput {
        RunOutput {
            code: Some(code),
            ..RunOutput::default()
        }
    }

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn load() -> Vec<String> {
        args(&["-p", CONF])
    }

    fn failed(op: &'static str, code: &'static str, message: &str) -> OpResult {
        OpResult::failed(op, code, message.to_owned())
    }

    #[test]
    fn install_writes_the_file_and_loads_it() {
        for first in [81, 443, 1024] {
            let linux = Linux::new(first);
            assert_eq!(install(&linux), result(INSTALL, Outcome::Done), "{first}");
            assert_eq!(linux.file(), Some((OURS.to_vec(), 0o644)));
            assert_eq!(linux.commands(), [load()]);
            assert_eq!(linux.value(), "80\n");
        }
    }

    /// The snapshot does not show line endings, so they are checked here.
    #[test]
    fn the_written_file() {
        let linux = Linux::new(1024);
        assert_eq!(install(&linux).outcome, Outcome::Done);
        let (bytes, mode) = linux.file().unwrap();
        assert_eq!(mode, 0o644);
        assert!(bytes.ends_with(b"=80\n") && !bytes.contains(&b'\r'));
        let text = String::from_utf8(bytes).unwrap();
        insta::assert_snapshot!(text);
    }

    /// The owner of the machine set the value: Marsa adds nothing of its own.
    #[test]
    fn install_where_port_80_is_allowed_already_writes_no_file() {
        for first in [0, 79, 80] {
            let linux = Linux::new(first);
            assert_eq!(
                install(&linux),
                result(INSTALL, Outcome::Unchanged),
                "{first}"
            );
            assert_eq!(linux.file(), None);
            assert!(linux.commands().is_empty());
            assert_eq!(linux.value(), format!("{first}\n"));
        }
    }

    #[test]
    fn install_over_our_loaded_file_is_unchanged() {
        let mut linux = Linux::with_file(80, OURS);
        // Any write fails here, so `unchanged` also shows that none was tried.
        linux.fake.fail_writes.insert(PathBuf::from(CONF));
        assert_eq!(install(&linux), result(INSTALL, Outcome::Unchanged));
        assert_eq!(linux.file(), Some((OURS.to_vec(), OLD_MODE)));
        assert!(linux.commands().is_empty());
    }

    /// Someone set the value back by hand: the file is loaded again, and not
    /// rewritten.
    #[test]
    fn install_loads_our_file_again_when_the_value_went_back() {
        for first in [81, 1024] {
            let linux = Linux::with_file(first, OURS);
            assert_eq!(install(&linux), result(INSTALL, Outcome::Done), "{first}");
            assert_eq!(linux.file(), Some((OURS.to_vec(), OLD_MODE)));
            assert_eq!(linux.commands(), [load()]);
            assert_eq!(linux.value(), "80\n");
        }
    }

    /// A file of Marsa's from another version, or edited by hand.
    const OLD_OURS: &[u8] =
        b"# Managed by Marsa. Do not edit.\nnet.ipv4.ip_unprivileged_port_start=443\n";

    #[test]
    fn install_rewrites_and_loads_our_file_with_other_content() {
        for first in [81, 443] {
            let linux = Linux::with_file(first, OLD_OURS);
            assert_eq!(install(&linux), result(INSTALL, Outcome::Done), "{first}");
            assert_eq!(linux.file(), Some((OURS.to_vec(), 0o644)));
            assert_eq!(linux.commands(), [load()]);
            assert_eq!(linux.value(), "80\n");
        }
    }

    /// Where port 80 is allowed already, the file is put right and nothing
    /// is loaded: a load would raise a lower value that someone else set.
    #[test]
    fn install_only_rewrites_our_file_where_port_80_is_allowed_already() {
        for first in [0, 79, 80] {
            let linux = Linux::with_file(first, OLD_OURS);
            assert_eq!(install(&linux), result(INSTALL, Outcome::Done), "{first}");
            assert_eq!(linux.file(), Some((OURS.to_vec(), 0o644)));
            assert!(linux.commands().is_empty());
            assert_eq!(linux.value(), format!("{first}\n"));
        }
    }

    /// The exact message also shows that it holds the path and no content.
    #[test]
    fn install_fails_on_a_foreign_file_and_keeps_it() {
        let linux = Linux::with_file(1024, FOREIGN);
        let message = format!("{CONF} was not written by Marsa, so it was left as it is");
        assert_eq!(install(&linux), failed(INSTALL, PORTS_FOREIGN, &message));
        assert_eq!(linux.file(), Some((FOREIGN.to_vec(), OLD_MODE)));
        assert!(linux.commands().is_empty());
        assert_eq!(linux.value(), "1024\n");
    }

    #[test]
    fn a_file_that_cannot_be_read_is_ports_read_and_kept() {
        let mut linux = Linux::with_file(80, OURS);
        linux.fake.fail_reads.insert(PathBuf::from(CONF));
        let message = format!("cannot read {CONF}: permission denied, so it was left as it is");
        assert_eq!(install(&linux), failed(INSTALL, PORTS_READ, &message));
        assert_eq!(remove(&linux), failed(REMOVE, PORTS_READ, &message));
        assert_eq!(linux.file(), Some((OURS.to_vec(), OLD_MODE)));
        assert!(linux.commands().is_empty());
        assert_eq!(linux.value(), "80\n");
    }

    #[test]
    fn install_without_a_value_it_can_read_changes_nothing() {
        let no_number = format!("{PROC} does not hold a port number");
        let values: [(Option<&[u8]>, String); 4] = [
            (None, format!("cannot read {PROC}: entity not found")),
            (Some(b"abc\n"), no_number.clone()),
            (Some(b"-1\n"), no_number.clone()),
            (Some(b"\xff\n"), no_number),
        ];
        for (value, message) in values {
            let linux = Linux::new(1024);
            match value {
                Some(bytes) => linux.set(PROC, bytes),
                None => drop(linux.fake.files.borrow_mut().remove(Path::new(PROC))),
            }
            assert_eq!(install(&linux), failed(INSTALL, PORTS_READ, &message));
            assert_eq!(linux.file(), None);
            assert!(linux.commands().is_empty());
        }
    }

    /// Also where port 80 is allowed already and only the file is put
    /// right: a failed write is not `done`.
    #[test]
    fn a_failed_rewrite_where_port_80_is_allowed_is_ports_write() {
        let mut linux = Linux::with_file(80, OLD_OURS);
        linux.fake.fail_writes.insert(PathBuf::from(CONF));
        let message = format!("cannot write {CONF}: permission denied");
        assert_eq!(install(&linux), failed(INSTALL, PORTS_WRITE, &message));
        assert_eq!(linux.file(), Some((OLD_OURS.to_vec(), OLD_MODE)));
        assert!(linux.commands().is_empty());
        assert_eq!(linux.value(), "80\n");
    }

    #[test]
    fn a_failed_write_is_ports_write_and_nothing_is_loaded() {
        let mut linux = Linux::new(1024);
        linux.fake.fail_writes.insert(PathBuf::from(CONF));
        let message = format!("cannot write {CONF}: permission denied");
        assert_eq!(install(&linux), failed(INSTALL, PORTS_WRITE, &message));
        assert!(linux.commands().is_empty());
        assert_eq!(linux.value(), "1024\n");
    }

    /// Also a file that was there before the call: it must not take effect
    /// at a later boot, after an install that said it failed.
    #[test]
    fn a_failed_load_removes_our_file() {
        let message = format!("sysctl -p {CONF} exited with code 255; {CONF} was removed again");
        for mut linux in [Linux::new(1024), Linux::with_file(1024, OURS)] {
            linux.fake.script(PROGRAM, &["-p", CONF], exit(255));
            assert_eq!(
                install(&linux),
                failed(INSTALL, PORTS_APPLY_FAILED, &message)
            );
            assert_eq!(linux.file(), None);
            assert_eq!(linux.commands(), [load()]);
            assert_eq!(linux.value(), "1024\n");
        }

        // No `sysctl` program at all.
        let mut linux = Linux::new(1024);
        linux.fake.defaults.clear();
        let message =
            format!("sysctl -p {CONF} could not run: entity not found; {CONF} was removed again");
        assert_eq!(
            install(&linux),
            failed(INSTALL, PORTS_APPLY_FAILED, &message)
        );
        assert_eq!(linux.file(), None);
    }

    /// The value is read again after the load: exit code 0 alone is not
    /// trusted.
    #[test]
    fn a_load_that_does_not_allow_port_80_is_a_failure() {
        let key = "net.ipv4.ip_unprivileged_port_start";
        let cases = [
            ("1024\n", format!("sysctl -p {CONF} ran, but {key} is 1024")),
            ("81\n", format!("sysctl -p {CONF} ran, but {key} is 81")),
            ("junk\n", format!("{PROC} does not hold a port number")),
        ];
        for (after_load, reason) in cases {
            for mut linux in [Linux::new(1024), Linux::with_file(1024, OURS)] {
                linux.after_load = after_load;
                let message = format!("{reason}; {CONF} was removed again");
                assert_eq!(
                    install(&linux),
                    failed(INSTALL, PORTS_APPLY_FAILED, &message)
                );
                assert_eq!(linux.file(), None);
            }
        }
    }

    /// The file is gone, but the running value is not Marsa's to guess.
    #[test]
    fn a_load_that_set_the_key_and_then_failed_says_that_the_value_stays() {
        let mut linux = Linux::new(1024);
        linux.fake.script(PROGRAM, &["-p", CONF], exit(255));
        linux.load_sets_then_fails = true;
        let message = format!(
            "sysctl -p {CONF} exited with code 255; {CONF} was removed again; \
             port 80 stays allowed until the next boot"
        );
        assert_eq!(
            install(&linux),
            failed(INSTALL, PORTS_APPLY_FAILED, &message)
        );
        assert_eq!(linux.file(), None);
        assert_eq!(linux.value(), "80\n");
    }

    /// The file stays, so the value comes back at every boot as well.
    #[test]
    fn a_failed_load_whose_file_stays_says_port_80_stays_allowed() {
        let mut linux = Linux::new(1024);
        linux.fake.script(PROGRAM, &["-p", CONF], exit(255));
        linux.fake.fail_removes.insert(PathBuf::from(CONF));
        linux.load_sets_then_fails = true;
        let message = format!(
            "sysctl -p {CONF} exited with code 255; {CONF} could not be removed; \
             port 80 stays allowed"
        );
        assert_eq!(
            install(&linux),
            failed(INSTALL, PORTS_APPLY_FAILED, &message)
        );
    }

    #[test]
    fn a_file_that_cannot_be_removed_after_a_failed_load_is_named() {
        let mut linux = Linux::new(1024);
        linux.fake.script(PROGRAM, &["-p", CONF], exit(1));
        linux.fake.fail_removes.insert(PathBuf::from(CONF));
        let message = format!("sysctl -p {CONF} exited with code 1; {CONF} could not be removed");
        assert_eq!(
            install(&linux),
            failed(INSTALL, PORTS_APPLY_FAILED, &message)
        );
        assert_eq!(linux.file(), Some((OURS.to_vec(), 0o644)));
    }

    #[test]
    fn remove_without_our_file_changes_nothing() {
        for linux in [Linux::new(80), Linux::with_file(80, FOREIGN)] {
            let before = linux.file();
            assert_eq!(remove(&linux), result(REMOVE, Outcome::Unchanged));
            assert_eq!(linux.file(), before);
            assert!(linux.commands().is_empty());
            assert_eq!(linux.value(), "80\n");
        }
    }

    /// The fake's `sysctl --system` loads Marsa's file while it is there, so
    /// the value at the end also shows that the delete came first.
    #[test]
    fn remove_deletes_the_file_sets_the_default_and_reloads_the_rest() {
        for first in [0, 80, 443, 1024] {
            let linux = Linux::with_file(first, OURS);
            assert_eq!(remove(&linux), result(REMOVE, Outcome::Done), "{first}");
            assert_eq!(linux.file(), None);
            assert_eq!(linux.commands(), [args(&["-w", RESET]), args(&RELOAD)]);
            assert_eq!(linux.value(), "1024\n");
        }
    }

    #[test]
    fn another_file_that_sets_the_key_applies_again() {
        let mut linux = Linux::with_file(80, OURS);
        linux.other_file = Some(80);
        assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
        assert_eq!(linux.file(), None);
        assert_eq!(linux.value(), "80\n");
    }

    /// `ports_remove` never reads the value: a value file that is missing
    /// must not keep Marsa's file on the machine.
    #[test]
    fn remove_does_not_need_the_value() {
        let linux = Linux::with_file(80, OURS);
        linux.fake.files.borrow_mut().remove(Path::new(PROC));
        assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
        assert_eq!(linux.file(), None);
        assert_eq!(linux.commands().len(), 2);
    }

    /// Nothing has changed by then, so a retry does the whole removal.
    #[test]
    fn a_failed_delete_is_ports_delete_and_changes_nothing() {
        let mut linux = Linux::with_file(80, OURS);
        linux.other_file = Some(80);
        linux.fake.fail_removes.insert(PathBuf::from(CONF));
        let message = format!("cannot delete {CONF}: permission denied");
        assert_eq!(remove(&linux), failed(REMOVE, PORTS_DELETE, &message));
        assert_eq!(linux.file(), Some((OURS.to_vec(), OLD_MODE)));
        assert!(linux.commands().is_empty());
        assert_eq!(linux.value(), "80\n");

        linux.fake.fail_removes.clear();
        assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
        assert_eq!(linux.file(), None);
        assert_eq!(linux.commands().len(), 2);
        // The other file's value is back, as without the failure.
        assert_eq!(linux.value(), "80\n");
    }

    #[test]
    fn a_failed_reset_is_ports_restore_failed_and_says_what_stays() {
        let mut linux = Linux::with_file(80, OURS);
        linux.fake.script(PROGRAM, &["-w", RESET], exit(1));
        let message = format!(
            "{CONF} was deleted, but sysctl -w {RESET} exited with code 1; \
             the value stays until the next boot"
        );
        assert_eq!(
            remove(&linux),
            failed(REMOVE, PORTS_RESTORE_FAILED, &message)
        );
        assert_eq!(linux.file(), None);
        assert_eq!(linux.commands(), [args(&["-w", RESET])]);
        assert_eq!(linux.value(), "80\n");
    }

    #[test]
    fn a_failed_reload_of_the_other_files_does_not_fail_the_removal() {
        let mut linux = Linux::with_file(80, OURS);
        linux.fake.script(PROGRAM, &RELOAD, exit(255));
        assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
        assert_eq!(linux.file(), None);
        assert_eq!(linux.value(), "1024\n");
    }

    /// The kernel's file reports a size of 0 and still holds the number. Read
    /// through the real `System`, on the one system that has the file.
    // `#[cfg]`: `/proc/sys` exists on Linux only.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_real_value_file_holds_a_port_number() {
        let port = first_allowed_port(&crate::system::RealSystem);
        assert!(port.as_ref().is_ok_and(|&port| port <= 65535), "{port:?}");
    }

    /// Through `apply`, as a real request: Linux only.
    #[test]
    fn the_ops_run_from_a_request_on_linux_only() {
        let ops = json!([{ "op": "ports_install" }, { "op": "ports_remove" }]);
        let raw = json!({ "version": 1, "ops": ops }).to_string();
        let request = Request::from_arg(OsStr::new(&raw)).unwrap();
        let linux = Linux::new(1024);
        let expected = [
            result("ports_install", Outcome::Done),
            result("ports_remove", Outcome::Done),
        ];
        assert_eq!(apply(&request, Os::Linux, &linux), expected);
        assert_eq!(linux.file(), None);
        assert_eq!(linux.value(), "1024\n");

        for os in [Os::Macos, Os::Windows] {
            let linux = Linux::new(1024);
            let results = apply(&request, os, &linux);
            assert_eq!(results.len(), 2);
            assert!(results.iter().all(|result| matches!(
                &result.outcome,
                Outcome::Failed { code, .. } if *code == crate::response::code::OP_NOT_SUPPORTED
            )));
            assert!(linux.commands().is_empty() && linux.file().is_none());
        }
    }
}
