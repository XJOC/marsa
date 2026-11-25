//! The log of each service, the log of the Laravel site that Logs shows, and
//! the source the Logs screen reads.

use std::collections::VecDeque;

use marsa_core::types::{LogLine, LogStream, ServiceId, SiteKind, SiteLogLine};

use super::{Model, Route};
use crate::bridge::Command;
use crate::logline::{self, LaravelReader, LogLevel};

/// The most log lines kept for each service, and for a site's log. The
/// oldest go first.
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
    /// The line goes on the entry above it, whose level it has: a line of a
    /// stack trace in a Laravel log. Always `false` in a program's log.
    pub continuation: bool,
}

/// What the window keeps of one log: the newest [`MAX_LOG_LINES`] lines.
#[derive(Debug, Default)]
pub struct ServiceLog {
    /// Oldest first.
    pub rows: VecDeque<LogRow>,
    /// The `seq` of the newest line in `rows`.
    pub(super) last_seq: Option<u64>,
}

impl ServiceLog {
    /// Adds the line `seq`, made by `row`, unless the log has it already
    /// (the recent lines and the live ones overlap). `row` runs only for a
    /// new line.
    fn push(&mut self, seq: u64, row: impl FnOnce() -> LogRow) {
        if self.last_seq.is_some_and(|last| seq <= last) {
            return;
        }
        self.last_seq = Some(seq);
        if self.rows.len() == MAX_LOG_LINES {
            self.rows.pop_front();
        }
        self.rows.push_back(row());
    }
}

/// The log of a Laravel site while Logs shows it: the bridge follows one at
/// a time, and only while the screen is open (rules.md 10).
#[derive(Debug)]
pub(super) struct SiteLogState {
    /// The site, by name.
    site: String,
    log: ServiceLog,
    /// Reads each line as it comes, in order: a line of a stack trace takes
    /// the level of its entry.
    reader: LaravelReader,
    /// Why the daemon does not send this log: a client error code.
    failed: Option<String>,
}

/// The log the Logs screen shows: a service's, by its id, or a Laravel
/// site's own. The screen opens on Caddy's, the one the window always reads
/// (the other logs are the services the daemon lists, and its Laravel sites).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LogSource {
    #[default]
    Caddy,
    /// Any other service the daemon lists, such as `php-fpm-8.4`. Made with
    /// [`LogSource::of`], so Caddy is never written twice.
    Service(ServiceId),
    /// The log a Laravel site writes (`storage/logs`), by the site's name.
    /// The daemon reads it; the window follows it only while it is shown.
    Site(String),
}

impl LogSource {
    /// The source of the service `id`.
    pub fn of(id: ServiceId) -> Self {
        if id == ServiceId::Caddy {
            Self::Caddy
        } else {
            Self::Service(id)
        }
    }

    /// The id of the service whose log this is. `None` for a site's log.
    pub fn service(&self) -> Option<ServiceId> {
        match self {
            Self::Caddy => Some(ServiceId::Caddy),
            Self::Service(id) => Some(*id),
            Self::Site(_) => None,
        }
    }
}

impl Model {
    /// Adds a line to the log of its service, unless the window has it
    /// already (the recent lines and the live ones overlap).
    pub(super) fn push_log(&mut self, line: LogLine) {
        let log = self.session.logs.entry(line.service).or_default();
        log.push(line.seq, || {
            let read = logline::read(&line.text);
            let (level, columns) = (read.level, logline::columns(&read.message));
            LogRow {
                stream: line.stream,
                level,
                columns,
                text: line.text,
                continuation: false,
            }
        });
    }

    /// Adds lines of the log of `site`, if it is the one followed: a line of
    /// a reader the window has stopped since is dropped.
    pub(super) fn push_site_log(&mut self, site: &str, lines: Vec<SiteLogLine>) {
        let Some(state) = self.followed_site_log(site) else {
            return;
        };
        let SiteLogState { log, reader, .. } = state;
        for line in lines {
            log.push(line.seq, || {
                let read = reader.read(&line.text);
                let (_, message) = logline::laravel_shown(&line.text);
                LogRow {
                    // A file has no streams: every line is the site's own.
                    stream: LogStream::Stderr,
                    level: read.level,
                    columns: logline::columns(message),
                    text: line.text,
                    continuation: read.continuation,
                }
            });
        }
    }

    /// The daemon did not send the log of `site`, or stopped: why.
    pub(super) fn site_log_failed(&mut self, site: &str, code: String) {
        if let Some(state) = self.followed_site_log(site) {
            state.failed = Some(code);
        }
    }

    fn followed_site_log(&mut self, site: &str) -> Option<&mut SiteLogState> {
        self.session
            .site_log
            .as_mut()
            .filter(|state| state.site == site)
    }

    /// The lines of the log of the service `id`, oldest first. Empty when it
    /// has written none.
    pub fn log_rows(&self, id: ServiceId) -> &VecDeque<LogRow> {
        static NONE: VecDeque<LogRow> = VecDeque::new();
        self.session.logs.get(&id).map_or(&NONE, |log| &log.rows)
    }

    /// The lines of the log of `source`, oldest first. A site's log has
    /// lines only while it is followed.
    pub fn source_rows(&self, source: &LogSource) -> &VecDeque<LogRow> {
        static NONE: VecDeque<LogRow> = VecDeque::new();
        match source {
            LogSource::Caddy => self.log_rows(ServiceId::Caddy),
            LogSource::Service(id) => self.log_rows(*id),
            LogSource::Site(name) => self
                .session
                .site_log
                .as_ref()
                .filter(|state| state.site == *name)
                .map_or(&NONE, |state| &state.log.rows),
        }
    }

    /// Why the log of `site` is not read: a client error code, such as
    /// `site_log_unsafe`.
    pub fn site_log_error(&self, site: &str) -> Option<&str> {
        let state = self.session.site_log.as_ref()?;
        if state.site == site {
            state.failed.as_deref()
        } else {
            None
        }
    }

    /// Whether the site list has `name`, as a Laravel project.
    pub fn is_laravel_site(&self, name: &str) -> bool {
        self.session
            .sites
            .sites
            .iter()
            .any(|site| site.name == name && site.kind == SiteKind::Laravel)
    }

    /// The route or the site list changed: if Logs shows the log of a site
    /// that is not in the list, or is no longer Laravel, it shows Caddy's
    /// instead. With no daemon the list is empty, so nothing is decided.
    pub(super) fn leave_gone_site_log(&mut self) {
        if let Route::Logs(LogSource::Site(name)) = &self.route
            && self.is_connected()
            && !self.is_laravel_site(name)
        {
            self.route = Route::Logs(LogSource::Caddy);
        }
    }

    /// What the bridge's reader of a site's log must do, for
    /// [`Model::next_command`]: follow the site Logs shows, once a session,
    /// and stop when the screen shows another log or another screen. A
    /// follow that failed or ended is not sent again until the screen opens
    /// again or the window reconnects: never once a frame.
    pub(super) fn next_site_log_command(&mut self) -> Option<Command> {
        let shown = match &self.route {
            Route::Logs(LogSource::Site(name)) => Some(name.as_str()),
            _ => None,
        };
        let followed = self
            .session
            .site_log
            .as_ref()
            .map(|state| state.site.as_str());
        if shown.is_some() && shown == followed {
            return None;
        }
        match shown {
            Some(name) if self.is_connected() && self.is_laravel_site(name) => {
                let site = name.to_owned();
                self.session.site_log = Some(SiteLogState {
                    site: site.clone(),
                    log: ServiceLog::default(),
                    reader: LaravelReader::default(),
                    failed: None,
                });
                Some(Command::FollowSiteLog(site))
            }
            // Another screen or another log: the reader stops, and its
            // lines go.
            _ => self.session.site_log.take().map(|_| Command::StopSiteLog),
        }
    }
}
