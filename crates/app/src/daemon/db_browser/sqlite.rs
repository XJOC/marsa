//! A site's SQLite file: found, checked, and opened read-only for one
//! request at a time.
//!
//! The file is opened for each request and closed after it. An open handle
//! would stop the user from deleting or replacing the file on Windows, and
//! could hold back a WAL checkpoint of the site's own connections.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::http::StatusCode;
use marsa_core::codes;
use marsa_core::db_browser::{self, Engine};
use marsa_core::sites::validate_name;
use marsa_core::types::{DbSiteFile, DbTables, SiteList};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{ConnectOptions, Connection};

use super::{catalog, site_list, unavailable};
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
    let list = site_list(daemon).await?;
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
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(not_found()),
        Err(_) => return Err(outside()),
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
pub(super) async fn open(file: &Path) -> Result<SqliteConnection, HttpError> {
    SqliteConnectOptions::new()
        .filename(file)
        .read_only(true)
        .create_if_missing(false)
        .disable_statement_logging()
        .connect()
        .await
        .map_err(|err| unavailable(err.to_string()))
}

/// The tables and views of `file`.
pub(super) async fn tables(file: PathBuf) -> Result<DbTables, HttpError> {
    let mut connection = open(&file).await?;
    let rows = sqlx::query(db_browser::tables_sql(Engine::Sqlite))
        .fetch_all(&mut connection)
        .await;
    // Closed before the answer, so the file is free again at once.
    let _ = connection.close().await;
    let rows = rows.map_err(|err| unavailable(err.to_string()))?;
    catalog::tables(&rows)
}
