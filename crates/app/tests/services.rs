//! The services and logs routes against a fake Caddy (task M3-07). The fake is
//! this test binary, run again with `MARSA_FAKE_SERVICE` set, as in
//! `tests/supervisor.rs`. In `serve` mode it reads the `caddy.json` the daemon
//! wrote, and answers `GET /config/`, `GET /id/<tag>` for the `@id` of the
//! admin object (this data folder's tag), `POST /load`, and `POST /stop` on
//! the admin address in it, as Caddy does. Nothing listens outside
//! `127.0.0.1`, and no real Caddy is needed.
//!
//! The router runs in this process (`oneshot`), with the real auth layer.
//! The start of Caddy with the daemon (task M5-16) runs on the same daemon,
//! with no request.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, TcpListener};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, BodyDataStream, to_bytes};
use axum::extract::Path;
use axum::http::{Method, Request, StatusCode, Uri};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use marsa::daemon::api::router;
use marsa::daemon::auth::AuthConfig;
use marsa::daemon::caddy::{Applied, Launched, caddy_spec_for, start_at_launch};
use marsa::daemon::sites_sync;
use marsa::daemon::state::Daemon;
use marsa_core::caddy::{PhpPools, SERVER_ID};
use marsa_core::config::SitesConfig;
use marsa_core::platform::daemon_file::{Token, daemon_authority};
use marsa_core::platform::process;
use marsa_core::services::ServiceSpec;
use marsa_core::types::{ErrorBody, Event, LogLine, ServiceAction, ServiceState, ServiceStatus};
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
        "serve" => serve(true),
        // Its admin API comes 1 s late, so a test can act during the start.
        "slow" => {
            std::thread::sleep(Duration::from_secs(1));
            serve(true);
        }
        // Another program on our admin port: everything but our tag.
        "impostor" => serve(false),
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

/// `GET /config/` answers the config of the start, or of the last
/// `POST /load`, as Caddy does. `tagged`: `GET /id/<tag>` answers the admin
/// object; otherwise every id is unknown (404).
fn serve(tagged: bool) {
    let path = std::env::var(FAKE_CONFIG).expect("the config path");
    let config = std::fs::read_to_string(path).expect("caddy.json");
    let config: serde_json::Value = serde_json::from_str(&config).expect("JSON");
    let listen = config["admin"]["listen"].as_str().expect("admin.listen");
    let listen = listen.to_owned();
    let current = Arc::new(Mutex::new(config));
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
                get({
                    let current = Arc::clone(&current);
                    move || {
                        let config = current.lock().expect("the config").clone();
                        async move { Json(config) }
                    }
                }),
            )
            .route(
                "/id/{id}",
                get({
                    let current = Arc::clone(&current);
                    move |Path(id): Path<String>| {
                        let admin = current.lock().expect("the config")["admin"].clone();
                        async move {
                            if tagged && admin["@id"] == id.as_str() {
                                Ok(Json(admin))
                            } else {
                                Err(StatusCode::NOT_FOUND)
                            }
                        }
                    }
                }),
            )
            .route(
                "/load",
                post(move |Json(config): Json<serde_json::Value>| async move {
                    *current.lock().expect("the config") = config;
                    println!("fake loaded");
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
    /// `fake`: run the fake in this mode instead of the newest Caddy in
    /// `runtimes/caddy`. `None` leaves the real lookup, and nothing is
    /// installed there.
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
            let program = std::env::current_exe().expect("the test binary has a path");
            caddy.spec = Some(ServiceSpec {
                // Runs only the entry point, under libtest and nextest alike.
                args: ["fake_caddy", "--exact", "--nocapture"]
                    .map(String::from)
                    .to_vec(),
                env: BTreeMap::from([
                    (FAKE.to_owned(), mode.to_owned()),
                    (FAKE_CONFIG.to_owned(), config),
                ]),
                stop_grace: Duration::from_millis(500),
                ..caddy_spec_for(&caddy.paths, program)
            });
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

/// The newest version folder counts only with its program in it.
#[tokio::test]
async fn start_with_a_version_folder_but_no_program_is_409_and_failed() {
    let app = App::new(None);
    let version = app.dir.path().join("runtimes").join("caddy").join("2.11.8");
    std::fs::create_dir_all(&version).expect("a version folder");

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

// Caddy starts with the daemon (task M5-16): what `server::run` does at
// start-up, with no request.

/// Polls until Caddy's task is in `procs`: the start spawned it, and waits
/// for its admin API now.
async fn until_caddy_runs(app: &App) {
    let started = Instant::now();
    while !app.daemon.proc_alive("caddy") {
        assert!(started.elapsed() < WAIT, "caddy never started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// What the fake's admin API runs now.
async fn admin_config(port: u16) -> serde_json::Value {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(WAIT)
        .build()
        .expect("an HTTP client");
    let response = client
        .get(format!("http://127.0.0.1:{port}/config/"))
        .send()
        .await
        .expect("the fake's admin API");
    response.json().await.expect("a JSON config")
}

/// The host of the first secure site in `config`.
fn first_host(config: &serde_json::Value) -> Option<&str> {
    config["apps"]["http"]["servers"]["marsa-tls"]["routes"][0]["match"][0]["host"][0].as_str()
}

fn written_config(app: &App) -> serde_json::Value {
    let path = app.dir.path().join("caddy").join("caddy.json");
    let text = std::fs::read_to_string(path).expect("caddy.json");
    serde_json::from_str(&text).expect("JSON")
}

/// Parks a folder with one static site, `shop`.
async fn park_shop(app: &App) -> TempDir {
    let parked = TempDir::new().expect("a parked folder");
    let shop = parked.path().join("shop");
    std::fs::create_dir(&shop).expect("a site folder");
    std::fs::write(shop.join("index.html"), "hi").expect("index.html");
    let dir = parked.path().to_path_buf();
    let park = move |sites: &mut SitesConfig| {
        sites.parked.push(dir);
        Ok(true)
    };
    app.daemon
        .change_sites(park)
        .await
        .expect("the folder is parked");
    parked
}

#[tokio::test]
async fn an_installed_caddy_starts_with_the_daemon() {
    let app = App::new(Some("serve"));
    assert_eq!(app.caddy().await.state, ServiceState::Stopped);

    let launched = start_at_launch(&app.daemon).wait(WAIT).await;
    let Some(Launched::Ran(status)) = launched else {
        panic!("not started: {launched:?}");
    };
    assert_eq!(status.state, ServiceState::Running);
    assert!(status.pid.is_some());
    let caddy = app.caddy().await;
    assert_eq!(
        (caddy.state, caddy.pid),
        (ServiceState::Running, status.pid)
    );
    app.wait_for_line("fake serving").await;
    // It runs the config the daemon wrote.
    assert_eq!(admin_config(app.admin_port).await, written_config(&app));

    app.daemon.stop_procs(WAIT).await;
    assert_eq!(app.caddy().await.state, ServiceState::Stopped);
}

#[tokio::test]
async fn no_caddy_installed_starts_nothing_and_fails_nothing() {
    let app = App::new(None);
    let launched = start_at_launch(&app.daemon).wait(WAIT).await;
    assert_eq!(launched, Some(Launched::NotInstalled));
    // A version folder without its program is no Caddy either.
    let version = app.dir.path().join("runtimes").join("caddy").join("2.11.8");
    std::fs::create_dir_all(&version).unwrap();
    let launched = start_at_launch(&app.daemon).wait(WAIT).await;
    assert_eq!(launched, Some(Launched::NotInstalled));

    // Stopped, not failed, with no line: nothing was tried.
    let caddy = app.caddy().await;
    assert_eq!((caddy.state, caddy.pid), (ServiceState::Stopped, None));
    assert!(!app.daemon.proc_alive("caddy"));
    let lines: Vec<LogLine> = app.json("GET", "/v1/logs/caddy", StatusCode::OK).await;
    assert!(lines.is_empty(), "{lines:?}");
    assert!(!app.dir.path().join("caddy").join("caddy.json").exists());
}

#[tokio::test]
async fn a_failed_start_with_the_daemon_names_why_and_the_api_serves_on() {
    let app = App::new(Some("port_in_use"));
    let launched = start_at_launch(&app.daemon).wait(WAIT).await;
    let Some(Launched::Ran(status)) = launched else {
        panic!("not run: {launched:?}");
    };
    assert_eq!((status.state, status.pid), (failed("port_in_use"), None));

    // The daemon serves on, and `marsa services` shows why.
    let health = app.send("GET", "/v1/health").await;
    assert_eq!(health.status(), StatusCode::OK);
    assert_eq!(app.caddy().await.state, failed("port_in_use"));
    let lines: Vec<LogLine> = app.json("GET", "/v1/logs/caddy", StatusCode::OK).await;
    let taken = lines
        .iter()
        .filter(|line| line.text.contains("address already in use"));
    assert_eq!(taken.count(), 1, "{lines:?}");
    assert!(!app.daemon.proc_alive("caddy"));
    // The start let go of the busy flag: a start by hand runs.
    let again: ServiceStatus = app
        .json("POST", "/v1/services/caddy/start", StatusCode::OK)
        .await;
    assert_eq!(again.state, failed("port_in_use"));
}

/// Task M5-16b: a start by hand while the daemon starts Caddy waits for that
/// start, then finds Caddy running. No second Caddy.
#[tokio::test]
async fn a_start_by_hand_during_the_start_with_the_daemon_waits_for_it() {
    let app = App::new(Some("slow"));
    let launch = start_at_launch(&app.daemon);
    // Spawned; its admin API comes 1 s later.
    until_caddy_runs(&app).await;

    let started: ServiceStatus = app
        .json("POST", "/v1/services/caddy/start", StatusCode::OK)
        .await;
    let launched = launch.wait(WAIT).await;
    let Some(Launched::Ran(first)) = launched else {
        panic!("not run: {launched:?}");
    };
    assert_eq!(first.state, ServiceState::Running);
    assert_eq!(
        (started.state, started.pid),
        (ServiceState::Running, first.pid)
    );
    // One Caddy ran, not two.
    app.wait_for_line("fake serving").await;
    app.daemon.stop_procs(WAIT).await;
}

/// Task M5-16b: `marsa services stop caddy` starts the daemon, which starts
/// Caddy. The stop waits for that start, then stops Caddy: it is not lost.
#[tokio::test]
async fn a_stop_during_the_start_with_the_daemon_waits_and_stops_caddy() {
    let app = App::new(Some("slow"));
    let launch = start_at_launch(&app.daemon);
    until_caddy_runs(&app).await;

    let stopped: ServiceStatus = app
        .json("POST", "/v1/services/caddy/stop", StatusCode::OK)
        .await;
    assert_eq!((stopped.state, stopped.pid), (ServiceState::Stopped, None));
    let launched = launch.wait(WAIT).await;
    let Some(Launched::Ran(first)) = launched else {
        panic!("not run: {launched:?}");
    };
    assert_eq!(first.state, ServiceState::Running);
    assert!(!app.daemon.proc_alive("caddy"));
    assert_eq!(app.caddy().await.state, ServiceState::Stopped);
    // Our Caddy answered with our tag, so it got `POST /stop`.
    app.wait_for_line("fake stopping").await;
}

/// A wait that runs out is still `service_busy`, and `run` does not wait.
#[tokio::test]
async fn a_wait_that_runs_out_is_busy() {
    // Deaf: the start holds Caddy for its whole 3 s limit.
    let app = App::with_ready_limit(Some("deaf"), Duration::from_secs(3));
    let launch = start_at_launch(&app.daemon);
    until_caddy_runs(&app).await;

    let wait = Duration::from_millis(300);
    let began = Instant::now();
    let caddy = &app.daemon.caddy;
    let err = caddy
        .run_waiting(&app.daemon, ServiceAction::Stop, wait)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "service_busy");
    assert!(began.elapsed() >= wait);
    let err = caddy
        .run(&app.daemon, ServiceAction::Start)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "service_busy");

    let launched = launch.wait(WAIT).await;
    let Some(Launched::Ran(status)) = launched else {
        panic!("not run: {launched:?}");
    };
    assert_eq!(status.state, failed("caddy_start_timeout"));
    assert!(!app.daemon.proc_alive("caddy"));
    app.wait_for_line("fake deaf").await;
}

#[tokio::test]
async fn a_shutdown_during_the_start_with_the_daemon_leaves_no_caddy() {
    // A daemon that already stops starts nothing.
    let app = App::new(Some("serve"));
    app.daemon.shutdown.cancel();
    let launched = start_at_launch(&app.daemon).wait(WAIT).await;
    assert_eq!(launched, Some(Launched::ShuttingDown));
    assert!(!app.daemon.proc_alive("caddy"));
    assert_eq!(app.caddy().await.state, ServiceState::Stopped);

    // A stop while the start waits for a Caddy that never answers.
    let app = App::new(Some("deaf"));
    let launch = start_at_launch(&app.daemon);
    until_caddy_runs(&app).await;
    let stopping = Instant::now();
    app.daemon.shutdown.cancel();
    assert_eq!(launch.wait(WAIT).await, Some(Launched::ShuttingDown));
    // It did not wait for its 30 s limit.
    assert!(stopping.elapsed() < WAIT / 2);
    // Then the shutdown stops `procs`, and Caddy is among them.
    app.daemon.stop_procs(WAIT).await;
    let caddy = app.caddy().await;
    assert_eq!((caddy.state, caddy.pid), (ServiceState::Stopped, None));
    assert!(!app.daemon.proc_alive("caddy"));
    assert!(!app.dir.path().join("run").join("caddy.pid").exists());
}

/// Caddy runs before the first sites pass: it starts without the sites, and
/// the pass sends them with `POST /load`.
#[tokio::test]
async fn caddy_that_starts_before_the_first_pass_gets_the_sites_through_load() {
    let app = App::new(Some("serve"));
    let _parked = park_shop(&app).await;
    let launched = start_at_launch(&app.daemon).wait(WAIT).await;
    let Some(Launched::Ran(status)) = launched else {
        panic!("not started: {launched:?}");
    };
    assert_eq!(status.state, ServiceState::Running);
    assert_eq!(first_host(&admin_config(app.admin_port).await), None);

    let sync = sites_sync::start(&app.daemon);
    let started = Instant::now();
    let config = loop {
        let config = admin_config(app.admin_port).await;
        if first_host(&config).is_some() {
            break config;
        }
        assert!(started.elapsed() < WAIT, "no site reached caddy: {config}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(first_host(&config), Some("shop.test"));
    assert_eq!(config, written_config(&app));

    app.daemon.shutdown.cancel();
    sync.stop().await;
    app.daemon.stop_procs(WAIT).await;
}

/// The first sites pass gave its config before Caddy started: Caddy starts on
/// it. No sync task runs here, so nothing sends a `POST /load`.
#[tokio::test]
async fn caddy_that_starts_after_the_first_pass_starts_on_the_sites() {
    let app = App::new(Some("serve"));
    let _parked = park_shop(&app).await;
    // What the pass does at its Caddy step. Caddy does not run, so it only
    // writes the file.
    let sites = app.daemon.sites().unwrap().sites;
    let applied = app
        .daemon
        .caddy
        .apply(&app.daemon, sites, PhpPools::default())
        .await;
    assert_eq!(applied.unwrap(), Applied::Written);

    let launched = start_at_launch(&app.daemon).wait(WAIT).await;
    let Some(Launched::Ran(status)) = launched else {
        panic!("not started: {launched:?}");
    };
    assert_eq!(status.state, ServiceState::Running);
    let config = admin_config(app.admin_port).await;
    assert_eq!(first_host(&config), Some("shop.test"));
    assert_eq!(config, written_config(&app));

    app.daemon.stop_procs(WAIT).await;
}

// Task M5-16b: only our tag makes a Caddy ours. `GET /config/` answers on
// any Caddy.

/// Our process answers `GET /config/` and `POST /stop`, but not our tag:
/// it is not taken for our Caddy, and gets no `POST /stop`.
#[tokio::test]
async fn a_caddy_without_our_tag_is_never_ready_and_gets_no_stop() {
    let app = App::with_ready_limit(Some("impostor"), Duration::from_secs(1));
    let status: ServiceStatus = app
        .json("POST", "/v1/services/caddy/start", StatusCode::OK)
        .await;
    assert_eq!(
        (status.state, status.pid),
        (failed("caddy_start_timeout"), None)
    );
    assert!(!app.daemon.proc_alive("caddy"));
    let lines = app.wait_for_line("fake serving").await;
    let stops = lines.iter().filter(|line| line.text == "fake stopping");
    assert_eq!(stops.count(), 0, "{lines:?}");
}

/// Another Marsa's Caddy (a second user's) on our admin port, a fake here:
/// records each request, answers `GET /config/` and its `marsa-srv` tag.
async fn other_marsa(port: u16) -> (Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().fallback({
        let calls = Arc::clone(&calls);
        move |method: Method, uri: Uri| {
            let call = format!("{method} {}", uri.path());
            calls.lock().expect("the calls").push(call);
            async move {
                let shared = format!("/id/{SERVER_ID}");
                match (method, uri.path()) {
                    (Method::GET, "/config/") => Ok(Json(serde_json::json!({}))),
                    (Method::GET, path) if path == shared => {
                        Ok(Json(serde_json::json!({ "@id": SERVER_ID })))
                    }
                    _ => Err(StatusCode::NOT_FOUND),
                }
            }
        }
    });
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .expect("the admin port");
    let server = tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    (calls, server)
}

/// Where no orphan check runs first (Linux and Windows), only the tag keeps
/// the start from taking another Marsa's Caddy for ours. On macOS the orphan
/// check finds it first. Either way it gets no config and no stop.
#[tokio::test]
async fn another_marsas_caddy_on_our_admin_port_gets_no_config_and_no_stop() {
    let app = App::with_ready_limit(Some("deaf"), Duration::from_secs(1));
    let (calls, server) = other_marsa(app.admin_port).await;

    let status: ServiceStatus = app
        .json("POST", "/v1/services/caddy/start", StatusCode::OK)
        .await;
    let code = if process::ORPHAN_RECOVERY {
        "caddy_other_marsa"
    } else {
        "caddy_start_timeout"
    };
    assert_eq!(status.state, failed(code));
    assert!(!app.daemon.proc_alive("caddy"));
    // The first sites pass after it only writes the file.
    let applied = app
        .daemon
        .caddy
        .apply(&app.daemon, Vec::new(), PhpPools::default())
        .await;
    assert_eq!(applied.unwrap(), Applied::Written);

    let calls = calls.lock().unwrap().clone();
    assert!(!calls.is_empty(), "it was asked who it is");
    assert!(
        calls.iter().all(|call| call.starts_with("GET ")),
        "{calls:?}"
    );
    server.abort();
}
