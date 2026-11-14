//! Windows: trust of Marsa's CA root in the machine's Root store (ADR 0018).
//!
//! The helper runs `certutil.exe` from the system folder itself, never
//! `caddy trust` (ADR 0011). The request carries the PEM text, parsed by the
//! strict parser of [`trust`](super::trust): exactly one certificate. The
//! helper writes its own clean copy to `<Program Files>\Marsa\ca-root.pem`,
//! a folder only administrators can write. That file is the record of what
//! Marsa trusted: `trust_remove` reads it and deletes by the certificate's
//! SHA-1 thumbprint, never by name. `certutil` matches a SHA-1, not a
//! SHA-256 (spike S-11); the thumbprint only finds the certificate, it
//! decides nothing about trust.
//!
//! Which certificate the service may trust at all is the service's check,
//! before any op runs (task M4-19f). No message holds any part of the PEM.

use std::io;
use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

use super::program;
use super::trust::{
    TRUST_ADD_FAILED, TRUST_CERT_INVALID, TRUST_DELETE, TRUST_READ, TRUST_REMOVE_FAILED,
    TRUST_WRITE, parse_pem, to_pem,
};
use crate::response::{OpResult, Outcome};
use crate::system::{RunOutput, System};

const CERTUTIL: &str = "certutil.exe";
/// The machine's "Trusted Root Certification Authorities" store.
const STORE: &str = "Root";
/// Marsa's copy of the certificate it trusts, in [`System::install_dir`].
const FILE: &str = "ca-root.pem";
/// Windows ignores the mode: the file gets the permissions of its folder.
const MODE: u32 = 0o644;
/// The largest file read back, as on macOS.
const MAX_FILE_BYTES: u64 = 32 * 1024;
/// `certutil -store` exits with `NTE_NOT_FOUND` (0x80090011) when no
/// certificate matches (spike S-11). An `HRESULT` is an `i32`.
const NOT_FOUND: i32 = 0x8009_0011_u32 as i32;

const INSTALL: &str = "trust_install";
const REMOVE: &str = "trust_remove";

/// `trust_install`: trusts the one certificate in `pem` as a root for every
/// user of the machine. A different certificate that Marsa trusted before is
/// removed first.
pub fn install(pem: &str, system: &impl System) -> OpResult {
    let der = match parse_pem(pem) {
        Ok(der) => der,
        Err(reason) => {
            return OpResult::failed(INSTALL, TRUST_CERT_INVALID, format!("pem {reason}"));
        }
    };
    let path = match ca_path(INSTALL, TRUST_WRITE, system) {
        Ok(path) => path,
        Err(failed) => return failed,
    };
    let thumbprint = sha1_hex(&der);
    match stored_thumbprint(INSTALL, &path, system) {
        Ok(Some(old)) if old == thumbprint => match in_store(INSTALL, &thumbprint, system) {
            Ok(true) => return result(INSTALL, Outcome::Unchanged),
            // Removed by hand since: added again below.
            Ok(false) => {}
            Err(failed) => return failed,
        },
        Ok(Some(old)) => {
            if let Err(failed) = remove_steps(INSTALL, &path, &old, system) {
                return failed;
            }
        }
        Ok(None) => {}
        Err(failed) => return failed,
    }
    if let Err(err) = system.write_atomic(&path, &to_pem(&der), MODE) {
        let message = format!("cannot write {}: {}", path.display(), err.kind());
        return OpResult::failed(INSTALL, TRUST_WRITE, message);
    }
    let file = path.to_string_lossy();
    let added = system.run_system_tool(CERTUTIL, &["-addstore", STORE, &file]);
    if !program::succeeded(&added) {
        return command_failed(INSTALL, TRUST_ADD_FAILED, "-addstore", &added);
    }
    // `-addstore` exits with 0 when the certificate is already there too, so
    // the store itself is the check.
    match in_store(INSTALL, &thumbprint, system) {
        Ok(true) => result(INSTALL, Outcome::Done),
        Ok(false) => {
            let message =
                "certutil -addstore exited with code 0, but the certificate is not in the store"
                    .to_owned();
            OpResult::failed(INSTALL, TRUST_ADD_FAILED, message)
        }
        Err(failed) => failed,
    }
}

/// `trust_remove`: removes the certificate that Marsa trusted, then its
/// file. Without the file, nothing of Marsa's is trusted.
pub fn remove(system: &impl System) -> OpResult {
    let path = match ca_path(REMOVE, TRUST_READ, system) {
        Ok(path) => path,
        Err(failed) => return failed,
    };
    match stored_thumbprint(REMOVE, &path, system) {
        Ok(None) => result(REMOVE, Outcome::Unchanged),
        Ok(Some(thumbprint)) => match remove_steps(REMOVE, &path, &thumbprint, system) {
            Ok(()) => result(REMOVE, Outcome::Done),
            Err(failed) => failed,
        },
        Err(failed) => failed,
    }
}

fn result(op: &'static str, outcome: Outcome) -> OpResult {
    OpResult { op, outcome }
}

fn ca_path(
    op: &'static str,
    code: &'static str,
    system: &impl System,
) -> Result<PathBuf, OpResult> {
    system
        .install_dir()
        .map(|dir| dir.join(FILE))
        .map_err(|err| {
            let message = format!("cannot find Marsa's program folder: {}", err.kind());
            OpResult::failed(op, code, message)
        })
}

/// The SHA-1 thumbprint of the certificate in Marsa's file, or `None` if
/// nothing is there.
fn stored_thumbprint(
    op: &'static str,
    path: &Path,
    system: &impl System,
) -> Result<Option<String>, OpResult> {
    let reason = match system.read(path, MAX_FILE_BYTES) {
        Ok(bytes) => match std::str::from_utf8(&bytes).map(parse_pem) {
            Ok(Ok(der)) => return Ok(Some(sha1_hex(&der))),
            _ => "is not one certificate".to_owned(),
        },
        // `read` follows links and `exists` does not, so a dangling link is
        // something there, not nothing.
        Err(err) if err.kind() == io::ErrorKind::NotFound && !system.exists(path) => {
            return Ok(None);
        }
        Err(err) => format!("cannot be read: {}", err.kind()),
    };
    let message = format!("{} {reason}, so it was left as it is", path.display());
    Err(OpResult::failed(op, TRUST_READ, message))
}

/// Whether the store holds the certificate: `certutil -store` exits with 0,
/// or with [`NOT_FOUND`]. Anything else is a failure of `op`.
fn in_store(op: &'static str, thumbprint: &str, system: &impl System) -> Result<bool, OpResult> {
    let found = system.run_system_tool(CERTUTIL, &["-store", STORE, thumbprint]);
    match &found {
        Ok(RunOutput { code: Some(0), .. }) => Ok(true),
        Ok(RunOutput {
            code: Some(NOT_FOUND),
            ..
        }) => Ok(false),
        _ => {
            let code = if op == INSTALL {
                TRUST_ADD_FAILED
            } else {
                TRUST_REMOVE_FAILED
            };
            Err(command_failed(op, code, "-store", &found))
        }
    }
}

/// Removes the certificate whose thumbprint is `thumbprint` from the store,
/// checks that it is gone, then deletes Marsa's file. After a failure the
/// file stays, so a retry still has the thumbprint.
fn remove_steps(
    op: &'static str,
    path: &Path,
    thumbprint: &str,
    system: &impl System,
) -> Result<(), OpResult> {
    if in_store(op, thumbprint, system)? {
        let deleted = system.run_system_tool(CERTUTIL, &["-delstore", STORE, thumbprint]);
        if !program::succeeded(&deleted) {
            return Err(command_failed(
                op,
                TRUST_REMOVE_FAILED,
                "-delstore",
                &deleted,
            ));
        }
        // `-delstore` exits with 0 when nothing matched too (spike S-11).
        if in_store(op, thumbprint, system)? {
            let message =
                "certutil -delstore exited with code 0, but the certificate is still in the store"
                    .to_owned();
            return Err(OpResult::failed(op, TRUST_REMOVE_FAILED, message));
        }
    }
    if let Err(err) = system.remove(path) {
        let message = format!("cannot delete {}: {}", path.display(), err.kind());
        return Err(OpResult::failed(op, TRUST_DELETE, message));
    }
    Ok(())
}

/// The output of `certutil` is not in the message: the exit code is enough
/// for `marsad`, and the owner can run the same command by hand.
fn command_failed(
    op: &'static str,
    code: &'static str,
    command: &str,
    result: &io::Result<RunOutput>,
) -> OpResult {
    let message = format!("certutil {command} {}", program::failure(result));
    OpResult::failed(op, code, message)
}

/// Lower-case hex, the form `certutil` prints and matches.
fn sha1_hex(der: &[u8]) -> String {
    Sha1::digest(der)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::ffi::OsStr;
    use std::fs;

    use serde_json::json;

    use super::*;
    use crate::ops;
    use crate::os::Os;
    use crate::request::Request;
    use crate::system::FakeSystem;

    const INSTALL_DIR: &str = r"C:\Program Files\Marsa";

    fn pem() -> String {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        fs::read_to_string(root.join("fixtures").join("helper").join("test-ca.pem")).unwrap()
    }

    fn thumbprint() -> String {
        sha1_hex(&parse_pem(&pem()).unwrap())
    }

    fn ca_file() -> PathBuf {
        Path::new(INSTALL_DIR).join(FILE)
    }

    /// A Windows machine whose Root store `certutil` changes: files and the
    /// call log come from a [`FakeSystem`], and `run_system_tool` acts like
    /// `certutil` on one certificate, the test CA.
    struct Machine {
        fake: FakeSystem,
        /// The test CA is in the Root store.
        present: Cell<bool>,
        /// `-addstore` puts it there, `-delstore` takes it out.
        add_works: bool,
        delete_works: bool,
        /// The exit code of `-addstore`.
        add_code: i32,
        /// Every `-store` exits with this code, if set.
        query_code: Option<i32>,
    }

    impl Machine {
        fn new(present: bool) -> Self {
            Self {
                fake: FakeSystem {
                    install_dir: Some(PathBuf::from(INSTALL_DIR)),
                    ..FakeSystem::default()
                },
                present: Cell::new(present),
                add_works: true,
                delete_works: true,
                add_code: 0,
                query_code: None,
            }
        }

        fn with_file(self, text: &str) -> Self {
            let file = (text.as_bytes().to_vec(), MODE);
            self.fake.files.borrow_mut().insert(ca_file(), file);
            self
        }

        fn file(&self) -> Option<Vec<u8>> {
            self.fake
                .files
                .borrow()
                .get(&ca_file())
                .map(|(bytes, _)| bytes.clone())
        }

        fn calls(&self) -> Vec<String> {
            let log = self.fake.log.borrow();
            log.iter()
                .map(|(program, args)| format!("{program} {}", args.join(" ")))
                .collect()
        }
    }

    fn exit(code: i32) -> io::Result<RunOutput> {
        Ok(RunOutput {
            code: Some(code),
            stdout: b"certutil output, never in a message".to_vec(),
            stderr: Vec::new(),
        })
    }

    impl System for Machine {
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
            self.fake.run(program, args)
        }

        fn current_exe(&self) -> io::Result<PathBuf> {
            self.fake.current_exe()
        }

        fn install_dir(&self) -> io::Result<PathBuf> {
            self.fake.install_dir()
        }

        fn run_system_tool(&self, program: &'static str, args: &[&str]) -> io::Result<RunOutput> {
            let logged = args.iter().map(|arg| (*arg).to_owned()).collect();
            self.fake
                .log
                .borrow_mut()
                .push((program.to_owned(), logged));
            assert_eq!(program, CERTUTIL);
            let ours = thumbprint();
            match args {
                ["-store", STORE, id] => match self.query_code {
                    Some(code) => exit(code),
                    None if self.present.get() && *id == ours => exit(0),
                    None => exit(NOT_FOUND),
                },
                ["-addstore", STORE, _] => {
                    if self.add_works {
                        self.present.set(true);
                    }
                    exit(self.add_code)
                }
                ["-delstore", STORE, id] => {
                    if self.delete_works && *id == ours {
                        self.present.set(false);
                    }
                    // Like certutil: 0, whether or not anything matched.
                    exit(0)
                }
                other => panic!("unexpected certutil call: {other:?}"),
            }
        }
    }

    fn status(result: &OpResult) -> &'static str {
        match result.outcome {
            Outcome::Done => "done",
            Outcome::Unchanged => "unchanged",
            Outcome::Failed { code, .. } => code,
        }
    }

    #[test]
    fn install_writes_the_file_adds_it_and_checks_the_store() {
        let machine = Machine::new(false);
        let result = install(&pem(), &machine);
        assert_eq!((result.op, status(&result)), (INSTALL, "done"));
        assert!(machine.present.get());
        assert_eq!(machine.file(), Some(to_pem(&parse_pem(&pem()).unwrap())));
        let file = ca_file().to_string_lossy().into_owned();
        let thumb = thumbprint();
        assert_eq!(
            machine.calls(),
            [
                format!("{CERTUTIL} -addstore Root {file}"),
                format!("{CERTUTIL} -store Root {thumb}"),
            ]
        );
    }

    #[test]
    fn the_same_certificate_already_in_the_store_is_unchanged() {
        let machine = Machine::new(true).with_file(&pem());
        let result = install(&pem(), &machine);
        assert_eq!(status(&result), "unchanged");
        assert_eq!(machine.calls().len(), 1, "only the query");
    }

    #[test]
    fn the_same_certificate_removed_by_hand_is_added_again() {
        let machine = Machine::new(false).with_file(&pem());
        assert_eq!(status(&install(&pem(), &machine)), "done");
        assert!(machine.present.get());
    }

    /// The file holds another certificate (the test CA with one byte of its
    /// signature changed): it is looked up by its own thumbprint and its file
    /// replaced, then the new one is added.
    #[test]
    fn a_different_certificate_from_before_is_removed_first() {
        let mut der = parse_pem(&pem()).unwrap();
        *der.last_mut().unwrap() ^= 1;
        let other = String::from_utf8(to_pem(&der)).unwrap();
        let old = sha1_hex(&der);
        assert_ne!(old, thumbprint());
        let machine = Machine::new(false).with_file(&other);
        assert_eq!(status(&install(&pem(), &machine)), "done");
        assert_eq!(machine.calls()[0], format!("{CERTUTIL} -store Root {old}"));
        assert_eq!(machine.file(), Some(to_pem(&parse_pem(&pem()).unwrap())));
    }

    #[test]
    fn an_add_that_leaves_nothing_in_the_store_fails() {
        let mut machine = Machine::new(false);
        machine.add_works = false;
        let result = install(&pem(), &machine);
        assert_eq!(status(&result), TRUST_ADD_FAILED);
    }

    #[test]
    fn a_failed_add_names_the_exit_and_not_the_output() {
        let mut machine = Machine::new(false);
        machine.add_works = false;
        machine.add_code = 5;
        let result = install(&pem(), &machine);
        let Outcome::Failed { code, message } = &result.outcome else {
            panic!("not failed");
        };
        assert_eq!(*code, TRUST_ADD_FAILED);
        assert_eq!(message, "certutil -addstore exited with code 5");
    }

    #[test]
    fn a_bad_pem_runs_nothing_and_writes_nothing() {
        let machine = Machine::new(false);
        let result = install("-----BEGIN CERTIFICATE-----\nnot base64\n", &machine);
        assert_eq!(status(&result), TRUST_CERT_INVALID);
        assert!(machine.calls().is_empty() && machine.file().is_none());
    }

    #[test]
    fn remove_deletes_by_thumbprint_checks_and_deletes_the_file() {
        let machine = Machine::new(true).with_file(&pem());
        let result = remove(&machine);
        assert_eq!((result.op, status(&result)), (REMOVE, "done"));
        assert!(!machine.present.get() && machine.file().is_none());
        let thumb = thumbprint();
        assert_eq!(
            machine.calls(),
            [
                format!("{CERTUTIL} -store Root {thumb}"),
                format!("{CERTUTIL} -delstore Root {thumb}"),
                format!("{CERTUTIL} -store Root {thumb}"),
            ]
        );
    }

    #[test]
    fn remove_after_a_removal_by_hand_only_deletes_the_file() {
        let machine = Machine::new(false).with_file(&pem());
        assert_eq!(status(&remove(&machine)), "done");
        assert_eq!(machine.calls().len(), 1, "only the query");
        assert!(machine.file().is_none());
    }

    #[test]
    fn remove_without_the_file_is_unchanged_and_runs_nothing() {
        let machine = Machine::new(true);
        assert_eq!(status(&remove(&machine)), "unchanged");
        assert!(machine.calls().is_empty());
        assert!(machine.present.get(), "not Marsa's to remove");
    }

    #[test]
    fn a_delete_that_leaves_the_certificate_keeps_the_file() {
        let mut machine = Machine::new(true).with_file(&pem());
        machine.delete_works = false;
        assert_eq!(status(&remove(&machine)), TRUST_REMOVE_FAILED);
        assert!(machine.file().is_some(), "kept for a retry");
    }

    #[test]
    fn a_file_that_is_not_one_certificate_is_left_alone() {
        let machine = Machine::new(true).with_file("not a certificate");
        for result in [remove(&machine), install(&pem(), &machine)] {
            assert_eq!(status(&result), TRUST_READ);
        }
        assert!(machine.calls().is_empty());
        assert_eq!(machine.file(), Some(b"not a certificate".to_vec()));
    }

    #[test]
    fn a_query_that_fails_is_a_failure_of_the_op() {
        let mut machine = Machine::new(true).with_file(&pem());
        machine.query_code = Some(1);
        assert_eq!(status(&remove(&machine)), TRUST_REMOVE_FAILED);
        assert_eq!(status(&install(&pem(), &machine)), TRUST_ADD_FAILED);
        assert!(machine.file().is_some());
    }

    #[test]
    fn without_the_install_folder_nothing_runs() {
        let machine = Machine::new(false);
        machine_without_folder(machine, |machine| {
            assert_eq!(status(&install(&pem(), machine)), TRUST_WRITE);
            assert_eq!(status(&remove(machine)), TRUST_READ);
            assert!(machine.calls().is_empty());
        });
    }

    fn machine_without_folder(mut machine: Machine, check: impl Fn(&Machine)) {
        machine.fake.install_dir = None;
        check(&machine);
    }

    #[test]
    fn the_thumbprint_is_lower_case_sha1_hex() {
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    /// Through the parser and `ops::apply`, as the service runs it; and the
    /// removal that `uninstall_all` runs on Windows.
    #[test]
    fn requests_trust_and_untrust_on_windows() {
        let apply = |ops: serde_json::Value, machine: &Machine| {
            let raw = json!({ "version": 1, "ops": ops }).to_string();
            let request = Request::from_arg(OsStr::new(&raw)).unwrap();
            let results = ops::apply(&request, Os::Windows, machine);
            results
                .iter()
                .map(|r| (r.op, status(r)))
                .collect::<Vec<_>>()
        };
        let machine = Machine::new(false);
        let trust = json!([{ "op": "trust_install", "pem": pem() }]);
        assert_eq!(apply(trust, &machine), [(INSTALL, "done")]);
        let all = apply(json!([{ "op": "uninstall_all" }]), &machine);
        assert!(all.contains(&(REMOVE, "done")), "{all:?}");
        assert!(!machine.present.get());
    }
}
