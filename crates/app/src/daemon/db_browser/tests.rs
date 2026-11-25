use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection};
use tempfile::TempDir;

use marsa_core::types::DbServer;

use super::{servers, sqlite};
use crate::daemon::http::test_router::TestRouter;
use crate::daemon::state::Daemon;

const ROUTES: [(&str, &str); 3] = [
    ("GET", "/v1/db/sources"),
    ("POST", "/v1/db/tables"),
    ("POST", "/v1/db/close"),
];

pub(super) struct App {
    pub(super) dir: TempDir,
    pub(super) daemon: Arc<Daemon>,
    pub(super) router: TestRouter,
}

impl App {
    pub(super) fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = dir.path().join("data");
        fs::create_dir_all(&data).unwrap();
        let daemon = Arc::new(Daemon::new(data));
        let router = TestRouter::new(Arc::clone(&daemon));
        Self {
            dir,
            daemon,
            router,
        }
    }

    /// A linked site `name` with an empty project folder.
    pub(super) async fn site(&self, name: &str) -> PathBuf {
        let root = self.dir.path().join("code").join(name);
        fs::create_dir_all(&root).unwrap();
        let body = json!({ "name": name, "path": root });
        let (status, _) = self
            .router
            .answer("POST", "/v1/sites/link", Some(body))
            .await;
        assert_eq!(status, StatusCode::OK);
        root
    }

    async fn tables(&self, source: Value) -> (StatusCode, Value) {
        let body = json!({ "source": source });
        self.router
            .answer("POST", "/v1/db/tables", Some(body))
            .await
    }
}

pub(super) fn blog() -> Value {
    json!({ "kind": "site_file", "site": "blog" })
}

/// Makes the site's SQLite file with `sql`, through a connection that may
/// write, as the site's own app would.
pub(super) async fn make_file(root: &Path, sql: &'static str) -> PathBuf {
    let file = root.join("database").join("database.sqlite");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let mut connection = SqliteConnectOptions::new()
        .filename(&file)
        .create_if_missing(true)
        .connect()
        .await
        .unwrap();
    sqlx::raw_sql(sql).execute(&mut connection).await.unwrap();
    connection.close().await.unwrap();
    file
}

const SHOP: &str = "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT UNIQUE);\
                    INSERT INTO users (email) VALUES ('ada@example.test');\
                    CREATE VIEW emails AS SELECT email FROM users;";

#[tokio::test]
async fn every_route_refuses_a_request_without_auth() {
    let app = App::new();
    for (method, path) in ROUTES {
        app.router.assert_refused_without_auth(method, path).await;
    }
}

#[tokio::test]
async fn with_no_site_and_no_server_there_is_nothing_to_open() {
    let app = App::new();
    let (status, json) = app.router.answer("GET", "/v1/db/sources", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json, json!({ "servers": [], "files": [] }));
}

/// A server that is installed and stopped is listed with its state, and no
/// connection is tried: the browser is not even made.
#[tokio::test]
async fn an_installed_server_that_does_not_run_is_listed_without_a_connection() {
    let app = App::new();
    let version = app
        .dir
        .path()
        .join("data")
        .join("runtimes")
        .join("mysql")
        .join("8.4.13");
    fs::create_dir_all(version).unwrap();
    let (status, json) = app.router.answer("GET", "/v1/db/sources", None).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let servers = json["servers"].as_array().unwrap();
    assert_eq!(servers.len(), 1, "{json}");
    assert_eq!(servers[0]["server"], "mysql");
    assert_eq!(servers[0]["state"], "stopped");
    assert_eq!(servers[0]["databases"], json!([]));
    assert!(servers[0].get("problem").is_none(), "{json}");
    assert!(app.daemon.open_db_browser().is_none());

    let body = json!({ "kind": "server", "server": "mysql", "database": "shop" });
    let (status, json) = app.tables(body).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["error"]["code"], "db_server_not_running");
    assert!(app.daemon.open_db_browser().is_none());

    // Close makes nothing either.
    app.router.send("POST", "/v1/db/close", None).await;
    assert!(app.daemon.open_db_browser().is_none());
}

#[tokio::test]
async fn a_sites_sqlite_file_is_listed_and_its_tables_read() {
    let app = App::new();
    let root = app.site("blog").await;
    app.site("api").await;
    make_file(&root, SHOP).await;

    let (status, json) = app.router.answer("GET", "/v1/db/sources", None).await;
    assert_eq!(status, StatusCode::OK);
    let file = json!({ "site": "blog", "path": "database/database.sqlite" });
    assert_eq!(json["files"], json!([file]));

    let (status, json) = app.tables(blog()).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(
        json,
        json!({ "tables": [
            { "table": { "name": "emails" }, "kind": "view" },
            { "table": { "name": "users" }, "kind": "table" },
        ] })
    );
}

/// The engine refuses every write, and the file's bytes stay the same.
#[tokio::test]
async fn the_browser_never_writes_the_file() {
    let app = App::new();
    let root = app.site("blog").await;
    let file = make_file(&root, SHOP).await;
    let before = fs::read(&file).unwrap();

    let mut connection = sqlite::open(&file).await.unwrap();
    for sql in [
        "CREATE TABLE notes (body TEXT)",
        "INSERT INTO users (email) VALUES ('eve@example.test')",
        "UPDATE users SET email = 'x'",
        "DELETE FROM users",
        "DROP TABLE users",
        "PRAGMA user_version = 7",
    ] {
        let err = sqlx::query(sql).execute(&mut connection).await.unwrap_err();
        let code = err.as_database_error().and_then(|db| db.code());
        // SQLITE_READONLY is 8.
        assert_eq!(code.as_deref(), Some("8"), "{sql}: {err}");
    }
    let err = sqlx::query("SELECT load_extension('nothing')")
        .execute(&mut connection)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not authorized"), "{err}");
    connection.close().await.unwrap();

    let (status, json) = app.tables(blog()).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(fs::read(&file).unwrap(), before);
}

/// The names in `folder`, sorted.
fn listing(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A database in WAL mode: its bytes stay the same, and the only new files
/// are SQLite's `-wal` (empty) and `-shm`, which a read-only connection
/// makes and cannot remove.
#[tokio::test]
async fn a_wal_database_keeps_its_bytes_and_gets_only_its_sidecars() {
    let app = App::new();
    let root = app.site("blog").await;
    let file = make_file(
        &root,
        "PRAGMA journal_mode = WAL; CREATE TABLE users (id INTEGER PRIMARY KEY);",
    )
    .await;
    let folder = file.parent().unwrap();
    // The app closed its last connection, which removed its WAL files.
    assert_eq!(listing(folder), ["database.sqlite"]);
    let before = fs::read(&file).unwrap();
    // Bytes 18 and 19 of the header are 2 in WAL mode: the test is not a
    // copy of the rollback-journal one.
    assert_eq!(before[18..20], [2, 2]);

    let (status, json) = app.tables(blog()).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(fs::read(&file).unwrap(), before);
    for name in listing(folder) {
        assert!(
            [
                "database.sqlite",
                "database.sqlite-shm",
                "database.sqlite-wal"
            ]
            .contains(&name.as_str()),
            "{name}"
        );
    }
    let wal = folder.join("database.sqlite-wal");
    if wal.exists() {
        assert_eq!(fs::metadata(&wal).unwrap().len(), 0);
    }
}

#[tokio::test]
async fn a_missing_file_is_not_made() {
    let app = App::new();
    let root = app.site("blog").await;
    let (status, json) = app.tables(blog()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json["error"]["code"], "db_file_not_found");
    assert!(!root.join("database").exists());

    let (_, json) = app.router.answer("GET", "/v1/db/sources", None).await;
    assert_eq!(json["files"], json!([]));

    // The open itself never makes the file either: it guards the moment
    // between the check and the open.
    let missing = root.join("missing.sqlite");
    let err = sqlite::open(&missing).await.unwrap_err();
    assert_eq!(err.code, "db_unavailable");
    assert!(!missing.exists());
    let relative = sqlite::open(Path::new("database.sqlite"))
        .await
        .unwrap_err();
    assert_eq!(
        relative.detail.as_deref(),
        Some("the SQLite path is not absolute")
    );
}

#[tokio::test]
async fn a_file_that_is_not_a_regular_file_inside_the_site_is_refused() {
    let app = App::new();
    let root = app.site("blog").await;
    // A folder where the file should be.
    fs::create_dir_all(root.join("database").join("database.sqlite")).unwrap();
    let (status, json) = app.tables(blog()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["error"]["code"], "db_file_outside_site");
    let (_, json) = app.router.answer("GET", "/v1/db/sources", None).await;
    assert_eq!(json["files"], json!([]));
}

// A link out of the site's folder. `#[cfg]` outside `platform`: making a
// symbolic link on Windows needs a right the test runner may not have.
#[cfg(unix)]
#[tokio::test]
async fn a_link_out_of_the_site_is_refused() {
    let app = App::new();
    let root = app.site("blog").await;
    let other = app.site("other").await;
    let outside = make_file(&other, SHOP).await;
    fs::create_dir_all(root.join("database")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("database").join("database.sqlite")).unwrap();

    let (status, json) = app.tables(blog()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["error"]["code"], "db_file_outside_site");

    // A link that stays inside the site is followed.
    fs::remove_file(root.join("database").join("database.sqlite")).unwrap();
    let inside = make_file(&root.join("storage"), SHOP).await;
    std::os::unix::fs::symlink(&inside, root.join("database").join("database.sqlite")).unwrap();
    let (status, json) = app.tables(blog()).await;
    assert_eq!(status, StatusCode::OK, "{json}");
}

#[tokio::test]
async fn a_bad_request_is_refused_before_any_file_is_opened() {
    let app = App::new();
    app.site("blog").await;
    let cases = [
        (
            json!({ "source": blog(), "path": "/etc/passwd" }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_body",
        ),
        (
            json!({ "source": { "kind": "site_file", "site": "blog", "path": "/x" } }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_body",
        ),
        (
            json!({ "source": { "kind": "site_file", "site": "../blog" } }),
            StatusCode::BAD_REQUEST,
            "site_name_invalid",
        ),
        (
            json!({ "source": { "kind": "site_file", "site": "shop" } }),
            StatusCode::NOT_FOUND,
            "site_not_found",
        ),
        (
            json!({ "source": { "kind": "server", "server": "mysql", "database": "shop" } }),
            StatusCode::CONFLICT,
            "db_server_not_running",
        ),
    ];
    for (body, status, code) in cases {
        let (got, json) = app
            .router
            .answer("POST", "/v1/db/tables", Some(body.clone()))
            .await;
        assert_eq!(
            (got, json["error"]["code"].as_str()),
            (status, Some(code)),
            "{body}"
        );
    }
}

/// What the database says goes to the client as `detail`, never in
/// `message`, which is for logs.
#[tokio::test]
async fn a_file_that_is_not_a_database_is_unavailable_with_the_reason() {
    let app = App::new();
    let root = app.site("blog").await;
    let file = root.join("database").join("database.sqlite");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(
        &file,
        "not a database, just text that is long enough to be a page".repeat(20),
    )
    .unwrap();
    let (status, json) = app.tables(blog()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(json["error"]["code"], "db_unavailable");
    assert_eq!(json["error"]["message"], "the database could not be read");
    let detail = json["detail"].as_str().unwrap();
    assert!(detail.contains("not a database"), "{detail}");
}

#[tokio::test]
async fn close_answers_with_no_content() {
    let app = App::new();
    let response = app.router.send("POST", "/v1/db/close", None).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

/// Collects every log line, of every thread, in memory.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The log has the source and the request, and nothing that came from the
/// database: not a table's name, a value, what the database said, or the
/// browser's own SQL (the driver logs every statement unless told not to).
#[tokio::test]
async fn the_log_holds_no_name_or_value_from_the_database() {
    // The files are made first: the connection that writes them logs its
    // SQL, as any connection of the driver does by default.
    let app = App::new();
    let root = app.site("blog").await;
    make_file(
        &root,
        "CREATE TABLE secret_table_x (v TEXT); INSERT INTO secret_table_x VALUES ('secret_value_y');",
    )
    .await;
    let other = app.site("other").await;
    let file = other.join("database").join("database.sqlite");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "secret_text_z ".repeat(100)).unwrap();

    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    // Global, so the driver's own thread is caught too. nextest runs each
    // test in its own process, so no other test sees it.
    tracing::subscriber::set_global_default(subscriber).unwrap();

    app.router.answer("GET", "/v1/db/sources", None).await;
    let (status, _) = app.tables(blog()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = app
        .tables(json!({ "kind": "site_file", "site": "other" }))
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);

    let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("database browser"), "{log}");
    assert!(log.contains("site/blog"), "{log}");
    for secret in [
        "secret_table_x",
        "secret_value_y",
        "secret_text_z",
        "not a database",
        "sqlite_master",
    ] {
        assert!(!log.contains(secret), "{secret} is in the log: {log}");
    }

    // The control: this capture does see the driver's statement log, from a
    // connection that keeps the driver's default.
    let mut connection = SqliteConnectOptions::new()
        .filename(root.join("database").join("database.sqlite"))
        .read_only(true)
        .connect()
        .await
        .unwrap();
    sqlx::query("SELECT count(*) FROM secret_table_x")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("SELECT count(*) FROM secret_table_x"), "{log}");
}

/// A server's stop closes the browser's connections to it first, and keeps
/// the others.
#[tokio::test]
async fn a_server_stop_closes_its_browser_sources_first() {
    let app = App::new();
    let store = &app.daemon.db_browser().store;
    store.slot(&(DbServer::Mysql, Some("shop".to_owned())));
    store.slot(&(DbServer::Mysql, None));
    store.slot(&(DbServer::Postgres, Some("app".to_owned())));
    let (status, json) = app
        .router
        .answer("POST", "/v1/services/mysql/stop", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(store.open(), [(DbServer::Postgres, Some("app".to_owned()))]);
}

/// What a server's error becomes: its text goes to `detail` only.
mod server_errors {
    use std::borrow::Cow;
    use std::error::Error as StdError;
    use std::fmt;

    use sqlx::error::{DatabaseError, ErrorKind};

    use super::super::servers::db_error;

    #[derive(Debug)]
    struct Fake(&'static str);

    impl fmt::Display for Fake {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("the server's own words")
        }
    }

    impl StdError for Fake {}

    impl DatabaseError for Fake {
        fn message(&self) -> &str {
            "the server's own words"
        }
        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.0))
        }
        fn as_error(&self) -> &(dyn StdError + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn StdError + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn StdError + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }
    }

    fn mapped(code: &'static str) -> (u16, &'static str, String, String) {
        let err = db_error(sqlx::Error::Database(Box::new(Fake(code))));
        (
            err.status.as_u16(),
            err.code,
            err.message,
            err.detail.unwrap_or_default(),
        )
    }

    #[test]
    fn a_refused_sign_in_is_its_own_code() {
        for code in ["28P01", "28000"] {
            let (status, code, message, detail) = mapped(code);
            assert_eq!((status, code), (502, "db_sign_in_failed"));
            assert!(!message.contains("own words"), "{message}");
            assert!(detail.contains("own words"), "{detail}");
        }
    }

    #[test]
    fn the_servers_time_limit_is_a_timeout() {
        let (status, code, _, detail) = mapped("57014");
        assert_eq!((status, code), (504, "db_timeout"));
        assert!(detail.contains("own words"), "{detail}");
    }

    /// `PGOPTIONS` would reach the startup packet: the browser refuses.
    #[test]
    fn startup_options_from_the_environment_are_refused() {
        use sqlx::postgres::PgConnectOptions;

        use super::super::servers::no_startup_options;

        let plain = PgConnectOptions::new_without_pgpass();
        if plain.get_options().is_none() {
            assert!(no_startup_options(&plain).is_ok());
        }
        let with = plain.options([("search_path", "elsewhere")]);
        let err = no_startup_options(&with).unwrap_err();
        assert_eq!(err.code, "db_unavailable");
    }

    #[test]
    fn any_other_failure_is_unavailable_with_the_reason() {
        let (status, code, message, detail) = mapped("42P01");
        assert_eq!((status, code), (502, "db_unavailable"));
        assert_eq!(message, "the database could not be read");
        assert!(detail.contains("own words"), "{detail}");
    }
}

/// `POST /v1/db/rows` on the site `blog`'s `table` (no schema).
async fn page(app: &App, table: &str, extra: Value) -> (StatusCode, Value) {
    let mut body = json!({ "source": blog(), "table": { "name": table }, "offset": 0 });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    app.router.answer("POST", "/v1/db/rows", Some(body)).await
}

const NUMBERED: &str = "CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT NOT NULL, \
     score REAL, body BLOB, created datetime);\
     INSERT INTO posts (id, title, score, body, created) \
     WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 600) \
     SELECT i, 'post ' || i, i / 2.0, x'00ff', '2026-11-24 10:00:00' FROM n;";

#[tokio::test]
async fn a_tables_columns_come_with_their_key() {
    let app = App::new();
    make_file(&app.site("blog").await, SHOP).await;
    let body = json!({ "source": blog(), "table": { "name": "users" } });
    let (status, json) = app.router.answer("POST", "/v1/db/table", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(
        json,
        json!({ "columns": [
            { "name": "id", "type_name": "INTEGER", "nullable": false, "default": null, "key": "primary" },
            { "name": "email", "type_name": "TEXT", "nullable": true, "default": null, "key": "unique" },
        ] })
    );
}

#[tokio::test]
async fn a_page_is_500_rows_in_the_keys_order() {
    let app = App::new();
    make_file(&app.site("blog").await, NUMBERED).await;
    let (status, json) = page(&app, "posts", json!({ "count": true })).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["rows"].as_array().unwrap().len(), 500);
    assert_eq!(json["key"], json!([0]));
    assert_eq!(json["total"], json!({ "rows": 600, "exact": true }));
    assert_eq!(
        json["columns"],
        json!([
            { "name": "id", "kind": "number" },
            { "name": "title", "kind": "text" },
            { "name": "score", "kind": "number" },
            { "name": "body", "kind": "binary" },
            { "name": "created", "kind": "time" },
        ])
    );
    // Values as SQLite writes them; a BLOB as its size only.
    assert_eq!(
        json["rows"][0],
        json!(["1", "post 1", "0.5", { "bytes": 2 }, "2026-11-24 10:00:00"])
    );
    assert_eq!(json["rows"][499][0], "500");

    // Without a count, the total is what the pages prove, marked as such.
    let (_, json) = page(&app, "posts", json!({ "offset": 500 })).await;
    assert_eq!(json["rows"].as_array().unwrap().len(), 100);
    assert_eq!(json["rows"][0][0], "501");
    assert_eq!(json["total"], json!({ "rows": 600, "exact": false }));

    let (status, json) = page(&app, "posts", json!({ "offset": 600 })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["rows"], json!([]));
}

#[tokio::test]
async fn a_page_sorts_by_a_column_then_the_key() {
    let app = App::new();
    make_file(&app.site("blog").await, NUMBERED).await;
    let sort = json!({ "sort": { "column": "id", "descending": true } });
    let (_, json) = page(&app, "posts", sort).await;
    assert_eq!(json["rows"][0][0], "600");
    let sort = json!({ "sort": { "column": "title" } });
    let (_, json) = page(&app, "posts", sort).await;
    // Text order: "post 1", "post 10", "post 100", ...
    assert_eq!(json["rows"][1][1], "post 10");

    let sort = json!({ "sort": { "column": "nope" } });
    let (status, json) = page(&app, "posts", sort).await;
    assert_eq!(
        (status, json["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("db_source_invalid"))
    );
    let (status, json) = page(&app, "missing", json!({})).await;
    assert_eq!(
        (status, json["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("db_table_not_found"))
    );
    let body =
        json!({ "source": blog(), "table": { "schema": "main", "name": "posts" }, "offset": 0 });
    let (status, _) = app.router.answer("POST", "/v1/db/rows", Some(body)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn every_cell_shape_and_a_table_with_no_key() {
    let app = App::new();
    make_file(
        &app.site("blog").await,
        "CREATE TABLE notes (body TEXT, n INTEGER);\
         INSERT INTO notes VALUES (NULL, 1), ('NULL', 2), ('', 3), (replace(hex(zeroblob(200)), '0', 'é'), 4), (x'ff00', 5);",
    )
    .await;
    let (status, json) = page(&app, "notes", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["key"], json!([]));
    let cells: Vec<Value> = json["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row[0].clone())
        .collect();
    assert_eq!(cells[0], Value::Null);
    assert_eq!(cells[1], "NULL");
    assert_eq!(cells[2], "");
    // 400 "é" are 800 bytes: cut at a character within 256.
    assert_eq!(cells[3]["bytes"], 800);
    let cut = cells[3]["text"].as_str().unwrap();
    assert_eq!((cut.len(), cut.chars().all(|c| c == 'é')), (256, true));
    assert_eq!(cells[4], json!({ "bytes": 2 }));
}

/// Page after page, every row comes once and in order.
#[tokio::test]
async fn walking_10000_rows_gives_each_once_in_order() {
    let app = App::new();
    make_file(
        &app.site("blog").await,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, label TEXT);\
         INSERT INTO t (id, label) WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 10000) \
         SELECT i, 'row ' || i FROM n;",
    )
    .await;
    let mut ids = Vec::new();
    let mut offset = 0;
    loop {
        let (status, json) = page(&app, "t", json!({ "offset": offset })).await;
        assert_eq!(status, StatusCode::OK);
        let rows = json["rows"].as_array().unwrap();
        if rows.is_empty() {
            break;
        }
        ids.extend(
            rows.iter()
                .map(|row| row[0].as_str().unwrap().parse::<u32>().unwrap()),
        );
        offset += rows.len();
    }
    assert_eq!(ids, (1..=10_000).collect::<Vec<_>>());
}

#[tokio::test]
async fn names_with_spaces_quotes_semicolons_and_arabic_are_one_name() {
    let app = App::new();
    make_file(
        &app.site("blog").await,
        r#"CREATE TABLE "we""ird; t" ("مفتاح" INTEGER PRIMARY KEY, "a b`c" TEXT);
           INSERT INTO "we""ird; t" VALUES (2, 'two'), (1, 'one');"#,
    )
    .await;
    let (status, json) = page(&app, "we\"ird; t", json!({ "sort": { "column": "a b`c" } })).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["columns"][0]["name"], "مفتاح");
    assert_eq!(json["rows"], json!([["1", "one"], ["2", "two"]]));
}

/// Values of 1 MiB each: the page comes back cut, far under 1 MiB.
#[tokio::test]
async fn large_values_come_back_cut() {
    let app = App::new();
    make_file(
        &app.site("blog").await,
        "CREATE TABLE big (id INTEGER PRIMARY KEY, v TEXT);\
         INSERT INTO big (id, v) WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 20) \
         SELECT i, hex(zeroblob(524288)) FROM n;",
    )
    .await;
    let body = json!({ "source": blog(), "table": { "name": "big" }, "offset": 0 });
    let response = app.router.send("POST", "/v1/db/rows", Some(body)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    assert!(bytes.len() < 64 * 1024, "{} bytes", bytes.len());
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["rows"][0][1]["bytes"], 1_048_576);
}

/// `POST /v1/db/cell` on the site `blog`'s `table`.
async fn cell(app: &App, table: &str, column: &str, key: Value) -> (StatusCode, Value) {
    let body =
        json!({ "source": blog(), "table": { "name": table }, "column": column, "key": key });
    app.router.answer("POST", "/v1/db/cell", Some(body)).await
}

#[tokio::test]
async fn a_whole_value_is_read_by_the_rows_key() {
    let app = App::new();
    make_file(
        &app.site("blog").await,
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, raw BLOB);\
         INSERT INTO docs VALUES (1, hex(zeroblob(50000)), x'0102'), (2, NULL, NULL);\
         INSERT INTO docs VALUES (3, hex(zeroblob(40000)), NULL);\
         CREATE TABLE loose (body TEXT); INSERT INTO loose VALUES ('x');",
    )
    .await;
    // 100,000 bytes: cut at 64 KiB, with the whole length.
    let (status, json) = cell(&app, "docs", "body", json!(["1"])).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["cell"]["bytes"], 100_000);
    assert_eq!(json["cell"]["text"].as_str().unwrap().len(), 65_536);
    // A BLOB is its size, a null a null, a number its text.
    let (_, json) = cell(&app, "docs", "raw", json!(["1"])).await;
    assert_eq!(json["cell"], json!({ "bytes": 2 }));
    let (_, json) = cell(&app, "docs", "body", json!(["2"])).await;
    assert_eq!(json["cell"], Value::Null);
    let (_, json) = cell(&app, "docs", "id", json!(["3"])).await;
    assert_eq!(json["cell"], "3");

    let refused = [
        (
            cell(&app, "docs", "body", json!(["9"])).await,
            404,
            "db_row_not_found",
        ),
        (
            cell(&app, "loose", "body", json!(["1"])).await,
            404,
            "db_row_not_found",
        ),
        (
            cell(&app, "docs", "nope", json!(["1"])).await,
            400,
            "db_source_invalid",
        ),
        (
            cell(&app, "docs", "body", json!(["1", "2"])).await,
            400,
            "db_source_invalid",
        ),
        (
            cell(&app, "nope", "body", json!(["1"])).await,
            404,
            "db_table_not_found",
        ),
    ];
    for ((status, json), want_status, code) in refused {
        assert_eq!(
            (status.as_u16(), json["error"]["code"].as_str()),
            (want_status, Some(code)),
            "{json}"
        );
    }
    // A value that is not a key is bound, never written into the SQL.
    let (status, _) = cell(&app, "docs", "body", json!(["1 OR 1=1"])).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[test]
fn a_server_connection_keeps_few_prepared_statements() {
    // The driver's options say their cache size only in their `Debug`.
    let kept = format!("statement_cache_capacity: {}", servers::STATEMENT_CACHE);
    assert!(format!("{:?}", servers::mysql_options(3306, Some("shop"))).contains(&kept));
    assert!(format!("{:?}", servers::pg_options(5432, None)).contains(&kept));
}
