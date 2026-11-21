//! The window's state, and how each message from the bridge changes it.
//!
//! Pure: no drawing and no I/O, so the tests run without a window. The
//! drawing code in `shell` and `screens` reads a [`Model`], and returns an
//! [`Action`] when the user presses something.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use marsa_core::helper::OpStatus;
use marsa_core::resolver;
use marsa_core::runtimes::Runtime;
use marsa_core::types::{
    Event, Health, HostsStatus, InstallEvent, InstallFailure, InstallProgress, InstallUpdate,
    LogLine, RuntimeList, ServiceAction, ServiceState, ServiceStatus, SetupReport, Site, SiteList,
    Source, VersionInUse, XdebugState,
};
use semver::Version;

mod logs;
mod runtimes;
mod session;
mod sites;

pub use logs::{LOG_SERVICE, LogRow, LogSource, MAX_LOG_LINES, ServiceLog};
pub use runtimes::{
    Change, Choice, MAIN_RUNTIME, PoolKind, RuntimeRow, has_default, is_pool, pool_of,
    runtime_missing, version_text,
};
pub use session::{Connection, Overall, Session};
use sites::SITE_IS_PARKED;
pub use sites::{LinkForm, Pick, SitesNotice, folder_path, url};

use crate::bridge::Command;
pub use crate::logline::{HiddenLevels, LogLevel};
pub use crate::prefs::Appearance;
pub use crate::tray::menu::Autostart;

/// The service that serves every site: no site opens while it is down.
pub const WEB_SERVER: &str = "caddy";
/// The daemon's DNS server for `.test` names, on macOS and Linux.
pub const DNS_SERVER: &str = "dns";
/// This build's version, shown in the Settings section.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// The PHP line the first-run panel installs.
pub const FIRST_PHP: &str = "8.4";
/// The most installs kept for the Runtimes section. The daemon runs one at a
/// time, so a few are plenty.
const MAX_INSTALLS: usize = 4;
/// The window's own code: the daemon stopped before an install ended, as
/// `marsa install` says it.
pub const INSTALL_INTERRUPTED: &str = "install_interrupted";
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

/// The "Add to /etc/hosts" button (Linux).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum HostsSync {
    #[default]
    Idle,
    /// Sent; the daemon may wait up to 5 minutes for the password prompt.
    Running,
    /// The names did not go in: the code of why.
    Failed(String),
}

/// Which of the model's results a notice in the bar tells about. Closing the
/// notice clears that result ([`Model::dismiss_notice`]), so the bar shows the
/// next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoticeSource {
    /// A call to the daemon failed ([`Model::notice`]).
    Call,
    /// The last sites action ([`Model::sites_notice`]).
    Sites,
    /// How the install shown ended ([`Model::install_result`]).
    Install,
    /// The last uninstall ([`Model::uninstall_notice`]).
    Uninstall,
    /// The runtimes could not be read, or a default not saved
    /// ([`Model::runtimes_notice`]).
    Runtimes,
    /// Start at login failed ([`Model::settings_notice`]).
    Autostart,
}

/// Something the window removes once the user confirms it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// `DELETE /v1/runtimes/{tool}/{version}`, with the full version.
    Uninstall { runtime: Runtime, version: Version },
    /// `DELETE /v1/sites/{name}`: a linked site.
    Unlink { site: String },
    /// `DELETE /v1/sites/park`: a parked folder, as the list names it.
    Forget { folder: PathBuf },
}

/// The lines under the Runtimes table about the last uninstall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UninstallNotice {
    /// The daemon deleted this version.
    Done { runtime: Runtime, version: Version },
    /// The daemon refused: the default, a site, or Caddy uses the version.
    InUse {
        runtime: Runtime,
        in_use: VersionInUse,
    },
    /// The daemon refused or failed for another reason. A client error code.
    Failed {
        runtime: Runtime,
        version: Version,
        code: String,
    },
}

/// One install the window asks the daemon for:
/// `POST /v1/runtimes/{tool}/install`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallJob {
    pub runtime: Runtime,
    /// `8.4`, with the rules of a `.php-version` file. `None`: the newest
    /// stable version.
    pub version: Option<String>,
}

/// What the Runtimes section shows of one install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallView {
    /// From the window's job, or from the daemon's `selected` step for an
    /// install started elsewhere (`marsa install` in a terminal).
    pub runtime: Option<Runtime>,
    /// What was asked for (`8.4`), then the version the daemon picked
    /// (`8.4.23`).
    pub version: Option<String>,
    pub step: InstallStep,
}

/// How far an install is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStep {
    /// Sent; the daemon has reported no step yet.
    Waiting,
    /// The signed version list was checked; a version is being picked.
    ListChecked,
    /// `done` of `total` bytes of the file `artifact`.
    Downloading {
        artifact: String,
        done: u64,
        total: u64,
    },
    /// The file passed its checksum and is being unpacked.
    Unpacking { artifact: String },
    /// The new program runs its own check (`php -v`).
    Checking,
    /// The version folder is in place; the daemon sets the default and
    /// the sites.
    Finishing,
    /// `already`: the version was there, and nothing was downloaded.
    Done { already: bool },
    /// The daemon's code and the facts its text may show.
    Failed(InstallFailure),
}

impl InstallStep {
    /// Whether the install has ended, well or not.
    pub fn is_over(&self) -> bool {
        matches!(self, InstallStep::Done { .. } | InstallStep::Failed(_))
    }

    /// The value of the progress bar, from 0 to 1: the share of the current
    /// file that has arrived, and a full bar once it is all there.
    pub fn fraction(&self) -> f32 {
        match self {
            InstallStep::Downloading { done, total, .. } if *total > 0 => {
                // Display only: `f64` holds any file size closely enough.
                (*done as f64 / *total as f64).clamp(0.0, 1.0) as f32
            }
            InstallStep::Unpacking { .. }
            | InstallStep::Checking
            | InstallStep::Finishing
            | InstallStep::Done { .. } => 1.0,
            InstallStep::Waiting
            | InstallStep::ListChecked
            | InstallStep::Downloading { .. }
            | InstallStep::Failed(_) => 0.0,
        }
    }
}

impl InstallView {
    fn waiting(job: &InstallJob) -> Self {
        Self {
            runtime: Some(job.runtime),
            version: job.version.clone(),
            step: InstallStep::Waiting,
        }
    }

    /// One `install` event of this install.
    fn apply(&mut self, update: InstallUpdate) {
        self.step = match update {
            InstallUpdate::Running(event) => match event {
                InstallEvent::ManifestVerified { .. } => InstallStep::ListChecked,
                InstallEvent::Selected { runtime, version } => {
                    self.runtime = Some(runtime);
                    self.version = Some(version);
                    return;
                }
                InstallEvent::Downloading {
                    artifact,
                    done,
                    total,
                } => InstallStep::Downloading {
                    artifact,
                    done,
                    total,
                },
                InstallEvent::Verified { artifact } | InstallEvent::Extracting { artifact } => {
                    InstallStep::Unpacking { artifact }
                }
                InstallEvent::Checking => InstallStep::Checking,
                InstallEvent::Installed { .. } => InstallStep::Finishing,
            },
            InstallUpdate::Done(done) => {
                self.runtime = Some(done.runtime);
                self.version = Some(done.version);
                InstallStep::Done {
                    already: done.already,
                }
            }
            InstallUpdate::Failed(failure) => InstallStep::Failed(failure),
        };
    }
}

/// One install the daemon reported, or one the window asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Install {
    /// The daemon's id. `None` until the daemon accepts the window's job, and
    /// once its session is gone: a new daemon counts from 1 again.
    id: Option<u64>,
    /// Started by this window: its end starts the next job of the queue.
    ours: bool,
    view: InstallView,
    /// Its result was shown in a notice that has since closed.
    noticed: bool,
}

impl Install {
    fn runs_for_us(&self) -> bool {
        self.ours && !self.view.step.is_over()
    }
}

/// The first-run panel: shown while neither PHP nor Caddy is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstRun {
    /// The one button: "Install PHP 8.4 and Caddy".
    Offer,
    /// The two installs run, one after the other.
    Installing,
    /// Both are installed: "Run setup" is the last step.
    Setup,
}

/// "Run setup" (`POST /v1/setup`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Setup {
    #[default]
    Idle,
    /// Sent; the daemon may wait up to 5 minutes for the password prompt.
    Running,
    /// The daemon's report, from this window or from `marsa setup`.
    Finished(SetupReport),
    /// It did not run. A client error code.
    Failed(String),
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
        service: String,
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
    /// press of Install; `App` sends it.
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
    /// The version typed in the install dialog of each runtime, by the
    /// runtime's folder name (`Runtime` is neither `Hash` nor `Ord`, and the
    /// folder name is its stable key). A runtime with no entry has an empty
    /// field: the newest stable version. A map, not an array in
    /// [`Runtime::ALL`] order, so a runtime that core adds needs nothing
    /// here. Kept across sessions.
    install_input: BTreeMap<&'static str, String>,
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
                services.sort_by(|a, b| a.id.cmp(&b.id));
                // The log of a service the daemon no longer lists goes with
                // it: a log never outlives its source.
                self.session
                    .logs
                    .retain(|id, _| services.iter().any(|service| service.id == *id));
                self.session.services = services;
            }
            Update::Log(lines) => lines.into_iter().for_each(|line| self.push_log(line)),
            Update::Notice(code) => self.session.notice = Some(code),
            Update::Sites(list) => self.set_sites(list),
            Update::Parked(path) => self.session.sites_notice = Some(SitesNotice::Parked(path)),
            Update::Linked { name, folder } => {
                // The user may have picked another folder since.
                let same = self
                    .link
                    .as_ref()
                    .is_some_and(|form| form.folder == folder && form.name.trim() == name);
                if same {
                    self.link = None;
                }
                self.session.sites_notice = Some(SitesNotice::Linked { name, folder });
            }
            Update::SitesFailed(code) => {
                self.session.sites_notice = Some(SitesNotice::Failed(code))
            }
            Update::Runtimes(list) => self.set_runtimes(list),
            Update::RuntimesFailed(code) => self.session.runtimes_notice = Some(code),
            Update::Saved(change) => self.settle(&change),
            Update::NotSaved { change, code } => {
                self.settle(&change);
                match change {
                    Change::Default { .. } => self.session.runtimes_notice = Some(code),
                    Change::SitePhp { .. } => {
                        self.session.sites_notice = Some(SitesNotice::Failed(code))
                    }
                }
            }
            Update::InstallAccepted { job, id } => self.accepted(&job, id),
            Update::InstallRefused { job, code } => self.refused(&job, code),
            Update::SetupDone(report) => self.session.setup = Setup::Finished(report),
            Update::Hosts(status) => self.session.hosts = status,
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
                self.session.settings_notice = Some(code);
            }
            Update::Removed(removal) => self.removed(removal),
            Update::NotRemoved {
                removal,
                code,
                in_use,
            } => self.not_removed(removal, code, in_use),
            Update::Xdebug(state) => self.session.xdebug = Some(state),
            Update::XdebugFailed(code) => self.session.xdebug_notice = Some(code),
            Update::XdebugAnswered(answer) => {
                self.session.xdebug_pending = None;
                match answer {
                    Ok(state) => self.session.xdebug = Some(state),
                    // The bridge reads the state again.
                    Err(code) => self.session.xdebug_notice = Some(code),
                }
            }
            Update::Event(event) => apply(self, event),
        }
    }

    // -- Runtimes: installs --

    /// The install the Runtimes section shows: one this window started that
    /// has not ended, else the newest one, whoever started it.
    pub fn install_view(&self) -> Option<&InstallView> {
        self.install_slot().map(|index| &self.installs[index].view)
    }

    /// Where [`Model::install_view`] is in the list.
    fn install_slot(&self) -> Option<usize> {
        self.installs
            .iter()
            .rposition(Install::runs_for_us)
            .or_else(|| self.installs.len().checked_sub(1))
    }

    /// How the install shown ended, until the notice about it is closed. The
    /// bar says it once; Home and the first-run panel keep their own words
    /// about a failure beside the button that fixes it.
    pub fn install_result(&self) -> Option<&InstallView> {
        let install = &self.installs[self.install_slot()?];
        (install.view.step.is_over() && !install.noticed).then_some(&install.view)
    }

    /// Whether a job of this window waits or runs. The install buttons wait
    /// for it: the daemon runs one install at a time.
    pub fn installing(&self) -> bool {
        !self.session.queue.is_empty() || self.installs.iter().any(Install::runs_for_us)
    }

    /// The user pressed Install for `runtime`: queues the version in its
    /// field, or says why the field is wrong. `false` if nothing was queued.
    pub fn request_install(&mut self, runtime: Runtime) -> bool {
        if !self.is_connected() || self.installing() {
            return false;
        }
        match install_version(self.install_text(runtime)) {
            Ok(version) => {
                let queued = self.queue_install(InstallJob { runtime, version });
                if queued {
                    // The text is used, and the dialog has done its work.
                    self.install_input.remove(runtime.dir_name());
                    self.session.install_dialog = None;
                }
                queued
            }
            Err(code) => {
                self.session.runtimes_notice = Some(code.to_owned());
                false
            }
        }
    }

    /// What the version field of `runtime`'s install dialog holds. Empty: the
    /// newest stable version.
    pub fn install_text(&self, runtime: Runtime) -> &str {
        self.install_input
            .get(runtime.dir_name())
            .map_or("", String::as_str)
    }

    /// The user typed `text` in the version field of `runtime`.
    pub fn set_install_text(&mut self, runtime: Runtime, text: String) {
        if text.is_empty() {
            self.install_input.remove(runtime.dir_name());
        } else {
            self.install_input.insert(runtime.dir_name(), text);
        }
    }

    /// Whether an install can be asked for now: connected, and no install or
    /// uninstall of this window waits or runs (the daemon runs one at a time).
    pub fn can_install(&self) -> bool {
        self.is_connected() && !self.installing() && !self.uninstalling()
    }

    /// "Install a version…": opens the install dialog of `runtime`. `false`
    /// when an install cannot be asked for now.
    pub fn open_install(&mut self, runtime: Runtime) -> bool {
        if !self.can_install() {
            return false;
        }
        // The notice was about the last action.
        self.session.runtimes_notice = None;
        self.session.install_dialog = Some(runtime);
        true
    }

    /// "Cancel" or Escape in the install dialog. The typed text stays for the
    /// next time.
    pub fn close_install(&mut self) {
        self.session.install_dialog = None;
        self.session.runtimes_notice = None;
    }

    /// Queues `job`, as the Install button does once its field is right.
    /// `false` if the window has no daemon, or one of its jobs waits or runs.
    pub fn queue_install(&mut self, job: InstallJob) -> bool {
        if !self.is_connected() || self.installing() {
            return false;
        }
        self.session.runtimes_notice = None;
        self.session.queue.push_back(job);
        true
    }

    /// The next job to send, once no job of this window runs. It shows as
    /// waiting until the daemon answers. The window calls this after each
    /// change, so one call sends at most one job.
    pub fn next_install(&mut self) -> Option<InstallJob> {
        // The daemon runs one install or uninstall at a time.
        if !self.is_connected()
            || self.uninstalling()
            || self.installs.iter().any(Install::runs_for_us)
        {
            return None;
        }
        let job = self.session.queue.pop_front()?;
        self.push_install(Install {
            id: None,
            ours: true,
            view: InstallView::waiting(&job),
            noticed: false,
        });
        Some(job)
    }

    fn push_install(&mut self, install: Install) {
        if self.installs.len() >= MAX_INSTALLS {
            let oldest_over = self
                .installs
                .iter()
                .position(|install| install.view.step.is_over());
            self.installs.remove(oldest_over.unwrap_or(0));
        }
        self.installs.push(install);
    }

    /// The window's job that waits for the daemon's answer.
    fn unanswered(&self, job: &InstallJob) -> Option<usize> {
        self.installs.iter().position(|install| {
            install.runs_for_us()
                && install.id.is_none()
                && install.view.runtime == Some(job.runtime)
        })
    }

    /// The daemon accepted `job` as install `id`.
    fn accepted(&mut self, job: &InstallJob, id: u64) {
        let Some(index) = self.unanswered(job) else {
            // From a session that has ended.
            return;
        };
        let Some(early) = self.installs.iter().position(|i| i.id == Some(id)) else {
            self.installs[index].id = Some(id);
            return;
        };
        // Its events came before the answer, as an install of no known
        // owner: they are this job's.
        let mut install = self.installs.remove(early);
        let index = if early < index { index - 1 } else { index };
        install.ours = true;
        install.view.runtime.get_or_insert(job.runtime);
        if install.view.version.is_none() {
            install.view.version.clone_from(&job.version);
        }
        let ended = match &install.view.step {
            InstallStep::Done { .. } => Some(true),
            InstallStep::Failed(_) => Some(false),
            _ => None,
        };
        self.installs[index] = install;
        if let Some(success) = ended {
            self.our_install_ended(success);
        }
    }

    /// The daemon refused `job`, or could not be reached.
    fn refused(&mut self, job: &InstallJob, code: String) {
        let Some(index) = self.unanswered(job) else {
            return;
        };
        self.installs[index].view.step = InstallStep::Failed(failure(code));
        self.our_install_ended(false);
    }

    /// One `install` event, of any install: the window shows those started
    /// in a terminal too.
    fn install_event(&mut self, progress: InstallProgress) {
        let index = match self.installs.iter().position(|i| i.id == Some(progress.id)) {
            Some(index) => index,
            None => {
                self.push_install(Install {
                    id: Some(progress.id),
                    ours: false,
                    view: InstallView {
                        runtime: None,
                        version: None,
                        step: InstallStep::Waiting,
                    },
                    noticed: false,
                });
                self.installs.len() - 1
            }
        };
        let install = &mut self.installs[index];
        let ran = install.runs_for_us();
        install.view.apply(progress.update);
        if ran && install.view.step.is_over() {
            let success = matches!(install.view.step, InstallStep::Done { .. });
            self.our_install_ended(success);
        }
    }

    /// A job of this window ended. A failure drops the jobs after it: the
    /// user sees why, and starts again.
    fn our_install_ended(&mut self, success: bool) {
        if !success {
            self.session.queue.clear();
        }
        if self.session.first_run == Some(FirstRun::Installing) && self.session.queue.is_empty() {
            self.session.first_run = if success {
                Some(FirstRun::Setup)
            } else {
                // The new runtimes list closes it if PHP or Caddy came.
                first_run_needed(&self.session.runtimes).then_some(FirstRun::Offer)
            };
        }
    }

    /// No event comes from a lost session: an install that runs shows as
    /// interrupted, and no id is kept, since a new daemon counts from 1
    /// again. Only the install shown stays.
    fn forget_installs(&mut self) {
        self.session.queue.clear();
        let shown = self
            .installs
            .iter()
            .rposition(Install::runs_for_us)
            .or_else(|| self.installs.len().checked_sub(1));
        self.installs = shown
            .map(|index| self.installs.swap_remove(index))
            .into_iter()
            .collect();
        for install in &mut self.installs {
            if !install.view.step.is_over() {
                install.view.step = InstallStep::Failed(failure(INSTALL_INTERRUPTED.to_owned()));
            }
            install.id = None;
            install.ours = false;
        }
    }

    // -- First run, setup, and settings --

    /// "Install PHP 8.4 and Caddy": the jobs of what is missing, PHP first.
    /// `false` if the panel does not offer it now.
    pub fn start_first_run(&mut self) -> bool {
        let offered = first_run_needed(&self.session.runtimes) && !self.first_run_closed;
        if !offered || !self.is_connected() || self.installing() {
            return false;
        }
        self.session.runtimes_notice = None;
        let jobs = [
            (Runtime::Php, Some(FIRST_PHP.to_owned())),
            (Runtime::Caddy, None),
        ];
        let missing = jobs
            .into_iter()
            .filter(|(runtime, _)| runtime_missing(&self.session.runtimes, *runtime));
        self.session
            .queue
            .extend(missing.map(|(runtime, version)| InstallJob { runtime, version }));
        self.session.first_run = Some(FirstRun::Installing);
        true
    }

    /// Whether Home gives way to the first-run panel (principles 2): the
    /// daemon answers, the user did not say "Not now", and the first site
    /// cannot open yet. That is when PHP or Caddy is not installed, or when
    /// there is no site. The window cannot read whether setup ran in an
    /// earlier session, so the sites decide: the first site ends it.
    pub fn needs_first_run(&self) -> bool {
        self.is_connected()
            && !self.first_run_closed
            && !self.session.runtimes.is_empty()
            && (first_run_needed(&self.session.runtimes) || self.session.sites.sites.is_empty())
    }

    /// Whether the first-run panel is what the main area shows: Home, while
    /// [`Model::needs_first_run`].
    pub fn first_run_shown(&self) -> bool {
        self.route == Route::Home && self.needs_first_run()
    }

    /// "Not now" or "Close" in the first-run panel.
    pub fn close_first_run(&mut self) {
        self.session.first_run = None;
        self.first_run_closed = true;
    }

    /// "Add to /etc/hosts". `false` while it or a setup runs, with no
    /// daemon, or with nothing to add.
    pub fn start_hosts_sync(&mut self) -> bool {
        let nothing = !self.session.hosts.used || self.session.hosts.missing.is_empty();
        let busy =
            self.session.hosts_sync == HostsSync::Running || self.session.setup == Setup::Running;
        if !self.is_connected() || nothing || busy {
            return false;
        }
        self.session.hosts_sync = HostsSync::Running;
        true
    }

    /// "Run setup". `false` while one runs, or with no daemon.
    pub fn start_setup(&mut self) -> bool {
        if !self.is_connected() || self.session.setup == Setup::Running {
            return false;
        }
        self.session.setup = Setup::Running;
        true
    }

    /// The "Start at login" box was clicked. Returns the state to ask for,
    /// or `None` while the state is unknown or not supported. The box shows
    /// the new state until the daemon answers, as in the menu-bar icon.
    pub fn toggle_autostart(&mut self) -> Option<bool> {
        if !self.is_connected() {
            return None;
        }
        let enabled = match self.session.autostart {
            Autostart::On => false,
            Autostart::Off | Autostart::Stale => true,
            Autostart::Unknown | Autostart::Unsupported => return None,
        };
        self.session.autostart = if enabled {
            Autostart::On
        } else {
            Autostart::Off
        };
        self.session.settings_notice = None;
        Some(enabled)
    }

    // -- Removals: uninstall, unlink, and forget, after a confirmation --

    /// Whether a removal can be asked for now: connected, and no other
    /// removal waits for its answer.
    pub fn can_remove(&self) -> bool {
        self.is_connected() && self.session.removing.is_none()
    }

    /// [`Model::can_remove`], and no job of this window installs: the daemon
    /// runs one install or uninstall at a time.
    pub fn can_uninstall(&self) -> bool {
        self.can_remove() && !self.installing()
    }

    /// The removal sent and not answered yet.
    pub fn removing(&self) -> Option<&Removal> {
        self.session.removing.as_ref()
    }

    /// Whether an uninstall waits for its answer. The install buttons wait
    /// for it.
    pub fn uninstalling(&self) -> bool {
        matches!(self.session.removing, Some(Removal::Uninstall { .. }))
    }

    fn may_remove(&self, removal: &Removal) -> bool {
        match removal {
            Removal::Uninstall { .. } => self.can_uninstall(),
            Removal::Unlink { .. } | Removal::Forget { .. } => self.can_remove(),
        }
    }

    /// The user pressed Uninstall, Unlink, or Forget: the confirmation
    /// opens. `false` when it cannot be removed now.
    pub fn ask_removal(&mut self, removal: Removal) -> bool {
        if !self.may_remove(&removal) {
            return false;
        }
        self.session.confirm = Some(removal);
        true
    }

    /// "Cancel", Escape, or a click beside the confirmation.
    pub fn cancel_removal(&mut self) {
        self.session.confirm = None;
    }

    /// The user confirmed: the removal to send, or `None` if it cannot be
    /// sent now. The last notice of its section goes: it was about the last
    /// action.
    pub fn confirm_removal(&mut self) -> Option<Removal> {
        let removal = self.session.confirm.take()?;
        if !self.may_remove(&removal) {
            return None;
        }
        match removal {
            Removal::Uninstall { .. } => {
                self.session.uninstall_notice = None;
                self.session.runtimes_notice = None;
            }
            Removal::Unlink { .. } | Removal::Forget { .. } => self.session.sites_notice = None,
        }
        self.session.removing = Some(removal.clone());
        Some(removal)
    }

    /// Whether `removal` is the one waiting for its answer, which is then no
    /// longer waited for. An answer from a lost session finds none.
    fn settle_removal(&mut self, removal: &Removal) -> bool {
        let ours = self.session.removing.as_ref() == Some(removal);
        if ours {
            self.session.removing = None;
        }
        ours
    }

    fn removed(&mut self, removal: Removal) {
        if !self.settle_removal(&removal) {
            return;
        }
        match removal {
            Removal::Uninstall { runtime, version } => {
                // The bridge reads the list again; until it comes, the
                // version is gone from the table already. The last version
                // took the default that picked it, as in the daemon.
                if let Some(row) = self
                    .session
                    .runtimes
                    .iter_mut()
                    .find(|r| r.runtime == runtime)
                {
                    let picks = |text: &str| resolver::best_match(text, &row.installed);
                    let picked = row.default.as_deref().and_then(|t| picks(t).ok().flatten());
                    row.installed.retain(|installed| *installed != version);
                    if row.installed.is_empty() && picked.as_ref() == Some(&version) {
                        row.default = None;
                    }
                }
                self.session.uninstall_notice = Some(UninstallNotice::Done { runtime, version });
            }
            // The new list comes as `sites_changed`.
            Removal::Unlink { site } => {
                // The page of the site is gone: back to the list, where the
                // line about it shows.
                if self.route == Route::Site(site.clone()) {
                    self.route = Route::Sites;
                }
                self.session.sites_notice = Some(SitesNotice::Unlinked(site));
            }
            Removal::Forget { folder } => {
                self.session.sites_notice = Some(SitesNotice::Forgot(folder))
            }
        }
    }

    fn not_removed(&mut self, removal: Removal, code: String, in_use: Option<VersionInUse>) {
        if !self.settle_removal(&removal) {
            return;
        }
        match removal {
            Removal::Uninstall { runtime, version } => {
                self.session.uninstall_notice = Some(match in_use {
                    Some(in_use) => UninstallNotice::InUse { runtime, in_use },
                    None => UninstallNotice::Failed {
                        runtime,
                        version,
                        code,
                    },
                });
            }
            Removal::Unlink { site } if code == SITE_IS_PARKED => {
                let folder = self.holder(&site).map(Path::to_path_buf);
                self.session.sites_notice = Some(SitesNotice::SiteIsParked { site, folder });
            }
            Removal::Unlink { .. } | Removal::Forget { .. } => {
                self.session.sites_notice = Some(SitesNotice::Failed(code));
            }
        }
    }

    // -- Xdebug --

    /// Whether the Xdebug box shows a tick: the state asked for until the
    /// daemon answers, else the daemon's. `None` until it is read.
    pub fn xdebug_on(&self) -> Option<bool> {
        self.session
            .xdebug_pending
            .or_else(|| self.session.xdebug.as_ref().map(|state| state.enabled))
    }

    /// The state the box asked for, while the daemon writes every `php.ini`
    /// and starts the pools again.
    pub fn xdebug_pending(&self) -> Option<bool> {
        self.session.xdebug_pending
    }

    /// The Xdebug box was clicked. Returns the state to ask for, or `None`
    /// while the state is unknown or a change runs. The box shows the new
    /// state until the daemon answers.
    pub fn toggle_xdebug(&mut self) -> Option<bool> {
        if !self.is_connected() || self.session.xdebug_pending.is_some() {
            return None;
        }
        let enabled = !self.session.xdebug.as_ref()?.enabled;
        self.session.xdebug_pending = Some(enabled);
        self.session.xdebug_notice = None;
        Some(enabled)
    }

    /// A notice was closed: the result it told about is cleared, so it does
    /// not come back, and the next result may show.
    pub fn dismiss_notice(&mut self, source: NoticeSource) {
        match source {
            NoticeSource::Call => self.session.notice = None,
            NoticeSource::Sites => self.session.sites_notice = None,
            NoticeSource::Install => {
                if let Some(index) = self.install_slot() {
                    self.installs[index].noticed = true;
                }
            }
            NoticeSource::Uninstall => self.session.uninstall_notice = None,
            NoticeSource::Runtimes => self.session.runtimes_notice = None,
            NoticeSource::Autostart => self.session.settings_notice = None,
        }
    }

    /// Applies what the user did. Returns the command to send to the
    /// daemon, if any.
    pub fn apply_action(&mut self, action: Action) -> Option<Command> {
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
                self.session.notice = None;
                Some(Command::Service { service, action })
            }
            Action::SetDefault { runtime, version } => {
                self.choose_default(runtime, version).map(Command::Save)
            }
            Action::Install(job) => {
                // The job waits in the queue: `App` sends it (`next_install`).
                self.queue_install(job);
                None
            }
            Action::SyncHosts => self.start_hosts_sync().then_some(Command::SyncHosts),
            Action::StartFirstRun => {
                // The jobs wait in the queue: `App` sends them.
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
                self.session.sites_notice = None;
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
                self.session.runtimes_notice = None;
                None
            }
            Action::SubmitInstall(runtime) => {
                // The job waits in the queue: `App` sends it (`next_install`).
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
            // Done by `App`, with the system's opener and dialog: nothing
            // changes here.
            Action::OpenSite(_)
            | Action::OpenFolder(_)
            | Action::OpenDataFolder
            | Action::PickFolder(_) => None,
        }
    }
}

/// Why "Add to /etc/hosts" did not put the names in: the code of the
/// error, of a skipped step, or of a failed one. `None` if it did, or there
/// was nothing to add.
fn not_added(report: &SetupReport) -> Option<String> {
    if let Some(error) = &report.error {
        return Some(error.code.clone());
    }
    if let Some(skipped) = report.skipped.first() {
        return Some(skipped.code.clone());
    }
    report
        .results
        .iter()
        .find(|result| result.status == OpStatus::Failed)
        .map(|result| {
            result
                .code
                .clone()
                .unwrap_or_else(|| "hosts_sync".to_owned())
        })
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

/// Whether the first-run panel is for this list: it has been read, and PHP
/// or Caddy is not installed. Both are needed to open a site.
pub fn first_run_needed(rows: &[RuntimeRow]) -> bool {
    !rows.is_empty()
        && (runtime_missing(rows, Runtime::Php) || runtime_missing(rows, Runtime::Caddy))
}

/// The version to ask for from a version field: `None` (the newest stable
/// version) when it is empty, else the text without the spaces around it.
///
/// # Errors
///
/// `invalid_version` or `unsupported_alias`: the rules the daemon's
/// installer uses, so no request is sent that it would refuse.
pub fn install_version(text: &str) -> Result<Option<String>, &'static str> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    resolver::best_match(text, &[]).map_err(|err| err.code())?;
    Ok(Some(text.to_owned()))
}

/// A failure with only its code, for an install that never reached the
/// daemon or whose end was lost.
fn failure(code: String) -> InstallFailure {
    InstallFailure {
        code,
        ..InstallFailure::default()
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
