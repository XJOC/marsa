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
//! No message holds the content of a file or the output of `sysctl`.

use std::io;
use std::path::Path;

use crate::ops::marked::{self, Current, MARKER};
use crate::ops::program;
use crate::response::{OpResult, Outcome};
use crate::system::System;

/// A file at [`FILE`] that Marsa did not write, so it was left as it is.
pub const PORTS_FOREIGN: &str = "ports_foreign";
/// [`FILE`] or the current value cannot be read, so nothing was changed.
pub const PORTS_READ: &str = "ports_read";
/// Writing [`FILE`] failed.
pub const PORTS_WRITE: &str = "ports_write";
/// `sysctl -p` failed, or the value did not change. A file that this call
/// wrote is removed again.
pub const PORTS_APPLY_FAILED: &str = "ports_apply_failed";
/// The value could not be set back to the default. [`FILE`] stays, so a
/// retry can finish.
pub const PORTS_RESTORE_FAILED: &str = "ports_restore_failed";
/// Deleting [`FILE`] failed.
pub const PORTS_DELETE: &str = "ports_delete";

/// Marsa's own sysctl file. The system loads this folder at every boot.
const FILE: &str = "/etc/sysctl.d/60-marsa.conf";
/// Everyone can read it, and only root can change it.
const MODE: u32 = 0o644;
const KEY: &str = "net.ipv4.ip_unprivileged_port_start";
/// Where the kernel shows the value of [`KEY`]: the lowest port a normal
/// process may bind.
const VALUE: &str = "/proc/sys/net/ipv4/ip_unprivileged_port_start";
/// The value Marsa sets: port 80 for HTTP, so port 443 too.
const LOWEST: u32 = 80;
/// The kernel's own value.
const DEFAULT: u32 = 1024;
const SYSCTL: &str = "/usr/sbin/sysctl";
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
    let lowest = match lowest_port(system) {
        Ok(port) => port,
        Err(message) => return OpResult::failed(INSTALL, PORTS_READ, message),
    };
    let in_place = ours.as_deref() == Some(content.as_bytes());
    // Port 80 is allowed already: by Marsa's file, or by the owner of the
    // machine. Then Marsa writes no file, so it has nothing to undo later.
    if lowest <= LOWEST && (in_place || ours.is_none()) {
        return result(INSTALL, Outcome::Unchanged);
    }
    if !in_place && let Err(err) = system.write_atomic(Path::new(FILE), content.as_bytes(), MODE) {
        let message = format!("cannot write {FILE}: {}", err.kind());
        return OpResult::failed(INSTALL, PORTS_WRITE, message);
    }
    let loaded = system.run(SYSCTL, &["-p", FILE]);
    let reason = match (program::succeeded(&loaded), lowest_port(system)) {
        (true, Ok(port)) if port <= LOWEST => return result(INSTALL, Outcome::Done),
        (true, Ok(port)) => format!("sysctl -p {FILE} ran, but {KEY} is {port}"),
        (true, Err(message)) => message,
        (false, _) => format!("sysctl -p {FILE} {}", program::failure(&loaded)),
    };
    // A file that was not loaded would take effect at the next boot, without
    // a word. So the file this call wrote goes again. Best effort: the
    // failed load is the error to report.
    if !in_place {
        let _ = system.remove(Path::new(FILE));
    }
    OpResult::failed(INSTALL, PORTS_APPLY_FAILED, reason)
}

/// `ports_remove`: deletes Marsa's file and undoes its value. Without the
/// file, nothing here is Marsa's: the value is not touched.
pub fn remove(system: &impl System) -> OpResult {
    match marked::current(system, Path::new(FILE)) {
        Current::Missing | Current::Foreign => return result(REMOVE, Outcome::Unchanged),
        Current::Unreadable(kind) => return unreadable(REMOVE, kind),
        Current::Ours(_) => {}
    }
    let lowest = match lowest_port(system) {
        Ok(port) => port,
        Err(message) => return OpResult::failed(REMOVE, PORTS_READ, message),
    };
    // Only the value of Marsa's file is undone. Any other value was set by
    // someone else after the install, and stays.
    let undo = lowest == LOWEST;
    if undo {
        // Before the file goes, so a retry after a failure still finds it.
        let reset = format!("{KEY}={DEFAULT}");
        let back = system.run(SYSCTL, &["-w", &reset]);
        if !program::succeeded(&back) {
            let message = format!("sysctl -w {reset} {}", program::failure(&back));
            return OpResult::failed(REMOVE, PORTS_RESTORE_FAILED, message);
        }
    }
    if let Err(err) = system.remove(Path::new(FILE)) {
        let message = format!("cannot delete {FILE}: {}", err.kind());
        return OpResult::failed(REMOVE, PORTS_DELETE, message);
    }
    if undo {
        // Every other sysctl file again: one of them may set the same key,
        // and that setting is not Marsa's to drop. The result is not
        // checked: `sysctl --system` also fails for a bad line in another
        // program's file, and the key is back at the kernel's value either
        // way.
        let _ = system.run(SYSCTL, &["--system"]);
    }
    result(REMOVE, Outcome::Done)
}

/// LF only, with a final newline.
fn content() -> String {
    format!("{MARKER}\n{KEY}={LOWEST}\n")
}

/// The lowest port a normal process may bind now, or why that is not known.
fn lowest_port(system: &impl System) -> Result<u32, String> {
    let bytes = system
        .read(Path::new(VALUE), MAX_VALUE_BYTES)
        .map_err(|err| format!("cannot read {VALUE}: {}", err.kind()))?;
    std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| text.trim_ascii().parse().ok())
        .ok_or_else(|| format!("{VALUE} does not hold a port number"))
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
    use std::cell::Cell;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::ops::apply;
    use crate::os::Os;
    use crate::request::Request;
    use crate::system::{FakeSystem, RunOutput};

    /// The mode of a file a test starts with. Marsa never writes it, so the
    /// mode shows whether an op rewrote the file.
    const OLD_MODE: u32 = 0o600;
    /// Another tool's file at the same path.
    const FOREIGN: &[u8] = b"net.ipv4.ip_unprivileged_port_start=443\n";

    /// A Linux whose `sysctl` changes the value file, as the real one does.
    /// Everything else goes straight to the [`FakeSystem`] inside.
    struct Linux {
        fake: FakeSystem,
        /// What `sysctl --system` sets the key to: `Some` when another file
        /// in `/etc/sysctl.d` sets it too.
        other_file: Option<u32>,
        /// A `sysctl` that exits with 0 and changes nothing.
        inert: Cell<bool>,
    }

    impl Linux {
        /// A machine where the lowest free port is `lowest`, with a working
        /// `sysctl` and no file of Marsa's.
        fn new(lowest: u32) -> Self {
            let mut fake = FakeSystem::default();
            fake.defaults.insert(SYSCTL, exit(0));
            let linux = Self {
                fake,
                other_file: None,
                inert: Cell::new(false),
            };
            linux.set(VALUE, format!("{lowest}\n").as_bytes());
            linux
        }

        fn with_file(lowest: u32, bytes: &[u8]) -> Self {
            let linux = Self::new(lowest);
            linux.set(FILE, bytes);
            linux
        }

        fn set(&self, path: &str, bytes: &[u8]) {
            let file = (bytes.to_vec(), OLD_MODE);
            self.fake
                .files
                .borrow_mut()
                .insert(PathBuf::from(path), file);
        }

        /// Marsa's file, or `None`.
        fn file(&self) -> Option<(Vec<u8>, u32)> {
            self.fake.files.borrow().get(Path::new(FILE)).cloned()
        }

        fn lowest(&self) -> String {
            let files = self.fake.files.borrow();
            String::from_utf8(files[Path::new(VALUE)].0.clone()).unwrap()
        }

        /// The arguments of every command, which must all be `sysctl`.
        fn commands(&self) -> Vec<Vec<String>> {
            let log = self.fake.log.borrow();
            assert!(log.iter().all(|(program, _)| program == SYSCTL));
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

        fn run(&self, program: &'static str, args: &[&str]) -> io::Result<RunOutput> {
            let output = self.fake.run(program, args)?;
            let value = match args {
                // Loads the file only if it is there, like the real one.
                ["-p", FILE] if self.file().is_some() => Some(LOWEST),
                ["-w", "net.ipv4.ip_unprivileged_port_start=1024"] => Some(DEFAULT),
                ["--system"] => self.other_file,
                _ => None,
            };
            if let (Some(0), false, Some(value)) = (output.code, self.inert.get(), value) {
                self.set(VALUE, format!("{value}\n").as_bytes());
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

    fn ours() -> Vec<u8> {
        content().into_bytes()
    }

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn load() -> Vec<String> {
        args(&["-p", FILE])
    }

    fn failed(op: &'static str, code: &'static str, message: &str) -> OpResult {
        OpResult::failed(op, code, message.to_owned())
    }

    #[test]
    fn install_writes_the_file_and_loads_it() {
        let linux = Linux::new(1024);
        assert_eq!(install(&linux), result(INSTALL, Outcome::Done));
        assert_eq!(linux.file(), Some((ours(), 0o644)));
        assert_eq!(linux.commands(), [load()]);
        assert_eq!(linux.lowest(), "80\n");
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
        for lowest in [80, 0] {
            let linux = Linux::new(lowest);
            assert_eq!(install(&linux), result(INSTALL, Outcome::Unchanged));
            assert_eq!(linux.file(), None);
            assert!(linux.commands().is_empty());
            assert_eq!(linux.lowest(), format!("{lowest}\n"));
        }
    }

    #[test]
    fn install_over_our_loaded_file_is_unchanged() {
        let mut linux = Linux::with_file(80, &ours());
        // Any write fails here, so `unchanged` also shows that none was tried.
        linux.fake.fail_writes.insert(PathBuf::from(FILE));
        assert_eq!(install(&linux), result(INSTALL, Outcome::Unchanged));
        assert_eq!(linux.file(), Some((ours(), OLD_MODE)));
        assert!(linux.commands().is_empty());
    }

    /// Someone set the value back by hand: the file is loaded again, and not
    /// rewritten.
    #[test]
    fn install_loads_our_file_again_when_the_value_went_back() {
        let linux = Linux::with_file(1024, &ours());
        assert_eq!(install(&linux), result(INSTALL, Outcome::Done));
        assert_eq!(linux.file(), Some((ours(), OLD_MODE)));
        assert_eq!(linux.commands(), [load()]);
        assert_eq!(linux.lowest(), "80\n");
    }

    #[test]
    fn install_rewrites_our_file_with_other_content() {
        let old = format!("{MARKER}\n{KEY}=443\n");
        let linux = Linux::with_file(443, old.as_bytes());
        assert_eq!(install(&linux), result(INSTALL, Outcome::Done));
        assert_eq!(linux.file(), Some((ours(), MODE)));
        assert_eq!(linux.lowest(), "80\n");
    }

    /// The exact message also shows that it holds the path and no content.
    #[test]
    fn install_fails_on_a_foreign_file_and_keeps_it() {
        let linux = Linux::with_file(1024, FOREIGN);
        let message = format!("{FILE} was not written by Marsa, so it was left as it is");
        assert_eq!(install(&linux), failed(INSTALL, PORTS_FOREIGN, &message));
        assert_eq!(linux.file(), Some((FOREIGN.to_vec(), OLD_MODE)));
        assert!(linux.commands().is_empty());
        assert_eq!(linux.lowest(), "1024\n");
    }

    #[test]
    fn a_file_or_a_value_that_cannot_be_read_is_ports_read() {
        let mut linux = Linux::with_file(1024, &ours());
        linux.fake.fail_reads.insert(PathBuf::from(FILE));
        let message = format!("cannot read {FILE}: permission denied, so it was left as it is");
        assert_eq!(install(&linux), failed(INSTALL, PORTS_READ, &message));
        assert_eq!(remove(&linux), failed(REMOVE, PORTS_READ, &message));

        let values: [(Option<&[u8]>, String); 4] = [
            (None, format!("cannot read {VALUE}: entity not found")),
            (
                Some(b"abc\n"),
                format!("{VALUE} does not hold a port number"),
            ),
            (
                Some(b"-1\n"),
                format!("{VALUE} does not hold a port number"),
            ),
            (
                Some(b"\xff\n"),
                format!("{VALUE} does not hold a port number"),
            ),
        ];
        for (value, message) in values {
            let linux = Linux::with_file(1024, &ours());
            match value {
                Some(bytes) => linux.set(VALUE, bytes),
                None => drop(linux.fake.files.borrow_mut().remove(Path::new(VALUE))),
            }
            assert_eq!(install(&linux), failed(INSTALL, PORTS_READ, &message));
            assert_eq!(remove(&linux), failed(REMOVE, PORTS_READ, &message));
            assert_eq!(linux.file(), Some((ours(), OLD_MODE)));
            assert!(linux.commands().is_empty());
        }
    }

    #[test]
    fn a_failed_write_is_ports_write_and_nothing_is_loaded() {
        let mut linux = Linux::new(1024);
        linux.fake.fail_writes.insert(PathBuf::from(FILE));
        let message = format!("cannot write {FILE}: permission denied");
        assert_eq!(install(&linux), failed(INSTALL, PORTS_WRITE, &message));
        assert!(linux.commands().is_empty());
        assert_eq!(linux.lowest(), "1024\n");
    }

    #[test]
    fn a_failed_load_removes_the_file_this_call_wrote() {
        let mut linux = Linux::new(1024);
        linux.fake.script(SYSCTL, &["-p", FILE], exit(255));
        let message = format!("sysctl -p {FILE} exited with code 255");
        assert_eq!(
            install(&linux),
            failed(INSTALL, PORTS_APPLY_FAILED, &message)
        );
        assert_eq!(linux.file(), None);
        assert_eq!(linux.lowest(), "1024\n");

        // No `sysctl` program at all.
        let mut linux = Linux::new(1024);
        linux.fake.defaults.clear();
        let message = format!("sysctl -p {FILE} could not run: entity not found");
        assert_eq!(
            install(&linux),
            failed(INSTALL, PORTS_APPLY_FAILED, &message)
        );
        assert_eq!(linux.file(), None);
    }

    /// The value is read again after the load: exit code 0 alone is not
    /// trusted.
    #[test]
    fn a_load_that_changes_nothing_is_a_failure() {
        let message = format!("sysctl -p {FILE} ran, but {KEY} is 1024");
        let linux = Linux::new(1024);
        linux.inert.set(true);
        assert_eq!(
            install(&linux),
            failed(INSTALL, PORTS_APPLY_FAILED, &message)
        );
        assert_eq!(linux.file(), None);

        // A file that was there before this call stays.
        let linux = Linux::with_file(1024, &ours());
        linux.inert.set(true);
        assert_eq!(
            install(&linux),
            failed(INSTALL, PORTS_APPLY_FAILED, &message)
        );
        assert_eq!(linux.file(), Some((ours(), OLD_MODE)));
    }

    #[test]
    fn remove_without_our_file_changes_nothing() {
        for linux in [Linux::new(80), Linux::with_file(80, FOREIGN)] {
            let before = linux.file();
            assert_eq!(remove(&linux), result(REMOVE, Outcome::Unchanged));
            assert_eq!(linux.file(), before);
            assert!(linux.commands().is_empty());
            assert_eq!(linux.lowest(), "80\n");
        }
    }

    #[test]
    fn remove_sets_the_default_back_deletes_the_file_and_reloads_the_rest() {
        let linux = Linux::with_file(80, &ours());
        assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
        assert_eq!(linux.file(), None);
        let reset = args(&["-w", "net.ipv4.ip_unprivileged_port_start=1024"]);
        assert_eq!(linux.commands(), [reset, args(&["--system"])]);
        assert_eq!(linux.lowest(), "1024\n");
    }

    #[test]
    fn another_file_that_sets_the_key_applies_again() {
        let mut linux = Linux::with_file(80, &ours());
        linux.other_file = Some(80);
        assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
        assert_eq!(linux.file(), None);
        assert_eq!(linux.lowest(), "80\n");
    }

    /// Someone changed the value after the install: it is theirs now.
    #[test]
    fn remove_leaves_a_value_that_is_not_ours() {
        for lowest in [0, 79, 81, 1024] {
            let linux = Linux::with_file(lowest, &ours());
            assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
            assert_eq!(linux.file(), None);
            assert!(linux.commands().is_empty());
            assert_eq!(linux.lowest(), format!("{lowest}\n"));
        }
    }

    #[test]
    fn a_failed_reset_is_ports_restore_failed_and_keeps_the_file() {
        let mut linux = Linux::with_file(80, &ours());
        let reset = "net.ipv4.ip_unprivileged_port_start=1024";
        linux.fake.script(SYSCTL, &["-w", reset], exit(1));
        let message = format!("sysctl -w {reset} exited with code 1");
        assert_eq!(
            remove(&linux),
            failed(REMOVE, PORTS_RESTORE_FAILED, &message)
        );
        assert_eq!(linux.file(), Some((ours(), OLD_MODE)));
        assert_eq!(linux.commands().len(), 1);
        assert_eq!(linux.lowest(), "80\n");
    }

    /// The value is back at the default by then, so a retry only deletes.
    #[test]
    fn a_failed_delete_is_ports_delete_and_a_retry_finishes() {
        let mut linux = Linux::with_file(80, &ours());
        linux.fake.fail_removes.insert(PathBuf::from(FILE));
        let message = format!("cannot delete {FILE}: permission denied");
        assert_eq!(remove(&linux), failed(REMOVE, PORTS_DELETE, &message));
        assert_eq!(linux.file(), Some((ours(), OLD_MODE)));
        assert_eq!(linux.commands().len(), 1);

        linux.fake.fail_removes.clear();
        assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
        assert_eq!(linux.file(), None);
        assert_eq!(linux.commands().len(), 1);
        assert_eq!(linux.lowest(), "1024\n");
    }

    #[test]
    fn a_failed_reload_of_the_other_files_does_not_fail_the_removal() {
        let mut linux = Linux::with_file(80, &ours());
        linux.fake.script(SYSCTL, &["--system"], exit(255));
        assert_eq!(remove(&linux), result(REMOVE, Outcome::Done));
        assert_eq!(linux.file(), None);
        assert_eq!(linux.lowest(), "1024\n");
    }

    /// Through `apply`, as a real request: Linux only.
    #[test]
    fn the_ops_run_from_a_request_on_linux_only() {
        let ops = json!([{ "op": "ports_install" }, { "op": "ports_remove" }]);
        let raw = json!({ "version": 1, "ops": ops }).to_string();
        let request = Request::from_arg(OsStr::new(&raw)).unwrap();
        let linux = Linux::new(1024);
        let expected = [
            result(INSTALL, Outcome::Done),
            result(REMOVE, Outcome::Done),
        ];
        assert_eq!(apply(&request, Os::Linux, &linux), expected);
        assert_eq!(linux.file(), None);
        assert_eq!(linux.lowest(), "1024\n");

        for os in [Os::Macos, Os::Windows] {
            let linux = Linux::new(1024);
            let results = apply(&request, os, &linux);
            assert!(results.iter().all(|result| matches!(
                &result.outcome,
                Outcome::Failed { code, .. } if *code == crate::response::code::OP_NOT_SUPPORTED
            )));
            assert!(linux.commands().is_empty() && linux.file().is_none());
        }
    }
}
