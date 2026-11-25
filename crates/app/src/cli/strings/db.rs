//! The text of `marsa db`.

/// The titles of the columns of `marsa db`.
pub const DB_SOURCES_HEADER: [&str; 3] = ["SOURCE", "SERVER", "STATE"];

/// The titles of the columns of `marsa db tables`.
pub const DB_TABLES_HEADER: [&str; 3] = ["TABLE", "KIND", "ROWS"];

/// `marsa db` with nothing to open.
pub const DB_NONE: &str = "No databases yet. Install a server with \"marsa install mysql\" or \
     \"marsa install postgres\", or keep a Laravel site's SQLite file at database/database.sqlite.\n";

/// The line under `marsa db`.
pub const DB_SOURCES_HINT: &str = "List a source's tables with \"marsa db tables <SOURCE>\".\n";

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
