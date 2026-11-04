//! macOS: trust of Marsa's CA root certificate in the System keychain.
//!
//! The helper runs `/usr/bin/security` itself, not `caddy trust` (ADR 0011):
//! Caddy is a download in a folder the user can write, so root never runs it,
//! and `security add-trusted-cert` is what `caddy trust` runs on macOS anyway.
//!
//! The request carries the PEM text. It must be exactly one certificate: the
//! file is world-readable and then trusted as a root, so a private key or a
//! second certificate in it is refused. The helper writes its own clean copy
//! to [`CA_PATH`]. That file is also the record of what Marsa trusted:
//! `trust_remove` reads the SHA-256 from it and deletes by that hash, never by
//! name, because another user may have a CA with the same name.
//!
//! No message holds any part of the PEM.

use std::io;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::response::{OpResult, Outcome};
use crate::system::{RunOutput, System};

/// The request's `pem` is not exactly one certificate.
pub const TRUST_CERT_INVALID: &str = "trust_cert_invalid";
/// The file at [`CA_PATH`] cannot be read as one certificate, so it was left
/// as it is and no command ran.
pub const TRUST_READ: &str = "trust_read";
/// Writing [`CA_PATH`] failed.
pub const TRUST_WRITE: &str = "trust_write";
/// `security add-trusted-cert` failed or timed out. The helper then ran the
/// steps of `trust_remove`. If they failed too, the file stays so a later
/// `trust_remove` can finish, and until then `trust_install` with the same
/// certificate answers `unchanged`: run `trust_remove` before a retry.
pub const TRUST_ADD_FAILED: &str = "trust_add_failed";
/// `security` could not remove the trust or the certificate. The file stays,
/// so a retry still knows the hash.
pub const TRUST_REMOVE_FAILED: &str = "trust_remove_failed";
/// Deleting [`CA_PATH`] failed.
pub const TRUST_DELETE: &str = "trust_delete";

/// Marsa's copy of the CA root it trusts, in a folder only root can write.
const CA_PATH: &str = "/Library/Application Support/Marsa/ca-root.pem";
/// Everyone can read a certificate, and only root can change it.
const MODE: u32 = 0o644;
/// The largest file the helper reads at [`CA_PATH`]. The clean copy of a
/// 16 KiB request is a little larger than 16 KiB.
const MAX_FILE_BYTES: u64 = 32 * 1024;
const SECURITY: &str = "/usr/bin/security";
const KEYCHAIN: &str = "/Library/Keychains/System.keychain";
/// What `security delete-certificate` prints, with exit code 1, when no
/// certificate has the hash.
const NO_CERTIFICATE: &[u8] = b"Unable to delete certificate matching";
/// Part of the text of `errSecItemNotFound` ("The specified item could not
/// be found in the keychain."), which `security remove-trusted-cert` prints
/// when the certificate has no admin trust settings. Unverified here: running
/// it would change the trust settings. The owner's manual check confirms it.
const NO_TRUST: &[u8] = b"could not be found";

const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
const END: &str = "-----END CERTIFICATE-----";
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/// Base64 characters per line in the written file (RFC 7468).
const LINE: usize = 64;

/// Trusts the file as a root for every user (`-d`: the admin trust settings)
/// and adds the certificate to the System keychain.
const ADD: [&str; 7] = [
    "add-trusted-cert",
    "-d",
    "-r",
    "trustRoot",
    "-k",
    KEYCHAIN,
    CA_PATH,
];
/// Removes the admin trust settings of the certificate in the file.
const UNTRUST: [&str; 3] = ["remove-trusted-cert", "-d", CA_PATH];

const INSTALL: &str = "trust_install";
const REMOVE: &str = "trust_remove";

/// `trust_install`: trusts the one certificate in `pem` as a root for every
/// user. A different certificate that Marsa trusted before is removed first.
pub fn install(pem: &str, system: &impl System) -> OpResult {
    let der = match parse_pem(pem) {
        Ok(der) => der,
        Err(reason) => {
            return OpResult::failed(INSTALL, TRUST_CERT_INVALID, format!("pem {reason}"));
        }
    };
    let hash = sha256_hex(&der);
    match stored_hash(INSTALL, system) {
        Ok(Some(old)) if old == hash => return result(INSTALL, Outcome::Unchanged),
        Ok(Some(old)) => {
            if let Err(failed) = remove_steps(INSTALL, &old, system) {
                return failed;
            }
        }
        Ok(None) => {}
        Err(failed) => return failed,
    }
    if let Err(err) = system.write_atomic(Path::new(CA_PATH), &to_pem(&der), MODE) {
        let message = format!("cannot write {CA_PATH}: {}", err.kind());
        return OpResult::failed(INSTALL, TRUST_WRITE, message);
    }
    let added = system.run(SECURITY, &ADD);
    if exited_with(&added, 0) {
        return result(INSTALL, Outcome::Done);
    }
    // `add-trusted-cert` can stop half way: after it added the certificate,
    // or even the trust, and then failed or timed out. The steps of
    // `trust_remove` undo that, and they delete the file only if every step
    // worked, so the hash is never lost. The failed add is the error either way.
    let _ = remove_steps(INSTALL, &hash, system);
    command_failed(INSTALL, TRUST_ADD_FAILED, "add-trusted-cert", &added)
}

/// `trust_remove`: removes the trust and the certificate that Marsa trusted,
/// then its file. Without the file, nothing of Marsa's is trusted.
pub fn remove(system: &impl System) -> OpResult {
    match stored_hash(REMOVE, system) {
        Ok(None) => result(REMOVE, Outcome::Unchanged),
        Ok(Some(hash)) => match remove_steps(REMOVE, &hash, system) {
            Ok(()) => result(REMOVE, Outcome::Done),
            Err(failed) => failed,
        },
        Err(failed) => failed,
    }
}

fn result(op: &'static str, outcome: Outcome) -> OpResult {
    OpResult { op, outcome }
}

/// The SHA-256 of the certificate at [`CA_PATH`], or `None` if nothing is
/// there.
fn stored_hash(op: &'static str, system: &impl System) -> Result<Option<String>, OpResult> {
    let path = Path::new(CA_PATH);
    let reason = match system.read(path, MAX_FILE_BYTES) {
        Ok(bytes) => match std::str::from_utf8(&bytes).map(parse_pem) {
            Ok(Ok(der)) => return Ok(Some(sha256_hex(&der))),
            _ => "is not one certificate".to_owned(),
        },
        // `read` follows symlinks and `exists` does not, so a dangling link
        // is something there, not nothing.
        Err(err) if err.kind() == io::ErrorKind::NotFound && !system.exists(path) => {
            return Ok(None);
        }
        Err(err) => format!("cannot be read: {}", err.kind()),
    };
    let message = format!("{CA_PATH} {reason}, so it was left as it is");
    Err(OpResult::failed(op, TRUST_READ, message))
}

/// Removes the trust and the certificate whose SHA-256 is `hash`, then
/// [`CA_PATH`]. After a failure the file stays, so a retry still has the hash.
fn remove_steps(op: &'static str, hash: &str, system: &impl System) -> Result<(), OpResult> {
    let untrust = system.run(SECURITY, &UNTRUST);
    if !is_untrusted(&untrust) {
        let failed = command_failed(op, TRUST_REMOVE_FAILED, "remove-trusted-cert", &untrust);
        return Err(failed);
    }
    let delete = system.run(SECURITY, &["delete-certificate", "-Z", hash, KEYCHAIN]);
    if !is_deleted(&delete) {
        let failed = command_failed(op, TRUST_REMOVE_FAILED, "delete-certificate", &delete);
        return Err(failed);
    }
    if let Err(err) = system.remove(Path::new(CA_PATH)) {
        let message = format!("cannot delete {CA_PATH}: {}", err.kind());
        return Err(OpResult::failed(op, TRUST_DELETE, message));
    }
    Ok(())
}

fn exited_with(result: &io::Result<RunOutput>, code: i32) -> bool {
    matches!(result, Ok(output) if output.code == Some(code))
}

/// `remove-trusted-cert` removed the trust, or there was none to remove
/// (any non-zero exit with [`NO_TRUST`]), for example after the user removed
/// it by hand. Anything else, such as a dialog that was refused or cancelled
/// (since Big Sur) or a timeout, means the trust may still be there.
fn is_untrusted(result: &io::Result<RunOutput>) -> bool {
    match result {
        Ok(output) if output.code == Some(0) => true,
        Ok(output) => output.code.is_some() && mentions(&output.stderr, NO_TRUST),
        Err(_) => false,
    }
}

/// `delete-certificate` deleted the certificate, or no certificate has the
/// hash (exit code 1 with [`NO_CERTIFICATE`]): it is already gone. Anything
/// else is a failure, which keeps the file and is the safe side.
fn is_deleted(result: &io::Result<RunOutput>) -> bool {
    let gone =
        |output: &RunOutput| output.code == Some(1) && mentions(&output.stderr, NO_CERTIFICATE);
    matches!(result, Ok(output) if output.code == Some(0) || gone(output))
}

/// `text` appears in `stderr`, ignoring ASCII case.
fn mentions(stderr: &[u8], text: &[u8]) -> bool {
    stderr
        .windows(text.len())
        .any(|window| window.eq_ignore_ascii_case(text))
}

/// The output of `security` is not in the message: the exit code is enough
/// for `marsad`, and the owner can run the same command by hand.
fn command_failed(
    op: &'static str,
    code: &'static str,
    command: &str,
    result: &io::Result<RunOutput>,
) -> OpResult {
    let message = format!("security {command} {}", failure_text(result));
    OpResult::failed(op, code, message)
}

fn failure_text(result: &io::Result<RunOutput>) -> String {
    match result {
        Ok(RunOutput {
            code: Some(code), ..
        }) => format!("exited with code {code}"),
        Ok(_) => "was stopped by a signal".to_owned(),
        Err(err) if err.kind() == io::ErrorKind::TimedOut => "timed out".to_owned(),
        Err(err) => format!("could not run: {}", err.kind()),
    }
}

/// The DER of the one certificate in `text`, or why `text` is not one.
///
/// Strict on purpose: exactly one `CERTIFICATE` block with only spaces, tabs,
/// and line breaks around it. A key block, a second block, or any other text
/// fails here, because the prefix or the suffix does not match or because
/// `-` and spaces are not base64.
fn parse_pem(text: &str) -> Result<Vec<u8>, &'static str> {
    let block = text.trim_matches(|c| matches!(c, ' ' | '\t' | '\r' | '\n'));
    let body = block
        .strip_prefix(BEGIN)
        .and_then(|rest| rest.strip_suffix(END))
        .ok_or("is not one CERTIFICATE block with only whitespace around it")?;
    let der = decode(body).ok_or("has a body that is not strict base64")?;
    if !is_certificate(&der) {
        return Err("is not one DER certificate");
    }
    Ok(der)
}

/// Standard base64 with `=` padding. Line breaks may sit between any two
/// characters, and nothing else may. The unused bits of the last character
/// must be zero, so every DER has exactly one encoding.
fn decode(body: &str) -> Option<Vec<u8>> {
    let chars: Vec<u8> = body.bytes().filter(|&c| c != b'\r' && c != b'\n').collect();
    if chars.is_empty() || !chars.len().is_multiple_of(4) {
        return None;
    }
    // The length is a multiple of 4, so nothing is left over.
    let (quads, _) = chars.as_chunks::<4>();
    let last = quads.len() - 1;
    let mut der = Vec::with_capacity(quads.len() * 3);
    for (i, quad) in quads.iter().enumerate() {
        let pad = quad.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && i != last) {
            return None;
        }
        let mut bits = 0_u32;
        for &c in &quad[..4 - pad] {
            bits = (bits << 6) | u32::from(sextet(c)?);
        }
        let bytes = (bits << (6 * pad)).to_be_bytes();
        let (data, unused) = bytes[1..].split_at(3 - pad);
        if unused.iter().any(|&byte| byte != 0) {
            return None;
        }
        der.extend_from_slice(data);
    }
    Some(der)
}

/// The value of one base64 character. `=` is not one: it is only padding.
fn sextet(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// The shape of an X.509 certificate (RFC 5280): one SEQUENCE, with nothing
/// after it, that holds exactly a SEQUENCE (the TBSCertificate), a SEQUENCE
/// (the signature algorithm), and a BIT STRING (the signature). Keys have
/// other shapes, so a key under a CERTIFICATE label fails here: PKCS#1,
/// PKCS#8, and SEC1 keys start with an INTEGER, and an encrypted PKCS#8 key
/// holds a SEQUENCE and an OCTET STRING.
fn is_certificate(der: &[u8]) -> bool {
    let Some((0x30, header, total)) = element(der) else {
        return false;
    };
    let mut rest = &der[header..total];
    let mut tags = Vec::new();
    while let Some((tag, _, size)) = element(rest) {
        tags.push(tag);
        rest = &rest[size..];
    }
    total == der.len() && rest.is_empty() && tags == [0x30, 0x30, 0x03]
}

/// The tag, the header length, and the total length of the DER element at
/// the start of `der`, if its length is in minimal form and fits in `der`.
/// A 16 KiB PEM never needs more than two length bytes.
fn element(der: &[u8]) -> Option<(u8, usize, usize)> {
    let (tag, header, len) = match *der {
        [tag, len, ..] if len < 0x80 => (tag, 2, usize::from(len)),
        [tag, 0x81, len, ..] if len >= 0x80 => (tag, 3, usize::from(len)),
        [tag, 0x82, high, low, ..] if high > 0 => {
            (tag, 4, usize::from(u16::from_be_bytes([high, low])))
        }
        _ => return None,
    };
    let total = header + len;
    (total <= der.len()).then_some((tag, header, total))
}

/// Upper case, as `security` prints it.
fn sha256_hex(der: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(der).iter() {
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    hex
}

/// The clean file: the BEGIN line, the base64 in lines of 64, the END line,
/// LF only, with a final newline.
fn to_pem(der: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(der.len().div_ceil(3) * 4);
    for chunk in der.chunks(3) {
        let [a, b, c] = [0, 1, 2].map(|i| chunk.get(i).copied().unwrap_or(0));
        let sextets = [
            a >> 2,
            ((a & 0x03) << 4) | (b >> 4),
            ((b & 0x0f) << 2) | (c >> 6),
            c & 0x3f,
        ];
        for (i, sextet) in sextets.into_iter().enumerate() {
            // A chunk of n bytes gives n + 1 characters, then padding.
            let symbol = if i <= chunk.len() {
                BASE64[usize::from(sextet)]
            } else {
                b'='
            };
            body.push(symbol);
        }
    }
    let mut pem = Vec::with_capacity(body.len() + body.len() / LINE + 64);
    pem.extend_from_slice(BEGIN.as_bytes());
    pem.push(b'\n');
    for line in body.chunks(LINE) {
        pem.extend_from_slice(line);
        pem.push(b'\n');
    }
    pem.extend_from_slice(END.as_bytes());
    pem.push(b'\n');
    pem
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

    /// A throwaway self-signed CA made once with `openssl req -x509`. Its key
    /// was deleted at once and never entered the repository.
    const TEST_CA: &str = include_str!("../../../../fixtures/helper/test-ca.pem");
    /// `openssl x509 -in test-ca.pem -outform der | shasum -a 256`, run once.
    const HASH: &str = "29E433200254DFCE602212438E9D64E306B672DB9BC4642A7620CEBCC0FD8057";
    /// The mode of a file a test starts with. Marsa never writes it, so the
    /// mode shows whether an op rewrote the file.
    const OLD_MODE: u32 = 0o600;
    /// What `remove-trusted-cert` prints when there is no trust to remove.
    const NOT_FOUND: &[u8] = b"SecTrustSettingsRemoveTrustSettings: \
        The specified item could not be found in the keychain.";
    /// What it prints when the trust change is refused.
    const REFUSED: &[u8] = b"SecTrustSettingsRemoveTrustSettings: \
        The authorization was denied since no user interaction was possible.";

    fn exit(code: i32, stderr: &[u8]) -> RunOutput {
        RunOutput {
            code: Some(code),
            stdout: Vec::new(),
            stderr: stderr.to_vec(),
        }
    }

    /// Every `security` command succeeds unless a test scripts it.
    fn fake() -> FakeSystem {
        let mut fake = FakeSystem::default();
        fake.defaults.insert(SECURITY, exit(0, b""));
        fake
    }

    fn fake_with(bytes: &[u8]) -> FakeSystem {
        let fake = fake();
        let file = (bytes.to_vec(), OLD_MODE);
        fake.files.borrow_mut().insert(PathBuf::from(CA_PATH), file);
        fake
    }

    fn script(fake: &mut FakeSystem, args: &[String], output: RunOutput) {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        fake.script(SECURITY, &args, output);
    }

    /// The `security` arguments of every command, in order.
    fn commands(fake: &FakeSystem) -> Vec<Vec<String>> {
        let log = fake.log.borrow();
        assert!(log.iter().all(|(program, _)| program == SECURITY));
        log.iter().map(|(_, args)| args.clone()).collect()
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn add() -> Vec<String> {
        strings(&ADD)
    }

    fn untrust() -> Vec<String> {
        strings(&UNTRUST)
    }

    fn delete(hash: &str) -> Vec<String> {
        strings(&["delete-certificate", "-Z", hash, KEYCHAIN])
    }

    /// The file at [`CA_PATH`], after a check that no other file appeared.
    fn file(fake: &FakeSystem) -> Option<(Vec<u8>, u32)> {
        let files = fake.files.borrow();
        assert!(files.keys().all(|path| path == Path::new(CA_PATH)));
        files.get(Path::new(CA_PATH)).cloned()
    }

    fn test_ca(mode: u32) -> Option<(Vec<u8>, u32)> {
        Some((TEST_CA.as_bytes().to_vec(), mode))
    }

    /// The test CA with one byte of its signature changed: another hash.
    fn other_ca() -> (Vec<u8>, String) {
        let mut der = parse_pem(TEST_CA).unwrap();
        *der.last_mut().unwrap() ^= 1;
        (to_pem(&der), sha256_hex(&der))
    }

    /// No line of the fixture's base64 is in the message.
    fn assert_no_pem_in(result: &OpResult) {
        let Outcome::Failed { message, .. } = &result.outcome else {
            panic!("{result:?} did not fail");
        };
        for line in TEST_CA.lines().filter(|line| !line.starts_with("-----")) {
            assert!(!message.contains(line), "{message}");
        }
    }

    #[test]
    fn install_writes_the_file_then_trusts_it() {
        let fake = fake();
        assert_eq!(install(TEST_CA, &fake), result(INSTALL, Outcome::Done));
        assert_eq!(commands(&fake), [add()]);
        // `openssl` writes the clean form too, so the copy equals the input.
        assert_eq!(file(&fake), test_ca(MODE));
    }

    /// From a messy but valid input: whitespace around the block, CRLF, and
    /// all the base64 on one line. The snapshot does not show line endings,
    /// so they are checked here.
    #[test]
    fn the_written_file() {
        let body: String = TEST_CA
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect();
        let messy = format!(" \t\r\n{BEGIN}\r\n{body}\r\n{END}\r\n\n");
        let fake = fake();
        assert_eq!(install(&messy, &fake).outcome, Outcome::Done);
        let (bytes, mode) = file(&fake).unwrap();
        assert_eq!((bytes.as_slice(), mode), (TEST_CA.as_bytes(), MODE));
        assert!(bytes.ends_with(b"-----\n") && !bytes.contains(&b'\r'));
        insta::assert_snapshot!(String::from_utf8(bytes).unwrap());
    }

    #[test]
    fn the_hash_is_the_one_openssl_gives() {
        assert_eq!(sha256_hex(&parse_pem(TEST_CA).unwrap()), HASH);
    }

    /// The vectors of RFC 4648, section 10, both ways.
    #[test]
    fn base64_vectors() {
        let vectors = [
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (data, text) in vectors {
            assert_eq!(decode(text).unwrap(), data.as_bytes(), "{text}");
            let pem = format!("{BEGIN}\n{text}\n{END}\n");
            assert_eq!(to_pem(data.as_bytes()), pem.as_bytes(), "{data}");
        }
    }

    /// Refused before any call on the system: no file read or written, no
    /// command, and no part of the PEM in the message.
    fn assert_refused(pem: &str) {
        let fake = fake();
        let result = install(pem, &fake);
        let refused = matches!(
            result.outcome,
            Outcome::Failed {
                code: TRUST_CERT_INVALID,
                ..
            }
        );
        assert!(refused, "{result:?}");
        assert_no_pem_in(&result);
        assert!(fake.log.borrow().is_empty());
        assert_eq!(file(&fake), None);
    }

    macro_rules! refused {
        ($($name:ident: $pem:expr,)*) => {$(
            #[test]
            fn $name() {
                assert_refused(&$pem);
            }
        )*};
    }

    /// The fixture's base64 in a key block. Not a real key: the helper must
    /// refuse it from the labels alone.
    fn as_key() -> String {
        TEST_CA.replace("CERTIFICATE", "PRIVATE KEY")
    }

    fn one_line(base64: &str) -> String {
        format!("{BEGIN}\n{base64}\n{END}\n")
    }

    fn der_pem(der: &[u8]) -> String {
        String::from_utf8(to_pem(der)).unwrap()
    }

    fn test_der_and(more: &[u8]) -> String {
        der_pem(&[parse_pem(TEST_CA).unwrap(), more.to_vec()].concat())
    }

    refused! {
        refuses_a_private_key_block: as_key(),
        refuses_two_certificate_blocks: format!("{TEST_CA}{TEST_CA}"),
        refuses_a_certificate_then_a_key: format!("{TEST_CA}{}", as_key()),
        refuses_a_key_then_a_certificate: format!("{}{TEST_CA}", as_key()),
        refuses_two_certificates_in_one_body: test_der_and(&parse_pem(TEST_CA).unwrap()),
        refuses_text_before_the_block: format!("Certificate:\n{TEST_CA}"),
        refuses_text_after_the_block: format!("{TEST_CA}subject=CN=Marsa Test CA\n"),
        refuses_an_empty_body: format!("{BEGIN}\n{END}\n"),
        refuses_a_character_outside_base64: TEST_CA.replacen("MIIB", "MII*", 1),
        refuses_a_space_inside_the_body: TEST_CA.replacen("MIIB", "MI IB", 1),
        refuses_missing_padding: TEST_CA.replacen("TI=", "TI", 1),
        refuses_padding_before_the_end: TEST_CA.replacen("MIIB", "MI==", 1),
        refuses_three_padding_characters: one_line("M==="),
        refuses_nonzero_padding_bits: TEST_CA.replacen("TI=", "TJ=", 1),
        refuses_der_that_is_not_a_sequence: one_line("MQA="),
        refuses_an_empty_sequence: der_pem(&[0x30, 0x00]),
        refuses_bytes_after_the_certificate: test_der_and(&[0x00]),
        // `30 06 30 00 30 00 03 00` passes (see `the_certificate_shape`).
        refuses_a_length_that_is_not_minimal: der_pem(&[0x30, 0x81, 0x06, 0x30, 0x00, 0x30, 0x00, 0x03, 0x00]),
        // A synthetic key: a SEQUENCE that starts with an INTEGER (version 0).
        refuses_a_key_labelled_certificate: der_pem(&[0x30, 0x03, 0x02, 0x01, 0x00]),
        // The shape of an encrypted PKCS#8 key: a SEQUENCE and an OCTET STRING.
        refuses_an_encrypted_key_labelled_certificate: der_pem(&[0x30, 0x04, 0x30, 0x00, 0x04, 0x00]),
    }

    #[test]
    fn the_certificate_shape() {
        assert!(is_certificate(&parse_pem(TEST_CA).unwrap()));
        assert!(is_certificate(&[
            0x30, 0x06, 0x30, 0x00, 0x30, 0x00, 0x03, 0x00
        ]));
        // A fourth element, and an element that runs past the end.
        let four = [0x30, 0x08, 0x30, 0x00, 0x30, 0x00, 0x03, 0x00, 0x05, 0x00];
        assert!(!is_certificate(&four));
        assert!(!is_certificate(&[
            0x30, 0x06, 0x30, 0x00, 0x30, 0x00, 0x03, 0x01
        ]));
    }

    #[test]
    fn stderr_matches_ignore_case_and_position() {
        assert!(mentions(b"x: The item COULD NOT BE FOUND.", NO_TRUST));
        assert!(!mentions(b"could not be", NO_TRUST));
    }

    /// Every failure path that sees the PEM: the request, the stored file,
    /// and `security` output that quotes it.
    #[test]
    fn no_failure_message_holds_the_pem() {
        assert_refused(&TEST_CA.replacen("TI=", "TI", 1));
        let cut = &TEST_CA.as_bytes()[..TEST_CA.len() - 30];
        let stored = fake_with(cut);
        assert_no_pem_in(&install(TEST_CA, &stored));
        assert_no_pem_in(&remove(&stored));
        let mut fake = fake();
        script(&mut fake, &add(), exit(1, TEST_CA.as_bytes()));
        assert_no_pem_in(&install(TEST_CA, &fake));
    }

    #[test]
    fn install_over_the_same_certificate_is_unchanged() {
        // Another file with the same DER: `unchanged` compares the hash.
        let crlf = TEST_CA.replace('\n', "\r\n");
        let mut fake = fake_with(crlf.as_bytes());
        fake.fail_writes.insert(PathBuf::from(CA_PATH));
        assert_eq!(install(TEST_CA, &fake), result(INSTALL, Outcome::Unchanged));
        assert!(commands(&fake).is_empty());
        assert_eq!(file(&fake), Some((crlf.into_bytes(), OLD_MODE)));
    }

    #[test]
    fn install_over_another_certificate_removes_it_first() {
        let (other, other_hash) = other_ca();
        assert_ne!(other_hash, HASH);
        let fake = fake_with(&other);
        assert_eq!(install(TEST_CA, &fake), result(INSTALL, Outcome::Done));
        let expected = [untrust(), delete(&other_hash), add()];
        assert_eq!(commands(&fake), expected);
        assert_eq!(file(&fake), test_ca(MODE));
    }

    #[test]
    fn a_failed_removal_of_the_old_certificate_stops_the_install() {
        let (other, other_hash) = other_ca();
        let mut fake = fake_with(&other);
        script(&mut fake, &delete(&other_hash), exit(51, b""));
        let message = "security delete-certificate exited with code 51".to_owned();
        let expected = OpResult::failed(INSTALL, TRUST_REMOVE_FAILED, message);
        assert_eq!(install(TEST_CA, &fake), expected);
        assert_eq!(commands(&fake), [untrust(), delete(&other_hash)]);
        assert_eq!(file(&fake), Some((other, OLD_MODE)));
    }

    #[test]
    fn remove_deletes_by_the_hash_then_the_file() {
        let fake = fake_with(TEST_CA.as_bytes());
        assert_eq!(remove(&fake), result(REMOVE, Outcome::Done));
        assert_eq!(commands(&fake), [untrust(), delete(HASH)]);
        assert_eq!(file(&fake), None);
    }

    #[test]
    fn remove_without_the_file_is_unchanged() {
        let fake = fake();
        assert_eq!(remove(&fake), result(REMOVE, Outcome::Unchanged));
        assert!(commands(&fake).is_empty());
    }

    /// The user removed the trust and the certificate by hand.
    fn script_gone(fake: &mut FakeSystem) {
        script(fake, &untrust(), exit(1, NOT_FOUND));
        let gone = format!("security: Unable to delete certificate matching \"{HASH}\"\n");
        script(fake, &delete(HASH), exit(1, gone.as_bytes()));
    }

    #[test]
    fn remove_goes_on_when_the_trust_and_the_certificate_are_gone() {
        let mut fake = fake_with(TEST_CA.as_bytes());
        script_gone(&mut fake);
        assert_eq!(remove(&fake), result(REMOVE, Outcome::Done));
        assert_eq!(commands(&fake), [untrust(), delete(HASH)]);
        assert_eq!(file(&fake), None);
    }

    /// A refused dialog, another error, and a signal even with the not-found
    /// text: the trust may still be there, so nothing else runs and the file
    /// keeps the hash.
    #[test]
    fn a_refused_untrust_keeps_the_file_and_deletes_nothing() {
        let killed = RunOutput {
            code: None,
            stdout: Vec::new(),
            stderr: NOT_FOUND.to_vec(),
        };
        for output in [exit(1, REFUSED), exit(44, b""), killed] {
            let text = failure_text(&Ok(output.clone()));
            let mut fake = fake_with(TEST_CA.as_bytes());
            script(&mut fake, &untrust(), output);
            let message = format!("security remove-trusted-cert {text}");
            let expected = OpResult::failed(REMOVE, TRUST_REMOVE_FAILED, message);
            assert_eq!(remove(&fake), expected);
            assert_eq!(commands(&fake), [untrust()]);
            assert_eq!(file(&fake), test_ca(OLD_MODE));
        }
    }

    /// Exit code 1 with another message, and the message with another code.
    #[test]
    fn a_failed_delete_keeps_the_file_for_a_retry() {
        let failures = [
            exit(1, b"SecKeychainItemDelete: Write permissions error."),
            exit(51, b"Unable to delete certificate matching"),
        ];
        for output in failures {
            let code = output.code.unwrap();
            let message = format!("security delete-certificate exited with code {code}");
            let mut fake = fake_with(TEST_CA.as_bytes());
            script(&mut fake, &delete(HASH), output);
            let expected = OpResult::failed(REMOVE, TRUST_REMOVE_FAILED, message);
            assert_eq!(remove(&fake), expected);
            assert_eq!(file(&fake), test_ca(OLD_MODE));
        }
    }

    /// `security` cannot run here (the fake has no output for it). A timeout
    /// takes the same path; see `a_timeout_is_a_failure`.
    #[test]
    fn remove_stops_when_security_cannot_run() {
        let mut fake = fake_with(TEST_CA.as_bytes());
        fake.defaults.clear();
        let message = "security remove-trusted-cert could not run: entity not found";
        let expected = OpResult::failed(REMOVE, TRUST_REMOVE_FAILED, message.to_owned());
        assert_eq!(remove(&fake), expected);
        assert_eq!(commands(&fake), [untrust()]);
        assert_eq!(file(&fake), test_ca(OLD_MODE));
    }

    fn add_failed() -> OpResult {
        let message = "security add-trusted-cert exited with code 1".to_owned();
        OpResult::failed(INSTALL, TRUST_ADD_FAILED, message)
    }

    /// The add failed before it changed anything: the cleanup finds nothing.
    #[test]
    fn a_failed_add_is_trust_add_failed_and_leaves_nothing() {
        let mut fake = fake();
        script(&mut fake, &add(), exit(1, b""));
        script_gone(&mut fake);
        assert_eq!(install(TEST_CA, &fake), add_failed());
        assert_eq!(commands(&fake), [add(), untrust(), delete(HASH)]);
        assert_eq!(file(&fake), None);
        // So a retry runs `add-trusted-cert` again.
        fake.outputs.clear();
        assert_eq!(install(TEST_CA, &fake), result(INSTALL, Outcome::Done));
    }

    /// The cleanup cannot remove the trust, so the file keeps the hash for a
    /// later `trust_remove`, and the error is still the failed add.
    #[test]
    fn a_failed_add_with_a_refused_untrust_keeps_the_file() {
        let mut fake = fake();
        script(&mut fake, &add(), exit(1, b""));
        script(&mut fake, &untrust(), exit(1, REFUSED));
        assert_eq!(install(TEST_CA, &fake), add_failed());
        assert_eq!(commands(&fake), [add(), untrust()]);
        assert_eq!(file(&fake), test_ca(MODE));
        fake.outputs.clear();
        assert_eq!(remove(&fake), result(REMOVE, Outcome::Done));
        assert_eq!(file(&fake), None);
    }

    /// `RealSystem::run` gives `TimedOut` at its limit (tests in `system`).
    #[test]
    fn a_timeout_is_a_failure() {
        let timed_out = Err(io::Error::from(io::ErrorKind::TimedOut));
        let failed = command_failed(INSTALL, TRUST_ADD_FAILED, "add-trusted-cert", &timed_out);
        let message = "security add-trusted-cert timed out".to_owned();
        assert_eq!(failed, OpResult::failed(INSTALL, TRUST_ADD_FAILED, message));
        assert_eq!(
            failure_text(&Ok(RunOutput::default())),
            "was stopped by a signal"
        );
        let mut fake = fake();
        fake.defaults.clear();
        let result = install(TEST_CA, &fake);
        let message = "security add-trusted-cert could not run: entity not found".to_owned();
        assert_eq!(result, OpResult::failed(INSTALL, TRUST_ADD_FAILED, message));
        // The cleanup cannot run either, so the file stays.
        assert_eq!(commands(&fake), [add(), untrust()]);
        assert_eq!(file(&fake), test_ca(MODE));
    }

    #[test]
    fn a_failed_write_is_trust_write_and_runs_no_command() {
        let mut fake = fake();
        fake.fail_writes.insert(PathBuf::from(CA_PATH));
        let message = format!("cannot write {CA_PATH}: permission denied");
        let expected = OpResult::failed(INSTALL, TRUST_WRITE, message);
        assert_eq!(install(TEST_CA, &fake), expected);
        assert!(commands(&fake).is_empty());
    }

    #[test]
    fn a_failed_file_delete_is_trust_delete() {
        let mut fake = fake_with(TEST_CA.as_bytes());
        fake.fail_removes.insert(PathBuf::from(CA_PATH));
        let message = format!("cannot delete {CA_PATH}: permission denied");
        assert_eq!(
            remove(&fake),
            OpResult::failed(REMOVE, TRUST_DELETE, message)
        );
        assert_eq!(file(&fake), test_ca(OLD_MODE));
    }

    /// Cut short, too large, a dangling link, unreadable: both ops leave the
    /// file and run no command, because the hash to remove is unknown.
    #[test]
    fn a_stored_file_that_is_not_one_certificate_is_left_alone() {
        let path = PathBuf::from(CA_PATH);
        let large = vec![b' '; usize::try_from(MAX_FILE_BYTES).unwrap() + 1];
        let mut dangling = fake();
        dangling.dangling.insert(path.clone());
        let mut unreadable = fake_with(TEST_CA.as_bytes());
        unreadable.fail_reads.insert(path);
        let cases = [
            (
                fake_with(&TEST_CA.as_bytes()[..100]),
                "is not one certificate",
            ),
            (fake_with(&large), "cannot be read: invalid data"),
            (dangling, "cannot be read: entity not found"),
            (unreadable, "cannot be read: permission denied"),
        ];
        for (fake, reason) in cases {
            let before = file(&fake);
            let message = format!("{CA_PATH} {reason}, so it was left as it is");
            let failed = |op| OpResult::failed(op, TRUST_READ, message.clone());
            assert_eq!(install(TEST_CA, &fake), failed(INSTALL));
            assert_eq!(remove(&fake), failed(REMOVE));
            assert!(commands(&fake).is_empty());
            assert_eq!(file(&fake), before);
        }
    }

    /// The fixture's text passes the request checks of task M4-05.
    #[test]
    fn a_request_installs_then_removes_the_fixture() {
        let ops = json!([{ "op": "trust_install", "pem": TEST_CA }, { "op": "trust_remove" }]);
        let raw = json!({ "version": 1, "ops": ops }).to_string();
        let request = Request::from_arg(OsStr::new(&raw)).unwrap();
        let fake = fake();
        let results = apply(&request, Os::Macos, &fake);
        let expected = [
            result(INSTALL, Outcome::Done),
            result(REMOVE, Outcome::Done),
        ];
        assert_eq!(results, expected);
        assert_eq!(commands(&fake), [add(), untrust(), delete(HASH)]);
        assert_eq!(file(&fake), None);
    }
}
