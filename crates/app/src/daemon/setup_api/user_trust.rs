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
//! - For the uninstall, [`remove`]: `security remove-trusted-cert <root.crt>`
//!   takes the user's trust away, then `security delete-certificate -Z
//!   <sha256> <login keychain>` deletes the certificate by its SHA-256. The
//!   hash comes from `security find-certificate -a -Z -p -c "Marsa Local CA"
//!   <login keychain>`, matched by the certificate's own base64 and never by
//!   name alone: another CA may carry the same name (ADR 0011).
//!
//! No message quotes what `security` printed: only the exit code.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use marsa_core::helper::OpResult;
use marsa_core::types::OpStatus;
use tokio_util::sync::CancellationToken;

use super::{CHECK_LIMIT, PEM_BEGIN, PROMPT_LIMIT, RunOutput, Runner, TRUST_INSTALL};

/// The tool, by its absolute path (AGENTS.md section 6).
pub const SECURITY: &str = "/usr/bin/security";
/// `security add-trusted-cert` did not add the user's trust: the dialog was
/// refused or cancelled, could not open (no desktop session), or the tool
/// failed or timed out. The same code as the helper's, so the CLI text fits.
pub const TRUST_ADD_FAILED: &str = "trust_add_failed";
/// `security` could not take the trust away or delete the certificate.
pub const TRUST_REMOVE_FAILED: &str = "trust_remove_failed";

/// The op name of the removal in a report, like the helper's.
const TRUST_REMOVE: &str = "trust_remove";
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

/// The login keychain of the user whose home folder is `home`.
pub fn login_keychain(home: &Path) -> PathBuf {
    LOGIN_KEYCHAIN
        .iter()
        .fold(home.to_path_buf(), |path, part| path.join(part))
}

/// `trust_install` for the current user: `unchanged` if the certificate at
/// `ca_root` is trusted already, `done` once macOS added the trust behind its
/// dialog, or `trust_add_failed`.
pub fn install(
    runner: &dyn Runner,
    home: &Path,
    ca_root: &Path,
    stop: &CancellationToken,
) -> OpResult {
    if let Some(problem) = not_absolute(home, ca_root) {
        tracing::warn!(problem, "security does not run");
        let message = format!("{problem}, so security did not run");
        return failed(TRUST_INSTALL, TRUST_ADD_FAILED, message);
    }
    if is_trusted(runner, ca_root, stop) {
        tracing::info!("Marsa's CA root is already trusted for the current user");
        return result(TRUST_INSTALL, OpStatus::Unchanged);
    }
    tracing::info!("asking macOS to trust Marsa's CA root for the current user");
    let args = add_args(&login_keychain(home), ca_root);
    let added = runner.run(SECURITY, &args, PROMPT_LIMIT, stop);
    if succeeded(&added) {
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

/// `trust_remove` for the current user, for the later uninstall: takes the
/// user's trust away from the certificate at `ca_root`, then deletes the
/// certificate whose text is `pem` from the login keychain. `unchanged` if
/// neither was there. No route calls it yet.
pub fn remove(
    runner: &dyn Runner,
    home: &Path,
    ca_root: &Path,
    pem: &str,
    stop: &CancellationToken,
) -> OpResult {
    if let Some(problem) = not_absolute(home, ca_root) {
        tracing::warn!(problem, "security does not run");
        let message = format!("{problem}, so security did not run");
        return failed(TRUST_REMOVE, TRUST_REMOVE_FAILED, message);
    }
    let keychain = login_keychain(home);
    let untrust = runner.run(SECURITY, &untrust_args(ca_root), PROMPT_LIMIT, stop);
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
fn not_absolute(home: &Path, ca_root: &Path) -> Option<&'static str> {
    if !ca_root.is_absolute() {
        Some("the path of the CA root certificate is not absolute")
    } else if !home.is_absolute() {
        Some("the home folder is not an absolute path")
    } else {
        None
    }
}

/// A failed op, with its code and an English message for the logs.
pub(super) fn failed(op: &str, code: &str, message: impl Into<String>) -> OpResult {
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

fn untrust_args(ca_root: &Path) -> Vec<OsString> {
    vec![
        OsString::from("remove-trusted-cert"),
        ca_root.as_os_str().to_owned(),
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

    fn ca_root() -> PathBuf {
        PathBuf::from(format!(
            "{DRIVE}/Users/dev/Library/Application Support/dev.marsa.Marsa/caddy/data/caddy/\
             pki/authorities/marsa/root.crt"
        ))
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

    fn remove_with(fake: &Fake) -> OpResult {
        let stop = CancellationToken::new();
        remove(fake, &home(), &ca_root(), TEST_CA, &stop)
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
        let (home, ca_root) = (home(), ca_root());
        // A relative path would name another file, and `-d` would be read
        // as an option.
        let cases = [
            ("a relative file", home.as_path(), Path::new("root.crt")),
            ("a file in .", home.as_path(), Path::new("./root.crt")),
            ("an option", home.as_path(), Path::new("-d")),
            ("a relative home", Path::new("Users/dev"), ca_root.as_path()),
        ];
        for (name, home, ca_root) in cases {
            let added = install(&*fake, home, ca_root, &stop);
            assert_eq!(added.op, TRUST_INSTALL, "{name}");
            assert_eq!(added.status, OpStatus::Failed, "{name}");
            assert_eq!(added.code.as_deref(), Some(TRUST_ADD_FAILED), "{name}");
            let removed = remove(&*fake, home, ca_root, TEST_CA, &stop);
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
        assert_eq!(remove_with(&fake), result(TRUST_REMOVE, OpStatus::Done));
        // Built like the code builds it: on Windows, where these tests also
        // run, `join` puts `\` between the parts.
        let keychain = login_keychain(&home());
        let keychain = keychain.to_str().expect("the test path is UTF-8");
        let ca_root = ca_root();
        let ca_root = ca_root.to_str().expect("the test path is UTF-8");
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
}
