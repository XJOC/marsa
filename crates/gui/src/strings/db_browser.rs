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
    let digits = rows.to_string();
    let mut grouped = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    format!("about {grouped}")
}

/// A null in a grid cell: a tag, never a color alone.
pub const NULL_TAG: &str = "NULL";

/// A binary value: only its size travels. "(binary, 14.2 KB)".
pub fn binary_value(bytes: u64) -> String {
    let size = if bytes < 1000 {
        format!("{bytes} B")
    } else if bytes < 1_000_000 {
        format!("{:.1} KB", bytes as f64 / 1000.0)
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    };
    format!("(binary, {size})")
}

/// What a screen reader hears for a grid.
pub fn grid_label(rows: usize, columns: usize) -> String {
    format!("Grid, {rows} rows, {columns} columns")
}
