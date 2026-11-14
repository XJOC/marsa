//! The SHA-256 of Marsa's CA certificate, as the Windows helper service
//! computes it (ADR 0018, task M4-19g1b).
//!
//! The service trusts only the certificate whose hash was given to its
//! `install`, so `marsad` must compute the same hash from the same PEM. The
//! helper does not depend on `marsa-core`, so the strict parser is written
//! twice, here and in `crates/helper/src/ops/trust.rs`; both are tested
//! against `fixtures/helper/test-ca.pem` and the hash in
//! `fixtures/helper/test-ca.sha256`.

use sha2::{Digest, Sha256};

const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
const END: &str = "-----END CERTIFICATE-----";
/// The largest `pem` the helper takes in a request (`request.rs`).
const MAX_PEM_BYTES: usize = 16 * 1024;

/// The SHA-256 of the DER of the one certificate in `pem`, upper-case hex;
/// `None` if `pem` is not exactly one certificate, which the helper refuses
/// too.
pub fn certificate_sha256(pem: &str) -> Option<String> {
    // The helper refuses a larger `pem` in any request, so no hash of one
    // could ever be used.
    if pem.len() > MAX_PEM_BYTES {
        return None;
    }
    let der = parse_pem(pem)?;
    Some(
        Sha256::digest(&der)
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect(),
    )
}

/// The DER of the one certificate in `text`: exactly one `CERTIFICATE`
/// block with only spaces, tabs, and line breaks around it, strict base64,
/// and the shape of an X.509 certificate.
fn parse_pem(text: &str) -> Option<Vec<u8>> {
    let block = text.trim_matches(|c| matches!(c, ' ' | '\t' | '\r' | '\n'));
    let body = block.strip_prefix(BEGIN)?.strip_suffix(END)?;
    let der = decode(body)?;
    is_certificate(&der).then_some(der)
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
/// after it, that holds exactly a SEQUENCE, a SEQUENCE, and a BIT STRING.
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
            .join("helper")
            .join(name)
    }

    fn pem() -> String {
        fs::read_to_string(fixture("test-ca.pem")).unwrap()
    }

    /// The same value the helper's own test reads.
    #[test]
    fn the_hash_is_the_one_the_helper_computes() {
        let expected = fs::read_to_string(fixture("test-ca.sha256")).unwrap();
        assert_eq!(certificate_sha256(&pem()).as_deref(), Some(expected.trim()));
    }

    #[test]
    fn crlf_and_whitespace_around_are_the_same_certificate() {
        let crlf = pem().replace('\n', "\r\n");
        let padded = format!(" \n{}\t\n", pem());
        let hash = certificate_sha256(&pem());
        assert!(hash.is_some());
        assert_eq!(certificate_sha256(&crlf), hash);
        assert_eq!(certificate_sha256(&padded), hash);
    }

    #[test]
    fn anything_but_one_strict_certificate_has_no_hash() {
        let pem = pem();
        let body = pem
            .trim()
            .strip_prefix(BEGIN)
            .unwrap()
            .strip_suffix(END)
            .unwrap();
        let cases = [
            String::new(),
            "not a certificate".to_owned(),
            format!("{pem}{pem}"),
            format!("text\n{pem}"),
            pem.replace("CERTIFICATE", "PRIVATE KEY"),
            format!("{BEGIN}{}{END}", body.replacen('A', "*", 1)),
            format!("{BEGIN}\nAAAA\n{END}"),
            format!("{BEGIN}\n{END}"),
        ];
        for case in cases {
            assert_eq!(certificate_sha256(&case), None, "{case:?}");
        }
    }

    /// The invalid PEMs the helper refuses too (`ops/mod.rs` of the
    /// helper), so the two parsers cannot drift apart.
    #[test]
    fn the_shared_invalid_pems_have_no_hash() {
        let mut seen = 0;
        for entry in fs::read_dir(fixture("pem-invalid")).unwrap() {
            let path = entry.unwrap().path();
            let text = fs::read_to_string(&path).unwrap();
            assert_eq!(certificate_sha256(&text), None, "{}", path.display());
            seen += 1;
        }
        assert!(seen >= 6, "{seen}");
    }

    /// The helper refuses a larger PEM in any request, so it has no hash
    /// here either.
    #[test]
    fn a_pem_over_16_kib_has_no_hash() {
        let padded = format!("{}{}", pem(), "\n".repeat(MAX_PEM_BYTES));
        assert_eq!(certificate_sha256(&padded), None);
    }

    #[test]
    fn base64_is_strict() {
        assert_eq!(decode("TWFu"), Some(b"Man".to_vec()));
        assert_eq!(decode("TWE="), Some(b"Ma".to_vec()));
        assert_eq!(decode("TQ=="), Some(b"M".to_vec()));
        // Unused bits that are not zero: a second encoding of "M".
        assert_eq!(decode("TR=="), None);
        assert_eq!(decode("TQ==TWFu"), None);
        assert_eq!(decode("TWF"), None);
        assert_eq!(decode("TW u"), None);
    }
}
