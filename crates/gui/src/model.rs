//! The window's state, and how each message from the bridge changes it.
//!
//! Pure: no drawing and no I/O, so the tests run without a window. The
//! drawing code in `app` only reads a [`Model`].

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use marsa_core::types::{
    Event, Health, LogLine, LogStream, ServiceAction, ServiceState, ServiceStatus, Site, SiteList,
};

/// The service whose log the window shows.
pub const LOG_SERVICE: &str = "caddy";
/// The most log lines kept. The oldest go first.
pub const MAX_LOG_LINES: usize = 5000;

/// What the bridge sends to the window.
#[derive(Debug)]
pub enum Update {
    /// Connected. A new session: the daemon may have restarted, so the old
    /// table and log are dropped.
    Connected(Health),
    /// The connection ended; the bridge tries again every second.
    Reconnecting,
    /// The daemon cannot be reached or started. A client error code; the
    /// bridge tries again every second.
    Failed(String),
    /// The whole services table.
    Services(Vec<ServiceStatus>),
    /// Recent lines of [`LOG_SERVICE`], oldest first.
    Log(Vec<LogLine>),
    /// A call failed but the connection is fine. A client error code.
    Notice(String),
    /// The whole site list, read when a session starts and again after the
    /// daemon dropped events.
    Sites(SiteList),
    /// The daemon accepted this folder to park. The new list comes as
    /// `sites_changed`.
    Parked(PathBuf),
    /// A sites call failed but the connection is fine. A client error code.
    SitesFailed(String),
    Event(Event),
}

/// The status line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Connection {
    #[default]
    Connecting,
    Reconnecting,
    Running(Health),
    ShuttingDown,
    Failed(String),
}

/// The line under the park field: what the last sites action did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SitesNotice {
    /// The daemon accepted this folder to park.
    Parked(PathBuf),
    /// A sites action failed. A client error code.
    Failed(String),
}

/// One log line, without the fields the window does not show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    pub stream: LogStream,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct Model {
    pub connection: Connection,
    /// Sorted by id.
    pub services: Vec<ServiceStatus>,
    pub log: VecDeque<LogRow>,
    /// The `seq` of the newest line in `log`.
    last_seq: Option<u64>,
    /// The last error that did not break the connection, as a code.
    pub notice: Option<String>,
    /// Sites sorted by name, the parked folders, and the skipped folders.
    pub sites: SiteList,
    /// The text in the park field. Kept across sessions: it is the user's
    /// typing, not daemon state.
    pub park_input: String,
    pub sites_notice: Option<SitesNotice>,
}

impl Model {
    pub fn update(&mut self, update: Update) {
        match update {
            Update::Connected(health) => {
                *self = Model {
                    connection: Connection::Running(health),
                    park_input: std::mem::take(&mut self.park_input),
                    ..Model::default()
                };
            }
            Update::Reconnecting => self.disconnect(Connection::Reconnecting),
            Update::Failed(code) => self.disconnect(Connection::Failed(code)),
            Update::Services(mut services) => {
                services.sort_by(|a, b| a.id.cmp(&b.id));
                self.services = services;
            }
            Update::Log(lines) => lines.into_iter().for_each(|line| self.push_log(line)),
            Update::Notice(code) => self.notice = Some(code),
            Update::Sites(list) => self.set_sites(list),
            Update::Parked(path) => {
                // The user may have typed another folder since.
                if park_path(&self.park_input).as_ref() == Ok(&path) {
                    self.park_input.clear();
                }
                self.sites_notice = Some(SitesNotice::Parked(path));
            }
            Update::SitesFailed(code) => self.sites_notice = Some(SitesNotice::Failed(code)),
            Update::Event(event) => apply(self, event),
        }
    }

    /// The folder to park from the park field, or `None` if the text is not
    /// an absolute path; the notice line then says why. Clears the last
    /// notice either way: it was about the last action.
    pub fn park_request(&mut self) -> Option<PathBuf> {
        match park_path(&self.park_input) {
            Ok(path) => {
                self.sites_notice = None;
                Some(path)
            }
            Err(code) => {
                self.sites_notice = Some(SitesNotice::Failed(code.to_owned()));
                None
            }
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.connection, Connection::Running(_))
    }

    /// The tables show live state only, so they go. The log stays: it may
    /// tell why the daemon stopped.
    fn disconnect(&mut self, connection: Connection) {
        self.connection = connection;
        self.services.clear();
        self.sites = SiteList::default();
    }

    /// The daemon sends the sites sorted by name already; sorting here keeps
    /// the table right even if a daemon does not.
    fn set_sites(&mut self, mut list: SiteList) {
        list.sites.sort_by(|a, b| a.name.cmp(&b.name));
        self.sites = list;
    }

    fn push_log(&mut self, line: LogLine) {
        if line.service != LOG_SERVICE || self.last_seq.is_some_and(|last| line.seq <= last) {
            return;
        }
        self.last_seq = Some(line.seq);
        if self.log.len() == MAX_LOG_LINES {
            self.log.pop_front();
        }
        self.log.push_back(LogRow {
            stream: line.stream,
            text: line.text,
        });
    }
}

/// Applies one daemon event.
pub fn apply(model: &mut Model, event: Event) {
    match event {
        Event::ServiceChanged(status) => {
            // A new run of the log service may get a new log buffer whose
            // `seq` starts again at 0, so the old `seq` must not hide it.
            if status.id == LOG_SERVICE && status.state == ServiceState::Starting {
                model.last_seq = None;
            }
            match model.services.binary_search_by(|s| s.id.cmp(&status.id)) {
                Ok(index) => model.services[index] = status,
                Err(index) => model.services.insert(index, status),
            }
        }
        Event::Log(line) => model.push_log(line),
        // The window has no install view yet (task M3-12 leaves it out).
        Event::Install(_) => {}
        // The whole new list: no need to ask the daemon again.
        Event::SitesChanged(list) => model.set_sites(list),
        Event::ShuttingDown => model.disconnect(Connection::ShuttingDown),
    }
}

/// The address of a site in the browser: `https://shop.test`, or
/// `http://shop.test` when HTTPS is off.
pub fn url(site: &Site) -> String {
    let scheme = if site.secure { "https" } else { "http" };
    format!("{scheme}://{}", site.host())
}

/// The folder the park field names, as the daemon wants it: absolute, with
/// `.` parts, doubled separators, and a trailing separator removed, as
/// `marsa park` does. Spaces around the text go, and one pair of double
/// quotes around it too ("Copy as path" in Windows Explorer adds them).
///
/// # Errors
///
/// `site_path_relative` for a relative or empty path. The daemon would
/// answer the same, so no request is sent.
pub fn park_path(input: &str) -> Result<PathBuf, &'static str> {
    let text = input.trim();
    let text = text
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(text);
    let path = Path::new(text);
    if !path.is_absolute() {
        return Err("site_path_relative");
    }
    // Not `canonicalize`: it reads the disk, follows links, and gives `\\?\`
    // paths on Windows.
    Ok(path.components().collect())
}

/// Whether the button for `action` makes sense in `state`.
pub fn can(state: &ServiceState, action: ServiceAction) -> bool {
    match action {
        ServiceAction::Start => {
            matches!(state, ServiceState::Stopped | ServiceState::Failed { .. })
        }
        ServiceAction::Stop | ServiceAction::Restart => {
            matches!(state, ServiceState::Starting | ServiceState::Running)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health() -> Health {
        Health {
            app_version: "0.0.0".into(),
            api_version: 1,
            pid: 4242,
        }
    }

    fn connected() -> Model {
        let mut model = Model::default();
        model.update(Update::Connected(health()));
        model
    }

    fn status(id: &str, state: ServiceState) -> ServiceStatus {
        ServiceStatus {
            id: id.into(),
            pid: (state == ServiceState::Running).then_some(7),
            state,
            restarts: 0,
            since_ms: None,
        }
    }

    fn changed(id: &str, state: ServiceState) -> Event {
        Event::ServiceChanged(status(id, state))
    }

    fn log_line(service: &str, seq: u64) -> LogLine {
        LogLine {
            service: service.into(),
            seq,
            at_ms: 0,
            stream: LogStream::Stderr,
            text: format!("line {seq}"),
        }
    }

    fn line(service: &str, seq: u64) -> Event {
        Event::Log(log_line(service, seq))
    }

    fn texts(model: &Model) -> Vec<&str> {
        model.log.iter().map(|row| row.text.as_str()).collect()
    }

    fn site(name: &str, secure: bool) -> Site {
        let root = PathBuf::from("/code").join(name);
        Site {
            name: name.into(),
            docroot: root.join("public"),
            root,
            kind: marsa_core::types::SiteKind::Laravel,
            php: None,
            secure,
        }
    }

    fn site_list(names: &[&str]) -> SiteList {
        SiteList {
            parked: vec![PathBuf::from("/code")],
            sites: names.iter().map(|name| site(name, true)).collect(),
            skipped: Vec::new(),
        }
    }

    fn names(model: &Model) -> Vec<&str> {
        model.sites.sites.iter().map(|s| s.name.as_str()).collect()
    }

    /// An absolute path on this system, from Unix-style parts. `cfg!`
    /// because a Windows path needs a drive to be absolute.
    fn absolute(unix: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{}", unix.replace('/', "\\")))
        } else {
            PathBuf::from(unix)
        }
    }

    #[test]
    fn a_service_event_changes_the_table() {
        let mut model = connected();
        apply(&mut model, changed("php", ServiceState::Stopped));
        apply(&mut model, changed("caddy", ServiceState::Starting));
        apply(&mut model, changed("caddy", ServiceState::Running));
        let expected = [
            status("caddy", ServiceState::Running),
            status("php", ServiceState::Stopped),
        ];
        assert_eq!(model.services, expected);
    }

    #[test]
    fn the_log_keeps_the_newest_5000_lines() {
        let mut model = connected();
        for seq in 0..6000 {
            apply(&mut model, line("caddy", seq));
        }
        let texts = texts(&model);
        assert_eq!(texts.len(), MAX_LOG_LINES);
        assert_eq!((texts[0], texts[4999]), ("line 1000", "line 5999"));
    }

    #[test]
    fn a_repeated_or_old_seq_is_ignored() {
        let mut model = connected();
        model.update(Update::Log(vec![log_line("caddy", 4)]));
        for seq in [4, 3, 5, 5] {
            apply(&mut model, line("caddy", seq));
        }
        assert_eq!(texts(&model), ["line 4", "line 5"]);
    }

    #[test]
    fn a_new_run_may_start_its_seq_again() {
        let mut model = connected();
        apply(&mut model, line("caddy", 9));
        apply(&mut model, changed("caddy", ServiceState::Starting));
        apply(&mut model, line("caddy", 0));
        assert_eq!(texts(&model), ["line 9", "line 0"]);
    }

    #[test]
    fn lines_of_other_services_are_not_shown() {
        let mut model = connected();
        apply(&mut model, line("php", 0));
        assert!(model.log.is_empty());
    }

    #[test]
    fn shutting_down_changes_the_status_line() {
        let mut model = connected();
        apply(&mut model, changed("caddy", ServiceState::Running));
        apply(&mut model, Event::ShuttingDown);
        assert_eq!(model.connection, Connection::ShuttingDown);
        assert!(model.services.is_empty());
        assert_eq!(
            crate::strings::status_line(&model.connection),
            "Daemon: shutting down"
        );
    }

    #[test]
    fn a_new_session_starts_clean_and_a_lost_one_keeps_the_log() {
        let mut model = connected();
        apply(&mut model, line("caddy", 3));
        model.update(Update::Notice("service_not_found".into()));
        model.update(Update::Reconnecting);
        assert_eq!(model.connection, Connection::Reconnecting);
        assert_eq!(texts(&model), ["line 3"]);

        model.update(Update::Connected(health()));
        assert!(model.is_connected());
        assert!(model.log.is_empty() && model.notice.is_none());
        // The restarted daemon counts from 0 again.
        apply(&mut model, line("caddy", 0));
        assert_eq!(texts(&model), ["line 0"]);
    }

    #[test]
    fn buttons_match_the_state() {
        use ServiceAction::{Restart, Start, Stop};
        let failed = ServiceState::Failed {
            code: "exited".into(),
        };
        let cases = [
            (ServiceState::Stopped, [true, false, false]),
            (failed, [true, false, false]),
            (ServiceState::Starting, [false, true, true]),
            (ServiceState::Running, [false, true, true]),
            (ServiceState::Stopping, [false, false, false]),
        ];
        for (state, expected) in cases {
            assert_eq!(
                [Start, Stop, Restart].map(|a| can(&state, a)),
                expected,
                "{state:?}"
            );
        }
    }

    #[test]
    fn a_sites_event_replaces_the_whole_list() {
        let mut model = connected();
        model.update(Update::Sites(site_list(&["blog", "shop"])));
        let mut next = site_list(&["api"]);
        next.skipped.push(marsa_core::types::SkippedFolder {
            path: PathBuf::from("/code/متجر"),
            code: "site_name_invalid".into(),
        });
        apply(&mut model, Event::SitesChanged(next.clone()));
        assert_eq!(model.sites, next);
    }

    #[test]
    fn sites_are_sorted_by_name() {
        let mut model = connected();
        model.update(Update::Sites(site_list(&["shop", "api", "blog"])));
        assert_eq!(names(&model), ["api", "blog", "shop"]);
        apply(&mut model, Event::SitesChanged(site_list(&["zoo", "app"])));
        assert_eq!(names(&model), ["app", "zoo"]);
    }

    #[test]
    fn the_url_follows_the_https_setting() {
        assert_eq!(url(&site("shop", true)), "https://shop.test");
        assert_eq!(url(&site("shop", false)), "http://shop.test");
    }

    #[test]
    fn the_sites_go_with_the_connection_but_the_typed_folder_stays() {
        let mut model = connected();
        model.update(Update::Sites(site_list(&["shop"])));
        model.park_input = "/code".into();
        model.update(Update::Reconnecting);
        assert!(model.sites.sites.is_empty() && model.sites.parked.is_empty());

        model.update(Update::Connected(health()));
        assert_eq!(model.park_input, "/code");
        assert!(model.sites_notice.is_none());
    }

    #[test]
    fn the_park_field_needs_an_absolute_path() {
        let code = absolute("/code");
        let shown = code.display().to_string();
        for input in [
            shown.clone(),
            format!("  {shown}  "),
            format!("\"{shown}\""),
            format!("{shown}/"),
            format!("{shown}/./"),
        ] {
            assert_eq!(park_path(&input), Ok(code.clone()), "{input:?}");
        }
        for input in ["", "   ", "code", "./code", "~/code", "\"\""] {
            assert_eq!(park_path(input), Err("site_path_relative"), "{input:?}");
        }
    }

    #[test]
    fn a_relative_folder_is_refused_without_a_request() {
        let mut model = connected();
        model.park_input = "code".into();
        assert_eq!(model.park_request(), None);
        let failed = SitesNotice::Failed("site_path_relative".into());
        assert_eq!(model.sites_notice, Some(failed));

        model.park_input = absolute("/code").display().to_string();
        assert_eq!(model.park_request(), Some(absolute("/code")));
        assert!(model.sites_notice.is_none());
    }

    #[test]
    fn a_park_clears_the_field_and_says_so() {
        let mut model = connected();
        model.park_input = format!("{}/", absolute("/code").display());
        model.update(Update::Parked(absolute("/code")));
        assert!(model.park_input.is_empty());
        let parked = SitesNotice::Parked(absolute("/code"));
        assert_eq!(model.sites_notice, Some(parked));

        // The user typed another folder meanwhile: it stays.
        model.park_input = absolute("/work").display().to_string();
        model.update(Update::Parked(absolute("/code")));
        assert_eq!(model.park_input, absolute("/work").display().to_string());
    }

    #[test]
    fn a_failed_sites_call_keeps_the_list_and_shows_the_code() {
        let mut model = connected();
        model.update(Update::Sites(site_list(&["shop"])));
        model.update(Update::SitesFailed("site_not_found".into()));
        assert_eq!(names(&model), ["shop"]);
        let failed = SitesNotice::Failed("site_not_found".into());
        assert_eq!(model.sites_notice, Some(failed));
    }
}
