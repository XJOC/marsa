//! What the daemon told the window, and how the connection to it reads as one status.

use std::collections::{BTreeMap, VecDeque};

use super::runtimes::Pending;
use super::*;

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

impl Connection {
    /// The line that says how the window and the daemon are connected.
    pub fn status_line(&self) -> String {
        match self {
            Connection::Connecting => strings::CONNECTING.to_owned(),
            Connection::Reconnecting => strings::RECONNECTING.to_owned(),
            Connection::Running(health) => strings::daemon_running(&health.app_version, health.pid),
            Connection::ShuttingDown => strings::DAEMON_SHUTTING_DOWN.to_owned(),
            Connection::Failed(code) => strings::connection_failed(code),
        }
    }

    /// The version of the daemon while connected.
    pub fn daemon_version(&self) -> Option<&str> {
        match self {
            Connection::Running(health) => Some(&health.app_version),
            _ => None,
        }
    }
}

/// How Marsa works as a whole, as far as the window can tell: the status at
/// the bottom of the sidebar (principles 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overall {
    /// Connecting to the daemon, or again after it went away.
    Connecting,
    /// The daemon is shutting down.
    Stopping,
    /// The daemon cannot be reached or started: no site opens.
    DaemonDown,
    /// The daemon runs, and there is no site yet.
    NoSites,
    /// The web server is starting.
    Starting,
    /// The web server does not run: no site opens.
    WebServerDown,
    /// The daemon and the web server run.
    Working,
}

impl Overall {
    /// The status at the bottom of the sidebar: a title and a caption, short
    /// enough for its width.
    pub const fn text(self) -> (&'static str, &'static str) {
        match self {
            Overall::Connecting => strings::OVERALL_CONNECTING,
            Overall::Stopping => strings::OVERALL_STOPPING,
            Overall::DaemonDown => strings::OVERALL_DAEMON_DOWN,
            Overall::NoSites => strings::OVERALL_NO_SITES,
            Overall::Starting => strings::OVERALL_STARTING,
            Overall::WebServerDown => strings::OVERALL_WEB_SERVER_DOWN,
            Overall::Working => strings::OVERALL_WORKING,
        }
    }
}

/// What the daemon told this window, and what the window is doing about it.
/// A new connection starts from [`Session::default`], so a field added here
/// is forgotten on reconnect without a list to keep in step.
#[derive(Debug, Default)]
pub struct Session {
    pub connection: Connection,
    /// The last result of each place that tells the user about one.
    pub notices: Notices,
    /// Sorted by id.
    pub services: Vec<ServiceStatus>,
    /// The log of each service that has written one, by the service's id.
    pub(super) logs: BTreeMap<String, ServiceLog>,
    /// Sites sorted by name, the parked folders, and the skipped folders.
    pub sites: SiteList,
    /// As the daemon lists them.
    pub runtimes: Vec<RuntimeRow>,
    /// Oldest first.
    pub(super) pending: Vec<Pending>,
    /// The runtime whose install dialog is open.
    pub install_dialog: Option<Runtime>,
    /// The window's jobs not sent yet, in order.
    pub(super) queue: VecDeque<InstallJob>,
    pub first_run: Option<FirstRun>,
    pub setup: Setup,
    pub autostart: Autostart,
    /// The removal the confirmation dialog asks about.
    pub confirm: Option<Removal>,
    /// The removal sent and not answered yet. One at a time.
    pub(super) removing: Option<Removal>,
    /// Whether PHP loads Xdebug, as the daemon said last. `None` until read.
    pub xdebug: Option<XdebugState>,
    /// The state the Xdebug box asked for, until the daemon answers.
    pub(super) xdebug_pending: Option<bool>,
    /// Linux: the sites the hosts file lacks.
    pub hosts: HostsStatus,
    pub hosts_sync: HostsSync,
    /// The daemon's checks (`GET /v1/doctor`), as read when the window
    /// connected and after each setup. `None` until read, or when the daemon
    /// gave none.
    pub doctor: Option<DoctorReport>,
}

impl Model {
    pub fn is_connected(&self) -> bool {
        matches!(self.session.connection, Connection::Running(_))
    }

    /// How Marsa works as a whole: the daemon, then the sites, then the web
    /// server that serves them.
    pub fn overall(&self) -> Overall {
        match &self.session.connection {
            Connection::Connecting | Connection::Reconnecting => Overall::Connecting,
            Connection::ShuttingDown => Overall::Stopping,
            Connection::Failed(_) => Overall::DaemonDown,
            Connection::Running(_) if self.session.sites.sites.is_empty() => Overall::NoSites,
            Connection::Running(_) => {
                let web = self
                    .session
                    .services
                    .iter()
                    .find(|service| service.id == WEB_SERVER);
                match web.map(|service| &service.state) {
                    Some(ServiceState::Running) => Overall::Working,
                    Some(ServiceState::Starting) => Overall::Starting,
                    _ => Overall::WebServerDown,
                }
            }
        }
    }

    /// The tables show live state only, so they go. The log stays: it may
    /// tell why the daemon stopped.
    pub(super) fn disconnect(&mut self, connection: Connection) {
        self.session.connection = connection;
        self.session.services.clear();
        self.session.sites = SiteList::default();
        self.session.runtimes.clear();
        // No answer comes from a lost session.
        self.session.pending.clear();
        self.forget_installs();
        self.session.first_run = None;
        // Its buttons are gone with the session.
        self.session.install_dialog = None;
        if self.session.setup == Setup::Running {
            self.session.setup = Setup::Idle;
        }
        self.session.autostart = Autostart::Unknown;
        // No answer comes from a lost session, and its buttons are gone.
        self.session.confirm = None;
        self.session.removing = None;
        self.session.xdebug = None;
        self.session.xdebug_pending = None;
    }
}
