//! Every text the window shows, in one place (AGENTS.md section 9), so a
//! second language can be added later without touching the drawing code.
//! Errors are shown by their stable code, never by their English message.
//!
//! One file per domain; this module re-exports them all, so a screen writes
//! `strings::NAME` whatever file the text lives in.

mod errors;
mod home;
mod runtimes;
mod services;
mod setup;
mod sites;
mod tray;

#[cfg(test)]
mod tests;

pub use errors::*;
pub use home::*;
pub use runtimes::*;
pub use services::*;
pub use setup::*;
pub use sites::*;
pub use tray::*;

use crate::model::{Connection, Overall};
use crate::theme::Section;

pub const APP_NAME: &str = "Marsa";

pub fn status_line(connection: &Connection) -> String {
    match connection {
        Connection::Connecting => "Connecting…".to_owned(),
        Connection::Reconnecting => "Reconnecting…".to_owned(),
        Connection::Running(health) => format!(
            "Daemon: running (v{}, PID {})",
            health.app_version, health.pid
        ),
        Connection::ShuttingDown => "Daemon: shutting down".to_owned(),
        // The bridge is gone: nothing tries again.
        Connection::Failed(code) if code == BRIDGE_FAILED => error_text(code),
        Connection::Failed(code) => format!("{} Retrying…", error_text(code)),
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
pub const fn overall_text(overall: Overall) -> (&'static str, &'static str) {
    match overall {
        Overall::Connecting => ("Connecting…", "Waiting for Marsa"),
        Overall::Stopping => ("Marsa is stopping", "Sites stop with it"),
        Overall::DaemonDown => ("Sites not opening", "Marsa is not running"),
        Overall::NoSites => ("Marsa is running", "No sites yet"),
        Overall::Starting => ("Sites starting", "Caddy is starting"),
        Overall::WebServerDown => ("Sites not opening", "Caddy stopped"),
        Overall::Working => ("All sites working", "Caddy is running"),
    }
}

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
