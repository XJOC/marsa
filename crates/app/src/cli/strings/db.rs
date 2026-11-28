//! The text of `marsa db`.

/// The titles of the columns of `marsa db`.
pub const DB_SOURCES_HEADER: [&str; 3] = ["SOURCE", "SERVER", "STATE"];

/// The titles of the columns of `marsa db tables`.
pub const DB_TABLES_HEADER: [&str; 3] = ["TABLE", "KIND", "ROWS"];

/// `marsa db` with nothing to open.
pub const DB_NONE: &str = "No databases yet. Install a server with \"marsa install mysql\" or \
     \"marsa install postgres\", or keep a Laravel site's SQLite file at database/database.sqlite.\n";

/// The line under `marsa db`.
pub const DB_SOURCES_HINT: &str = "List a source's tables with \"marsa db tables <SOURCE>\", \
     or Valkey's keys with \"marsa db keys valkey/0\".\n";

/// How a Valkey database is written: this, then its number.
pub const DB_VALKEY_PREFIX: &str = "valkey/";

/// A page of keys with none.
pub const DB_NO_KEYS: &str = "No keys found.\n";

/// A stream, or a module's type.
pub const DB_KEY_NOT_SHOWN: &str = "Marsa does not show values of this type.\n";

/// The titles of a hash, a list, a set, and a sorted set.
pub const DB_HASH_HEADER: [&str; 2] = ["FIELD", "VALUE"];
pub const DB_LIST_HEADER: [&str; 2] = ["INDEX", "VALUE"];
pub const DB_SET_HEADER: [&str; 1] = ["MEMBER"];
pub const DB_ZSET_HEADER: [&str; 2] = ["MEMBER", "SCORE"];

/// A `--type` Marsa does not know.
pub fn db_key_types(words: &[&str]) -> String {
    format!("the types are {}", words.join(", "))
}

/// The STATE of a Valkey database in `marsa db`.
pub fn db_state_keys(state: &str, keys: u64) -> String {
    let noun = if keys == 1 { "key" } else { "keys" };
    format!("{state}, {} {noun}", db_count(keys))
}

/// Under a page of keys: the whole database.
pub fn db_keys_total(db: u8, total: u64) -> String {
    let noun = if total == 1 { "key" } else { "keys" };
    format!("{} {noun} in {DB_VALKEY_PREFIX}{db}.\n", db_count(total))
}

/// Keys whose names cannot be printed.
pub fn db_keys_unshown(count: u64) -> String {
    let names = if count == 1 {
        "key with a name"
    } else {
        "keys with names"
    };
    format!("{} {names} Marsa cannot show.\n", db_count(count))
}

/// How to read the next page.
pub fn db_keys_more(db: u8, cursor: u64) -> String {
    format!("More keys: marsa db keys {DB_VALKEY_PREFIX}{db} --cursor {cursor}\n")
}

/// The first line of `marsa db key`: `hash, expires in 2 min, 4 fields`.
pub fn db_key_line(kind: &str, ttl: Option<u64>, size: Option<&str>) -> String {
    let expiry = ttl.map_or("no expiry".to_owned(), |seconds| {
        format!("expires in {}", db_duration(seconds))
    });
    match size {
        Some(size) => format!("{kind}, {expiry}, {size}\n"),
        None => format!("{kind}, {expiry}\n"),
    }
}

/// A time to live, in its two largest units: `1 h 59 min`, `45 s`.
pub fn db_duration(seconds: u64) -> String {
    let (days, hours) = (seconds / 86_400, seconds % 86_400 / 3_600);
    let (minutes, rest) = (seconds % 3_600 / 60, seconds % 60);
    match (days, hours, minutes) {
        (0, 0, 0) => format!("{rest} s"),
        (0, 0, _) => format!("{minutes} min {rest} s"),
        (0, _, _) => format!("{hours} h {minutes} min"),
        _ => format!("{} d {hours} h", db_count(days)),
    }
}

/// The size of a collection: `12 fields`, `1 item`.
pub fn db_elements(kind: &str, count: u64) -> String {
    let noun = match kind {
        "hash" => "field",
        "list" => "item",
        _ => "member",
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("{} {noun}{plural}", db_count(count))
}

/// Under a collection that holds more than travels.
pub fn db_first_elements(shown: u64, total: u64) -> String {
    format!(
        "Marsa shows the first {} of {}.\n",
        db_count(shown),
        db_count(total)
    )
}

/// Under a string that was cut.
pub fn db_first_bytes(shown: u64, whole: u64) -> String {
    format!(
        "Marsa shows the first {} of {}.\n",
        db_size(shown),
        db_size(whole)
    )
}

/// A SQLite file in the SERVER column.
pub const DB_SQLITE: &str = "SQLite";

/// A table with no tables in it.
pub const DB_NO_TABLES: &str = "No tables yet.\n";

/// A page or a result with no rows.
pub const DB_NO_ROWS: &str = "No rows.\n";

/// A statement that gave no rows, such as `SET`.
pub const DB_DONE_NO_ROWS: &str = "Done. No rows.\n";

/// A null value, as the servers' own clients write it.
pub const DB_NULL: &str = "NULL";

/// The end of a value that was cut.
pub const DB_CUT: &str = "…";

/// A table with no primary key: its order can change between pages.
pub const DB_NO_KEY: &str =
    "This table has no primary key, so the order of its rows can change between pages.\n";

/// A result cut at its limit.
pub const DB_RESULT_CUT: &str = "Marsa keeps the first 1,000 rows or 8 MB of a result. \
     Add LIMIT or WHERE to see the others.\n";

/// A view in the KIND column.
pub fn db_kind(view: bool) -> &'static str {
    if view { "view" } else { "table" }
}

/// A binary value: only its size travels.
pub fn db_binary(bytes: u64) -> String {
    format!("(binary, {})", db_size(bytes))
}

/// A size in bytes, as `14.2 KB`.
pub fn db_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `12408` as `12,408`.
pub fn db_count(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// The line under a page: `Rows 1 to 500 of 12,408`, or `of about …` for an
/// estimate.
pub fn db_page_line(first: u64, last: u64, total: u64, exact: bool) -> String {
    let about = if exact { "" } else { "about " };
    format!(
        "Rows {} to {} of {about}{}.\n",
        db_count(first),
        db_count(last),
        db_count(total)
    )
}

/// The line under a result: `8 rows in 4 ms`.
pub fn db_result_line(rows: u64, elapsed_ms: u64) -> String {
    let noun = if rows == 1 { "row" } else { "rows" };
    format!("{} {noun} in {elapsed_ms} ms.\n", db_count(rows))
}

/// The estimate in the ROWS column.
pub fn db_estimate(rows: Option<u64>) -> String {
    rows.map_or(String::new(), |rows| format!("about {}", db_count(rows)))
}
