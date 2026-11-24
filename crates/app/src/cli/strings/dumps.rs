//! The text of `marsa dumps`.

use marsa_core::types::DumpsStatus;

/// `marsa dumps` with no dump: what shows here, and what does not.
pub const DUMPS_NONE: &str = "No dumps yet. dump() in any PHP site of Marsa shows here, with its file and line. \
     dd() shows here too, and leaves the page empty (HTTP 500). dump() in \"php artisan\", \
     tinker, or a test stays in the terminal: run that shell with VAR_DUMPER_FORMAT=server \
     to send it here as well.";

/// `marsa dumps` while the capture is off.
pub const DUMPS_OFF: &str =
    "Dump capture is off, so dump() shows in the page. Run \"marsa dumps on\" to turn it on.";

/// The listener cannot take port 9912.
pub const DUMPS_PORT_BUSY: &str = "Another program uses port 9912 (for example \"php artisan dump-server\"), so dump() \
     shows in the page. Stop that program, then run \"marsa dumps on\".";

/// `marsa dumps clear`.
pub const DUMPS_CLEARED: &str = "Deleted every dump.\n";

/// `marsa dumps on`, with the listener up.
pub const DUMPS_NOW_ON: &str =
    "Dump capture is on. dump() in a PHP site shows in \"marsa dumps\" now.\n";

/// `marsa dumps off`.
pub const DUMPS_NOW_OFF: &str = "Dump capture is off. dump() shows in the page again.\n";

/// After a dump the window cut at its limit.
pub const DUMP_CUT: &str = "(This dump was cut: it was longer than Marsa keeps.)";

/// On stderr, when `--follow` starts.
pub const DUMPS_FOLLOWING: &str = "Showing new dumps as they come. Ctrl+C stops.";

/// On stderr, when the daemon closes the stream.
pub const DUMPS_ENDED: &str = "The daemon stopped, so no more dumps come.";

/// The line above the list.
pub fn dumps_count(listed: usize, latest: u64) -> String {
    let dumps = if listed == 1 { "dump" } else { "dumps" };
    format!("The last {listed} {dumps} (the newest is #{latest}):")
}

/// What to say instead of the list while the listener is not up, if
/// anything.
pub fn dumps_status_note(status: DumpsStatus) -> Option<&'static str> {
    match status {
        DumpsStatus::Listening => None,
        DumpsStatus::PortInUse => Some(DUMPS_PORT_BUSY),
        DumpsStatus::Disabled => Some(DUMPS_OFF),
    }
}
