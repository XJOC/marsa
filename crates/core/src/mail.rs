//! Mailpit's API answers, read into [`crate::types`]: what `marsad` gives
//! the window and the CLI for `GET /v1/mail` and `GET /v1/mail/{id}`.
//!
//! Pure functions on the bytes Mailpit sent; the daemon does the request.
//! Mailpit is written in Go, so its JSON names are in PascalCase, and its
//! times are RFC 3339 text.

use serde::Deserialize;

use crate::databases::mailpit::MAILPIT_UI_PORT;
use crate::types::{MAIL_LIST_LIMIT, MailList, MailSummary, MailText};

/// Why an answer of Mailpit could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Mailpit's answer is not what Marsa reads: {0}")]
pub struct BadAnswer(String);

/// Whether `id` can be a Mailpit id in a URL path: 1 to 64 of
/// `[A-Za-z0-9_-]`. Mailpit's own are 22 such characters. Anything else is
/// refused before a request, so no `/`, `.`, `?`, or `%` reaches its path.
pub fn is_mail_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// The path and query of Mailpit's list of the newest mails.
pub fn list_path() -> String {
    format!("/api/v1/messages?limit={MAIL_LIST_LIMIT}")
}

/// The path of one mail in Mailpit's API. `id` passed [`is_mail_id`].
pub fn message_path(id: &str) -> String {
    format!("/api/v1/message/{id}")
}

/// Mailpit's own page for one mail. `id` passed [`is_mail_id`].
pub fn web_url(id: &str) -> String {
    format!("http://127.0.0.1:{MAILPIT_UI_PORT}/view/{id}")
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawAddress {
    #[serde(default)]
    name: String,
    #[serde(default)]
    address: String,
}

#[derive(Deserialize)]
struct RawList {
    total: u64,
    unread: u64,
    #[serde(default)]
    messages: Vec<RawSummary>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawSummary {
    #[serde(rename = "ID")]
    id: String,
    #[serde(default)]
    read: bool,
    from: Option<RawAddress>,
    #[serde(default)]
    to: Option<Vec<RawAddress>>,
    #[serde(default)]
    subject: String,
    created: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    attachments: u32,
    #[serde(default)]
    snippet: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawMessage {
    #[serde(rename = "ID")]
    id: String,
    from: Option<RawAddress>,
    #[serde(default)]
    to: Option<Vec<RawAddress>>,
    #[serde(default)]
    subject: String,
    date: String,
    #[serde(default)]
    text: String,
}

/// `Name <address>`, or the address alone when the name is empty.
fn address(raw: &RawAddress) -> String {
    match raw.name.trim() {
        "" => raw.address.clone(),
        name => format!("{name} <{}>", raw.address),
    }
}

fn addresses(raw: Option<&Vec<RawAddress>>) -> Vec<String> {
    raw.map(|list| list.iter().map(address).collect())
        .unwrap_or_default()
}

/// Reads `GET /api/v1/messages`. Each id must pass [`is_mail_id`], so the
/// window can ask for it again.
///
/// # Errors
///
/// [`BadAnswer`] for JSON of another shape, an id that is not one, or a time
/// that is not RFC 3339.
pub fn read_list(body: &[u8]) -> Result<MailList, BadAnswer> {
    let raw: RawList = serde_json::from_slice(body).map_err(|err| BadAnswer(err.to_string()))?;
    let messages = raw
        .messages
        .into_iter()
        .take(MAIL_LIST_LIMIT)
        .map(|m| {
            if !is_mail_id(&m.id) {
                return Err(BadAnswer(format!("the id {:?} is not one", m.id)));
            }
            Ok(MailSummary {
                from: m.from.as_ref().map(address).unwrap_or_default(),
                to: addresses(m.to.as_ref()),
                received: rfc3339_millis(&m.created)?,
                id: m.id,
                subject: m.subject,
                size: m.size,
                read: m.read,
                snippet: m.snippet,
                attachments: m.attachments,
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(MailList {
        total: raw.total,
        unread: raw.unread,
        messages,
    })
}

/// Reads `GET /api/v1/message/{id}` for the mail `id`.
///
/// # Errors
///
/// [`BadAnswer`] for JSON of another shape, the answer of another mail, or
/// a date that is not RFC 3339.
pub fn read_message(id: &str, body: &[u8]) -> Result<MailText, BadAnswer> {
    let raw: RawMessage = serde_json::from_slice(body).map_err(|err| BadAnswer(err.to_string()))?;
    if raw.id != id {
        return Err(BadAnswer(format!("asked for {id}, got {:?}", raw.id)));
    }
    Ok(MailText {
        from: raw.from.as_ref().map(address).unwrap_or_default(),
        to: addresses(raw.to.as_ref()),
        date: rfc3339_millis(&raw.date)?,
        web_url: web_url(id),
        id: raw.id,
        subject: raw.subject,
        text: raw.text,
    })
}

/// `2026-11-24T10:15:30.123456789+03:00` (or `...Z`) in milliseconds since
/// the Unix epoch, as Go writes a `time.Time` in JSON. A time before 1970
/// reads as 0.
fn rfc3339_millis(text: &str) -> Result<u64, BadAnswer> {
    let bad = || BadAnswer(format!("the time {text:?} is not RFC 3339"));
    let number = |part: &str| -> Result<i64, BadAnswer> {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        part.parse().map_err(|_| bad())
    };
    let (date, time) = text.split_once('T').ok_or_else(bad)?;
    let mut ymd = date.splitn(3, '-');
    let (year, month, day) = (
        number(ymd.next().ok_or_else(bad)?)?,
        number(ymd.next().ok_or_else(bad)?)?,
        number(ymd.next().ok_or_else(bad)?)?,
    );
    let (clock, offset) = if let Some(clock) = time.strip_suffix('Z') {
        (clock, 0)
    } else {
        let at = time.rfind(['+', '-']).ok_or_else(bad)?;
        let (clock, zone) = time.split_at(at);
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let (hours, minutes) = zone[1..].split_once(':').ok_or_else(bad)?;
        (clock, sign * (number(hours)? * 60 + number(minutes)?))
    };
    let (whole, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut hms = whole.splitn(3, ':');
    let (hour, minute, second) = (
        number(hms.next().ok_or_else(bad)?)?,
        number(hms.next().ok_or_else(bad)?)?,
        number(hms.next().ok_or_else(bad)?)?,
    );
    let millis = match fraction {
        "" => 0,
        digits => {
            let three: String = digits.chars().chain("000".chars()).take(3).collect();
            number(digits)?;
            number(&three)?
        }
    };
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return Err(bad());
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset * 60;
    Ok(u64::try_from(seconds * 1000 + millis).unwrap_or(0))
}

/// Days since 1970-01-01 of a date in the proleptic Gregorian calendar
/// (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cut from Mailpit 1.34.0's answers (its API documentation and
    // `internal/storage/structs.go`): the fields Marsa reads, and others it
    // ignores.
    const LIST: &str = r#"{
        "total": 3, "unread": 1, "count": 2, "messages_count": 3, "start": 0, "tags": [],
        "messages": [
            {"ID": "XyZ12aBcD34eFgH56iJkLm", "MessageID": "a@b", "Read": false,
             "From": {"Name": "Laravel", "Address": "hello@example.test"},
             "To": [{"Name": "", "Address": "user@example.test"}],
             "Cc": null, "Bcc": null, "ReplyTo": [],
             "Subject": "Reset Password", "Created": "2026-11-24T10:15:30.123456789+03:00",
             "Username": "", "Tags": [], "Size": 2048, "Attachments": 1, "Snippet": "Click here"},
            {"ID": "second_id-2", "Read": true, "From": null, "To": null,
             "Subject": "", "Created": "1970-01-01T00:00:01Z", "Size": 10, "Attachments": 0,
             "Snippet": ""}
        ]
    }"#;

    const MESSAGE: &str = r#"{
        "ID": "XyZ12aBcD34eFgH56iJkLm", "MessageID": "a@b",
        "From": {"Name": "Laravel", "Address": "hello@example.test"},
        "To": [{"Name": "User", "Address": "user@example.test"}],
        "Subject": "Reset Password", "Date": "2026-11-24T07:15:30Z",
        "Text": "Click here to reset.\r\n", "HTML": "<p>Click</p>", "Size": 2048,
        "Inline": [], "Attachments": []
    }"#;

    #[test]
    fn reads_the_list() {
        let list = read_list(LIST.as_bytes()).expect("list");
        assert_eq!((list.total, list.unread, list.messages.len()), (3, 1, 2));
        let first = &list.messages[0];
        assert_eq!(first.from, "Laravel <hello@example.test>");
        assert_eq!(first.to, ["user@example.test"]);
        assert_eq!(first.subject, "Reset Password");
        // 07:15:30.123 UTC on 2026-11-24.
        assert_eq!(first.received, 1_795_504_530_123);
        assert_eq!(
            (first.size, first.read, first.attachments),
            (2048, false, 1)
        );
        let second = &list.messages[1];
        assert_eq!((second.from.as_str(), second.to.len()), ("", 0));
        assert_eq!(second.received, 1000);
    }

    #[test]
    fn reads_one_message() {
        let mail = read_message("XyZ12aBcD34eFgH56iJkLm", MESSAGE.as_bytes()).expect("mail");
        assert_eq!(mail.text, "Click here to reset.\r\n");
        assert_eq!(mail.to, ["User <user@example.test>"]);
        assert_eq!(mail.date, 1_795_504_530_000);
        assert_eq!(
            mail.web_url,
            "http://127.0.0.1:8025/view/XyZ12aBcD34eFgH56iJkLm"
        );
        // The answer for another mail.
        assert!(read_message("other", MESSAGE.as_bytes()).is_err());
    }

    #[test]
    fn bad_answers_are_refused() {
        for body in [
            "",
            "[]",
            r#"{"total": 1}"#,
            &LIST.replace("XyZ12aBcD34eFgH56iJkLm", "../x"),
            &LIST.replace("2026-11-24T10:15:30.123456789+03:00", "yesterday"),
        ] {
            assert!(read_list(body.as_bytes()).is_err(), "{body}");
        }
    }

    #[test]
    fn mail_ids() {
        for good in ["XyZ12aBcD34eFgH56iJkLm", "a", "a_b-c", &"x".repeat(64)] {
            assert!(is_mail_id(good), "{good}");
        }
        for bad in [
            "",
            "../x",
            "a/b",
            "a.b",
            "a?b",
            "a%2F",
            "é",
            &"x".repeat(65),
        ] {
            assert!(!is_mail_id(bad), "{bad}");
        }
    }

    #[test]
    fn rfc3339_times() {
        let cases = [
            ("1970-01-01T00:00:00Z", 0),
            ("2000-03-01T00:00:00Z", 951_868_800_000),
            ("2026-11-24T10:15:30+03:00", 1_795_504_530_000),
            ("2026-11-24T04:15:30.5-03:00", 1_795_504_530_500),
            ("1969-12-31T23:59:59Z", 0),
        ];
        for (text, millis) in cases {
            assert_eq!(rfc3339_millis(text), Ok(millis), "{text}");
        }
        for bad in [
            "",
            "2026-11-24",
            "2026-13-01T00:00:00Z",
            "2026-11-24T25:00:00Z",
            "2026-11-24T10:15:30",
            "2026-11-24T10:15:30+0300",
        ] {
            assert!(rfc3339_millis(bad).is_err(), "{bad}");
        }
    }
}
