//! The menu as data: the state the icon knows, the updates and clicks that
//! change it, and the lines the menu shows for it. Nothing here touches a
//! window or a menu widget, so every case is unit-tested without one.

use marsa_core::resolver;
use marsa_core::runtimes::Runtime;
use marsa_core::types::AutostartState;
use semver::Version;

use crate::model::version_text;
use crate::strings;

/// The daemon, as the icon last saw it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Daemon {
    /// `ensure_running` is on its way: at the start, and after "Start Marsa".
    #[default]
    Starting,
    Running,
    /// After "Stop Marsa", or the daemon's own `shutting_down` event, until
    /// it is gone.
    Stopping,
    Stopped,
    /// It could not be started or reached. A client error code.
    Failed(String),
}

/// Whether the icon starts at login, as the daemon last said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Autostart {
    /// Not read yet, or not readable.
    #[default]
    Unknown,
    Off,
    On,
    /// A login file is there, but it starts another `marsa-tray` (the app
    /// moved). A click writes it again.
    Stale,
    /// The daemon does not support it on this system (Windows for now).
    Unsupported,
}

impl From<AutostartState> for Autostart {
    fn from(state: AutostartState) -> Self {
        if state.enabled {
            Autostart::On
        } else if state.stale {
            Autostart::Stale
        } else {
            Autostart::Off
        }
    }
}

/// Everything the menu is drawn from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    pub daemon: Daemon,
    /// The installed PHP versions, oldest first. Kept while the daemon is
    /// stopped, shown disabled; read again when it is back.
    pub php: Vec<Version>,
    /// The default PHP as saved (`8.4`).
    pub php_default: Option<String>,
    pub autostart: Autostart,
    /// Why the last action failed, if it did: a client error code. Shown
    /// until the next click or the next change of the daemon.
    pub notice: Option<String>,
}

/// What the worker tells the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Daemon(Daemon),
    /// The PHP versions and default, read when a session starts and after
    /// an install.
    Php {
        installed: Vec<Version>,
        default: Option<String>,
    },
    /// The daemon's `default_changed` event for PHP: as saved (`8.4`).
    PhpDefault(String),
    Autostart(Autostart),
    /// An action failed; the state it touched was read again before this.
    /// A client error code.
    Failed(String),
}

/// What the menu asks the worker to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `ensure_running`: start `marsad` if none runs.
    Start,
    /// `POST /v1/shutdown`. The icon stays.
    Stop,
    /// `PUT /v1/runtimes/php/default` with this text (`8.4` or `8.4.23`).
    SetPhp(String),
    /// `PUT /v1/autostart`.
    SetAutostart(bool),
}

impl State {
    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Daemon(daemon) => {
                // The notice was about the daemon that was.
                self.notice = None;
                self.daemon = daemon;
            }
            Update::Php { installed, default } => {
                self.php = installed;
                self.php_default = default;
            }
            Update::PhpDefault(default) => self.php_default = Some(default),
            Update::Autostart(autostart) => self.autostart = autostart,
            Update::Failed(code) => self.notice = Some(code),
        }
    }

    /// The installed version the default picks now, if any.
    pub fn php_default_version(&self) -> Option<Version> {
        resolver::best_match(self.php_default.as_deref()?, &self.php)
            .ok()
            .flatten()
    }

    /// The version at `index` of the PHP submenu was clicked. It is checked
    /// at once; the command sends it. `None` when it is the default already,
    /// there is no such line, or the daemon is not running.
    pub fn pick_php(&mut self, index: usize) -> Option<Command> {
        if self.daemon != Daemon::Running {
            return None;
        }
        let chosen = self.php.get(index)?.clone();
        if self.php_default_version().as_ref() == Some(&chosen) {
            return None;
        }
        // `8.4` when that line picks the version now, so later patch
        // releases follow; the full version otherwise. The window's rule.
        let text = version_text(Runtime::Php, &chosen, &self.php);
        self.notice = None;
        self.php_default = Some(text.clone());
        Some(Command::SetPhp(text))
    }

    /// "Start at Login" was clicked. The mark flips at once; the command
    /// sends it. `None` when the line is disabled.
    pub fn toggle_autostart(&mut self) -> Option<Command> {
        if self.daemon != Daemon::Running {
            return None;
        }
        let (next, enabled) = match self.autostart {
            Autostart::On => (Autostart::Off, false),
            Autostart::Off | Autostart::Stale => (Autostart::On, true),
            Autostart::Unknown | Autostart::Unsupported => return None,
        };
        self.notice = None;
        self.autostart = next;
        Some(Command::SetAutostart(enabled))
    }

    /// "Stop Marsa" or "Start Marsa" was clicked. `None` while the daemon
    /// is on its way up or down.
    pub fn power(&mut self) -> Option<Command> {
        let (next, command) = match self.daemon {
            Daemon::Running => (Daemon::Stopping, Command::Stop),
            Daemon::Stopped | Daemon::Failed(_) => (Daemon::Starting, Command::Start),
            Daemon::Starting | Daemon::Stopping => return None,
        };
        self.notice = None;
        self.daemon = next;
        Some(command)
    }
}

/// One line of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub enabled: bool,
    /// `Some` for a line with a check mark.
    pub checked: Option<bool>,
}

impl Item {
    fn text(label: impl Into<String>, enabled: bool) -> Self {
        Self {
            label: label.into(),
            enabled,
            checked: None,
        }
    }

    fn check(label: impl Into<String>, enabled: bool, checked: bool) -> Self {
        Self {
            label: label.into(),
            enabled,
            checked: Some(checked),
        }
    }
}

/// The menu, top to bottom. Separators are not listed: the real menu puts
/// them in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The daemon's state. Never enabled.
    pub status: Item,
    /// Why the last action failed, under the status line. Never enabled.
    pub notice: Option<Item>,
    pub open: Item,
    /// The PHP submenu's own line: enabled when there is a version to pick.
    pub php: Item,
    /// The lines inside it, in the order of [`State::php`], the default
    /// checked.
    pub php_versions: Vec<Item>,
    pub autostart: Item,
    /// "Stop Marsa" or "Start Marsa".
    pub power: Item,
    pub quit: Item,
}

impl Layout {
    /// Every line top to bottom, the submenu's lines right after it.
    pub fn lines(&self) -> Vec<&Item> {
        let mut lines = vec![&self.status];
        lines.extend(self.notice.as_ref());
        lines.push(&self.open);
        lines.push(&self.php);
        lines.extend(&self.php_versions);
        lines.extend([&self.autostart, &self.power, &self.quit]);
        lines
    }
}

/// The lines the menu shows for `state`.
pub fn layout(state: &State) -> Layout {
    let running = state.daemon == Daemon::Running;
    let default = state.php_default_version();
    let php_versions = state
        .php
        .iter()
        .map(|version| {
            Item::check(
                version.to_string(),
                running,
                default.as_ref() == Some(version),
            )
        })
        .collect();
    let (autostart_label, checked, enabled) = match state.autostart {
        Autostart::Unknown | Autostart::Unsupported => (strings::TRAY_AUTOSTART, false, false),
        Autostart::Off => (strings::TRAY_AUTOSTART, false, running),
        Autostart::On => (strings::TRAY_AUTOSTART, true, running),
        Autostart::Stale => (strings::TRAY_AUTOSTART_STALE, false, running),
    };
    let power = match &state.daemon {
        Daemon::Starting => Item::text(strings::TRAY_STOP, false),
        Daemon::Running => Item::text(strings::TRAY_STOP, true),
        Daemon::Stopping => Item::text(strings::TRAY_START, false),
        Daemon::Stopped | Daemon::Failed(_) => Item::text(strings::TRAY_START, true),
    };
    Layout {
        status: Item::text(strings::tray_status(&state.daemon), false),
        notice: state
            .notice
            .as_deref()
            .map(|code| Item::text(strings::error_text(code), false)),
        open: Item::text(strings::TRAY_OPEN, true),
        php: Item::text(strings::TRAY_PHP, running && !state.php.is_empty()),
        php_versions,
        autostart: Item::check(autostart_label, enabled, checked),
        power,
        quit: Item::text(strings::TRAY_QUIT, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        text.parse().unwrap()
    }

    /// A running daemon with these PHP versions, and autostart off.
    fn running_with(versions: &[&str], default: Option<&str>) -> State {
        let mut state = State::default();
        state.apply(Update::Daemon(Daemon::Running));
        state.apply(Update::Php {
            installed: versions.iter().map(|text| v(text)).collect(),
            default: default.map(str::to_owned),
        });
        state.apply(Update::Autostart(Autostart::Off));
        state
    }

    fn labels(layout: &Layout) -> Vec<String> {
        layout
            .lines()
            .iter()
            .map(|item| item.label.clone())
            .collect()
    }

    fn version_checks(layout: &Layout) -> Vec<bool> {
        layout
            .php_versions
            .iter()
            .map(|item| item.checked == Some(true))
            .collect()
    }

    #[test]
    fn at_the_start_the_menu_waits_for_the_daemon() {
        let menu = layout(&State::default());
        assert_eq!(
            labels(&menu),
            [
                "Starting Marsa…",
                "Open Marsa",
                "PHP",
                "Start at Login",
                "Stop Marsa",
                "Quit",
            ]
        );
        assert!(!menu.status.enabled);
        assert!(menu.notice.is_none());
        assert!(menu.open.enabled);
        assert!(!menu.php.enabled);
        assert_eq!(menu.autostart.checked, Some(false));
        assert!(!menu.autostart.enabled);
        assert!(!menu.power.enabled, "nothing to stop yet");
        assert!(menu.quit.enabled);
    }

    #[test]
    fn a_running_daemon_lists_php_with_the_default_checked() {
        let state = running_with(&["8.3.27", "8.4.23"], Some("8.4"));
        let menu = layout(&state);
        assert_eq!(menu.status.label, "Marsa is running");
        assert!(menu.php.enabled);
        let versions: Vec<&str> = menu
            .php_versions
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(versions, ["8.3.27", "8.4.23"]);
        assert_eq!(version_checks(&menu), [false, true]);
        assert!(menu.php_versions.iter().all(|item| item.enabled));
        assert_eq!(menu.autostart.checked, Some(false));
        assert!(menu.autostart.enabled);
        assert_eq!(menu.power.label, "Stop Marsa");
        assert!(menu.power.enabled);
        // The submenu's lines come right after its own line.
        assert_eq!(labels(&menu)[2..5], ["PHP", "8.3.27", "8.4.23"]);
    }

    #[test]
    fn a_default_that_matches_nothing_checks_no_version() {
        let menu = layout(&running_with(&["8.3.27", "8.4.23"], Some("8.2")));
        assert_eq!(version_checks(&menu), [false, false]);
        let menu = layout(&running_with(&["8.3.27"], None));
        assert_eq!(version_checks(&menu), [false]);
    }

    #[test]
    fn without_php_the_submenu_is_disabled() {
        let menu = layout(&running_with(&[], None));
        assert!(!menu.php.enabled);
        assert!(menu.php_versions.is_empty());
    }

    #[test]
    fn stop_then_start_again() {
        let mut state = running_with(&["8.4.23"], Some("8.4"));
        assert_eq!(state.power(), Some(Command::Stop));
        let menu = layout(&state);
        assert_eq!(menu.status.label, "Stopping Marsa…");
        assert_eq!(menu.power.label, "Start Marsa");
        assert!(!menu.power.enabled);
        assert!(!menu.php.enabled);
        assert!(!menu.php_versions[0].enabled);
        assert!(!menu.autostart.enabled);
        assert_eq!(state.power(), None, "already stopping");

        state.apply(Update::Daemon(Daemon::Stopped));
        let menu = layout(&state);
        assert_eq!(menu.status.label, "Marsa is stopped");
        assert!(menu.power.enabled);
        assert_eq!(menu.php_versions[0].label, "8.4.23", "the list is kept");
        assert!(!menu.php_versions[0].enabled);

        assert_eq!(state.power(), Some(Command::Start));
        let menu = layout(&state);
        assert_eq!(menu.status.label, "Starting Marsa…");
        assert_eq!(menu.power.label, "Stop Marsa");
        assert!(!menu.power.enabled);
        assert_eq!(state.power(), None, "already starting");
    }

    #[test]
    fn a_failed_start_shows_the_error_by_its_code() {
        let mut state = State::default();
        state.apply(Update::Daemon(Daemon::Failed("daemon_start_failed".into())));
        let menu = layout(&state);
        assert_eq!(menu.status.label, "Cannot start the daemon (marsad).");
        assert_eq!(menu.power.label, "Start Marsa");
        assert!(menu.power.enabled);
        assert_eq!(state.power(), Some(Command::Start));
    }

    #[test]
    fn the_start_at_login_line() {
        let cases = [
            (Autostart::Unknown, "Start at Login", false, false),
            (Autostart::Off, "Start at Login", false, true),
            (Autostart::On, "Start at Login", true, true),
            (
                Autostart::Stale,
                "Start at Login (outdated, click to fix)",
                false,
                true,
            ),
            (Autostart::Unsupported, "Start at Login", false, false),
        ];
        for (autostart, label, checked, enabled) in cases {
            let mut state = running_with(&[], None);
            state.apply(Update::Autostart(autostart));
            let item = layout(&state).autostart;
            assert_eq!(item.label, label, "{autostart:?}");
            assert_eq!(item.checked, Some(checked), "{autostart:?}");
            assert_eq!(item.enabled, enabled, "{autostart:?}");
        }
        // The daemon's answer.
        let on = AutostartState {
            enabled: true,
            stale: false,
        };
        assert_eq!(Autostart::from(on), Autostart::On);
        let stale = AutostartState {
            enabled: false,
            stale: true,
        };
        assert_eq!(Autostart::from(stale), Autostart::Stale);
        assert_eq!(Autostart::from(AutostartState::default()), Autostart::Off);
    }

    #[test]
    fn a_click_on_start_at_login_flips_it_and_asks_the_daemon() {
        let mut state = running_with(&[], None);
        assert_eq!(state.toggle_autostart(), Some(Command::SetAutostart(true)));
        assert_eq!(state.autostart, Autostart::On);
        assert_eq!(layout(&state).autostart.checked, Some(true));
        assert_eq!(state.toggle_autostart(), Some(Command::SetAutostart(false)));
        assert_eq!(state.autostart, Autostart::Off);

        state.apply(Update::Autostart(Autostart::Stale));
        assert_eq!(state.toggle_autostart(), Some(Command::SetAutostart(true)));
        assert_eq!(state.autostart, Autostart::On);

        for disabled in [Autostart::Unknown, Autostart::Unsupported] {
            state.apply(Update::Autostart(disabled));
            assert_eq!(state.toggle_autostart(), None, "{disabled:?}");
        }

        state.apply(Update::Autostart(Autostart::Off));
        state.apply(Update::Daemon(Daemon::Stopped));
        assert_eq!(state.toggle_autostart(), None, "the daemon is stopped");
    }

    #[test]
    fn picking_a_version_checks_it_at_once_and_sends_its_line() {
        let mut state = running_with(&["8.3.27", "8.4.23"], Some("8.4"));
        assert_eq!(state.pick_php(0), Some(Command::SetPhp("8.3".into())));
        assert_eq!(version_checks(&layout(&state)), [true, false]);
        assert_eq!(state.pick_php(0), None, "the default already");
        assert_eq!(state.pick_php(5), None, "no such line");

        // An older patch release of a line is sent in full, so it stays
        // the one used.
        let mut state = running_with(&["8.4.20", "8.4.23"], Some("8.4"));
        assert_eq!(state.pick_php(0), Some(Command::SetPhp("8.4.20".into())));
        assert_eq!(version_checks(&layout(&state)), [true, false]);

        state.apply(Update::Daemon(Daemon::Stopped));
        assert_eq!(state.pick_php(1), None, "the daemon is stopped");
    }

    #[test]
    fn the_default_changed_event_moves_the_check() {
        let mut state = running_with(&["8.3.27", "8.4.23"], Some("8.4"));
        state.apply(Update::PhpDefault("8.3".into()));
        assert_eq!(version_checks(&layout(&state)), [true, false]);
        // A new list after an install, with a default set by it.
        state.apply(Update::Php {
            installed: vec![v("8.3.27"), v("8.4.23"), v("8.5.1")],
            default: Some("8.5".into()),
        });
        assert_eq!(version_checks(&layout(&state)), [false, false, true]);
    }

    #[test]
    fn a_failed_action_shows_a_notice_until_the_next_click() {
        let mut state = running_with(&["8.4.23"], Some("8.4"));
        state.apply(Update::Failed("tray_not_found".into()));
        let menu = layout(&state);
        let notice = menu.notice.as_ref().expect("a notice line");
        assert_eq!(
            notice.label,
            "marsa-tray was not found next to marsad, so Marsa cannot start at login."
        );
        assert!(!notice.enabled);
        assert_eq!(
            labels(&menu)[..2],
            [menu.status.label.clone(), notice.label.clone()]
        );

        // The state read again after the failure does not hide the notice.
        state.apply(Update::Autostart(Autostart::Off));
        assert!(layout(&state).notice.is_some());
        // The next click does.
        assert!(state.toggle_autostart().is_some());
        assert!(layout(&state).notice.is_none());

        state.apply(Update::Failed("autostart_write".into()));
        assert_eq!(
            layout(&state).notice.map(|item| item.label),
            Some("Cannot write the login file.".to_owned())
        );
        // So does a change of the daemon.
        state.apply(Update::Daemon(Daemon::Stopped));
        assert!(layout(&state).notice.is_none());
    }
}
