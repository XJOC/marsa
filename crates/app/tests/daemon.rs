//! Runs the real `marsad` on a temp data folder and tests its API from outside
//! (architecture.md section 11). Task M3-06.
//!
//! No fixed sleeps: each wait polls with a generous limit, so slow CI machines
//! pass and a broken daemon fails with its log.

use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use marsa_core::platform::daemon_file::{daemon_authority, daemon_file_path, read_daemon_file};
use marsa_core::types::{API_VERSION, ErrorBody, Event, Health, ServiceState, ServiceStatus};
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use tempfile::TempDir;

/// For starting and for a second daemon to give up.
const LIMIT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(20);

/// A running `marsad`. Dropping it kills the process, so a failed test leaves
/// nothing behind.
struct Marsad {
    child: Child,
    /// After `child`: the folder is deleted once the process is gone.
    dir: TempDir,
    port: u16,
    bearer: String,
}

impl Marsad {
    /// Starts `marsad` and waits for `run/daemon.json`.
    async fn start() -> Self {
        Self::start_in(TempDir::new().expect("temp data folder")).await
    }

    /// [`Marsad::start`] on a data folder a test filled first.
    async fn start_in(dir: TempDir) -> Self {
        let child = command(&dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start marsad");
        let mut marsad = Marsad {
            child,
            dir,
            port: 0,
            bearer: String::new(),
        };
        let started = Instant::now();
        loop {
            if let Ok(file) = read_daemon_file(&marsad.run_dir()) {
                assert_eq!(file.pid, marsad.child.id());
                marsad.port = file.port;
                marsad.bearer = file.token.bearer_header();
                return marsad;
            }
            if let Some(status) = marsad.child.try_wait().expect("check marsad") {
                panic!("marsad exited with {status}; log:\n{}", marsad.log());
            }
            assert!(
                started.elapsed() < LIMIT,
                "no daemon.json; log:\n{}",
                marsad.log()
            );
            tokio::time::sleep(POLL).await;
        }
    }

    fn run_dir(&self) -> PathBuf {
        self.dir.path().join("run")
    }

    fn log(&self) -> String {
        let path = self.dir.path().join("logs").join("marsad.log");
        fs::read_to_string(path).unwrap_or_default()
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", daemon_authority(self.port))
    }

    /// Waits until the process exits, `limit` after `since` at most.
    async fn exit_status(&mut self, since: Instant, limit: Duration) -> ExitStatus {
        loop {
            if let Some(status) = self.child.try_wait().expect("check marsad") {
                return status;
            }
            assert!(
                since.elapsed() < limit,
                "marsad still runs; log:\n{}",
                self.log()
            );
            tokio::time::sleep(POLL).await;
        }
    }
}

impl Drop for Marsad {
    fn drop(&mut self) {
        // Errors mean the process has already exited.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn command(dir: &TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_marsad"));
    command
        .arg("--data-dir")
        .arg(dir.path())
        .stdin(Stdio::null())
        .env("MARSA_LOG", "debug");
    command
}

/// `no_proxy`: a proxy from `HTTP_PROXY` would otherwise get the token.
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(LIMIT)
        .build()
        .expect("HTTP client")
}

async fn error_code(response: reqwest::Response) -> String {
    let body: ErrorBody = response.json().await.expect("an ErrorBody");
    body.error.code
}

#[tokio::test]
async fn health_needs_the_token_and_gives_the_version() {
    let marsad = Marsad::start().await;
    let client = client();
    let get = |path: &str| client.get(marsad.url(path));

    let response = get("/v1/health")
        .header(AUTHORIZATION, &marsad.bearer)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let health: Health = response.json().await.unwrap();
    let expected = Health {
        app_version: env!("CARGO_PKG_VERSION").into(),
        api_version: API_VERSION,
        pid: marsad.child.id(),
    };
    assert_eq!(health, expected);

    // The auth layer covers the fallback too: an unknown path says nothing
    // without the token.
    for path in ["/v1/health", "/v1/no-such-route"] {
        let response = get(path).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(error_code(response).await, "unauthorized", "{path}");
    }
    let response = get("/v1/no-such-route")
        .header(AUTHORIZATION, &marsad.bearer)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(error_code(response).await, "not_found");
}

/// What an uninstall left when its delete stopped half way goes before the
/// daemon serves (task M5-07); the versions stay.
#[tokio::test]
async fn the_start_deletes_unfinished_uninstalls() {
    let dir = TempDir::new().expect("temp data folder");
    let php = dir.path().join("runtimes").join("php");
    for folder in ["del-0a1b2c3d/bin", "8.3.12/bin"] {
        fs::create_dir_all(php.join(folder)).expect("a folder");
    }
    let marsad = Marsad::start_in(dir).await;
    let php = marsad.dir.path().join("runtimes").join("php");
    let names: Vec<_> = fs::read_dir(&php)
        .expect("runtimes/php")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    assert_eq!(names, ["8.3.12"]);
}

/// The start looks for a Caddy to start (task M5-16). None is installed
/// here: the log says so, and Caddy stays `stopped`, not `failed`. With a
/// Caddy installed it would take Marsa's own ports, so `tests/services.rs`
/// tests that start on free ports, in process.
#[tokio::test]
async fn the_start_without_caddy_starts_none_and_fails_nothing() {
    let marsad = Marsad::start().await;
    let started = Instant::now();
    while !marsad
        .log()
        .contains("no caddy is installed; it does not start with the daemon")
    {
        assert!(
            started.elapsed() < LIMIT,
            "no word about caddy; log:\n{}",
            marsad.log()
        );
        tokio::time::sleep(POLL).await;
    }
    let response = client()
        .get(marsad.url("/v1/services"))
        .header(AUTHORIZATION, &marsad.bearer)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let services: Vec<ServiceStatus> = response.json().await.unwrap();
    let caddy = services.iter().find(|service| service.id == "caddy");
    let caddy = caddy.expect("caddy is listed");
    assert_eq!((&caddy.state, caddy.pid), (&ServiceState::Stopped, None));
}

#[tokio::test]
async fn a_second_daemon_on_the_same_folder_exits_with_3() {
    let marsad = Marsad::start().await;

    let mut second = tokio::process::Command::from(command(&marsad.dir));
    let second = second.kill_on_drop(true).output();
    let output = tokio::time::timeout(LIMIT, second)
        .await
        .expect("the second marsad exits")
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("daemon_already_running"), "{stderr}");

    // The first daemon keeps its file and still answers.
    let file = read_daemon_file(&marsad.run_dir()).unwrap();
    assert_eq!((file.pid, file.port), (marsad.child.id(), marsad.port));
    let response = client()
        .get(marsad.url("/v1/health"))
        .header(AUTHORIZATION, &marsad.bearer)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn shutdown_ends_the_events_and_exits_0() {
    let mut marsad = Marsad::start().await;
    let client = client();

    let mut events = client
        .get(marsad.url("/v1/events"))
        .header(AUTHORIZATION, &marsad.bearer)
        .send()
        .await
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);
    assert_eq!(events.headers()[CONTENT_TYPE], "text/event-stream");

    let asked = Instant::now();
    let response = client
        .post(marsad.url("/v1/shutdown"))
        .header(AUTHORIZATION, &marsad.bearer)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(response.bytes().await.unwrap().is_empty());

    // `shutting_down`, then the end of the stream.
    let mut body = Vec::new();
    while let Some(chunk) = events.chunk().await.unwrap() {
        body.extend_from_slice(&chunk);
    }
    let body = String::from_utf8(body).unwrap();
    let received: Vec<Event> = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|json| serde_json::from_str(json).unwrap())
        .collect();
    assert_eq!(received, [Event::ShuttingDown], "{body}");

    let status = marsad.exit_status(asked, Duration::from_secs(5)).await;
    assert_eq!(status.code(), Some(0), "log:\n{}", marsad.log());
    assert!(!daemon_file_path(&marsad.run_dir()).exists());
    // The lock is free again.
    let lock = File::options()
        .write(true)
        .open(marsad.run_dir().join("daemon.lock"))
        .unwrap();
    lock.try_lock().unwrap();
}

/// SIGTERM and Ctrl+C (SIGINT) stop the daemon cleanly too. Unix only: Windows
/// has no SIGTERM, and a test cannot send Ctrl+C to one process there.
#[cfg(unix)]
#[tokio::test]
async fn sigterm_and_sigint_stop_the_daemon_cleanly() {
    for signal in ["-TERM", "-INT"] {
        let mut marsad = Marsad::start().await;
        let sent = Instant::now();
        let status = Command::new("/bin/kill")
            .args([signal, &marsad.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let status = marsad.exit_status(sent, Duration::from_secs(5)).await;
        assert_eq!(status.code(), Some(0), "{signal}; log:\n{}", marsad.log());
        assert!(!daemon_file_path(&marsad.run_dir()).exists(), "{signal}");
    }
}
