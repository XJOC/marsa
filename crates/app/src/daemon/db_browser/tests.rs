use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection};
use tempfile::TempDir;

use super::sqlite;
use crate::daemon::http::test_router::TestRouter;
use crate::daemon::state::Daemon;

const ROUTES: [(&str, &str); 3] = [
    ("GET", "/v1/db/sources"),
    ("POST", "/v1/db/tables"),
    ("POST", "/v1/db/close"),
];

struct App {
    dir: TempDir,
    router: TestRouter,
}

impl App {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let data = dir.path().join("data");
        fs::create_dir_all(&data).unwrap();
        let router = TestRouter::new(Arc::new(Daemon::new(data)));
        Self { dir, router }
    }

    /// A linked site `name` with an empty project folder.
    async fn site(&self, name: &str) -> PathBuf {
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

fn blog() -> Value {
    json!({ "kind": "site_file", "site": "blog" })
}

/// Makes the site's SQLite file with `sql`, through a connection that may
/// write, as the site's own app would.
async fn make_file(root: &Path, sql: &'static str) -> PathBuf {
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
