//! The window's state, and how each message from the bridge changes it.
//!
//! Pure: no drawing and no I/O, so the tests run without a window. The
//! drawing code in `shell` and `screens` reads a [`Model`], and returns an
//! [`Action`] when the user presses something.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use marsa_core::helper::{OpName, OpStatus};
use marsa_core::resolver;
use marsa_core::runtimes::Runtime;
use marsa_core::types::{
    DoctorReport, DumpList, Event, Health, HostsStatus, InstallEvent, InstallFailure,
    InstallProgress, InstallUpdate, LogLine, MailList, MailText, PoolKind, RuntimeList,
    ServiceAction, ServiceId, ServiceState, ServiceStatus, SetupReport, Site, SiteList, Source,
    VersionInUse, XdebugState,
};
use semver::Version;

mod dumps;
mod installs;
mod logs;
mod mail;
mod messages;
mod notices;
mod removals;
mod runtimes;
mod session;
mod setup;
mod sites;

pub use dumps::DumpsState;
use installs::Install;
pub use installs::{InstallJob, InstallStep, InstallView, install_version};
pub use logs::{LogRow, LogSource, MAX_LOG_LINES, ServiceLog};
pub use mail::{MAILPIT, MailState, mailpit_url};
pub use messages::{Action, Update};
pub use notices::{NoticeSource, Notices};
pub use removals::{Removal, UninstallNotice};
pub use runtimes::{
    Change, Choice, MAIN_RUNTIME, RuntimeRow, has_default, runtime_missing, service_description,
    service_name, version_text,
};
pub use session::{Connection, Overall, Session};
use setup::not_added;
pub use setup::{FIRST_PHP, FirstRun, HostsSync, Setup, first_run_needed};
use sites::SITE_IS_PARKED;
pub use sites::{LinkForm, Pick, SitesNotice, folder_path, url};

use crate::bridge::Command;
pub use crate::logline::{HiddenLevels, LogLevel};
pub use crate::prefs::Appearance;
use crate::strings::{self, ReportLine};
pub use crate::tray::menu::Autostart;

/// This build's version, shown in the Settings section.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// What the window does after an [`Action`], beyond changing the model.
/// [`Model::apply_action`] returns it, so `App` has one place to read it.
#[derive(Debug)]
pub enum Effect {
    /// The model changed, and that is all.
    None,
    /// Send a command to the daemon through the bridge.
    Send(Command),
    /// Open the site in the browser.
    OpenSite(String),
    /// Show a folder in the system's file manager.
    OpenFolder(PathBuf),
    /// Open the system's folder dialog.
    PickFolder(Pick),
    /// Open this page of Mailpit's in the browser.
    OpenUrl(String),
    /// Put this text on the clipboard.
    CopyText(String),
}

impl Effect {
    /// The command to send, if this is one.
    pub fn command(self) -> Option<Command> {
        match self {
            Self::Send(command) => Some(command),
            _ => None,
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

impl From<Option<Command>> for Effect {
    fn from(command: Option<Command>) -> Self {
        command.map_or(Self::None, Self::Send)
    }
}

/// The screen the main area shows (code.md, Router). The window's own
/// state, not the daemon's: a reconnect keeps it. [Laravel: the route that
/// picks the view.]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Route {
    #[default]
    Home,
    Sites,
    /// One site's page, by name.
    Site(String),
    /// The Runtimes screen with this runtime chosen.
    Runtimes(Runtime),
    Services,
    Logs(LogSource),
    Mail,
    Dumps,
    Settings,
}

/// The window's state: the daemon's session, and what belongs to the window
/// itself and survives a reconnect.
#[derive(Debug, Default)]
pub struct Model {
    pub session: Session,
    /// The levels the user turned off in the Logs filters. Kept across
    /// sessions: it is the user's choice, not daemon state.
    pub log_hidden: HiddenLevels,
    /// The text of the Logs filter field. Kept across sessions.
    pub log_filter: String,
    /// Follow is off: the Logs screen stays where the user scrolled. Kept
    /// across sessions; `false`, which is what [`Default`] gives, follows.
    pub log_paused: bool,
    /// The text of the Sites search field. Kept across sessions: it is the
    /// user's typing, not daemon state.
    pub sites_search: String,
    /// Open after a folder to link was picked. Kept across sessions: it is
    /// the user's typing, not daemon state.
    pub link: Option<LinkForm>,
    /// The version typed in the install dialog of each runtime. A runtime
    /// with no entry has an empty field: the newest stable version. A map,
    /// not an array in [`Runtime::ALL`] order, so a runtime that core adds
    /// needs nothing here. Kept across sessions.
    install_input: BTreeMap<Runtime, String>,
    /// The runtime the Runtimes screen showed last, so the sidebar's item
    /// opens it again. Kept across sessions.
    runtime_chosen: Option<Runtime>,
    /// Oldest first, at most [`MAX_INSTALLS`]. Only ended installs with no id
    /// survive a reconnect: what the user saw last stays on screen.
    installs: Vec<Install>,
    /// The user closed the first-run panel: it stays closed while this
    /// window is open.
    first_run_closed: bool,
    /// The data folder this window and its daemon use. Shown, never changed.
    pub data_dir: PathBuf,
    /// The user's home folder, read once at start. Display only: a folder
    /// under it is shown as `~/…` (`strings::home_path`), and no program ever
    /// gets the shortened text.
    pub home: Option<PathBuf>,
    /// The screen the main area shows.
    pub route: Route,
    /// The user folded the sidebar to its icons. A narrow window folds it
    /// anyway (`shell::sidebar`).
    pub sidebar_folded: bool,
    /// The Appearance setting: System, Light, or Dark. Read from `gui.toml`
    /// at start; `App` applies a change and writes it back. The window's own
    /// setting, not the daemon's, so a reconnect keeps it.
    pub appearance: Appearance,
}

impl Model {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            ..Self::default()
        }
    }

    pub fn update(&mut self, update: Update) {
        match update {
            Update::Connected(health) => {
                self.forget_installs();
                self.session = Session {
                    connection: Connection::Running(health),
                    ..Session::default()
                };
            }
            Update::Reconnecting => self.disconnect(Connection::Reconnecting),
            Update::Failed(code) => self.disconnect(Connection::Failed(code)),
            Update::Services(mut services) => {
                // By the id text, so `php-fpm-8.4` comes between `mysql` and
                // `postgres`, as in `marsa services`.
                services.sort_by_cached_key(|service| service.id.to_string());
                // The log of a service the daemon no longer lists goes with
                // it: a log never outlives its source.
                self.session
                    .logs
                    .retain(|id, _| services.iter().any(|service| service.id == *id));
                self.session.services = services;
            }
            Update::Log(lines) => lines.into_iter().for_each(|line| self.push_log(line)),
            Update::Notice(code) => self.session.notices.set_code(NoticeSource::Call, code),
            Update::Sites(list) => self.set_sites(list),
            Update::Parked(path) => self.session.notices.set_sites(SitesNotice::Parked(path)),
            Update::Linked { name, folder } => {
                // The user may have picked another folder since.
                let same = self
                    .link
                    .as_ref()
                    .is_some_and(|form| form.folder == folder && form.name.trim() == name);
                if same {
                    self.link = None;
                }
                self.session
                    .notices
                    .set_sites(SitesNotice::Linked { name, folder });
            }
            Update::SitesFailed(code) => self.session.notices.set_sites(SitesNotice::Failed(code)),
            Update::Runtimes(list) => self.set_runtimes(list),
            Update::RuntimesFailed(code) => {
                self.session.notices.set_code(NoticeSource::Runtimes, code)
            }
            Update::Saved(change) => self.settle(&change),
            Update::NotSaved { change, code } => {
                self.settle(&change);
                match change {
                    Change::Default { .. } => {
                        self.session.notices.set_code(NoticeSource::Runtimes, code)
                    }
                    Change::SitePhp { .. } => {
                        self.session.notices.set_sites(SitesNotice::Failed(code))
                    }
                }
            }
            Update::InstallAccepted { job, id } => self.accepted(&job, id),
            Update::InstallRefused { job, code } => self.refused(&job, code),
            Update::SetupDone(report) => self.session.setup = Setup::Finished(report),
            Update::Hosts(status) => self.session.hosts = status,
            Update::Doctor(report) => self.session.doctor = Some(report),
            Update::HostsSynced(report) => {
                self.session.hosts_sync = match not_added(&report) {
                    Some(code) => HostsSync::Failed(code),
                    None => HostsSync::Idle,
                };
            }
            Update::HostsSyncFailed(code) => self.session.hosts_sync = HostsSync::Failed(code),
            Update::SetupFailed(code) => self.session.setup = Setup::Failed(code),
            Update::Autostart(state) => self.session.autostart = state,
            Update::AutostartFailed(code) => {
                self.session.autostart = Autostart::Unknown;
                self.session.notices.set_code(NoticeSource::Autostart, code);
            }
            Update::Removed(removal) => self.removed(removal),
            Update::NotRemoved {
                removal,
                code,
                in_use,
            } => self.not_removed(removal, code, in_use),
            Update::Xdebug(state) => self.session.xdebug = Some(state),
            Update::XdebugFailed(code) => self.session.notices.set_code(NoticeSource::Xdebug, code),
            Update::Mail(list) => self.set_mail(list),
            Update::MailText(text) => self.set_mail_text(text),
            Update::MailFailed(code) => self.mail_failed(code),
            Update::Dumps { list, replace } => self.set_dumps(list, replace),
            Update::DumpsFailed(code) => self.dumps_failed(code),
            Update::XdebugAnswered(answer) => {
                self.session.xdebug_pending = None;
                match answer {
                    Ok(state) => self.session.xdebug = Some(state),
                    // The bridge reads the state again.
                    Err(code) => self.session.notices.set_code(NoticeSource::Xdebug, code),
                }
            }
            Update::Event(event) => apply(self, event),
        }
    }

    /// A notice was closed: the result it told about is cleared, so it does
    /// not come back, and the next result may show.
    pub fn dismiss_notice(&mut self, source: NoticeSource) {
        match source {
            NoticeSource::Install => {
                if let Some(index) = self.install_slot() {
                    self.installs[index].noticed = true;
                }
            }
            source => self.session.notices.clear(source),
        }
    }

    /// Applies what the user did, and says what the window must do beyond
    /// the state: send a command, or ask the system. A queued install goes
    /// out here too, once none of this window runs.
    pub fn apply_action(&mut self, action: Action) -> Effect {
        match action {
            Action::OpenSite(name) => Effect::OpenSite(name),
            Action::OpenFolder(name) => self
                .site_root(&name)
                .map_or(Effect::None, |root| Effect::OpenFolder(root.to_path_buf())),
            Action::OpenDataFolder => Effect::OpenFolder(self.data_dir.clone()),
            Action::PickFolder(pick) => Effect::PickFolder(pick),
            Action::OpenMailpit => Effect::OpenUrl(mailpit_url()),
            Action::OpenMailPage => self.mail_page().map_or(Effect::None, Effect::OpenUrl),
            Action::CopyDump => self
                .session
                .dumps
                .shown()
                .map_or(Effect::None, |dump| Effect::CopyText(dump.text.clone())),
            action => self.change(action).or_else(|| self.next_command()).into(),
        }
    }

    /// The command the window's next queued job needs, if the daemon is free
    /// for it. Called after each action and each message from the daemon, so
    /// a job that waited starts when the one before it ends.
    pub fn next_command(&mut self) -> Option<Command> {
        self.next_install().map(Command::Install)
    }

    /// The folder the user picked in the system's dialog.
    pub fn picked(&mut self, pick: Pick, folder: &Path) -> Effect {
        match pick {
            Pick::Park => self.park_request(folder).map(Command::Park).into(),
            Pick::Link => {
                self.pick_link_folder(folder);
                Effect::None
            }
        }
    }

    /// The window itself failed to do something (the system's opener):
    /// shown as a call that failed.
    pub fn report(&mut self, code: &str) {
        self.session
            .notices
            .set_code(NoticeSource::Call, code.to_owned());
    }

    fn change(&mut self, action: Action) -> Option<Command> {
        match action {
            Action::Navigate(route) => {
                if let Route::Runtimes(runtime) = &route {
                    self.runtime_chosen = Some(*runtime);
                }
                // No event carries mail: the screen reads it as it opens.
                let read = route == Route::Mail;
                // Opening the screen counts every dump as seen, and asks
                // for what came since; leaving it counts the new ones again.
                let dumps = route == Route::Dumps;
                self.session.dumps.set_open(dumps);
                self.route = route;
                if read {
                    self.read_mail()
                } else if dumps && self.is_connected() {
                    Some(Command::ReadDumps)
                } else {
                    None
                }
            }
            Action::FoldSidebar => {
                self.sidebar_folded = !self.sidebar_folded;
                None
            }
            Action::ToggleLogLevel(level) => {
                self.log_hidden = self.log_hidden.toggled(level);
                None
            }
            Action::SetLogFilter(text) => {
                self.log_filter = text;
                None
            }
            Action::SetLogFollow(follow) => {
                self.log_paused = !follow;
                None
            }
            Action::Service { service, action } => {
                // The notice was about the last action.
                self.session.notices.clear(NoticeSource::Call);
                Some(Command::Service { service, action })
            }
            Action::SetDefault { runtime, version } => {
                self.choose_default(runtime, version).map(Command::Save)
            }
            Action::Install(job) => {
                // The job joins the queue; `apply_action` sends it when the daemon is free.
                self.queue_install(job);
                None
            }
            Action::SyncHosts => self.start_hosts_sync().then_some(Command::SyncHosts),
            Action::StartFirstRun => {
                // The jobs join the queue; the first goes out now, the rest as each ends.
                self.start_first_run();
                None
            }
            Action::RunSetup => self.start_setup().then_some(Command::Setup),
            Action::CloseFirstRun => {
                self.close_first_run();
                None
            }
            Action::SearchSites(text) => {
                self.sites_search = text;
                None
            }
            // The site lists are the daemon's: with no daemon, nothing to send.
            Action::SitePhp { .. } | Action::SetSecure { .. } if !self.is_connected() => None,
            Action::SitePhp { site, choice } => {
                self.choose_site_php(&site, choice).map(Command::Save)
            }
            Action::SetSecure { site, secure } => {
                // The notice was about the last action.
                self.session.notices.clear(NoticeSource::Sites);
                Some(Command::Secure { site, secure })
            }
            Action::AskUnlink(site) => {
                self.ask_removal(Removal::Unlink { site });
                None
            }
            Action::AskForget(folder) => {
                self.ask_removal(Removal::Forget { folder });
                None
            }
            Action::SetLinkName(name) => {
                if let Some(form) = self.link.as_mut() {
                    form.name = name;
                }
                None
            }
            Action::SubmitLink => {
                if !self.is_connected() {
                    return None;
                }
                let (name, folder) = self.link_request()?;
                Some(Command::Link { name, folder })
            }
            Action::CancelLink => {
                self.link = None;
                None
            }
            Action::DismissNotice(source) => {
                self.dismiss_notice(source);
                None
            }
            Action::AskInstall(runtime) => {
                self.open_install(runtime);
                None
            }
            Action::SetInstallVersion { runtime, text } => {
                self.set_install_text(runtime, text);
                // The line under the field was about the last text.
                self.session.notices.clear(NoticeSource::Runtimes);
                None
            }
            Action::SubmitInstall(runtime) => {
                // The job joins the queue; `apply_action` sends it when the daemon is free.
                self.request_install(runtime);
                None
            }
            Action::CancelInstall => {
                self.close_install();
                None
            }
            Action::AskUninstall { runtime, version } => {
                self.ask_removal(Removal::Uninstall { runtime, version });
                None
            }
            Action::ToggleXdebug => self.toggle_xdebug().map(Command::SetXdebug),
            Action::ToggleAutostart => self.toggle_autostart().map(Command::SetAutostart),
            Action::ConfirmRemoval => self.confirm_removal().map(|removal| match removal {
                // The bridge's mail reader deletes it, in order with its reads.
                Removal::Mail => Command::ClearMail,
                removal => Command::Remove(removal),
            }),
            Action::ChooseMail(id) => self.choose_mail(id),
            Action::ChooseDump(id) => {
                self.choose_dump(id);
                None
            }
            Action::ClearDumps => self.clear_dumps(),
            Action::ToggleDumps => self.toggle_capture(),
            Action::AskDeleteMail => {
                self.ask_removal(Removal::Mail);
                None
            }
            Action::CancelRemoval => {
                self.cancel_removal();
                None
            }
            Action::SetAppearance(appearance) => {
                // `App` sees the change after the frame: it sets egui's
                // theme and writes `gui.toml`.
                self.appearance = appearance;
                None
            }
            // Done by `apply_action`: they only ask the system.
            Action::OpenSite(_)
            | Action::OpenFolder(_)
            | Action::OpenDataFolder
            | Action::PickFolder(_)
            | Action::OpenMailpit
            | Action::OpenMailPage
            | Action::CopyDump => None,
        }
    }
}

/// Applies one daemon event.
pub fn apply(model: &mut Model, event: Event) {
    match event {
        Event::ServiceChanged(status) => {
            // A new run of a service may get a new log buffer whose `seq`
            // starts again at 0, so the old `seq` must not hide it.
            if status.state == ServiceState::Starting
                && let Some(log) = model.session.logs.get_mut(&status.id)
            {
                log.last_seq = None;
            }
            match model
                .session
                .services
                .binary_search_by(|s| s.id.cmp(&status.id))
            {
                Ok(index) => model.session.services[index] = status,
                Err(index) => model.session.services.insert(index, status),
            }
        }
        Event::Log(line) => model.push_log(line),
        Event::Install(progress) => model.install_event(progress),
        // The whole new list: no need to ask the daemon again.
        Event::SitesChanged(list) => model.set_sites(list),
        Event::DefaultChanged(default) => {
            let row = model
                .session
                .runtimes
                .iter_mut()
                .find(|r| r.runtime == default.runtime);
            if let Some(row) = row {
                row.default = Some(default.default);
            }
        }
        // From this window or from `marsa setup`: the same report.
        Event::SetupFinished(report) => model.session.setup = Setup::Finished(report),
        // The bridge follows the jobs it starts (`client::run_setup`); the
        // model learns each end from `setup_finished` and the like.
        Event::Job(_) => {}
        // The Dumps screen asks for the new dumps (a later change).
        Event::DumpsChanged { .. } => {}
        Event::ShuttingDown => model.disconnect(Connection::ShuttingDown),
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
mod tests;
