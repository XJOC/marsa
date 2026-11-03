//! The daemon client against a fake daemon: an axum server on `127.0.0.1:0`
//! with a `run/daemon.json` in a temp folder (task M3-08). No network beyond
//! loopback. Starting the real `marsad` is tested in M3-09.

#![cfg(feature = "full")]

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{RawQuery, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use marsa_core::client::{
    ClientError, DaemonClient, EventStream, ensure_running, ensure_running_with,
};
use marsa_core::platform::daemon_file::{
    DaemonFile, Token, daemon_file_path, remove_daemon_file, write_daemon_file,
};
use marsa_core::types::{
    API_VERSION, ErrorBody, Event, Health, LogLine, LogStream, ServiceAction, ServiceState,
    ServiceStatus,
};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What the fake daemon saw of one request.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    target: String,
    authorization: Option<String>,
    host: Option<String>,
    origin: bool,
}

type Log = Arc<Mutex<Vec<Seen>>>;

struct Fake {
    dir: TempDir,
    port: u16,
    /// The token in hex, to look for it where it must not be.
    token: String,
    seen: Log,
}

impl Fake {
    /// Serves `router` on a free loopback port and writes the daemon file.
    async fn start(router: Router) -> Self {
        let seen = Log::default();
        let router = router.layer(middleware::from_fn_with_state(seen.clone(), record));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback port");
        let port = listener.local_addr().expect("a bound address").port();
        tokio::spawn(async move { axum::serve(listener, router).await.expect("serve") });
        let dir = TempDir::new().expect("a temp folder");
        let token = write_file(dir.path(), port);
        Fake {
            dir,
            port,
            token,
            seen,
        }
    }

    fn data_dir(&self) -> &Path {
        self.dir.path()
    }

    fn client(&self) -> DaemonClient {
        DaemonClient::connect(self.data_dir()).expect("connect")
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("lock").clone()
    }

    /// Neither the message, its sources, nor `Debug` holds the token.
    fn assert_no_token(&self, err: &ClientError) {
        let mut shown = format!("{err} {err:?} {err:#?}");
        let mut source = std::error::Error::source(err);
        while let Some(cause) = source {
            shown.push_str(&format!(" {cause} {cause:?}"));
            source = cause.source();
        }
        assert!(!shown.contains(&self.token), "{shown}");
    }
}

/// Writes `<data_dir>/run/daemon.json` for `port` and returns the token in hex.
fn write_file(data_dir: &Path, port: u16) -> String {
    let token = Token::generate().expect("a token");
    let hex = token
        .bearer_header()
        .strip_prefix("Bearer ")
        .expect("a bearer header")
        .to_owned();
    let file = DaemonFile {
        pid: 1,
        port,
        token,
        app_version: VERSION.to_owned(),
    };
    write_daemon_file(&data_dir.join("run"), &file).expect("write the daemon file");
    hex
}

async fn record(State(seen): State<Log>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let text = |name| {
        headers
            .get(name)
            .map(|value: &header::HeaderValue| value.to_str().expect("ASCII header").to_owned())
    };
    let entry = Seen {
        method: request.method().to_string(),
        target: request.uri().to_string(),
        authorization: text(header::AUTHORIZATION),
        host: text(header::HOST),
        origin: headers.contains_key(header::ORIGIN),
    };
    seen.lock().expect("lock").push(entry);
    next.run(request).await
}

fn health(app_version: &str, api_version: u32) -> Health {
    Health {
        app_version: app_version.to_owned(),
        api_version,
        pid: 42,
    }
}

fn serve_health(answer: Health) -> Router {
    Router::new().route(
        "/v1/health",
        get(move || {
            let answer = answer.clone();
            async move { Json(answer) }
        }),
    )
}

/// `POST /v1/shutdown` that counts calls and, like the real daemon, deletes
/// the daemon file.
fn serve_shutdown(
    router: Router,
    run_dir: Arc<Mutex<Option<std::path::PathBuf>>>,
) -> (Router, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let router = router.route(
        "/v1/shutdown",
        post(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            if let Some(run) = run_dir.lock().expect("lock").as_deref() {
                remove_daemon_file(run).expect("delete the daemon file");
            }
            async { StatusCode::ACCEPTED }
        }),
    );
    (router, calls)
}

fn caddy(state: ServiceState) -> ServiceStatus {
    ServiceStatus {
        id: "caddy".into(),
        state,
        pid: None,
        restarts: 0,
        since_ms: Some(1),
    }
}

fn log_line(seq: u64, text: &str) -> LogLine {
    LogLine {
        service: "caddy".into(),
        seq,
        at_ms: 5,
        stream: LogStream::Stderr,
        text: text.into(),
    }
}

/// An SSE body the way axum's `Sse` writes it, with keep-alive comments.
fn sse_body<T: serde::Serialize>(events: &[T]) -> String {
    let mut body = String::from(":\n\n");
    for event in events {
        body.push_str(&format!(
            "data: {}\n\n: keep-alive\n\n",
            serde_json::to_string(event).expect("JSON")
        ));
    }
    body
}

fn sse_response(body: String) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/event-stream")], body)
}

async fn collect<T: serde::de::DeserializeOwned>(mut stream: EventStream<T>) -> Vec<T> {
    let mut out = Vec::new();
    while let Some(event) = stream.next().await {
        out.push(event.expect("an event"));
    }
    out
}

#[tokio::test]
async fn requests_carry_the_token_and_no_origin() {
    let fake = Fake::start(serve_health(health(VERSION, API_VERSION))).await;
    let answer = fake.client().health().await.unwrap();
    assert_eq!(answer, health(VERSION, API_VERSION));

    let seen = fake.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    let request = &seen[0];
    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("GET", "/v1/health")
    );
    let bearer = format!("Bearer {}", fake.token);
    assert_eq!(request.authorization.as_deref(), Some(bearer.as_str()));
    assert_eq!(request.host, Some(format!("127.0.0.1:{}", fake.port)));
    assert!(!request.origin);
}

#[test]
fn debug_shows_the_port_and_never_the_token() {
    let dir = TempDir::new().unwrap();
    let token = write_file(dir.path(), 51234);
    let client = DaemonClient::connect(dir.path()).unwrap();
    assert_eq!(client.port(), 51234);
    for shown in [format!("{client:?}"), format!("{client:#?}")] {
        assert!(!shown.contains(&token), "{shown}");
        assert!(shown.contains("51234"), "{shown}");
    }
}

#[tokio::test]
async fn an_error_body_becomes_an_api_error_with_the_daemons_code() {
    let router = Router::new().route(
        "/v1/services/{id}/{action}",
        post(|| async {
            let body = ErrorBody::new("service_not_found", "no service named php");
            (StatusCode::NOT_FOUND, Json(body))
        }),
    );
    let fake = Fake::start(router).await;
    let err = fake
        .client()
        .service_action("php", ServiceAction::Start)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "service_not_found");
    let ClientError::Api {
        status,
        code,
        message,
    } = &err
    else {
        panic!("{err:?}");
    };
    assert_eq!(
        (*status, code.as_str(), message.as_str()),
        (404, "service_not_found", "no service named php")
    );
    fake.assert_no_token(&err);
    assert_eq!(fake.seen()[0].target, "/v1/services/php/start");
}

#[tokio::test]
async fn an_error_without_an_error_body_is_a_bad_response() {
    // axum's own 404 has an empty body.
    let fake = Fake::start(Router::new()).await;
    let err = fake.client().logs("caddy").await.unwrap_err();
    assert_eq!(err.code(), "daemon_bad_response", "{err:?}");
    fake.assert_no_token(&err);
}

#[tokio::test]
async fn a_body_that_is_not_the_expected_json_is_a_bad_response() {
    let router = Router::new().route(
        "/v1/services",
        get(|| async { Json(serde_json::json!({ "not": "a list" })) }),
    );
    let fake = Fake::start(router).await;
    let err = fake.client().services().await.unwrap_err();
    assert_eq!(err.code(), "daemon_bad_response", "{err:?}");
    fake.assert_no_token(&err);
}

#[tokio::test]
async fn services_and_logs_are_read() {
    let router = Router::new()
        .route(
            "/v1/services",
            get(|| async { Json(vec![caddy(ServiceState::Stopped)]) }),
        )
        .route(
            "/v1/services/{id}/{action}",
            post(|| async { Json(caddy(ServiceState::Running)) }),
        )
        .route(
            "/v1/logs/{id}",
            get(|RawQuery(query): RawQuery| async move {
                let lines = vec![log_line(0, "serving"), log_line(1, "مرسى")];
                match query.as_deref() {
                    Some("follow=true") => sse_response(sse_body(&lines)).into_response(),
                    _ => Json(lines).into_response(),
                }
            }),
        );
    let fake = Fake::start(router).await;
    let client = fake.client();
    assert_eq!(
        client.services().await.unwrap(),
        vec![caddy(ServiceState::Stopped)]
    );
    let started = client
        .service_action("caddy", ServiceAction::Restart)
        .await
        .unwrap();
    assert_eq!(started, caddy(ServiceState::Running));
    let lines = vec![log_line(0, "serving"), log_line(1, "مرسى")];
    assert_eq!(client.logs("caddy").await.unwrap(), lines);
    assert_eq!(
        collect(client.follow_logs("caddy").await.unwrap()).await,
        lines
    );

    let targets: Vec<String> = fake
        .seen()
        .into_iter()
        .map(|seen| format!("{} {}", seen.method, seen.target))
        .collect();
    assert_eq!(
        targets,
        [
            "GET /v1/services",
            "POST /v1/services/caddy/restart",
            "GET /v1/logs/caddy",
            "GET /v1/logs/caddy?follow=true",
        ]
    );
}

#[tokio::test]
async fn events_reads_three_events() {
    let events = vec![
        Event::ServiceChanged(caddy(ServiceState::Running)),
        Event::Log(log_line(7, "مرسى")),
        Event::ShuttingDown,
    ];
    let body = sse_body(&events);
    let router = Router::new().route("/v1/events", get(move || async move { sse_response(body) }));
    let fake = Fake::start(router).await;
    assert_eq!(collect(fake.client().events().await.unwrap()).await, events);
}

#[tokio::test]
async fn a_stream_answered_with_json_is_a_bad_response() {
    let router = Router::new().route("/v1/events", get(|| async { Json(Vec::<Event>::new()) }));
    let fake = Fake::start(router).await;
    let err = fake.client().events().await.unwrap_err();
    assert_eq!(err.code(), "daemon_bad_response", "{err:?}");
}

/// A hand-written HTTP server that sends the body one byte per write, so the
/// client's reads split lines, `\r\n` pairs, and Arabic letters.
#[tokio::test]
async fn events_survive_a_body_sent_one_byte_at_a_time() {
    let events = vec![Event::Log(log_line(1, "مرسى")), Event::ShuttingDown];
    let body = format!(
        "data: {}\r\n\r\n: keep-alive\r\n\r\ndata: {{\"type\":\r\ndata: \"shutting_down\"}}\r\n\r\n",
        serde_json::to_string(&events[0]).unwrap()
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.set_nodelay(true).unwrap();
        let mut head = Vec::new();
        let mut buf = [0_u8; 1024];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            let read = socket.read(&mut buf).await.unwrap();
            assert!(read > 0, "the request head ended early");
            head.extend_from_slice(&buf[..read]);
        }
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        for byte in body.as_bytes() {
            socket.write_all(&[*byte]).await.unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    let dir = TempDir::new().unwrap();
    write_file(dir.path(), port);
    let client = DaemonClient::connect(dir.path()).unwrap();
    assert_eq!(collect(client.events().await.unwrap()).await, events);
}

#[tokio::test]
async fn an_id_cannot_reach_another_route() {
    let (router, shutdowns) = serve_shutdown(Router::new(), Arc::default());
    let router = router.route(
        "/v1/shutdown/{action}",
        post(|| async { StatusCode::ACCEPTED }),
    );
    let fake = Fake::start(router).await;
    let client = fake.client();
    for id in [
        "../../shutdown",
        "x/../../shutdown#",
        "%2e%2e/%2e%2e/shutdown",
        "../../shutdown?a=b",
    ] {
        let _ = client.service_action(id, ServiceAction::Start).await;
        let _ = client.logs(id).await;
    }
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
    let seen = fake.seen();
    assert_eq!(seen.len(), 8, "{seen:?}");
    for request in &seen {
        assert!(!request.target.starts_with("/v1/shutdown"), "{request:?}");
    }
    assert_eq!(
        seen[2].target,
        "/v1/services/x%2F..%2F..%2Fshutdown%23/start"
    );

    // The URL rules would drop these segments and shorten the path, so they
    // are refused before any request.
    for id in ["", ".", ".."] {
        let err = client
            .service_action(id, ServiceAction::Start)
            .await
            .unwrap_err();
        assert_eq!(err.code(), "invalid_id", "{err:?}");
        let err = client.follow_logs(id).await.unwrap_err();
        assert_eq!(err.code(), "invalid_id", "{err:?}");
    }
    assert_eq!(fake.seen().len(), 8);
}

#[tokio::test]
async fn no_file_or_a_dead_port_means_not_running() {
    let dir = TempDir::new().unwrap();
    let err = DaemonClient::connect(dir.path()).unwrap_err();
    assert_eq!(err.code(), "daemon_not_running", "{err:?}");

    // A file left by a crash: nothing listens on port 1.
    let token = write_file(dir.path(), 1);
    let err = DaemonClient::connect(dir.path())
        .unwrap()
        .health()
        .await
        .unwrap_err();
    assert_eq!(err.code(), "daemon_not_running", "{err:?}");
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains(&token), "{shown}");
}

#[test]
fn a_broken_file_keeps_its_code() {
    let dir = TempDir::new().unwrap();
    write_file(dir.path(), 1);
    let path = daemon_file_path(&dir.path().join("run"));
    std::fs::write(&path, "not json").unwrap();
    let err = DaemonClient::connect(dir.path()).unwrap_err();
    assert_eq!(err.code(), "daemon_file_invalid", "{err:?}");
}

#[tokio::test]
async fn ensure_running_returns_a_daemon_of_this_version() {
    let (router, shutdowns) =
        serve_shutdown(serve_health(health(VERSION, API_VERSION)), Arc::default());
    let fake = Fake::start(router).await;
    let missing = fake.data_dir().join("no-marsad");
    let client = ensure_running_with(fake.data_dir(), &missing)
        .await
        .unwrap();
    assert_eq!(client.port(), fake.port);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_daemon_with_a_newer_api_is_refused_and_left_running() {
    // Even with another app version: the API check comes first.
    for app_version in [VERSION, "99.0.0"] {
        let answer = health(app_version, API_VERSION + 1);
        let (router, shutdowns) = serve_shutdown(serve_health(answer), Arc::default());
        let fake = Fake::start(router).await;
        let missing = fake.data_dir().join("no-marsad");
        let err = ensure_running_with(fake.data_dir(), &missing)
            .await
            .unwrap_err();
        assert_eq!(err.code(), "daemon_api_too_new", "{err:?}");
        assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
        assert!(daemon_file_path(&fake.data_dir().join("run")).exists());
        fake.assert_no_token(&err);
    }
}

#[tokio::test]
async fn a_daemon_of_another_version_is_stopped_then_replaced() {
    let run_dir = Arc::new(Mutex::new(None));
    let (router, shutdowns) = serve_shutdown(
        serve_health(health("0.0.0-old", API_VERSION)),
        run_dir.clone(),
    );
    let fake = Fake::start(router).await;
    *run_dir.lock().unwrap() = Some(fake.data_dir().join("run"));

    // No `marsad` here, so the start after the stop fails, which shows it was tried.
    let missing = fake.data_dir().join("no-marsad");
    let err = ensure_running_with(fake.data_dir(), &missing)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "daemon_start_failed", "{err:?}");
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    assert!(!daemon_file_path(&fake.data_dir().join("run")).exists());
    fake.assert_no_token(&err);
}

#[tokio::test]
async fn no_daemon_and_no_marsad_is_a_start_failure() {
    let dir = TempDir::new().unwrap();
    let err = ensure_running_with(dir.path(), &dir.path().join("no-marsad"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "daemon_start_failed", "{err:?}");
}

/// The window runs these on a tokio runtime, which needs `Send`.
#[test]
fn futures_and_streams_are_send() {
    fn is_send<T: Send>(_: &T) {}
    fn is_send_type<T: Send + Sync>() {}
    let dir = Path::new("unused");
    is_send(&ensure_running(dir));
    is_send(&ensure_running_with(dir, dir));
    is_send_type::<DaemonClient>();
    is_send_type::<EventStream<Event>>();
}
