//! The Dumps screen: the `dump()` calls of PHP sites that `marsad` caught,
//! and the switch that sends them here.

use super::mail_age;

/// Under the header of Dumps: where the dumps come from.
pub const DUMPS_LEAD: &str =
    "dump() in any PHP site of Marsa shows here, with its file, line, and URL.";
/// The switch in the header. On while the dump server is on.
pub const CAPTURE: &str = "Capture";
/// Over "Capture" while the daemon cannot be asked.
pub const CAPTURE_OFFLINE_HINT: &str = "Marsa is not connected to its daemon.";
/// Puts the shown dump's text on the clipboard.
pub const COPY_DUMP: &str = "Copy";
/// No dialog: dumps live in memory and come back with the next dump().
pub const CLEAR_DUMPS: &str = "Clear";
/// Over "Clear" when there is nothing to clear.
pub const NO_DUMPS_TO_CLEAR: &str = "There are no dumps to clear.";

/// The Dumps screen while the window has no daemon to ask.
pub const DUMPS_OFFLINE: &str = "Dumps show here once Marsa is connected to its daemon.";
/// The empty state, and what shows here: the fix is in the code, so there
/// is no button.
pub const NO_DUMPS: &str = "No dumps yet.";
pub const NO_DUMPS_HOW: &str = "dump() in any site of Marsa shows here. dd() shows here too, and leaves the page empty (HTTP 500). dump() in artisan, tinker, or a test stays in the terminal.";
/// The state while the capture is off.
pub const DUMPS_OFF: &str = "Capture is off, so dump() shows in the page.";
pub const DUMPS_OFF_HOW: &str =
    "Turn it on to see the dumps of your sites here, with their file and line.";
pub const TURN_ON_CAPTURE: &str = "Turn on capture";
/// The state while another program holds the dump server's port.
pub const DUMPS_BUSY: &str = "Another program uses port 9912, so dump() shows in the page.";
pub const DUMPS_BUSY_HOW: &str =
    "If it is \"php artisan dump-server\", stop it, then turn capture off and on again.";

/// A dump that carries no file, for the row's first line.
pub const DUMP_NO_PLACE: &str = "dump()";
/// The labels of the facts of the shown dump.
pub const DUMP_FILE: &str = "File";
pub const DUMP_URL: &str = "URL";
pub const DUMP_LABEL: &str = "Label";
pub const DUMP_SENT: &str = "Sent";
/// The text of a dump cut at its limit ends with this line.
pub const DUMP_CUT: &str = "This dump was longer than Marsa keeps, so it ends here.";
/// Under the last row once the list holds as many as the daemon keeps.
pub const DUMPS_KEPT: &str = "The newest 500 are kept.";

/// How long ago a dump came: "just now", "4 min ago", "2 h ago".
pub fn dump_age(at_ms: u64, now_ms: u64) -> String {
    mail_age(at_ms, now_ms)
}

/// What a screen reader says after a dump's place: whether it is new, its
/// second line, and how old it is.
pub fn dump_row_description(second: &str, age: &str, new: bool) -> String {
    super::mail_row_description(second, age, new)
}

/// The last two parts of a path, so a row shows `routes/web.php` and not
/// the whole folder: the facts of the detail have the whole path. A path
/// with one part, or none, is shown as it is.
pub fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect();
    match parts.as_slice() {
        [.., folder, file] => format!("{folder}/{file}"),
        _ => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_cut_to_its_folder_and_file() {
        assert_eq!(
            short_path("/Users/omar/Code/shop/routes/web.php"),
            "routes/web.php"
        );
        assert_eq!(
            short_path("C:\\code\\shop\\app\\Http\\Foo.php"),
            "Http/Foo.php"
        );
        assert_eq!(short_path("web.php"), "web.php");
        assert_eq!(short_path("/web.php"), "/web.php");
        assert_eq!(short_path(""), "");
    }

    #[test]
    fn the_screen_names_the_port_the_server_uses() {
        let port = marsa_core::dumps::DUMPS_PORT.to_string();
        assert!(DUMPS_BUSY.contains(&port));
    }
}
