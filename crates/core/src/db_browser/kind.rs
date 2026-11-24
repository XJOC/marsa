//! How the window shows a column's cells, from the name of its type as the
//! catalog writes it. Only for display: a wrong guess draws a number at the
//! start of its cell, nothing worse.

use super::Engine;
use crate::types::DbColumnKind;

/// The kind of a column of type `type_name` (any case, with or without a
/// length, such as `varchar(255)` or `bigint unsigned`).
pub fn column_kind(engine: Engine, type_name: &str) -> DbColumnKind {
    let name = type_name.trim().to_ascii_lowercase();
    if engine == Engine::Sqlite {
        return sqlite_kind(&name);
    }
    // The first word, without its length: `timestamp(6) with time zone` is
    // `timestamp`, `double precision` is `double`.
    let base = name
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or_default();
    match base {
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "int2" | "int4"
        | "int8" | "decimal" | "numeric" | "float" | "float4" | "float8" | "double" | "real"
        | "serial" | "bigserial" | "smallserial" | "money" | "bit" | "year" => DbColumnKind::Number,
        "date" | "time" | "timetz" | "datetime" | "timestamp" | "timestamptz" | "interval" => {
            DbColumnKind::Time
        }
        "binary" | "varbinary" | "blob" | "tinyblob" | "mediumblob" | "longblob" | "bytea" => {
            DbColumnKind::Binary
        }
        "char" | "varchar" | "character" | "text" | "tinytext" | "mediumtext" | "longtext"
        | "enum" | "set" | "json" | "jsonb" | "uuid" | "citext" | "xml" | "name" => {
            DbColumnKind::Text
        }
        _ => DbColumnKind::Other,
    }
}

/// SQLite's own rules for a declared type (its "type affinity"), with dates
/// first: Laravel declares them `datetime` or `date`, whose affinity is
/// numeric, though the values are text.
fn sqlite_kind(name: &str) -> DbColumnKind {
    if name.contains("date") || name.contains("time") {
        DbColumnKind::Time
    } else if name.contains("int") {
        DbColumnKind::Number
    } else if name.contains("char") || name.contains("clob") || name.contains("text") {
        DbColumnKind::Text
    } else if name.contains("blob") {
        DbColumnKind::Binary
    } else if ["real", "floa", "doub", "num", "dec"]
        .iter()
        .any(|part| name.contains(part))
    {
        DbColumnKind::Number
    } else {
        DbColumnKind::Other
    }
}
