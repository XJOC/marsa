//! The window's state, and how each message from the bridge changes it.
//!
//! Pure: no drawing and no I/O, so the tests run without a window. The
//! drawing code in `app` only reads a [`Model`], and calls its methods when
//! the user presses something.

use std::collections::VecDeque;
use std::mem;
use std::path::{Path, PathBuf};

use marsa_core::resolver;
use marsa_core::runtimes::Runtime;
use marsa_core::sites;
use marsa_core::types::{
    Event, Health, InstallEvent, InstallFailure, InstallProgress, InstallUpdate, LogLine,
    LogStream, RuntimeList, ServiceAction, ServiceState, ServiceStatus, SetupReport, Site,
    SiteList, Source,
};
use semver::Version;

pub use crate::tray::menu::Autostart;

/// The service whose log the window shows.
pub const LOG_SERVICE: &str = "caddy";
/// The most log lines kept. The oldest go first.
pub const MAX_LOG_LINES: usize = 5000;
/// This build's version, shown in the Settings section.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// The PHP line the first-run panel installs.
pub const FIRST_PHP: &str = "8.4";
/// The start of the id of each PHP pool: php-fpm on macOS and Linux
/// (`php-fpm-8.4`), php-cgi on Windows (`php-cgi-8.4`, ADR 0016). The daemon
/// starts and stops the pools with the sites, and refuses actions on them.
const POOL_PREFIXES: [&str; 2] = ["php-fpm-", "php-cgi-"];
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

/// The line under the sites buttons: what the last sites action did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SitesNotice {
    /// The daemon accepted this folder to park.
    Parked(PathBuf),
    /// The daemon linked `folder` as the site `name`.
    Linked { name: String, folder: PathBuf },
    /// A sites action failed. A client error code.
    Failed(String),
}

/// One log line, without the fields the window does not show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    pub stream: LogStream,
    pub text: String,
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

/// The form under the sites buttons once a folder to link was picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkForm {
    pub folder: PathBuf,
    /// The site name the user types. Made from the folder's name at first.
    pub name: String,
}

/// What a folder dialog was opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Park,
    Link,
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
    pub sites_notice: Option<SitesNotice>,
    /// Open after a folder to link was picked. Kept across sessions: it is
    /// the user's typing, not daemon state.
    pub link: Option<LinkForm>,
    /// In [`Runtime::ALL`] order.
    pub runtimes: Vec<RuntimeRow>,
    /// The last runtimes read, default change, or version field failed. A
    /// client error code.
    pub runtimes_notice: Option<String>,
    /// Oldest first.
    pending: Vec<Pending>,
    /// The version field of each runtime, in [`Runtime::ALL`] order. Empty:
    /// the newest stable version. Kept across sessions.
    pub install_input: [String; 4],
    /// Oldest first, at most [`MAX_INSTALLS`].
    installs: Vec<Install>,
    /// The window's jobs not sent yet, in order.
    queue: VecDeque<InstallJob>,
    pub first_run: Option<FirstRun>,
    /// The user closed the first-run panel: it stays closed while this
    /// window is open.
    first_run_closed: bool,
    pub setup: Setup,
    pub autostart: Autostart,
    /// The last autostart read or change failed. A client error code.
    pub settings_notice: Option<String>,
    /// The data folder this window and its daemon use. Shown, never changed.
    pub data_dir: PathBuf,
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
                *self = Model {
                    connection: Connection::Running(health),
                    link: self.link.take(),
                    install_input: mem::take(&mut self.install_input),
                    // Only ended installs with no id are left: what the user
                    // saw last stays on screen.
                    installs: mem::take(&mut self.installs),
                    first_run_closed: self.first_run_closed,
                    data_dir: mem::take(&mut self.data_dir),
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
            Update::Parked(path) => self.sites_notice = Some(SitesNotice::Parked(path)),
            Update::Linked { name, folder } => {
                // The user may have picked another folder since.
                let same = self
                    .link
                    .as_ref()
                    .is_some_and(|form| form.folder == folder && form.name.trim() == name);
                if same {
                    self.link = None;
                }
                self.sites_notice = Some(SitesNotice::Linked { name, folder });
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
            Update::InstallAccepted { job, id } => self.accepted(&job, id),
            Update::InstallRefused { job, code } => self.refused(&job, code),
            Update::SetupDone(report) => self.setup = Setup::Finished(report),
            Update::SetupFailed(code) => self.setup = Setup::Failed(code),
            Update::Autostart(state) => self.autostart = state,
            Update::AutostartFailed(code) => {
                self.autostart = Autostart::Unknown;
                self.settings_notice = Some(code);
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
    /// folders named by a version. Opens or closes the first-run panel.
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
        let needed = first_run_needed(&self.runtimes);
        match self.first_run {
            None if needed && !self.first_run_closed => self.first_run = Some(FirstRun::Offer),
            // PHP or Caddy came some other way (`marsa install`).
            Some(FirstRun::Offer) if !needed => self.first_run = None,
            _ => {}
        }
    }

    // -- Sites: park and link with the folder dialog (task M5-10) --

    /// The user picked `folder` to park. Returns the folder to send, or
    /// `None` if it is not an absolute path; the notice line then says why.
    /// Clears the last notice either way: it was about the last action.
    pub fn park_request(&mut self, folder: &Path) -> Option<PathBuf> {
        match folder_path(folder) {
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

    /// The user picked `folder` to link: the form opens, with a site name
    /// made from the folder's name when one can be made (`My_App` gives
    /// `my-app`), as `marsa link` does.
    pub fn pick_link_folder(&mut self, folder: &Path) {
        match folder_path(folder) {
            Ok(folder) => {
                let name = folder
                    .file_name()
                    .and_then(sites::name_from_folder)
                    .unwrap_or_default();
                self.sites_notice = None;
                self.link = Some(LinkForm { folder, name });
            }
            Err(code) => self.sites_notice = Some(SitesNotice::Failed(code.to_owned())),
        }
    }

    /// The site name and folder of the link form, to send. `None` when the
    /// name is not valid: the notice line says why, and no request is sent
    /// (the daemon would answer `site_name_invalid` too).
    pub fn link_request(&mut self) -> Option<(String, PathBuf)> {
        let form = self.link.as_ref()?;
        let name = form.name.trim();
        if let Err(err) = sites::validate_name(name) {
            self.sites_notice = Some(SitesNotice::Failed(err.code().to_owned()));
            return None;
        }
        let request = (name.to_owned(), form.folder.clone());
        self.sites_notice = None;
        Some(request)
    }

    /// The site the link form would replace: one with the same name and
    /// another folder. The daemon links the new folder in its place.
    pub fn link_replaces(&self) -> Option<&Site> {
        let form = self.link.as_ref()?;
        let name = form.name.trim();
        self.sites
            .sites
            .iter()
            .find(|site| site.name == name && site.root != form.folder)
    }

    // -- Runtimes: installs (task M5-10) --

    /// The install the Runtimes section shows: one this window started that
    /// has not ended, else the newest one, whoever started it.
    pub fn install_view(&self) -> Option<&InstallView> {
        self.installs
            .iter()
            .rev()
            .find(|install| install.runs_for_us())
            .or_else(|| self.installs.last())
            .map(|install| &install.view)
    }

    /// Whether a job of this window waits or runs. The install buttons wait
    /// for it: the daemon runs one install at a time.
    pub fn installing(&self) -> bool {
        !self.queue.is_empty() || self.installs.iter().any(Install::runs_for_us)
    }

    /// The user pressed Install for `runtime`: queues the version in its
    /// field, or says why the field is wrong. `false` if nothing was queued.
    pub fn request_install(&mut self, runtime: Runtime) -> bool {
        if !self.is_connected() || self.installing() {
            return false;
        }
        match install_version(&self.install_input[runtime_index(runtime)]) {
            Ok(version) => {
                self.runtimes_notice = None;
                self.queue.push_back(InstallJob { runtime, version });
                true
            }
            Err(code) => {
                self.runtimes_notice = Some(code.to_owned());
                false
            }
        }
    }

    /// The next job to send, once no job of this window runs. It shows as
    /// waiting until the daemon answers. The window calls this after each
    /// change, so one call sends at most one job.
    pub fn next_install(&mut self) -> Option<InstallJob> {
        if !self.is_connected() || self.installs.iter().any(Install::runs_for_us) {
            return None;
        }
        let job = self.queue.pop_front()?;
        self.push_install(Install {
            id: None,
            ours: true,
            view: InstallView::waiting(&job),
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
            self.queue.clear();
        }
        if self.first_run == Some(FirstRun::Installing) && self.queue.is_empty() {
            self.first_run = if success {
                Some(FirstRun::Setup)
            } else {
                // The new runtimes list closes it if PHP or Caddy came.
                first_run_needed(&self.runtimes).then_some(FirstRun::Offer)
            };
        }
    }

    /// No event comes from a lost session: an install that runs shows as
    /// interrupted, and no id is kept, since a new daemon counts from 1
    /// again. Only the install shown stays.
    fn forget_installs(&mut self) {
        self.queue.clear();
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

    // -- First run, setup, and settings (task M5-10) --

    /// "Install PHP 8.4 and Caddy": the two jobs, in that order. `false` if
    /// the panel does not offer it now.
    pub fn start_first_run(&mut self) -> bool {
        if self.first_run != Some(FirstRun::Offer) || !self.is_connected() || self.installing() {
            return false;
        }
        self.runtimes_notice = None;
        self.queue.extend([
            InstallJob {
                runtime: Runtime::Php,
                version: Some(FIRST_PHP.to_owned()),
            },
            InstallJob {
                runtime: Runtime::Caddy,
                version: None,
            },
        ]);
        self.first_run = Some(FirstRun::Installing);
        true
    }

    /// "Not now" or "Close" in the first-run panel.
    pub fn close_first_run(&mut self) {
        self.first_run = None;
        self.first_run_closed = true;
    }

    /// "Run setup". `false` while one runs, or with no daemon.
    pub fn start_setup(&mut self) -> bool {
        if !self.is_connected() || self.setup == Setup::Running {
            return false;
        }
        self.setup = Setup::Running;
        true
    }

    /// The "Start at login" box was clicked. Returns the state to ask for,
    /// or `None` while the state is unknown or not supported. The box shows
    /// the new state until the daemon answers, as in the menu-bar icon.
    pub fn toggle_autostart(&mut self) -> Option<bool> {
        if !self.is_connected() {
            return None;
        }
        let enabled = match self.autostart {
            Autostart::On => false,
            Autostart::Off | Autostart::Stale => true,
            Autostart::Unknown | Autostart::Unsupported => return None,
        };
        self.autostart = if enabled {
            Autostart::On
        } else {
            Autostart::Off
        };
        self.settings_notice = None;
        Some(enabled)
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
        self.forget_installs();
        self.first_run = None;
        if self.setup == Setup::Running {
            self.setup = Setup::Idle;
        }
        self.autostart = Autostart::Unknown;
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
        Event::Install(progress) => model.install_event(progress),
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
        // From this window or from `marsa setup`: the same report.
        Event::SetupFinished(report) => model.setup = Setup::Finished(report),
        Event::ShuttingDown => model.disconnect(Connection::ShuttingDown),
    }
}

/// Whether the first-run panel is for this list: it has been read, and
/// neither PHP nor Caddy is installed.
pub fn first_run_needed(rows: &[RuntimeRow]) -> bool {
    let missing = |runtime| {
        rows.iter()
            .find(|row| row.runtime == runtime)
            .is_none_or(|row| row.installed.is_empty())
    };
    !rows.is_empty() && missing(Runtime::Php) && missing(Runtime::Caddy)
}

/// The position of `runtime` in [`Runtime::ALL`], and in
/// [`Model::install_input`].
pub fn runtime_index(runtime: Runtime) -> usize {
    match runtime {
        Runtime::Php => 0,
        Runtime::Node => 1,
        Runtime::Composer => 2,
        Runtime::Caddy => 3,
    }
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

/// Whether the service `id` is a PHP pool, which starts and stops with the
/// sites that need it.
pub fn is_pool(id: &str) -> bool {
    POOL_PREFIXES.iter().any(|prefix| id.starts_with(prefix))
}

/// The address of a site in the browser: `https://shop.test`, or
/// `http://shop.test` when HTTPS is off.
pub fn url(site: &Site) -> String {
    let scheme = if site.secure { "https" } else { "http" };
    format!("{scheme}://{}", site.host())
}

/// A picked folder as the daemon wants it: absolute, with `.` parts,
/// doubled separators, and a trailing separator removed, as `marsa park`
/// does.
///
/// # Errors
///
/// `site_path_relative` for a relative or empty path. The daemon would
/// answer the same, so no request is sent.
pub fn folder_path(folder: &Path) -> Result<PathBuf, &'static str> {
    if !folder.is_absolute() {
        return Err("site_path_relative");
    }
    // Not `canonicalize`: it reads the disk, follows links, and gives `\\?\`
    // paths on Windows.
    Ok(folder.components().collect())
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
    fn the_sites_go_with_the_connection_but_the_link_form_stays() {
        let mut model = connected();
        model.update(Update::Sites(site_list(&["shop"])));
        model.pick_link_folder(&absolute("/work/api"));
        model.update(Update::Reconnecting);
        assert!(model.sites.sites.is_empty() && model.sites.parked.is_empty());

        model.update(Update::Connected(health()));
        let form = model.link.as_ref().unwrap();
        assert_eq!(
            (form.folder.clone(), form.name.as_str()),
            (absolute("/work/api"), "api")
        );
        assert!(model.sites_notice.is_none());
    }

    #[test]
    fn a_picked_folder_needs_an_absolute_path() {
        let code = absolute("/code");
        let shown = code.display().to_string();
        for input in [shown.clone(), format!("{shown}/"), format!("{shown}/./")] {
            assert_eq!(
                folder_path(Path::new(&input)),
                Ok(code.clone()),
                "{input:?}"
            );
        }
        for input in ["", "code", "./code", "~/code"] {
            let got = folder_path(Path::new(input));
            assert_eq!(got, Err("site_path_relative"), "{input:?}");
        }
    }

    #[test]
    fn a_relative_folder_is_refused_without_a_request() {
        let mut model = connected();
        assert_eq!(model.park_request(Path::new("code")), None);
        let failed = SitesNotice::Failed("site_path_relative".into());
        assert_eq!(model.sites_notice, Some(failed.clone()));
        model.pick_link_folder(Path::new("code"));
        assert!(model.link.is_none());
        assert_eq!(model.sites_notice, Some(failed));

        let picked = format!("{}/", absolute("/code").display());
        assert_eq!(
            model.park_request(Path::new(&picked)),
            Some(absolute("/code"))
        );
        assert!(model.sites_notice.is_none());
    }

    #[test]
    fn a_park_says_so() {
        let mut model = connected();
        model.update(Update::Parked(absolute("/code")));
        let parked = SitesNotice::Parked(absolute("/code"));
        assert_eq!(model.sites_notice, Some(parked));
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

    // -- Pools (ADR 0016) --

    #[test]
    fn php_fpm_and_php_cgi_pools_are_managed_by_the_sites() {
        assert!(is_pool("php-fpm-8.4"));
        assert!(is_pool("php-cgi-8.4"));
        for id in ["caddy", "php", "php-fpm", "my-php-fpm-8.4", "dns"] {
            assert!(!is_pool(id), "{id}");
        }
    }

    // -- Installs (task M5-10) --

    use marsa_core::types::{InstallDone, SetupSkip};

    fn job(runtime: Runtime, version: Option<&str>) -> InstallJob {
        InstallJob {
            runtime,
            version: version.map(str::to_owned),
        }
    }

    fn install(id: u64, update: InstallUpdate) -> Event {
        Event::Install(InstallProgress { id, update })
    }

    fn running(id: u64, event: InstallEvent) -> Event {
        install(id, InstallUpdate::Running(event))
    }

    fn downloading(id: u64, done: u64, total: u64) -> Event {
        let artifact = "cli".to_owned();
        running(
            id,
            InstallEvent::Downloading {
                artifact,
                done,
                total,
            },
        )
    }

    fn done(id: u64, runtime: Runtime, version: &str) -> Event {
        install(
            id,
            InstallUpdate::Done(InstallDone {
                runtime,
                version: version.into(),
                dir: PathBuf::from("/d/runtimes").join(version),
                already: false,
            }),
        )
    }

    fn failed(id: u64, code: &str) -> Event {
        install(id, InstallUpdate::Failed(failure(code.into())))
    }

    fn step(model: &Model) -> InstallStep {
        model.install_view().unwrap().step.clone()
    }

    /// Connected, with this list read: PHP and Caddy as given, no Node or
    /// Composer.
    fn with_runtimes(php: &[&str], caddy: &[&str]) -> Model {
        let mut model = connected();
        model.update(Update::Runtimes(list(php, caddy)));
        model
    }

    fn list(php: &[&str], caddy: &[&str]) -> RuntimeList {
        let row = |runtime, installed: &[&str]| marsa_core::types::RuntimeVersions {
            runtime,
            installed: installed.iter().map(|t| (*t).to_owned()).collect(),
            default: None,
        };
        RuntimeList {
            runtimes: vec![
                row(Runtime::Php, php),
                row(Runtime::Node, &[]),
                row(Runtime::Composer, &[]),
                row(Runtime::Caddy, caddy),
            ],
        }
    }

    #[test]
    fn the_version_field_follows_the_installers_rules() {
        for (text, want) in [
            ("", None),
            ("   ", None),
            ("8.4", Some("8.4")),
            (" 8.4.23 ", Some("8.4.23")),
            ("22", Some("22")),
        ] {
            assert_eq!(
                install_version(text),
                Ok(want.map(str::to_owned)),
                "{text:?}"
            );
        }
        assert_eq!(install_version("latest"), Err("unsupported_alias"));
        assert_eq!(install_version("8.x.y"), Err("invalid_version"));
    }

    #[test]
    fn an_install_runs_one_job_at_a_time_and_shows_each_step() {
        let mut model = with_runtimes(&["8.3.27"], &["2.11.7"]);
        model.install_input[runtime_index(Runtime::Php)] = " 8.4 ".into();
        assert!(model.request_install(Runtime::Php));
        // One job at a time: a second press waits for the first.
        assert!(!model.request_install(Runtime::Node));
        assert!(model.installing());

        let sent = model.next_install();
        assert_eq!(sent, Some(job(Runtime::Php, Some("8.4"))));
        assert_eq!(model.next_install(), None);
        assert_eq!(step(&model), InstallStep::Waiting);

        model.update(Update::InstallAccepted {
            job: sent.unwrap(),
            id: 7,
        });
        apply(
            &mut model,
            running(7, InstallEvent::ManifestVerified { serial: 2 }),
        );
        assert_eq!(step(&model), InstallStep::ListChecked);
        let selected = InstallEvent::Selected {
            runtime: Runtime::Php,
            version: "8.4.23".into(),
        };
        apply(&mut model, running(7, selected));
        apply(&mut model, downloading(7, 1, 4));
        let view = model.install_view().unwrap();
        assert_eq!(view.version.as_deref(), Some("8.4.23"));
        assert!((view.step.fraction() - 0.25).abs() < f32::EPSILON);
        let artifact = "cli".to_owned();
        apply(
            &mut model,
            running(7, InstallEvent::Extracting { artifact }),
        );
        assert_eq!(step(&model).fraction(), 1.0);
        apply(&mut model, running(7, InstallEvent::Checking));
        let dir = PathBuf::from("/d");
        apply(&mut model, running(7, InstallEvent::Installed { dir }));
        assert_eq!(step(&model), InstallStep::Finishing);

        apply(&mut model, done(7, Runtime::Php, "8.4.23"));
        assert_eq!(step(&model), InstallStep::Done { already: false });
        assert!(!model.installing());
        model.install_input[runtime_index(Runtime::Node)] = "22".into();
        assert!(model.request_install(Runtime::Node));
    }

    #[test]
    fn a_bad_version_field_is_refused_without_a_request() {
        let mut model = with_runtimes(&[], &[]);
        model.install_input[runtime_index(Runtime::Node)] = "lts/*".into();
        assert!(!model.request_install(Runtime::Node));
        assert_eq!(model.runtimes_notice.as_deref(), Some("unsupported_alias"));
        assert_eq!(model.next_install(), None);
        // An empty field asks for the newest version.
        model.install_input[runtime_index(Runtime::Node)].clear();
        assert!(model.request_install(Runtime::Node));
        assert!(model.runtimes_notice.is_none());
        assert_eq!(model.next_install(), Some(job(Runtime::Node, None)));
    }

    #[test]
    fn events_that_come_before_the_answer_are_the_jobs() {
        let mut model = with_runtimes(&[], &["2.11.7"]);
        assert!(model.request_install(Runtime::Composer));
        let sent = model.next_install().unwrap();
        // The event stream is faster than the answer to the request.
        apply(&mut model, downloading(3, 2, 4));
        apply(&mut model, done(3, Runtime::Composer, "2.8.3"));
        assert!(model.installing(), "the job waits for its answer");
        model.update(Update::InstallAccepted { job: sent, id: 3 });
        assert!(!model.installing());
        assert_eq!(step(&model), InstallStep::Done { already: false });
        assert_eq!(model.installs.len(), 1);
    }

    #[test]
    fn a_refused_install_says_why_and_drops_the_rest() {
        let mut model = with_runtimes(&[], &[]);
        assert!(model.start_first_run());
        let sent = model.next_install().unwrap();
        let code = "install_busy".to_owned();
        model.update(Update::InstallRefused { job: sent, code });
        assert_eq!(
            step(&model),
            InstallStep::Failed(failure("install_busy".into()))
        );
        // Caddy is not sent after PHP failed; the offer is back.
        assert_eq!(model.next_install(), None);
        assert!(!model.installing());
        assert_eq!(model.first_run, Some(FirstRun::Offer));
    }

    #[test]
    fn an_install_from_a_terminal_shows_too() {
        let mut model = with_runtimes(&[], &[]);
        apply(&mut model, downloading(9, 3, 4));
        let view = model.install_view().unwrap();
        assert_eq!(view.runtime, None);
        assert!((view.step.fraction() - 0.75).abs() < f32::EPSILON);
        // It is not the window's: the buttons stay.
        assert!(!model.installing());
        apply(&mut model, failed(9, "checksum_mismatch"));
        assert_eq!(
            step(&model),
            InstallStep::Failed(failure("checksum_mismatch".into()))
        );
    }

    #[test]
    fn a_lost_session_interrupts_the_install_and_keeps_it_shown() {
        let mut model = with_runtimes(&[], &[]);
        assert!(model.start_first_run());
        let sent = model.next_install().unwrap();
        model.update(Update::InstallAccepted { job: sent, id: 1 });
        apply(&mut model, downloading(1, 1, 2));
        apply(&mut model, Event::ShuttingDown);
        let interrupted = InstallStep::Failed(failure(INSTALL_INTERRUPTED.into()));
        assert_eq!(step(&model), interrupted);
        assert!(!model.installing() && model.first_run.is_none());

        // A new daemon counts from 1 again: its install 1 is a new one.
        model.update(Update::Connected(health()));
        assert_eq!(step(&model), interrupted);
        apply(&mut model, downloading(1, 0, 2));
        assert_eq!(model.installs.len(), 2);
        assert_eq!(step(&model).fraction(), 0.0);
    }

    #[test]
    fn at_most_four_installs_are_kept() {
        let mut model = with_runtimes(&[], &[]);
        for id in 1..=6 {
            apply(&mut model, failed(id, "download_failed"));
        }
        let ids: Vec<_> = model.installs.iter().map(|i| i.id).collect();
        assert_eq!(ids, [Some(3), Some(4), Some(5), Some(6)]);
    }

    // -- First run (task M5-10) --

    #[test]
    fn the_first_run_panel_shows_with_no_php_and_no_caddy() {
        assert!(with_runtimes(&[], &[]).first_run == Some(FirstRun::Offer));
        assert!(with_runtimes(&["8.4.23"], &[]).first_run.is_none());
        assert!(with_runtimes(&[], &["2.11.7"]).first_run.is_none());
        // Not before the list is read.
        assert!(connected().first_run.is_none());
        assert!(!first_run_needed(&[]));

        // PHP installed from a terminal closes the offer.
        let mut model = with_runtimes(&[], &[]);
        model.update(Update::Runtimes(list(&["8.4.23"], &[])));
        assert!(model.first_run.is_none());
    }

    #[test]
    fn the_first_run_installs_php_then_caddy_then_offers_setup() {
        let mut model = with_runtimes(&[], &[]);
        assert!(model.start_first_run());
        assert!(!model.start_first_run(), "once");
        assert_eq!(model.first_run, Some(FirstRun::Installing));

        let php = model.next_install().unwrap();
        assert_eq!(php, job(Runtime::Php, Some(FIRST_PHP)));
        model.update(Update::InstallAccepted { job: php, id: 1 });
        apply(&mut model, done(1, Runtime::Php, "8.4.23"));
        // The bridge reads the list again after each install.
        model.update(Update::Runtimes(list(&["8.4.23"], &[])));
        assert_eq!(model.first_run, Some(FirstRun::Installing));

        let caddy = model.next_install().unwrap();
        assert_eq!(caddy, job(Runtime::Caddy, None));
        model.update(Update::InstallAccepted { job: caddy, id: 2 });
        apply(&mut model, done(2, Runtime::Caddy, "2.11.7"));
        assert_eq!(model.first_run, Some(FirstRun::Setup));
        model.update(Update::Runtimes(list(&["8.4.23"], &["2.11.7"])));
        assert_eq!(model.first_run, Some(FirstRun::Setup));
        assert_eq!(model.next_install(), None);
    }

    #[test]
    fn a_failed_caddy_closes_the_first_run_once_php_is_there() {
        let mut model = with_runtimes(&[], &[]);
        model.start_first_run();
        let php = model.next_install().unwrap();
        model.update(Update::InstallAccepted { job: php, id: 1 });
        apply(&mut model, done(1, Runtime::Php, "8.4.23"));
        model.update(Update::Runtimes(list(&["8.4.23"], &[])));
        let caddy = model.next_install().unwrap();
        model.update(Update::InstallAccepted { job: caddy, id: 2 });
        apply(&mut model, failed(2, "download_failed"));
        // PHP is there: Caddy is installed from the Runtimes section.
        assert!(model.first_run.is_none());
        assert_eq!(
            step(&model),
            InstallStep::Failed(failure("download_failed".into()))
        );
    }

    #[test]
    fn a_closed_first_run_stays_closed() {
        let mut model = with_runtimes(&[], &[]);
        model.close_first_run();
        model.update(Update::Runtimes(list(&[], &[])));
        assert!(model.first_run.is_none());
        model.update(Update::Reconnecting);
        model.update(Update::Connected(health()));
        model.update(Update::Runtimes(list(&[], &[])));
        assert!(model.first_run.is_none());
        assert!(!model.start_first_run());
    }

    // -- Setup (task M5-10) --

    fn report(code: &str) -> SetupReport {
        SetupReport {
            skipped: vec![SetupSkip {
                op: "trust_install".into(),
                code: code.into(),
            }],
            ..SetupReport::default()
        }
    }

    #[test]
    fn setup_runs_once_and_shows_its_report() {
        let mut model = Model::default();
        assert!(!model.start_setup(), "no daemon");
        let mut model = connected();
        assert!(model.start_setup());
        assert!(!model.start_setup(), "one at a time");
        // The event and the answer carry the same report.
        apply(&mut model, Event::SetupFinished(report("ca_not_ready")));
        model.update(Update::SetupDone(report("ca_not_ready")));
        assert_eq!(model.setup, Setup::Finished(report("ca_not_ready")));
        assert!(model.start_setup(), "again");
        model.update(Update::SetupFailed("setup_not_supported".into()));
        assert_eq!(model.setup, Setup::Failed("setup_not_supported".into()));
    }

    #[test]
    fn a_setup_from_a_terminal_shows_and_a_lost_one_ends() {
        let mut model = connected();
        apply(&mut model, Event::SetupFinished(report("ca_unreadable")));
        assert_eq!(model.setup, Setup::Finished(report("ca_unreadable")));
        assert!(model.start_setup());
        model.update(Update::Reconnecting);
        assert_eq!(model.setup, Setup::Idle);
    }

    // -- Settings (task M5-10) --

    #[test]
    fn start_at_login_follows_each_state() {
        let mut model = connected();
        assert_eq!(model.autostart, Autostart::Unknown);
        assert_eq!(model.toggle_autostart(), None);
        for (state, want, shown) in [
            (Autostart::Off, Some(true), Autostart::On),
            (Autostart::On, Some(false), Autostart::Off),
            // A login file for another `marsa-tray`: written again.
            (Autostart::Stale, Some(true), Autostart::On),
            (Autostart::Unsupported, None, Autostart::Unsupported),
        ] {
            model.update(Update::Autostart(state));
            assert_eq!(model.toggle_autostart(), want, "{state:?}");
            assert_eq!(model.autostart, shown, "{state:?}");
        }
        model.update(Update::AutostartFailed("autostart_write".into()));
        assert_eq!(model.autostart, Autostart::Unknown);
        assert_eq!(model.settings_notice.as_deref(), Some("autostart_write"));
        // The bridge reads it again; the notice stays until the next click.
        model.update(Update::Autostart(Autostart::Off));
        assert_eq!(model.settings_notice.as_deref(), Some("autostart_write"));
        model.toggle_autostart();
        assert!(model.settings_notice.is_none());

        model.update(Update::Reconnecting);
        assert_eq!(model.autostart, Autostart::Unknown);
        assert_eq!(model.toggle_autostart(), None);
    }

    #[test]
    fn the_data_folder_and_the_typing_stay_across_sessions() {
        let mut model = Model::new(absolute("/data"));
        model.update(Update::Connected(health()));
        model.install_input[runtime_index(Runtime::Php)] = "8.3".into();
        model.update(Update::Reconnecting);
        model.update(Update::Connected(health()));
        assert_eq!(model.data_dir, absolute("/data"));
        assert_eq!(model.install_input[0], "8.3");
        assert!(!APP_VERSION.is_empty());
        assert_eq!(
            Runtime::ALL.map(runtime_index),
            [0, 1, 2, 3],
            "the fields follow Runtime::ALL"
        );
    }

    // -- Link (task M5-10) --

    #[test]
    fn a_picked_folder_gets_a_name_made_from_it() {
        let mut model = connected();
        model.pick_link_folder(&absolute("/work/My_App.v2"));
        let form = model.link.clone().unwrap();
        assert_eq!(form.folder, absolute("/work/My_App.v2"));
        assert_eq!(form.name, "my-app-v2");
        // No valid name can be made: the user types one.
        model.pick_link_folder(&absolute("/work/متجر"));
        assert_eq!(model.link.as_ref().unwrap().name, "");
    }

    #[test]
    fn a_bad_site_name_is_refused_without_a_request() {
        let mut model = connected();
        model.pick_link_folder(&absolute("/work/متجر"));
        assert_eq!(model.link_request(), None);
        let invalid = SitesNotice::Failed("site_name_invalid".into());
        assert_eq!(model.sites_notice, Some(invalid.clone()));
        for name in ["Shop", "-shop", "shop-", "my shop", &"a".repeat(64)] {
            model.link.as_mut().unwrap().name = name.to_owned();
            assert_eq!(model.link_request(), None, "{name}");
            assert_eq!(model.sites_notice, Some(invalid.clone()));
        }
        model.link.as_mut().unwrap().name = " shop ".into();
        let want = ("shop".to_owned(), absolute("/work/متجر"));
        assert_eq!(model.link_request(), Some(want));
        assert!(model.sites_notice.is_none());
    }

    #[test]
    fn linking_over_another_folder_is_named_and_a_link_closes_the_form() {
        let mut model = connected();
        model.update(Update::Sites(site_list(&["shop"])));
        model.pick_link_folder(&absolute("/work/shop"));
        let replaced = model.link_replaces().map(|site| site.root.clone());
        assert_eq!(replaced, Some(PathBuf::from("/code/shop")));
        // The same folder again keeps the site: nothing is replaced.
        model.link.as_mut().unwrap().folder = PathBuf::from("/code/shop");
        assert!(model.link_replaces().is_none());

        model.pick_link_folder(&absolute("/work/api"));
        let (name, folder) = model.link_request().unwrap();
        model.update(Update::Linked {
            name: name.clone(),
            folder: folder.clone(),
        });
        assert!(model.link.is_none());
        assert_eq!(
            model.sites_notice,
            Some(SitesNotice::Linked { name, folder })
        );
    }

    #[test]
    fn a_link_answer_keeps_a_newer_form() {
        let mut model = connected();
        model.pick_link_folder(&absolute("/work/web"));
        let name = "api".to_owned();
        let folder = absolute("/work/api");
        model.update(Update::Linked { name, folder });
        assert_eq!(model.link.as_ref().unwrap().name, "web");
    }
}
