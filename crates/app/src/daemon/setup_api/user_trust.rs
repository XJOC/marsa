//! macOS: trust of Marsa's CA root for the current user, by `marsad` itself
//! and without root (ADR 0015, task M4-26).
//!
//! `security add-trusted-cert` changes trust settings, and macOS wants the
//! user to approve that in its own dialog. Behind `osascript ... with
//! administrator privileges` (ADR 0012) no dialog can open, so the root step
//! of ADR 0011 failed on macOS 26 with `trust_add_failed` (task M4-17). Here
//! the daemon, as the user, runs the same tool against the user's login
//! keychain: the dialog opens, and only this user trusts a CA whose private
//! key sits in this user's folder. Safari and Chrome read the user's trust
//! settings; Firefox has its own store (task M4-16).
//!
//! Every program runs through [`Runner`]: nothing here reads a keychain
//! itself, and the tests run no `security` at all. The certificate file is
//! the one `read_ca` checked for the report; `security` opens it again as
//! the same user, so no privilege boundary is crossed.
//!
//! The commands:
//!
//! - `security verify-cert -c <root.crt> -L -l`: exit 0 means this exact
//!   certificate is already trusted as a root, by the user, the admin, or
//!   the system domain. Trust settings are kept by the certificate itself,
//!   so another CA with the same name is not trusted by this. Nothing else
//!   runs then: `unchanged`.
//! - `security add-trusted-cert -r trustRoot -k <login keychain> <root.crt>`:
//!   puts the certificate in the login keychain (one that is there already is
//!   fine) and sets the user's trust, behind the dialog. Five minutes, like
//!   the admin prompt.
//! - For the uninstall, [`remove`]: `security remove-trusted-cert <file>`
//!   takes the user's trust away, then `security delete-certificate -Z
//!   <sha256> <login keychain>` deletes the certificate by its SHA-256. The
//!   hash comes from `security find-certificate -a -Z -p -c "Marsa Local CA"
//!   <login keychain>`, matched by the certificate's own base64 and never by
//!   name alone: another CA may carry the same name (ADR 0011).
//!
//! **The record** (task M5-13): once the user trusts the CA (`done`, or
//! `unchanged` when it was trusted already), [`install`] keeps the text it
//! checked in `<data>/trust/user-trusted.pem` ([`record_path`]), mode 0600.
//! [`remove`] takes both the file for `remove-trusted-cert` and the text it
//! matches in the keychain from that one record, so the certificate that is
//! untrusted is the one that is deleted, even if Caddy's folder was deleted
//! or its CA changed since. Without a record (a setup before this task) it
//! falls back to Caddy's current `root.crt`. The record goes once the
//! removal succeeded. One record: a CA that changes between two setups
//! replaces it, and the older CA is no longer known here.
//!
//! No message quotes what `security` printed: only the exit code.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use marsa_core::helper::OpResult;
use marsa_core::types::OpStatus;
use tokio_util::sync::CancellationToken;

use super::{CHECK_LIMIT, PEM_BEGIN, PROMPT_LIMIT, RunOutput, Runner, TRUST_INSTALL, read_ca};

/// The tool, by its absolute path (AGENTS.md section 6).
pub const SECURITY: &str = "/usr/bin/security";
/// `security add-trusted-cert` did not add the user's trust: the dialog was
/// refused or cancelled, could not open (no desktop session), or the tool
/// failed or timed out. The same code as the helper's, so the CLI text fits.
pub const TRUST_ADD_FAILED: &str = "trust_add_failed";
/// `security` could not take the trust away or delete the certificate.
pub const TRUST_REMOVE_FAILED: &str = "trust_remove_failed";

/// The op name of the removal in a report. Not the helper's `trust_remove`:
/// `uninstall_all` reports that one too, for a trust added as root before
/// ADR 0015, in the same report (task M5-13).
pub const TRUST_REMOVE: &str = "user_trust_remove";
/// The common name of Marsa's root certificate: `CA_NAME` in
/// `marsa_core::caddy`. A test checks that the two agree.
const CA_NAME: &str = "Marsa Local CA";
/// The user's login keychain, under the home folder.
const LOGIN_KEYCHAIN: [&str; 3] = ["Library", "Keychains", "login.keychain-db"];
/// What `remove-trusted-cert` prints when there is no trust to take away:
/// part of the text of `errSecItemNotFound` (-25300, "The specified item
/// could not be found in the keychain.": the user's trust settings have no
/// entry for the certificate), and of `errSecNoTrustSettings` (-25263, "No
/// Trust Settings were found.": the user has no trust settings at all, the
/// usual case before the first setup). Unverified: running it would change
/// the owner's trust settings. The helper's `ops/trust.rs` relies on the
/// first text for the admin domain.
const NO_TRUST: [&[u8]; 2] = [b"could not be found", b"No Trust Settings were found"];
/// The line before each certificate in the output of `find-certificate -Z`.
const HASH_LINE: &str = "SHA-256 hash: ";
const PEM_END: &str = "-----END CERTIFICATE-----";
/// The record of the certificate this user trusts, under the data folder.
const RECORD: [&str; 2] = ["trust", "user-trusted.pem"];
/// `read_ca`'s answer for a file that is not there.
const NO_FILE: &str = "ca_not_ready";

/// The login keychain of the user whose home folder is `home`.
pub fn login_keychain(home: &Path) -> PathBuf {
    LOGIN_KEYCHAIN
        .iter()
        .fold(home.to_path_buf(), |path, part| path.join(part))
}

/// `<data_dir>/trust/user-trusted.pem`: the certificate this user trusted,
/// as [`install`] checked it.
pub fn record_path(data_dir: &Path) -> PathBuf {
    RECORD
        .iter()
        .fold(data_dir.to_path_buf(), |path, part| path.join(part))
}

/// `trust_install` for the current user: `unchanged` if the certificate at
/// `ca_root` is trusted already, `done` once macOS added the trust behind its
/// dialog, or `trust_add_failed`. `pem` is the text `read_ca` checked at
/// `ca_root`; after `done` or `unchanged` it is kept at `record` (see the
/// module notes).
pub fn install(
    runner: &dyn Runner,
    home: &Path,
    ca_root: &Path,
    pem: &str,
    record: &Path,
    stop: &CancellationToken,
) -> OpResult {
    if let Some(problem) = not_absolute(home, ca_root, record) {
        tracing::warn!(problem, "security does not run");
        let message = format!("{problem}, so security did not run");
        return failed(TRUST_INSTALL, TRUST_ADD_FAILED, message);
    }
    if is_trusted(runner, ca_root, stop) {
        tracing::info!("Marsa's CA root is already trusted for the current user");
        keep_record(record, pem);
        return result(TRUST_INSTALL, OpStatus::Unchanged);
    }
    tracing::info!("asking macOS to trust Marsa's CA root for the current user");
    let args = add_args(&login_keychain(home), ca_root);
    let added = runner.run(SECURITY, &args, PROMPT_LIMIT, stop);
    if succeeded(&added) {
        keep_record(record, pem);
        return result(TRUST_INSTALL, OpStatus::Done);
    }
    let how = failure_text(&added, PROMPT_LIMIT);
    tracing::warn!(outcome = %how, "macOS did not add the user's trust for Marsa's CA root");
    // The dialog shows in the session `marsad` runs in, not in the one
    // `marsa setup` was typed in: a daemon started over SSH has none.
    let message = format!(
        "macOS did not add the trust for the current user: security add-trusted-cert {how}. \
         Run \"marsa setup\" in Terminal on the Mac's desktop and approve the dialog; if \
         Marsa was started over SSH, run \"marsa daemon stop\" first"
    );
    failed(TRUST_INSTALL, TRUST_ADD_FAILED, message)
}

/// `trust_remove` for the current user, for the uninstall: takes the user's
/// trust away from the certificate in `record`, then deletes the certificate
/// with that text from the login keychain, and then the record. Without a
/// record, the same with Caddy's `ca_root`. `unchanged` if neither the trust
/// nor the certificate was there, or there is no file to name either.
///
/// A record that is there but cannot be used fails with
/// `trust_remove_failed` before any `security` call, and stays: the trust it
/// names may still be there, and `root.crt` may be another certificate.
pub fn remove(
    runner: &dyn Runner,
    home: &Path,
    ca_root: &Path,
    record: &Path,
    stop: &CancellationToken,
) -> OpResult {
    if let Some(problem) = not_absolute(home, ca_root, record) {
        tracing::warn!(problem, "security does not run");
        let message = format!("{problem}, so security did not run");
        return failed(TRUST_REMOVE, TRUST_REMOVE_FAILED, message);
    }
    let (file, pem, recorded) = match trusted_file(ca_root, record) {
        Ok(Some(found)) => found,
        Ok(None) => {
            tracing::info!("no record and no CA root certificate: no user trust to remove");
            return result(TRUST_REMOVE, OpStatus::Unchanged);
        }
        Err(problem) => {
            tracing::warn!(problem, "security does not run");
            let message = format!("{problem}, so security did not run");
            return failed(TRUST_REMOVE, TRUST_REMOVE_FAILED, message);
        }
    };
    let removed = remove_trust(runner, home, file, &pem, stop);
    if recorded && removed.status != OpStatus::Failed {
        forget_record(record);
    }
    removed
}

/// The file to untrust and its text: the record if there is one, else
/// Caddy's `root.crt`, and whether it is the record. `None` if neither is
/// there.
fn trusted_file<'a>(
    ca_root: &'a Path,
    record: &'a Path,
) -> Result<Option<(&'a Path, String, bool)>, &'static str> {
    match read_ca(record) {
        Ok(pem) => return Ok(Some((record, pem, true))),
        Err(NO_FILE) => {}
        Err(_) => return Err("the record of the trusted certificate cannot be used"),
    }
    match read_ca(ca_root) {
        Ok(pem) => Ok(Some((ca_root, pem, false))),
        Err(NO_FILE) => Ok(None),
        Err(_) => Err("the CA root certificate cannot be used"),
    }
}

/// The three `security` steps of [`remove`] for the certificate at `file`,
/// whose text is `pem`.
fn remove_trust(
    runner: &dyn Runner,
    home: &Path,
    file: &Path,
    pem: &str,
    stop: &CancellationToken,
) -> OpResult {
    let keychain = login_keychain(home);
    let untrust = runner.run(SECURITY, &untrust_args(file), PROMPT_LIMIT, stop);
    let had_trust = match &untrust {
        Ok(output) if output.success() => true,
        // No trust to take away: the first setup never ran, or the user
        // removed it by hand. A dialog refused or cancelled, or a timeout,
        // is a failure: the trust may still be there.
        Ok(output) if output.code.is_some() && no_trust(&output.stderr) => false,
        _ => {
            let command = "remove-trusted-cert";
            return command_failed(TRUST_REMOVE, command, &untrust, PROMPT_LIMIT);
        }
    };
    let listed = runner.run(SECURITY, &find_args(&keychain), CHECK_LIMIT, stop);
    let hash = match &listed {
        Ok(output) if output.success() => hash_of(&output.stdout, pem),
        _ => return command_failed(TRUST_REMOVE, "find-certificate", &listed, CHECK_LIMIT),
    };
    let Some(hash) = hash else {
        let outcome = if had_trust {
            OpStatus::Done
        } else {
            OpStatus::Unchanged
        };
        return result(TRUST_REMOVE, outcome);
    };
    let deleted = runner.run(SECURITY, &delete_args(&hash, &keychain), PROMPT_LIMIT, stop);
    if succeeded(&deleted) {
        result(TRUST_REMOVE, OpStatus::Done)
    } else {
        command_failed(TRUST_REMOVE, "delete-certificate", &deleted, PROMPT_LIMIT)
    }
}

/// Why `security` must not get these paths, or `None`. Each goes on its
/// command line as one argument: a relative one would name another file
/// (the runner's working folder is `/`), and one that starts with `-` would
/// be read as an option. An absolute path does neither.
fn not_absolute(home: &Path, ca_root: &Path, record: &Path) -> Option<&'static str> {
    if !ca_root.is_absolute() {
        Some("the path of the CA root certificate is not absolute")
    } else if !record.is_absolute() {
        Some("the path of the record of the trusted certificate is not absolute")
    } else if !home.is_absolute() {
        Some("the home folder is not an absolute path")
    } else {
        None
    }
}

/// Keeps `pem` at `record`. A failure is logged and does not fail the trust,
/// which macOS has added: the uninstall then falls back to `root.crt`.
fn keep_record(record: &Path, pem: &str) {
    match write_record(record, pem) {
        Ok(()) => tracing::debug!(path = %record.display(), "kept the record of the trusted CA"),
        Err(err) => tracing::warn!(
            path = %record.display(),
            error = %err,
            "cannot keep the record of the trusted CA; the uninstall will use root.crt"
        ),
    }
}

/// Writes `pem` to a temp file in the folder of `record`, mode 0600, then
/// renames it over `record`: a reader finds the old record or the new one,
/// never half of one. The folder is made, mode 0700, if it is missing.
fn write_record(record: &Path, pem: &str) -> io::Result<()> {
    let (Some(dir), Some(name)) = (record.parent(), record.file_name()) else {
        let reason = "the record path has no folder or no file name";
        return Err(io::Error::new(io::ErrorKind::InvalidInput, reason));
    };
    create_private_dir(dir)?;
    let mut temp = OsString::from(".");
    temp.push(name);
    temp.push(".tmp");
    let temp = dir.join(temp);
    // `write_new` closes the file before the rename.
    let written = write_new(&temp, pem.as_bytes()).and_then(|()| fs::rename(&temp, record));
    if written.is_err() {
        // Best effort: the first error is the one to report.
        let _ = fs::remove_file(&temp);
    }
    written
}

/// Writes a new file, never through one that is there: a temp file left by
/// a crash, or a link put in its place, is deleted first. One setup runs at
/// a time, so no other writer shares the name.
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = match create_new(path) {
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            fs::remove_file(path)?;
            create_new(path)?
        }
        other => other?,
    };
    file.write_all(bytes)?;
    file.sync_all()
}

fn create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    // `#[cfg]` outside `platform`: the Unix mode of the new file. Windows
    // gives it the ACL of the user's own data folder.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// `dir` and its parents. A new `dir` is for its owner only.
fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    // `#[cfg]` outside `platform`: the Unix mode of the new folder, as above.
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Deletes the record after the removal succeeded. A failure is logged: the
/// next removal finds the trust gone and deletes the record then.
fn forget_record(record: &Path) {
    match fs::remove_file(record) {
        Ok(()) => tracing::debug!(path = %record.display(), "deleted the record of the trusted CA"),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(
            path = %record.display(),
            error = %err,
            "cannot delete the record of the trusted CA"
        ),
    }
}

/// A failed op, with its code and an English message for the logs.
pub(crate) fn failed(op: &str, code: &str, message: impl Into<String>) -> OpResult {
    OpResult {
        op: op.to_owned(),
        status: OpStatus::Failed,
        code: Some(code.to_owned()),
        message: Some(message.into()),
    }
}

fn result(op: &str, status: OpStatus) -> OpResult {
    OpResult {
        op: op.to_owned(),
        status,
        code: None,
        message: None,
    }
}

/// Whether the certificate at `ca_root` is trusted as a root for this user
/// already. A check that cannot run counts as not trusted: the add then
/// tells.
fn is_trusted(runner: &dyn Runner, ca_root: &Path, stop: &CancellationToken) -> bool {
    match runner.run(SECURITY, &verify_args(ca_root), CHECK_LIMIT, stop) {
        Ok(output) if output.success() => true,
        Ok(output) => {
            tracing::debug!(exit = ?output.code, "Marsa's CA root is not trusted for the current user yet");
            false
        }
        Err(err) => {
            tracing::warn!(error = %err, "cannot run security verify-cert");
            false
        }
    }
}

/// `-L`: no fetch of missing certificates from the net. `-l`: the one
/// certificate given is a CA, which `verify-cert` refuses otherwise.
fn verify_args(ca_root: &Path) -> Vec<OsString> {
    vec![
        OsString::from("verify-cert"),
        OsString::from("-c"),
        ca_root.as_os_str().to_owned(),
        OsString::from("-L"),
        OsString::from("-l"),
    ]
}

/// No `-d`: the user domain, not the admin one (ADR 0015).
fn add_args(keychain: &Path, ca_root: &Path) -> Vec<OsString> {
    vec![
        OsString::from("add-trusted-cert"),
        OsString::from("-r"),
        OsString::from("trustRoot"),
        OsString::from("-k"),
        keychain.as_os_str().to_owned(),
        ca_root.as_os_str().to_owned(),
    ]
}

fn untrust_args(file: &Path) -> Vec<OsString> {
    vec![
        OsString::from("remove-trusted-cert"),
        file.as_os_str().to_owned(),
    ]
}

/// Every certificate named like Marsa's in the keychain, each with its
/// hashes (`-Z`) and its text (`-p`).
fn find_args(keychain: &Path) -> Vec<OsString> {
    vec![
        OsString::from("find-certificate"),
        OsString::from("-a"),
        OsString::from("-Z"),
        OsString::from("-p"),
        OsString::from("-c"),
        OsString::from(CA_NAME),
        keychain.as_os_str().to_owned(),
    ]
}

fn delete_args(hash: &str, keychain: &Path) -> Vec<OsString> {
    vec![
        OsString::from("delete-certificate"),
        OsString::from("-Z"),
        OsString::from(hash),
        keychain.as_os_str().to_owned(),
    ]
}

fn succeeded(result: &io::Result<RunOutput>) -> bool {
    matches!(result, Ok(output) if output.success())
}

/// One of the [`NO_TRUST`] texts appears in `stderr`, ignoring ASCII case.
fn no_trust(stderr: &[u8]) -> bool {
    NO_TRUST.iter().any(|text| {
        stderr
            .windows(text.len())
            .any(|window| window.eq_ignore_ascii_case(text))
    })
}

/// `limit`: how long `command` was given, for the message of a timeout.
fn command_failed(
    op: &str,
    command: &str,
    result: &io::Result<RunOutput>,
    limit: Duration,
) -> OpResult {
    let how = failure_text(result, limit);
    tracing::warn!(command, outcome = %how, "security could not remove the user's trust");
    let message = format!("security {command} {how}");
    failed(op, TRUST_REMOVE_FAILED, message)
}

/// How a command ended, when it did not end well. `limit` is the time it
/// was given: a timeout names it.
fn failure_text(result: &io::Result<RunOutput>, limit: Duration) -> String {
    match result {
        Ok(RunOutput {
            code: Some(code), ..
        }) => format!("exited with code {code}"),
        Ok(_) => "was stopped by a signal".to_owned(),
        Err(err) if err.kind() == io::ErrorKind::TimedOut => {
            format!("did not finish in {}", limit_text(limit))
        }
        Err(err) if err.kind() == io::ErrorKind::Interrupted => {
            "was stopped with the daemon".to_owned()
        }
        Err(err) => format!("could not run: {}", err.kind()),
    }
}

/// `limit` in words: whole minutes, else seconds.
fn limit_text(limit: Duration) -> String {
    let seconds = limit.as_secs();
    match (seconds / 60, seconds % 60) {
        (1, 0) => "1 minute".to_owned(),
        (minutes, 0) if minutes > 1 => format!("{minutes} minutes"),
        _ => format!("{seconds} seconds"),
    }
}

/// The SHA-256 that `find-certificate -a -Z -p` printed for the certificate
/// whose text is `pem`, or `None` if the keychain holds no such certificate.
/// The match is on the base64 of the DER with the line breaks removed:
/// standard base64 gives one text per DER, so two prints of one certificate
/// match.
fn hash_of(listing: &[u8], pem: &str) -> Option<String> {
    let wanted = body(pem)?;
    let text = String::from_utf8_lossy(listing);
    certificates(&text)
        .into_iter()
        .find(|(_, body)| *body == wanted)
        .map(|(hash, _)| hash)
}

/// Each `(SHA-256, base64 body)` in the output of `find-certificate -a -Z
/// -p`: a `SHA-256 hash:` line, a `SHA-1 hash:` line, then the PEM, for each
/// match. A hash that is not 64 hex digits is skipped: it never goes on a
/// command line. `\r` is trimmed (AGENTS.md section 6).
fn certificates(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut hash: Option<String> = None;
    let mut body: Option<String> = None;
    for line in text.lines().map(|line| line.trim_end_matches('\r')) {
        if let Some(rest) = line.strip_prefix(HASH_LINE) {
            hash = Some(rest.trim())
                .filter(|hash| is_sha256(hash))
                .map(str::to_owned);
            body = None;
        } else if line == PEM_BEGIN {
            body = Some(String::new());
        } else if line == PEM_END {
            if let (Some(hash), Some(body)) = (hash.take(), body.take()) {
                found.push((hash, body));
            }
        } else if let Some(body) = body.as_mut() {
            body.push_str(line.trim());
        }
    }
    found
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The base64 of the certificate in `pem`, without line breaks.
fn body(pem: &str) -> Option<String> {
    let start = pem.find(PEM_BEGIN)? + PEM_BEGIN.len();
    let end = start + pem[start..].find(PEM_END)?;
    let body: String = pem[start..end]
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    (!body.is_empty()).then_some(body)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{Fake, TEST_CA, exit, ok};
    use super::*;
    use marsa_core::caddy::CaddyPaths;
    use tempfile::TempDir;

    /// `openssl x509 -in test-ca.pem -outform der | shasum -a 256`, run
    /// once; the same value as in the helper's `ops/trust.rs` tests.
    const HASH: &str = "29E433200254DFCE602212438E9D64E306B672DB9BC4642A7620CEBCC0FD8057";
    /// Another certificate with Marsa's name: the test CA's base64 with one
    /// character changed, so only the hash and the body differ.
    const OTHER_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";
    /// A drive on Windows, where these tests also run: a path without one is
    /// not absolute there, and `security` gets absolute paths only.
    const DRIVE: &str = if cfg!(windows) { "C:" } else { "" };

    fn home() -> PathBuf {
        PathBuf::from(format!("{DRIVE}/Users/dev"))
    }

    /// What `find-certificate -a -Z -p` prints for `certs`, in its format.
    fn listing(certs: &[(&str, &str)]) -> String {
        certs
            .iter()
            .map(|(hash, pem)| format!("SHA-256 hash: {hash}\nSHA-1 hash: 0123456789ABCDEF0123456789ABCDEF01234567\n{pem}"))
            .collect()
    }

    fn other_pem() -> String {
        let body = body(TEST_CA).unwrap();
        let changed = if body.starts_with('M') { "N" } else { "M" };
        format!("{PEM_BEGIN}\n{changed}{}\n{PEM_END}\n", &body[1..])
    }

    /// The `security` calls, as `(first argument, all arguments)`.
    fn security_calls(fake: &Fake) -> Vec<Vec<String>> {
        fake.calls()
            .into_iter()
            .filter(|call| call.program == SECURITY)
            .map(|call| call.args)
            .collect()
    }

    /// A data folder with Caddy's `root.crt` (if `ca` is given) and the
    /// record (if `record` is given). Both paths are real files in a temp
    /// folder; the home folder stays a path only.
    struct Data {
        dir: TempDir,
    }

    impl Data {
        fn new(ca: Option<&str>, record: Option<&str>) -> Self {
            let data = Self {
                dir: TempDir::new().unwrap(),
            };
            for (path, text) in [(data.ca_root(), ca), (data.record(), record)] {
                if let Some(text) = text {
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    fs::write(&path, text).unwrap();
                }
            }
            data
        }

        fn ca_root(&self) -> PathBuf {
            CaddyPaths::in_dir(self.dir.path()).ca_root()
        }

        fn record(&self) -> PathBuf {
            record_path(self.dir.path())
        }

        fn install(&self, fake: &Fake) -> OpResult {
            let stop = CancellationToken::new();
            install(
                fake,
                &home(),
                &self.ca_root(),
                TEST_CA,
                &self.record(),
                &stop,
            )
        }

        fn remove(&self, fake: &Fake) -> OpResult {
            let stop = CancellationToken::new();
            remove(fake, &home(), &self.ca_root(), &self.record(), &stop)
        }

        /// What the record holds, or `None` if there is none.
        fn recorded(&self) -> Option<String> {
            fs::read_to_string(self.record()).ok()
        }
    }

    /// `remove` with the test CA in `root.crt` and no record: the path the
    /// uninstall takes after a setup made before the record existed.
    fn remove_with(fake: &Fake) -> OpResult {
        Data::new(Some(TEST_CA), None).remove(fake)
    }

    /// `remove` with the test CA in `root.crt` and no record, and the path
    /// of that `root.crt` as `security` gets it.
    fn remove_with_root(fake: &Fake) -> (OpResult, String) {
        let data = Data::new(Some(TEST_CA), None);
        let removed = data.remove(fake);
        (removed, data.ca_root().to_str().unwrap().to_owned())
    }

    #[test]
    fn the_login_keychain_is_under_the_home_folder() {
        assert_eq!(
            login_keychain(Path::new("/Users/dev")),
            Path::new("/Users/dev/Library/Keychains/login.keychain-db")
        );
    }

    #[test]
    fn a_path_that_is_not_absolute_never_reaches_security() {
        let fake = Fake::answering(|program, args| panic!("{program} {args:?} ran"));
        let stop = CancellationToken::new();
        // Real files at the absolute paths, so only the relative one stops
        // the call.
        let data = Data::new(Some(TEST_CA), Some(TEST_CA));
        let (home, ca_root, record) = (home(), data.ca_root(), data.record());
        // A relative path would name another file, and `-d` would be read
        // as an option.
        let cases = [
            (
                "a relative file",
                home.as_path(),
                Path::new("root.crt"),
                record.as_path(),
            ),
            (
                "a file in .",
                home.as_path(),
                Path::new("./root.crt"),
                record.as_path(),
            ),
            (
                "an option",
                home.as_path(),
                Path::new("-d"),
                record.as_path(),
            ),
            (
                "a relative record",
                home.as_path(),
                ca_root.as_path(),
                Path::new("user-trusted.pem"),
            ),
            (
                "a record as an option",
                home.as_path(),
                ca_root.as_path(),
                Path::new("-d"),
            ),
            (
                "a relative home",
                Path::new("Users/dev"),
                ca_root.as_path(),
                record.as_path(),
            ),
        ];
        for (name, home, ca_root, record) in cases {
            let added = install(&*fake, home, ca_root, TEST_CA, record, &stop);
            assert_eq!(added.op, TRUST_INSTALL, "{name}");
            assert_eq!(added.status, OpStatus::Failed, "{name}");
            assert_eq!(added.code.as_deref(), Some(TRUST_ADD_FAILED), "{name}");
            let removed = remove(&*fake, home, ca_root, record, &stop);
            assert_eq!(removed.op, TRUST_REMOVE, "{name}");
            assert_eq!(removed.status, OpStatus::Failed, "{name}");
            assert_eq!(removed.code.as_deref(), Some(TRUST_REMOVE_FAILED), "{name}");
            for result in [added, removed] {
                let message = result.message.unwrap();
                assert!(
                    message.contains("security did not run"),
                    "{name}: {message}"
                );
            }
        }
        assert!(fake.calls().is_empty());
        // Nothing was written or deleted either.
        assert_eq!(data.recorded().as_deref(), Some(TEST_CA));
    }

    #[test]
    fn a_timeout_names_the_limit_of_its_command() {
        assert_eq!(limit_text(PROMPT_LIMIT), "5 minutes");
        assert_eq!(limit_text(CHECK_LIMIT), "10 seconds");
        assert_eq!(limit_text(Duration::from_secs(60)), "1 minute");
        assert_eq!(limit_text(Duration::from_secs(90)), "90 seconds");
        assert_eq!(limit_text(Duration::ZERO), "0 seconds");
    }

    #[test]
    fn the_ca_name_is_the_one_caddy_gives_the_root() {
        use marsa_core::caddy::{CaddyInput, PhpPools, sites_config};
        use marsa_core::sites::{Site, SiteKind};

        // A secure site, so the config names the CA.
        let site = Site {
            name: "shop".into(),
            root: PathBuf::from("/code/shop"),
            docroot: PathBuf::from("/code/shop"),
            kind: SiteKind::Static,
            php: None,
            php_source: None,
            secure: true,
        };
        let config = sites_config(&CaddyInput::default(), &[site], &PhpPools::default()).unwrap();
        let ca = &config["apps"]["pki"]["certificate_authorities"]["marsa"];
        assert_eq!(ca["root_common_name"], CA_NAME, "{config}");
    }

    #[test]
    fn the_body_is_the_base64_without_line_breaks() {
        let base64 = body(TEST_CA).unwrap();
        assert!(!base64.contains('\n') && !base64.contains(' '), "{base64}");
        assert!(base64.len() > 100 && base64.bytes().all(|b| b.is_ascii_graphic()));
        // The text around the block does not matter; a block without a body
        // or without an end is nothing.
        assert_eq!(body(&format!(" \r\n{TEST_CA}\n")), Some(base64));
        assert_eq!(body(&format!("{PEM_BEGIN}\n{PEM_END}\n")), None);
        assert_eq!(body(PEM_BEGIN), None);
        assert_eq!(body(""), None);
    }

    #[test]
    fn the_listing_gives_each_certificate_its_hash() {
        let other = other_pem();
        let text = listing(&[(OTHER_HASH, &other), (HASH, TEST_CA)]);
        let found = certificates(&text);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].0, OTHER_HASH);
        assert_eq!(found[1], (HASH.to_owned(), body(TEST_CA).unwrap()));
        assert_eq!(hash_of(text.as_bytes(), TEST_CA), Some(HASH.to_owned()));
        assert_eq!(
            hash_of(text.as_bytes(), &other),
            Some(OTHER_HASH.to_owned())
        );

        // CRLF, and nothing before the first hash line, are fine.
        let crlf = text.replace('\n', "\r\n");
        assert_eq!(hash_of(crlf.as_bytes(), TEST_CA), Some(HASH.to_owned()));

        // No match, an empty listing, and a hash that is not one: nothing.
        assert_eq!(
            hash_of(listing(&[(OTHER_HASH, &other)]).as_bytes(), TEST_CA),
            None
        );
        assert_eq!(hash_of(b"", TEST_CA), None);
        for bad in [
            "29E4",
            "ZZE433200254DFCE602212438E9D64E306B672DB9BC4642A7620CEBCC0FD8057",
            "",
        ] {
            let text = listing(&[(bad, TEST_CA)]);
            assert_eq!(hash_of(text.as_bytes(), TEST_CA), None, "{bad}");
        }
        // A certificate before any hash line is not matched to a later one.
        let text = format!("{TEST_CA}{}", listing(&[(OTHER_HASH, &other)]));
        assert_eq!(hash_of(text.as_bytes(), TEST_CA), None);
    }

    #[test]
    fn remove_takes_the_trust_away_then_deletes_the_certificate_by_hash() {
        let text = listing(&[(OTHER_HASH, &other_pem()), (HASH, TEST_CA)]);
        let fake = Fake::answering(move |_, args| match args[0].as_str() {
            "find-certificate" => ok(&text),
            _ => ok(""),
        });
        let (removed, ca_root) = remove_with_root(&fake);
        assert_eq!(removed, result(TRUST_REMOVE, OpStatus::Done));
        // Built like the code builds it: on Windows, where these tests also
        // run, `join` puts `\` between the parts.
        let keychain = login_keychain(&home());
        let keychain = keychain.to_str().expect("the test path is UTF-8");
        let ca_root = ca_root.as_str();
        assert_eq!(
            security_calls(&fake),
            [
                vec!["remove-trusted-cert", ca_root],
                vec![
                    "find-certificate",
                    "-a",
                    "-Z",
                    "-p",
                    "-c",
                    "Marsa Local CA",
                    keychain
                ],
                vec!["delete-certificate", "-Z", HASH, keychain],
            ]
        );
        let limits: Vec<_> = fake.calls().iter().map(|call| call.limit).collect();
        assert_eq!(limits, [PROMPT_LIMIT, CHECK_LIMIT, PROMPT_LIMIT]);
    }

    #[test]
    fn remove_with_nothing_there_is_unchanged_and_deletes_nothing() {
        let no_trust = [
            // The trust settings have no entry for the certificate.
            "SecTrustSettingsRemoveTrustSettings: The specified item could not be found in the \
             keychain.\n",
            // The user has no trust settings at all: the usual case before
            // the first setup.
            "SecTrustSettingsRemoveTrustSettings: No Trust Settings were found.\n",
        ];
        for stderr in no_trust {
            let fake = Fake::answering(move |_, args| match args[0].as_str() {
                "remove-trusted-cert" => exit(1, stderr),
                "find-certificate" => ok(""),
                other => panic!("{other} ran"),
            });
            assert_eq!(
                remove_with(&fake),
                result(TRUST_REMOVE, OpStatus::Unchanged),
                "{stderr}"
            );
            assert_eq!(security_calls(&fake).len(), 2, "{stderr}");
        }

        // The trust was there, the certificate was not: done.
        let fake = Fake::answering(move |_, args| match args[0].as_str() {
            "find-certificate" => ok(""),
            _ => ok(""),
        });
        assert_eq!(remove_with(&fake), result(TRUST_REMOVE, OpStatus::Done));
        assert_eq!(security_calls(&fake).len(), 2);
    }

    #[test]
    fn remove_stops_at_the_first_command_that_fails() {
        /// What `security` answers, by its arguments.
        type Script = Box<dyn Fn(&[String]) -> io::Result<RunOutput> + Send + Sync>;
        let refused = "SecTrustSettingsRemoveTrustSettings: The authorization was denied since \
                       no user interaction was possible.\n";
        let text = listing(&[(HASH, TEST_CA)]);
        // The case, the message, the script, and how many commands ran.
        let cases: Vec<(&str, &str, Script, usize)> = vec![
            (
                "the dialog refused",
                "security remove-trusted-cert exited with code 1",
                Box::new(move |args| match args[0].as_str() {
                    "remove-trusted-cert" => exit(1, refused),
                    other => panic!("{other} ran"),
                }),
                1,
            ),
            (
                "the dialog left open",
                "security remove-trusted-cert did not finish in 5 minutes",
                Box::new(|args| match args[0].as_str() {
                    "remove-trusted-cert" => Err(io::Error::from(io::ErrorKind::TimedOut)),
                    other => panic!("{other} ran"),
                }),
                1,
            ),
            (
                "the listing left open",
                "security find-certificate did not finish in 10 seconds",
                Box::new(|args| match args[0].as_str() {
                    "find-certificate" => Err(io::Error::from(io::ErrorKind::TimedOut)),
                    "remove-trusted-cert" => ok(""),
                    other => panic!("{other} ran"),
                }),
                2,
            ),
            (
                "the listing failed",
                "security find-certificate exited with code 1",
                Box::new(|args| match args[0].as_str() {
                    "find-certificate" => exit(1, "security: SecKeychainSearchCopyNext"),
                    "remove-trusted-cert" => ok(""),
                    other => panic!("{other} ran"),
                }),
                2,
            ),
            (
                "the delete failed",
                "security delete-certificate was stopped by a signal",
                Box::new(move |args| match args[0].as_str() {
                    "find-certificate" => ok(&text),
                    "delete-certificate" => Ok(RunOutput {
                        code: None,
                        ..RunOutput::default()
                    }),
                    _ => ok(""),
                }),
                3,
            ),
        ];
        for (name, message, answer, calls) in cases {
            let fake = Fake::answering(move |_, args| answer(args));
            let want = failed(TRUST_REMOVE, TRUST_REMOVE_FAILED, message);
            assert_eq!(remove_with(&fake), want, "{name}");
            assert_eq!(security_calls(&fake).len(), calls, "{name}");
        }
    }

    #[test]
    fn the_record_is_under_the_data_folder() {
        assert_eq!(
            record_path(Path::new("/data")),
            Path::new("/data").join("trust").join("user-trusted.pem")
        );
    }

    #[test]
    fn a_trust_added_or_already_there_is_recorded() {
        let added = Fake::answering(|_, args| match args[0].as_str() {
            "verify-cert" => exit(1, "CSSMERR_TP_NOT_TRUSTED"),
            _ => ok(""),
        });
        let there = Fake::answering(|_, args| match args[0].as_str() {
            "verify-cert" => ok(""),
            other => panic!("{other} ran"),
        });
        for (fake, status) in [(added, OpStatus::Done), (there, OpStatus::Unchanged)] {
            let data = Data::new(Some(TEST_CA), None);
            assert_eq!(data.install(&fake), result(TRUST_INSTALL, status));
            assert_eq!(data.recorded().as_deref(), Some(TEST_CA), "{status:?}");
            // `security` still got Caddy's file: the record is written after.
            let ca_root = data.ca_root();
            assert_eq!(security_calls(&fake)[0][2], ca_root.to_str().unwrap());
            // Only the record is in its folder: no temp file is left.
            let folder = data.record().parent().unwrap().to_path_buf();
            let names: Vec<_> = fs::read_dir(&folder)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(names, ["user-trusted.pem"], "{status:?}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode(&data.record()), 0o600);
                assert_eq!(mode(&folder), 0o700);
            }
        }
    }

    #[test]
    fn a_trust_not_added_records_nothing_and_keeps_an_old_record() {
        let fake = Fake::answering(|_, args| match args[0].as_str() {
            "verify-cert" => exit(1, "CSSMERR_TP_NOT_TRUSTED"),
            _ => exit(1, "refused"),
        });
        let data = Data::new(Some(TEST_CA), None);
        assert_eq!(data.install(&fake).status, OpStatus::Failed);
        assert_eq!(data.recorded(), None);

        let old = other_pem();
        let data = Data::new(Some(TEST_CA), Some(&old));
        assert_eq!(data.install(&fake).status, OpStatus::Failed);
        assert_eq!(data.recorded(), Some(old));
    }

    #[test]
    fn a_record_replaces_a_temp_file_or_a_link_left_in_its_way() {
        let fake = Fake::answering(|_, _| ok(""));
        let data = Data::new(Some(TEST_CA), Some(&other_pem()));
        let folder = data.record().parent().unwrap().to_path_buf();
        let temp = folder.join(".user-trusted.pem.tmp");
        fs::write(&temp, "left by a crash").unwrap();
        assert_eq!(data.install(&fake).status, OpStatus::Unchanged);
        assert_eq!(data.recorded().as_deref(), Some(TEST_CA));
        assert!(!temp.exists());

        // A link at the temp name is replaced, not written through.
        #[cfg(unix)]
        {
            let target = data.dir.path().join("elsewhere");
            fs::write(&target, "not Marsa's").unwrap();
            std::os::unix::fs::symlink(&target, &temp).unwrap();
            assert_eq!(data.install(&fake).status, OpStatus::Unchanged);
            assert_eq!(fs::read_to_string(&target).unwrap(), "not Marsa's");
            assert_eq!(data.recorded().as_deref(), Some(TEST_CA));
        }
    }

    #[test]
    fn a_record_that_cannot_be_written_does_not_fail_the_trust() {
        let fake = Fake::answering(|_, _| ok(""));
        let data = Data::new(Some(TEST_CA), None);
        // A file where the record's folder goes.
        fs::write(data.dir.path().join("trust"), "a file").unwrap();
        assert_eq!(
            data.install(&fake),
            result(TRUST_INSTALL, OpStatus::Unchanged)
        );
        assert_eq!(data.recorded(), None);
    }

    #[test]
    fn remove_takes_the_file_and_the_text_from_the_record() {
        // Caddy's root changed since the trust was added: the record still
        // names the old certificate, and that one is untrusted and deleted.
        let old = other_pem();
        let data = Data::new(Some(TEST_CA), Some(&old));
        let text = listing(&[(OTHER_HASH, &old), (HASH, TEST_CA)]);
        let fake = Fake::answering(move |_, args| match args[0].as_str() {
            "find-certificate" => ok(&text),
            _ => ok(""),
        });
        assert_eq!(data.remove(&fake), result(TRUST_REMOVE, OpStatus::Done));
        let record = data.record();
        let keychain = login_keychain(&home());
        let calls = security_calls(&fake);
        assert_eq!(calls[0], ["remove-trusted-cert", record.to_str().unwrap()]);
        assert_eq!(
            calls[2],
            [
                "delete-certificate",
                "-Z",
                OTHER_HASH,
                keychain.to_str().unwrap()
            ]
        );
        assert_eq!(calls.len(), 3);
        // The removal succeeded, so the record is gone; root.crt stays.
        assert_eq!(data.recorded(), None);
        assert!(data.ca_root().is_file());

        // The same with Caddy's folder deleted.
        let data = Data::new(None, Some(TEST_CA));
        let fake = Fake::answering(|_, args| match args[0].as_str() {
            "remove-trusted-cert" => exit(1, "No Trust Settings were found."),
            _ => ok(""),
        });
        assert_eq!(
            data.remove(&fake),
            result(TRUST_REMOVE, OpStatus::Unchanged)
        );
        let record = data.record();
        assert_eq!(
            security_calls(&fake)[0],
            ["remove-trusted-cert", record.to_str().unwrap()]
        );
        assert_eq!(data.recorded(), None);
    }

    #[test]
    fn a_failed_removal_keeps_the_record() {
        let data = Data::new(Some(TEST_CA), Some(TEST_CA));
        let fake = Fake::answering(|_, args| match args[0].as_str() {
            "remove-trusted-cert" => exit(1, "The authorization was denied."),
            other => panic!("{other} ran"),
        });
        assert_eq!(data.remove(&fake).status, OpStatus::Failed);
        assert_eq!(data.recorded().as_deref(), Some(TEST_CA));
    }

    #[test]
    fn without_a_record_or_a_root_there_is_nothing_to_remove() {
        let data = Data::new(None, None);
        let fake = Fake::answering(|program, args| panic!("{program} {args:?} ran"));
        assert_eq!(
            data.remove(&fake),
            result(TRUST_REMOVE, OpStatus::Unchanged)
        );
    }

    #[test]
    fn a_record_or_root_that_cannot_be_used_fails_before_security() {
        let fake = Fake::answering(|program, args| panic!("{program} {args:?} ran"));
        let cases = [
            (
                "a record that is not a certificate",
                Some(TEST_CA),
                Some("garbage"),
            ),
            (
                "a record that is not a certificate, no root",
                None,
                Some("garbage"),
            ),
            ("a root that is not a certificate", Some("garbage"), None),
        ];
        for (name, ca, record) in cases {
            let data = Data::new(ca, record);
            let removed = data.remove(&fake);
            assert_eq!(removed.status, OpStatus::Failed, "{name}");
            assert_eq!(removed.code.as_deref(), Some(TRUST_REMOVE_FAILED), "{name}");
            let message = removed.message.unwrap();
            assert!(
                message.contains("security did not run"),
                "{name}: {message}"
            );
            // The record stays: the trust it names may still be there.
            assert_eq!(data.recorded().as_deref(), record, "{name}");
        }
        // A folder in place of the record.
        let data = Data::new(Some(TEST_CA), None);
        fs::create_dir_all(data.record()).unwrap();
        assert_eq!(data.remove(&fake).status, OpStatus::Failed);
        assert!(data.record().is_dir());
    }
}
