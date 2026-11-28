//! The SQL text of the browser's reads of a table: a page, a count, and one
//! value. Every statement is fixed text, or fixed text around names that
//! went through [`quote_ident`] and numbers from `u64`. Values are bound.

use std::fmt::Write as _;

use super::{Engine, PAGE_ROWS, SqlError};
use crate::types::{DbSort, DbTableRef};

/// The longest name the browser quotes. MySQL allows 64 characters and
/// PostgreSQL 63 bytes, so a longer one is not a real name.
pub const MAX_NAME_BYTES: usize = 255;

/// `name` as one quoted identifier: `` `name` `` for MySQL, `"name"` for
/// PostgreSQL and SQLite. A quote inside the name is doubled, so no name can
/// end the identifier early.
pub fn quote_ident(engine: Engine, name: &str) -> Result<String, SqlError> {
    if name.is_empty() || name.len() > MAX_NAME_BYTES || name.contains('\0') {
        return Err(SqlError::BadName);
    }
    let quote = match engine {
        Engine::Mysql => '`',
        Engine::Postgres | Engine::Sqlite => '"',
    };
    let mut quoted = String::with_capacity(name.len() + 2);
    quoted.push(quote);
    for c in name.chars() {
        if c == quote {
            quoted.push(quote);
        }
        quoted.push(c);
    }
    quoted.push(quote);
    Ok(quoted)
}

/// The table's name in a statement: `"schema"."name"` in PostgreSQL, the
/// quoted name alone elsewhere.
pub fn table_name(engine: Engine, table: &DbTableRef) -> Result<String, SqlError> {
    let name = quote_ident(engine, &table.name)?;
    match (&table.schema, engine) {
        (None, _) => Ok(name),
        (Some(schema), Engine::Postgres) => Ok(format!("{}.{name}", quote_ident(engine, schema)?)),
        (Some(_), Engine::Mysql | Engine::Sqlite) => Err(SqlError::SchemaNotAllowed),
    }
}

/// One page: [`PAGE_ROWS`] rows after `offset`, in the order of `sort` and
/// then the primary key `key`, so the order is the same from page to page. A
/// table with no key and no sort has no `ORDER BY`: its order can change.
pub fn page_sql(
    engine: Engine,
    table: &DbTableRef,
    key: &[String],
    sort: Option<&DbSort>,
    offset: u64,
) -> Result<String, SqlError> {
    let mut order = Vec::new();
    if let Some(sort) = sort {
        let column = quote_ident(engine, &sort.column)?;
        order.push(if sort.descending {
            format!("{column} DESC")
        } else {
            column
        });
    }
    for column in key {
        // Sorting by a key column again would change nothing.
        if sort.is_none_or(|sort| sort.column != *column) {
            order.push(quote_ident(engine, column)?);
        }
    }
    let mut sql = format!("SELECT * FROM {}", table_name(engine, table)?);
    if !order.is_empty() {
        let _ = write!(sql, " ORDER BY {}", order.join(", "));
    }
    // PostgreSQL and SQLite read `OFFSET` as a signed 64-bit number. A larger
    // one is past the end of any table, as `i64::MAX` is.
    let offset = offset.min(i64::MAX.unsigned_abs());
    let _ = write!(sql, " LIMIT {PAGE_ROWS} OFFSET {offset}");
    Ok(sql)
}

/// The exact number of rows. The daemon gives it a short time limit, and uses
/// the catalog's estimate when it does not finish.
pub fn count_sql(engine: Engine, table: &DbTableRef) -> Result<String, SqlError> {
    let mut sql = format!("SELECT COUNT(*) FROM {}", table_name(engine, table)?);
    // Without a condition SQLite counts a table in one step, which its
    // progress handler cannot stop. With one, it walks the rows.
    if engine == Engine::Sqlite {
        sql.push_str(" WHERE 1");
    }
    Ok(sql)
}

/// One whole value: `column` of the row whose primary key columns `key` equal
/// the bound values, one for each column, in order. PostgreSQL compares the
/// key as text, because the window has its text only and a bound text does
/// not compare with a number there; MySQL and SQLite turn the bound text
/// into the column's type themselves.
///
/// A bound statement answers in each server's binary form, so MySQL and
/// PostgreSQL give the value cast to text, or, for a `binary` column, only
/// its length in bytes. SQLite gives the value itself.
pub fn cell_sql(
    engine: Engine,
    table: &DbTableRef,
    column: &str,
    key: &[String],
    binary: bool,
) -> Result<String, SqlError> {
    if key.is_empty() {
        return Err(SqlError::NoKey);
    }
    let column = quote_ident(engine, column)?;
    let value = match (engine, binary) {
        (Engine::Mysql, true) => format!("OCTET_LENGTH({column})"),
        (Engine::Mysql, false) => format!("CAST({column} AS CHAR)"),
        (Engine::Postgres, true) => format!("octet_length({column})::bigint"),
        (Engine::Postgres, false) => format!("{column}::text"),
        (Engine::Sqlite, _) => column,
    };
    let mut sql = format!("SELECT {value} FROM {} WHERE ", table_name(engine, table)?);
    for (index, name) in key.iter().enumerate() {
        if index > 0 {
            sql.push_str(" AND ");
        }
        let name = quote_ident(engine, name)?;
        let _ = match engine {
            Engine::Postgres => write!(sql, "{name}::text = ${}", index + 1),
            Engine::Mysql | Engine::Sqlite => write!(sql, "{name} = ?"),
        };
    }
    Ok(sql)
}
