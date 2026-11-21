//! The log of each service: the newest lines, and the source the Logs screen reads.

use std::collections::VecDeque;

use marsa_core::types::{LogLine, LogStream};

use super::Model;
use crate::logline::{self, LogLevel};

/// The service whose log the Logs screen opens on, and the one the window
/// always reads (the other logs are the services the daemon lists).
pub const LOG_SERVICE: &str = "caddy";

/// The most log lines kept for each service. The oldest go first.
pub const MAX_LOG_LINES: usize = 5000;

/// One log line, without the fields the window does not show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    pub stream: LogStream,
    /// Read from the text once, when the line arrives (`logline`).
    pub level: LogLevel,
    /// The columns of the mono face the message takes (`logline::columns`),
    /// so the list knows how far to scroll sideways without laying out a
    /// line it does not draw.
    pub columns: usize,
    /// The line as the program wrote it.
    pub text: String,
}

/// What the window keeps of one service's output: the newest
/// [`MAX_LOG_LINES`] lines.
#[derive(Debug, Default)]
pub struct ServiceLog {
    /// Oldest first.
    pub rows: VecDeque<LogRow>,
    /// The `seq` of the newest line in `rows`.
    pub(super) last_seq: Option<u64>,
}

/// The log the Logs screen shows: one service's, by its id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LogSource {
    #[default]
    Caddy,
    /// Any other service the daemon lists, such as `php-fpm-8.4`. Made with
    /// [`LogSource::of`], so Caddy is never written twice.
    Service(String),
}

impl LogSource {
    /// The source of the service `id`.
    pub fn of(id: &str) -> Self {
        if id == LOG_SERVICE {
            Self::Caddy
        } else {
            Self::Service(id.to_owned())
        }
    }

    /// The id of the service whose log this is.
    pub fn id(&self) -> &str {
        match self {
            Self::Caddy => LOG_SERVICE,
            Self::Service(id) => id,
        }
    }
}

impl Model {
    /// Adds a line to the log of its service, unless the window has it
    /// already (the recent lines and the live ones overlap).
    pub(super) fn push_log(&mut self, line: LogLine) {
        let log = self.session.logs.entry(line.service).or_default();
        if log.last_seq.is_some_and(|last| line.seq <= last) {
            return;
        }
        log.last_seq = Some(line.seq);
        if log.rows.len() == MAX_LOG_LINES {
            log.rows.pop_front();
        }
        let read = logline::read(&line.text);
        let (level, columns) = (read.level, logline::columns(&read.message));
        log.rows.push_back(LogRow {
            stream: line.stream,
            level,
            columns,
            text: line.text,
        });
    }

    /// The lines of the log of the service `id`, oldest first. Empty when it
    /// has written none.
    pub fn log_rows(&self, id: &str) -> &VecDeque<LogRow> {
        static NONE: VecDeque<LogRow> = VecDeque::new();
        self.session.logs.get(id).map_or(&NONE, |log| &log.rows)
    }
}
