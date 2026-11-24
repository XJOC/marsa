//! Every text the window shows, in one place (AGENTS.md section 9), so a
//! second language can be added later without touching the drawing code.
//! Errors are shown by their stable code, never by their English message.
//!
//! One file per domain; this module re-exports them all, so a screen writes
//! `strings::NAME` whatever file the text lives in.

mod errors;
mod home;
mod mail;
mod runtimes;
mod services;
mod setup;
mod sites;
mod tray;

#[cfg(test)]
pub(crate) mod tests;

pub use errors::*;
pub use home::*;
pub use mail::*;
pub use runtimes::*;
pub use services::*;
pub use setup::*;
pub use sites::*;
pub use tray::*;

use crate::theme::Section;

pub const APP_NAME: &str = "Marsa";

/// The line that says how the window and the daemon are connected.
pub const CONNECTING: &str = "Connecting…";
pub const RECONNECTING: &str = "Reconnecting…";
pub const DAEMON_SHUTTING_DOWN: &str = "Daemon: shutting down";

pub fn daemon_running(version: &str, pid: u32) -> String {
    format!("Daemon: running (v{version}, PID {pid})")
}

/// A connection that failed with `code`.
pub fn connection_failed(code: &str) -> String {
    // The bridge is gone: nothing tries again.
    if code == BRIDGE_FAILED {
        error_text(code)
    } else {
        format!("{} Retrying…", error_text(code))
    }
}

/// A section's name: its sidebar label, and what a screen reader says for
/// its tile.
pub const fn section_name(section: Section) -> &'static str {
    match section {
        Section::Home => "Home",
        Section::Sites => SITES,
        Section::Runtimes => "Runtimes",
        Section::Services => SERVICES,
        Section::Mail => "Mail",
        Section::Dumps => "Dumps",
        Section::Logs => LOGS,
        Section::Databases => "Databases",
        Section::Queues => "Queues",
    }
}

/// The sidebar's fold button, by what a press does.
pub const HIDE_LABELS: &str = "Hide labels";
pub const SHOW_LABELS: &str = "Show labels";
/// Why the fold button does nothing in a narrow window.
pub const FOLD_NARROW_HINT: &str =
    "The window is too narrow for labels. Make it wider to show them.";

/// The status at the bottom of the sidebar: a title and a caption, short
/// enough for its width.
pub const OVERALL_CONNECTING: (&str, &str) = ("Connecting…", "Waiting for Marsa");
pub const OVERALL_STOPPING: (&str, &str) = ("Marsa is stopping", "Sites stop with it");
pub const OVERALL_DAEMON_DOWN: (&str, &str) = ("Sites not opening", "Marsa is not running");
pub const OVERALL_NO_SITES: (&str, &str) = ("Marsa is running", "No sites yet");
pub const OVERALL_STARTING: (&str, &str) = ("Sites starting", "Caddy is starting");
pub const OVERALL_WEB_SERVER_DOWN: (&str, &str) = ("Sites not opening", "Caddy stopped");
pub const OVERALL_WORKING: (&str, &str) = ("All sites working", "Caddy is running");

/// What a screen reader says for a row badge: "3 new mails", "9 dumps".
pub fn row_badge_label(section: Section, count: u32) -> String {
    let (one, many) = match section {
        Section::Home => ("event", "events"),
        Section::Sites => ("site", "sites"),
        Section::Runtimes => ("runtime", "runtimes"),
        Section::Services => ("service", "services"),
        Section::Mail => ("new mail", "new mails"),
        Section::Dumps => ("dump", "dumps"),
        Section::Logs => ("log line", "log lines"),
        Section::Databases => ("database", "databases"),
        Section::Queues => ("job", "jobs"),
    };
    format!("{count} {}", if count == 1 { one } else { many })
}
