//! The menu-bar icon and the menu-bar panel.

// The menu-bar icon (`marsa-tray`).

/// The tooltip of the icon.
pub const TRAY_TOOLTIP: &str = "Marsa";
pub const TRAY_OPEN: &str = "Open Marsa";
/// The PHP submenu: the installed versions, the default checked.
pub const TRAY_PHP: &str = "PHP";
pub const TRAY_AUTOSTART: &str = "Start at Login";
/// A login file is there, but it starts another `marsa-tray` (the app
/// moved): a click writes it again.
pub const TRAY_AUTOSTART_STALE: &str = "Start at Login (outdated, click to fix)";
/// The line of the menu where the sites would be.
pub const TRAY_NO_SITES: &str = "No sites yet";
pub const TRAY_STOP: &str = "Stop Marsa";
pub const TRAY_START: &str = "Start Marsa";
/// Closes the icon only; the daemon and the sites keep running.
pub const TRAY_QUIT: &str = "Quit";
/// The icon's own code: its background thread could not start.
pub const TRAY_WORKER_FAILED: &str = "tray_worker_failed";
/// The icon's own code: `marsa-gui` could not be started.
pub const WINDOW_START_FAILED: &str = "window_start_failed";

// The menu-bar panel (`marsa-gui --panel`). "Open Marsa", "Stop
// Marsa", "Start Marsa", and "Quit" are the icon's own words above.

/// The name of the panel for a screen reader, and the title of its window.
pub const PANEL_NAME: &str = "Marsa panel";
/// The name of the list of sites, for a screen reader.
pub const PANEL_SITES: &str = "Sites";
/// Under the name of the Xdebug switch.
pub const PANEL_XDEBUG_CAPTION: &str = "PHP runs slower while it is on";
/// Where the list of sites has none (the status above already says "No sites
/// yet"): what to do about it.
pub const PANEL_NO_SITES: &str = "Add a site in Marsa.";
/// Under the pointer on "Quit": it closes the icon, not Marsa.
pub const PANEL_QUIT_HINT: &str = "Closes this icon. Sites keep working.";
/// Why "Stop Marsa" cannot be pressed: the daemon is not there yet.
pub const PANEL_NOT_CONNECTED: &str = "Marsa is not connected yet.";
/// Why "Stop Marsa" cannot be pressed: it is stopping already.
pub const PANEL_ALREADY_STOPPING: &str = "Marsa is stopping already.";

/// A site's caption in the panel: `minor` is `8.4`.
pub fn panel_php(minor: &str) -> String {
    format!("PHP {minor}")
}

/// What a screen reader says for a site of the panel, and what the key does.
pub fn panel_open_site(name: &str) -> String {
    format!("Open {name}")
}

/// Under the nine sites of the panel, when there are more: the window has
/// them all.
pub fn panel_more_sites(more: usize) -> String {
    match more {
        1 => "1 more site in Marsa".to_owned(),
        n => format!("{n} more sites in Marsa"),
    }
}

/// The first line of the menu: the daemon's state.
pub const TRAY_STATUS_STARTING: &str = "Starting Marsa…";
pub const TRAY_STATUS_RUNNING: &str = "Marsa is running";
pub const TRAY_STATUS_STOPPING: &str = "Stopping Marsa…";
pub const TRAY_STATUS_STOPPED: &str = "Marsa is stopped";
