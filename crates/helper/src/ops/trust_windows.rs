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
//! `certutil` reads its certificate id as an index, a serial number, a
//! name, or a hash. An id of digits only is an index: `-store Root 000…0`
//! lists the store's first certificate. So a thumbprint without a letter
//! (about 7 in a billion) is never passed: `trust_install` refuses that
//! certificate. A serial number matches as a number (`000…0a` finds a root
//! whose serial number is `0a`), so another root whose serial number equals
//! the 160-bit thumbprint, or whose name is that text, would match too; only
//! an administrator can put such a root there.
//!
//! The same certificate in the store without Marsa's file (Marsa's file was
//! deleted, or `caddy trust` ran elevated on Marsa's own CA) is recorded as
//! Marsa's: the CA's key is Marsa's own, and `marsa uninstall --all` must
//! leave nothing. Only the machine store is touched: a copy in one user's
//! own Root store stays.
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
    let thumbprint = sha1_hex(&der);
    if !is_hash_id(&thumbprint) {
        let message = "its SHA-1 has no letter, and certutil reads such an id as an index";
        return OpResult::failed(INSTALL, TRUST_CERT_INVALID, message.to_owned());
    }
    let path = match ca_path(INSTALL, TRUST_WRITE, system) {
        Ok(path) => path,
        Err(failed) => return failed,
    };
    // `certutil` gets the file's path as text.
    let Some(file) = path.to_str() else {
        let message = format!("{} is not valid Unicode", path.display());
        return OpResult::failed(INSTALL, TRUST_WRITE, message);
    };
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
        Ok(None) => match in_store(INSTALL, &thumbprint, system) {
            // Trusted already, without Marsa's file: recorded as Marsa's
            // (see the module doc), and the store stays as it is.
            Ok(true) => {
                return match write_file(&path, &der, system) {
                    Ok(()) => result(INSTALL, Outcome::Unchanged),
                    Err(failed) => failed,
                };
            }
            Ok(false) => {}
            Err(failed) => return failed,
        },
        Err(failed) => return failed,
    }
    if let Err(failed) = write_file(&path, &der, system) {
        return failed;
    }
    let added = system.run_system_tool(CERTUTIL, &["-addstore", STORE, file]);
    if !program::succeeded(&added) {
        // `-addstore` can stop half way, after it added the certificate. The
        // steps of `trust_remove` undo that, and they delete the file only if
        // every step worked, so the thumbprint is never lost. The failed add
        // is the error either way.
        let _ = remove_steps(INSTALL, &path, &thumbprint, system);
        return command_failed(INSTALL, TRUST_ADD_FAILED, "-addstore", &added);
    }
    // `-addstore` exits with 0 when the certificate is already there too, so
    // the store itself is the check.
    match in_store(INSTALL, &thumbprint, system) {
        Ok(true) => result(INSTALL, Outcome::Done),
        Ok(false) => {
            // Nothing of it is trusted, so the record goes too.
            let _ = remove_steps(INSTALL, &path, &thumbprint, system);
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

/// Writes Marsa's clean copy of the certificate.
fn write_file(path: &Path, der: &[u8], system: &impl System) -> Result<(), OpResult> {
    system
        .write_atomic(path, &to_pem(der), MODE)
        .map_err(|err| {
            let message = format!("cannot write {}: {}", path.display(), err.kind());
            OpResult::failed(INSTALL, TRUST_WRITE, message)
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
    // `install` refuses such a certificate, so only a file that Marsa did not
    // write gets here. As an index, it would delete another root.
    if !is_hash_id(thumbprint) {
        let message = format!(
            "{} holds a certificate whose SHA-1 has no letter, so it was left as it is",
            path.display()
        );
        return Err(OpResult::failed(op, TRUST_REMOVE_FAILED, message));
    }
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

/// Whether `certutil` can read `thumbprint` only as a hash, not as an
/// index: it holds a letter (see the module doc).
fn is_hash_id(thumbprint: &str) -> bool {
    thumbprint.bytes().any(|byte| byte.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeSet;
    use std::ffi::OsStr;
    use std::fs;

    use serde_json::json;

    use super::*;
    use crate::ops;
    use crate::os::Os;
    use crate::request::Request;
    use crate::system::FakeSystem;

    const INSTALL_DIR: &str = r"C:\Program Files\Marsa";
    const SYSTEM_DIR: &str = r"C:\Windows\System32";

    fn pem() -> String {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        fs::read_to_string(root.join("fixtures").join("helper").join("test-ca.pem")).unwrap()
    }

    fn thumbprint() -> String {
        sha1_hex(&parse_pem(&pem()).unwrap())
    }

    /// Another certificate: the test CA with one byte of its signature
    /// changed. Its PEM and its thumbprint.
    fn other() -> (String, String) {
        let mut der = parse_pem(&pem()).unwrap();
        *der.last_mut().unwrap() ^= 1;
        let old = sha1_hex(&der);
        assert_ne!(old, thumbprint());
        (String::from_utf8(to_pem(&der)).unwrap(), old)
    }

    fn ca_file() -> PathBuf {
        Path::new(INSTALL_DIR).join(FILE)
    }

    /// A Windows machine whose Root store `certutil` changes: files and the
    /// call log come from a [`FakeSystem`], and `run_system_tool` acts like
    /// `certutil` on a store of thumbprints.
    struct Machine {
        fake: FakeSystem,
        /// The thumbprints in the Root store.
        store: RefCell<BTreeSet<String>>,
        /// `-addstore` puts the certificate of its file there, and
        /// `-delstore` takes the one it names out.
        add_works: bool,
        delete_works: bool,
        /// The exit code of `-addstore`; `None` is a time out.
        add_code: Option<i32>,
        /// The exit code of `-delstore`.
        delete_code: i32,
        /// Every `-store` exits with this code, if set.
        query_code: Option<i32>,
        /// `-store` calls that still answer as asked; after them, every one
        /// exits with `query_code`.
        queries_before_failing: Cell<usize>,
    }

    impl Machine {
        fn new(present: bool) -> Self {
            let machine = Self {
                fake: FakeSystem {
                    install_dir: Some(PathBuf::from(INSTALL_DIR)),
                    system_dir: Some(PathBuf::from(SYSTEM_DIR)),
                    ..FakeSystem::default()
                },
                store: RefCell::default(),
                add_works: true,
                delete_works: true,
                add_code: Some(0),
                delete_code: 0,
                query_code: None,
                queries_before_failing: Cell::new(0),
            };
            if present {
                machine.store.borrow_mut().insert(thumbprint());
            }
            machine
        }

        fn with_file(self, text: &str) -> Self {
            let file = (text.as_bytes().to_vec(), MODE);
            self.fake.files.borrow_mut().insert(ca_file(), file);
            self
        }

        fn with_in_store(self, thumbprint: &str) -> Self {
            self.store.borrow_mut().insert(thumbprint.to_owned());
            self
        }

        fn holds(&self, thumbprint: &str) -> bool {
            self.store.borrow().contains(thumbprint)
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

        fn query(&self, id: &str) -> io::Result<RunOutput> {
            let left = self.queries_before_failing.get();
            match self.query_code {
                Some(code) if left == 0 => return exit(code),
                Some(_) => self.queries_before_failing.set(left - 1),
                None => {}
            }
            exit(if self.holds(id) { 0 } else { NOT_FOUND })
        }

        fn add(&self, file: &str) -> io::Result<RunOutput> {
            if self.add_works {
                let bytes = self.fake.read(Path::new(file), MAX_FILE_BYTES).unwrap();
                let der = parse_pem(std::str::from_utf8(&bytes).unwrap()).unwrap();
                self.store.borrow_mut().insert(sha1_hex(&der));
            }
            match self.add_code {
                Some(code) => exit(code),
                None => Err(io::ErrorKind::TimedOut.into()),
            }
        }

        fn delete(&self, id: &str) -> io::Result<RunOutput> {
            if self.delete_works {
                self.store.borrow_mut().remove(id);
            }
            // Like certutil: 0 whether or not anything matched, unless set.
            exit(self.delete_code)
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

        fn is_link(&self, path: &Path) -> bool {
            self.fake.is_link(path)
        }

        fn is_read_only(&self, path: &Path) -> bool {
            self.fake.is_read_only(path)
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
            self.fake.run(program, args)
        }

        fn current_exe(&self) -> io::Result<PathBuf> {
            self.fake.current_exe()
        }

        fn system_dir(&self) -> io::Result<PathBuf> {
            self.fake.system_dir()
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
            match args {
                ["-store", STORE, id] => self.query(id),
                ["-addstore", STORE, file] => self.add(file),
                ["-delstore", STORE, id] => self.delete(id),
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

    fn message(result: &OpResult) -> &str {
        match &result.outcome {
            Outcome::Failed { message, .. } => message,
            _ => panic!("not failed: {:?}", result.outcome),
        }
    }

    fn store(id: &str) -> String {
        format!("{CERTUTIL} -store Root {id}")
    }

    fn add() -> String {
        format!("{CERTUTIL} -addstore Root {}", ca_file().display())
    }

    fn delete(id: &str) -> String {
        format!("{CERTUTIL} -delstore Root {id}")
    }

    fn our_file() -> Option<Vec<u8>> {
        Some(to_pem(&parse_pem(&pem()).unwrap()))
    }

    #[test]
    fn install_checks_the_store_writes_the_file_adds_it_and_checks_again() {
        let machine = Machine::new(false);
        let result = install(&pem(), &machine);
        assert_eq!((result.op, status(&result)), (INSTALL, "done"));
        let ours = thumbprint();
        assert!(machine.holds(&ours));
        assert_eq!(machine.file(), our_file());
        assert_eq!(machine.calls(), [store(&ours), add(), store(&ours)]);
    }

    #[test]
    fn the_same_certificate_already_in_the_store_is_unchanged() {
        let machine = Machine::new(true).with_file(&pem());
        let result = install(&pem(), &machine);
        assert_eq!(status(&result), "unchanged");
        assert_eq!(machine.calls(), [store(&thumbprint())], "only the query");
    }

    /// No file, but the store holds it: recorded as Marsa's, so that
    /// `trust_remove` takes it out later. Nothing is added.
    #[test]
    fn the_same_certificate_trusted_without_the_file_is_recorded() {
        let machine = Machine::new(true);
        assert_eq!(status(&install(&pem(), &machine)), "unchanged");
        assert_eq!(machine.calls(), [store(&thumbprint())]);
        assert_eq!(machine.file(), our_file());
        assert_eq!(status(&remove(&machine)), "done");
        assert!(!machine.holds(&thumbprint()));
    }

    #[test]
    fn the_same_certificate_removed_by_hand_is_added_again() {
        let machine = Machine::new(false).with_file(&pem());
        assert_eq!(status(&install(&pem(), &machine)), "done");
        assert!(machine.holds(&thumbprint()));
    }

    /// The file names another certificate, which is in the store: it goes
    /// by its own thumbprint, then the new one is added.
    #[test]
    fn a_different_certificate_from_before_is_removed_first() {
        let (other_pem, old) = other();
        let machine = Machine::new(false)
            .with_file(&other_pem)
            .with_in_store(&old);
        assert_eq!(status(&install(&pem(), &machine)), "done");
        let ours = thumbprint();
        assert_eq!(
            machine.calls(),
            [store(&old), delete(&old), store(&old), add(), store(&ours)]
        );
        assert!(!machine.holds(&old) && machine.holds(&ours));
        assert_eq!(machine.file(), our_file());
    }

    /// The old certificate stays in the store: nothing new is written or
    /// added, and the file still names the old one.
    #[test]
    fn an_old_certificate_that_stays_stops_the_install() {
        let (other_pem, old) = other();
        let mut machine = Machine::new(false)
            .with_file(&other_pem)
            .with_in_store(&old);
        machine.delete_works = false;
        assert_eq!(status(&install(&pem(), &machine)), TRUST_REMOVE_FAILED);
        assert_eq!(machine.calls(), [store(&old), delete(&old), store(&old)]);
        assert_eq!(machine.file(), Some(other_pem.into_bytes()));
        assert!(!machine.holds(&thumbprint()));
    }

    #[test]
    fn an_add_that_leaves_nothing_in_the_store_fails_and_keeps_no_record() {
        let mut machine = Machine::new(false);
        machine.add_works = false;
        let result = install(&pem(), &machine);
        assert_eq!(status(&result), TRUST_ADD_FAILED);
        assert!(machine.file().is_none(), "nothing of it is trusted");
    }

    #[test]
    fn a_failed_add_names_the_exit_and_not_the_output() {
        let mut machine = Machine::new(false);
        machine.add_works = false;
        machine.add_code = Some(5);
        let result = install(&pem(), &machine);
        assert_eq!(status(&result), TRUST_ADD_FAILED);
        assert_eq!(message(&result), "certutil -addstore exited with code 5");
        assert!(machine.file().is_none());
    }

    /// `-addstore` added the certificate and then timed out: the steps of
    /// `trust_remove` take it out again, and the add is the error.
    #[test]
    fn an_add_that_times_out_half_way_is_undone() {
        let mut machine = Machine::new(false);
        machine.add_code = None;
        let result = install(&pem(), &machine);
        assert_eq!(status(&result), TRUST_ADD_FAILED);
        assert_eq!(message(&result), "certutil -addstore timed out");
        let ours = thumbprint();
        assert!(!machine.holds(&ours) && machine.file().is_none());
        assert_eq!(
            machine.calls(),
            [
                store(&ours),
                add(),
                store(&ours),
                delete(&ours),
                store(&ours)
            ]
        );
    }

    /// The undo fails too: the file stays, so `trust_remove` can finish.
    #[test]
    fn a_failed_add_whose_undo_fails_keeps_the_file() {
        let mut machine = Machine::new(false);
        machine.add_code = None;
        machine.delete_works = false;
        assert_eq!(status(&install(&pem(), &machine)), TRUST_ADD_FAILED);
        assert_eq!(machine.file(), our_file());
        machine.delete_works = true;
        assert_eq!(status(&remove(&machine)), "done");
        assert!(!machine.holds(&thumbprint()) && machine.file().is_none());
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
        let ours = thumbprint();
        assert!(!machine.holds(&ours) && machine.file().is_none());
        assert_eq!(machine.calls(), [store(&ours), delete(&ours), store(&ours)]);
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
        assert!(machine.holds(&thumbprint()), "not Marsa's to remove");
    }

    #[test]
    fn a_delete_that_leaves_the_certificate_keeps_the_file() {
        let mut machine = Machine::new(true).with_file(&pem());
        machine.delete_works = false;
        let result = remove(&machine);
        assert_eq!(status(&result), TRUST_REMOVE_FAILED);
        assert!(message(&result).contains("still in the store"));
        assert!(machine.file().is_some(), "kept for a retry");
    }

    #[test]
    fn a_failed_delete_names_its_exit_and_keeps_the_file() {
        let mut machine = Machine::new(true).with_file(&pem());
        machine.delete_works = false;
        machine.delete_code = 1;
        let result = remove(&machine);
        assert_eq!(status(&result), TRUST_REMOVE_FAILED);
        assert_eq!(message(&result), "certutil -delstore exited with code 1");
        assert!(machine.file().is_some());
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

    /// The check after `-delstore` fails: not `done`, and the file stays.
    #[test]
    fn a_check_that_fails_after_the_delete_keeps_the_file() {
        let mut machine = Machine::new(true).with_file(&pem());
        machine.query_code = Some(1);
        machine.queries_before_failing.set(1);
        let result = remove(&machine);
        assert_eq!(status(&result), TRUST_REMOVE_FAILED);
        assert_eq!(message(&result), "certutil -store exited with code 1");
        assert!(machine.file().is_some());
    }

    #[test]
    fn without_the_install_folder_nothing_runs() {
        let mut machine = Machine::new(false);
        machine.fake.install_dir = None;
        assert_eq!(status(&install(&pem(), &machine)), TRUST_WRITE);
        assert_eq!(status(&remove(&machine)), TRUST_READ);
        assert!(machine.calls().is_empty());
    }

    #[test]
    fn the_thumbprint_is_lower_case_sha1_hex() {
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn only_a_thumbprint_with_a_letter_is_a_hash_for_certutil() {
        assert!(is_hash_id(&thumbprint()));
        assert!(is_hash_id(&format!("{}a", "0".repeat(39))));
        assert!(!is_hash_id(&"0".repeat(40)));
        assert!(!is_hash_id(&"1234567890".repeat(4)));
    }

    /// A file that Marsa did not write, whose certificate's SHA-1 has no
    /// letter: as an index it would name another root, so nothing runs.
    #[test]
    fn a_stored_thumbprint_without_a_letter_is_never_passed() {
        let machine = Machine::new(false);
        let path = ca_file();
        let result = remove_steps(REMOVE, &path, &"0".repeat(40), &machine);
        let failed = result.unwrap_err();
        assert_eq!(status(&failed), TRUST_REMOVE_FAILED);
        assert!(machine.calls().is_empty());
    }

    /// Through the parser and `ops::apply`, as the service runs it; and the
    /// removals that `uninstall_all` runs on Windows.
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
        assert_eq!(all, [("hosts_remove", "unchanged"), (REMOVE, "done")]);
        assert!(!machine.holds(&thumbprint()));
    }
}
