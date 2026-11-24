//! Standard base64, the one decoder the crate uses.

/// Standard base64 with `=` padding. Line breaks may sit between any two
/// characters, and nothing else may. The unused bits of the last character
/// must be zero, so every input has exactly one encoding.
pub(crate) fn decode(body: &[u8]) -> Option<Vec<u8>> {
    let chars: Vec<u8> = body
        .iter()
        .copied()
        .filter(|&c| c != b'\r' && c != b'\n')
        .collect();
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

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn base64_is_strict() {
        assert_eq!(decode(b"TWFu"), Some(b"Man".to_vec()));
        assert_eq!(decode(b"TWE="), Some(b"Ma".to_vec()));
        assert_eq!(decode(b"TQ=="), Some(b"M".to_vec()));
        // Unused bits that are not zero: a second encoding of "M".
        assert_eq!(decode(b"TR=="), None);
        assert_eq!(decode(b"TQ==TWFu"), None);
        assert_eq!(decode(b"TWF"), None);
        assert_eq!(decode(b"TW u"), None);
    }
}
