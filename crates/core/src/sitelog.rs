//! The pure parts of reading a Laravel site's log: which file names are
//! Laravel's, which file is the newest, and how bytes become lines. `marsad`
//! opens and reads the files (`daemon::sites::log`); nothing here touches the
//! disk.

use std::time::SystemTime;

/// The most of a file's end that is read at first: older lines are left out
/// (`SiteLog::cut_at_start`).
pub const MAX_TAIL_BYTES: u64 = 1024 * 1024;

/// The most of a line that is kept, as for a service's output. The rest of
/// the line is read and dropped, never held.
pub const MAX_LINE_BYTES: usize = 8 * 1024;

/// The most new bytes read in one look at a followed file; the rest waits for
/// the next look.
pub const MAX_STEP_BYTES: u64 = 256 * 1024;

/// The most lines kept from the first read: a file of empty lines would
/// otherwise give a million of them from [`MAX_TAIL_BYTES`].
pub const MAX_TAIL_LINES: usize = 10_000;

/// A look at a followed file stops reading once it has this many new lines.
pub const MAX_STEP_LINES: usize = 5_000;

/// Ends a line that was cut at [`MAX_LINE_BYTES`].
pub const CUT_MARK: char = '…';

/// Whether `name` is one of Laravel's own log files: `laravel.log` (the
/// `single` channel) or `laravel-YYYY-MM-DD.log` (the `daily` channel), with
/// digits only. Any other file in `storage/logs` is never read.
pub fn is_log_name(name: &str) -> bool {
    if name == "laravel.log" {
        return true;
    }
    let Some(date) = name
        .strip_prefix("laravel-")
        .and_then(|rest| rest.strip_suffix(".log"))
    else {
        return false;
    };
    let digits = |part: &str, len: usize| {
        part.len() == len && part.bytes().all(|byte| byte.is_ascii_digit())
    };
    let parts: Vec<&str> = date.split('-').collect();
    matches!(parts.as_slice(), [year, month, day]
        if digits(year, 4) && digits(month, 2) && digits(day, 2))
}

/// The newest of `files` (name and modification time) whose name passes
/// [`is_log_name`]: the latest time, and on a tie the greater name, so the
/// choice never depends on the order of the folder.
pub fn newest<'a>(files: impl IntoIterator<Item = (&'a str, SystemTime)>) -> Option<&'a str> {
    files
        .into_iter()
        .filter(|(name, _)| is_log_name(name))
        .max_by(|(a_name, a_time), (b_name, b_time)| {
            a_time.cmp(b_time).then_with(|| a_name.cmp(b_name))
        })
        .map(|(name, _)| name)
}

/// Turns the bytes of a file, given in any pieces, into lines. Holds at most
/// [`MAX_LINE_BYTES`] of a line: a longer one is cut and ends with
/// [`CUT_MARK`], and its rest is dropped as it comes. A line ends at `\n`;
/// a `\r` before it is removed (Windows line ends). The text is UTF-8 with
/// bad bytes replaced.
#[derive(Debug, Default)]
pub struct LineSplitter {
    /// The line so far, at most [`MAX_LINE_BYTES`].
    partial: Vec<u8>,
    /// The line passed [`MAX_LINE_BYTES`].
    cut: bool,
    /// Drop everything up to the first `\n`: reading started inside a line.
    skip_first: bool,
}

impl LineSplitter {
    /// A splitter for a read that starts at a line's start (`false`), or
    /// somewhere in a line, whose first piece is then dropped (`true`).
    pub fn new(mid_line: bool) -> Self {
        Self {
            skip_first: mid_line,
            ..Self::default()
        }
    }

    /// Adds `bytes`, and pushes each line they end to `out`. A last line with
    /// no `\n` yet stays inside until the next bytes, or [`Self::finish`].
    pub fn push(&mut self, mut bytes: &[u8], out: &mut Vec<String>) {
        while let Some(end) = bytes.iter().position(|&byte| byte == b'\n') {
            if self.skip_first {
                self.skip_first = false;
            } else {
                self.take(&bytes[..end]);
                out.push(self.line());
            }
            bytes = &bytes[end + 1..];
        }
        if !self.skip_first {
            self.take(bytes);
        }
    }

    /// The last line, when the file ends without `\n`. `None` if no bytes
    /// are waiting.
    pub fn finish(&mut self) -> Option<String> {
        (!self.skip_first && (!self.partial.is_empty() || self.cut)).then(|| self.line())
    }

    /// Keeps what fits of `bytes` in the line: one byte over
    /// [`MAX_LINE_BYTES`], so a line of exactly that size and its `\r` is not
    /// cut.
    fn take(&mut self, bytes: &[u8]) {
        let room = (MAX_LINE_BYTES + 1).saturating_sub(self.partial.len());
        if bytes.len() > room {
            self.cut = true;
        }
        self.partial
            .extend_from_slice(&bytes[..bytes.len().min(room)]);
    }

    /// The line held, and a fresh start.
    fn line(&mut self) -> String {
        let mut bytes = std::mem::take(&mut self.partial);
        let mut cut = std::mem::take(&mut self.cut);
        if !cut && bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        if bytes.len() > MAX_LINE_BYTES {
            cut = true;
        }
        if cut {
            bytes.truncate(MAX_LINE_BYTES);
            bytes.truncate(whole_chars(&bytes));
        }
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if cut {
            text.push(CUT_MARK);
        }
        text
    }
}

/// The length of `bytes` without a UTF-8 character that the cut split in two
/// at its end, so the cut drops it instead of showing a replacement mark.
fn whole_chars(bytes: &[u8]) -> usize {
    // A character is at most 4 bytes: its lead byte is among the last 3.
    for back in 1..=3.min(bytes.len()) {
        let at = bytes.len() - back;
        let byte = bytes[at];
        if byte & 0xC0 == 0x80 {
            continue;
        }
        let size = match byte {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => 1,
        };
        return if size > back { at } else { bytes.len() };
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn laravel_names_pass_and_near_names_do_not() {
        for name in [
            "laravel.log",
            "laravel-2026-11-24.log",
            "laravel-0000-00-00.log",
        ] {
            assert!(is_log_name(name), "{name}");
        }
        for name in [
            "laravel-2026-1-1.log",
            "laravel.log.1",
            "laravel-2026-11-24.log.bak",
            "laravel-2026-11-24-1.log",
            "laravel-2026-11-2a.log",
            "laravel-.log",
            "worker.log",
            "Laravel.log",
            "laravel-２０26-11-24.log",
            "../laravel.log",
        ] {
            assert!(!is_log_name(name), "{name}");
        }
    }

    #[test]
    fn newest_takes_the_latest_time_then_the_greater_name() {
        let at = |secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        assert_eq!(newest([("laravel.log", at(5))]), Some("laravel.log"));
        let daily = [
            ("laravel-2026-11-23.log", at(5)),
            ("laravel-2026-11-24.log", at(9)),
            ("worker.log", at(99)),
        ];
        assert_eq!(newest(daily), Some("laravel-2026-11-24.log"));
        let tie = [
            ("laravel-2026-11-24.log", at(5)),
            ("laravel.log", at(5)),
            ("laravel-2026-11-23.log", at(5)),
        ];
        assert_eq!(newest(tie), Some("laravel.log"));
        assert_eq!(newest([("laravel.log.1", at(5))]), None);
        assert_eq!(newest([]), None);
    }

    fn split(splitter: &mut LineSplitter, pieces: &[&[u8]]) -> Vec<String> {
        let mut out = Vec::new();
        for piece in pieces {
            splitter.push(piece, &mut out);
        }
        out
    }

    #[test]
    fn lines_end_at_newline_in_any_pieces_and_lose_their_cr() {
        let mut splitter = LineSplitter::new(false);
        let lines = split(&mut splitter, &[b"one\r\ntw", b"o\n\nthr", b"ee"]);
        assert_eq!(lines, ["one", "two", ""]);
        assert_eq!(splitter.finish().as_deref(), Some("three"));
        assert_eq!(splitter.finish(), None);
    }

    #[test]
    fn a_start_inside_a_line_drops_its_first_piece() {
        let mut splitter = LineSplitter::new(true);
        assert_eq!(
            split(&mut splitter, &[b"half", b" line\nwhole\n"]),
            ["whole"]
        );
        let mut splitter = LineSplitter::new(true);
        assert_eq!(split(&mut splitter, &[b"no end"]), Vec::<String>::new());
        assert_eq!(splitter.finish(), None);
    }

    #[test]
    fn a_long_line_is_cut_and_its_rest_dropped() {
        let mut splitter = LineSplitter::new(false);
        let long = vec![b'a'; 100 * 1024];
        let lines = split(&mut splitter, &[&long, &long, b"\nnext\n"]);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), MAX_LINE_BYTES + CUT_MARK.len_utf8());
        assert!(lines[0].ends_with(CUT_MARK));
        assert_eq!(lines[1], "next");
        assert!(splitter.partial.capacity() <= 2 * MAX_LINE_BYTES);
    }

    #[test]
    fn a_cut_never_splits_a_character_and_bad_bytes_are_replaced() {
        let mut splitter = LineSplitter::new(false);
        let mut line = vec![b'a'; MAX_LINE_BYTES - 1];
        line.extend_from_slice("é and more\n".as_bytes());
        let lines = split(&mut splitter, &[&line]);
        assert_eq!(
            lines[0],
            format!("{}{CUT_MARK}", "a".repeat(MAX_LINE_BYTES - 1))
        );
        let lines = split(&mut splitter, &[b"bad \xff byte\n"]);
        assert_eq!(lines, ["bad \u{fffd} byte"]);
        // An earlier bad byte does not stop the trim of a split character.
        let mut line = b"\xff".to_vec();
        line.extend(vec![b'a'; MAX_LINE_BYTES - 2]);
        line.extend_from_slice("€ more\n".as_bytes());
        let lines = split(&mut splitter, &[&line]);
        assert!(lines[0].ends_with(&format!("a{CUT_MARK}")));
    }

    #[test]
    fn a_line_of_exactly_the_limit_and_its_cr_is_not_cut() {
        let mut splitter = LineSplitter::new(false);
        let mut line = vec![b'a'; MAX_LINE_BYTES];
        line.extend_from_slice(b"\r\n");
        let lines = split(
            &mut splitter,
            &[&line[..MAX_LINE_BYTES], &line[MAX_LINE_BYTES..]],
        );
        assert_eq!(lines, ["a".repeat(MAX_LINE_BYTES)]);
        let mut line = vec![b'a'; MAX_LINE_BYTES + 1];
        line.push(b'\n');
        let lines = split(&mut splitter, &[&line]);
        assert_eq!(lines, [format!("{}{CUT_MARK}", "a".repeat(MAX_LINE_BYTES))]);
    }
}
