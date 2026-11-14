//! Linux: trust of Marsa's CA root certificate in the system store
//! (ADR 0014). The macOS twin is `trust`.
//!
//! The helper writes its own clean copy of the certificate where the
//! system's own tool looks for local roots, then runs that tool. It picks
//! the family by the tool and the folder that exist:
//!
//! | Family | The file | The tool |
//! | --- | --- | --- |
//! | Debian, Ubuntu | `/usr/local/share/ca-certificates/marsa-local-ca.crt` | `/usr/sbin/update-ca-certificates --fresh` |
//! | Fedora | `/etc/pki/ca-trust/source/anchors/marsa-local-ca.pem` | `/usr/bin/update-ca-trust` |
//!
//! The tool alone does not tell: openSUSE has `update-ca-certificates` and
//! Arch has `update-ca-trust`, but each reads another folder. Without the
//! folder, the helper would write a file nothing reads and report success.
//!
//! `--fresh` makes Debian's tool rebuild its links. Without it, a new
//! certificate in the same file keeps the old hash link, and a deleted one
//! leaves dangling links. Fedora's tool rebuilds everything each time.
//!
//! The tool runs also when the file is as wanted: a run cut short (a killed
//! helper, a failed run) is then finished by the next one. It changes
//! nothing in a store that is right.
//!
//! The request carries the PEM text, never a path (ADR 0011), and the strict
//! parser of `trust` takes exactly one certificate. The file is also the
//! record of what Marsa trusted. A file at that path that is not exactly one
//! certificate was not written by the helper, and is left as it is.
//!
//! This covers `curl`, PHP, and most programs. Chrome and Firefox keep their
//! own stores in the user's folders: `marsad` adds the certificate there as
//! the user (task M4-18g).
//!
//! No message holds any part of the PEM or the output of a tool.
//!
//! Known limits: a tool that times out is killed, but a program it started
//! may still write the store later. On Debian the folder of local roots may
//! belong to the group `staff`, which Debian counts as root. Java's own
//! store (`ca-certificates-java`) may keep the certificate after a removal.

use std::io;
use std::path::Path;

use crate::ops::program;
use crate::ops::trust::{self, TRUST_CERT_INVALID, TRUST_DELETE, TRUST_READ, TRUST_WRITE};
use crate::response::{OpResult, Outcome};
use crate::system::{RunOutput, System};

/// This system has neither of the two tools, so nothing was changed.
pub const TRUST_TOOL_MISSING: &str = "trust_tool_missing";
/// The store's tool failed, in an install or a removal. Not the codes of
/// the macOS op (`trust_add_failed`, `trust_remove_failed`): the UI explains
/// those with macOS steps (a dialog, Keychain Access).
pub const TRUST_STORE_UPDATE_FAILED: &str = "trust_store_update_failed";

/// Where a family keeps local roots, and the tool that rebuilds its store.
struct Store {
    /// The folder the tool reads local roots from. It must exist.
    folder: &'static str,
    file: &'static str,
    tool: &'static str,
    args: &'static [&'static str],
}

/// The file must end in `.crt`: the tool skips any other name.
const DEBIAN: Store = Store {
    folder: "/usr/local/share/ca-certificates",
    file: "/usr/local/share/ca-certificates/marsa-local-ca.crt",
    tool: "/usr/sbin/update-ca-certificates",
    args: &["--fresh"],
};
const FEDORA: Store = Store {
    folder: "/etc/pki/ca-trust/source/anchors",
    file: "/etc/pki/ca-trust/source/anchors/marsa-local-ca.pem",
    tool: "/usr/bin/update-ca-trust",
    args: &[],
};
/// Debian's first: its tool has a name of its own.
const STORES: [&Store; 2] = [&DEBIAN, &FEDORA];

/// Everyone can read a certificate, and only root can change it.
const MODE: u32 = 0o644;
/// The largest file the helper reads at a store path. The clean copy of a
/// 16 KiB request is a little larger than 16 KiB.
const MAX_FILE_BYTES: u64 = 32 * 1024;

const INSTALL: &str = "trust_install";
const REMOVE: &str = "trust_remove";

/// `trust_install`: every program that uses the system store trusts the one
/// certificate in `pem`. A different certificate that Marsa trusted before
/// is removed first.
pub fn install(pem: &str, system: &impl System) -> OpResult {
    let der = match trust::parse_pem(pem) {
        Ok(der) => der,
        Err(reason) => {
            return OpResult::failed(INSTALL, TRUST_CERT_INVALID, format!("pem {reason}"));
        }
    };
    let Some(store) = store(system) else {
        let message = format!(
            "this system has neither {} with {} nor {} with {}",
            DEBIAN.tool, DEBIAN.folder, FEDORA.tool, FEDORA.folder
        );
        return OpResult::failed(INSTALL, TRUST_TOOL_MISSING, message);
    };
    match stored_hash(INSTALL, store, system) {
        // The file is right: the run finishes one that was cut short.
        Ok(Some(old)) if old == trust::sha256_hex(&der) => {
            let updated = update(store, system);
            if program::succeeded(&updated) {
                return result(INSTALL, Outcome::Unchanged);
            }
            let message = format!(
                "{} {}; {} stays, and running setup again tries again",
                store.tool,
                program::failure(&updated),
                store.file
            );
            return OpResult::failed(INSTALL, TRUST_STORE_UPDATE_FAILED, message);
        }
        // Nothing there, or another certificate of Marsa's: written over.
        Ok(_) => {}
        Err(failed) => return failed,
    }
    if let Err(err) = system.write_atomic(Path::new(store.file), &trust::to_pem(&der), MODE) {
        let message = format!("cannot write {}: {}", store.file, err.kind());
        return OpResult::failed(INSTALL, TRUST_WRITE, message);
    }
    let updated = update(store, system);
    if program::succeeded(&updated) {
        return result(INSTALL, Outcome::Done);
    }
    // The tool may have stopped half way. Without the file, one more run
    // puts the store back without any certificate of Marsa's.
    let cleanup = match system.remove(Path::new(store.file)) {
        Ok(()) => "was removed again",
        Err(_) => "could not be removed",
    };
    let again = update(store, system);
    let mut message = format!(
        "{} {}; {} {cleanup}",
        store.tool,
        program::failure(&updated),
        store.file
    );
    if !program::succeeded(&again) {
        message.push_str(&format!(
            ", but {} {} again; run it again to drop the certificate from the store",
            store.tool,
            program::failure(&again)
        ));
    }
    OpResult::failed(INSTALL, TRUST_STORE_UPDATE_FAILED, message)
}

/// `trust_remove`: deletes Marsa's file and rebuilds the store without it.
/// Without the file the tool still runs, so a removal whose run failed is
/// finished now. On a system with neither tool, a file of Marsa's at either
/// path still goes: it must not come back when the tool does.
pub fn remove(system: &impl System) -> OpResult {
    let Some(store) = store(system) else {
        return remove_without_tool(system);
    };
    match stored_hash(REMOVE, store, system) {
        Ok(None) => {
            let updated = update(store, system);
            if program::succeeded(&updated) {
                return result(REMOVE, Outcome::Unchanged);
            }
            let message = format!(
                "{} {}; run it again to make sure no certificate of Marsa's is in the store",
                store.tool,
                program::failure(&updated)
            );
            OpResult::failed(REMOVE, TRUST_STORE_UPDATE_FAILED, message)
        }
        Ok(Some(_)) => match remove_steps(REMOVE, store, system) {
            Ok(()) => result(REMOVE, Outcome::Done),
            Err(failed) => failed,
        },
        Err(failed) => failed,
    }
}

/// [`remove`] on a system with neither tool: deletes a file of Marsa's at
/// either path. Nothing can rebuild the store, so a deleted file is
/// `trust_tool_missing`.
fn remove_without_tool(system: &impl System) -> OpResult {
    let mut deleted = Vec::new();
    for store in STORES {
        match stored_hash(REMOVE, store, system) {
            Ok(None) => {}
            Ok(Some(_)) => {
                if let Err(err) = system.remove(Path::new(store.file)) {
                    let message = format!("cannot delete {}: {}", store.file, err.kind());
                    return OpResult::failed(REMOVE, TRUST_DELETE, message);
                }
                deleted.push(store.file);
            }
            Err(failed) => return failed,
        }
    }
    if deleted.is_empty() {
        return result(REMOVE, Outcome::Unchanged);
    }
    let message = format!(
        "{} was deleted, but this system has no tool to rebuild its store, which may keep \
         the certificate until its tool runs",
        deleted.join(" and ")
    );
    OpResult::failed(REMOVE, TRUST_TOOL_MISSING, message)
}

/// The store of this system: the first whose tool and folder both exist.
fn store(system: &impl System) -> Option<&'static Store> {
    let exists = |path: &str| system.exists(Path::new(path));
    STORES
        .into_iter()
        .find(|store| exists(store.tool) && exists(store.folder))
}

/// One run of the store's tool.
fn update(store: &Store, system: &impl System) -> io::Result<RunOutput> {
    system.run(store.tool, store.args)
}

/// The SHA-256 of the certificate in the store's file, or `None` if nothing
/// is there.
fn stored_hash(
    op: &'static str,
    store: &Store,
    system: &impl System,
) -> Result<Option<String>, OpResult> {
    let path = Path::new(store.file);
    // Marsa never writes a link, so one here is someone else's, whatever it
    // points to: `read` would follow it.
    if system.is_link(path) {
        let message = format!("{} is a link, so it was left as it is", store.file);
        return Err(OpResult::failed(op, TRUST_READ, message));
    }
    let reason = match system.read(path, MAX_FILE_BYTES) {
        Ok(bytes) => match std::str::from_utf8(&bytes).map(trust::parse_pem) {
            Ok(Ok(der)) => return Ok(Some(trust::sha256_hex(&der))),
            _ => "is not one certificate".to_owned(),
        },
        // A link that appeared after the check above: `read` follows it and
        // `exists` does not, so a dangling one is something there.
        Err(err) if err.kind() == io::ErrorKind::NotFound && !system.exists(path) => {
            return Ok(None);
        }
        Err(err) => format!("cannot be read: {}", err.kind()),
    };
    let message = format!("{} {reason}, so it was left as it is", store.file);
    Err(OpResult::failed(op, TRUST_READ, message))
}

/// Deletes the store's file, then runs the tool, which drops the
/// certificate from the store. A failed delete changes nothing.
fn remove_steps(op: &'static str, store: &Store, system: &impl System) -> Result<(), OpResult> {
    if let Err(err) = system.remove(Path::new(store.file)) {
        let message = format!("cannot delete {}: {}", store.file, err.kind());
        return Err(OpResult::failed(op, TRUST_DELETE, message));
    }
    let updated = update(store, system);
    if !program::succeeded(&updated) {
        let message = format!(
            "{} was deleted, but {} {}; run it again to drop the certificate from the store",
            store.file,
            store.tool,
            program::failure(&updated)
        );
        return Err(OpResult::failed(op, TRUST_STORE_UPDATE_FAILED, message));
    }
    Ok(())
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
    use crate::system::{FakeSystem, RunOutput};

    /// A throwaway self-signed CA, in the clean form the helper writes. Its
    /// key was deleted at once and never entered the repository.
    const TEST_CA: &str = include_str!("../../../../fixtures/helper/test-ca.pem");

    // Written out again, not taken from the constants above: a test must
    // fail if one of those changes.
    const DEBIAN_FILE: &str = "/usr/local/share/ca-certificates/marsa-local-ca.crt";
    const DEBIAN_TOOL: &str = "/usr/sbin/update-ca-certificates";
    const DEBIAN_FOLDER: &str = "/usr/local/share/ca-certificates";
    const FEDORA_FILE: &str = "/etc/pki/ca-trust/source/anchors/marsa-local-ca.pem";
    const FEDORA_TOOL: &str = "/usr/bin/update-ca-trust";
    const ANCHORS: &str = "/etc/pki/ca-trust/source/anchors";

    /// The mode of a file a test starts with. Marsa never writes it, so the
    /// mode shows whether an op rewrote the file.
    const OLD_MODE: u32 = 0o600;

    /// One family in a test: its tool, its file, and a fake system that has
    /// the tool, which succeeds unless a test scripts it.
    struct Family {
        tool: &'static str,
        args: &'static [&'static str],
        file: &'static str,
        fake: FakeSystem,
        /// For each run of the tool: whether the file was there.
        seen: RefCell<Vec<bool>>,
        /// That many runs of the tool fail first.
        failing_runs: Cell<u32>,
    }

    impl System for Family {
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
            self.seen.borrow_mut().push(self.stored().is_some());
            if self.failing_runs.get() > 0 {
                self.failing_runs.set(self.failing_runs.get() - 1);
                let args = args.iter().map(|arg| (*arg).to_owned()).collect();
                self.fake.log.borrow_mut().push((program.to_owned(), args));
                return Ok(exit(1));
            }
            self.fake.run(program, args)
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

    fn put(fake: &FakeSystem, path: &str, bytes: &[u8]) {
        let file = (bytes.to_vec(), OLD_MODE);
        fake.files.borrow_mut().insert(PathBuf::from(path), file);
    }

    fn debian() -> Family {
        let mut fake = FakeSystem::default();
        put(&fake, DEBIAN_TOOL, b"");
        put(&fake, DEBIAN_FOLDER, b"");
        fake.defaults.insert(DEBIAN_TOOL, exit(0));
        Family {
            tool: DEBIAN_TOOL,
            args: &["--fresh"],
            file: DEBIAN_FILE,
            fake,
            seen: RefCell::default(),
            failing_runs: Cell::new(0),
        }
    }

    fn fedora() -> Family {
        let mut fake = FakeSystem::default();
        put(&fake, FEDORA_TOOL, b"");
        put(&fake, ANCHORS, b"");
        fake.defaults.insert(FEDORA_TOOL, exit(0));
        Family {
            tool: FEDORA_TOOL,
            args: &[],
            file: FEDORA_FILE,
            fake,
            seen: RefCell::default(),
            failing_runs: Cell::new(0),
        }
    }

    fn families() -> [Family; 2] {
        [debian(), fedora()]
    }

    impl Family {
        fn with_file(self, bytes: &[u8]) -> Self {
            put(&self.fake, self.file, bytes);
            self
        }

        /// The store's file, or `None`.
        fn stored(&self) -> Option<(Vec<u8>, u32)> {
            self.fake.files.borrow().get(Path::new(self.file)).cloned()
        }

        /// How often the tool ran, with its own arguments. Nothing else may
        /// run.
        fn runs(&self) -> usize {
            let log = self.fake.log.borrow();
            let args = self.args.iter().map(|arg| (*arg).to_owned()).collect();
            let tool = (self.tool.to_owned(), args);
            assert!(log.iter().all(|call| *call == tool), "{log:?}");
            log.len()
        }

        /// For each run of the tool: whether the file was there.
        fn seen(&self) -> Vec<bool> {
            self.seen.borrow().clone()
        }
    }

    /// The test CA with one byte of its signature changed: another hash.
    fn other_ca() -> Vec<u8> {
        let mut der = trust::parse_pem(TEST_CA).unwrap();
        *der.last_mut().unwrap() ^= 1;
        trust::to_pem(&der)
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

    /// No line of the fixture's base64 is in the message.
    fn assert_no_pem_in(result: &OpResult) {
        let message = message_of(result);
        for line in TEST_CA.lines().filter(|line| !line.starts_with("-----")) {
            assert!(!message.contains(line), "{message}");
        }
    }

    #[test]
    fn install_writes_the_clean_file_and_runs_the_tool() {
        for family in families() {
            assert_eq!(install(TEST_CA, &family), result(INSTALL, Outcome::Done));
            // The fixture is in the clean form already: 64 columns, LF.
            assert_eq!(family.stored(), Some((TEST_CA.as_bytes().to_vec(), 0o644)));
            assert_eq!(family.runs(), 1, "{}", family.tool);
            assert_eq!(family.seen(), [true]);
        }
    }

    /// Text around the certificate does not reach the file that everyone
    /// reads.
    #[test]
    fn install_writes_its_own_copy_not_the_request_text() {
        let family = debian();
        let padded = format!("\n\n  {}\r\n\t", TEST_CA.replace('\n', "\r\n"));
        assert_eq!(install(&padded, &family), result(INSTALL, Outcome::Done));
        assert_eq!(family.stored(), Some((TEST_CA.as_bytes().to_vec(), 0o644)));
    }

    #[test]
    fn install_over_the_same_certificate_is_unchanged() {
        for mut family in families().map(|family| family.with_file(TEST_CA.as_bytes())) {
            // Any write fails here, so `unchanged` also shows none was tried.
            family.fake.fail_writes.insert(PathBuf::from(family.file));
            assert_eq!(
                install(TEST_CA, &family),
                result(INSTALL, Outcome::Unchanged)
            );
            assert_eq!(
                family.stored(),
                Some((TEST_CA.as_bytes().to_vec(), OLD_MODE))
            );
            // The tool still runs: it finishes a run that was cut short.
            assert_eq!(family.runs(), 1);
            assert_eq!(family.seen(), [true]);
        }
    }

    /// The same certificate, and a tool that fails: the file stays.
    #[test]
    fn a_failed_run_over_the_same_certificate_keeps_the_file() {
        for family in families().map(|family| family.with_file(TEST_CA.as_bytes())) {
            family.failing_runs.set(1);
            let failed = install(TEST_CA, &family);
            assert_eq!(code_of(&failed), TRUST_STORE_UPDATE_FAILED);
            assert_eq!(
                message_of(&failed),
                format!(
                    "{} exited with code 1; {} stays, and running setup again tries again",
                    family.tool, family.file
                )
            );
            assert_eq!(
                family.stored(),
                Some((TEST_CA.as_bytes().to_vec(), OLD_MODE))
            );
        }
    }

    /// Another certificate of Marsa's is written over, then one run: on
    /// Debian `--fresh` rebuilds its links for the new one.
    #[test]
    fn install_over_another_certificate_writes_over_it() {
        for family in families().map(|family| family.with_file(&other_ca())) {
            assert_eq!(install(TEST_CA, &family), result(INSTALL, Outcome::Done));
            assert_eq!(family.stored(), Some((TEST_CA.as_bytes().to_vec(), 0o644)));
            assert_eq!(family.runs(), 1);
            assert_eq!(family.seen(), [true]);
        }
    }

    #[test]
    fn a_pem_that_is_not_one_certificate_changes_nothing() {
        let key =
            "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIA==\n-----END PRIVATE KEY-----\n";
        let two = format!("{TEST_CA}{TEST_CA}");
        let with_key = format!("{TEST_CA}{key}");
        for pem in [key, two.as_str(), with_key.as_str(), "hello", "/etc/shadow"] {
            for family in families() {
                let failed = install(pem, &family);
                assert_eq!(code_of(&failed), TRUST_CERT_INVALID, "{pem:?}");
                assert_no_pem_in(&failed);
                assert_eq!(family.stored(), None);
                assert_eq!(family.runs(), 0);
            }
        }
    }

    /// Checked before the system is looked at, so a bad request never
    /// depends on the machine.
    #[test]
    fn a_bad_pem_is_refused_before_the_tool_is_looked_for() {
        let fake = FakeSystem::default();
        assert_eq!(code_of(&install("hello", &fake)), TRUST_CERT_INVALID);
    }

    #[test]
    fn a_system_with_neither_tool_is_trust_tool_missing_and_remove_is_unchanged() {
        // No tool at all; Arch Linux: `update-ca-trust` with another folder
        // for local roots; openSUSE: `update-ca-certificates` with another
        // folder too.
        let arch = FakeSystem::default();
        put(&arch, FEDORA_TOOL, b"");
        let opensuse = FakeSystem::default();
        put(&opensuse, DEBIAN_TOOL, b"");
        for fake in [FakeSystem::default(), arch, opensuse] {
            let failed = install(TEST_CA, &fake);
            assert_eq!(code_of(&failed), TRUST_TOOL_MISSING);
            assert_eq!(
                message_of(&failed),
                format!(
                    "this system has neither {DEBIAN_TOOL} with {DEBIAN_FOLDER} nor \
                     {FEDORA_TOOL} with {ANCHORS}"
                )
            );
            assert_eq!(remove(&fake), result(REMOVE, Outcome::Unchanged));
            assert!(fake.log.borrow().is_empty());
            let files = fake.files.borrow();
            let tools = [Path::new(FEDORA_TOOL), Path::new(DEBIAN_TOOL)];
            assert!(files.keys().all(|path| tools.contains(&path.as_path())));
        }
    }

    /// Ubuntu can have `update-ca-trust` from a package too.
    /// The tool is gone, but a file of Marsa's is still there: it goes, so
    /// it does not come back with the tool, and nothing can rebuild the store.
    #[test]
    fn remove_without_a_tool_still_deletes_marsa_s_file() {
        let fake = FakeSystem::default();
        put(&fake, DEBIAN_FOLDER, b"");
        put(&fake, DEBIAN_FILE, TEST_CA.as_bytes());
        let failed = remove(&fake);
        assert_eq!(code_of(&failed), TRUST_TOOL_MISSING);
        assert_eq!(
            message_of(&failed),
            format!(
                "{DEBIAN_FILE} was deleted, but this system has no tool to rebuild its store, \
                 which may keep the certificate until its tool runs"
            )
        );
        assert!(!fake.files.borrow().contains_key(Path::new(DEBIAN_FILE)));
        assert!(fake.log.borrow().is_empty());
        // A file that is not one certificate stays, as with a tool.
        let fake = FakeSystem::default();
        put(&fake, FEDORA_FILE, b"someone else's file\n");
        assert_eq!(code_of(&remove(&fake)), TRUST_READ);
        assert!(fake.files.borrow().contains_key(Path::new(FEDORA_FILE)));
    }

    #[test]
    fn the_debian_tool_wins_when_both_exist() {
        let family = debian();
        put(&family.fake, FEDORA_TOOL, b"");
        put(&family.fake, ANCHORS, b"");
        assert_eq!(install(TEST_CA, &family), result(INSTALL, Outcome::Done));
        assert!(family.stored().is_some());
        assert_eq!(family.runs(), 1);
        let files = family.fake.files.borrow();
        assert!(!files.contains_key(Path::new(FEDORA_FILE)));
    }

    #[test]
    fn a_stored_file_that_is_not_one_certificate_is_left_alone() {
        let two = format!("{TEST_CA}{TEST_CA}");
        for content in ["someone else's file\n", two.as_str(), ""] {
            for family in families().map(|family| family.with_file(content.as_bytes())) {
                let message = format!(
                    "{} is not one certificate, so it was left as it is",
                    family.file
                );
                for failed in [install(TEST_CA, &family), remove(&family)] {
                    assert_eq!(code_of(&failed), TRUST_READ);
                    assert_eq!(message_of(&failed), message);
                }
                assert_eq!(
                    family.stored(),
                    Some((content.as_bytes().to_vec(), OLD_MODE))
                );
                assert_eq!(family.runs(), 0);
            }
        }
    }

    /// A link is left alone too when it points to the very certificate:
    /// Marsa never writes one.
    #[test]
    fn a_stored_file_that_cannot_be_read_or_a_link_is_left_alone() {
        let mut unreadable = debian().with_file(TEST_CA.as_bytes());
        unreadable
            .fake
            .fail_reads
            .insert(PathBuf::from(DEBIAN_FILE));
        let mut dangling = debian();
        dangling.fake.dangling.insert(PathBuf::from(DEBIAN_FILE));
        let mut link = debian().with_file(TEST_CA.as_bytes());
        link.fake.links.insert(PathBuf::from(DEBIAN_FILE));
        let cases = [
            (unreadable, "cannot be read: permission denied"),
            (dangling, "is a link"),
            (link, "is a link"),
        ];
        for (family, reason) in cases {
            let message = format!("{DEBIAN_FILE} {reason}, so it was left as it is");
            for failed in [install(TEST_CA, &family), remove(&family)] {
                assert_eq!(code_of(&failed), TRUST_READ);
                assert_eq!(message_of(&failed), message);
            }
            assert_eq!(family.runs(), 0);
        }
    }

    #[test]
    fn a_failed_write_is_trust_write_and_the_tool_does_not_run() {
        for mut family in families() {
            family.fake.fail_writes.insert(PathBuf::from(family.file));
            let failed = install(TEST_CA, &family);
            assert_eq!(code_of(&failed), TRUST_WRITE);
            assert_eq!(
                message_of(&failed),
                format!("cannot write {}: permission denied", family.file)
            );
            assert_eq!(family.runs(), 0);
        }
    }

    /// The second run of the tool is for the store without the file.
    #[test]
    fn a_tool_that_fails_takes_the_file_out_again() {
        for family in families() {
            family.failing_runs.set(1);
            let failed = install(TEST_CA, &family);
            assert_eq!(code_of(&failed), TRUST_STORE_UPDATE_FAILED);
            assert_eq!(
                message_of(&failed),
                format!(
                    "{} exited with code 1; {} was removed again",
                    family.tool, family.file
                )
            );
            assert_no_pem_in(&failed);
            assert_eq!(family.stored(), None);
            assert_eq!(family.seen(), [true, false]);
        }
    }

    /// The second run fails too: the store may still hold the certificate,
    /// and the message says to run the tool again.
    #[test]
    fn a_second_failed_run_is_named() {
        for family in families() {
            family.failing_runs.set(2);
            let failed = install(TEST_CA, &family);
            assert_eq!(code_of(&failed), TRUST_STORE_UPDATE_FAILED);
            assert_eq!(
                message_of(&failed),
                format!(
                    "{tool} exited with code 1; {file} was removed again, but {tool} exited \
                     with code 1 again; run it again to drop the certificate from the store",
                    tool = family.tool,
                    file = family.file
                )
            );
            assert_eq!(family.seen(), [true, false]);
        }
    }

    #[test]
    fn a_file_that_cannot_be_removed_after_a_failed_tool_is_named() {
        let mut family = debian();
        family.failing_runs.set(1);
        family.fake.fail_removes.insert(PathBuf::from(DEBIAN_FILE));
        let failed = install(TEST_CA, &family);
        assert_eq!(code_of(&failed), TRUST_STORE_UPDATE_FAILED);
        assert!(message_of(&failed).ends_with(&format!("{DEBIAN_FILE} could not be removed")));
        assert!(family.stored().is_some());
    }

    /// The tool still runs: it finishes a removal whose run failed.
    #[test]
    fn remove_without_a_file_is_unchanged() {
        for family in families() {
            assert_eq!(remove(&family), result(REMOVE, Outcome::Unchanged));
            assert_eq!(family.runs(), 1);
            assert_eq!(family.seen(), [false]);
        }
    }

    #[test]
    fn remove_without_a_file_and_a_failing_tool_says_so() {
        for family in families() {
            family.failing_runs.set(1);
            let failed = remove(&family);
            assert_eq!(code_of(&failed), TRUST_STORE_UPDATE_FAILED);
            assert_eq!(
                message_of(&failed),
                format!(
                    "{} exited with code 1; run it again to make sure no certificate of \
                     Marsa's is in the store",
                    family.tool
                )
            );
        }
    }

    #[test]
    fn remove_deletes_the_file_then_runs_the_tool() {
        // Any one certificate there is Marsa's: the name is its own.
        for content in [TEST_CA.as_bytes().to_vec(), other_ca()] {
            for family in families().map(|family| family.with_file(&content)) {
                assert_eq!(remove(&family), result(REMOVE, Outcome::Done));
                assert_eq!(family.stored(), None);
                assert_eq!(family.runs(), 1);
                assert_eq!(family.seen(), [false]);
            }
        }
    }

    #[test]
    fn a_failed_delete_is_trust_delete_and_changes_nothing() {
        for mut family in families().map(|family| family.with_file(TEST_CA.as_bytes())) {
            family.fake.fail_removes.insert(PathBuf::from(family.file));
            let failed = remove(&family);
            assert_eq!(code_of(&failed), TRUST_DELETE);
            assert_eq!(
                message_of(&failed),
                format!("cannot delete {}: permission denied", family.file)
            );
            assert!(family.stored().is_some());
            assert_eq!(family.runs(), 0);
        }
    }

    #[test]
    fn a_tool_that_fails_after_the_delete_is_trust_store_update_failed() {
        for family in families().map(|family| family.with_file(TEST_CA.as_bytes())) {
            family.failing_runs.set(1);
            let failed = remove(&family);
            assert_eq!(code_of(&failed), TRUST_STORE_UPDATE_FAILED);
            assert_eq!(
                message_of(&failed),
                format!(
                    "{} was deleted, but {} exited with code 1; \
                     run it again to drop the certificate from the store",
                    family.file, family.tool
                )
            );
            assert_eq!(family.stored(), None);
        }
    }

    /// Through `apply`, as a real request, on Linux.
    #[test]
    fn a_request_installs_then_removes_the_fixture() {
        let ops = json!([
            { "op": "trust_install", "pem": TEST_CA },
            { "op": "trust_remove" },
        ]);
        let raw = json!({ "version": 1, "ops": ops }).to_string();
        let request = Request::from_arg(OsStr::new(&raw)).unwrap();
        for family in families() {
            let expected = [
                result(INSTALL, Outcome::Done),
                result(REMOVE, Outcome::Done),
            ];
            assert_eq!(apply(&request, Os::Linux, &family), expected);
            assert_eq!(family.stored(), None);
            assert_eq!(family.runs(), 2);
            assert_eq!(family.seen(), [true, false]);
        }
    }
}
