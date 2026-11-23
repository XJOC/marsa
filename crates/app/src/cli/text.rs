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

/// `rows` as a table: each column as wide as its widest cell, and two
/// spaces apart, with no space at a line's end. The first row is usually
/// the titles.
pub fn columns<const N: usize>(rows: &[[String; N]]) -> String {
    let mut widths = [0; N];
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut text = String::new();
    for row in rows {
        let mut line = String::new();
        for (cell, width) in row.iter().zip(widths) {
            line.push_str(&format!("{cell:<width$}  "));
        }
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_line_up_and_lines_end_without_spaces() {
        let rows = [
            ["ID".to_owned(), "NAME".to_owned()],
            ["1".to_owned(), "a".to_owned()],
            ["100".to_owned(), String::new()],
        ];
        assert_eq!(columns(&rows), "ID   NAME\n1    a\n100\n");
    }

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
