//! macOS: `/etc/resolver/test`, so the system asks the DNS server of `marsad`
//! for every name under `.test` (`man 5 resolver`).
//!
//! The file has a `port` line, so the DNS server does not need port 53. That
//! matters: Laravel Herd runs `dnsmasq` on `127.0.0.1:53` (ADR 0008).
//!
//! The helper changes the file only if Marsa wrote it: its first line is
//! exactly [`MARKER`]. Herd and Valet write the same path, and their file is
//! never overwritten or deleted. On the owner's Mac today the file is Herd's
//! (`nameserver 127.0.0.1`), so `resolver_install` fails there with
//! `resolver_foreign`. `marsad` decides what to do then (a later task).
//!
//! No external command runs here: only [`System::read`], `write_atomic`, and
//! `remove`.

use std::io;
use std::path::Path;

use crate::response::{OpResult, Outcome, code};
use crate::system::System;

/// The file macOS reads for the names under `.test`.
const PATH: &str = "/etc/resolver/test";
/// Everyone can read it, and only root can change it.
const MODE: u32 = 0o644;
/// The first line of the file Marsa writes.
const MARKER: &str = "# Managed by Marsa. Do not edit.";
/// The largest file the helper reads. Marsa's file is far smaller, so a
/// larger one counts as foreign.
const MAX_BYTES: u64 = 4 * 1024;

const INSTALL: &str = "resolver_install";
const REMOVE: &str = "resolver_remove";

/// What is at [`PATH`] now.
enum Current {
    Missing,
    /// Marsa's file, with its content.
    Ours(Vec<u8>),
    /// Anything else: another tool's file, a file over [`MAX_BYTES`], a
    /// folder, or a file that cannot be read.
    Foreign,
}

/// `resolver_install`: names under `.test` go to `127.0.0.1:<port>`.
pub fn install(port: u16, system: &impl System) -> OpResult {
    let content = content(port);
    let outcome = match current(system) {
        Current::Foreign => {
            // The path only: the content of another tool's file stays out.
            let message = format!("{PATH} was not written by Marsa, so it was left as it is");
            return OpResult::failed(INSTALL, code::RESOLVER_FOREIGN, message);
        }
        Current::Ours(bytes) if bytes == content.as_bytes() => Outcome::Unchanged,
        Current::Missing | Current::Ours(_) => {
            if let Err(err) = system.write_atomic(Path::new(PATH), content.as_bytes(), MODE) {
                let message = format!("cannot write {PATH}: {}", err.kind());
                return OpResult::failed(INSTALL, code::RESOLVER_WRITE, message);
            }
            Outcome::Done
        }
    };
    OpResult {
        op: INSTALL,
        outcome,
    }
}

/// `resolver_remove`: deletes the file if Marsa wrote it. A missing or
/// foreign file is `unchanged`: nothing of Marsa's is there to remove.
pub fn remove(system: &impl System) -> OpResult {
    let outcome = match current(system) {
        Current::Missing | Current::Foreign => Outcome::Unchanged,
        Current::Ours(_) => {
            if let Err(err) = system.remove(Path::new(PATH)) {
                let message = format!("cannot delete {PATH}: {}", err.kind());
                return OpResult::failed(REMOVE, code::RESOLVER_DELETE, message);
            }
            Outcome::Done
        }
    };
    OpResult {
        op: REMOVE,
        outcome,
    }
}

/// LF only, with a final newline.
fn content(port: u16) -> String {
    format!("{MARKER}\nnameserver 127.0.0.1\nport {port}\n")
}

fn current(system: &impl System) -> Current {
    match system.read(Path::new(PATH), MAX_BYTES) {
        Ok(bytes) if is_ours(&bytes) => Current::Ours(bytes),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Current::Missing,
        // Fail safe: a file the helper cannot read in full may be another
        // tool's, so it is never changed.
        _ => Current::Foreign,
    }
}

/// The first line is exactly the marker. Nothing is trimmed: a near match
/// (a `\r`, a space) is not a file Marsa wrote, so it counts as foreign.
fn is_ours(bytes: &[u8]) -> bool {
    bytes.split(|&byte| byte == b'\n').next() == Some(MARKER.as_bytes())
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
    use crate::system::FakeSystem;

    /// Laravel Herd's file at the same path.
    const HERD: &[u8] = b"nameserver 127.0.0.1\n";
    /// The mode of the file a test starts with. Marsa never writes it, so the
    /// mode shows whether an op rewrote the file.
    const OLD_MODE: u32 = 0o600;

    fn ours(port: u16) -> Vec<u8> {
        content(port).into_bytes()
    }

    /// Marsa's file, padded to `len` bytes.
    fn ours_padded(len: u64) -> Vec<u8> {
        let mut bytes = ours(18053);
        bytes.resize(usize::try_from(len).unwrap(), b'#');
        bytes
    }

    fn fake_with(bytes: &[u8]) -> FakeSystem {
        let fake = FakeSystem::default();
        let file = (bytes.to_vec(), OLD_MODE);
        fake.files.borrow_mut().insert(PathBuf::from(PATH), file);
        fake
    }

    /// The file at [`PATH`] after the ops. Every test reads it through this,
    /// so every test also checks that no command ran and no other file
    /// appeared.
    fn file(fake: &FakeSystem) -> Option<(Vec<u8>, u32)> {
        assert!(fake.log.borrow().is_empty(), "a resolver op ran a command");
        let files = fake.files.borrow();
        assert!(files.keys().all(|path| path == Path::new(PATH)));
        files.get(Path::new(PATH)).cloned()
    }

    fn result(op: &'static str, outcome: Outcome) -> OpResult {
        OpResult { op, outcome }
    }

    fn foreign() -> OpResult {
        let message = format!("{PATH} was not written by Marsa, so it was left as it is");
        OpResult::failed(INSTALL, code::RESOLVER_FOREIGN, message)
    }

    fn apply_ops(ops: serde_json::Value, fake: &FakeSystem) -> Vec<OpResult> {
        let raw = json!({ "version": 1, "ops": ops }).to_string();
        let request = Request::from_arg(OsStr::new(&raw)).unwrap();
        apply(&request, Os::Macos, fake)
    }

    #[test]
    fn install_writes_a_missing_file() {
        let fake = FakeSystem::default();
        assert_eq!(install(18053, &fake), result(INSTALL, Outcome::Done));
        assert_eq!(file(&fake), Some((ours(18053), 0o644)));
    }

    /// The snapshot does not show line endings, so they are checked here.
    #[test]
    fn the_written_file() {
        let fake = FakeSystem::default();
        assert_eq!(install(18053, &fake).outcome, Outcome::Done);
        let (bytes, mode) = file(&fake).unwrap();
        assert_eq!(mode, 0o644);
        assert!(bytes.ends_with(b"18053\n") && !bytes.contains(&b'\r'));
        let text = String::from_utf8(bytes).unwrap();
        insta::assert_snapshot!(text);
    }

    #[test]
    fn remove_of_a_missing_file_is_unchanged() {
        let fake = FakeSystem::default();
        assert_eq!(remove(&fake), result(REMOVE, Outcome::Unchanged));
        assert_eq!(file(&fake), None);
    }

    #[test]
    fn install_over_our_same_file_is_unchanged() {
        let mut fake = fake_with(&ours(18053));
        // Any write fails here, so `unchanged` also shows that none was tried.
        fake.fail_writes.insert(PathBuf::from(PATH));
        assert_eq!(install(18053, &fake), result(INSTALL, Outcome::Unchanged));
        assert_eq!(file(&fake), Some((ours(18053), OLD_MODE)));
    }

    #[test]
    fn remove_deletes_our_same_file() {
        let fake = fake_with(&ours(18053));
        assert_eq!(remove(&fake), result(REMOVE, Outcome::Done));
        assert_eq!(file(&fake), None);
    }

    #[test]
    fn install_rewrites_our_file_with_another_port() {
        let fake = fake_with(&ours(18054));
        assert_eq!(install(18053, &fake), result(INSTALL, Outcome::Done));
        assert_eq!(file(&fake), Some((ours(18053), MODE)));
    }

    #[test]
    fn remove_deletes_our_file_with_another_port() {
        let fake = fake_with(&ours(18054));
        assert_eq!(remove(&fake), result(REMOVE, Outcome::Done));
        assert_eq!(file(&fake), None);
    }

    /// The exact message also shows that it holds the path and no content.
    #[test]
    fn install_fails_on_a_foreign_file_and_keeps_it() {
        let fake = fake_with(HERD);
        assert_eq!(install(18053, &fake), foreign());
        assert_eq!(file(&fake), Some((HERD.to_vec(), OLD_MODE)));
    }

    #[test]
    fn remove_keeps_a_foreign_file() {
        let fake = fake_with(HERD);
        assert_eq!(remove(&fake), result(REMOVE, Outcome::Unchanged));
        assert_eq!(file(&fake), Some((HERD.to_vec(), OLD_MODE)));
    }

    #[test]
    fn a_foreign_file_survives_install_then_remove() {
        let fake = fake_with(HERD);
        let ops = json!([
            { "op": "resolver_install", "port": 18053 },
            { "op": "resolver_remove" },
        ]);
        let expected = [foreign(), result(REMOVE, Outcome::Unchanged)];
        assert_eq!(apply_ops(ops, &fake), expected);
        assert_eq!(file(&fake), Some((HERD.to_vec(), OLD_MODE)));
    }

    /// The marker is there, but the size alone makes it foreign.
    #[test]
    fn install_fails_on_a_file_over_4_kib_and_keeps_it() {
        let large = ours_padded(MAX_BYTES + 1);
        let fake = fake_with(&large);
        assert_eq!(install(18053, &fake), foreign());
        assert_eq!(file(&fake), Some((large, OLD_MODE)));
    }

    #[test]
    fn remove_keeps_a_file_over_4_kib() {
        let large = ours_padded(MAX_BYTES + 1);
        let fake = fake_with(&large);
        assert_eq!(remove(&fake), result(REMOVE, Outcome::Unchanged));
        assert_eq!(file(&fake), Some((large, OLD_MODE)));
        // Exactly 4 KiB is still read, so this one is ours.
        let fake = fake_with(&ours_padded(MAX_BYTES));
        assert_eq!(remove(&fake), result(REMOVE, Outcome::Done));
        assert_eq!(file(&fake), None);
    }

    #[test]
    fn only_the_exact_marker_line_makes_a_file_ours() {
        let near = [
            format!("{MARKER}\r\nport 18053\n"),
            format!("{MARKER} \n"),
            format!(" {MARKER}\n"),
            format!("\n{MARKER}\n"),
            MARKER.to_lowercase(),
            String::new(),
        ];
        for text in near {
            let fake = fake_with(text.as_bytes());
            assert_eq!(
                remove(&fake),
                result(REMOVE, Outcome::Unchanged),
                "{text:?}"
            );
            assert_eq!(file(&fake), Some((text.into_bytes(), OLD_MODE)));
        }
        // The marker alone, with no newline, is still the first line.
        let fake = fake_with(MARKER.as_bytes());
        assert_eq!(remove(&fake), result(REMOVE, Outcome::Done));
    }

    #[test]
    fn a_failed_write_is_resolver_write_and_the_next_op_still_runs() {
        let mut fake = fake_with(&ours(18054));
        fake.fail_writes.insert(PathBuf::from(PATH));
        let ops = json!([
            { "op": "resolver_install", "port": 18053 },
            { "op": "resolver_remove" },
        ]);
        let message = format!("cannot write {PATH}: permission denied");
        let expected = [
            OpResult::failed(INSTALL, code::RESOLVER_WRITE, message),
            result(REMOVE, Outcome::Done),
        ];
        assert_eq!(apply_ops(ops, &fake), expected);
        assert_eq!(file(&fake), None);
    }

    #[test]
    fn a_failed_delete_is_resolver_delete_and_keeps_the_file() {
        let mut fake = fake_with(&ours(18053));
        fake.fail_removes.insert(PathBuf::from(PATH));
        let message = format!("cannot delete {PATH}: permission denied");
        let expected = OpResult::failed(REMOVE, code::RESOLVER_DELETE, message);
        assert_eq!(remove(&fake), expected);
        assert_eq!(file(&fake), Some((ours(18053), OLD_MODE)));
    }
}
