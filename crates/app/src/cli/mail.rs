//! `marsa mail`, `marsa mail show <ID>`, and `marsa mail clear`: the mail
//! Mailpit caught, through `GET /v1/mail`, `GET /v1/mail/{id}`, and
//! `DELETE /v1/mail`. The CLI never talks to Mailpit itself.

use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use marsa_core::types::{MailList, MailText};

use super::Failure;
use super::daemon::{connect, failure};
use super::strings;
use super::text::{clipped, columns, printable, printable_multiline};

/// The arguments of `marsa mail`.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct MailArgs {
    #[command(subcommand)]
    pub command: Option<MailCommand>,
}

/// What `marsa mail` does. Without one, it lists the newest mails.
#[derive(Debug, Clone, PartialEq, Eq, clap::Subcommand)]
pub enum MailCommand {
    /// Print one mail's plain text, and the address of its page.
    Show {
        /// The mail's id, from "marsa mail".
        id: String,
    },
    /// Delete every mail.
    Clear,
}

/// The widest subject and sender in the list, so a row fits a terminal.
const SUBJECT_WIDTH: usize = 50;
const FROM_WIDTH: usize = 30;

/// Starts the daemon if needed, then lists, shows, or deletes the mail.
pub async fn run(args: &MailArgs, data_dir: &Path) -> Result<ExitCode, Failure> {
    let client = connect(data_dir).await?;
    let text = match &args.command {
        None => client.mail().await.map(|list| render_list(&list, now())),
        Some(MailCommand::Show { id }) => client.mail_text(id).await.map(|mail| render_mail(&mail)),
        Some(MailCommand::Clear) => client
            .clear_mail()
            .await
            .map(|_| strings::MAIL_CLEARED.to_owned()),
    };
    let text = text.map_err(|err| failure(&err, data_dir, Some("mailpit")))?;
    print!("{text}");
    Ok(ExitCode::SUCCESS)
}

/// Now, in milliseconds since the Unix epoch.
fn now() -> u64 {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

/// The list: a count line, then a table newest first, then where to read the
/// mail in the browser. `now` gives each mail's age.
pub fn render_list(list: &MailList, now: u64) -> String {
    if list.messages.is_empty() {
        return format!("{}\n", strings::MAIL_NONE);
    }
    let mut rows = vec![strings::MAIL_HEADER.map(str::to_owned)];
    rows.extend(list.messages.iter().map(|mail| {
        let new = if mail.read { "" } else { strings::MAIL_NEW };
        [
            printable(&mail.id),
            strings::age(now.saturating_sub(mail.received)),
            clipped(&mail.from, FROM_WIDTH),
            clipped(&mail.subject, SUBJECT_WIDTH),
            new.to_owned(),
        ]
    }));
    format!(
        "{}\n{}{}\n",
        strings::mail_count(list.total, list.unread, list.messages.len()),
        columns(&rows),
        strings::MAIL_READ_HINT
    )
}

/// One mail: its headers, its text, and its page for the HTML.
pub fn render_mail(mail: &MailText) -> String {
    let body = if mail.text.trim().is_empty() {
        strings::MAIL_NO_TEXT.to_owned()
    } else {
        printable_multiline(&mail.text.replace("\r\n", "\n"))
    };
    format!(
        "From:    {}\nTo:      {}\nSubject: {}\n\n{}\n\n{} {}\n",
        printable(&mail.from),
        printable(&mail.to.join(", ")),
        printable(&mail.subject),
        body.trim_end(),
        strings::MAIL_PAGE,
        printable(&mail.web_url)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::types::MailSummary;

    fn summary(id: &str, subject: &str, received: u64, read: bool) -> MailSummary {
        MailSummary {
            id: id.into(),
            from: "Laravel <hello@example.test>".into(),
            to: vec!["user@example.test".into()],
            subject: subject.into(),
            received,
            size: 100,
            read,
            snippet: String::new(),
            attachments: 0,
        }
    }

    #[test]
    fn the_list_has_a_count_a_table_and_the_page() {
        let now = 10 * 60 * 60 * 1000;
        let list = MailList {
            total: 3,
            unread: 1,
            messages: vec![
                summary("abc", "Reset\u{1b}[2J Password", now - 90_000, false),
                summary("defg", "Welcome", now - 3 * 3_600_000, true),
            ],
        };
        assert_eq!(
            render_list(&list, now),
            "3 mails, 1 new. The newest 2:\n\
             ID    AGE        FROM                          SUBJECT\n\
             abc   1 min ago  Laravel <hello@example.test>  Reset[2J Password  new\n\
             defg  3 h ago    Laravel <hello@example.test>  Welcome\n\
             Read one with \"marsa mail show <ID>\", or all of them at http://127.0.0.1:8025.\n"
        );
    }

    #[test]
    fn no_mail_says_how_to_send_some() {
        let text = render_list(&MailList::default(), 0);
        assert!(text.contains("MAIL_MAILER=smtp"), "{text}");
    }

    #[test]
    fn one_mail_shows_its_text_and_its_page() {
        let mail = MailText {
            id: "abc".into(),
            from: "Laravel <hello@example.test>".into(),
            to: vec!["a@example.test".into(), "b@example.test".into()],
            subject: "Reset".into(),
            date: 0,
            text: "Line one\r\nLine two\u{1b}[2J\r\n".into(),
            web_url: "http://127.0.0.1:8025/view/abc".into(),
        };
        assert_eq!(
            render_mail(&mail),
            "From:    Laravel <hello@example.test>\n\
             To:      a@example.test, b@example.test\n\
             Subject: Reset\n\n\
             Line one\nLine two[2J\n\n\
             HTML and attachments: http://127.0.0.1:8025/view/abc\n"
        );
        let html_only = MailText {
            text: " \r\n".into(),
            ..mail
        };
        assert!(render_mail(&html_only).contains("no plain text"));
    }
}
