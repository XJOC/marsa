//! Services and their logs: names, descriptions, and state words.

use marsa_core::types::ServiceAction;

use crate::logline::LogLevel;

use super::*;

pub const SERVICES: &str = "Services";
/// After the title of Services: what the screen is for.
pub const SERVICES_HINT: &str = "Programs Marsa runs for your sites";
/// The Services screen while the window has no daemon to ask.
pub const SERVICES_OFFLINE: &str = "Your services show here once Marsa is connected to its daemon.";
/// The Services screen when the daemon lists no program.
pub const NO_SERVICES: &str = "No services yet. A program shows here once Marsa starts it.";
/// Under the Services table, while a PHP pool is listed.
pub const SERVICES_POOLS_NOTE: &str = "PHP pools start and stop with the sites that need them.";
/// The names of the columns, for a screen reader (the table has no header).
pub const COLUMN_SERVICE: &str = "Service";
pub const COLUMN_STATE: &str = "State";
pub const COLUMN_ACTIONS: &str = "Actions";

pub const LOGS: &str = "Logs";
/// The name of the control that picks whose log is shown.
pub const LOG_SOURCE: &str = "Log source";
/// The name of the filter field of Logs for a screen reader, and its
/// placeholder.
pub const FILTER_LINES: &str = "Filter lines";
pub const FOLLOW: &str = "Follow";
/// Over the Follow chip.
pub const FOLLOW_HINT: &str = "Keeps the newest line in view. Scrolling up stops it.";
/// The Logs screen while the window has no daemon to ask and no log kept.
pub const LOGS_OFFLINE: &str = "Logs show here once Marsa is connected to its daemon.";
/// The Logs screen when the filters leave no line.
pub const LOG_NO_MATCH: &str = "No lines match.";

/// The Logs screen when the program has written nothing yet.
pub fn log_empty(name: &str) -> String {
    format!("{name} has not written anything yet.")
}

/// A Laravel site's own log among the sources of Logs: "Laravel · shop".
pub fn site_log_source(site: &str) -> String {
    format!("Laravel · {site}")
}

/// The Logs screen on a Laravel site's log while the site has no log file.
pub const SITE_LOG_EMPTY: &str = "No log yet. It appears when the site writes one.";

/// A level in the filter chips.
pub const fn log_level_name(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Info => "Info",
        LogLevel::Warning => "Warning",
        LogLevel::Error => "Error",
    }
}

/// A level as the word at the start of a line, in the mono face.
pub const fn log_level_word(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Info => "INFO",
        LogLevel::Warning => "WARN",
        LogLevel::Error => "ERROR",
    }
}

/// What a screen reader says for a line: its level, then the message.
pub fn log_entry_label(level: LogLevel, message: &str) -> String {
    format!("{}: {message}", log_level_name(level))
}

/// The button that opens the list of logs, when there are too many for the
/// segmented control. It opens a menu, so it ends with "…".
pub fn log_source_menu(name: &str) -> String {
    format!("{name}…")
}

/// What a service is called: Caddy, DNS, or a PHP pool by its line.
pub const SERVICE_CADDY: &str = "Caddy";
pub const SERVICE_DNS: &str = "DNS";

pub fn service_php_fpm(line: &str) -> String {
    format!("PHP-FPM {line}")
}

pub fn service_php_cgi(line: &str) -> String {
    format!("PHP-CGI {line}")
}

/// What a service does, in one line under its name.
pub const DESCRIPTION_CADDY: &str = "The web server for every .test site.";
pub const DESCRIPTION_DNS: &str = "Finds every .test site on this computer.";
/// A service this window does not know.
pub const DESCRIPTION_OTHER: &str = "A program Marsa runs for your sites.";
/// A pool that no site needs now.
pub const DESCRIPTION_POOL_WAITING: &str = "No site needs it now. It starts when one does.";

/// A pool that runs: `sites` is how many sites run its PHP `line`.
pub fn description_pool(line: &str, sites: usize) -> String {
    match sites {
        0 => format!("Runs PHP {line} for your sites."),
        1 => format!("Runs PHP {line} for 1 site."),
        n => format!("Runs PHP {line} for {n} sites."),
    }
}

/// The tooltip of a state: the process id, while there is one.
pub fn service_pid(pid: u32) -> String {
    format!("Process {pid}")
}

/// What a screen reader says for the "Show log" of a service.
pub fn service_show_log_label(name: &str) -> String {
    format!("{SHOW_LOG} of {name}")
}

/// What a screen reader says for a service's button: "Restart Caddy".
pub fn service_action_label(action: ServiceAction, name: &str) -> String {
    format!("{} {name}", action_text(action))
}

pub fn action_text(action: ServiceAction) -> &'static str {
    match action {
        ServiceAction::Start => "Start",
        ServiceAction::Stop => "Stop",
        ServiceAction::Restart => "Restart",
    }
}

/// The words of a service's state pill (components.md, Status).
pub const PILL_RUNNING: &str = "Running";
pub const PILL_WAITING: &str = "Waiting";
pub const PILL_STOPPED: &str = "Stopped";
pub const PILL_STARTING: &str = "Starting";
pub const PILL_STOPPING: &str = "Stopping";
pub const PILL_FAILED: &str = "Failed";
/// A site's worker whose switch is off.
pub const PILL_OFF: &str = "Off";
