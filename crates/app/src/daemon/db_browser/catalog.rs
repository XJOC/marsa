//! The rows of the catalog statements (`marsa_core::db_browser`), read into
//! the API's types. Every column of those statements is text or null, so one
//! reader serves the three engines.

use marsa_core::types::{DbTableInfo, DbTableKind, DbTableRef, DbTables};
use sqlx::{ColumnIndex, Decode, Row, Type};

use super::unavailable;
use crate::daemon::http::routes::HttpError;

/// The rows of `tables_sql`: schema, name, kind, and estimate.
pub(super) fn tables<R>(rows: &[R]) -> Result<DbTables, HttpError>
where
    R: Row,
    for<'r> String: Decode<'r, R::Database> + Type<R::Database>,
    usize: ColumnIndex<R>,
{
    let mut tables = Vec::with_capacity(rows.len());
    for row in rows {
        let kind = match text(row, 2)?.as_deref() {
            Some("table") => DbTableKind::Table,
            Some("view") => DbTableKind::View,
            _ => return Err(unreadable()),
        };
        tables.push(DbTableInfo {
            table: DbTableRef {
                schema: text(row, 0)?,
                name: text(row, 1)?.ok_or_else(unreadable)?,
            },
            kind,
            // Digits, or null when the server keeps no estimate.
            estimate: text(row, 3)?.and_then(|rows| rows.parse().ok()),
        });
    }
    Ok(DbTables { tables })
}

/// Column `index` of `row`, as text or null.
fn text<R>(row: &R, index: usize) -> Result<Option<String>, HttpError>
where
    R: Row,
    for<'r> String: Decode<'r, R::Database> + Type<R::Database>,
    usize: ColumnIndex<R>,
{
    row.try_get::<Option<String>, usize>(index)
        .map_err(|err| unavailable(err.to_string()))
}

fn unreadable() -> HttpError {
    unavailable("the catalog gave a row this daemon cannot read".to_owned())
}
