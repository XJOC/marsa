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
    DoctorReport, Event, Health, HostsStatus, InstallEvent, InstallFailure, InstallProgress,
    InstallUpdate, LogLine, PoolKind, RuntimeList, ServiceAction, ServiceId, ServiceState,
    ServiceStatus, SetupReport, Site, SiteList, Source, VersionInUse, XdebugState,
};
use semver::Version;

mod installs;
mod logs;
mod notices;
mod removals;
mod runtimes;
mod session;
mod setup;
mod sites;

use installs::Install;
pub use installs::{InstallJob, InstallStep, InstallView, install_version};
pub use logs::{LogRow, LogSource, MAX_LOG_LINES, ServiceLog};
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
    /// Recent lines of the services' logs (each line names its service),
    /// oldest first.
    Log(Vec<LogLine>),
    /// A call failed but the connection is fine. A client error code.
    Notice(String),
    /// The whole site list, read when a session starts and again after the
    /// daemon dropped events.
    Sites(SiteList),
    /// The daemon accepted this folder to park. The new list comes as
    /// `sites_changed`.
    Parked(PathBuf),
    /// The daemon linked `folder` as the site `name`. The new list comes as
    /// `sites_changed`.
    Linked {
        name: String,
        folder: PathBuf,
    },
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
    /// The daemon accepted `job`: its `install` events carry `id`.
    InstallAccepted {
        job: InstallJob,
        id: u64,
    },
    /// The daemon refused `job` (`install_busy`), or could not be reached. A
    /// client error code.
    InstallRefused {
        job: InstallJob,
        code: String,
    },
    /// The answer to "Run setup".
    SetupDone(SetupReport),
    /// "Run setup" ended before the helper ran (`setup_running`,
    /// `setup_not_supported`), or the daemon could not be reached. A client
    /// error code.
    SetupFailed(String),
    /// Whether the menu-bar icon starts at login, as the daemon says.
    Autostart(Autostart),
    /// Reading or changing it failed. A client error code.
    AutostartFailed(String),
    /// The daemon removed this. After an uninstall the bridge reads the
    /// runtimes again: no event carries them.
    Removed(Removal),
    /// The daemon refused or failed the removal, or could not be reached. A
    /// client error code; `in_use` holds the facts of `version_in_use`.
    NotRemoved {
        removal: Removal,
        code: String,
        in_use: Option<VersionInUse>,
    },
    /// The daemon's checks, as read (no event carries them).
    Doctor(DoctorReport),
    /// Whether PHP loads Xdebug, as read (no event carries it).
    Xdebug(XdebugState),
    /// Reading it failed. A client error code.
    XdebugFailed(String),
    /// The answer to the Xdebug box: the state after the change, or a client
    /// error code.
    XdebugAnswered(Result<XdebugState, String>),
    /// Linux: whether the hosts file holds this data
    /// folder's names, and the sites it lacks. Read when a session starts,
    /// after each site list, after setup, and after "Add to /etc/hosts".
    Hosts(HostsStatus),
    /// The answer to "Add to /etc/hosts".
    HostsSynced(SetupReport),
    /// "Add to /etc/hosts" did not run, or the daemon could not be reached.
    /// A client error code.
    HostsSyncFailed(String),
    Event(Event),
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
    Settings,
}

/// What the user did in the window. The shell and the screens return it,
/// and [`Model::apply_action`] turns it into state, and into a bridge
/// [`Command`] when the daemon must act (code.md, State). [Laravel: a form
/// posted to a controller.]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// A sidebar item, or the status at its bottom.
    Navigate(Route),
    /// The fold button of the sidebar: hide or show the labels.
    FoldSidebar,
    /// A service's Start, Stop, or Restart.
    Service {
        service: ServiceId,
        action: ServiceAction,
    },
    /// A level filter of the Logs screen: show or hide its lines.
    ToggleLogLevel(LogLevel),
    /// The text of the filter field of the Logs screen.
    SetLogFilter(String),
    /// The Follow button of the Logs screen, or a scroll away from the end
    /// of the log: keep the newest line in view, or stop.
    SetLogFollow(bool),
    /// A version button of the default picker (Home): save `version` as the
    /// default of `runtime`.
    SetDefault { runtime: Runtime, version: Version },
    /// A fix that installs a program (Home: "Install Caddy"). Queued like a
    /// press of Install; `Model::apply_action` sends it.
    Install(InstallJob),
    /// The fix "Add to /etc/hosts" (Linux).
    SyncHosts,
    /// "Open" on a site. A local action: `App` hands the address
    /// ([`Model::site_url`]) to the system, never a screen (code.md, State).
    OpenSite(String),
    /// "Open folder" on a site's page: the system's file manager shows the
    /// site's folder. A local action like [`Action::OpenSite`]: `App` finds
    /// the folder ([`Model::site_root`]) and starts the system's opener.
    OpenFolder(String),
    /// The first-run button that installs what is missing.
    StartFirstRun,
    /// "Run setup".
    RunSetup,
    /// "Park a folder…" or "Link a folder…". A local action: `App` opens the
    /// system's folder dialog, and takes the folder it gives back.
    PickFolder(Pick),
    /// "Not now": Home takes the place of the first-run panel.
    CloseFirstRun,
    /// The text of the search field of the Sites screen.
    SearchSites(String),
    /// An entry of a site's version menu: save `choice` as its PHP, or clear
    /// its own setting ([`Choice::Auto`]).
    SitePhp { site: String, choice: Choice },
    /// The site's menu: serve it over HTTPS (`true`) or plain HTTP.
    SetSecure { site: String, secure: bool },
    /// "Unlink…" of a site: the confirmation opens.
    AskUnlink(String),
    /// "Forget…" of a parked folder: the confirmation opens.
    AskForget(PathBuf),
    /// The site name typed in the link form.
    SetLinkName(String),
    /// "Link" in the link form (or Enter): sends the link if the name is
    /// valid.
    SubmitLink,
    /// "Cancel" in the link form.
    CancelLink,
    /// The close button of a notice, or its 4 seconds running out: the
    /// result it told about goes.
    DismissNotice(NoticeSource),
    /// "Install a version…" (or the dashed "+") of a runtime: its install
    /// dialog opens.
    AskInstall(Runtime),
    /// The text typed in the install dialog's version field.
    SetInstallVersion { runtime: Runtime, text: String },
    /// "Install" in the dialog (or Enter): queues the version in the field,
    /// or says why it is wrong. `App` sends the job.
    SubmitInstall(Runtime),
    /// "Cancel" or Escape in the install dialog.
    CancelInstall,
    /// "Uninstall…" in a version's row menu: the confirmation opens.
    AskUninstall { runtime: Runtime, version: Version },
    /// The Xdebug switch.
    ToggleXdebug,
    /// The confirmation's action ("Unlink", "Forget", "Uninstall"): sends the
    /// removal it asked about.
    ConfirmRemoval,
    /// "Cancel" or Escape in the confirmation.
    CancelRemoval,
    /// The Start at login switch in Settings.
    ToggleAutostart,
    /// "Open folder" on the data folder in Settings. A local action like
    /// [`Action::OpenFolder`]: `App` shows [`Model::data_dir`] in the system's
    /// file manager.
    OpenDataFolder,
    /// An option of the Appearance control in Settings. `App` applies it at
    /// once and writes it to `gui.toml`.
    SetAppearance(Appearance),
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
                self.route = route;
                None
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
            Action::ConfirmRemoval => self.confirm_removal().map(Command::Remove),
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
            | Action::PickFolder(_) => None,
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
