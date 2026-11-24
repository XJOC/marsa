//! A table's columns and one page of its rows (ADR 0028, section 4).
//!
//! Every value is read as the text the database gives, cut to
//! `CELL_BYTES` while the row is read; a binary value travels as its size
//! only. The table and its sort column are checked against the catalog
//! before any SQL is built. One page is held at a time, never the table.

use axum::http::StatusCode;
use marsa_core::codes;
use marsa_core::db_browser::{Engine, SqlError, column_kind};
use marsa_core::types::{DbCell, DbColumn, DbPageColumn, DbSort, DbTableRef, DbTables, DbTotal};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, TypeInfo, ValueRef};

use super::unavailable;
use crate::daemon::http::routes::HttpError;

/// `table`, if the catalog lists it; else 404 `db_table_not_found`, before
/// any SQL names it. Its catalog entry, for the row estimate.
pub(super) fn listed(
    tables: &DbTables,
    table: &DbTableRef,
) -> Result<marsa_core::types::DbTableInfo, HttpError> {
    tables
        .tables
        .iter()
        .find(|info| &info.table == table)
        .cloned()
        .ok_or_else(|| {
            HttpError::new(
                StatusCode::NOT_FOUND,
                codes::DB_TABLE_NOT_FOUND,
                "the database has no such table",
            )
        })
}

/// `sort`'s column, if the table has it; else 400 `db_source_invalid`.
pub(super) fn check_sort(columns: &[DbColumn], sort: Option<&DbSort>) -> Result<(), HttpError> {
    match sort {
        Some(sort) if !columns.iter().any(|column| column.name == sort.column) => {
            Err(HttpError::new(
                StatusCode::BAD_REQUEST,
                codes::DB_SOURCE_INVALID,
                "the table has no such column",
            ))
        }
        _ => Ok(()),
    }
}

/// The page's columns and the indexes of the key's columns among them.
pub(super) fn page_columns(
    engine: Engine,
    columns: &[DbColumn],
    key: &[String],
) -> (Vec<DbPageColumn>, Vec<u32>) {
    let page = columns
        .iter()
        .map(|column| DbPageColumn {
            name: column.name.clone(),
            kind: column_kind(engine, &column.type_name),
        })
        .collect();
    let indexes = key
        .iter()
        .filter_map(|name| columns.iter().position(|column| &column.name == name))
        .filter_map(|at| u32::try_from(at).ok())
        .collect();
    (page, indexes)
}

/// The total when no exact count was asked for, or it did not finish: the
/// server's estimate, or what the pages so far prove.
pub(super) fn estimated(estimate: Option<u64>, seen: u64) -> DbTotal {
    DbTotal {
        rows: estimate.unwrap_or(0).max(seen),
        exact: false,
    }
}

/// A statement the browser could not build: a name it cannot quote.
pub(super) fn sql_error(err: SqlError) -> HttpError {
    HttpError::new(StatusCode::BAD_REQUEST, err.code(), err.to_string())
}

/// Cell `index` of a SQLite row. SQLite gives every value but a BLOB as
/// text when asked for bytes, as its own command-line tool shows it.
pub(super) fn sqlite_cell(
    row: &SqliteRow,
    index: usize,
    limit: usize,
) -> Result<DbCell, HttpError> {
    let value = row
        .try_get_raw(index)
        .map_err(|err| unavailable(err.to_string()))?;
    if value.is_null() {
        return Ok(DbCell::Null);
    }
    let blob = value.type_info().name() == "BLOB";
    let bytes: &[u8] = row
        .try_get_unchecked(index)
        .map_err(|err| unavailable(err.to_string()))?;
    if blob {
        return Ok(DbCell::Binary {
            bytes: bytes.len() as u64,
        });
    }
    Ok(marsa_core::db_browser::cut_cell(bytes, limit))
}
