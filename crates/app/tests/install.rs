//! `marsa install` and `marsa runtimes` through the daemon (task M3-12).
//!
//! The real `marsa` binary talks to the real router, auth layer, and runtimes
//! routes, served in this process on `127.0.0.1` for a temp data folder, with
//! the lock and `run/daemon.json` that `marsad` makes. `marsa` finds that
//! daemon as it finds `marsad`, so it starts no other.
//!
//! Only the installer is a fake (`Daemon::installs.runner`). The real one
//! needs the published manifest, signed with the key built into the binary,
//! over https; tests cannot swap any of them, on purpose (task M2-05). The
//! core installer's own tests cover the signed manifest, the checksums, and
//! the atomic move. No network beyond loopback.

use std::fs::{self, File, OpenOptions};
use std::net::Ipv4Addr;
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use marsa::daemon::api::router;
use marsa::daemon::auth::AuthConfig;
use marsa::daemon::runtimes_api::{InstallFuture, RunInstall};
use marsa::daemon::state::Daemon;
use marsa_core::installer::{InstallError, InstallRequest, InstalledVersion};
use marsa_core::platform::daemon_file::{
    DaemonFile, Token, daemon_authority, daemon_lock_path, write_daemon_file,
};
use marsa_core::recipes::Platform;
use marsa_core::runtimes::{Runtime, RuntimesDir};
use marsa_core::types::{
    ErrorBody, Event, InstallAccepted, InstallEvent, InstallProgress, InstallUpdate, RuntimeList,
};
use serde::de::DeserializeOwned;
use tempfile::TempDir;
use tokio::sync::{Semaphore, broadcast};
use tower::ServiceExt as _;

/// Generous: a binary can start slowly on a busy CI machine.
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(20);
const TOTAL: u64 = 2_000_000;

/// What the fake installer was asked for.
type Asked = (Runtime, Option<String>, bool);

/// Sends the steps the real installer sends, then makes the version folder.
/// `8.9` is a version the "manifest" does not have.
#[derive(Default)]
struct FakeInstall {
    /// When set, each install waits here after its first event until the
    /// test adds a permit.
    gate: Option<Semaphore>,
    asked: Mutex<Vec<Asked>>,
}

impl FakeInstall {
    fn gated() -> Self {
        Self {
            gate: Some(Semaphore::new(0)),
            ..Self::default()
        }
    }

    fn open_gate(&self) {
        self.gate.as_ref().expect("a gated fake").add_permits(1);
    }

    fn asked(&self) -> Vec<Asked> {
        self.asked.lock().expect("lock").clone()
    }
}

impl RunInstall for FakeInstall {
    fn run<'a>(
        &'a self,
        data_dir: &'a Path,
        request: InstallRequest<'a>,
        events: &'a mut (dyn FnMut(InstallEvent) + Send),
    ) -> InstallFuture<'a> {
        Box::pin(async move {
            let asked = (
                request.runtime,
                request.version.map(str::to_owned),
                request.force,
            );
            self.asked.lock().expect("lock").push(asked);
            events(InstallEvent::ManifestVerified { serial: 3 });
            if let Some(gate) = &self.gate {
                // Dropped at once, so the gate stays open once opened.
                let _permit = gate.acquire().await.expect("the gate is not closed");
            }
            if request.version == Some("8.9") {
                return Err(InstallError::NoSuchVersion {
                    runtime: "php",
                    requested: Some("8.9".into()),
                    available: vec!["8.3.27".into(), "8.4.23".into()],
                });
            }
            let runtime = request.runtime;
            let version = match runtime {
                Runtime::Php => "8.4.23",
                Runtime::Node => "22.11.0",
                Runtime::Composer => "2.8.4",
            };
            events(InstallEvent::Selected {
                runtime,
                version: version.into(),
            });
            let runtimes = RuntimesDir::in_dir(data_dir);
            let dir = runtimes.runtime_dir(runtime).join(version);
            let installed = runtimes.installed(runtime)?;
            let already = !request.force && installed.iter().any(|v| v.to_string() == version);
            if !already {
                for done in [0, TOTAL / 2, TOTAL] {
                    let artifact = "cli".into();
                    events(InstallEvent::Downloading {
                        artifact,
                        done,
                        total: TOTAL,
                    });
                }
                events(InstallEvent::Verified {
                    artifact: "cli".into(),
                });
                events(InstallEvent::Extracting {
                    artifact: "cli".into(),
                });
                events(InstallEvent::Checking);
                fs::create_dir_all(dir.join("bin")).map_err(|source| InstallError::Io {
                    path: dir.clone(),
                    source,
                })?;
            }
            events(InstallEvent::Installed { dir: dir.clone() });
            Ok(InstalledVersion {
                runtime,
                version: version.parse().expect("a semver version"),
                dir,
                already,
            })
        })
    }
}

/// The daemon's routes on a loopback port, found by `marsa` through
/// `run/daemon.lock` and `run/daemon.json`. Dropping it stops the server.
struct Served {
    daemon: Arc<Daemon>,
    fake: Arc<FakeInstall>,
    /// The same router, for requests without a socket (`oneshot`).
    router: Router,
    bearer: String,
    port: u16,
    server: tokio::task::JoinHandle<()>,
    /// Held as `marsad` holds it, so `marsa` does not start one.
    _lock: File,
    /// Last: deleted after the rest.
    dir: TempDir,
}

impl Served {
    async fn start(fake: FakeInstall) -> Self {
        let dir = TempDir::new().expect("a temp data folder");
        let run = dir.path().join("run");
        fs::create_dir_all(&run).expect("the run folder");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(daemon_lock_path(&run))
            .expect("the lock file");
        lock.try_lock().expect("the daemon lock");

        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("a loopback port");
        let port = listener.local_addr().expect("an address").port();
        let fake = Arc::new(fake);
        let mut daemon = Daemon::new(dir.path().to_path_buf());
        daemon.installs.runner = fake.clone();
        let daemon = Arc::new(daemon);

        let file = DaemonFile {
            pid: std::process::id(),
            port,
            token: Token::generate().expect("a token"),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        write_daemon_file(&run, &file).expect("run/daemon.json");
        let bearer = file.token.bearer_header();
        let router = router(daemon.clone(), AuthConfig::new(Arc::new(file.token), port));
        let server = tokio::spawn({
            let router = router.clone();
            async move { axum::serve(listener, router).await.expect("serve") }
        });
        Served {
            daemon,
            fake,
            router,
            bearer,
            port,
            server,
            _lock: lock,
            dir,
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn command(&self, args: &[&str]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_marsa"));
        command
            .arg("--data-dir")
            .arg(self.path())
            .args(args)
            .env_remove("MARSA_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    async fn marsa(&self, args: &[&str]) -> Output {
        let output = self.command(args).output();
        tokio::time::timeout(WAIT, output)
            .await
            .unwrap_or_else(|_| panic!("marsa {args:?} still runs"))
            .expect("run marsa")
    }

    /// Checks one run of `marsa`: its exit code, stdout, and stderr.
    async fn expect(&self, args: &[&str], code: i32, stdout: &str, stderr: &str) {
        check(args, &self.marsa(args).await, code, stdout, stderr);
    }

    /// A request through the router, with the token and the right `Host`.
    async fn send(&self, method: &str, path: &str, body: Option<&str>) -> (StatusCode, Vec<u8>) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", daemon_authority(self.port))
            .header("authorization", &self.bearer);
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let request = request
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_owned())))
            .expect("a request");
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("a response");
        let status = response.status();
        let body = to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("a body");
        (status, body.to_vec())
    }

    async fn json<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        want: StatusCode,
    ) -> T {
        let (status, bytes) = self.send(method, path, body).await;
        let text = String::from_utf8_lossy(&bytes);
        assert_eq!(status, want, "{method} {path}: {text}");
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|err| panic!("{method} {path}: {err}: {text}"))
    }

    async fn error(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        want: StatusCode,
    ) -> String {
        let body: ErrorBody = self.json(method, path, body, want).await;
        body.error.code
    }

    /// Waits until an install runs, or none does.
    async fn wait_running(&self, running: bool) {
        let started = Instant::now();
        while self.daemon.installs.is_running() != running {
            assert!(started.elapsed() < WAIT, "an install still runs: {running}");
            tokio::time::sleep(POLL).await;
        }
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

#[track_caller]
fn check(args: &[&str], out: &Output, code: i32, stdout: &str, stderr: &str) {
    let context = format!(
        "marsa {args:?}\nstdout:\n{}\nstderr:\n{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(code), "{context}");
    assert_eq!(text(&out.stdout), stdout, "{context}");
    assert_eq!(text(&out.stderr), stderr, "{context}");
}

fn platform() -> String {
    Platform::current().map_or_else(|| "this system".to_owned(), |p| p.to_string())
}

/// Reads the bus until an update of install `id` that `wanted` accepts.
async fn wait_update(
    events: &mut broadcast::Receiver<Event>,
    id: u64,
    wanted: impl Fn(&InstallUpdate) -> bool,
) -> InstallUpdate {
    let started = Instant::now();
    loop {
        let left = WAIT.saturating_sub(started.elapsed());
        let event = tokio::time::timeout(left, events.recv())
            .await
            .unwrap_or_else(|_| panic!("no such update of install {id}"))
            .expect("the bus is open");
        if let Event::Install(InstallProgress { id: got, update }) = event
            && got == id
            && wanted(&update)
        {
            return update;
        }
    }
}

/// The last update of install `id`: `done` or `failed`.
async fn last_update(events: &mut broadcast::Receiver<Event>, id: u64) -> InstallUpdate {
    wait_update(events, id, |update| {
        !matches!(update, InstallUpdate::Running(_))
    })
    .await
}

const BUSY: &str =
    "marsa: Another install is running. Wait for it to end, then try again. [install_busy]\n";

#[tokio::test(flavor = "multi_thread")]
async fn install_through_the_daemon_prints_progress_and_installs() {
    let served = Served::start(FakeInstall::default()).await;
    served
        .expect(
            &["runtimes"],
            0,
            "PHP       none installed\n\
             Node.js   none installed\n\
             Composer  none installed\n",
            "",
        )
        .await;

    let dir = RuntimesDir::in_dir(served.path())
        .runtime_dir(Runtime::Php)
        .join("8.4.23");
    served
        .expect(
            &["install", "php", "8.4"],
            0,
            &format!("Installed PHP 8.4.23 in {}\n", dir.display()),
            &format!(
                "Verified the version list (serial 3)\n\
                 Installing PHP 8.4.23 for {}\n\
                 \x20 cli      [#####     ] 1.0 MB of 2.0 MB (50%)\n\
                 \x20 cli      [##########] 2.0 MB of 2.0 MB (100%)\n\
                 \x20 cli      [##########] 2.0 MB, sha256 ok\n\
                 \x20 cli      unpacking\n\
                 Checking PHP 8.4.23...\n\
                 Tip: run \"marsa install composer\" for Composer.\n",
                platform()
            ),
        )
        .await;
    assert!(dir.join("bin").is_dir());
    assert_eq!(
        served.fake.asked(),
        [(Runtime::Php, Some("8.4".to_owned()), false)]
    );

    served
        .expect(&["runtimes", "php"], 0, "PHP       8.4.23\n", "")
        .await;
    let list: RuntimeList = served
        .json("GET", "/v1/runtimes", None, StatusCode::OK)
        .await;
    assert_eq!(list.runtimes[0].installed, ["8.4.23"]);

    // Already there: the same exit code 0, and no download.
    served
        .expect(
            &["install", "php"],
            0,
            "PHP 8.4.23 is already installed. Use --force to reinstall.\n",
            &format!(
                "Verified the version list (serial 3)\n\
                 Installing PHP 8.4.23 for {}\n\
                 Tip: run \"marsa install composer\" for Composer.\n",
                platform()
            ),
        )
        .await;
    served
        .expect(
            &["install", "composer", "--force"],
            0,
            &format!(
                "Installed Composer 2.8.4 in {}\n",
                RuntimesDir::in_dir(served.path())
                    .runtime_dir(Runtime::Composer)
                    .join("2.8.4")
                    .display()
            ),
            &format!(
                "Verified the version list (serial 3)\n\
                 Installing Composer 2.8.4 for {}\n\
                 \x20 cli      [#####     ] 1.0 MB of 2.0 MB (50%)\n\
                 \x20 cli      [##########] 2.0 MB of 2.0 MB (100%)\n\
                 \x20 cli      [##########] 2.0 MB, sha256 ok\n\
                 \x20 cli      unpacking\n\
                 Checking Composer 2.8.4...\n",
                platform()
            ),
        )
        .await;
    assert_eq!(
        served.fake.asked()[1..],
        [(Runtime::Php, None, false), (Runtime::Composer, None, true)]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_install_prints_the_text_of_its_code_and_its_facts() {
    let served = Served::start(FakeInstall::default()).await;
    served
        .expect(
            &["install", "php", "8.9"],
            1,
            "",
            "Verified the version list (serial 3)\n\
             marsa: No PHP version matches \"8.9\". Available: 8.3.27, 8.4.23 \
             [install_no_such_version]\n",
        )
        .await;
    assert!(!served.daemon.installs.is_running());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_install_while_one_runs_is_409_install_busy() {
    let served = Served::start(FakeInstall::gated()).await;
    let first = served
        .command(&["install", "node"])
        .spawn()
        .expect("start marsa");
    served.wait_running(true).await;

    // Through the CLI: its text for the code, exit code 1.
    served.expect(&["install", "php", "8.4"], 1, "", BUSY).await;
    // Through the API: 409 with the code.
    let body = Some(r#"{ "version": "2", "force": false }"#);
    let code = served
        .error(
            "POST",
            "/v1/runtimes/composer/install",
            body,
            StatusCode::CONFLICT,
        )
        .await;
    assert_eq!(code, "install_busy");
    assert_eq!(served.fake.asked().len(), 1, "{:?}", served.fake.asked());

    served.fake.open_gate();
    let out = tokio::time::timeout(WAIT, first.wait_with_output())
        .await
        .expect("the first install ends")
        .expect("marsa ran");
    let dir = RuntimesDir::in_dir(served.path())
        .runtime_dir(Runtime::Node)
        .join("22.11.0");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        format!("Installed Node.js 22.11.0 in {}\n", dir.display())
    );
    served.wait_running(false).await;

    // Free again: the next one is accepted, with the next id.
    let mut events = served.daemon.events.subscribe();
    let accepted: InstallAccepted = served
        .json(
            "POST",
            "/v1/runtimes/composer/install",
            body,
            StatusCode::ACCEPTED,
        )
        .await;
    assert_eq!(accepted, InstallAccepted { id: 2 });
    match last_update(&mut events, 2).await {
        InstallUpdate::Done(done) => assert_eq!(
            (done.runtime, done.version.as_str(), done.already),
            (Runtime::Composer, "2.8.4", false)
        ),
        other => panic!("not done: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_install_ends_when_the_daemon_stops() {
    let served = Served::start(FakeInstall::gated()).await;
    let mut events = served.daemon.events.subscribe();
    let install = served
        .command(&["install", "php"])
        .spawn()
        .expect("start marsa");
    // The fake now waits at its gate, after its first step.
    wait_update(&mut events, 1, |update| {
        matches!(update, InstallUpdate::Running(_))
    })
    .await;
    // What `server::serve` does on a stop request.
    served.daemon.shutdown.cancel();
    served.daemon.publish(Event::ShuttingDown);
    served.wait_running(false).await;

    let out = tokio::time::timeout(WAIT, install.wait_with_output())
        .await
        .expect("marsa ends")
        .expect("marsa ran");
    check(
        &["install", "php"],
        &out,
        1,
        "",
        "Verified the version list (serial 3)\n\
         marsa: The Marsa daemon stopped before the install ended. Run \"marsa runtimes\" \
         to see what is installed, then try again. [install_interrupted]\n",
    );
    assert!(!served.path().join("runtimes").join("php").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_install_requests_get_their_codes() {
    let served = Served::start(FakeInstall::default()).await;
    let empty = Some("{}");
    for (method, path, body, status, code) in [
        (
            "POST",
            "/v1/runtimes/ruby/install",
            empty,
            StatusCode::NOT_FOUND,
            "unknown_tool",
        ),
        (
            "POST",
            "/v1/runtimes/PHP/install",
            empty,
            StatusCode::NOT_FOUND,
            "unknown_tool",
        ),
        (
            "POST",
            "/v1/runtimes/php/install",
            Some("not json"),
            StatusCode::BAD_REQUEST,
            "invalid_body",
        ),
        (
            "POST",
            "/v1/runtimes/php/install",
            Some(r#"{ "force": "yes" }"#),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_body",
        ),
        (
            "POST",
            "/v1/runtimes/php/install",
            None,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "invalid_body",
        ),
        (
            "GET",
            "/v1/runtimes/php/install",
            None,
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        ),
        (
            "POST",
            "/v1/runtimes",
            empty,
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        ),
    ] {
        let got = served.error(method, path, body, status).await;
        assert_eq!(got, code, "{method} {path} {body:?}");
    }
    assert!(served.fake.asked().is_empty());
    assert!(!served.daemon.installs.is_running());

    // `{}` is a whole request: the newest version, without force.
    let mut events = served.daemon.events.subscribe();
    let accepted: InstallAccepted = served
        .json(
            "POST",
            "/v1/runtimes/node/install",
            empty,
            StatusCode::ACCEPTED,
        )
        .await;
    assert!(matches!(
        last_update(&mut events, accepted.id).await,
        InstallUpdate::Done(_)
    ));
    assert_eq!(served.fake.asked(), [(Runtime::Node, None, false)]);
}
