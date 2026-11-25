//! The text of the Databases screen (ADR 0028).

use marsa_core::types::{DbServer, DbSource};

/// "Find a table": its label and what it shows while empty.
pub const FIND_TABLE: &str = "Find a table";

/// The column titles of the tables of a source.
pub const DB_TABLE: &str = "Table";
pub const DB_KIND: &str = "Kind";
pub const DB_ROWS: &str = "Rows";

/// A view in the Kind column and in the list.
pub const DB_VIEW: &str = "view";
/// A table in the Kind column.
pub const DB_TABLE_KIND: &str = "table";

/// Under the tables: the counts are the server's.
pub const DB_ESTIMATES: &str = "Row counts are the server's estimates.";

/// Nothing to open.
pub const NO_DATABASES: &str = "No databases yet.";
pub const NO_DATABASES_HOW: &str = "Install a server with \"marsa install mysql\" or \
     \"marsa install postgres\", or keep a Laravel site's SQLite file at \
     database/database.sqlite.";

/// "Try again" after Marsa could not sign in.
pub const TRY_AGAIN: &str = "Try again";

/// The source menu's button with nothing chosen.
pub const CHOOSE_SOURCE: &str = "Choose a database";

/// A server's name in a sentence.
pub fn server_name(server: DbServer) -> &'static str {
    server.kind().name()
}

/// "MySQL is not running."
pub fn db_server_stopped(server: DbServer) -> String {
    format!("{} is not running.", server_name(server))
}

/// How to start it, under [`db_server_stopped`].
pub fn db_server_stopped_how(server: DbServer) -> String {
    format!("Start {} to browse its databases.", server_name(server))
}

/// The button that starts it.
pub fn start_server(server: DbServer) -> String {
    format!("Start {}", server_name(server))
}

/// "Marsa could not sign in to MySQL."
pub fn db_sign_in_failed(server: DbServer) -> String {
    format!("Marsa could not sign in to {}.", server_name(server))
}

/// Why, under [`db_sign_in_failed`].
pub const DB_SIGN_IN_HOW: &str = "Marsa signs in as root with no password. If root has a \
     password now, this screen cannot open the server yet.";

/// "No tables in blog yet."
pub fn no_tables(source: &DbSource) -> String {
    format!("No tables in {} yet.", source_short(source))
}

/// How a database gets tables: Laravel's own command.
pub const NO_TABLES_HOW: &str = "php artisan migrate";

/// A source as the menu and its button show it: "MySQL / shop", "blog
/// (SQLite)".
pub fn source_label(source: &DbSource) -> String {
    match source {
        DbSource::Server { server, database } => format!("{} / {database}", server_name(*server)),
        DbSource::SiteFile { site } => format!("{site} (SQLite)"),
    }
}

/// The database or the site, alone.
pub fn source_short(source: &DbSource) -> &str {
    match source {
        DbSource::Server { database, .. } => database,
        DbSource::SiteFile { site } => site,
    }
}

/// A row estimate in the list: "12.4k"; in the table: "about 12,408".
pub fn rows_short(rows: u64) -> String {
    match rows {
        0..1000 => rows.to_string(),
        1000..1_000_000 => format!("{:.1}k", rows as f64 / 1000.0),
        _ => format!("{:.1}M", rows as f64 / 1_000_000.0),
    }
}

pub fn rows_about(rows: u64) -> String {
    format!("about {}", grouped(rows))
}

/// "12,408".
fn grouped(number: u64) -> String {
    let digits = number.to_string();
    let mut grouped = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// A null in a grid cell: a tag, never a color alone.
pub const NULL_TAG: &str = "NULL";

/// A binary value: only its size travels. "(binary, 14.2 KB)".
pub fn binary_value(bytes: u64) -> String {
    format!("(binary, {})", size(bytes))
}

/// "512 B", "14.2 KB", "2.1 MB".
pub fn size(bytes: u64) -> String {
    if bytes < 1000 {
        format!("{bytes} B")
    } else if bytes < 1_000_000 {
        format!("{:.1} KB", bytes as f64 / 1000.0)
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }
}

/// What a screen reader hears for a grid.
pub fn grid_label(rows: usize, columns: usize) -> String {
    format!("Grid, {rows} rows, {columns} columns")
}

/// The tabs of an open table.
pub const DB_ROWS_TAB: &str = "Rows";
pub const DB_COLUMNS_TAB: &str = "Columns";
/// What a screen reader hears for the tab strip.
pub const DB_TABLE_TABS: &str = "Table";

/// Reads the page and its count again (F5, or Cmd or Ctrl R).
pub const DB_REFRESH: &str = "Refresh";

/// The page bar's buttons, and why one is off.
pub const DB_PREVIOUS: &str = "Previous";
pub const DB_NEXT: &str = "Next";
pub const DB_FIRST_PAGE: &str = "This is the first page.";
pub const DB_LAST_PAGE: &str = "This is the last page.";

/// The page bar: "Rows 1 to 500 of 12,408", or "of about 1,240,000".
pub fn page_range(first: u64, last: u64, total: u64, exact: bool) -> String {
    let about = if exact { "" } else { "about " };
    format!(
        "Rows {} to {} of {about}{}",
        grouped(first),
        grouped(last),
        grouped(total)
    )
}

/// While a page loads: "Loading rows 501 to 1,000…".
pub fn page_loading(first: u64, last: u64) -> String {
    format!("Loading rows {} to {}…", grouped(first), grouped(last))
}

/// A table with no rows.
pub const DB_NO_ROWS: &str = "No rows.";

/// Under the grid of a table with no primary key.
pub const DB_NO_KEY: &str =
    "This table has no primary key, so the order of its rows can change between pages.";

/// The value pane's buttons.
pub const DB_COPY: &str = "Copy";
pub const DB_CLOSE_VALUE: &str = "Close the value";
/// Why "Copy" is off: only a binary value's size travels.
pub const DB_NO_COPY: &str = "Only the size of a binary value is shown.";

/// Under a value longer than the pane shows: "The first 64 KB of 2.1 MB."
pub fn value_cut(whole: u64) -> String {
    let shown = marsa_core::db_browser::VALUE_BYTES / 1024;
    format!("The first {shown} KB of {}.", size(whole))
}

/// While the whole value is read.
pub const DB_READING_VALUE: &str = "Reading the whole value…";

/// The value pane's caption: the column's kind and the value's size,
/// "text · 1.2 KB".
pub fn value_caption(kind: marsa_core::types::DbColumnKind, bytes: u64) -> String {
    use marsa_core::types::DbColumnKind;
    let kind = match kind {
        DbColumnKind::Text => "text",
        DbColumnKind::Number => "number",
        DbColumnKind::Time => "date and time",
        DbColumnKind::Binary => "binary",
        DbColumnKind::Other => "value",
    };
    format!("{kind} · {}", size(bytes))
}

/// The column titles of the Columns tab.
pub const DB_COLUMN: &str = "Column";
pub const DB_TYPE: &str = "Type";
pub const DB_NULL: &str = "Null";
pub const DB_DEFAULT: &str = "Default";
pub const DB_KEY: &str = "Key";
/// What a screen reader hears for the table of columns.
pub const DB_COLUMNS_LABEL: &str = "Columns";

/// Whether a column takes NULL.
pub fn yes_no(yes: bool) -> &'static str {
    if yes { "Yes" } else { "No" }
}

/// A column's key, as a tag.
pub fn key_name(key: marsa_core::types::DbKey) -> &'static str {
    match key {
        marsa_core::types::DbKey::Primary => "Primary key",
        marsa_core::types::DbKey::Unique => "Unique",
    }
}

/// The header's switch between a source's tables and its SQL.
pub const DB_VIEW_LABEL: &str = "View";
pub const DB_TABLES_VIEW: &str = "Tables";
pub const DB_SQL_VIEW: &str = "SQL";

/// What a screen reader says for the editor.
pub const DB_EDITOR: &str = "SQL";

/// The run bar.
pub const DB_RUN: &str = "Run";
pub const DB_CANCEL: &str = "Cancel";
pub const DB_READ_ONLY_TAG: &str = "Read-only";

/// The run bar's hint: Cmd on macOS, Ctrl elsewhere.
pub fn run_hint(mac: bool) -> &'static str {
    if mac {
        "Cmd+Enter runs the statement at the cursor."
    } else {
        "Ctrl+Enter runs the statement at the cursor."
    }
}

/// "8 rows in 4 ms", "1 row in 0 ms".
pub fn rows_in(rows: usize, ms: u64) -> String {
    let word = if rows == 1 { "row" } else { "rows" };
    format!("{} {word} in {ms} ms", grouped(rows as u64))
}

/// "Running for 12 s…": changes once a second.
pub fn running_for(seconds: u64) -> String {
    format!("Running for {seconds} s…")
}

/// A statement past the daemon's time limit.
pub const DB_QUERY_TIMEOUT: &str = "The statement ran longer than 5 minutes and was stopped.";

/// A cancelled statement: in the run bar, not an error.
pub const DB_CANCELLED: &str = "Cancelled.";

/// While a statement runs, where its rows will be.
pub const DB_ROWS_LATER: &str = "The rows show here when the statement ends.";

/// Under a result cut at 1,000 rows (or 8 MB).
pub const DB_RESULT_CUT: &str = "The first 1,000 rows. Add LIMIT or WHERE to see the others.";

/// A statement with no rows.
pub const DB_DONE_NO_ROWS: &str = "Done. No rows.";

/// The banner's sentence over the server's own words: "MySQL did not run
/// the statement."
pub fn query_failed(source: &DbSource) -> String {
    let name = match source {
        DbSource::Server { server, .. } => server_name(*server),
        DbSource::SiteFile { .. } => "SQLite",
    };
    format!("{name} did not run the statement.")
}

/// A paste past the limit, refused.
pub fn sql_too_long() -> String {
    let kib = marsa_core::db_browser::SQL_BYTES / 1024;
    format!("The text is limited to {kib} KB. The paste was not added.")
}
