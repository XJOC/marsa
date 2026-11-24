//! The database browser (ADR 0028): what the window and `marsa db` ask
//! `marsad`, and what it answers. A request names a source from a closed list
//! of local things. It never carries a host, a port, a user, a password, a
//! URL, or a file path, so every request type refuses a field it does not
//! know.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::ServiceState;
use crate::codes;
use crate::databases::ServerKind;

/// A SQL server of Marsa's own that the browser can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbServer {
    Postgres,
    Mysql,
}

impl DbServer {
    /// In the order lists show them, as [`ServerKind::ALL`].
    pub const ALL: [Self; 2] = [Self::Postgres, Self::Mysql];

    /// The service that runs it.
    pub fn kind(self) -> ServerKind {
        match self {
            Self::Postgres => ServerKind::Postgres,
            Self::Mysql => ServerKind::Mysql,
        }
    }
}

/// What a request opens: a database of a Marsa server, or a site's SQLite
/// file.
///
/// On the command line it is written `mysql/shop`, `postgres/app`, or
/// `site/blog` ([`fmt::Display`] and [`FromStr`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DbSource {
    Server {
        server: DbServer,
        database: String,
    },
    /// The SQLite file of a site in Marsa's site list.
    SiteFile {
        site: String,
    },
}

/// The command-line word before the `/` of a site's SQLite file.
const SITE_PREFIX: &str = "site";

impl fmt::Display for DbSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Server { server, database } => write!(f, "{}/{database}", server.kind().id()),
            Self::SiteFile { site } => write!(f, "{SITE_PREFIX}/{site}"),
        }
    }
}

/// A source the command line cannot read: no `/`, an unknown word before it,
/// or nothing after it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a source is written mysql/<database>, postgres/<database>, or site/<site>")]
pub struct DbSourceInvalid;

impl DbSourceInvalid {
    pub fn code(&self) -> &'static str {
        codes::DB_SOURCE_INVALID
    }
}

impl FromStr for DbSource {
    type Err = DbSourceInvalid;

    /// The first `/` ends the word, so a database name may hold a `/`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (word, name) = text.split_once('/').ok_or(DbSourceInvalid)?;
        if name.is_empty() {
            return Err(DbSourceInvalid);
        }
        if word == SITE_PREFIX {
            return Ok(Self::SiteFile { site: name.into() });
        }
        let server = DbServer::ALL
            .into_iter()
            .find(|server| server.kind().id() == word)
            .ok_or(DbSourceInvalid)?;
        Ok(Self::Server {
            server,
            database: name.into(),
        })
    }
}

/// A table or view. `schema` is PostgreSQL's (`public` and others), and is
/// empty for MySQL (the source's database) and SQLite.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DbTableRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub name: String,
}

/// `GET /v1/db/sources`: everything the browser can open.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbSources {
    /// Each installed server, in [`DbServer::ALL`] order.
    pub servers: Vec<DbServerInfo>,
    /// The sites that have a SQLite file, by site name.
    pub files: Vec<DbSiteFile>,
}

/// One installed server in [`DbSources`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbServerInfo {
    pub server: DbServer,
    /// The installed version, such as `8.4.3`.
    pub version: String,
    #[serde(flatten)]
    pub state: ServiceState,
    /// Its databases, by name, without the system ones. Empty unless it runs.
    pub databases: Vec<DbDatabase>,
    /// An error code when it runs but its databases could not be listed,
    /// such as `db_sign_in_failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// One database of a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbDatabase {
    pub name: String,
}

/// A site's SQLite file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbSiteFile {
    pub site: String,
    /// Inside the site's folder, with `/`, for display only:
    /// `database/database.sqlite`.
    pub path: String,
}

/// `POST /v1/db/tables`: the tables and views of a source, by schema and
/// name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbTables {
    pub tables: Vec<DbTableInfo>,
}

/// One entry of [`DbTables`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbTableInfo {
    pub table: DbTableRef,
    pub kind: DbTableKind,
    /// The server's own estimate of its rows, when it keeps one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbTableKind {
    Table,
    View,
}

/// `POST /v1/db/table`: the columns of a table, in their order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbTable {
    pub columns: Vec<DbColumn>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbColumn {
    pub name: String,
    /// As the server writes it, such as `varchar(255)` or `bigint`.
    pub type_name: String,
    pub nullable: bool,
    /// The default as the server writes it: an expression, not a value.
    pub default: Option<String>,
    pub key: Option<DbKey>,
}

/// The key a column is part of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbKey {
    Primary,
    /// A unique index on this column alone.
    Unique,
}

/// `POST /v1/db/rows`: one page of a table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbPage {
    pub columns: Vec<DbPageColumn>,
    /// The columns of the primary key, as indexes into `columns`, in the
    /// key's order. Empty when the table has none: its order can change.
    pub key: Vec<u32>,
    /// Each row has one cell for each column.
    pub rows: Vec<Vec<DbCell>>,
    /// The number of rows before this page.
    pub offset: u64,
    pub total: DbTotal,
}

/// A column of a page or a SQL result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbPageColumn {
    pub name: String,
    pub kind: DbColumnKind,
}

/// How the window shows a column's cells, from its type's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbColumnKind {
    Text,
    /// Drawn at the end of its cell.
    Number,
    Time,
    Binary,
    Other,
}

/// The rows of a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbTotal {
    pub rows: u64,
    /// `false` when counting took too long and `rows` is the server's
    /// estimate.
    pub exact: bool,
}

/// One value, as text.
///
/// On the wire it stays small, because a page has thousands: `null`, a
/// string, `{"text": "...", "bytes": n}` for a text that was cut, and
/// `{"bytes": n}` for a binary value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbCell {
    Null,
    Text(String),
    /// The start of a longer text, cut at a character. `bytes` is the length
    /// of the whole value.
    Cut {
        text: String,
        bytes: u64,
    },
    /// A value that is not UTF-8 text, such as a BLOB: only its length
    /// travels.
    Binary {
        bytes: u64,
    },
}

impl Serialize for DbCell {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_none(),
            Self::Text(text) => serializer.serialize_str(text),
            Self::Cut { text, bytes } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("text", text)?;
                map.serialize_entry("bytes", bytes)?;
                map.end()
            }
            Self::Binary { bytes } => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("bytes", bytes)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for DbCell {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(CellVisitor)
    }
}

struct CellVisitor;

impl<'de> Visitor<'de> for CellVisitor {
    type Value = DbCell;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("null, a string, or an object with \"bytes\"")
    }

    fn visit_unit<E: de::Error>(self) -> Result<DbCell, E> {
        Ok(DbCell::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<DbCell, E> {
        Ok(DbCell::Null)
    }

    fn visit_str<E: de::Error>(self, text: &str) -> Result<DbCell, E> {
        Ok(DbCell::Text(text.to_owned()))
    }

    fn visit_string<E: de::Error>(self, text: String) -> Result<DbCell, E> {
        Ok(DbCell::Text(text))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<DbCell, A::Error> {
        let mut text: Option<String> = None;
        let mut bytes: Option<u64> = None;
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "text" if text.is_none() => text = Some(map.next_value()?),
                "bytes" if bytes.is_none() => bytes = Some(map.next_value()?),
                "text" | "bytes" => return Err(de::Error::duplicate_field("text or bytes")),
                other => return Err(de::Error::unknown_field(other, &["text", "bytes"])),
            }
        }
        let bytes = bytes.ok_or_else(|| de::Error::missing_field("bytes"))?;
        Ok(match text {
            Some(text) => DbCell::Cut { text, bytes },
            None => DbCell::Binary { bytes },
        })
    }
}

/// `POST /v1/db/cell`: one whole value, up to `db_browser::VALUE_BYTES`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbValue {
    pub cell: DbCell,
}

/// The rows of a finished SQL statement: `GET /v1/db/results/{id}`. Not
/// paged: the first `db_browser::RESULT_ROWS` rows, or fewer when their text
/// passes `db_browser::RESULT_BYTES`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbResult {
    pub columns: Vec<DbPageColumn>,
    pub rows: Vec<Vec<DbCell>>,
    /// The statement gave more rows than were kept.
    pub cut: bool,
    pub elapsed_ms: u64,
}
/// `POST /v1/db/tables`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DbTablesRequest {
    pub source: DbSource,
}

/// `POST /v1/db/table`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DbTableRequest {
    pub source: DbSource,
    pub table: DbTableRef,
}

/// `POST /v1/db/rows`: the page that starts after `offset` rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DbRowsRequest {
    pub source: DbSource,
    pub table: DbTableRef,
    pub offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<DbSort>,
}

/// One column to sort a page by, before the primary key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DbSort {
    pub column: String,
    #[serde(default)]
    pub descending: bool,
}

/// `POST /v1/db/cell`: the whole value of `column` in the row whose primary
/// key is `key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DbCellRequest {
    pub source: DbSource,
    pub table: DbTableRef,
    pub column: String,
    /// The text of each primary key column, in the key's order
    /// ([`DbPage::key`]).
    pub key: Vec<String>,
}

/// `POST /v1/jobs/db/query`: one statement of the user's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DbQueryRequest {
    pub source: DbSource,
    pub sql: String,
}

#[cfg(test)]
mod db_browser_tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    fn round_trip<T>(value: &T, expected: Value)
    where
        T: Serialize + DeserializeOwned + PartialEq + fmt::Debug,
    {
        assert_eq!(serde_json::to_value(value).unwrap(), expected);
        let back: T = serde_json::from_value(expected).unwrap();
        assert_eq!(&back, value);
    }

    fn shop() -> DbSource {
        DbSource::Server {
            server: DbServer::Mysql,
            database: "shop".into(),
        }
    }

    fn users() -> DbTableRef {
        DbTableRef {
            schema: None,
            name: "users".into(),
        }
    }

    #[test]
    fn a_source_names_its_kind() {
        round_trip(
            &shop(),
            json!({ "kind": "server", "server": "mysql", "database": "shop" }),
        );
        round_trip(
            &DbSource::SiteFile {
                site: "blog".into(),
            },
            json!({ "kind": "site_file", "site": "blog" }),
        );
    }

    #[test]
    fn a_source_is_written_with_a_slash_on_the_command_line() {
        let cases = [
            ("mysql/shop", shop()),
            (
                "postgres/app",
                DbSource::Server {
                    server: DbServer::Postgres,
                    database: "app".into(),
                },
            ),
            (
                "site/blog",
                DbSource::SiteFile {
                    site: "blog".into(),
                },
            ),
        ];
        for (text, source) in cases {
            assert_eq!(text.parse::<DbSource>(), Ok(source.clone()), "{text}");
            assert_eq!(source.to_string(), text);
        }
        // Only the first slash ends the word.
        assert_eq!(
            "postgres/a/b".parse::<DbSource>(),
            Ok(DbSource::Server {
                server: DbServer::Postgres,
                database: "a/b".into(),
            })
        );
    }

    #[test]
    fn a_source_with_an_unknown_word_or_no_name_is_refused() {
        for text in [
            "shop",
            "mysql/",
            "/shop",
            "valkey/x",
            "redis/x",
            "MySQL/shop",
            "",
        ] {
            let err = text.parse::<DbSource>().unwrap_err();
            assert_eq!(err.code(), codes::DB_SOURCE_INVALID, "{text}");
        }
    }

    #[test]
    fn a_cell_is_small_on_the_wire() {
        round_trip(&DbCell::Null, json!(null));
        round_trip(&DbCell::Text("Ada".into()), json!("Ada"));
        // The text "NULL" and the empty text are not a null.
        round_trip(&DbCell::Text("NULL".into()), json!("NULL"));
        round_trip(&DbCell::Text(String::new()), json!(""));
        round_trip(
            &DbCell::Cut {
                text: "Lorem".into(),
                bytes: 70_000,
            },
            json!({ "text": "Lorem", "bytes": 70_000 }),
        );
        round_trip(&DbCell::Binary { bytes: 16 }, json!({ "bytes": 16 }));
    }

    #[test]
    fn a_cell_of_another_shape_is_refused() {
        for value in [
            json!(5),
            json!(true),
            json!(["a"]),
            json!({ "text": "a" }),
            json!({ "bytes": 1, "path": "/etc" }),
            json!({ "bytes": -1 }),
        ] {
            assert!(
                serde_json::from_value::<DbCell>(value.clone()).is_err(),
                "{value}"
            );
        }
        // `json!` keeps the last of two equal keys, so this one is text.
        for text in [
            r#"{"bytes": 1, "bytes": 2}"#,
            r#"{"text": "a", "text": "b", "bytes": 1}"#,
        ] {
            assert!(serde_json::from_str::<DbCell>(text).is_err(), "{text}");
        }
    }

    #[test]
    fn a_page_round_trips() {
        let page = DbPage {
            columns: vec![
                DbPageColumn {
                    name: "id".into(),
                    kind: DbColumnKind::Number,
                },
                DbPageColumn {
                    name: "avatar".into(),
                    kind: DbColumnKind::Binary,
                },
            ],
            key: vec![0],
            rows: vec![vec![DbCell::Text("1".into()), DbCell::Binary { bytes: 9 }]],
            offset: 500,
            total: DbTotal {
                rows: 12_408,
                exact: false,
            },
        };
        round_trip(
            &page,
            json!({
                "columns": [
                    { "name": "id", "kind": "number" },
                    { "name": "avatar", "kind": "binary" },
                ],
                "key": [0],
                "rows": [["1", { "bytes": 9 }]],
                "offset": 500,
                "total": { "rows": 12_408, "exact": false },
            }),
        );
    }

    #[test]
    fn a_server_shows_its_state_and_problem() {
        let sources = DbSources {
            servers: vec![DbServerInfo {
                server: DbServer::Postgres,
                version: "17.2".into(),
                state: ServiceState::Running,
                databases: vec![DbDatabase { name: "app".into() }],
                problem: Some(codes::DB_SIGN_IN_FAILED.into()),
            }],
            files: vec![DbSiteFile {
                site: "blog".into(),
                path: "database/database.sqlite".into(),
            }],
        };
        round_trip(
            &sources,
            json!({
                "servers": [{
                    "server": "postgres",
                    "version": "17.2",
                    "state": "running",
                    "databases": [{ "name": "app" }],
                    "problem": "db_sign_in_failed",
                }],
                "files": [{ "site": "blog", "path": "database/database.sqlite" }],
            }),
        );
    }

    #[test]
    fn a_table_entry_and_a_column_round_trip() {
        round_trip(
            &DbTableInfo {
                table: DbTableRef {
                    schema: Some("audit".into()),
                    name: "log".into(),
                },
                kind: DbTableKind::View,
                estimate: None,
            },
            json!({ "table": { "schema": "audit", "name": "log" }, "kind": "view" }),
        );
        round_trip(
            &DbTableInfo {
                table: users(),
                kind: DbTableKind::Table,
                estimate: Some(12_400),
            },
            json!({ "table": { "name": "users" }, "kind": "table", "estimate": 12_400 }),
        );
        round_trip(
            &DbColumn {
                name: "id".into(),
                type_name: "bigint unsigned".into(),
                nullable: false,
                default: None,
                key: Some(DbKey::Primary),
            },
            json!({
                "name": "id",
                "type_name": "bigint unsigned",
                "nullable": false,
                "default": null,
                "key": "primary",
            }),
        );
    }

    #[test]
    fn a_table_outside_postgres_has_no_schema() {
        round_trip(&users(), json!({ "name": "users" }));
        round_trip(
            &DbTableRef {
                schema: Some("audit".into()),
                name: "log".into(),
            },
            json!({ "schema": "audit", "name": "log" }),
        );
    }

    #[test]
    fn the_requests_round_trip() {
        round_trip(
            &DbRowsRequest {
                source: shop(),
                table: users(),
                offset: 1000,
                sort: Some(DbSort {
                    column: "email".into(),
                    descending: true,
                }),
            },
            json!({
                "source": { "kind": "server", "server": "mysql", "database": "shop" },
                "table": { "name": "users" },
                "offset": 1000,
                "sort": { "column": "email", "descending": true },
            }),
        );
        round_trip(
            &DbCellRequest {
                source: shop(),
                table: users(),
                column: "bio".into(),
                key: vec!["7".into()],
            },
            json!({
                "source": { "kind": "server", "server": "mysql", "database": "shop" },
                "table": { "name": "users" },
                "column": "bio",
                "key": ["7"],
            }),
        );
    }

    /// No request can say where to connect, as whom, or which file to open:
    /// a field the type does not know is an error, at every level.
    #[test]
    fn a_request_with_a_connection_field_is_refused() {
        let source = json!({ "kind": "server", "server": "mysql", "database": "shop" });
        let table = json!({ "name": "users" });
        let requests = [
            json!({ "source": source }),
            json!({ "source": source, "table": table }),
            json!({ "source": source, "table": table, "offset": 0 }),
            json!({ "source": source, "table": table, "column": "bio", "key": ["1"] }),
            json!({ "source": source, "sql": "SELECT 1" }),
        ];
        let parses: [fn(Value) -> bool; 5] = [
            |v| serde_json::from_value::<DbTablesRequest>(v).is_ok(),
            |v| serde_json::from_value::<DbTableRequest>(v).is_ok(),
            |v| serde_json::from_value::<DbRowsRequest>(v).is_ok(),
            |v| serde_json::from_value::<DbCellRequest>(v).is_ok(),
            |v| serde_json::from_value::<DbQueryRequest>(v).is_ok(),
        ];
        for (request, parses) in requests.iter().zip(parses) {
            assert!(parses(request.clone()), "{request}");
            for field in ["host", "port", "user", "password", "path", "url"] {
                let mut top = request.clone();
                top[field] = json!("x");
                assert!(!parses(top), "{field} at the top of {request}");
                let mut in_source = request.clone();
                in_source["source"][field] = json!("x");
                assert!(!parses(in_source), "{field} in the source of {request}");
                if request.get("table").is_some() {
                    let mut in_table = request.clone();
                    in_table["table"][field] = json!("x");
                    assert!(!parses(in_table), "{field} in the table of {request}");
                }
            }
        }
        let with_sort = json!({
            "source": source,
            "table": table,
            "offset": 0,
            "sort": { "column": "id", "path": "/x" },
        });
        assert!(serde_json::from_value::<DbRowsRequest>(with_sort).is_err());
        let site_with_path =
            json!({ "source": { "kind": "site_file", "site": "blog", "path": "/x" } });
        assert!(serde_json::from_value::<DbTablesRequest>(site_with_path).is_err());
    }
}
