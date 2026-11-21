//! Text from the daemon or from a folder name, made safe to print: a control
//! character could move the cursor or recolor the terminal.

/// `text` without control characters.
pub fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// [`printable`], but line breaks and tabs stay.
pub fn printable_multiline(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// [`printable`], and at most `most` characters.
pub fn clipped(text: &str, most: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(most)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_dropped() {
        assert_eq!(printable("a\u{1b}[2Jb\nc\td"), "a[2Jbcd");
        assert_eq!(printable_multiline("a\u{1b}[2Jb\nc\td"), "a[2Jb\nc\td");
    }

    #[test]
    fn a_version_from_the_daemon_is_clipped() {
        assert_eq!(clipped("0.1.0", 64), "0.1.0");
        assert_eq!(clipped("0.1\n\u{1b}[2J.0", 64), "0.1[2J.0");
        assert_eq!(clipped(&"9".repeat(100), 64).len(), 64);
    }
}
