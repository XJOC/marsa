//! First run, setup, the hosts file, start at login, and Xdebug: the one-time and settings actions.

use super::*;

/// The PHP line the first-run panel installs.
pub const FIRST_PHP: &str = "8.4";

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

/// Why "Add to /etc/hosts" did not put the names in: the code of the
/// error, of a skipped step, or of a failed one. `None` if it did, or there
/// was nothing to add.
pub(super) fn not_added(report: &SetupReport) -> Option<String> {
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

/// Whether the first-run panel is for this list: it has been read, and PHP
/// or Caddy is not installed. Both are needed to open a site.
pub fn first_run_needed(rows: &[RuntimeRow]) -> bool {
    !rows.is_empty()
        && (runtime_missing(rows, Runtime::Php) || runtime_missing(rows, Runtime::Caddy))
}

impl Model {
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
}
