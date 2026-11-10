//! The daemon client against a fake daemon: an axum server on `127.0.0.1:0`
//! that holds `run/daemon.lock` and has a `run/daemon.json` in a temp folder,
//! like `marsad` (task M3-08). No network beyond loopback. Starting the real
//! `marsad` is tested in M3-09.

#![cfg(feature = "full")]

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{RawQuery, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use marsa_core::client::{
    ClientError, DaemonClient, EventStream, StreamItem, ensure_running, ensure_running_with,
    stop_daemon,
};
use marsa_core::platform::daemon_file::{
    DaemonFile, Token, daemon_file_path, daemon_lock_path, remove_daemon_file, write_daemon_file,
};
use marsa_core::runtimes::Runtime;
use marsa_core::types::{
    API_VERSION, DefaultVersion, ErrorBody, Event, Health, InstallAccepted, InstallRequest,
    LinkRequest, LogLine, LogStream, OpResult, OpStatus, ParkRequest, RuntimeList, RuntimeVersions,
    ServiceAction, ServiceState, ServiceStatus, SetupReport, SetupSkip, Site, SiteKind, SiteList,
    SitePatch, SkippedFolder,
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

/// `run/daemon.lock`, held the way `marsad` holds it while it runs.
#[derive(Clone)]
struct DaemonLock(Arc<Mutex<Option<File>>>);

impl DaemonLock {
    fn take(data_dir: &Path) -> Self {
        let run = data_dir.join("run");
        fs::create_dir_all(&run).expect("the run folder");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(daemon_lock_path(&run))
            .expect("the lock file");
        file.try_lock().expect("the daemon lock");
        DaemonLock(Arc::new(Mutex::new(Some(file))))
    }

    /// Lets go, as `marsad` does when it exits.
    fn release(&self) {
        if let Some(file) = self.0.lock().expect("lock").take() {
            file.unlock().expect("unlock");
        }
    }
}

struct Fake {
    _temp: Option<TempDir>,
    data_dir: PathBuf,
    port: u16,
    /// The token in hex, to look for it where it must not be.
    token: String,
    seen: Log,
    lock: DaemonLock,
}

impl Fake {
    /// A fake daemon in a new temp folder.
    async fn start(router: Router) -> Self {
        let temp = TempDir::new().expect("a temp folder");
        let lock = DaemonLock::take(temp.path());
        let mut fake = Fake::serve(temp.path(), lock, router).await;
        fake._temp = Some(temp);
        fake
    }

    /// Serves `router` on a free loopback port for `data_dir`, holding
    /// `lock`, then writes the daemon file: the order `marsad` uses.
    async fn serve(data_dir: &Path, lock: DaemonLock, router: Router) -> Self {
        let seen = Log::default();
        let router = router.layer(middleware::from_fn_with_state(seen.clone(), record));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback port");
        let port = listener.local_addr().expect("a bound address").port();
        tokio::spawn(async move { axum::serve(listener, router).await.expect("serve") });
        let token = write_file(data_dir, port);
        Fake {
            _temp: None,
            data_dir: data_dir.to_path_buf(),
            port,
            token,
            seen,
            lock,
        }
    }

    fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    fn client(&self) -> DaemonClient {
        DaemonClient::connect(self.data_dir()).expect("connect")
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("lock").clone()
    }

    /// Neither the message, its sources, nor `Debug` holds the token.
    fn assert_no_token(&self, err: &ClientError) {
        assert_no_token(&self.token, err);
    }
}

fn assert_no_token(token: &str, err: &ClientError) {
    let mut shown = format!("{err} {err:?} {err:#?}");
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        shown.push_str(&format!(" {cause} {cause:?}"));
        source = cause.source();
    }
    assert!(!shown.contains(token), "{shown}");
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

/// `POST /v1/shutdown` that counts calls. With `stop`, it also does what
/// `marsad` does: deletes the daemon file and lets go of the lock.
fn serve_shutdown(
    router: Router,
    stop: Option<(&Path, &DaemonLock)>,
) -> (Router, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let stop = stop.map(|(data_dir, lock)| (data_dir.join("run"), lock.clone()));
    let router = router.route(
        "/v1/shutdown",
        post(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            if let Some((run, lock)) = &stop {
                remove_daemon_file(run).expect("delete the daemon file");
                lock.release();
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

fn sse_data<T: serde::Serialize>(event: &T) -> String {
    format!("data: {}\n\n", serde_json::to_string(event).expect("JSON"))
}

/// An SSE body the way axum's `Sse` writes it, with keep-alive comments.
fn sse_body<T: serde::Serialize>(events: &[T]) -> String {
    let mut body = String::from(":\n\n");
    for event in events {
        body.push_str(&sse_data(event));
        body.push_str(": keep-alive\n\n");
    }
    body
}

fn sse_response(body: String) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/event-stream")], body)
}

async fn collect<T: serde::de::DeserializeOwned>(mut stream: EventStream<T>) -> Vec<StreamItem<T>> {
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item.expect("an item"));
    }
    out
}

fn items<T: Clone>(events: &[T]) -> Vec<StreamItem<T>> {
    events.iter().cloned().map(StreamItem::Item).collect()
}

/// An executable shell script standing in for `marsad`. It gets
/// `--data-dir <path>` as `$1 $2`.
#[cfg(unix)]
fn fake_marsad(dir: &Path, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("marsad");
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write the script");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// After `delay`, a fake daemon of this version takes the lock in
/// `data_dir`, and after `delay` again it writes its file and answers.
// Only the start-up tests use it, and they need a shell-script `marsad`.
#[cfg(unix)]
fn daemon_later(data_dir: &Path, delay: Duration) -> tokio::task::JoinHandle<Fake> {
    let data_dir = data_dir.to_path_buf();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let lock = DaemonLock::take(&data_dir);
        tokio::time::sleep(delay).await;
        Fake::serve(&data_dir, lock, serve_health(health(VERSION, API_VERSION))).await
    })
}

// ---------------------------------------------------------------------------
// Calls

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
    let _lock = DaemonLock::take(dir.path());
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
async fn daemon_text_in_an_error_is_cleaned_and_short() {
    let router = Router::new().route(
        "/v1/services",
        get(|| async {
            let message = format!("line\r\nforged log line\u{1b}[2J{}", "x".repeat(500));
            let body = ErrorBody::new("caddy_not\ninstalled", message);
            (StatusCode::CONFLICT, Json(body))
        }),
    );
    let fake = Fake::start(router).await;
    let err = fake.client().services().await.unwrap_err();
    assert_eq!(err.code(), "caddy_notinstalled");
    let ClientError::Api { message, .. } = &err else {
        panic!("{err:?}");
    };
    assert!(
        message.starts_with("lineforged log line[2Jxxx"),
        "{message}"
    );
    assert_eq!(message.chars().count(), 200);
    assert!(!err.to_string().chars().any(char::is_control), "{err}");
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
async fn a_body_over_16_mib_is_refused() {
    let router = Router::new().route(
        "/v1/services",
        get(|| async { "x".repeat(16 * 1024 * 1024 + 1) }),
    );
    let fake = Fake::start(router).await;
    let err = fake.client().services().await.unwrap_err();
    assert_eq!(err.code(), "daemon_bad_response", "{err:?}");
    assert!(err.to_string().contains("16 MiB"), "{err}");
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let other = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = format!(
        "http://127.0.0.1:{}/v1/health",
        other.local_addr().unwrap().port()
    );
    let router = Router::new().route(
        "/v1/health",
        get(move || {
            let target = target.clone();
            async move { (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, target)]) }
        }),
    );
    let fake = Fake::start(router).await;
    let err = fake.client().health().await.unwrap_err();
    assert_eq!(err.code(), "daemon_bad_response", "{err:?}");
    let accepted = tokio::time::timeout(Duration::from_millis(200), other.accept()).await;
    assert!(accepted.is_err(), "the redirect was followed");
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
    let followed = collect(client.follow_logs("caddy").await.unwrap()).await;
    assert_eq!(followed, items(&lines));

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
async fn runtimes_and_install_are_called() {
    let list = RuntimeList {
        runtimes: vec![RuntimeVersions {
            runtime: Runtime::Php,
            installed: vec!["8.4.23".into()],
        }],
    };
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let router = Router::new()
        .route(
            "/v1/runtimes",
            get({
                let list = list.clone();
                move || async move { Json(list) }
            }),
        )
        .route(
            "/v1/runtimes/{tool}/install",
            post({
                let bodies = bodies.clone();
                move |Json(body): Json<InstallRequest>| async move {
                    let mut bodies = bodies.lock().expect("lock");
                    bodies.push(body);
                    if bodies.len() == 1 {
                        (StatusCode::ACCEPTED, Json(InstallAccepted { id: 4 })).into_response()
                    } else {
                        let busy = ErrorBody::new("install_busy", "another install is running");
                        (StatusCode::CONFLICT, Json(busy)).into_response()
                    }
                }
            }),
        );
    let fake = Fake::start(router).await;
    let client = fake.client();
    assert_eq!(client.runtimes().await.unwrap(), list);
    let accepted = client.install(Runtime::Php, Some("8.4"), false).await;
    assert_eq!(accepted.unwrap(), InstallAccepted { id: 4 });
    let err = client
        .install(Runtime::Composer, None, true)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "install_busy");
    assert!(
        matches!(err, ClientError::Api { status: 409, .. }),
        "{err:?}"
    );

    assert_eq!(
        *bodies.lock().unwrap(),
        [
            InstallRequest {
                version: Some("8.4".into()),
                force: false,
            },
            InstallRequest {
                version: None,
                force: true,
            },
        ]
    );
    let targets: Vec<String> = fake
        .seen()
        .into_iter()
        .map(|seen| format!("{} {}", seen.method, seen.target))
        .collect();
    assert_eq!(
        targets,
        [
            "GET /v1/runtimes",
            "POST /v1/runtimes/php/install",
            "POST /v1/runtimes/composer/install",
        ]
    );
}

#[tokio::test]
async fn set_default_puts_the_version_and_keeps_the_daemons_code() {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let answer = DefaultVersion {
        runtime: Runtime::Php,
        default: "8.3".into(),
        version: "8.3.12".into(),
    };
    let router = Router::new()
        .route(
            "/v1/runtimes/php/default",
            put({
                let bodies = bodies.clone();
                let answer = answer.clone();
                move |Json(body): Json<serde_json::Value>| async move {
                    bodies.lock().expect("lock").push(body);
                    Json(answer)
                }
            }),
        )
        .route(
            "/v1/runtimes/composer/default",
            put(|| async {
                let body = ErrorBody::new("no_default_version", "composer has no default version");
                (StatusCode::BAD_REQUEST, Json(body))
            }),
        );
    let fake = Fake::start(router).await;
    let client = fake.client();
    assert_eq!(
        client.set_default(Runtime::Php, "8.3").await.unwrap(),
        answer
    );
    let err = client
        .set_default(Runtime::Composer, "2")
        .await
        .unwrap_err();
    assert_eq!(err.code(), "no_default_version");
    assert!(
        matches!(err, ClientError::Api { status: 400, .. }),
        "{err:?}"
    );
    fake.assert_no_token(&err);

    assert_eq!(
        *bodies.lock().unwrap(),
        [serde_json::json!({ "version": "8.3" })]
    );
    let targets: Vec<String> = fake
        .seen()
        .into_iter()
        .map(|seen| format!("{} {}", seen.method, seen.target))
        .collect();
    assert_eq!(
        targets,
        [
            "PUT /v1/runtimes/php/default",
            "PUT /v1/runtimes/composer/default",
        ]
    );
}

#[tokio::test]
async fn an_id_cannot_reach_another_route() {
    let (router, shutdowns) = serve_shutdown(Router::new(), None);
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

// ---------------------------------------------------------------------------
// Sites (task M4-10c)

/// A list with one site, so each answer can be told apart by `parked`.
fn site_list(parked: &str) -> SiteList {
    // Absolute on every system; nothing is created there.
    let code = std::env::temp_dir().join("code");
    let root = code.join("shop");
    SiteList {
        parked: vec![PathBuf::from(parked)],
        sites: vec![Site {
            name: "shop".into(),
            docroot: root.join("public"),
            root,
            kind: SiteKind::Laravel,
            php: Some(semver::Version::new(8, 4, 23)),
            secure: false,
        }],
        skipped: vec![SkippedFolder {
            path: code.join("متجر"),
            code: "site_name_invalid".into(),
        }],
    }
}

/// `GET`, `POST park`, `POST link`, and `PATCH {name}`, each answering its own
/// list. The bodies the daemon got, as JSON.
fn serve_sites(bodies: &Arc<Mutex<Vec<serde_json::Value>>>) -> Router {
    let keep = |bodies: &Arc<Mutex<Vec<serde_json::Value>>>, answer: SiteList| {
        let bodies = bodies.clone();
        move |Json(body): Json<serde_json::Value>| async move {
            bodies.lock().expect("lock").push(body);
            Json(answer)
        }
    };
    Router::new()
        .route("/v1/sites", get(|| async { Json(site_list("get")) }))
        .route(
            "/v1/sites/park",
            post(keep(bodies, site_list("park"))).patch(keep(bodies, site_list("patch park"))),
        )
        .route("/v1/sites/link", post(keep(bodies, site_list("link"))))
        .route("/v1/sites/{name}", patch(keep(bodies, site_list("patch"))))
}

#[tokio::test]
async fn sites_park_link_and_update_site_are_called() {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let fake = Fake::start(serve_sites(&bodies)).await;
    let client = fake.client();
    let folder = fake.data_dir().join("code");
    let api = fake.data_dir().join("work").join("api");

    assert_eq!(client.sites().await.unwrap(), site_list("get"));
    assert_eq!(client.park(&folder).await.unwrap(), site_list("park"));
    assert_eq!(client.link("api", &api).await.unwrap(), site_list("link"));
    let secure = SitePatch {
        secure: Some(true),
        ..SitePatch::default()
    };
    assert_eq!(
        client.update_site("shop", &secure).await.unwrap(),
        site_list("patch")
    );
    let clear = SitePatch {
        php: Some(None),
        secure: Some(false),
        proxy: Some(None),
    };
    // `park` is a valid site name: its fixed path takes `PATCH` too.
    assert_eq!(
        client.update_site("park", &clear).await.unwrap(),
        site_list("patch park")
    );

    let want = [
        serde_json::to_value(ParkRequest {
            path: folder.clone(),
        })
        .unwrap(),
        serde_json::to_value(LinkRequest {
            name: "api".into(),
            path: api.clone(),
        })
        .unwrap(),
        serde_json::json!({ "secure": true }),
        serde_json::json!({ "php": null, "secure": false, "proxy": null }),
    ];
    assert_eq!(*bodies.lock().unwrap(), want);
    let targets: Vec<String> = fake
        .seen()
        .into_iter()
        .map(|seen| format!("{} {}", seen.method, seen.target))
        .collect();
    assert_eq!(
        targets,
        [
            "GET /v1/sites",
            "POST /v1/sites/park",
            "POST /v1/sites/link",
            "PATCH /v1/sites/shop",
            "PATCH /v1/sites/park",
        ]
    );
    let bearer = format!("Bearer {}", fake.token);
    for seen in fake.seen() {
        assert_eq!(seen.authorization.as_deref(), Some(bearer.as_str()));
        assert!(!seen.origin);
    }
}

#[tokio::test]
async fn a_site_error_keeps_the_daemons_code() {
    let router = Router::new()
        .route(
            "/v1/sites/{name}",
            patch(|| async {
                let body = ErrorBody::new("site_not_found", "no site named \"nope\"");
                (StatusCode::NOT_FOUND, Json(body))
            }),
        )
        .route(
            "/v1/sites/park",
            post(|| async {
                let body = ErrorBody::new("site_not_a_folder", "/x is not a folder");
                (StatusCode::BAD_REQUEST, Json(body))
            }),
        );
    let fake = Fake::start(router).await;
    let client = fake.client();
    let err = client
        .update_site("nope", &SitePatch::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "site_not_found");
    assert!(
        matches!(err, ClientError::Api { status: 404, .. }),
        "{err:?}"
    );
    fake.assert_no_token(&err);
    let err = client.park(&fake.data_dir().join("x")).await.unwrap_err();
    assert_eq!(err.code(), "site_not_a_folder");
    fake.assert_no_token(&err);
}

#[tokio::test]
async fn a_site_name_cannot_reach_another_route() {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let fake = Fake::start(serve_sites(&bodies)).await;
    let client = fake.client();
    let patch = SitePatch::default();
    client.update_site("../link", &patch).await.unwrap();
    assert_eq!(fake.seen()[0].target, "/v1/sites/..%2Flink");
    for name in ["", ".", ".."] {
        let err = client.update_site(name, &patch).await.unwrap_err();
        assert_eq!(err.code(), "invalid_id", "{err:?}");
    }
    assert_eq!(fake.seen().len(), 1);
}

/// A path that is not UTF-8 cannot go into JSON. It is refused with the
/// daemon's code, and nothing is sent.
#[tokio::test]
async fn a_path_that_is_not_utf8_is_refused_before_any_request() {
    // How a path that is not UTF-8 is built differs by system.
    #[cfg(unix)]
    let name = {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::OsStr::from_bytes(b"caf\xe9").to_os_string()
    };
    #[cfg(windows)]
    let name = {
        use std::os::windows::ffi::OsStringExt;
        // A lone surrogate: valid in a Windows path, not in UTF-8.
        std::ffi::OsString::from_wide(&[0x63, 0xD800])
    };
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let fake = Fake::start(serve_sites(&bodies)).await;
    let client = fake.client();
    let path = fake.data_dir().join(name);
    let err = client.park(&path).await.unwrap_err();
    assert_eq!(err.code(), "site_path_not_utf8", "{err:?}");
    let err = client.link("cafe", &path).await.unwrap_err();
    assert_eq!(err.code(), "site_path_not_utf8", "{err:?}");
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}

// ---------------------------------------------------------------------------
// Setup (task M4-13)

#[tokio::test]
async fn setup_posts_and_reads_the_report() {
    let report = SetupReport {
        results: vec![OpResult {
            op: "resolver_install".into(),
            status: OpStatus::Done,
            code: None,
            message: None,
        }],
        skipped: vec![SetupSkip {
            op: "trust_install".into(),
            code: "ca_not_ready".into(),
        }],
        ..SetupReport::default()
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let router = Router::new().route(
        "/v1/setup",
        post({
            let report = report.clone();
            move || {
                let (report, calls) = (report.clone(), calls.clone());
                async move {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        Json(report).into_response()
                    } else {
                        let busy = ErrorBody::new("setup_running", "another setup is running");
                        (StatusCode::CONFLICT, Json(busy)).into_response()
                    }
                }
            }
        }),
    );
    let fake = Fake::start(router).await;
    let client = fake.client();
    assert_eq!(client.setup().await.unwrap(), report);
    let err = client.setup().await.unwrap_err();
    assert_eq!(err.code(), "setup_running");
    assert!(
        matches!(err, ClientError::Api { status: 409, .. }),
        "{err:?}"
    );
    fake.assert_no_token(&err);

    let targets: Vec<String> = fake
        .seen()
        .into_iter()
        .map(|seen| format!("{} {}", seen.method, seen.target))
        .collect();
    assert_eq!(targets, ["POST /v1/setup", "POST /v1/setup"]);
    let bearer = format!("Bearer {}", fake.token);
    for seen in fake.seen() {
        assert_eq!(seen.authorization.as_deref(), Some(bearer.as_str()));
        assert!(!seen.origin);
    }
}

// ---------------------------------------------------------------------------
// Event streams

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
    let got = collect(fake.client().events().await.unwrap()).await;
    assert_eq!(got, items(&events));
}

#[tokio::test]
async fn a_lag_note_is_its_own_item() {
    let first = Event::ServiceChanged(caddy(ServiceState::Running));
    let body = format!(
        "{}: lagged 5\n\n{}",
        sse_data(&first),
        sse_data(&Event::ShuttingDown)
    );
    let router = Router::new().route("/v1/events", get(move || async move { sse_response(body) }));
    let fake = Fake::start(router).await;
    let got = collect(fake.client().events().await.unwrap()).await;
    assert_eq!(
        got,
        [
            StreamItem::Item(first),
            StreamItem::Lagged(5),
            StreamItem::Item(Event::ShuttingDown),
        ]
    );
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
    let _lock = DaemonLock::take(dir.path());
    write_file(dir.path(), port);
    let client = DaemonClient::connect(dir.path()).unwrap();
    assert_eq!(
        collect(client.events().await.unwrap()).await,
        items(&events)
    );
}

// ---------------------------------------------------------------------------
// Finding the daemon

#[tokio::test]
async fn no_daemon_or_a_dead_port_means_not_running() {
    let dir = TempDir::new().unwrap();
    let err = DaemonClient::connect(dir.path()).unwrap_err();
    assert_eq!(err.code(), "daemon_not_running", "{err:?}");

    // The lock is held but there is no file yet: still not running.
    let _lock = DaemonLock::take(dir.path());
    let err = DaemonClient::connect(dir.path()).unwrap_err();
    assert_eq!(err.code(), "daemon_not_running", "{err:?}");

    // A daemon that has stopped serving: nothing listens on its port.
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    };
    let token = write_file(dir.path(), port);
    let err = DaemonClient::connect(dir.path())
        .unwrap()
        .health()
        .await
        .unwrap_err();
    assert_eq!(err.code(), "daemon_not_running", "{err:?}");
    assert_no_token(&token, &err);
}

#[test]
fn a_broken_file_keeps_its_code() {
    let dir = TempDir::new().unwrap();
    let _lock = DaemonLock::take(dir.path());
    write_file(dir.path(), 1);
    let path = daemon_file_path(&dir.path().join("run"));
    fs::write(&path, "not json").unwrap();
    let err = DaemonClient::connect(dir.path()).unwrap_err();
    assert_eq!(err.code(), "daemon_file_invalid", "{err:?}");
}

/// A file left by a crash: the lock is free, and a program now listens on
/// the old port. It must never get the old token.
#[tokio::test]
async fn a_stale_file_with_a_free_lock_is_never_contacted() {
    let squatter = Fake::start(serve_health(health(VERSION, API_VERSION))).await;
    squatter.lock.release();

    let err = DaemonClient::connect(squatter.data_dir()).unwrap_err();
    assert_eq!(err.code(), "daemon_not_running", "{err:?}");
    assert!(!stop_daemon(squatter.data_dir()).await.unwrap());
    let missing = squatter.data_dir().join("no-marsad");
    let err = ensure_running_with(squatter.data_dir(), &missing)
        .await
        .unwrap_err();
    // It went straight to starting marsad.
    assert_eq!(err.code(), "daemon_start_failed", "{err:?}");
    assert!(err.to_string().contains("cannot run"), "{err}");
    assert!(squatter.seen().is_empty(), "{:?}", squatter.seen());
}

// ---------------------------------------------------------------------------
// The version handshake

#[tokio::test]
async fn ensure_running_returns_a_daemon_of_this_version() {
    let (router, shutdowns) = serve_shutdown(serve_health(health(VERSION, API_VERSION)), None);
    let fake = Fake::start(router).await;
    let missing = fake.data_dir().join("no-marsad");
    let client = ensure_running_with(fake.data_dir(), &missing)
        .await
        .unwrap();
    assert_eq!(client.port(), fake.port);
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_newer_daemon_is_used_as_is() {
    let (router, shutdowns) = serve_shutdown(serve_health(health("99.0.0", API_VERSION)), None);
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
    // Whatever its app version: the API check comes first.
    for app_version in [VERSION, "99.0.0", "0.0.0-old"] {
        let answer = health(app_version, API_VERSION + 1);
        let (router, shutdowns) = serve_shutdown(serve_health(answer), None);
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
async fn an_older_daemon_is_stopped_then_replaced() {
    let temp = TempDir::new().unwrap();
    let lock = DaemonLock::take(temp.path());
    let router = serve_health(health("0.0.0-old", API_VERSION));
    let (router, shutdowns) = serve_shutdown(router, Some((temp.path(), &lock)));
    let fake = Fake::serve(temp.path(), lock, router).await;

    // No `marsad` here, so the start after the stop fails, which shows it was
    // tried.
    let missing = temp.path().join("no-marsad");
    let err = ensure_running_with(temp.path(), &missing)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "daemon_start_failed", "{err:?}");
    assert!(err.to_string().contains("cannot run"), "{err}");
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    assert!(!daemon_file_path(&temp.path().join("run")).exists());
    fake.assert_no_token(&err);
}

/// Two clients find an older daemon at once. Whichever stops it, the other
/// one's new daemon answers while both wait, and both use it: neither starts
/// a second `marsad`.
#[tokio::test]
async fn two_clients_replacing_a_daemon_both_use_the_new_one() {
    let answer = Arc::new(Mutex::new(health("0.0.0-old", API_VERSION)));
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let router = Router::new()
        .route(
            "/v1/health",
            get({
                let answer = answer.clone();
                move || {
                    let now = answer.lock().expect("lock").clone();
                    async move { Json(now) }
                }
            }),
        )
        .route(
            "/v1/shutdown",
            post({
                let shutdowns = shutdowns.clone();
                move || {
                    shutdowns.fetch_add(1, Ordering::SeqCst);
                    // The new daemon a client started is up a little later.
                    let answer = answer.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        *answer.lock().expect("lock") = health(VERSION, API_VERSION);
                    });
                    async { StatusCode::ACCEPTED }
                }
            }),
        );
    let fake = Fake::start(router).await;
    let missing = fake.data_dir().join("no-marsad");
    let (a, b) = tokio::join!(
        ensure_running_with(fake.data_dir(), &missing),
        ensure_running_with(fake.data_dir(), &missing),
    );
    assert_eq!(a.unwrap().port(), fake.port);
    assert_eq!(b.unwrap().port(), fake.port);
    assert!((1..=2).contains(&shutdowns.load(Ordering::SeqCst)));
}

/// A daemon that holds the lock but has no file (it is stopping) is waited
/// for; once it lets go, `marsad` is started.
#[tokio::test]
async fn a_held_lock_is_waited_for_then_marsad_is_started() {
    let temp = TempDir::new().unwrap();
    let lock = DaemonLock::take(temp.path());
    let delay = Duration::from_millis(300);
    let releaser = tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        lock.release();
    });
    let started = Instant::now();
    let missing = temp.path().join("no-marsad");
    let err = ensure_running_with(temp.path(), &missing)
        .await
        .unwrap_err();
    assert!(started.elapsed() >= delay, "{:?}", started.elapsed());
    assert_eq!(err.code(), "daemon_start_failed", "{err:?}");
    assert!(err.to_string().contains("cannot run"), "{err}");
    releaser.await.unwrap();
}

#[tokio::test]
async fn no_daemon_and_no_marsad_is_a_start_failure() {
    let dir = TempDir::new().unwrap();
    let err = ensure_running_with(dir.path(), &dir.path().join("no-marsad"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "daemon_start_failed", "{err:?}");
}

// ---------------------------------------------------------------------------
// Starting marsad. The stand-in `marsad` is a shell script, so these run on
// macOS and Linux only; M3-09 tests the real binary on all three systems.

/// The new daemon takes the lock, then writes its file later. Meanwhile the
/// file of a crashed daemon still points at a program on its old port, which
/// must get nothing.
#[cfg(unix)]
#[tokio::test]
async fn ensure_running_waits_for_the_new_daemon_and_skips_the_stale_file() {
    let temp = TempDir::new().unwrap();
    let data = temp.path().join("data");
    let squatter = Fake::serve(
        &data,
        DaemonLock::take(&data),
        serve_health(health(VERSION, API_VERSION)),
    )
    .await;
    squatter.lock.release();

    let marsad = fake_marsad(temp.path(), "exec /bin/sleep 3");
    let later = daemon_later(&data, Duration::from_millis(250));
    let client = ensure_running_with(&data, &marsad).await.unwrap();
    let fake = later.await.unwrap();
    assert_eq!(client.port(), fake.port);
    assert!(squatter.seen().is_empty(), "{:?}", squatter.seen());
}

#[cfg(unix)]
#[tokio::test]
async fn marsad_that_stops_early_fails_at_once_and_names_its_log() {
    let temp = TempDir::new().unwrap();
    let marsad = fake_marsad(temp.path(), "exit 1");
    let started = Instant::now();
    let err = ensure_running_with(&temp.path().join("data"), &marsad)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "daemon_start_failed", "{err:?}");
    let shown = err.to_string();
    assert!(
        shown.contains("logs") && shown.contains("marsad.log"),
        "{shown}"
    );
    // At once means before the start wait runs out, not a fixed time: a new
    // script can take seconds to start on a busy machine (task M4-20).
    assert!(started.elapsed() < marsa_core::client::START_WAIT);
}

/// The first `marsad` exits with code 3 (the lock was taken a moment ago),
/// the lock is free now, so it is started once more. The fake daemon comes
/// up only after that second start, so the test does not depend on how fast
/// the system runs a new script.
#[cfg(unix)]
#[tokio::test]
async fn marsad_that_found_the_lock_taken_is_started_again() {
    let temp = TempDir::new().unwrap();
    let data = temp.path().join("data");
    fs::create_dir_all(&data).unwrap();
    let marsad = fake_marsad(
        temp.path(),
        r#"if [ -e "$2/first-run" ]; then : > "$2/second-run"; exec /bin/sleep 3; fi
: > "$2/first-run"
exit 3"#,
    );
    let later = tokio::spawn({
        let data = data.clone();
        async move {
            while !data.join("second-run").exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let lock = DaemonLock::take(&data);
            Fake::serve(&data, lock, serve_health(health(VERSION, API_VERSION))).await
        }
    });
    let client = ensure_running_with(&data, &marsad).await.unwrap();
    assert_eq!(client.port(), later.await.unwrap().port);
    assert!(data.join("first-run").exists());
}

// ---------------------------------------------------------------------------
// Stopping

#[tokio::test]
async fn stop_daemon_stops_it_and_says_whether_one_ran() {
    let temp = TempDir::new().unwrap();
    assert!(!stop_daemon(temp.path()).await.unwrap());

    let lock = DaemonLock::take(temp.path());
    let (router, shutdowns) = serve_shutdown(Router::new(), Some((temp.path(), &lock)));
    let _fake = Fake::serve(temp.path(), lock, router).await;
    assert!(stop_daemon(temp.path()).await.unwrap());
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    assert!(!stop_daemon(temp.path()).await.unwrap());
}

/// The window runs these on a tokio runtime, which needs `Send`.
#[test]
fn futures_and_streams_are_send() {
    fn is_send<T: Send>(_: &T) {}
    fn is_send_type<T: Send + Sync>() {}
    let dir = Path::new("unused");
    is_send(&ensure_running(dir));
    is_send(&ensure_running_with(dir, dir));
    is_send(&stop_daemon(dir));
    let dir = TempDir::new().unwrap();
    let _lock = DaemonLock::take(dir.path());
    write_file(dir.path(), 51234);
    let client = DaemonClient::connect(dir.path()).unwrap();
    let patch = SitePatch::default();
    is_send(&client.sites());
    is_send(&client.park(dir.path()));
    is_send(&client.link("shop", dir.path()));
    is_send(&client.update_site("shop", &patch));
    is_send_type::<DaemonClient>();
    is_send_type::<EventStream<Event>>();
}
