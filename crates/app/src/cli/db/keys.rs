//! `marsa db keys` and `marsa db key`: Valkey's keys, through the same
//! routes as the window. A name and a value are printed as data, without the
//! control characters that could steer a terminal.

use marsa_core::client::{ClientError, DaemonClient};
use marsa_core::db_browser::VALKEY_DATABASES;
use marsa_core::types::{
    DbCell, DbKeyDetail, DbKeyKind, DbKeyRequest, DbKeyValue, DbKeys, DbKeysRequest,
};

use super::{cell, table};
use crate::cli::strings;
use crate::cli::text::{printable, printable_multiline};

/// The database `text` names, written `valkey/0`.
pub(super) fn database(text: &str) -> Option<u8> {
    let number = text.strip_prefix(strings::DB_VALKEY_PREFIX)?;
    // No `+`, which `parse` would take.
    if !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    number.parse().ok().filter(|db| *db < VALKEY_DATABASES)
}

/// A value of `--type`.
pub(super) fn kind(word: &str) -> Result<DbKeyKind, String> {
    DbKeyKind::from_word(word).ok_or_else(|| {
        let words: Vec<&str> = DbKeyKind::ALL.iter().map(|kind| kind.word()).collect();
        strings::db_key_types(&words)
    })
}

pub(super) async fn list(
    client: &DaemonClient,
    request: &DbKeysRequest,
) -> Result<String, ClientError> {
    let found = client.db_keys(request).await?;
    Ok(render_keys(request, &found))
}

pub(super) async fn value(
    client: &DaemonClient,
    request: &DbKeyRequest,
) -> Result<String, ClientError> {
    let detail = client.db_key(request).await?;
    Ok(render_key(&detail))
}

fn render_keys(request: &DbKeysRequest, found: &DbKeys) -> String {
    let mut text = String::new();
    for key in &found.keys {
        text.push_str(&printable(key));
        text.push('\n');
    }
    if found.keys.is_empty() {
        text.push_str(strings::DB_NO_KEYS);
    }
    text.push_str(&strings::db_keys_total(request.db, found.total));
    if found.unshown > 0 {
        text.push_str(&strings::db_keys_unshown(found.unshown));
    }
    if found.cursor != 0 {
        text.push_str(&strings::db_keys_more(request.db, found.cursor));
    }
    text
}

fn render_key(detail: &DbKeyDetail) -> String {
    let (kind, size, shown) = match &detail.value {
        DbKeyValue::String { value } => ("string", string_size(value), None),
        DbKeyValue::Hash { fields, total } => ("hash", *total, Some(fields.len())),
        DbKeyValue::List { items, total } => ("list", *total, Some(items.len())),
        DbKeyValue::Set { members, total } => ("set", *total, Some(members.len())),
        DbKeyValue::Zset { members, total } => ("zset", *total, Some(members.len())),
        DbKeyValue::Other { name } => (name.as_str(), 0, None),
    };
    let measured = match &detail.value {
        DbKeyValue::Other { .. } => None,
        DbKeyValue::String { .. } => Some(strings::db_size(size)),
        _ => Some(strings::db_elements(kind, size)),
    };
    let mut text = strings::db_key_line(&printable(kind), detail.ttl, measured.as_deref());
    match &detail.value {
        DbKeyValue::String { value } => text.push_str(&string(value)),
        DbKeyValue::Hash { fields, .. } => {
            let rows = fields
                .iter()
                .map(|(field, value)| vec![cell(field), cell(value)]);
            text.push_str(&titled(&strings::DB_HASH_HEADER, rows));
        }
        DbKeyValue::List { items, .. } => {
            let rows = items
                .iter()
                .enumerate()
                .map(|(at, item)| vec![at.to_string(), cell(item)]);
            text.push_str(&titled(&strings::DB_LIST_HEADER, rows));
        }
        DbKeyValue::Set { members, .. } => {
            let rows = members.iter().map(|member| vec![cell(member)]);
            text.push_str(&titled(&strings::DB_SET_HEADER, rows));
        }
        DbKeyValue::Zset { members, .. } => {
            let rows = members
                .iter()
                .map(|(member, score)| vec![cell(member), printable(score)]);
            text.push_str(&titled(&strings::DB_ZSET_HEADER, rows));
        }
        DbKeyValue::Other { .. } => text.push_str(strings::DB_KEY_NOT_SHOWN),
    }
    if let Some(shown) = shown.filter(|shown| (*shown as u64) < size) {
        text.push_str(&strings::db_first_elements(shown as u64, size));
    }
    text
}

fn titled(titles: &[&str], rows: impl Iterator<Item = Vec<String>>) -> String {
    let titles = titles.iter().map(|title| (*title).to_owned()).collect();
    let lines: Vec<Vec<String>> = std::iter::once(titles).chain(rows).collect();
    table(&lines)
}

/// The length of the whole string, in bytes.
fn string_size(value: &DbCell) -> u64 {
    match value {
        DbCell::Null => 0,
        DbCell::Text(text) => text.len() as u64,
        DbCell::Cut { bytes, .. } | DbCell::Binary { bytes } => *bytes,
    }
}

/// A string's value on lines of its own, with its line breaks.
fn string(value: &DbCell) -> String {
    match value {
        DbCell::Text(text) => format!("{}\n", printable_multiline(text)),
        DbCell::Cut { text, bytes } => format!(
            "{}\n{}",
            printable_multiline(text),
            strings::db_first_bytes(text.len() as u64, *bytes)
        ),
        other => format!("{}\n", cell(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> DbCell {
        DbCell::Text(text.into())
    }

    #[test]
    fn a_database_is_written_valkey_and_a_number() {
        assert_eq!(database("valkey/0"), Some(0));
        assert_eq!(database("valkey/15"), Some(15));
        for other in [
            "valkey/16",
            "valkey/",
            "valkey/+1",
            "valkey/-1",
            "valkey/1 ",
            "valkey",
            "mysql/1",
            "1",
        ] {
            assert_eq!(database(other), None, "{other}");
        }
    }

    #[test]
    fn a_type_is_one_of_valkeys_words() {
        assert_eq!(kind("zset"), Ok(DbKeyKind::Zset));
        let refused = kind("sorted").unwrap_err();
        assert!(refused.contains("string, hash, list"), "{refused}");
    }

    #[test]
    fn a_page_of_keys_says_how_to_go_on() {
        let request = DbKeysRequest {
            db: 1,
            ..DbKeysRequest::default()
        };
        let found = DbKeys {
            keys: vec!["a\u{1b}[31m\nb".into(), "queues:default".into()],
            cursor: 176,
            unshown: 1,
            total: 1_200,
        };
        assert_eq!(
            render_keys(&request, &found),
            "a[31mb\nqueues:default\n1,200 keys in valkey/1.\n\
             1 key with a name Marsa cannot show.\n\
             More keys: marsa db keys valkey/1 --cursor 176\n"
        );
        let none = DbKeys::default();
        assert_eq!(
            render_keys(&request, &none),
            "No keys found.\n0 keys in valkey/1.\n"
        );
    }

    #[test]
    fn each_type_of_key_reads_as_a_table() {
        let detail = |ttl, value| render_key(&DbKeyDetail { ttl, value });
        let hash = DbKeyValue::Hash {
            fields: vec![(text("sku"), text("14"))],
            total: 900,
        };
        assert_eq!(
            detail(Some(7_140), hash),
            "hash, expires in 1 h 59 min, 900 fields\nFIELD  VALUE\nsku    14\n\
             Marsa shows the first 1 of 900.\n"
        );
        let list = DbKeyValue::List {
            items: vec![text("job")],
            total: 1,
        };
        assert_eq!(
            detail(None, list),
            "list, no expiry, 1 item\nINDEX  VALUE\n0      job\n"
        );
        let set = DbKeyValue::Set {
            members: vec![DbCell::Binary { bytes: 3 }],
            total: 1,
        };
        assert_eq!(
            detail(None, set),
            "set, no expiry, 1 member\nMEMBER\n(binary, 3 B)\n"
        );
        let zset = DbKeyValue::Zset {
            members: vec![(text("omar"), "12.5".into())],
            total: 1,
        };
        assert_eq!(
            detail(Some(45), zset),
            "zset, expires in 45 s, 1 member\nMEMBER  SCORE\nomar    12.5\n"
        );
        let stream = DbKeyValue::Other {
            name: "stream".into(),
        };
        assert_eq!(
            detail(None, stream),
            format!("stream, no expiry\n{}", strings::DB_KEY_NOT_SHOWN)
        );
    }

    #[test]
    fn a_string_keeps_its_lines_and_says_when_it_is_cut() {
        let detail = |value| {
            render_key(&DbKeyDetail {
                ttl: None,
                value: DbKeyValue::String { value },
            })
        };
        assert_eq!(detail(text("a\nb\u{7}")), "string, no expiry, 4 B\na\nb\n");
        let cut = DbCell::Cut {
            text: "ab".into(),
            bytes: 3_000_000,
        };
        assert_eq!(
            detail(cut),
            "string, no expiry, 3.0 MB\nab\nMarsa shows the first 2 B of 3.0 MB.\n"
        );
    }
}
