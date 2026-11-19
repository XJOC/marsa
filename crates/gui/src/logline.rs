//! What a line of a program's output says: how serious it is,
//! and the message without the parts the Logs screen shows elsewhere. The
//! daemon sends the text as the program wrote it, so the window reads the two
//! formats the programs it runs use today:
//!
//! - Caddy writes one JSON object per line when its output is not a
//!   terminal: `{"level":"error","ts":1763476328.1,"logger":"http.log.error",
//!   "msg":"dial tcp 127.0.0.1:9000: connection refused", ...}`.
//! - php-fpm writes `[18-Nov-2026 14:32:09] NOTICE: fpm is running, pid 4251`.
//!
//! A line in neither format is plain information with its whole text as the
//! message: the window does not guess a level from the words in it.
//!
//! Pure: no drawing and no I/O. [Laravel: a small value parser, like a
//! `Str` helper.]

use std::borrow::Cow;

use serde::Deserialize;

/// How serious a line is, as its own text says (components.md, Logs). The
/// filters of the Logs screen are these three.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    /// Debug, info, and notice lines, and every line that says no level.
    Info,
    /// A warning.
    Warning,
    /// An error, or worse: fatal, panic, alert.
    Error,
}

impl LogLevel {
    pub const ALL: [Self; 3] = [Self::Info, Self::Warning, Self::Error];

    const fn bit(self) -> u8 {
        match self {
            Self::Info => 1,
            Self::Warning => 2,
            Self::Error => 4,
        }
    }
}

/// The levels the user turned off in the Logs filters. Nothing off, which is
/// what [`Default`] is, shows every line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HiddenLevels(u8);

impl HiddenLevels {
    /// Whether lines of `level` are left out.
    pub const fn hides(self, level: LogLevel) -> bool {
        self.0 & level.bit() != 0
    }

    /// `level` turned off if it was on, and on if it was off.
    #[must_use]
    pub const fn toggled(self, level: LogLevel) -> Self {
        Self(self.0 ^ level.bit())
    }

    /// No level is left out.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// The fields of a Caddy line the window reads. Every other field is
/// skipped while parsing.
#[derive(Deserialize)]
struct CaddyLine<'a> {
    level: Option<&'a str>,
    #[serde(borrow)]
    msg: Option<Cow<'a, str>>,
    #[serde(borrow)]
    error: Option<Cow<'a, str>>,
}

/// A line that says no more than its level and its message.
#[derive(Debug)]
pub struct Parsed<'a> {
    pub level: LogLevel,
    pub message: Cow<'a, str>,
}

fn parse(text: &str) -> Option<Parsed<'_>> {
    caddy(text).or_else(|| fpm(text))
}

/// The level and the message of `text`, read once. A line in no known
/// format is information, shown whole.
pub fn read(text: &str) -> Parsed<'_> {
    parse(text).unwrap_or(Parsed {
        level: LogLevel::Info,
        message: Cow::Borrowed(text),
    })
}

/// The most columns of a message the Logs screen draws: a line longer than
/// this ends in an ellipsis, and the tooltip has it whole. It bounds what one
/// row costs to lay out, and how far the list scrolls sideways.
pub const MAX_LINE_COLUMNS: usize = 4000;

/// How many columns of the mono face `message` takes, at most
/// [`MAX_LINE_COLUMNS`]. A character that is not ASCII counts for two: the
/// room is never short, and a line with a wide character scrolls a little
/// further than it must.
pub fn columns(message: &str) -> usize {
    let wide = message
        .chars()
        .map(|c| if c.is_ascii() { 1 } else { 2 })
        .sum::<usize>();
    wide.min(MAX_LINE_COLUMNS)
}

/// `message`, or its first columns and an ellipsis if it is longer than
/// [`MAX_LINE_COLUMNS`].
pub fn clipped(message: &str) -> Cow<'_, str> {
    if columns(message) < MAX_LINE_COLUMNS {
        return Cow::Borrowed(message);
    }
    let mut kept = String::new();
    let mut used = 0;
    for c in message.chars() {
        used += if c.is_ascii() { 1 } else { 2 };
        if used >= MAX_LINE_COLUMNS {
            break;
        }
        kept.push(c);
    }
    kept.push('…');
    Cow::Owned(kept)
}

fn caddy(text: &str) -> Option<Parsed<'_>> {
    if !text.starts_with('{') {
        return None;
    }
    let line: CaddyLine<'_> = serde_json::from_str(text).ok()?;
    let level = match line.level? {
        "debug" | "info" => LogLevel::Info,
        "warn" => LogLevel::Warning,
        "error" | "dpanic" | "panic" | "fatal" => LogLevel::Error,
        _ => return None,
    };
    let message = match (line.msg, line.error) {
        (Some(msg), Some(error)) => Cow::Owned(format!("{msg}: {error}")),
        (Some(msg), None) => msg,
        (None, Some(error)) => error,
        // A line with a level and nothing to say: show it whole.
        (None, None) => Cow::Borrowed(text),
    };
    Some(Parsed { level, message })
}

fn fpm(text: &str) -> Option<Parsed<'_>> {
    // `[18-Nov-2026 14:32:09] WARNING: [pool www] child 4 said into stderr`.
    let (_, after) = text.strip_prefix('[')?.split_once("] ")?;
    let (word, message) = after.split_once(": ")?;
    let level = match word {
        "DEBUG" | "NOTICE" => LogLevel::Info,
        "WARNING" => LogLevel::Warning,
        "ERROR" | "ALERT" => LogLevel::Error,
        _ => return None,
    };
    Some(Parsed {
        level,
        message: Cow::Borrowed(message),
    })
}

/// How serious the line `text` is.
pub fn level(text: &str) -> LogLevel {
    read(text).level
}

/// The line without what the screen shows elsewhere (its level, and the
/// JSON around a Caddy message). A line in no known format is shown whole.
pub fn message(text: &str) -> Cow<'_, str> {
    read(text).message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caddy_line_has_the_level_it_says_and_its_message_without_the_json() {
        let cases = [
            (
                r#"{"level":"info","ts":1.7e9,"msg":"serving initial configuration"}"#,
                LogLevel::Info,
            ),
            (r#"{"level":"debug","msg":"d"}"#, LogLevel::Info),
            (
                r#"{"level":"warn","ts":1,"logger":"tls","msg":"certificate expires soon"}"#,
                LogLevel::Warning,
            ),
            (
                r#"{"level":"error","msg":"dial tcp: refused"}"#,
                LogLevel::Error,
            ),
            (r#"{"level":"panic","msg":"p"}"#, LogLevel::Error),
            (r#"{"level":"fatal","msg":"f"}"#, LogLevel::Error),
        ];
        for (text, level) in cases {
            assert_eq!(super::level(text), level, "{text}");
        }
        assert_eq!(
            message(r#"{"level":"info","ts":1.7e9,"msg":"serving initial configuration"}"#),
            "serving initial configuration"
        );
        // An escaped quote and a unicode escape are the message's own text.
        assert_eq!(
            message(r#"{"level":"info","msg":"say \"hi\" <3"}"#),
            "say \"hi\" <3"
        );
    }

    #[test]
    fn an_error_field_follows_the_message_and_fields_of_other_shapes_are_skipped() {
        let text = r#"{"level":"error","msg":"reverse proxy failed","error":"connection refused","request":{"level":"info","msg":"nested","error":"no"},"status":502}"#;
        assert_eq!(level(text), LogLevel::Error);
        assert_eq!(message(text), "reverse proxy failed: connection refused");
        // Only an error: it is the message.
        assert_eq!(message(r#"{"level":"error","error":"boom"}"#), "boom");
        // A level and nothing to say: the line itself.
        let bare = r#"{"level":"warn"}"#;
        assert_eq!(message(bare), bare);
        assert_eq!(level(bare), LogLevel::Warning);
    }

    #[test]
    fn a_php_fpm_line_has_its_level_and_its_message_without_the_stamp() {
        let line = |word: &str| format!("[18-Nov-2026 14:32:09] {word}: fpm is running, pid 4251");
        for (word, want) in [
            ("DEBUG", LogLevel::Info),
            ("NOTICE", LogLevel::Info),
            ("WARNING", LogLevel::Warning),
            ("ERROR", LogLevel::Error),
            ("ALERT", LogLevel::Error),
        ] {
            assert_eq!(level(&line(word)), want, "{word}");
            assert_eq!(message(&line(word)), "fpm is running, pid 4251", "{word}");
        }
        // The pool's own bracket stays in the message.
        let pool = "[18-Nov-2026 14:32:09] WARNING: [pool www] child 4 said into stderr";
        assert_eq!(message(pool), "[pool www] child 4 said into stderr");
    }

    #[test]
    fn a_line_in_no_known_format_is_information_and_is_shown_whole() {
        for text in [
            "",
            "ready",
            "panic: runtime error: index out of range",
            "ERROR: not at the start",
            "[18-Nov-2026 14:32:09] LOUD: unknown word",
            // JSON that is cut off, or has an unknown level.
            r#"{"level":"info","msg":"cut"#,
            r#"{"level":"shouting","msg":"x"}"#,
            r#"{"msg":"no level"}"#,
            "{not json}",
        ] {
            assert_eq!(level(text), LogLevel::Info, "{text:?}");
            assert_eq!(message(text), text, "{text:?}");
        }
    }

    #[test]
    fn a_message_takes_its_columns_and_a_very_long_one_is_cut_with_an_ellipsis() {
        assert_eq!(columns(""), 0);
        assert_eq!(columns("ready"), 5);
        // A character that is not ASCII counts for two: the room is never short.
        assert_eq!(columns("é漢"), 4);
        let long = "x".repeat(MAX_LINE_COLUMNS + 500);
        assert_eq!(columns(&long), MAX_LINE_COLUMNS);
        assert_eq!(clipped("short"), "short");
        let cut = clipped(&long);
        assert!(cut.ends_with('…'));
        assert_eq!(cut.chars().count(), MAX_LINE_COLUMNS);
        // Just under the limit: whole.
        let fits = "y".repeat(MAX_LINE_COLUMNS - 1);
        assert_eq!(clipped(&fits), fits.as_str());
        // Wide characters are cut by their columns, not their count.
        let wide = "漢".repeat(MAX_LINE_COLUMNS);
        assert!(columns(&clipped(&wide)) <= MAX_LINE_COLUMNS);
    }

    #[test]
    fn the_levels_off_are_a_small_set_that_starts_empty() {
        let none = HiddenLevels::default();
        assert!(none.is_empty());
        for level in LogLevel::ALL {
            assert!(!none.hides(level));
        }
        let one = none.toggled(LogLevel::Warning);
        assert!(one.hides(LogLevel::Warning));
        assert!(!one.hides(LogLevel::Info) && !one.hides(LogLevel::Error));
        // Each level is its own bit: all three can be off.
        let all = LogLevel::ALL.into_iter().fold(none, HiddenLevels::toggled);
        assert!(LogLevel::ALL.into_iter().all(|level| all.hides(level)));
        // Toggling twice is the same as not.
        assert_eq!(one.toggled(LogLevel::Warning), none);
    }
}
