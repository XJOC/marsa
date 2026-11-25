//! The user's statements as jobs, on a site's SQLite file.

use std::time::Duration;

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::tests::{App, blog, make_file};

const POSTS: &str = "CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT);\
     INSERT INTO posts (id, title) WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1500) \
     SELECT i, 'post ' || i FROM n;";

/// Starts `sql` on the site `blog`, and waits for the job's last event.
async fn query(app: &App, sql: &str) -> Value {
    let body = json!({ "source": blog(), "sql": sql });
    let (status, json) = app
        .router
        .answer("POST", "/v1/jobs/db/query", Some(body))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{json}");
    let id = json["id"].as_u64().unwrap();
    for _ in 0..200 {
        let (_, event) = app
            .router
            .answer("GET", &format!("/v1/jobs/{id}"), None)
            .await;
        if event["status"] != "running" {
            return event;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("job {id} did not end");
}

async fn results(app: &App, id: &Value) -> (StatusCode, Value) {
    let uri = format!("/v1/db/results/{id}");
    app.router.answer("GET", &uri, None).await
}

#[tokio::test]
async fn a_statement_runs_as_a_job_and_its_rows_are_read_once() {
    let app = App::new();
    make_file(&app.site("blog").await, POSTS).await;
    let event = query(
        &app,
        "SELECT id, title FROM posts WHERE id <= 3 ORDER BY id",
    )
    .await;
    assert_eq!(event["kind"], "db_query", "{event}");
    assert_eq!(event["status"], "done", "{event}");
    assert_eq!(event["result"], "db_query");
    // The event carries what the rows are, never the rows.
    assert_eq!(event["data"]["rows"], 3);
    assert_eq!(event["data"]["cut"], false);
    assert!(event["data"].get("columns").is_some());
    assert!(!event.to_string().contains("post 1"), "{event}");

    let (status, json) = results(&app, &event["id"]).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(
        json["rows"],
        json!([["1", "post 1"], ["2", "post 2"], ["3", "post 3"]])
    );
    assert_eq!(json["columns"][1]["name"], "title");
    let (status, json) = results(&app, &event["id"]).await;
    assert_eq!(
        (status, json["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("db_result_gone"))
    );
}

#[tokio::test]
async fn a_large_result_is_cut_at_1000_rows() {
    let app = App::new();
    make_file(&app.site("blog").await, POSTS).await;
    let event = query(&app, "SELECT * FROM posts").await;
    assert_eq!(
        (
            event["data"]["rows"].as_u64(),
            event["data"]["cut"].as_bool()
        ),
        (Some(1000), Some(true))
    );
    let (_, json) = results(&app, &event["id"]).await;
    assert_eq!(json["rows"].as_array().unwrap().len(), 1000);
    assert_eq!(json["cut"], true);
}

/// The engine refuses every write, and the job says so with its words.
#[tokio::test]
async fn a_statement_that_changes_data_is_refused_by_the_engine() {
    let app = App::new();
    let file = make_file(&app.site("blog").await, POSTS).await;
    let before = std::fs::read(&file).unwrap();
    for sql in [
        "INSERT INTO posts (title) VALUES ('x')",
        "UPDATE posts SET title = 'x'",
        "DELETE FROM posts",
        "DROP TABLE posts",
        "CREATE TABLE notes (body TEXT)",
    ] {
        let event = query(&app, sql).await;
        assert_eq!(event["status"], "failed", "{sql}: {event}");
        assert_eq!(event["code"], "db_read_only", "{sql}");
        assert!(
            event["detail"].as_str().is_some_and(|d| !d.is_empty()),
            "{event}"
        );
        assert_eq!(
            event["message"],
            "the statement changes data, and the browser is read-only"
        );
    }
    assert_eq!(std::fs::read(&file).unwrap(), before);
}

#[tokio::test]
async fn a_wrong_statement_fails_with_the_databases_words() {
    let app = App::new();
    make_file(&app.site("blog").await, POSTS).await;
    let event = query(&app, "SELECT * FORM posts").await;
    assert_eq!(
        (event["status"].as_str(), event["code"].as_str()),
        (Some("failed"), Some("db_query_failed"))
    );
    assert!(
        event["detail"].as_str().unwrap().contains("syntax"),
        "{event}"
    );
    // A statement with no rows ends with no columns.
    let event = query(&app, "PRAGMA foreign_keys").await;
    assert_eq!(event["status"], "done", "{event}");
}

#[tokio::test]
async fn only_one_statement_of_at_most_256_kib_starts() {
    let app = App::new();
    make_file(&app.site("blog").await, POSTS).await;
    let long = format!("SELECT '{}'", "x".repeat(256 * 1024));
    for (sql, code) in [
        ("SELECT 1; SELECT 2", "db_one_statement"),
        ("-- only a note", "db_one_statement"),
        (long.as_str(), "db_sql_too_long"),
    ] {
        let body = json!({ "source": blog(), "sql": sql });
        let (status, json) = app
            .router
            .answer("POST", "/v1/jobs/db/query", Some(body))
            .await;
        assert_eq!(
            (status, json["error"]["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some(code)),
            "{json}"
        );
    }
    // A missing site answers the request, before any job.
    let body = json!({ "source": { "kind": "site_file", "site": "shop" }, "sql": "SELECT 1" });
    let (status, _) = app
        .router
        .answer("POST", "/v1/jobs/db/query", Some(body))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[test]
fn one_statement_at_a_time_on_each_source() {
    use marsa_core::types::DbSource;

    let queries = super::query::Queries::default();
    let blog = DbSource::SiteFile {
        site: "blog".into(),
    };
    let shop = DbSource::SiteFile {
        site: "shop".into(),
    };
    queries.reserve(&blog).unwrap();
    queries.reserve(&shop).unwrap();
    assert_eq!(queries.reserve(&blog).unwrap_err().code, "db_busy");
    queries.release(&blog);
    queries.reserve(&blog).unwrap();
}

#[tokio::test]
async fn close_drops_the_rows_no_one_read() {
    let app = App::new();
    make_file(&app.site("blog").await, POSTS).await;
    let event = query(&app, "SELECT 1").await;
    app.router.send("POST", "/v1/db/close", None).await;
    let (status, _) = results(&app, &event["id"]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A statement that would run for hours in SQLite, reading the file all
/// along (so it holds the file's shared lock), in constant memory.
const LONG: &str = "SELECT count(*) FROM posts a, posts b, posts c, posts d";

async fn start(app: &App, sql: &str) -> u64 {
    let body = json!({ "source": blog(), "sql": sql });
    let (status, json) = app
        .router
        .answer("POST", "/v1/jobs/db/query", Some(body))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{json}");
    json["id"].as_u64().unwrap()
}

async fn ended(app: &App, id: u64) -> Value {
    for _ in 0..300 {
        let (_, event) = app
            .router
            .answer("GET", &format!("/v1/jobs/{id}"), None)
            .await;
        if event["status"] != "running" {
            return event;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("job {id} did not end");
}

#[tokio::test]
async fn a_running_statement_is_cancelled() {
    let app = App::new();
    let file = make_file(&app.site("blog").await, POSTS).await;
    let id = start(&app, LONG).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let response = app
        .router
        .send("DELETE", &format!("/v1/jobs/{id}"), None)
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let event = ended(&app, id).await;
    assert_eq!(
        (event["status"].as_str(), event["code"].as_str()),
        (Some("failed"), Some("db_query_cancelled"))
    );
    // Stopped inside SQLite, not only no longer awaited: while the read ran,
    // it held the file's shared lock, and this write would wait and fail.
    let writer = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&file)
        .busy_timeout(Duration::from_secs(1));
    let writer = sqlx::ConnectOptions::disable_statement_logging(writer);
    let mut writer = sqlx::ConnectOptions::connect(&writer).await.unwrap();
    let mut wrote = false;
    for _ in 0..20 {
        let insert = sqlx::query("INSERT INTO posts (title) VALUES ('after')");
        if insert.execute(&mut writer).await.is_ok() {
            wrote = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(wrote, "the cancelled statement still holds the file");
    // It ran no longer: a new statement on the same file starts at once.
    let again = query(&app, "SELECT 1").await;
    assert_eq!(again["status"], "done", "{again}");
    // Over: no job to cancel.
    let (status, json) = app
        .router
        .answer("DELETE", &format!("/v1/jobs/{id}"), None)
        .await;
    assert_eq!(
        (status, json["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("not_found"))
    );
    for uri in ["/v1/jobs/999", "/v1/jobs/x"] {
        let (status, _) = app.router.answer("DELETE", uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
}

/// Every other kind of job keeps today's behavior: it cannot be cancelled.
#[tokio::test]
async fn another_kind_of_job_is_not_cancellable() {
    use marsa_core::types::{JobEvent, JobKind, JobStep, JobUpdate};

    let app = App::new();
    let running = JobEvent {
        id: 41,
        kind: JobKind::Setup,
        update: JobUpdate::Running {
            step: JobStep::Started,
        },
    };
    app.daemon.jobs().record(&running);
    let (status, json) = app.router.answer("DELETE", "/v1/jobs/41", None).await;
    assert_eq!(
        (status, json["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("job_not_cancellable"))
    );
    let (_, event) = app.router.answer("GET", "/v1/jobs/41", None).await;
    assert_eq!(event["status"], "running");
}

#[tokio::test]
async fn close_cancels_a_running_statement() {
    let app = App::new();
    make_file(&app.site("blog").await, POSTS).await;
    let id = start(&app, LONG).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    app.router.send("POST", "/v1/db/close", None).await;
    let event = ended(&app, id).await;
    assert_eq!(event["code"], "db_query_cancelled", "{event}");
}
