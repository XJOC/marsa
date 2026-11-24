//! A Laravel site's own log, read by `marsad`: `GET /v1/sites/{name}/log`.
//! The window and the CLI never open the project's files.

use serde::{Deserialize, Serialize};

/// The end of the site's newest log file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteLog {
    /// The file's name only (`laravel-2026-11-24.log`), never its path.
    /// `None`: the site has no log yet, which is not an error.
    pub file: Option<String>,
    /// Oldest first.
    pub lines: Vec<SiteLogLine>,
    /// The file has older lines that were not read (only its last
    /// `sitelog::MAX_TAIL_BYTES` are).
    pub cut_at_start: bool,
}

/// One line of the file. A Laravel entry with a stack trace is many lines;
/// the window groups them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteLogLine {
    /// From 0 for each request or stream, one per line, so a client can tell
    /// a gap.
    pub seq: u64,
    /// Without its line end. A line over `sitelog::MAX_LINE_BYTES` is cut
    /// and ends with `…`.
    pub text: String,
}
