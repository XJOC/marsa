//! The text of Valkey's keys on the Databases screen.

use marsa_core::types::{DbCell, DbKeyKind};

use super::db_browser::{grouped, size};
use crate::model::KeyShape;

/// "Find a key": its label and what it shows while empty.
pub const FIND_KEY: &str = "Find a key";
/// What a screen reader hears for the list of keys.
pub const DB_KEY_LIST: &str = "Keys";
pub const DB_MORE_KEYS: &str = "More keys";
pub const DB_READING_KEYS: &str = "Reading keys…";
pub const DB_READING_KEY: &str = "Reading the key…";

/// Under the list with no names.
pub const NO_KEYS: &str = "No keys.";
pub const NO_KEYS_MATCH: &str = "No keys match.";
/// The part of the database searched so far held none.
pub const NO_KEYS_YET: &str = "None found yet.";
/// Under [`NO_KEYS_MATCH`] in the detail.
pub const NO_KEYS_MATCH_HOW: &str = "Change the search or the type.";
/// Under [`no_keys_in`].
pub const NO_KEYS_HOW: &str =
    "Laravel writes here once your app uses Redis for its cache, sessions, or queues.";
/// The detail with no key chosen.
pub const CHOOSE_KEY: &str = "Choose a key to see its value.";
pub const KEY_GONE: &str = "This key is gone.";
pub const KEY_GONE_WHY: &str = "It may have expired.";
pub const KEY_NOT_SHOWN: &str = "Marsa does not show values of this type.";
/// The pane of a string.
pub const KEY_VALUE: &str = "Value";
pub const KEY_INDEX: &str = "Index";
pub const KEY_SCORE: &str = "Score";

/// The screen when Valkey does not run.
pub const VALKEY_STOPPED: &str = "Valkey is not running.";
pub const VALKEY_STOPPED_HOW: &str = "Start it to see its keys.";
pub const START_VALKEY: &str = "Start Valkey";
pub const VALKEY_SIGN_IN_FAILED: &str = "Marsa could not sign in to Valkey.";
pub const VALKEY_SIGN_IN_HOW: &str =
    "Marsa connects with no password. If Valkey asks for one, this screen cannot read it.";

/// The source menu's button: "Valkey / Database 0".
pub fn keys_source(db: u8) -> String {
    format!("Valkey / Database {db}")
}

/// A row of the source menu: "Valkey / Database 1 · 34 keys".
pub fn keys_choice(db: u8, keys: u64) -> String {
    format!("{} · {}", keys_source(db), counted(keys, "key"))
}

/// The type menu and its button; `None` is every type.
pub fn key_filter(kind: Option<DbKeyKind>) -> &'static str {
    match kind {
        None => "All types",
        Some(DbKeyKind::String) => "String",
        Some(DbKeyKind::Hash) => "Hash",
        Some(DbKeyKind::List) => "List",
        Some(DbKeyKind::Set) => "Set",
        Some(DbKeyKind::Zset) => "Sorted set",
        Some(DbKeyKind::Stream) => "Stream",
    }
}

/// The type tag of a key.
pub fn key_type(shape: &KeyShape) -> &str {
    match shape {
        KeyShape::String => key_filter(Some(DbKeyKind::String)),
        KeyShape::Hash => key_filter(Some(DbKeyKind::Hash)),
        KeyShape::List => key_filter(Some(DbKeyKind::List)),
        KeyShape::Set => key_filter(Some(DbKeyKind::Set)),
        KeyShape::Zset => key_filter(Some(DbKeyKind::Zset)),
        // Valkey's own word, such as "stream".
        KeyShape::Other(word) => word,
    }
}

/// "212 keys in this database".
pub fn keys_in_database(total: u64) -> String {
    format!("{} in this database", counted(total, "key"))
}

/// "3 keys with names Marsa cannot show."
pub fn keys_unshown(count: u64) -> String {
    if count == 1 {
        "1 key with a name Marsa cannot show.".to_owned()
    } else {
        format!("{} keys with names Marsa cannot show.", grouped(count))
    }
}

/// Under a list at its limit.
pub fn key_list_full(limit: usize) -> String {
    format!(
        "Showing {} keys. Use Find a key to narrow the list.",
        grouped(limit as u64)
    )
}

/// "No keys in database 0 yet."
pub fn no_keys_in(db: u8) -> String {
    format!("No keys in database {db} yet.")
}

/// "Expires in 1 h 59 min", or "No expiry".
pub fn key_expiry(ttl: Option<u64>) -> String {
    ttl.map_or_else(
        || "No expiry".to_owned(),
        |seconds| format!("Expires in {}", duration(seconds)),
    )
}

/// A time to live in its two largest units: "2 d 3 h", "45 s".
fn duration(seconds: u64) -> String {
    let (days, hours) = (seconds / 86_400, seconds % 86_400 / 3_600);
    let (minutes, rest) = (seconds % 3_600 / 60, seconds % 60);
    match (days, hours, minutes) {
        (0, 0, 0) => format!("{rest} s"),
        (0, 0, _) => format!("{minutes} min {rest} s"),
        (0, _, _) => format!("{hours} h {minutes} min"),
        _ => format!("{} d {hours} h", grouped(days)),
    }
}

/// The word for an element of a collection.
fn element(shape: &KeyShape) -> &'static str {
    match shape {
        KeyShape::Hash => "field",
        KeyShape::List => "item",
        _ => "member",
    }
}

/// "12 fields".
fn counted(count: u64, noun: &str) -> String {
    let plural = if count == 1 { "" } else { "s" };
    format!("{} {noun}{plural}", grouped(count))
}

/// The facts line's size: "128 B", "12 fields"; none for a type not read.
pub fn key_size(shape: &KeyShape, total: u64) -> Option<String> {
    match shape {
        KeyShape::String => Some(size(total)),
        KeyShape::Other(_) => None,
        _ => Some(counted(total, element(shape))),
    }
}

/// The grid's last line: "12 fields", or "The first 500 of 12,408 items."
pub fn key_footer(shape: &KeyShape, shown: u64, total: u64) -> String {
    if shown < total {
        format!(
            "The first {} of {} {}s.",
            grouped(shown),
            grouped(total),
            element(shape)
        )
    } else {
        counted(total, element(shape))
    }
}

/// The titles of a collection's grid.
pub fn key_columns(shape: &KeyShape) -> &'static [&'static str] {
    match shape {
        KeyShape::Hash => &["Field", KEY_VALUE],
        KeyShape::List => &[KEY_INDEX, KEY_VALUE],
        KeyShape::Set => &["Member"],
        KeyShape::Zset => &["Member", KEY_SCORE],
        KeyShape::String | KeyShape::Other(_) => &[],
    }
}

/// The caption of a string's pane: "string · 1.2 KB".
pub fn string_caption(bytes: u64) -> String {
    format!("string · {}", size(bytes))
}

/// The caption of an element's pane: its size.
pub fn element_caption(cell: &DbCell) -> String {
    match cell {
        DbCell::Null => String::new(),
        DbCell::Text(text) => size(text.len() as u64),
        DbCell::Cut { bytes, .. } | DbCell::Binary { bytes } => size(*bytes),
    }
}

/// Under a cut value: "The first 8.2 KB of 2.1 MB."
pub fn key_cut(shown: u64, whole: u64) -> String {
    format!("The first {} of {}.", size(shown), size(whole))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_to_live_reads_in_two_units() {
        for (seconds, text) in [
            (0, "Expires in 0 s"),
            (59, "Expires in 59 s"),
            (61, "Expires in 1 min 1 s"),
            (7_140, "Expires in 1 h 59 min"),
            (90_000, "Expires in 1 d 1 h"),
        ] {
            assert_eq!(key_expiry(Some(seconds)), text);
        }
        assert_eq!(key_expiry(None), "No expiry");
    }

    #[test]
    fn sizes_and_counts_name_what_they_count() {
        assert_eq!(key_size(&KeyShape::Hash, 1).as_deref(), Some("1 field"));
        assert_eq!(key_size(&KeyShape::String, 188).as_deref(), Some("188 B"));
        assert_eq!(key_size(&KeyShape::Other("stream".into()), 0), None);
        assert_eq!(
            key_footer(&KeyShape::List, 500, 12_408),
            "The first 500 of 12,408 items."
        );
        assert_eq!(key_footer(&KeyShape::Zset, 3, 3), "3 members");
        assert_eq!(keys_choice(1, 34), "Valkey / Database 1 · 34 keys");
        assert_eq!(keys_unshown(1), "1 key with a name Marsa cannot show.");
    }
}
