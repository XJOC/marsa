//! The services and logs routes against a fake Caddy (task M3-07). The fake is
//! this test binary, run again with `MARSA_FAKE_SERVICE` set, as in
//! `tests/supervisor.rs`. In `serve` mode it reads the `caddy.json` the daemon
//! wrote, and answers `GET /config/` and `POST /stop` on the admin address in
//! it. Nothing listens outside `127.0.0.1`, and no real Caddy is needed.
//!
//! The router runs in this process (`oneshot`), with the real auth layer.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, TcpListener};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, BodyDataStream, to_bytes};
use axum::http::{Request, StatusCode, Uri};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use marsa::daemon::api::router;
use marsa::daemon::auth::AuthConfig;
use marsa::daemon::state::Daemon;
use marsa_core::platform::daemon_file::{Token, daemon_authority};
use marsa_core::services::ServiceSpec;
use marsa_core::types::{ErrorBody, Event, LogLine, ServiceState, ServiceStatus};
use serde::de::DeserializeOwned;
use tempfile::TempDir;
use tokio_stream::StreamExt as _;
use tower::ServiceExt as _;

const FAKE: &str = "MARSA_FAKE_SERVICE";
/// Where the fake finds `caddy.json`.
const FAKE_CONFIG: &str = "MARSA_FAKE_CADDY_CONFIG";
/// The daemon port in the `Host` header. No socket is bound for it.
const PORT: u16 = 51235;
/// Generous: a test binary can start slowly on a busy CI machine.
const WAIT: Duration = Duration::from_secs(30);

/// Not a check: the fake Caddy's entry point. In a normal test run the
/// variable is unset and this returns at once. The fake's lines start with
/// `fake `, because the harness prints lines of its own.
#[test]
fn fake_caddy() {
    let Ok(mode) = std::env::var(FAKE) else {
        return;
    };
    match mode.as_str() {
        "serve" => serve(),
        // What Caddy prints when another program holds its HTTP port.
        "port_in_use" => {
            eprintln!(
                "Error: loading initial config: loading new config: http app module: start: \
                 listening on 127.0.0.1:18080: listen tcp 127.0.0.1:18080: bind: address already in use"
            );
            std::process::exit(1);
        }
        // Runs, but never opens the admin API.
        "deaf" => {
            println!("fake deaf");
            std::thread::sleep(Duration::from_secs(3600));
        }
        other => panic!("unknown fake mode {other}"),
    }
}

fn serve() {
    let path = std::env::var(FAKE_CONFIG).expect("the config path");
    let config = std::fs::read_to_string(path).expect("caddy.json");
    let config: serde_json::Value = serde_json::from_str(&config).expect("JSON");
    let listen = config["admin"]["listen"].as_str().expect("admin.listen");
    let listen = listen.to_owned();
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(&listen)
            .await
            .expect("the admin address");
        println!("fake serving");
        eprintln!("fake admin on {listen}");
        let app = Router::new()
            .route(
                "/config/",
                get(move || {
                    let config = config.clone();
                    async move { Json(config) }
                }),
            )
            .route(
                "/stop",
                post(|| async {
                    println!("fake stopping");
                    // Like Caddy: answer first, then exit.
                    tokio::spawn(async {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        std::process::exit(0);
                    });
                }),
            )
            .fallback(|uri: Uri| async move { println!("fake request {}", uri.path()) });
        axum::serve(listener, app).await.expect("serve");
    });
}

/// A daemon on a temp data folder, and its router.
struct App {
    daemon: Arc<Daemon>,
    router: Router,
    bearer: String,
    admin_port: u16,
    /// Last: deleted after the daemon.
    dir: TempDir,
}

impl App {
    /// `fake`: run the fake in this mode instead of `bin/caddy`. `None`
    /// leaves the real path, where nothing is installed.
    fn new(fake: Option<&str>) -> Self {
        Self::with_ready_limit(fake, WAIT)
    }

    fn with_ready_limit(fake: Option<&str>, ready_limit: Duration) -> Self {
        let dir = TempDir::new().expect("a temp data folder");
        let mut daemon = Daemon::new(dir.path().to_path_buf());
        let admin_port = free_port();
        if let Some(mode) = fake {
            let caddy = &mut daemon.caddy;
            caddy.input.admin_port = admin_port;
            // Free ports, so the orphan check before a start never meets a
            // Caddy that runs on this machine on Marsa's own ports.
            caddy.input.http_port = free_port();
            caddy.input.https_port = free_port();
            caddy.ready_limit = ready_limit;
            let config = caddy.paths.config_file.to_string_lossy().into_owned();
            caddy.spec = ServiceSpec {
                program: std::env::current_exe().expect("the test binary has a path"),
                // Runs only the entry point, under libtest and nextest alike.
                args: ["fake_caddy", "--exact", "--nocapture"]
                    .map(String::from)
                    .to_vec(),
                env: BTreeMap::from([
                    (FAKE.to_owned(), mode.to_owned()),
                    (FAKE_CONFIG.to_owned(), config),
                ]),
                stop_grace: Duration::from_millis(500),
                ..caddy.spec.clone()
            };
        }
        let token = Token::generate().expect("a token");
        let bearer = token.bearer_header();
        let daemon = Arc::new(daemon);
        let router = router(daemon.clone(), AuthConfig::new(Arc::new(token), PORT));
        App {
            daemon,
            router,
            bearer,
            admin_port,
            dir,
        }
    }

    async fn send(&self, method: &str, path: &str) -> Response {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", daemon_authority(PORT))
            .header("authorization", &self.bearer)
            .body(Body::empty())
            .expect("a request");
        self.router
            .clone()
            .oneshot(request)
            .await
            .expect("a response")
    }

    /// Sends, checks the status, and reads the JSON body.
    async fn json<T: DeserializeOwned>(&self, method: &str, path: &str, want: StatusCode) -> T {
        let response = self.send(method, path).await;
        let status = response.status();
        let body = to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("a body");
        let text = String::from_utf8_lossy(&body);
        assert_eq!(status, want, "{method} {path}: {text}");
        serde_json::from_slice(&body).unwrap_or_else(|err| panic!("{method} {path}: {err}: {text}"))
    }

    async fn error(&self, method: &str, path: &str, want: StatusCode) -> String {
        let body: ErrorBody = self.json(method, path, want).await;
        body.error.code
    }

    async fn caddy(&self) -> ServiceStatus {
        let list: Vec<ServiceStatus> = self.json("GET", "/v1/services", StatusCode::OK).await;
        assert_eq!(list.len(), 1, "{list:?}");
        list.into_iter().next().expect("one service")
    }

    /// Polls `GET /v1/logs/caddy` until `count` lines have `text`.
    async fn wait_for_lines(&self, text: &str, count: usize) -> Vec<LogLine> {
        let started = Instant::now();
        loop {
            let lines: Vec<LogLine> = self.json("GET", "/v1/logs/caddy", StatusCode::OK).await;
            if lines.iter().filter(|line| line.text == text).count() == count {
                return lines;
            }
            assert!(
                started.elapsed() < WAIT,
                "not {count} {text:?} in {lines:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_for_line(&self, text: &str) -> Vec<LogLine> {
        self.wait_for_lines(text, 1).await
    }
}

/// A port that was free a moment ago, on `127.0.0.1`.
fn free_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a free port");
    listener.local_addr().expect("an address").port()
}

/// Reads the `data:` lines of an SSE body.
struct Follow {
    body: BodyDataStream,
    pending: String,
    lines: Vec<LogLine>,
}

impl Follow {
    fn new(response: Response) -> Self {
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        Follow {
            body: response.into_body().into_data_stream(),
            pending: String::new(),
            lines: Vec::new(),
        }
    }

    /// Reads until a line has `text`.
    async fn until(&mut self, text: &str) {
        while !self.lines.iter().any(|line| line.text == text) {
            let chunk = tokio::time::timeout(WAIT, self.body.next())
                .await
                .unwrap_or_else(|_| panic!("no line {text:?} in {:?}", self.lines))
                .expect("the stream is open")
                .expect("a chunk");
            self.push(&chunk);
        }
    }

    /// Reads to the end of the stream.
    async fn end(&mut self) {
        while let Some(chunk) = tokio::time::timeout(WAIT, self.body.next())
            .await
            .expect("the stream ends")
        {
            self.push(&chunk.expect("a chunk"));
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.pending
            .push_str(std::str::from_utf8(chunk).expect("UTF-8"));
        while let Some(end) = self.pending.find("\n\n") {
            for field in self.pending[..end].lines() {
                if let Some(json) = field.strip_prefix("data: ") {
                    self.lines
                        .push(serde_json::from_str(json).expect("a LogLine"));
                }
            }
            self.pending.drain(..end + 2);
        }
    }
}

fn failed(code: &str) -> ServiceState {
    ServiceState::Failed { code: code.into() }
}

/// Each `seq` once, in order: no line came twice through the follow stream.
fn assert_in_order(lines: &[LogLine]) {
    let seqs: Vec<u64> = lines.iter().map(|line| line.seq).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
}

#[tokio::test]
async fn start_shows_running_and_logs_then_stop() {
    let app = App::new(Some("serve"));
    // Listed before the first start.
    let caddy = app.caddy().await;
    assert_eq!(
        (caddy.id.as_str(), caddy.state),
        ("caddy", ServiceState::Stopped)
    );

    let started: ServiceStatus = app
        .json("POST", "/v1/services/caddy/start", StatusCode::OK)
        .await;
    assert_eq!(started.state, ServiceState::Running);
    let pid = started.pid.expect("a pid");
    // `caddy.json` was written before the start: the fake read its admin
    // address from it.
    let config = std::fs::read_to_string(app.dir.path().join("caddy").join("caddy.json")).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config).unwrap();
    let admin = format!("127.0.0.1:{}", app.admin_port);
    assert_eq!(config["admin"]["listen"], admin.as_str());
    assert_eq!(app.caddy().await.state, ServiceState::Running);

    // A second start changes nothing.
    let again: ServiceStatus = app
        .json("POST", "/v1/services/caddy/start", StatusCode::OK)
        .await;
    assert_eq!((again.state, again.pid), (ServiceState::Running, Some(pid)));

    // One line from each pipe.
    app.wait_for_line("fake serving").await;
    app.wait_for_line(&format!("fake admin on {admin}")).await;

    // Follow: the recent lines, then a new one, each once.
    let mut follow = Follow::new(app.send("GET", "/v1/logs/caddy?follow=true").await);
    follow.until("fake serving").await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    client
        .get(format!("http://{admin}/hello"))
        .send()
        .await
        .unwrap();
    follow.until("fake request /hello").await;
    assert_in_order(&follow.lines);

    let stopped: ServiceStatus = app
        .json("POST", "/v1/services/caddy/stop", StatusCode::OK)
        .await;
    assert_eq!((stopped.state, stopped.pid), (ServiceState::Stopped, None));
    assert!(!app.daemon.proc_alive("caddy"));
    // Caddy has exited, so its pid file is gone (ADR 0009).
    assert!(!app.dir.path().join("run").join("caddy.pid").exists());
    // A second stop changes nothing.
    let again: ServiceStatus = app
        .json("POST", "/v1/services/caddy/stop", StatusCode::OK)
        .await;
    assert_eq!(again.state, ServiceState::Stopped);

    // The lines stay readable after the stop, and are in `logs/caddy.log`.
    app.wait_for_line("fake stopping").await;
    let file = std::fs::read_to_string(app.dir.path().join("logs").join("caddy.log")).unwrap();
    assert!(file.contains("fake serving\n"), "{file:?}");
    assert!(file.contains("fake request /hello\n"), "{file:?}");

    // `shutting_down` ends the follow stream.
    app.daemon.shutdown.cancel();
    app.daemon.publish(Event::ShuttingDown);
    follow.end().await;
    assert_in_order(&follow.lines);
}

#[tokio::test]
async fn restart_keeps_seq_growing_and_shutdown_stops_caddy() {
    let app = App::new(Some("serve"));
    // `restart` on a stopped Caddy starts it.
    let first: ServiceStatus = app
        .json("POST", "/v1/services/caddy/restart", StatusCode::OK)
        .await;
    assert_eq!(first.state, ServiceState::Running);
    app.wait_for_line("fake serving").await;

    let second: ServiceStatus = app
        .json("POST", "/v1/services/caddy/restart", StatusCode::OK)
        .await;
    assert_eq!(second.state, ServiceState::Running);
    assert_ne!(second.pid, first.pid);
    // Both runs print `fake serving`. The second run's lines come after the
    // first run's: `seq` never goes back.
    let lines = app.wait_for_lines("fake serving", 2).await;
    assert_in_order(&lines);
    assert!(lines.iter().any(|line| line.text == "fake stopping"));

    // The daemon's shutdown stops Caddy through `procs`.
    app.daemon.stop_procs(WAIT).await;
    let caddy = app.caddy().await;
    assert_eq!((caddy.state, caddy.pid), (ServiceState::Stopped, None));
}

#[tokio::test]
async fn unknown_ids_actions_and_queries_are_errors() {
    let app = App::new(Some("serve"));
    let cases = [
        ("POST /v1/services/php/start", 404, "service_not_found"),
        ("POST /v1/services/php/kill", 404, "service_not_found"),
        ("POST /v1/services/caddy/kill", 400, "unknown_action"),
        ("POST /v1/services/caddy/Start", 400, "unknown_action"),
        ("GET /v1/logs/php", 404, "service_not_found"),
        ("GET /v1/logs/caddy?follow=maybe", 400, "invalid_query"),
        ("POST /v1/services/%FF/start", 400, "invalid_path"),
        ("GET /v1/services/caddy/start", 405, "method_not_allowed"),
    ];
    for (request, status, code) in cases {
        let (method, path) = request.split_once(' ').unwrap();
        let status = StatusCode::from_u16(status).unwrap();
        assert_eq!(app.error(method, path, status).await, code, "{request}");
    }
    // Nothing started, and the logs are an empty list.
    assert_eq!(app.caddy().await.state, ServiceState::Stopped);
    let lines: Vec<LogLine> = app
        .json("GET", "/v1/logs/caddy?follow=false", StatusCode::OK)
        .await;
    assert!(lines.is_empty());
}

#[tokio::test]
async fn start_without_caddy_is_409_and_failed() {
    let app = App::new(None);
    let code = app
        .error("POST", "/v1/services/caddy/start", StatusCode::CONFLICT)
        .await;
    assert_eq!(code, "caddy_not_installed");
    assert_eq!(app.caddy().await.state, failed("caddy_not_installed"));
    assert!(!app.daemon.proc_alive("caddy"));
}

#[tokio::test]
async fn a_taken_port_fails_without_a_restart() {
    let app = App::new(Some("port_in_use"));
    let status: ServiceStatus = app
        .json("POST", "/v1/services/caddy/start", StatusCode::OK)
        .await;
    assert_eq!(status.state, failed("port_in_use"));
    assert_eq!((status.restarts, status.pid), (0, None));
    // The task has ended, so no restart can come later.
    assert!(!app.daemon.proc_alive("caddy"));
    let lines: Vec<LogLine> = app.json("GET", "/v1/logs/caddy", StatusCode::OK).await;
    let errors = lines
        .iter()
        .filter(|l| l.text.contains("address already in use"));
    assert_eq!(errors.count(), 1, "one run only: {lines:?}");
}

#[tokio::test]
async fn a_caddy_whose_admin_api_never_answers_times_out() {
    let limit = Duration::from_secs(1);
    let app = App::with_ready_limit(Some("deaf"), limit);
    let started = Instant::now();
    let status: ServiceStatus = app
        .json("POST", "/v1/services/caddy/start", StatusCode::OK)
        .await;
    assert!(started.elapsed() >= limit);
    assert_eq!(
        (status.state, status.pid),
        (failed("caddy_start_timeout"), None)
    );
    // Stopped, not left running.
    assert!(!app.daemon.proc_alive("caddy"));
    app.wait_for_line("fake deaf").await;
}
