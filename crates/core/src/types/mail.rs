//! The mail Mailpit caught, as `marsad` gives it: `GET /v1/mail` and
//! `GET /v1/mail/{id}`. The window and the CLI never talk to Mailpit.

use serde::{Deserialize, Serialize};

/// `GET /v1/mail`, and the answer to `DELETE /v1/mail`: the newest mails,
/// newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailList {
    /// Every mail Mailpit keeps, also those not in `messages`.
    pub total: u64,
    /// The mails not opened yet, in Mailpit's page or through
    /// `GET /v1/mail/{id}`.
    pub unread: u64,
    /// At most [`MAIL_LIST_LIMIT`], newest first.
    pub messages: Vec<MailSummary>,
}

/// The most mails `GET /v1/mail` lists.
pub const MAIL_LIST_LIMIT: usize = 200;

/// One mail in [`MailList`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailSummary {
    /// Mailpit's id: the `{id}` of `GET /v1/mail/{id}`.
    pub id: String,
    /// `Name <address>`, or the address alone.
    pub from: String,
    /// Each recipient as `from` is written.
    pub to: Vec<String>,
    pub subject: String,
    /// When Mailpit took it, in milliseconds since the Unix epoch.
    pub received: u64,
    /// In bytes, as sent.
    pub size: u64,
    pub read: bool,
    /// The start of its text, on one line.
    pub snippet: String,
    pub attachments: u32,
}

/// `GET /v1/mail/{id}`: one mail's plain text. Its HTML is not sent: the
/// window opens [`MailText::web_url`] in the browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailText {
    pub id: String,
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    /// The date the mail says it was sent, in milliseconds since the Unix
    /// epoch.
    pub date: u64,
    /// The plain text part. Empty when the mail has HTML only.
    pub text: String,
    /// Mailpit's own page for this mail, on `127.0.0.1`.
    pub web_url: String,
}

#[cfg(test)]
mod mail_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mail_list_round_trips() {
        let list = MailList {
            total: 3,
            unread: 1,
            messages: vec![MailSummary {
                id: "abc".into(),
                from: "App <app@example.test>".into(),
                to: vec!["user@example.test".into()],
                subject: "Reset".into(),
                received: 1_700_000_000_000,
                size: 1024,
                read: false,
                snippet: "Click".into(),
                attachments: 0,
            }],
        };
        let value = serde_json::to_value(&list).expect("json");
        assert_eq!(
            value["messages"][0]["received"],
            json!(1_700_000_000_000_u64)
        );
        assert_eq!(
            serde_json::from_value::<MailList>(value).expect("back"),
            list
        );
    }
}
