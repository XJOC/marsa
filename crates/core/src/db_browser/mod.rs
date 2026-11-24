//! The database browser's pure part (ADR 0028): the limits, the SQL text
//! `marsad` sends for the browser's own reads (the catalog, and a table's
//! pages and values), the splitter of the user's SQL into statements, and
//! the cut of a cell.
//!
//! Nothing here opens a connection or reads a file: it takes data and returns
//! data. No value from the user is ever written into SQL text. Names are
//! quoted with [`quote_ident`] after the daemon found them in the server's
//! catalog, numbers come from `u64`, and every other value is bound.

mod catalog;
mod split;
mod sql;

use std::path::{Path, PathBuf};

pub use catalog::*;
pub use split::*;
pub use sql::*;

use crate::codes;
use crate::types::{DbCell, DbServer, DbSource};

/// Rows in one page of a table.
pub const PAGE_ROWS: u64 = 500;
/// Bytes of one cell in a page, cut at a character.
pub const CELL_BYTES: usize = 256;
/// Bytes of one whole value (`POST /v1/db/cell`), and of one cell in a SQL
/// result.
pub const VALUE_BYTES: usize = 64 * 1024;
/// Rows of a SQL result.
pub const RESULT_ROWS: usize = 1_000;
/// Cell text of a SQL result.
pub const RESULT_BYTES: usize = 8 * 1024 * 1024;
/// Length of one SQL statement of the user's.
pub const SQL_BYTES: usize = 256 * 1024;

/// Where Laravel keeps its SQLite file, inside a site's folder. It is the
/// only place the browser looks: `marsad` does not read a project's
/// environment file, where another path could be set.
const SQLITE_FILE: [&str; 2] = ["database", "database.sqlite"];

/// The site's SQLite file, under its folder. The daemon still checks that it
/// is a regular file and that it stays inside the folder once links are
/// followed.
pub fn sqlite_file(root: &Path) -> PathBuf {
    SQLITE_FILE
        .iter()
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

/// [`sqlite_file`] inside the site's folder, as the window shows it.
pub fn sqlite_file_shown() -> String {
    SQLITE_FILE.join("/")
}

/// The SQL dialect a source speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Engine {
    Mysql,
    Postgres,
    Sqlite,
}

impl From<DbServer> for Engine {
    fn from(server: DbServer) -> Self {
        match server {
            DbServer::Mysql => Self::Mysql,
            DbServer::Postgres => Self::Postgres,
        }
    }
}

impl DbSource {
    pub fn engine(&self) -> Engine {
        match self {
            Self::Server { server, .. } => (*server).into(),
            Self::SiteFile { .. } => Engine::Sqlite,
        }
    }
}

/// A request the browser's SQL cannot be built for. The daemon checks names
/// against the catalog first, so this is a second guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SqlError {
    /// Empty, longer than [`MAX_NAME_BYTES`], or holding a NUL. The name is
    /// not in the message: it came from the user.
    #[error("a table, schema, or column name is empty, too long, or holds a NUL")]
    BadName,
    /// A schema with MySQL or SQLite, where a table is always in the source's
    /// own database.
    #[error("only PostgreSQL tables have a schema")]
    SchemaNotAllowed,
    /// A whole value is read by primary key, and this table has none.
    #[error("the table has no primary key")]
    NoKey,
}

impl SqlError {
    pub fn code(&self) -> &'static str {
        codes::DB_SOURCE_INVALID
    }
}

/// A value read from the server as `bytes`, cut to `limit` bytes at a
/// character. A value that is not UTF-8 text keeps only its length. A null
/// is the caller's [`DbCell::Null`].
pub fn cut_cell(bytes: &[u8], limit: usize) -> DbCell {
    let whole = bytes.len() as u64;
    let Ok(text) = std::str::from_utf8(bytes) else {
        return DbCell::Binary { bytes: whole };
    };
    if text.len() <= limit {
        return DbCell::Text(text.to_owned());
    }
    DbCell::Cut {
        text: text[..text.floor_char_boundary(limit)].to_owned(),
        bytes: whole,
    }
}

#[cfg(test)]
mod tests;
