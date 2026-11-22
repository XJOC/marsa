//! Lowercase hex, the one encoder the crate uses.

use std::fmt::Write as _;

/// `bytes` as lowercase hex, two characters per byte.
#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing to a `String` cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::encode;

    #[test]
    fn bytes_become_two_lowercase_digits_each() {
        assert_eq!(encode(&[]), "");
        assert_eq!(encode(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }
}
