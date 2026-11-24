//! The text of `marsa mail`.

/// The titles of the columns of `marsa mail`. The last column, with no
/// title, marks a mail not read yet.
pub const MAIL_HEADER: [&str; 5] = ["ID", "AGE", "FROM", "SUBJECT", ""];

/// The mark of a mail not read yet.
pub const MAIL_NEW: &str = "new";

/// `marsa mail` with no mail: how a Laravel project sends some.
pub const MAIL_NONE: &str = "No mail yet. In a Laravel project, set MAIL_MAILER=smtp in its .env: \
     Laravel then sends to 127.0.0.1:2525, where Mailpit keeps every mail on this computer.";

/// The line under the list.
pub const MAIL_READ_HINT: &str =
    "Read one with \"marsa mail show <ID>\", or all of them at http://127.0.0.1:8025.";

/// `marsa mail clear`.
pub const MAIL_CLEARED: &str = "Deleted every mail.\n";

/// `marsa mail show` for a mail with HTML only.
pub const MAIL_NO_TEXT: &str = "(This mail has no plain text. Open its page to read it.)";

/// Before the address of a mail's page.
pub const MAIL_PAGE: &str = "HTML and attachments:";

/// The line above the list: how many mails, how many new, and how many
/// listed.
pub fn mail_count(total: u64, unread: u64, listed: usize) -> String {
    let mails = if total == 1 { "mail" } else { "mails" };
    format!("{total} {mails}, {unread} new. The newest {listed}:")
}

/// How long ago, from `millis`: `just now`, `5 min ago`, `3 h ago`, or
/// `2 d ago`.
pub fn age(millis: u64) -> String {
    let minutes = millis / 60_000;
    match minutes {
        0 => "just now".to_owned(),
        1..=59 => format!("{minutes} min ago"),
        60..=1439 => format!("{} h ago", minutes / 60),
        _ => format!("{} d ago", minutes / 1440),
    }
}
