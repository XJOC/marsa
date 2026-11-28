//! A site's SQLite file: found, checked, and opened read-only for one
//! request at a time.
//!
//! The file is opened for each request and closed after it. An open handle
//! would stop the user from deleting or replacing the file on Windows, and
//! could hold back a WAL checkpoint of the site's own connections.
//!
//! The database file itself is never written or created. A database in WAL
//! mode is the one exception around it: SQLite makes its `-wal` and `-shm`
//! files beside it when they are missing, even for a read-only connection,
//! and that connection cannot remove them, because removing them needs a
//! checkpoint, which writes. They hold no data of their own, and the site's
//! app uses or removes them at its next open. Not making them would need
//! `immutable`, which reads torn pages while the app writes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use axum::http::StatusCode;
use marsa_core::codes;
use marsa_core::db_browser::{self, Engine};
use marsa_core::sites::validate_name;
use marsa_core::types::{
    DbCellRequest, DbColumn, DbPage, DbRowsRequest, DbSiteFile, DbTable, DbTableInfo, DbTableRef,
    DbTables, DbTotal, DbValue, SiteList,
};
use sqlx::error::DatabaseError;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{ConnectOptions, Connection, Row};
use tokio_stream::StreamExt;

use super::{COUNT_LIMIT, catalog, rows, unavailable};
use crate::daemon::http::routes::HttpError;
use crate::daemon::state::Daemon;

/// The sites whose SQLite file the browser can open, by site name.
pub(super) fn files(list: &SiteList) -> Vec<DbSiteFile> {
    list.sites
        .iter()
        .filter(|site| checked_file(&site.root).is_ok())
        .map(|site| DbSiteFile {
            site: site.name.clone(),
            path: db_browser::sqlite_file_shown(),
        })
        .collect()
}

/// The SQLite file of the site `name`, after [`checked_file`].
pub(super) async fn site_file(daemon: &Arc<Daemon>, name: &str) -> Result<PathBuf, HttpError> {
    validate_name(name)
        .map_err(|err| HttpError::new(StatusCode::BAD_REQUEST, err.code(), err.to_string()))?;
    let list = daemon.site_list().await?;
    let root = list
        .sites
        .iter()
        .find(|site| site.name == name)
        .map(|site| site.root.clone())
        .ok_or_else(|| {
            let message = format!("no site named {name:?}");
            HttpError::new(StatusCode::NOT_FOUND, codes::SITE_NOT_FOUND, message)
        })?;
    crate::daemon::http::blocking("db file", move || checked_file(&root)).await
}

/// `root`'s SQLite file, if it is a regular file inside `root` once every
/// link is followed. The paths are made canonical only to compare them: the
/// path opened is the plain one (architecture.md section 10).
///
/// The driver opens the file by its path later, so a link swapped in
/// between is followed. That is accepted: the swap needs the right to write
/// in the site's folder, the open is read-only, and the answer goes only to
/// the same user, who holds the token.
fn checked_file(root: &Path) -> Result<PathBuf, HttpError> {
    let path = db_browser::sqlite_file(root);
    let not_found = || {
        HttpError::new(
            StatusCode::NOT_FOUND,
            codes::DB_FILE_NOT_FOUND,
            "the site has no SQLite file",
        )
    };
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        // `NotADirectory`: `database` is a file.
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Err(not_found());
        }
        // Permission denied, for one: the system's words carry no path.
        Err(err) => return Err(unavailable(err.to_string())),
    };
    let inside = match (fs::canonicalize(&path), fs::canonicalize(root)) {
        (Ok(file), Ok(root)) => file.starts_with(root),
        _ => false,
    };
    if metadata.is_file() && inside {
        Ok(path)
    } else {
        Err(outside())
    }
}

fn outside() -> HttpError {
    HttpError::new(
        StatusCode::FORBIDDEN,
        codes::DB_FILE_OUTSIDE_SITE,
        "the SQLite file is not a regular file inside the site's folder",
    )
}

/// Opens `file` for reading only. It is never made if it is missing, and
/// nothing the browser does is logged by the driver: its statement log
/// would hold SQL text.
///
/// The file may come with a project cloned from anywhere, so the schema is
/// not trusted (its views and triggers cannot call functions with side
/// effects), and each page's cells are checked before they are read.
pub(super) async fn open(file: &Path) -> Result<SqliteConnection, HttpError> {
    // The driver opens with SQLite's URI flag: a relative name such as
    // `file:x?mode=rw` would be read as a URI with its own options. The site
    // list holds absolute folders only, so this never fires.
    if !file.is_absolute() {
        return Err(unavailable("the SQLite path is not absolute".to_owned()));
    }
    SqliteConnectOptions::new()
        .filename(file)
        .read_only(true)
        .create_if_missing(false)
        .pragma("trusted_schema", "OFF")
        .pragma("cell_size_check", "ON")
        .disable_statement_logging()
        .connect()
        .await
        .map_err(|err| unavailable(err.to_string()))
}

/// Makes SQLite interrupt what runs on `connection` once `go_on` answers
/// false: it is asked every 1,000 steps of SQLite's machine. Dropping the
/// wait for a statement does not stop it, and the connection's close waits
/// for it.
pub(super) async fn interrupt_unless(
    connection: &mut SqliteConnection,
    go_on: impl FnMut() -> bool + Send + 'static,
) -> Result<(), HttpError> {
    connection
        .lock_handle()
        .await
        .map_err(|err| unavailable(err.to_string()))?
        .set_progress_handler(1000, go_on);
    Ok(())
}

/// SQLite's primary result code of `err`: the low byte of the code the
/// driver gives, which may be an extended one.
pub(super) fn primary_code(err: &dyn DatabaseError) -> Option<i32> {
    let code: i32 = err.code()?.parse().ok()?;
    Some(code & 0xff)
}

/// SQLite's result code for a statement that was interrupted.
const SQLITE_INTERRUPT: i32 = 9;

/// Opens `file`, runs `work` on it, and closes it before the answer, so
/// the file is free again at once, whatever `work` returned.
async fn with_file<T>(
    file: &Path,
    work: impl AsyncFnOnce(&mut SqliteConnection) -> Result<T, HttpError>,
) -> Result<T, HttpError> {
    let mut connection = open(file).await?;
    let result = work(&mut connection).await;
    let _ = connection.close().await;
    result
}

/// The tables and views of `file`.
pub(super) async fn tables(file: PathBuf) -> Result<DbTables, HttpError> {
    with_file(&file, async |connection| read_tables(connection).await).await
}

async fn read_tables(connection: &mut SqliteConnection) -> Result<DbTables, HttpError> {
    let rows = sqlx::query(db_browser::tables_sql(Engine::Sqlite)).fetch(connection);
    let tables = catalog::list(rows, |err| unavailable(err.to_string()), catalog::table).await?;
    Ok(DbTables { tables })
}

/// The columns of `table`, after the catalog listed it, and its key.
async fn read_columns(
    connection: &mut SqliteConnection,
    table: &DbTableRef,
) -> Result<(DbTableInfo, Vec<DbColumn>, Vec<String>), HttpError> {
    let info = rows::listed(&read_tables(connection).await?, table)?;
    let found = sqlx::query(db_browser::columns_sql(Engine::Sqlite))
        .bind(&table.name)
        .fetch_all(connection)
        .await
        .map_err(|err| unavailable(err.to_string()))?;
    let (columns, key) = catalog::columns(&found)?;
    Ok((info, columns, key))
}

/// `POST /v1/db/table` for a site's file.
pub(super) async fn table(file: PathBuf, table: DbTableRef) -> Result<DbTable, HttpError> {
    with_file(&file, async |connection| {
        let (_, columns, _) = read_columns(connection, &table).await?;
        Ok(DbTable { columns })
    })
    .await
}

/// `POST /v1/db/rows` for a site's file: one page, read row by row.
pub(super) async fn page(file: PathBuf, request: DbRowsRequest) -> Result<DbPage, HttpError> {
    with_file(&file, async |connection| {
        let (info, columns, key) = read_columns(connection, &request.table).await?;
        rows::check_sort(&columns, request.sort.as_ref())?;
        let sql = db_browser::page_sql(
            Engine::Sqlite,
            &request.table,
            &key,
            request.sort.as_ref(),
            request.offset,
        )
        .map_err(rows::sql_error)?;
        let (page_columns, key) = rows::page_columns(Engine::Sqlite, &columns, &key);
        let mut found = Vec::new();
        {
            let mut stream = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch(&mut *connection);
            while let Some(row) = stream.next().await {
                let row = row.map_err(|err| unavailable(err.to_string()))?;
                let cells = (0..row.len())
                    .map(|index| rows::sqlite_cell(&row, index, db_browser::CELL_BYTES))
                    .collect::<Result<Vec<_>, _>>()?;
                found.push(cells);
            }
        }
        let seen = request.offset.saturating_add(found.len() as u64);
        let total = if request.count {
            count(connection, &request.table)
                .await?
                .unwrap_or_else(|| rows::estimated(info.estimate, seen))
        } else {
            rows::estimated(info.estimate, seen)
        };
        Ok(DbPage {
            columns: page_columns,
            key,
            rows: found,
            offset: request.offset,
            total,
        })
    })
    .await
}

/// The exact number of rows, or `None` past [`COUNT_LIMIT`], when SQLite
/// interrupts the count. Last on the connection: the limit stays on it.
async fn count(
    connection: &mut SqliteConnection,
    table: &DbTableRef,
) -> Result<Option<DbTotal>, HttpError> {
    let sql = db_browser::count_sql(Engine::Sqlite, table).map_err(rows::sql_error)?;
    let started = Instant::now();
    interrupt_unless(connection, move || started.elapsed() < COUNT_LIMIT).await?;
    let counted = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql))
        .fetch_one(connection)
        .await;
    match counted {
        Ok(rows) => Ok(Some(DbTotal {
            rows: u64::try_from(rows).unwrap_or(0),
            exact: true,
        })),
        Err(err) if err.as_database_error().and_then(primary_code) == Some(SQLITE_INTERRUPT) => {
            Ok(None)
        }
        Err(err) => Err(unavailable(err.to_string())),
    }
}

/// `POST /v1/db/cell` for a site's file: the whole value, up to
/// `VALUE_BYTES`, by the row's primary key, whose values are bound.
pub(super) async fn cell(file: PathBuf, request: DbCellRequest) -> Result<DbValue, HttpError> {
    with_file(&file, async |connection| {
        let (_, columns, key) = read_columns(connection, &request.table).await?;
        rows::check_cell(Engine::Sqlite, &columns, &key, &request)?;
        let sql =
            db_browser::cell_sql(Engine::Sqlite, &request.table, &request.column, &key, false)
                .map_err(rows::sql_error)?;
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
        for value in &request.key {
            query = query.bind(value.clone());
        }
        let row = query
            .fetch_optional(&mut *connection)
            .await
            .map_err(|err| unavailable(err.to_string()))?
            .ok_or_else(rows::row_not_found)?;
        Ok(DbValue {
            cell: rows::sqlite_cell(&row, 0, db_browser::VALUE_BYTES)?,
        })
    })
    .await
}
