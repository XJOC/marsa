//! The rows of the catalog statements (`marsa_core::db_browser`), read into
//! the API's types. Every column of those statements is text or null, so one
//! reader serves the three engines.

use std::pin::pin;

use axum::http::StatusCode;
use marsa_core::codes;
use marsa_core::db_browser::CATALOG_ENTRIES;
use marsa_core::types::{DbColumn, DbKey, DbTableInfo, DbTableKind, DbTableRef};
use sqlx::{ColumnIndex, Decode, Row, Type};
use tokio_stream::{Stream, StreamExt};

use super::unavailable;
use crate::daemon::http::routes::HttpError;

/// Reads a list of databases or of tables, one `entry` for each row, as the
/// rows arrive: at most [`CATALOG_ENTRIES`], so that a catalog's size does
/// not become `marsad`'s. One row more is an error. `failed` says what the
/// driver's error becomes.
pub(super) async fn list<R, T>(
    rows: impl Stream<Item = Result<R, sqlx::Error>>,
    failed: fn(sqlx::Error) -> HttpError,
    entry: fn(&R) -> Result<T, HttpError>,
) -> Result<Vec<T>, HttpError> {
    let mut rows = pin!(rows);
    let mut entries = Vec::new();
    while let Some(row) = rows.next().await {
        let row = row.map_err(failed)?;
        if entries.len() == CATALOG_ENTRIES {
            return Err(too_large());
        }
        entries.push(entry(&row)?);
    }
    Ok(entries)
}

/// A 5xx on purpose: the server connection it was read on still has rows on
/// the way, so it is closed, not kept.
fn too_large() -> HttpError {
    HttpError::new(
        StatusCode::BAD_GATEWAY,
        codes::DB_CATALOG_TOO_LARGE,
        format!("there are more than {CATALOG_ENTRIES} databases or tables"),
    )
}

/// A row of `databases_sql`: the name.
pub(super) fn database<R>(row: &R) -> Result<Option<String>, HttpError>
where
    R: Row,
    for<'r> String: Decode<'r, R::Database> + Type<R::Database>,
    usize: ColumnIndex<R>,
{
    text(row, 0)
}

/// A row of `tables_sql`: schema, name, kind, and estimate.
pub(super) fn table<R>(row: &R) -> Result<DbTableInfo, HttpError>
where
    R: Row,
    for<'r> String: Decode<'r, R::Database> + Type<R::Database>,
    usize: ColumnIndex<R>,
{
    let kind = match text(row, 2)?.as_deref() {
        Some("table") => DbTableKind::Table,
        Some("view") => DbTableKind::View,
        _ => return Err(unreadable()),
    };
    Ok(DbTableInfo {
        table: DbTableRef {
            schema: text(row, 0)?,
            name: text(row, 1)?.ok_or_else(unreadable)?,
        },
        kind,
        // Digits, or null when the server keeps no estimate.
        estimate: text(row, 3)?.and_then(|rows| rows.parse().ok()),
    })
}

/// The rows of `columns_sql`: the columns in their order, and the names
/// of the primary key's columns in the key's order.
pub(super) fn columns<R>(rows: &[R]) -> Result<(Vec<DbColumn>, Vec<String>), HttpError>
where
    R: Row,
    for<'r> String: Decode<'r, R::Database> + Type<R::Database>,
    usize: ColumnIndex<R>,
{
    let mut columns = Vec::with_capacity(rows.len());
    let mut key: Vec<(u32, String)> = Vec::new();
    for row in rows {
        let name = text(row, 0)?.ok_or_else(unreadable)?;
        let key_kind = match text(row, 4)?.as_deref() {
            Some("primary") => Some(DbKey::Primary),
            Some("unique") => Some(DbKey::Unique),
            None => None,
            Some(_) => return Err(unreadable()),
        };
        let place: u32 = text(row, 5)?
            .and_then(|place| place.parse().ok())
            .ok_or_else(unreadable)?;
        if place > 0 {
            key.push((place, name.clone()));
        }
        columns.push(DbColumn {
            name,
            type_name: text(row, 1)?.unwrap_or_default(),
            nullable: text(row, 2)?.as_deref() == Some("1"),
            default: text(row, 3)?,
            key: key_kind,
        });
    }
    key.sort();
    Ok((columns, key.into_iter().map(|(_, name)| name).collect()))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn number(row: &usize) -> Result<usize, HttpError> {
        Ok(*row)
    }

    #[tokio::test]
    async fn a_list_of_exactly_the_limit_is_whole() {
        let rows = tokio_stream::iter((0..CATALOG_ENTRIES).map(Ok));
        let found = list(rows, super::super::servers::db_error, number)
            .await
            .unwrap();
        assert_eq!(found.len(), CATALOG_ENTRIES);
    }

    /// The rows never end here: the error comes while they are read.
    #[tokio::test]
    async fn one_row_more_stops_the_read_with_an_error() {
        let rows = tokio_stream::iter((0..).map(Ok));
        let err = list(rows, super::super::servers::db_error, number)
            .await
            .unwrap_err();
        assert_eq!(err.code, codes::DB_CATALOG_TOO_LARGE);
        assert!(err.status.is_server_error());
    }
}
