use std::path::Path;

use super::split::{KEYWORDS, Lexer};
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
fn table_statements(engine: Engine) -> String {
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
    insta::assert_snapshot!("statements_mysql", table_statements(Engine::Mysql));
    insta::assert_snapshot!("statements_postgres", table_statements(Engine::Postgres));
    insta::assert_snapshot!("statements_sqlite", table_statements(Engine::Sqlite));
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

/// The text of each statement `statements` finds.
fn split(engine: Engine, text: &str) -> Vec<String> {
    statements(engine, text)
        .into_iter()
        .map(|range| text[range].to_owned())
        .collect()
}

#[test]
fn a_semicolon_in_a_string_or_comment_does_not_end_the_statement() {
    for engine in ENGINES {
        assert_eq!(
            split(engine, "SELECT 1; SELECT ';' -- x;"),
            ["SELECT 1", "SELECT ';' -- x;"],
            "{engine:?}"
        );
        assert_eq!(
            split(engine, "SELECT 'it''s; ok' ; SELECT 2;"),
            ["SELECT 'it''s; ok'", "SELECT 2"],
            "{engine:?}"
        );
        assert_eq!(
            split(engine, "SELECT \"a;b\" FROM t /* ; */;\nSELECT 2"),
            ["SELECT \"a;b\" FROM t /* ; */", "SELECT 2"],
            "{engine:?}"
        );
    }
    assert_eq!(
        split(Engine::Mysql, "SELECT `a;b` FROM t"),
        ["SELECT `a;b` FROM t"]
    );
    assert_eq!(
        split(Engine::Sqlite, "SELECT `a;b` FROM t"),
        ["SELECT `a;b` FROM t"]
    );
}

#[test]
fn each_engine_has_its_own_escapes_and_comments() {
    // MySQL: `\'` stays in the text. PostgreSQL and SQLite: `\` is a letter.
    let backslash = r"SELECT 'a\';b'";
    assert_eq!(split(Engine::Mysql, backslash).len(), 1);
    assert_eq!(split(Engine::Postgres, backslash).len(), 2);
    assert_eq!(split(Engine::Sqlite, backslash).len(), 2);
    // PostgreSQL's E'...' escapes like MySQL.
    assert_eq!(split(Engine::Postgres, r"SELECT E'a\';b'").len(), 1);
    // `#` is a comment in MySQL only (PostgreSQL's XOR).
    assert_eq!(split(Engine::Mysql, "SELECT 1 # a; b\nFROM t").len(), 1);
    assert_eq!(split(Engine::Postgres, "SELECT 1 # 2; SELECT 3").len(), 2);
    // MySQL needs a space after `--`; the others do not.
    assert_eq!(split(Engine::Mysql, "SELECT 1 --x; SELECT 2").len(), 2);
    assert_eq!(split(Engine::Postgres, "SELECT 1 --x; SELECT 2").len(), 1);
    // Only PostgreSQL nests block comments.
    let nested = "/* a /* b */ ; */ SELECT 1";
    assert_eq!(split(Engine::Postgres, nested), [nested]);
    assert_eq!(split(Engine::Mysql, nested), ["*/ SELECT 1"]);
}

#[test]
fn a_dollar_quote_hides_its_semicolons_in_postgres_only() {
    let body = "CREATE FUNCTION f() RETURNS int AS $$ SELECT 1; $$ LANGUAGE sql; SELECT 2";
    assert_eq!(split(Engine::Postgres, body).len(), 2);
    let tagged = "DO $body$ BEGIN PERFORM 1; END $body$; SELECT 2";
    assert_eq!(
        split(Engine::Postgres, tagged),
        ["DO $body$ BEGIN PERFORM 1; END $body$", "SELECT 2"]
    );
    // `$1` is a parameter and `a$b$` a name, not a quote.
    assert_eq!(split(Engine::Postgres, "SELECT $1; SELECT 2").len(), 2);
    assert_eq!(split(Engine::Postgres, "SELECT a$b$; SELECT 2").len(), 2);
    assert_eq!(split(Engine::Mysql, "SELECT '$$'; SELECT $$").len(), 2);
}

#[test]
fn comments_and_space_alone_are_no_statement() {
    for engine in ENGINES {
        assert!(split(engine, "").is_empty());
        assert!(split(engine, "  ;  ;\n").is_empty());
        assert!(split(engine, "-- just a note").is_empty());
        assert!(split(engine, "/* a */ ; /* b */").is_empty());
        assert_eq!(split(engine, "SELECT 1; -- done"), ["SELECT 1"]);
    }
}

#[test]
fn what_the_lexer_does_not_close_stays_one_statement() {
    for engine in ENGINES {
        assert_eq!(
            split(engine, "SELECT 'abc; SELECT 2"),
            ["SELECT 'abc; SELECT 2"]
        );
        assert_eq!(
            split(engine, "SELECT 1 /* ; SELECT 2"),
            ["SELECT 1 /* ; SELECT 2"]
        );
    }
    assert_eq!(split(Engine::Postgres, "SELECT $$; SELECT 2").len(), 1);
}

#[test]
fn the_statement_at_the_cursor() {
    let text = "SELECT 1;\n\nSELECT 'é;';  \n";
    let at = |cursor| statement_at(Engine::Mysql, text, cursor).map(|range| &text[range]);
    assert_eq!(at(0), Some("SELECT 1"));
    assert_eq!(at(4), Some("SELECT 1"));
    // Right after the `;`: still the statement before it.
    assert_eq!(at(9), Some("SELECT 1"));
    assert_eq!(at(10), Some("SELECT 'é;'"));
    assert_eq!(at(15), Some("SELECT 'é;'"));
    // In the space after the last statement, and past the end.
    assert_eq!(at(text.len()), Some("SELECT 'é;'"));
    assert_eq!(at(text.len() + 10), Some("SELECT 'é;'"));
    // Before any code: the statement after.
    let leading = "-- note\n;SELECT 2";
    assert_eq!(
        statement_at(Engine::Mysql, leading, 2).map(|range| &leading[range]),
        Some("SELECT 2")
    );
    assert_eq!(statement_at(Engine::Mysql, "", 0), None);
    assert_eq!(statement_at(Engine::Mysql, " -- x", 3), None);
}

fn kinds(engine: Engine, line: &str, state: &LineState) -> (Vec<(TokenKind, String)>, LineState) {
    let (tokens, end) = tokens(engine, line, state);
    let words = tokens
        .into_iter()
        .map(|token| (token.kind, line[token.range].to_owned()))
        .collect();
    (words, end)
}

#[test]
fn a_line_is_split_into_colored_tokens() {
    use TokenKind::*;
    let (words, end) = kinds(
        Engine::Postgres,
        "select \"Name\", 'x', 4.5e-3 from t -- all",
        &LineState::Code,
    );
    let expected = [
        (Keyword, "select"),
        (Other, "\"Name\""),
        (Other, ","),
        (String, "'x'"),
        (Other, ","),
        (Number, "4.5e-3"),
        (Keyword, "from"),
        (Other, "t"),
        (Comment, "-- all"),
    ];
    let expected: Vec<_> = expected
        .iter()
        .map(|(k, w)| (*k, (*w).to_owned()))
        .collect();
    assert_eq!(words, expected);
    assert_eq!(end, LineState::Code);
    // A MySQL `"..."` is a text, not a name.
    let (words, _) = kinds(Engine::Mysql, "\"x\"", &LineState::Code);
    assert_eq!(words, [(String, "\"x\"".to_owned())]);
}

#[test]
fn a_line_starts_where_the_line_before_ended() {
    use TokenKind::*;
    let (_, state) = kinds(Engine::Postgres, "SELECT 'it", &LineState::Code);
    assert_eq!(
        state,
        LineState::Quoted {
            quote: b'\'',
            backslash: false
        }
    );
    let (words, state) = kinds(Engine::Postgres, "s' FROM t", &state);
    assert_eq!(words[0], (String, "s'".to_owned()));
    assert_eq!(words[1], (Keyword, "FROM".to_owned()));
    assert_eq!(state, LineState::Code);

    let (_, state) = kinds(Engine::Postgres, "/* a /* b */", &LineState::Code);
    assert_eq!(state, LineState::Comment { depth: 1 });
    let (_, state) = kinds(Engine::Postgres, "AS $fn$ SELECT 1;", &LineState::Code);
    assert_eq!(state, LineState::Dollar { tag: "fn".into() });
    let (words, state) = kinds(Engine::Postgres, "$fn$ LANGUAGE sql", &state);
    assert_eq!(words[0], (String, "$fn$".to_owned()));
    assert_eq!(state, LineState::Code);
    // An empty line keeps the state.
    let open = LineState::Comment { depth: 2 };
    assert_eq!(
        tokens(Engine::Postgres, "", &open),
        (Vec::new(), open.clone())
    );
}

/// Lexing line by line, each line from the state the one before ended in,
/// gives each byte the kind lexing the whole text gives it.
#[test]
fn lines_lexed_alone_agree_with_the_whole_text() {
    let text = "SELECT 'a\nb;' AS \"x\ny\", $t$ 1;\n2 $t$ /* c\n/* d */ e */ -- f\n\
                FROM `t` # g\nWHERE n = 1e+5; SELECT E'\\'\n' 'é😀' ;";
    for engine in ENGINES {
        let mut whole = vec![None; text.len()];
        for token in Lexer::new(engine, text, LineState::Code).tokens() {
            assert!(text.is_char_boundary(token.range.start));
            assert!(text.is_char_boundary(token.range.end));
            whole[token.range].fill(Some(token.kind));
        }
        let mut by_line = vec![None; text.len()];
        let mut state = LineState::Code;
        let mut offset = 0;
        for line in text.split('\n') {
            let (tokens, end) = tokens(engine, line, &state);
            for token in tokens {
                by_line[offset + token.range.start..offset + token.range.end]
                    .fill(Some(token.kind));
            }
            state = end;
            offset += line.len() + 1;
        }
        // The newlines inside a token belong to it in the whole text only.
        for (at, byte) in text.bytes().enumerate() {
            if byte != b'\n' {
                assert_eq!(whole[at], by_line[at], "{engine:?} at {at}");
            }
        }
    }
}

#[test]
fn the_keywords_are_sorted_for_the_search() {
    assert!(KEYWORDS.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(
        KEYWORDS
            .iter()
            .all(|word| *word == word.to_ascii_uppercase())
    );
}
