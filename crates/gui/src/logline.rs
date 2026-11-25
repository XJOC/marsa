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
//! A Laravel site's own log is read in its own format, and only there
//! ([`LaravelReader`]): `[2026-11-24 10:15:03] local.ERROR: message`, then
//! the lines of a stack trace, which go on the entry above them.
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

/// The head of an entry of a Laravel log, as Laravel's line formatter
/// writes it: `[2026-11-24 10:15:03] local.ERROR: Call to undefined method
/// {"exception":"[object] ..."}`.
#[derive(Debug, PartialEq, Eq)]
struct LaravelEntry<'a> {
    /// What is between the brackets, as written: `2026-11-24 10:15:03`.
    stamp: &'a str,
    level: LogLevel,
    /// What follows the level, the context JSON included: the window does
    /// not take a message apart. Empty for an entry that says nothing.
    message: &'a str,
}

/// The head of a Laravel entry, if `text` is one: a stamp in brackets that
/// starts with a digit, a channel and a dot (the environment, such as
/// `local`; a line without one is read too), then one of the eight levels of
/// PSR-3 and a colon. `[stacktrace]` and `[previous exception]` start with
/// no digit, so they are never a head.
fn laravel_entry(text: &str) -> Option<LaravelEntry<'_>> {
    let (stamp, after) = text.strip_prefix('[')?.split_once("] ")?;
    if !stamp.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let (head, rest) = after.split_once(':')?;
    if head.contains(char::is_whitespace) {
        return None;
    }
    let word = match head.rsplit_once('.') {
        Some(("", _)) => return None,
        Some((_, word)) => word,
        None => head,
    };
    let level = match word {
        "DEBUG" | "INFO" | "NOTICE" => LogLevel::Info,
        "WARNING" => LogLevel::Warning,
        "ERROR" | "CRITICAL" | "ALERT" | "EMERGENCY" => LogLevel::Error,
        _ => return None,
    };
    // `local.INFO:` with nothing after it is an entry with no message.
    let message = if rest.is_empty() {
        rest
    } else {
        rest.strip_prefix(' ')?
    };
    Some(LaravelEntry {
        stamp,
        level,
        message,
    })
}

/// How a line of a Laravel log reads, given the lines before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LaravelLine {
    pub(crate) level: LogLevel,
    /// The line goes on the entry above it (its stack trace, the rest of
    /// its context): it has that entry's level.
    pub(crate) continuation: bool,
}

/// Reads a Laravel log one line at a time, oldest first. It keeps the level
/// of the last entry: a line that does not start an entry goes on it and
/// takes its level, so a filter on Error keeps an error's stack trace.
/// [Laravel: `Str::of($line)->match()`, with a memory of the lines before.]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LaravelReader {
    /// The level of the last entry read. `None` before the first: the read
    /// may start inside an entry whose head it never saw.
    entry: Option<LogLevel>,
}

impl LaravelReader {
    pub(crate) fn read(&mut self, text: &str) -> LaravelLine {
        if let Some(entry) = laravel_entry(text) {
            self.entry = Some(entry.level);
            return LaravelLine {
                level: entry.level,
                continuation: false,
            };
        }
        match self.entry {
            Some(level) => LaravelLine {
                level,
                continuation: true,
            },
            // Before any entry: plain information, shown whole.
            None => LaravelLine {
                level: LogLevel::Info,
                continuation: false,
            },
        }
    }
}

/// How many columns the time column of a log with its own stamps takes: a
/// Laravel stamp, `2026-11-24 10:15:03`.
pub const STAMP_COLUMNS: usize = 19;

/// `stamp` cut at [`STAMP_COLUMNS`] characters, for the time column. A
/// project that writes a longer stamp (`2026-11-24T10:15:03.123456+00:00`)
/// keeps its date and time; the tooltip has the whole line.
pub fn stamp_clipped(stamp: &str) -> &str {
    stamp
        .char_indices()
        .nth(STAMP_COLUMNS)
        .map_or(stamp, |(at, _)| &stamp[..at])
}

/// What a row of a Laravel log shows: the stamp of an entry, for the time
/// column, and its message; or no stamp and the whole line, for any other
/// line.
pub(crate) fn laravel_shown(text: &str) -> (Option<&str>, &str) {
    laravel_entry(text).map_or((None, text), |entry| (Some(entry.stamp), entry.message))
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

    /// Each line read in order by one reader: its level, and whether it goes
    /// on the entry above it.
    fn laravel(lines: &[&str]) -> Vec<(LogLevel, bool)> {
        let mut reader = LaravelReader::default();
        lines
            .iter()
            .map(|text| {
                let line = reader.read(text);
                (line.level, line.continuation)
            })
            .collect()
    }

    #[test]
    fn a_laravel_entry_has_its_level_its_stamp_and_its_message_with_the_context_kept() {
        let error = r#"[2026-11-24 10:15:03] local.ERROR: Call to undefined method {"exception":"[object]..."} []"#;
        assert_eq!(
            laravel_entry(error),
            Some(LaravelEntry {
                stamp: "2026-11-24 10:15:03",
                level: LogLevel::Error,
                message: r#"Call to undefined method {"exception":"[object]..."} []"#,
            })
        );
        let warning = "[2026-11-24 10:15:03] production.WARNING: Disk almost full";
        assert_eq!(
            laravel_shown(warning),
            (Some("2026-11-24 10:15:03"), "Disk almost full")
        );
        let line = |word: &str| format!("[2026-11-24 10:15:03] local.{word}: said");
        for (word, want) in [
            ("DEBUG", LogLevel::Info),
            ("INFO", LogLevel::Info),
            ("NOTICE", LogLevel::Info),
            ("WARNING", LogLevel::Warning),
            ("ERROR", LogLevel::Error),
            ("CRITICAL", LogLevel::Error),
            ("ALERT", LogLevel::Error),
            ("EMERGENCY", LogLevel::Error),
        ] {
            let text = line(word);
            let entry = laravel_entry(&text).expect(word);
            assert_eq!((entry.level, entry.message), (want, "said"), "{word}");
        }
    }

    #[test]
    fn the_lines_after_an_entry_go_on_it_and_take_its_level() {
        let lines = [
            "[2026-11-24 10:15:03] local.ERROR: boom {\"exception\":\"[object] (Error(code: 0): boom at /app/Foo.php:12)",
            "[stacktrace]",
            "#0 /app/Foo.php(12): bar()",
            "#1 {main}",
            "\"}",
            "}",
            "",
            "[2026-11-24 10:15:04] local.WARNING: slow",
            "#0 /app/Bar.php(3): baz()",
            "[2026-11-24 10:15:05] local.INFO: ok",
            "[previous exception] [object] (Exception(code: 0): first at /app/Foo.php:9)",
        ];
        let (error, warning, info) = (LogLevel::Error, LogLevel::Warning, LogLevel::Info);
        assert_eq!(
            laravel(&lines),
            [
                (error, false),
                (error, true),
                (error, true),
                (error, true),
                (error, true),
                (error, true),
                (error, true),
                (warning, false),
                (warning, true),
                (info, false),
                (info, true),
            ]
        );
        // A line that goes on an entry is shown whole, with no stamp.
        assert_eq!(
            laravel_shown("#0 /app/Foo.php(12): bar()"),
            (None, "#0 /app/Foo.php(12): bar()")
        );
        assert_eq!(laravel_shown("[stacktrace]"), (None, "[stacktrace]"));
    }

    #[test]
    fn a_line_before_any_entry_is_information_shown_whole() {
        // The read started inside an entry whose head it never saw.
        let lines = [
            "#3 /app/Old.php(1): old()",
            "\"}",
            "",
            "[2026-11-24 10:15:03] local.ERROR: new",
        ];
        assert_eq!(
            laravel(&lines),
            [
                (LogLevel::Info, false),
                (LogLevel::Info, false),
                (LogLevel::Info, false),
                (LogLevel::Error, false),
            ]
        );
        assert_eq!(
            laravel_shown("#3 /app/Old.php(1): old()"),
            (None, "#3 /app/Old.php(1): old()")
        );
    }

    #[test]
    fn a_stamp_and_a_level_without_a_channel_is_an_entry_and_odd_heads_are_not() {
        let bare = laravel_entry("[2026-11-24 10:15:03] ERROR: no channel").expect("an entry");
        assert_eq!((bare.level, bare.message), (LogLevel::Error, "no channel"));
        // An ISO stamp, as a project may set it, is kept as written.
        let iso =
            laravel_entry("[2026-11-24T10:15:03.123456+00:00] testing.INFO: hi").expect("iso");
        assert_eq!(iso.stamp, "2026-11-24T10:15:03.123456+00:00");
        for text in [
            "[2026-11-24 10:15:03] local.LOUD: an unknown level",
            "[2026-11-24 10:15:03] local.error: lower case",
            "[2026-11-24 10:15:03] .ERROR: an empty channel",
            "[2026-11-24 10:15:03] my env.ERROR: a space in the channel",
            "[2026-11-24 10:15:03] local.ERROR:no space",
            "[2026-11-24 10:15:03] no level at all",
            "[stacktrace]",
            "[previous exception] [object] (Exception(code: 0): x)",
            "2026-11-24 10:15:03 local.ERROR: no brackets",
            "",
        ] {
            assert_eq!(laravel_entry(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_laravel_entry_may_have_an_empty_message() {
        for text in [
            "[2026-11-24 10:15:03] local.INFO:",
            "[2026-11-24 10:15:03] local.INFO: ",
        ] {
            let entry = laravel_entry(text).expect(text);
            assert_eq!(
                (entry.level, entry.message),
                (LogLevel::Info, ""),
                "{text:?}"
            );
            assert_eq!(laravel_shown(text), (Some("2026-11-24 10:15:03"), ""));
        }
    }

    #[test]
    fn a_stamp_is_cut_at_the_time_column() {
        assert_eq!(stamp_clipped("2026-11-24 10:15:03"), "2026-11-24 10:15:03");
        assert_eq!(
            stamp_clipped("2026-11-24T10:15:03.123456+00:00"),
            "2026-11-24T10:15:03"
        );
        assert_eq!(stamp_clipped("1"), "1");
        assert_eq!(stamp_clipped(""), "");
        // Cut on a character, never inside one.
        let wide = "٢".repeat(STAMP_COLUMNS + 3);
        assert_eq!(stamp_clipped(&wide).chars().count(), STAMP_COLUMNS);
    }

    #[test]
    fn a_laravel_line_in_a_program_log_is_left_as_it_is() {
        // Caddy's and php-fpm's lines are not read as Laravel's: the format
        // is read only for a site's own log.
        let text = "[2026-11-24 10:15:03] local.ERROR: Call to undefined method";
        assert_eq!(level(text), LogLevel::Info);
        assert_eq!(message(text), text);
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
