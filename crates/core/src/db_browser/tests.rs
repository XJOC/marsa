use std::path::Path;

use super::*;
use crate::types::{DbServer, DbSort, DbTableRef};

const ENGINES: [Engine; 3] = [Engine::Mysql, Engine::Postgres, Engine::Sqlite];

fn table(name: &str) -> DbTableRef {
    DbTableRef {
        schema: None,
        name: name.into(),
    }
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn quote_of(engine: Engine) -> char {
    match engine {
        Engine::Mysql => '`',
        Engine::Postgres | Engine::Sqlite => '"',
    }
}

/// `quoted` is one identifier: it starts and ends with the quote, and every
/// quote between them is doubled, so nothing inside can end it early.
fn is_one_identifier(quoted: &str, quote: char) -> bool {
    let Some(inner) = quoted
        .strip_prefix(quote)
        .and_then(|rest| rest.strip_suffix(quote))
    else {
        return false;
    };
    let pair: String = [quote, quote].iter().collect();
    !inner.replace(&pair, "").contains(quote)
}

#[test]
fn a_name_is_quoted_for_its_engine() {
    assert_eq!(quote_ident(Engine::Mysql, "users").unwrap(), "`users`");
    assert_eq!(quote_ident(Engine::Postgres, "users").unwrap(), "\"users\"");
    assert_eq!(quote_ident(Engine::Sqlite, "users").unwrap(), "\"users\"");
    assert_eq!(quote_ident(Engine::Mysql, "we`ird").unwrap(), "`we``ird`");
    assert_eq!(
        quote_ident(Engine::Postgres, "we\"ird").unwrap(),
        "\"we\"\"ird\""
    );
    // The other engine's quote is an ordinary character.
    assert_eq!(quote_ident(Engine::Mysql, "a\"b").unwrap(), "`a\"b`");
    assert_eq!(quote_ident(Engine::Sqlite, "a`b").unwrap(), "\"a`b\"");
    assert_eq!(quote_ident(Engine::Postgres, "طلبات").unwrap(), "\"طلبات\"");
}

#[test]
fn an_injection_in_a_name_stays_one_quoted_name() {
    let attempts = [
        "x\"; DROP TABLE y; --",
        "x`; DROP TABLE y; --",
        "x'; DROP TABLE y; --",
        "users` WHERE 1=1; --",
        "\" OR \"\"=\"",
        "``",
        "\"\"",
        "a]; DROP TABLE y; --",
        "x */ DROP TABLE y; /*",
        "x\nDROP TABLE y",
        "$$; DROP TABLE y; $$",
    ];
    for engine in ENGINES {
        for name in attempts {
            let quoted = quote_ident(engine, name).unwrap();
            assert!(
                is_one_identifier(&quoted, quote_of(engine)),
                "{engine:?} {name:?} became {quoted}"
            );
        }
    }
}

#[test]
fn an_empty_long_or_nul_name_is_refused() {
    let longest = "a".repeat(MAX_NAME_BYTES);
    for engine in ENGINES {
        assert!(quote_ident(engine, &longest).is_ok());
        for name in [String::new(), "a".repeat(MAX_NAME_BYTES + 1), "a\0b".into()] {
            let err = quote_ident(engine, &name).unwrap_err();
            assert_eq!(err, SqlError::BadName);
            assert_eq!(err.code(), crate::codes::DB_SOURCE_INVALID);
        }
    }
}

#[test]
fn only_a_postgres_table_has_a_schema() {
    let log = DbTableRef {
        schema: Some("audit".into()),
        name: "log".into(),
    };
    assert_eq!(
        table_name(Engine::Postgres, &log).unwrap(),
        "\"audit\".\"log\""
    );
    for engine in [Engine::Mysql, Engine::Sqlite] {
        assert_eq!(table_name(engine, &log), Err(SqlError::SchemaNotAllowed));
    }
    let empty_schema = DbTableRef {
        schema: Some(String::new()),
        name: "log".into(),
    };
    assert_eq!(
        table_name(Engine::Postgres, &empty_schema),
        Err(SqlError::BadName)
    );
}

/// Each statement of a table's reads, one per line, for a snapshot.
fn statements(engine: Engine) -> String {
    let users = table("users");
    let id = names(&["id"]);
    let by_email = DbSort {
        column: "email".into(),
        descending: true,
    };
    let by_id = DbSort {
        column: "id".into(),
        descending: false,
    };
    let mut lines = vec![
        page_sql(engine, &users, &id, None, 0).unwrap(),
        page_sql(engine, &users, &id, Some(&by_email), 500).unwrap(),
        page_sql(engine, &users, &id, Some(&by_id), u64::MAX).unwrap(),
        page_sql(engine, &users, &names(&["order_id", "line"]), None, 1000).unwrap(),
        page_sql(engine, &table("events"), &[], None, 99_500).unwrap(),
        page_sql(engine, &table("events"), &[], Some(&by_email), 0).unwrap(),
        count_sql(engine, &users).unwrap(),
        cell_sql(engine, &users, "bio", &id).unwrap(),
        cell_sql(engine, &users, "note", &names(&["order_id", "line"])).unwrap(),
    ];
    if engine == Engine::Postgres {
        let log = DbTableRef {
            schema: Some("audit".into()),
            name: "log".into(),
        };
        lines.push(page_sql(engine, &log, &id, None, 0).unwrap());
        lines.push(count_sql(engine, &log).unwrap());
    }
    lines.join("\n")
}

#[test]
fn the_table_statements_of_each_engine() {
    insta::assert_snapshot!("statements_mysql", statements(Engine::Mysql));
    insta::assert_snapshot!("statements_postgres", statements(Engine::Postgres));
    insta::assert_snapshot!("statements_sqlite", statements(Engine::Sqlite));
}

#[test]
fn the_catalog_statements_of_each_engine() {
    let catalog = |engine: Engine, server: Option<DbServer>| {
        let mut lines: Vec<&str> = server.into_iter().map(databases_sql).collect();
        lines.extend([tables_sql(engine), columns_sql(engine)]);
        lines.join("\n")
    };
    insta::assert_snapshot!(
        "catalog_mysql",
        catalog(Engine::Mysql, Some(DbServer::Mysql))
    );
    insta::assert_snapshot!(
        "catalog_postgres",
        catalog(Engine::Postgres, Some(DbServer::Postgres))
    );
    insta::assert_snapshot!("catalog_sqlite", catalog(Engine::Sqlite, None));
}

#[test]
fn a_bad_name_anywhere_stops_the_statement() {
    let users = table("users");
    let bad = String::from("a\0b");
    let sort = DbSort {
        column: bad.clone(),
        descending: false,
    };
    for engine in ENGINES {
        assert!(page_sql(engine, &table(&bad), &[], None, 0).is_err());
        assert!(page_sql(engine, &users, std::slice::from_ref(&bad), None, 0).is_err());
        assert!(page_sql(engine, &users, &[], Some(&sort), 0).is_err());
        assert!(count_sql(engine, &table("")).is_err());
        assert!(cell_sql(engine, &users, &bad, &names(&["id"])).is_err());
        assert!(cell_sql(engine, &users, "bio", std::slice::from_ref(&bad)).is_err());
    }
}

#[test]
fn a_value_is_read_by_primary_key_only() {
    for engine in ENGINES {
        assert_eq!(
            cell_sql(engine, &table("events"), "body", &[]),
            Err(SqlError::NoKey)
        );
    }
}

#[test]
fn a_short_text_stays_whole() {
    assert_eq!(cut_cell(b"", 4), DbCell::Text(String::new()));
    assert_eq!(cut_cell(b"abcd", 4), DbCell::Text("abcd".into()));
}

#[test]
fn a_long_text_is_cut_at_a_character_and_keeps_its_length() {
    assert_eq!(
        cut_cell(b"abcdef", 4),
        DbCell::Cut {
            text: "abcd".into(),
            bytes: 6
        }
    );
    // "é" is two bytes: a cut at 3 would split the second one.
    assert_eq!(
        cut_cell("ééé".as_bytes(), 3),
        DbCell::Cut {
            text: "é".into(),
            bytes: 6
        }
    );
    // "😀" is four bytes: a limit under it leaves no character.
    assert_eq!(
        cut_cell("😀".as_bytes(), 3),
        DbCell::Cut {
            text: String::new(),
            bytes: 4
        }
    );
    let long = "x".repeat(CELL_BYTES * 4);
    let DbCell::Cut { text, bytes } = cut_cell(long.as_bytes(), CELL_BYTES) else {
        panic!("not cut");
    };
    assert_eq!((text.len(), bytes), (CELL_BYTES, 1024));
}

#[test]
fn bytes_that_are_not_utf8_keep_only_their_length() {
    assert_eq!(
        cut_cell(&[0xff, 0x00, 0x41], 256),
        DbCell::Binary { bytes: 3 }
    );
    // Not text even when the start would be.
    let mut tail = b"abc".to_vec();
    tail.push(0xc3);
    assert_eq!(cut_cell(&tail, 2), DbCell::Binary { bytes: 4 });
}

#[test]
fn the_sqlite_file_is_laravels_default() {
    let root = Path::new("site");
    assert_eq!(
        sqlite_file(root),
        root.join("database").join("database.sqlite")
    );
    assert_eq!(sqlite_file_shown(), "database/database.sqlite");
}

#[test]
fn each_source_speaks_its_engine() {
    let server = |server| crate::types::DbSource::Server {
        server,
        database: "app".into(),
    };
    assert_eq!(server(DbServer::Mysql).engine(), Engine::Mysql);
    assert_eq!(server(DbServer::Postgres).engine(), Engine::Postgres);
    let site = crate::types::DbSource::SiteFile {
        site: "blog".into(),
    };
    assert_eq!(site.engine(), Engine::Sqlite);
}
