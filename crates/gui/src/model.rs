//! The window's state, and how each message from the bridge changes it.
//!
//! Pure: no drawing and no I/O, so the tests run without a window. The
//! drawing code in `app` only reads a [`Model`].

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use marsa_core::resolver;
use marsa_core::runtimes::Runtime;
use marsa_core::types::{
    Event, Health, LogLine, LogStream, RuntimeList, ServiceAction, ServiceState, ServiceStatus,
    Site, SiteList, Source,
};
use semver::Version;

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
    /// Every runtime with its installed versions and default, read when a
    /// session starts and after an install.
    Runtimes(RuntimeList),
    /// The runtimes could not be read. A client error code.
    RuntimesFailed(String),
    /// The daemon saved this change. The new state comes as an event.
    Saved(Change),
    /// The daemon refused this change, or could not be reached. A client
    /// error code.
    NotSaved {
        change: Change,
        code: String,
    },
    Event(Event),
}

/// One runtime in the Runtimes section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRow {
    pub runtime: Runtime,
    /// Oldest first.
    pub installed: Vec<Version>,
    /// As saved in `global.toml` (`8.4`). Always `None` for Composer and
    /// Caddy.
    pub default: Option<String>,
}

/// One entry of a PHP or Node list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// A site with no setting of its own: its version file, its
    /// `composer.json`, or the default picks the version. Sites only.
    Auto,
    /// This installed version.
    Version(Version),
}

/// A setting the window changes through the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// `PUT /v1/runtimes/{tool}/default` with this text (`8.4`).
    Default { runtime: Runtime, version: String },
    /// `PATCH /v1/sites/{name}` with this PHP text. `None` clears the site's
    /// own setting.
    SitePhp { site: String, php: Option<String> },
}

impl Change {
    fn setting(&self) -> Setting<'_> {
        match self {
            Change::Default { runtime, .. } => Setting::Default(*runtime),
            Change::SitePhp { site, .. } => Setting::SitePhp(site),
        }
    }
}

/// Which setting a [`Change`] is about. Borrows the site name, so finding
/// the pending change of a row copies nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setting<'a> {
    Default(Runtime),
    SitePhp(&'a str),
}

/// A change sent to the daemon and not answered yet, with the entry its
/// list shows meanwhile. A refusal shows the daemon's value again.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    change: Change,
    shown: Choice,
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
    /// In [`Runtime::ALL`] order.
    pub runtimes: Vec<RuntimeRow>,
    /// The last runtimes read or default change failed. A client error code.
    pub runtimes_notice: Option<String>,
    /// Oldest first.
    pending: Vec<Pending>,
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
            Update::Runtimes(list) => self.set_runtimes(list),
            Update::RuntimesFailed(code) => self.runtimes_notice = Some(code),
            Update::Saved(change) => self.settle(&change),
            Update::NotSaved { change, code } => {
                self.settle(&change);
                match change {
                    Change::Default { .. } => self.runtimes_notice = Some(code),
                    Change::SitePhp { .. } => self.sites_notice = Some(SitesNotice::Failed(code)),
                }
            }
            Update::Event(event) => apply(self, event),
        }
    }

    /// The installed PHP versions, oldest first.
    pub fn php_versions(&self) -> &[Version] {
        self.row(Runtime::Php)
            .map_or(&[], |row| row.installed.as_slice())
    }

    /// The entry a site's PHP list shows: the one the user just picked, the
    /// version of the site's own setting, or [`Choice::Auto`].
    pub fn site_choice(&self, site: &Site) -> Choice {
        if let Some(shown) = self.pending_choice(Setting::SitePhp(&site.name)) {
            return shown.clone();
        }
        match (&site.php_source, &site.php) {
            (Some(Source::ProjectSetting), Some(version)) => Choice::Version(version.clone()),
            _ => Choice::Auto,
        }
    }

    /// The user picked `choice` in the PHP list of the site `name`. Returns
    /// the change to send, or `None` if it is the entry shown already. The
    /// list shows `choice` until the daemon answers.
    pub fn choose_site_php(&mut self, name: &str, choice: Choice) -> Option<Change> {
        let site = self.sites.sites.iter().find(|site| site.name == name)?;
        if self.site_choice(site) == choice {
            return None;
        }
        let php = match &choice {
            Choice::Auto => None,
            Choice::Version(version) => {
                Some(version_text(Runtime::Php, version, self.php_versions()))
            }
        };
        let change = Change::SitePhp {
            site: name.to_owned(),
            php,
        };
        // The notice was about the last action.
        self.sites_notice = None;
        self.pending.push(Pending {
            change: change.clone(),
            shown: choice,
        });
        Some(change)
    }

    /// The installed version the default of `runtime` picks now, or the one
    /// the user just picked. `None` when there is no default, or it matches
    /// no installed version.
    pub fn default_choice(&self, runtime: Runtime) -> Option<Version> {
        if let Some(Choice::Version(version)) = self.pending_choice(Setting::Default(runtime)) {
            return Some(version.clone());
        }
        let row = self.row(runtime)?;
        let text = row.default.as_deref()?;
        resolver::best_match(text, &row.installed).ok().flatten()
    }

    /// The user picked `version` as the default of `runtime`. Returns the
    /// change to send, or `None` if it is the default already.
    pub fn choose_default(&mut self, runtime: Runtime, version: Version) -> Option<Change> {
        if !has_default(runtime) || self.default_choice(runtime).as_ref() == Some(&version) {
            return None;
        }
        let installed = self.row(runtime).map_or(&[][..], |row| &row.installed);
        let change = Change::Default {
            runtime,
            version: version_text(runtime, &version, installed),
        };
        self.runtimes_notice = None;
        self.pending.push(Pending {
            change: change.clone(),
            shown: Choice::Version(version),
        });
        Some(change)
    }

    fn row(&self, runtime: Runtime) -> Option<&RuntimeRow> {
        self.runtimes.iter().find(|row| row.runtime == runtime)
    }

    /// What the newest unanswered change of `setting` shows, if any.
    fn pending_choice(&self, setting: Setting<'_>) -> Option<&Choice> {
        // Newest first: the user may pick again before the first answer.
        self.pending
            .iter()
            .rev()
            .find(|pending| pending.change.setting() == setting)
            .map(|pending| &pending.shown)
    }

    /// The daemon answered `change`: its list shows the daemon's value again.
    fn settle(&mut self, change: &Change) {
        if let Some(index) = self.pending.iter().position(|p| p.change == *change) {
            self.pending.remove(index);
        }
    }

    /// Versions that are not semver are left out: the daemon lists only
    /// folders named by a version.
    fn set_runtimes(&mut self, list: RuntimeList) {
        self.runtimes = list
            .runtimes
            .into_iter()
            .map(|runtime| {
                let mut installed: Vec<Version> = runtime
                    .installed
                    .iter()
                    .filter_map(|text| text.parse().ok())
                    .collect();
                installed.sort();
                RuntimeRow {
                    runtime: runtime.runtime,
                    installed,
                    default: runtime.default,
                }
            })
            .collect();
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
        self.runtimes.clear();
        // No answer comes from a lost session.
        self.pending.clear();
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
        Event::DefaultChanged(default) => {
            let row = model
                .runtimes
                .iter_mut()
                .find(|r| r.runtime == default.runtime);
            if let Some(row) = row {
                row.default = Some(default.default);
            }
        }
        // No setup button yet (task M4-13 adds the route and the CLI only).
        Event::SetupFinished(_) => {}
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

/// Whether `runtime` has a default version. Composer and Caddy have none: the
/// newest installed version is used.
pub fn has_default(runtime: Runtime) -> bool {
    matches!(runtime, Runtime::Php | Runtime::Node)
}

/// The text to save for `chosen`: its line (`8.4` for PHP, `22` for Node)
/// when the line picks `chosen` now, so later patch releases follow, as with
/// `marsa use 8.4` and the default set by the first install. Otherwise the
/// full version, so an older patch release stays the one used.
pub fn version_text(runtime: Runtime, chosen: &Version, installed: &[Version]) -> String {
    let line = match runtime {
        Runtime::Node => chosen.major.to_string(),
        _ => format!("{}.{}", chosen.major, chosen.minor),
    };
    match resolver::best_match(&line, installed) {
        Ok(Some(best)) if best == *chosen => line,
        _ => chosen.to_string(),
    }
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
            php_source: None,
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

    // -- The PHP and Node lists (task M5-02) --

    fn v(text: &str) -> Version {
        text.parse().unwrap()
    }

    /// The daemon's runtimes list: PHP and Node with these versions and
    /// defaults, Composer and Caddy with none.
    fn runtime_list(php: (&[&str], Option<&str>), node: (&[&str], Option<&str>)) -> RuntimeList {
        let row = |runtime, (installed, default): (&[&str], Option<&str>)| {
            marsa_core::types::RuntimeVersions {
                runtime,
                installed: installed.iter().map(|t| (*t).to_owned()).collect(),
                default: default.map(str::to_owned),
            }
        };
        RuntimeList {
            runtimes: vec![
                row(Runtime::Php, php),
                row(Runtime::Node, node),
                row(Runtime::Composer, (&["2.8.3"], None)),
                row(Runtime::Caddy, (&[], None)),
            ],
        }
    }

    fn php_site(name: &str, php: Option<&str>, source: Option<Source>) -> Site {
        Site {
            php: php.map(v),
            php_source: source,
            ..site(name, true)
        }
    }

    /// Connected, with PHP 8.3.27 and 8.4.23 (default `8.4`), Node 22, and
    /// these sites.
    fn with_php(sites: Vec<Site>) -> Model {
        let mut model = connected();
        let php: &[&str] = &["8.4.23", "8.3.27"];
        model.update(Update::Runtimes(runtime_list(
            (php, Some("8.4")),
            (&["22.11.0"], None),
        )));
        model.update(Update::Sites(SiteList {
            sites,
            ..SiteList::default()
        }));
        model
    }

    fn site_named<'a>(model: &'a Model, name: &str) -> &'a Site {
        model.sites.sites.iter().find(|s| s.name == name).unwrap()
    }

    fn choice(model: &Model, name: &str) -> Choice {
        model.site_choice(site_named(model, name))
    }

    fn site_php(site: &str, php: Option<&str>) -> Change {
        Change::SitePhp {
            site: site.into(),
            php: php.map(str::to_owned),
        }
    }

    #[test]
    fn the_runtimes_list_has_each_default() {
        let model = with_php(Vec::new());
        assert_eq!(model.php_versions(), [v("8.3.27"), v("8.4.23")]);
        assert_eq!(model.default_choice(Runtime::Php), Some(v("8.4.23")));
        // Node has no default yet; Composer never has one.
        assert_eq!(model.default_choice(Runtime::Node), None);
        assert_eq!(model.default_choice(Runtime::Composer), None);
        assert!(has_default(Runtime::Node) && !has_default(Runtime::Caddy));

        // A default that matches nothing installed picks nothing; its text
        // stays for the window to show.
        let mut model = connected();
        model.update(Update::Runtimes(runtime_list(
            (&["8.4.23"], Some("8.2")),
            (&[], None),
        )));
        assert_eq!(model.default_choice(Runtime::Php), None);
        assert_eq!(model.runtimes[0].default.as_deref(), Some("8.2"));
    }

    #[test]
    fn picking_a_default_sends_its_line_and_shows_it_until_the_answer() {
        let mut model = with_php(Vec::new());
        let change = model.choose_default(Runtime::Php, v("8.3.27"));
        let want = Change::Default {
            runtime: Runtime::Php,
            version: "8.3".into(),
        };
        assert_eq!(change, Some(want.clone()));
        assert_eq!(model.default_choice(Runtime::Php), Some(v("8.3.27")));

        // Saved: the daemon's value again, which its event has changed.
        apply(&mut model, default_changed(Runtime::Php, "8.3", "8.3.27"));
        model.update(Update::Saved(want));
        assert_eq!(model.default_choice(Runtime::Php), Some(v("8.3.27")));
        assert_eq!(model.runtimes[0].default.as_deref(), Some("8.3"));

        // The default already, and a runtime with no default: nothing to send.
        assert_eq!(model.choose_default(Runtime::Php, v("8.3.27")), None);
        assert_eq!(model.choose_default(Runtime::Composer, v("2.8.3")), None);
        // Node lines are majors.
        let node = model.choose_default(Runtime::Node, v("22.11.0"));
        let want = Change::Default {
            runtime: Runtime::Node,
            version: "22".into(),
        };
        assert_eq!(node, Some(want));
    }

    fn default_changed(runtime: Runtime, default: &str, version: &str) -> Event {
        Event::DefaultChanged(marsa_core::types::DefaultVersion {
            runtime,
            default: default.into(),
            version: version.into(),
        })
    }

    #[test]
    fn a_refused_default_goes_back_and_says_why() {
        let mut model = with_php(Vec::new());
        let change = model.choose_default(Runtime::Php, v("8.3.27")).unwrap();
        let code = "version_not_installed".to_owned();
        model.update(Update::NotSaved { change, code });
        assert_eq!(model.default_choice(Runtime::Php), Some(v("8.4.23")));
        assert_eq!(
            model.runtimes_notice.as_deref(),
            Some("version_not_installed")
        );
        // The next pick clears the notice.
        model.choose_default(Runtime::Php, v("8.3.27"));
        assert!(model.runtimes_notice.is_none());
    }

    #[test]
    fn a_default_set_elsewhere_reaches_the_list() {
        // `marsa use php 8.3` in a terminal.
        let mut model = with_php(Vec::new());
        apply(&mut model, default_changed(Runtime::Php, "8.3", "8.3.27"));
        assert_eq!(model.default_choice(Runtime::Php), Some(v("8.3.27")));
    }

    #[test]
    fn the_line_is_saved_only_when_it_picks_the_same_version() {
        let installed = [v("8.3.27"), v("8.4.22"), v("8.4.23"), v("8.5.0-rc1")];
        for (runtime, chosen, want) in [
            (Runtime::Php, "8.4.23", "8.4"),
            // An older patch release stays the one used.
            (Runtime::Php, "8.4.22", "8.4.22"),
            (Runtime::Php, "8.3.27", "8.3"),
            // `8.5` never picks a pre-release.
            (Runtime::Php, "8.5.0-rc1", "8.5.0-rc1"),
        ] {
            assert_eq!(
                version_text(runtime, &v(chosen), &installed),
                want,
                "{chosen}"
            );
        }
        let node = [v("20.18.0"), v("22.11.0")];
        assert_eq!(version_text(Runtime::Node, &v("22.11.0"), &node), "22");
    }

    #[test]
    fn a_site_that_follows_the_default_can_be_pinned() {
        let shop = php_site("shop", Some("8.4.23"), Some(Source::GlobalDefault));
        let mut model = with_php(vec![shop]);
        assert_eq!(choice(&model, "shop"), Choice::Auto);
        assert_eq!(model.choose_site_php("shop", Choice::Auto), None);

        // The same version as the default is a pin too: the default may move.
        let change = model.choose_site_php("shop", Choice::Version(v("8.4.23")));
        assert_eq!(change, Some(site_php("shop", Some("8.4"))));
        assert_eq!(choice(&model, "shop"), Choice::Version(v("8.4.23")));
    }

    #[test]
    fn a_site_with_a_version_file_keeps_it_until_a_version_is_picked() {
        let file = Source::VersionFile(PathBuf::from("/code/shop/.php-version"));
        let shop = php_site("shop", Some("8.3.27"), Some(file));
        let mut model = with_php(vec![shop]);
        assert_eq!(choice(&model, "shop"), Choice::Auto);

        let change = model.choose_site_php("shop", Choice::Version(v("8.4.23")));
        assert_eq!(change.clone(), Some(site_php("shop", Some("8.4"))));
        // Saved, and the new list says the site's own setting won.
        let pinned = php_site("shop", Some("8.4.23"), Some(Source::ProjectSetting));
        apply(
            &mut model,
            Event::SitesChanged(SiteList {
                sites: vec![pinned],
                ..SiteList::default()
            }),
        );
        model.update(Update::Saved(change.unwrap()));
        assert_eq!(choice(&model, "shop"), Choice::Version(v("8.4.23")));
    }

    #[test]
    fn the_first_entry_clears_the_sites_own_setting() {
        let shop = php_site("shop", Some("8.3.27"), Some(Source::ProjectSetting));
        let mut model = with_php(vec![shop]);
        assert_eq!(choice(&model, "shop"), Choice::Version(v("8.3.27")));
        assert_eq!(
            model.choose_site_php("shop", Choice::Version(v("8.3.27"))),
            None
        );

        let change = model.choose_site_php("shop", Choice::Auto);
        assert_eq!(change, Some(site_php("shop", None)));
        assert_eq!(choice(&model, "shop"), Choice::Auto);
        // No such site: nothing to send.
        assert_eq!(model.choose_site_php("gone", Choice::Auto), None);
    }

    #[test]
    fn a_refused_site_php_goes_back_and_says_why() {
        let shop = php_site("shop", Some("8.4.23"), Some(Source::GlobalDefault));
        let mut model = with_php(vec![shop]);
        let change = model.choose_site_php("shop", Choice::Version(v("8.3.27")));
        let code = "config_write".to_owned();
        model.update(Update::NotSaved {
            change: change.unwrap(),
            code,
        });
        assert_eq!(choice(&model, "shop"), Choice::Auto);
        let failed = SitesNotice::Failed("config_write".into());
        assert_eq!(model.sites_notice, Some(failed));
    }

    #[test]
    fn the_newest_pick_shows_until_its_own_answer() {
        let shop = php_site("shop", Some("8.4.23"), Some(Source::GlobalDefault));
        let mut model = with_php(vec![shop]);
        let first = model.choose_site_php("shop", Choice::Version(v("8.3.27")));
        let second = model.choose_site_php("shop", Choice::Auto);
        assert_eq!(second, Some(site_php("shop", None)));
        // The first answer does not hide the second pick.
        model.update(Update::Saved(first.unwrap()));
        assert_eq!(choice(&model, "shop"), Choice::Auto);
    }

    #[test]
    fn a_sites_event_moves_the_lists_without_a_reload() {
        let shop = php_site("shop", Some("8.4.23"), Some(Source::GlobalDefault));
        let mut model = with_php(vec![shop]);
        // `marsa` pinned it in a terminal.
        let pinned = php_site("shop", Some("8.3.27"), Some(Source::ProjectSetting));
        apply(
            &mut model,
            Event::SitesChanged(SiteList {
                sites: vec![pinned],
                ..SiteList::default()
            }),
        );
        assert_eq!(choice(&model, "shop"), Choice::Version(v("8.3.27")));
    }

    #[test]
    fn a_lost_session_drops_the_runtimes_and_the_picks() {
        let shop = php_site("shop", Some("8.4.23"), Some(Source::GlobalDefault));
        let mut model = with_php(vec![shop.clone()]);
        model.choose_site_php("shop", Choice::Version(v("8.3.27")));
        model.update(Update::Reconnecting);
        assert!(model.runtimes.is_empty() && model.php_versions().is_empty());
        // The pick was not saved: the old value shows with the new session.
        model.update(Update::Connected(health()));
        model.update(Update::Sites(SiteList {
            sites: vec![shop],
            ..SiteList::default()
        }));
        assert_eq!(choice(&model, "shop"), Choice::Auto);
        model.update(Update::RuntimesFailed("runtimes_read".into()));
        assert_eq!(model.runtimes_notice.as_deref(), Some("runtimes_read"));
    }
}
