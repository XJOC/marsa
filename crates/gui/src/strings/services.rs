//! Services and their logs: names, descriptions, and state words.

use marsa_core::types::ServiceAction;

use crate::model::{DNS_SERVER, LogLevel, PoolKind, WEB_SERVER, pool_of};
use crate::widgets::PillState;

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

/// What a service is called: `caddy` is Caddy, `php-fpm-8.4` is PHP-FPM 8.4.
/// A service this window does not know shows under its id.
pub fn service_name(id: &str) -> String {
    if id == WEB_SERVER {
        return "Caddy".to_owned();
    }
    if id == DNS_SERVER {
        return "DNS".to_owned();
    }
    match pool_of(id) {
        Some((PoolKind::Fpm, line)) => format!("PHP-FPM {line}"),
        Some((PoolKind::Cgi, line)) => format!("PHP-CGI {line}"),
        None => id.to_owned(),
    }
}

/// What a service does, in one line under its name. `waiting`: a pool that
/// no site needs now. `sites`: how many sites run the PHP line of a pool.
pub fn service_description(id: &str, waiting: bool, sites: usize) -> String {
    if id == WEB_SERVER {
        return "The web server for every .test site.".to_owned();
    }
    if id == DNS_SERVER {
        return "Finds every .test site on this computer.".to_owned();
    }
    let Some((_, line)) = pool_of(id) else {
        return "A program Marsa runs for your sites.".to_owned();
    };
    match (waiting, sites) {
        (true, _) => "No site needs it now. It starts when one does.".to_owned(),
        (false, 0) => format!("Runs PHP {line} for your sites."),
        (false, 1) => format!("Runs PHP {line} for 1 site."),
        (false, n) => format!("Runs PHP {line} for {n} sites."),
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

/// The word of a service's state pill (components.md, Status).
pub const fn pill_state_word(state: PillState) -> &'static str {
    match state {
        PillState::Running => "Running",
        PillState::Waiting => "Waiting",
        PillState::Stopped => "Stopped",
        PillState::Starting => "Starting",
        PillState::Stopping => "Stopping",
        PillState::Failed => "Failed",
    }
}
