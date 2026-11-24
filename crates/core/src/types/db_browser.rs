//! The database browser (ADR 0028): what the window and `marsa db` ask
//! `marsad`, and what it answers. A request names a source from a closed list
//! of local things. It never carries a host, a port, a user, a password, a
//! URL, or a file path, so every request type refuses a field it does not
//! know.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

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
    /// (`DbPage::key`).
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
