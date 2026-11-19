//! Which certificates this data folder put into the user's NSS databases.
//!
//! Every Caddy root of Marsa has the subject `CN=Marsa Local CA`, and
//! `certutil -L -n <nickname>` lists every certificate with the subject of
//! the one it names. So a nickname carries the certificate's SHA-256:
//! `Marsa Local CA <first 16 hex digits>` ([`Fingerprint::nickname`]), and
//! `certutil -D -n <nickname>` deletes that certificate and no other.
//! `<data>/trust/nss-fingerprints` records the full SHA-256 of each
//! certificate this data folder may have added, one per line. A nickname
//! is deleted only when its fingerprint is in the record, so two data
//! folders of one user never delete each other's root.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read};

use super::super::PEM_BEGIN;
use super::super::user_trust::{self, PEM_END, TrustFiles};
use super::certutil::CA_NAME;

/// How many hex digits of the SHA-256 go into a nickname: 64 bits.
const NICKNAME_DIGITS: usize = 16;
/// The record, in the trust folder.
const RECORD: &str = "nss-fingerprints";
/// A record larger than this is not Marsa's.
const MAX_RECORD_BYTES: u64 = 64 * 1024;

/// The SHA-256 of one certificate, as 64 lower-case hex digits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Fingerprint(String);

impl Fingerprint {
    /// The 64 hex digits.
    pub(super) fn hex(&self) -> &str {
        &self.0
    }

    /// The fingerprint of the one certificate in `pem`.
    pub(crate) fn of(pem: &str) -> Option<Self> {
        marsa_core::helper::certificate_sha256(pem).map(|hex| Self(hex.to_ascii_lowercase()))
    }

    /// The fingerprint of the certificate whose base64 is `body`.
    pub(super) fn of_body(body: &str) -> Option<Self> {
        Self::of(&format!("{PEM_BEGIN}\n{body}\n{PEM_END}\n"))
    }

    /// A line of the record: 64 hex digits, in any case.
    fn parse(line: &str) -> Option<Self> {
        let line = line.trim_end_matches('\r');
        user_trust::is_sha256(line).then(|| Self(line.to_ascii_lowercase()))
    }

    /// Its nickname in an NSS database: `Marsa Local CA 29e433200254dfce`.
    pub(crate) fn nickname(&self) -> String {
        format!("{CA_NAME} {}", &self.0[..NICKNAME_DIGITS])
    }
}

/// The hex digits of a nickname made by [`Fingerprint::nickname`].
pub(super) fn fingerprint_digits(nickname: &str) -> Option<&str> {
    let digits = nickname.strip_prefix(CA_NAME)?.strip_prefix(' ')?;
    (digits.len() == NICKNAME_DIGITS
        && digits
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
    .then_some(digits)
}

/// `<data>/trust/nss-fingerprints`.
pub(crate) fn record_path(files: &TrustFiles) -> std::path::PathBuf {
    files.dir().join(RECORD)
}

/// The fingerprints in this data folder's record; none when it is not
/// there. Lines that are not a fingerprint are skipped.
///
/// # Errors
///
/// A record that is a link, larger than [`MAX_RECORD_BYTES`], or cannot be
/// read.
pub(super) fn read_record(files: &TrustFiles) -> Result<BTreeSet<Fingerprint>, String> {
    let path = record_path(files);
    let refused = |reason: String| format!("{} {reason}", path.display());
    match fs::symlink_metadata(&path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(err) => return Err(refused(format!("cannot be checked: {}", err.kind()))),
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(refused("is not a file (a link, or a folder)".into()));
        }
        Ok(meta) if meta.len() > MAX_RECORD_BYTES => {
            return Err(refused(format!("is larger than {MAX_RECORD_BYTES} bytes")));
        }
        Ok(_) => {}
    }
    let mut text = String::new();
    File::open(&path)
        .and_then(|file| file.take(MAX_RECORD_BYTES).read_to_string(&mut text))
        .map_err(|err| refused(format!("cannot be read: {}", err.kind())))?;
    Ok(text.lines().filter_map(Fingerprint::parse).collect())
}

/// Writes the record whole, mode 0600; an empty set deletes it.
pub(super) fn write_record(
    files: &TrustFiles,
    fingerprints: &BTreeSet<Fingerprint>,
) -> Result<(), String> {
    let path = record_path(files);
    if fingerprints.is_empty() {
        return match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!("cannot delete {}: {}", path.display(), err.kind())),
        };
    }
    let text: String = fingerprints
        .iter()
        .map(|fp| format!("{}\n", fp.0))
        .collect();
    user_trust::write_pending(&path, &text)
}

#[cfg(test)]
mod tests {
    use super::super::super::tests::{TEST_CA, TEST_CA_2};
    use super::*;

    const OURS: &str = "Marsa Local CA 29e433200254dfce";
    const SECOND: &str = "Marsa Local CA b103833491ca1d3c";
    /// `openssl x509 -in test-ca.pem -outform der | shasum -a 256`.
    const HASH_OURS: &str = "29e433200254dfce602212438e9d64e306b672db9bc4642a7620cebcc0fd8057";

    #[test]
    fn nicknames_and_fingerprints() {
        let ours = Fingerprint::of(TEST_CA).unwrap();
        assert_eq!(ours.0, HASH_OURS);
        assert_eq!(ours.nickname(), OURS);
        assert_eq!(Fingerprint::of(TEST_CA_2).unwrap().nickname(), SECOND);
        assert_eq!(
            Fingerprint::of_body(&user_trust::body(TEST_CA).unwrap()),
            Some(ours.clone())
        );
        assert_eq!(
            Fingerprint::parse(&format!("{}\r", HASH_OURS.to_uppercase())),
            Some(ours)
        );
        for line in ["", "29e4", &format!("{HASH_OURS}0"), &"g".repeat(64)] {
            assert_eq!(Fingerprint::parse(line), None, "{line}");
        }
        assert_eq!(fingerprint_digits(OURS), Some("29e433200254dfce"));
        for nickname in [
            CA_NAME,
            "Marsa Local CA x2",
            "Marsa Local CA 29E433200254DFCE",
        ] {
            assert_eq!(fingerprint_digits(nickname), None, "{nickname}");
        }
    }
}
