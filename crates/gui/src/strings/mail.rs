//! The Mail screen: the mail Mailpit caught, how old each one is, and the
//! confirmation of "Delete all…".

/// Under the header of Mail: where the mail comes from.
pub const MAIL_LEAD: &str = "Mail your projects send to 127.0.0.1:2525 stays on this computer.";
pub const OPEN_MAILPIT: &str = "Open Mailpit";
/// Ends with "…": it opens a confirmation.
pub const DELETE_ALL_MAIL: &str = "Delete all…";
/// Over "Delete all…" when the list is empty, or not read.
pub const NO_MAIL_TO_DELETE: &str = "There is no mail to delete.";
/// Over "Delete all…" while another removal waits for its answer.
pub const DELETE_MAIL_BUSY: &str = "Wait until the last removal ends.";
/// Over "Open Mailpit" while Mailpit does not run.
pub const MAILPIT_NOT_RUNNING_HINT: &str = "Mailpit is not running. Start it to open its page.";

/// The Mail screen while the window has no daemon to ask.
pub const MAIL_OFFLINE: &str = "Mail shows here once Marsa is connected to its daemon.";
/// The empty state, and how to fill it: the fix is in the project, so there
/// is no button.
pub const NO_MAIL: &str = "No mail yet.";
pub const NO_MAIL_HOW: &str = "In a Laravel project, set MAIL_MAILER=smtp in its .env. Laravel then sends to 127.0.0.1:2525, and the mail shows up here.";
/// The state while Mailpit does not run.
pub const MAILPIT_STOPPED: &str = "Mailpit is not running.";
pub const MAILPIT_STOPPED_HOW: &str =
    "Mailpit catches the mail your projects send. Start it to see the mail here.";
pub const START_MAILPIT: &str = "Start Mailpit";
/// Over "Start Mailpit" while its service changes.
pub const MAILPIT_STARTING: &str = "Mailpit is starting.";
pub const MAILPIT_STOPPING: &str = "Mailpit is stopping. Start it again once it has stopped.";

/// The detail card before a mail is chosen.
pub const CHOOSE_MAIL: &str = "Choose a mail to read it.";
/// After a mail that is not opened yet, beside its dot: the word, so the
/// color is never the only clue.
pub const MAIL_NEW: &str = "New";
/// The labels of the facts of the chosen mail.
pub const MAIL_FROM: &str = "From";
pub const MAIL_TO: &str = "To";
pub const MAIL_SENT: &str = "Sent";
/// A mail with an empty subject.
pub const NO_SUBJECT: &str = "(no subject)";
/// A mail with HTML only. "Open in browser" under it opens the HTML.
pub const MAIL_NO_TEXT: &str = "(This mail has no plain text. Open it in the browser to read it.)";

/// The confirmation of "Delete all…".
pub const DELETE_MAIL_TITLE: &str = "Delete every mail?";
pub const DELETE_MAIL_TEXT: &str =
    "Mailpit deletes all the mail it caught, also the mail not listed here. This cannot be undone.";
pub const DELETE_MAIL_CONFIRM: &str = "Delete all";
/// While the daemon deletes the mail.
pub const DELETING_MAIL: &str = "Deleting every mail…";

/// Under the last row when Mailpit keeps more mail than the list holds.
pub fn mail_more(listed: usize, total: u64) -> String {
    format!("The newest {listed} of {total}. Older mail is in Mailpit.")
}

/// What a screen reader says after a mail's sender: whether it is new, its
/// subject, and how old it is.
pub fn mail_row_description(subject: &str, age: &str, new: bool) -> String {
    if new {
        format!("{MAIL_NEW}, {subject}, {age}")
    } else {
        format!("{subject}, {age}")
    }
}

/// How long before `now_ms` the time `at_ms` was, both in milliseconds since
/// the Unix epoch: "just now", "4 min ago", "2 h ago", "3 d ago". A time
/// after `now_ms` (a clock that runs ahead) is "just now".
pub fn mail_age(at_ms: u64, now_ms: u64) -> String {
    const MINUTE: u64 = 60_000;
    const HOUR: u64 = 60;
    const DAY: u64 = 24 * HOUR;
    let minutes = now_ms.saturating_sub(at_ms) / MINUTE;
    match minutes {
        0 => "just now".to_owned(),
        1..HOUR => format!("{minutes} min ago"),
        HOUR..DAY => format!("{} h ago", minutes / HOUR),
        _ => format!("{} d ago", minutes / DAY),
    }
}

#[cfg(test)]
mod tests {
    use marsa_core::databases::mailpit::MAILPIT_PORT;

    use super::*;

    #[test]
    fn the_age_of_a_mail_is_said_in_the_largest_whole_unit() {
        let now = 1_800_000_000_000;
        let ago = |ms: u64| mail_age(now - ms, now);
        assert_eq!(ago(0), "just now");
        assert_eq!(ago(59_999), "just now");
        assert_eq!(ago(60_000), "1 min ago");
        assert_eq!(ago(4 * 60_000 + 30_000), "4 min ago");
        assert_eq!(ago(59 * 60_000), "59 min ago");
        assert_eq!(ago(60 * 60_000), "1 h ago");
        assert_eq!(ago(23 * 3_600_000 + 59 * 60_000), "23 h ago");
        assert_eq!(ago(24 * 3_600_000), "1 d ago");
        assert_eq!(ago(40 * 24 * 3_600_000), "40 d ago");
        // A mail from a clock that runs ahead, and the epoch itself.
        assert_eq!(mail_age(now + 5_000, now), "just now");
        assert_eq!(mail_age(0, 0), "just now");
    }

    #[test]
    fn the_lines_that_name_the_smtp_address_name_mailpits_port() {
        let address = format!("127.0.0.1:{MAILPIT_PORT}");
        assert!(MAIL_LEAD.contains(&address));
        assert!(NO_MAIL_HOW.contains(&address));
    }

    #[test]
    fn a_new_mail_says_new_in_words_and_the_cut_list_says_how_many() {
        assert_eq!(
            mail_row_description("Reset", "just now", true),
            "New, Reset, just now"
        );
        assert_eq!(
            mail_row_description("Reset", "2 h ago", false),
            "Reset, 2 h ago"
        );
        assert_eq!(
            mail_more(200, 214),
            "The newest 200 of 214. Older mail is in Mailpit."
        );
    }
}
