//! `marsa db`: the database browser on the command line (ADR 0028), through
//! the same routes as the window. Read-only: a statement that changes data
//! is refused by the database, and Valkey's keys are only read ([`keys`]).
//! The CLI never connects to a database itself.

mod keys;

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use marsa_core::client::ClientError;
use marsa_core::codes;
use marsa_core::databases::ServerKind;
use marsa_core::db_browser::PAGE_ROWS;
use marsa_core::types::{
    DbCell, DbKeyKind, DbKeyRequest, DbKeysRequest, DbPage, DbQueryRequest, DbResult,
    DbRowsRequest, DbServer, DbSort, DbSource, DbSources, DbTableRef, DbTables, ServiceState,
};

use super::Failure;
use super::daemon::{connect, failure};
use super::strings;
use super::text::{columns, printable, printable_multiline};

/// The arguments of `marsa db`.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct DbArgs {
    #[command(subcommand)]
    pub command: Option<DbCommand>,
}

/// What `marsa db` does. Without one, it lists what can be opened.
#[derive(Debug, Clone, PartialEq, Eq, clap::Subcommand)]
pub enum DbCommand {
    /// List the tables and views of a source, such as mysql/shop.
    Tables { source: String },
    /// Print one page of a table's rows (500 a page).
    Rows {
        source: String,
        table: String,
        /// The page, from 1.
        #[arg(long, default_value_t = 1)]
        page: u64,
        /// Sort by this column first.
        #[arg(long)]
        sort: Option<String>,
        /// With --sort: the largest first.
        #[arg(long, requires = "sort")]
        desc: bool,
        /// The PostgreSQL schema of the table.
        #[arg(long, default_value = "public")]
        schema: String,
    },
    /// List the keys of a Valkey database.
    Keys {
        /// Valkey's database, such as valkey/1.
        #[arg(default_value = "valkey/0")]
        database: String,
        /// Only the keys this Valkey pattern matches, such as 'laravel*'.
        #[arg(long = "match", value_name = "PATTERN")]
        pattern: Option<String>,
        /// Only the keys of this type: string, hash, list, set, zset, or
        /// stream.
        #[arg(long = "type", value_name = "TYPE", value_parser = keys::kind)]
        kind: Option<DbKeyKind>,
        /// Go on from where the page before stopped.
        #[arg(long, default_value_t = 0)]
        cursor: u64,
    },
    /// Print one key's type, time to live, and value.
    Key {
        /// Valkey's database, such as valkey/0.
        database: String,
        key: String,
    },
    /// Run one statement and print its rows. Statements that change data are
    /// refused: the browser is read-only.
    Query {
        source: String,
        sql: String,
        /// Cancel the statement after this many seconds.
        #[arg(long)]
        timeout: Option<u64>,
    },
}

/// Starts the daemon if needed, then lists, reads, or runs.
pub async fn run(args: &DbArgs, data_dir: &Path) -> Result<ExitCode, Failure> {
    let source = |text: &str| {
        text.parse::<DbSource>()
            .map_err(|err| Failure::new(err.code(), data_dir, None))
    };
    let database = |text: &str| {
        keys::database(text).ok_or_else(|| Failure::new(codes::DB_SOURCE_INVALID, data_dir, None))
    };
    let client = connect(data_dir).await?;
    // The service an error of Valkey names.
    let service = matches!(
        args.command,
        Some(DbCommand::Keys { .. } | DbCommand::Key { .. })
    )
    .then(|| ServerKind::Valkey.id());
    let text = match &args.command {
        None => client
            .db_sources()
            .await
            .map(|sources| render_sources(&sources)),
        Some(DbCommand::Tables { source: text }) => {
            let source = source(text)?;
            client
                .db_tables(&source)
                .await
                .map(|tables| render_tables(&tables))
        }
        Some(DbCommand::Rows {
            source: text,
            table,
            page,
            sort,
            desc,
            schema,
        }) => {
            let source = source(text)?;
            let schema = matches!(
                source,
                DbSource::Server {
                    server: DbServer::Postgres,
                    ..
                }
            )
            .then(|| schema.clone());
            let request = DbRowsRequest {
                source,
                table: DbTableRef {
                    schema,
                    name: table.clone(),
                },
                offset: page.saturating_sub(1).saturating_mul(PAGE_ROWS),
                sort: sort.clone().map(|column| DbSort {
                    column,
                    descending: *desc,
                }),
                count: true,
            };
            client
                .db_rows(&request)
                .await
                .map(|page| render_page(&page))
        }
        Some(DbCommand::Keys {
            database: text,
            pattern,
            kind,
            cursor,
        }) => {
            let request = DbKeysRequest {
                db: database(text)?,
                pattern: pattern.clone(),
                kind: *kind,
                cursor: *cursor,
            };
            keys::list(&client, &request).await
        }
        Some(DbCommand::Key {
            database: text,
            key,
        }) => {
            let request = DbKeyRequest {
                db: database(text)?,
                key: key.clone(),
            };
            keys::value(&client, &request).await
        }
        Some(DbCommand::Query {
            source: text,
            sql,
            timeout,
        }) => {
            let request = DbQueryRequest {
                source: source(text)?,
                sql: sql.clone(),
            };
            let cancel_after = timeout.map(Duration::from_secs);
            client
                .run_db_query(&request, cancel_after)
                .await
                .map(|(_, result)| render_result(&result))
        }
    };
    match text {
        Ok(text) => {
            print!("{text}");
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => Ok(report(&err, data_dir, service)),
    }
}

/// The usual error line, then what the database said, as it said it: a
/// server's message is data, never translated.
fn report(err: &ClientError, data_dir: &Path, service: Option<&str>) -> ExitCode {
    let code = failure(err, data_dir, service).report();
    if let Some(detail) = err.detail() {
        eprintln!("{}", printable_multiline(detail));
    }
    code
}

fn render_sources(sources: &DbSources) -> String {
    let mut rows = vec![strings::DB_SOURCES_HEADER.map(str::to_owned)];
    for info in &sources.servers {
        let name = format!("{} {}", info.server.kind().name(), info.version)
            .trim()
            .to_owned();
        let state = server_state(&info.state, info.problem.as_deref());
        let id = info.server.kind().id();
        if info.databases.is_empty() {
            rows.push([id.to_owned(), name.clone(), state.clone()]);
        }
        for database in &info.databases {
            let source = format!("{id}/{}", printable(&database.name));
            rows.push([source, name.clone(), state.clone()]);
        }
    }
    if let Some(info) = &sources.valkey {
        let id = ServerKind::Valkey.id();
        let name = format!("{} {}", ServerKind::Valkey.name(), info.version)
            .trim()
            .to_owned();
        let state = server_state(&info.state, info.problem.as_deref());
        if info.databases.is_empty() {
            rows.push([id.to_owned(), name.clone(), state.clone()]);
        }
        for database in &info.databases {
            let source = format!("{id}/{}", database.db);
            let held = strings::db_state_keys(&state, database.keys);
            rows.push([source, name.clone(), held]);
        }
    }
    for file in &sources.files {
        let source = format!("site/{}", printable(&file.site));
        rows.push([source, strings::DB_SQLITE.to_owned(), printable(&file.path)]);
    }
    if rows.len() == 1 {
        return strings::DB_NONE.to_owned();
    }
    columns(&rows) + strings::DB_SOURCES_HINT
}

/// A server's state in `marsa db`, or why it could not be read.
fn server_state(state: &ServiceState, problem: Option<&str>) -> String {
    match (state, problem) {
        (_, Some(problem)) => strings::message(problem, &strings::Details::default()),
        (ServiceState::Running, None) => "running".to_owned(),
        (ServiceState::Failed { .. }, None) => "failed".to_owned(),
        _ => "stopped".to_owned(),
    }
}

fn render_tables(tables: &DbTables) -> String {
    if tables.tables.is_empty() {
        return strings::DB_NO_TABLES.to_owned();
    }
    let mut rows = vec![strings::DB_TABLES_HEADER.map(str::to_owned)];
    for info in &tables.tables {
        let name = match info.table.schema.as_deref() {
            Some(schema) if schema != "public" => {
                format!("{}.{}", printable(schema), printable(&info.table.name))
            }
            _ => printable(&info.table.name),
        };
        let kind = strings::db_kind(info.kind == marsa_core::types::DbTableKind::View);
        rows.push([name, kind.to_owned(), strings::db_estimate(info.estimate)]);
    }
    columns(&rows)
}

fn render_page(page: &DbPage) -> String {
    let mut text = grid(&page.columns, &page.rows);
    if page.rows.is_empty() {
        text = strings::DB_NO_ROWS.to_owned();
    } else {
        let first = page.offset + 1;
        let last = page.offset + page.rows.len() as u64;
        text.push_str(&strings::db_page_line(
            first,
            last,
            page.total.rows,
            page.total.exact,
        ));
    }
    if page.key.is_empty() {
        text.push_str(strings::DB_NO_KEY);
    }
    text
}

fn render_result(result: &DbResult) -> String {
    if result.columns.is_empty() {
        return strings::DB_DONE_NO_ROWS.to_owned();
    }
    let mut text = grid(&result.columns, &result.rows);
    text.push_str(&strings::db_result_line(
        result.rows.len() as u64,
        result.elapsed_ms,
    ));
    if result.cut {
        text.push_str(strings::DB_RESULT_CUT);
    }
    text
}

/// A table of cells, with the column names as titles.
fn grid(header: &[marsa_core::types::DbPageColumn], rows: &[Vec<DbCell>]) -> String {
    let mut lines: Vec<Vec<String>> = vec![header.iter().map(|c| printable(&c.name)).collect()];
    lines.extend(rows.iter().map(|row| row.iter().map(cell).collect()));
    table(&lines)
}

fn cell(cell: &DbCell) -> String {
    match cell {
        DbCell::Null => strings::DB_NULL.to_owned(),
        DbCell::Text(text) => printable(text),
        DbCell::Cut { text, .. } => printable(text) + strings::DB_CUT,
        DbCell::Binary { bytes } => strings::db_binary(*bytes),
    }
}

/// [`columns`] for rows whose number of cells is known only at run time.
fn table(rows: &[Vec<String>]) -> String {
    let count = rows.first().map_or(0, Vec::len);
    let mut widths = vec![0; count];
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut text = String::new();
    for row in rows {
        let mut line = String::new();
        for (cell, width) in row.iter().zip(&widths) {
            line.push_str(&format!("{cell:<width$}  "));
        }
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use marsa_core::types::{DbColumnKind, DbPageColumn, DbTotal};

    use super::*;

    fn column(name: &str) -> DbPageColumn {
        DbPageColumn {
            name: name.into(),
            kind: DbColumnKind::Text,
        }
    }

    #[test]
    fn a_page_shows_each_cell_shape_and_its_range() {
        let page = DbPage {
            columns: vec![column("id"), column("bio"), column("avatar")],
            key: vec![0],
            rows: vec![vec![
                DbCell::Text("1".into()),
                DbCell::Cut {
                    text: "Lorem\u{1b}[31m".into(),
                    bytes: 9000,
                },
                DbCell::Binary { bytes: 14_200 },
            ]],
            offset: 500,
            total: DbTotal {
                rows: 12_408,
                exact: false,
            },
        };
        assert_eq!(
            render_page(&page),
            "id  bio         avatar\n\
             1   Lorem[31m…  (binary, 14.2 KB)\n\
             Rows 501 to 501 of about 12,408.\n"
        );
        let null = DbPage {
            rows: vec![vec![
                DbCell::Null,
                DbCell::Text(String::new()),
                DbCell::Null,
            ]],
            key: Vec::new(),
            ..page
        };
        let text = render_page(&null);
        assert!(text.contains("NULL"), "{text}");
        assert!(text.ends_with(strings::DB_NO_KEY), "{text}");
    }

    #[test]
    fn a_result_says_its_rows_time_and_cut() {
        let result = DbResult {
            columns: vec![column("n")],
            rows: vec![vec![DbCell::Text("8".into())]],
            cut: true,
            elapsed_ms: 4,
        };
        assert_eq!(
            render_result(&result),
            format!("n\n8\n1 row in 4 ms.\n{}", strings::DB_RESULT_CUT)
        );
        let none = DbResult {
            columns: Vec::new(),
            rows: Vec::new(),
            cut: false,
            elapsed_ms: 1,
        };
        assert_eq!(render_result(&none), strings::DB_DONE_NO_ROWS);
    }

    #[test]
    fn numbers_and_sizes_read_well() {
        assert_eq!(strings::db_count(1_240_000), "1,240,000");
        assert_eq!(strings::db_count(500), "500");
        assert_eq!(strings::db_size(999), "999 B");
        assert_eq!(strings::db_size(2_100_000), "2.1 MB");
    }
}
