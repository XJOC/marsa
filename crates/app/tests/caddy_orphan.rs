//! Orphan Caddy recovery (task M3-11, ADR 0009). This test binary plays every
//! part, run again with `MARSA_ORPHAN_FAKE` set:
//!
//! - `serve`: a fake Caddy. It reads the `caddy.json` the daemon wrote,
//!   listens on its admin and site addresses, answers `GET /id/<tag>` for
//!   the `@id` of the admin object (this data folder's tag) and for
//!   `marsa-srv`, as Caddy does, and exits on `POST /stop`.
//! - `stubborn`: the same, but `POST /stop` does nothing: only a signal stops it.
//! - `idle`: runs our program and holds no port.
//! - `daemon`: a small `marsad`: a `Daemon` that starts the fake Caddy, then
//!   waits until the test kills it with SIGKILL.
//!
//! The rejection tests record the signals and send none. Every fake ends by
//! itself after two minutes, so a failed test leaves nothing running for long.

// The orphan check runs on macOS only (ADR 0009): Linux waits for spike
// S-06b, and Windows for Job Objects (architecture.md section 9).
#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use marsa::daemon::caddy::caddy_spec_for;
use marsa::daemon::caddy::orphan::{BoxFuture, Processes};
use marsa::daemon::state::Daemon;
use marsa_core::caddy::{CaddyInput, SERVER_ID};
use marsa_core::orphan::{Liveness, PidRecord};
use marsa_core::platform::process::{self, Signal, read_pid_file, write_pid_file};
use marsa_core::services::ServiceSpec;
use marsa_core::types::{ServiceAction, ServiceState, ServiceStatus};
use serde_json::{Value, json};
use tempfile::TempDir;

const FAKE: &str = "MARSA_ORPHAN_FAKE";
/// Where the fake Caddy finds `caddy.json`.
const CONFIG: &str = "MARSA_ORPHAN_CONFIG";
/// For `daemon`: the data folder, the fake Caddy's mode, and the ports.
const DATA: &str = "MARSA_ORPHAN_DATA";
const CADDY_MODE: &str = "MARSA_ORPHAN_CADDY";
const PORTS: &str = "MARSA_ORPHAN_PORTS";
/// Generous: a test binary can start slowly on a busy CI machine.
const WAIT: Duration = Duration::from_secs(30);
const FAKE_LIFE: Duration = Duration::from_secs(120);

/// Not a check: the entry point of the fakes. In a normal test run the
/// variable is unset and this returns at once.
#[test]
fn fake_process() {
    let Ok(mode) = std::env::var(FAKE) else {
        return;
    };
    match mode.as_str() {
        "serve" => fake_caddy(true),
        "stubborn" => fake_caddy(false),
        "idle" => std::thread::sleep(FAKE_LIFE),
        "daemon" => fake_daemon(),
        other => panic!("unknown fake mode {other}"),
    }
}

/// Writes a line and ignores a closed pipe: after the daemon dies, nobody
/// reads the fake Caddy's output.
fn say(text: &str) {
    let _ = writeln!(io::stdout(), "{text}");
}

fn fake_caddy(stops: bool) {
    let path = std::env::var(CONFIG).expect("the config path");
    let config = std::fs::read_to_string(path).expect("caddy.json");
    let config: Value = serde_json::from_str(&config).expect("JSON");
    let admin_object = config["admin"].clone();
    let admin = admin_object["listen"].as_str().expect("admin.listen");
    let tag = admin_object["@id"].as_str().expect("admin.@id");
    let server = config["apps"]["http"]["servers"]["marsa"].clone();
    let site = server["listen"][0].as_str().expect("a site address");
    let (admin, tag, site) = (admin.to_owned(), tag.to_owned(), site.to_owned());
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    runtime.block_on(async move {
        tokio::spawn(async {
            tokio::time::sleep(FAKE_LIFE).await;
            std::process::exit(2);
        });
        let admin = tokio::net::TcpListener::bind(&admin).await.expect("admin");
        let site = tokio::net::TcpListener::bind(&site).await.expect("site");
        let api = Router::new()
            .route("/config/", get(|| async { "{}" }))
            .route(
                &format!("/id/{tag}"),
                get(move || {
                    let admin = admin_object.clone();
                    async move { Json(admin) }
                }),
            )
            .route(
                &format!("/id/{SERVER_ID}"),
                get(move || {
                    let server = server.clone();
                    async move { Json(server) }
                }),
            )
            .route(
                "/stop",
                post(move || async move {
                    say("fake stop requested");
                    if stops {
                        // Like Caddy: answer first, then exit.
                        tokio::spawn(async {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            std::process::exit(0);
                        });
                    }
                }),
            );
        let pages = Router::new().fallback(|| async { "fake site" });
        tokio::spawn(async move { axum::serve(site, pages).await });
        say("fake caddy serving");
        axum::serve(admin, api).await.expect("serve");
    });
}

fn fake_daemon() {
    let data = PathBuf::from(std::env::var(DATA).expect("the data folder"));
    let mode = std::env::var(CADDY_MODE).expect("the caddy mode");
    let ports = std::env::var(PORTS).expect("the ports");
    let ports: Vec<u16> = ports
        .split(',')
        .map(|p| p.parse().expect("a port"))
        .collect();
    let ports = Ports {
        admin: ports[0],
        http: ports[1],
        https: ports[2],
    };
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    runtime.block_on(async move {
        let daemon = daemon_on(&data, &mode, ports, Arc::new(Recorder::sending()));
        let status = start(&daemon).await;
        assert_eq!(status.state, ServiceState::Running);
        let pid = status.pid.expect("a pid");
        // The test may kill this process as soon as the line is out.
        wait_for_pid_file(&data, pid).await;
        say(&format!("fake daemon ready caddy={pid}"));
        std::future::pending::<()>().await;
    });
}

#[derive(Debug, Clone, Copy)]
struct Ports {
    admin: u16,
    http: u16,
    https: u16,
}

impl Ports {
    fn free() -> Self {
        Self {
            admin: free_port(),
            http: free_port(),
            https: free_port(),
        }
    }
}

/// A port that was free a moment ago, on `127.0.0.1`.
fn free_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a free port");
    listener.local_addr().expect("an address").port()
}

/// Records each signal. Only `sending` passes it on to the OS.
#[derive(Debug)]
struct Recorder {
    send: bool,
    signals: Mutex<Vec<(u32, Signal)>>,
}

impl Recorder {
    fn sending() -> Self {
        Self {
            send: true,
            signals: Mutex::default(),
        }
    }

    fn recording() -> Self {
        Self {
            send: false,
            signals: Mutex::default(),
        }
    }

    fn signals(&self) -> Vec<(u32, Signal)> {
        self.signals.lock().expect("the recorder lock").clone()
    }
}

impl Processes for Recorder {
    fn identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
        Box::pin(process::identity(pid))
    }

    fn executable(&self, pid: u32) -> BoxFuture<'_, Option<PathBuf>> {
        Box::pin(process::executable(pid))
    }

    fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>> {
        self.signals
            .lock()
            .expect("the recorder lock")
            .push((pid, signal));
        if self.send {
            Box::pin(process::send_signal(pid, signal))
        } else {
            Box::pin(async { Ok(()) })
        }
    }
}

/// A daemon on `data` whose Caddy is this binary in fake mode `mode`.
fn daemon_on(data: &Path, mode: &str, ports: Ports, processes: Arc<dyn Processes>) -> Daemon {
    let mut daemon = Daemon::new(data.to_path_buf());
    let caddy = &mut daemon.caddy;
    caddy.input = CaddyInput {
        admin_port: ports.admin,
        http_port: ports.http,
        https_port: ports.https,
    };
    caddy.ready_limit = WAIT;
    caddy.processes = processes;
    let config = caddy.paths.config_file.to_string_lossy().into_owned();
    caddy.spec = Some(ServiceSpec {
        // Runs only the entry point, under libtest and nextest alike.
        args: ["fake_process", "--exact", "--nocapture"]
            .map(String::from)
            .to_vec(),
        env: BTreeMap::from([
            (FAKE.to_owned(), mode.to_owned()),
            (CONFIG.to_owned(), config),
        ]),
        stop_grace: Duration::from_millis(500),
        ..caddy_spec_for(&caddy.paths, program())
    });
    daemon
}

fn program() -> PathBuf {
    std::env::current_exe().expect("the test binary has a path")
}

async fn start(daemon: &Daemon) -> ServiceStatus {
    let status = daemon.caddy.run(daemon, ServiceAction::Start).await;
    status.expect("the start runs")
}

fn pid_path(data: &Path) -> PathBuf {
    data.join("run").join("caddy.pid")
}

async fn wait_for_pid_file(data: &Path, pid: u32) -> PidRecord {
    let started = Instant::now();
    loop {
        if let Some(record) = read_pid_file(&pid_path(data))
            && record.pid == pid
        {
            return record;
        }
        assert!(started.elapsed() < WAIT, "no caddy.pid for {pid}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Whether `record` still names a live process with its path and start time.
async fn alive(record: &PidRecord) -> bool {
    let now = process::identity(record.pid).await;
    now == Liveness::Alive {
        exe: record.exe.clone(),
        started: record.started.clone(),
    }
}

/// The lines the daemon added to Caddy's log.
fn notes(daemon: &Daemon) -> Vec<String> {
    let lines = daemon.caddy.recent_logs().into_iter().map(|line| line.text);
    lines.filter(|text| text.starts_with("marsad: ")).collect()
}

fn port_in_use() -> ServiceState {
    failed("port_in_use")
}

fn failed(code: &str) -> ServiceState {
    ServiceState::Failed { code: code.into() }
}

/// Kills an orphan this test made, once it is checked to still be that one.
async fn kill_orphan(record: &PidRecord) {
    if alive(record).await {
        let _ = process::send_signal(record.pid, Signal::Kill).await;
    }
}

/// Serves another Marsa's Caddy on `port`: it answers to the shared
/// `marsa-srv` tag only. Gives the count of `POST /stop` requests.
async fn other_marsa(port: u16) -> Arc<AtomicUsize> {
    let stops = Arc::new(AtomicUsize::new(0));
    let counted = stops.clone();
    let app = Router::new()
        .route(
            &format!("/id/{SERVER_ID}"),
            get(|| async { Json(json!({ "@id": SERVER_ID, "listen": [":1"] })) }),
        )
        .route(
            "/stop",
            post(move || async move {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .fallback(|| async {
            let error = json!({ "error": "unknown object ID" });
            (StatusCode::NOT_FOUND, Json(error))
        });
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .expect("the admin port");
    tokio::spawn(async move { axum::serve(listener, app).await });
    stops
}

/// A process this test started. Dropping it kills it: its pid cannot be
/// reused before the `wait`.
struct Bystander(Child);

impl Bystander {
    fn spawn(command: &mut Command) -> Self {
        Self(command.spawn().expect("a bystander"))
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn alive(&mut self) -> bool {
        self.0.try_wait().expect("try_wait").is_none()
    }
}

impl Drop for Bystander {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs the small daemon, which starts a fake Caddy in `mode`, kills the
/// daemon with SIGKILL, and gives the pid file of the Caddy it left behind.
async fn orphan_of_a_killed_daemon(data: &Path, mode: &str, ports: Ports) -> PidRecord {
    let ports_text = format!("{},{},{}", ports.admin, ports.http, ports.https);
    let mut daemon = Command::new(program())
        .args(["fake_process", "--exact", "--nocapture"])
        .env(FAKE, "daemon")
        .env(DATA, data)
        .env(CADDY_MODE, mode)
        .env(PORTS, ports_text)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the small daemon");
    let stdout = daemon.stdout.take().expect("its stdout");
    let ready = tokio::task::spawn_blocking(move || {
        for line in BufReader::new(stdout).lines() {
            let line = line.expect("a line");
            if let Some(pid) = line.strip_prefix("fake daemon ready caddy=") {
                return pid.parse::<u32>().expect("a pid");
            }
        }
        panic!("the small daemon ended before it was ready");
    });
    let pid = tokio::time::timeout(WAIT, ready)
        .await
        .expect("the small daemon is ready in time")
        .expect("the reader");
    // The crash: SIGKILL, so no clean-up code runs.
    daemon.kill().expect("SIGKILL");
    daemon.wait().expect("wait");
    let record = read_pid_file(&pid_path(data)).expect("caddy.pid");
    assert_eq!(record.pid, pid);
    record
}

#[tokio::test]
async fn an_orphan_of_a_killed_daemon_stops_on_post_stop_and_is_replaced() {
    let dir = TempDir::new().unwrap();
    let ports = Ports::free();
    let orphan = orphan_of_a_killed_daemon(dir.path(), "serve", ports).await;
    // It outlived its daemon, and still answers with our tag.
    assert!(alive(&orphan).await, "{orphan:?}");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let tag = format!("http://127.0.0.1:{}/id/{SERVER_ID}", ports.admin);
    assert!(client.get(&tag).send().await.unwrap().status().is_success());

    let recorder = Arc::new(Recorder::sending());
    let daemon = daemon_on(dir.path(), "serve", ports, recorder.clone());
    let status = start(&daemon).await;
    assert_eq!(
        status.state,
        ServiceState::Running,
        "{:?}",
        daemon.caddy.recent_logs()
    );
    let pid = status.pid.unwrap();
    assert_ne!(pid, orphan.pid);
    assert!(!alive(&orphan).await, "the orphan is gone");
    assert!(recorder.signals().is_empty(), "POST /stop was enough");
    wait_for_pid_file(dir.path(), pid).await;
    let notes = notes(&daemon);
    let stopping = "marsad: Stopping an old Caddy left by a crashed marsad.";
    assert!(notes.iter().any(|n| n == stopping), "{notes:?}");

    // A clean stop removes the file.
    daemon.stop_procs(WAIT).await;
    assert_eq!(read_pid_file(&pid_path(dir.path())), None);
}

#[tokio::test]
async fn an_orphan_that_ignores_post_stop_gets_sigterm_after_its_check() {
    let dir = TempDir::new().unwrap();
    let ports = Ports::free();
    let orphan = orphan_of_a_killed_daemon(dir.path(), "stubborn", ports).await;
    assert!(alive(&orphan).await, "{orphan:?}");

    let recorder = Arc::new(Recorder::sending());
    let daemon = daemon_on(dir.path(), "serve", ports, recorder.clone());
    let started = Instant::now();
    let status = start(&daemon).await;
    assert_eq!(
        status.state,
        ServiceState::Running,
        "{:?}",
        daemon.caddy.recent_logs()
    );
    // `POST /stop` had its 2 s first.
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert_eq!(recorder.signals(), [(orphan.pid, Signal::Term)]);
    assert!(!alive(&orphan).await, "the orphan is gone");
    daemon.stop_procs(WAIT).await;
}

#[tokio::test]
async fn a_pid_file_naming_another_program_is_never_signalled() {
    // R4: the pid now runs `sleep`. The file has its start time, so only the
    // path tells it apart.
    let dir = TempDir::new().unwrap();
    let mut sleeper = Bystander::spawn(Command::new("/bin/sleep").arg("120"));
    let Liveness::Alive { started, .. } = process::identity(sleeper.pid()).await else {
        panic!("no identity for the sleeper");
    };
    let record = PidRecord {
        pid: sleeper.pid(),
        exe: program(),
        started,
    };
    write_pid_file(&pid_path(dir.path()), &record).unwrap();

    let recorder = Arc::new(Recorder::recording());
    let daemon = daemon_on(dir.path(), "serve", Ports::free(), recorder.clone());
    let status = start(&daemon).await;
    assert_eq!(status.state, ServiceState::Running);
    assert!(recorder.signals().is_empty());
    assert!(sleeper.alive());
    // The new Caddy's pid replaced the stale one.
    wait_for_pid_file(dir.path(), status.pid.unwrap()).await;
    daemon.stop_procs(WAIT).await;
}

#[tokio::test]
async fn a_pid_file_with_another_start_time_is_never_signalled() {
    // R5e: the pid runs our very program, but not the one we started. A
    // program holds our site port, so a reap would end in signals.
    let dir = TempDir::new().unwrap();
    let ports = Ports::free();
    let mut idle = Bystander::spawn(Command::new(program()).env(FAKE, "idle").args([
        "fake_process",
        "--exact",
        "--nocapture",
    ]));
    let Liveness::Alive { exe, started } = process::identity(idle.pid()).await else {
        panic!("no identity for the idle process");
    };
    let accepted = [program(), std::fs::canonicalize(program()).unwrap()];
    assert!(accepted.contains(&exe), "{exe:?}: only the time may differ");
    assert_ne!(started, "Thu Jan 1 00:00:00 1970");
    let record = PidRecord {
        pid: idle.pid(),
        exe,
        started: "Thu Jan 1 00:00:00 1970".into(),
    };
    write_pid_file(&pid_path(dir.path()), &record).unwrap();
    let _held = TcpListener::bind((Ipv4Addr::LOCALHOST, ports.http)).unwrap();

    let recorder = Arc::new(Recorder::recording());
    let daemon = daemon_on(dir.path(), "serve", ports, recorder.clone());
    let status = start(&daemon).await;
    assert_eq!(status.state, port_in_use());
    assert!(recorder.signals().is_empty());
    assert!(idle.alive());
    assert!(!daemon.proc_alive("caddy"));
    // Nothing touched: the file stays as it was.
    assert_eq!(read_pid_file(&pid_path(dir.path())), Some(record));
    let notes = notes(&daemon);
    assert!(
        notes.iter().any(|n| n.contains("site port is held by")),
        "{notes:?}"
    );
}

#[tokio::test]
async fn a_process_that_only_claims_our_path_is_never_signalled() {
    // `argv[0]` is chosen by whoever starts a process: this `sleep` shows our
    // Caddy's path in `ps`, with its real start time, so the pid file
    // matches. Only `lsof`, which reads the real executable, tells it apart.
    // Nothing answers on the admin port, and a program holds the site port,
    // so the ladder goes straight to the check before SIGTERM.
    let dir = TempDir::new().unwrap();
    let ports = Ports::free();
    let mut sleeper = Bystander::spawn(Command::new("/bin/sleep").arg0(program()).arg("120"));
    let Liveness::Alive { exe, started } = process::identity(sleeper.pid()).await else {
        panic!("no identity for the sleeper");
    };
    assert_eq!(exe, program(), "ps shows the claimed path");
    let record = PidRecord {
        pid: sleeper.pid(),
        exe,
        started,
    };
    write_pid_file(&pid_path(dir.path()), &record).unwrap();
    let _held = TcpListener::bind((Ipv4Addr::LOCALHOST, ports.http)).unwrap();

    let recorder = Arc::new(Recorder::recording());
    let daemon = daemon_on(dir.path(), "serve", ports, recorder.clone());
    let status = start(&daemon).await;
    assert!(
        matches!(status.state, ServiceState::Failed { .. }),
        "{:?}",
        status.state
    );
    assert_eq!(status.state, port_in_use());
    assert!(recorder.signals().is_empty());
    assert!(sleeper.alive());
    assert!(!daemon.proc_alive("caddy"));
}

#[tokio::test]
async fn a_dead_pid_is_never_signalled() {
    // R3: the file names a process that has exited.
    let dir = TempDir::new().unwrap();
    let mut gone = Command::new("/bin/sleep").arg("0").spawn().unwrap();
    gone.wait().unwrap();
    let record = PidRecord {
        pid: gone.id(),
        exe: program(),
        started: "Wed Nov 4 03:31:27 2026".into(),
    };
    write_pid_file(&pid_path(dir.path()), &record).unwrap();

    let recorder = Arc::new(Recorder::recording());
    let daemon = daemon_on(dir.path(), "serve", Ports::free(), recorder.clone());
    let status = start(&daemon).await;
    assert_eq!(status.state, ServiceState::Running);
    assert!(recorder.signals().is_empty());
    wait_for_pid_file(dir.path(), status.pid.unwrap()).await;
    daemon.stop_procs(WAIT).await;
}

#[tokio::test]
async fn a_foreign_caddy_on_our_admin_port_is_left_alone() {
    // R5 and R5b: a Caddy without our tag answers on our admin port, as Caddy
    // does for an unknown id. The pid file names this very process, with its
    // real path and start time, so only the tag says "not ours".
    let dir = TempDir::new().unwrap();
    let ports = Ports::free();
    let stops = Arc::new(AtomicUsize::new(0));
    let counted = stops.clone();
    let foreign = Router::new()
        .route(
            &format!("/id/{SERVER_ID}"),
            get(|| async {
                let error = json!({ "error": format!("unknown object ID '{SERVER_ID}'") });
                (StatusCode::NOT_FOUND, Json(error))
            }),
        )
        .route(
            "/stop",
            post(move || async move {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        );
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, ports.admin))
        .await
        .unwrap();
    tokio::spawn(async move { axum::serve(listener, foreign).await });
    let me = std::process::id();
    let Liveness::Alive { exe, started } = process::identity(me).await else {
        panic!("no identity for this test");
    };
    let record = PidRecord {
        pid: me,
        exe,
        started,
    };
    write_pid_file(&pid_path(dir.path()), &record).unwrap();

    let recorder = Arc::new(Recorder::recording());
    let daemon = daemon_on(dir.path(), "serve", ports, recorder.clone());
    let status = start(&daemon).await;
    assert_eq!(status.state, port_in_use());
    assert!(recorder.signals().is_empty());
    assert_eq!(stops.load(Ordering::SeqCst), 0, "no POST /stop to it");
    assert!(!daemon.proc_alive("caddy"));
    assert_eq!(read_pid_file(&pid_path(dir.path())), Some(record));
    let notes = notes(&daemon);
    assert!(notes.iter().any(|n| n.contains("not ours")), "{notes:?}");
}

#[tokio::test]
async fn a_port_held_by_another_program_is_a_conflict() {
    // R6: no pid file, nothing on the admin port, a program on the site port.
    let dir = TempDir::new().unwrap();
    let ports = Ports::free();
    let _held = TcpListener::bind((Ipv4Addr::LOCALHOST, ports.http)).unwrap();

    let recorder = Arc::new(Recorder::recording());
    let daemon = daemon_on(dir.path(), "serve", ports, recorder.clone());
    let status = start(&daemon).await;
    assert_eq!(status.state, port_in_use());
    assert!(recorder.signals().is_empty());
    assert!(!daemon.proc_alive("caddy"));
    assert_eq!(read_pid_file(&pid_path(dir.path())), None);
    let notes = notes(&daemon);
    let note = notes.iter().find(|n| n.contains("site port is held by"));
    let note = note.unwrap_or_else(|| panic!("no note in {notes:?}"));
    // `lsof` names the owner, once.
    let me = format!("({})", std::process::id());
    assert_eq!(note.matches(&me).count(), 1, "{note}");
}

#[tokio::test]
async fn another_marsas_caddy_gets_no_stop_and_no_signal() {
    // Another data folder's Caddy on the shared admin port: it answers to
    // `marsa-srv`, not to our instance tag. Even a pid file that matches this
    // very process does not make it ours.
    let dir = TempDir::new().unwrap();
    let ports = Ports::free();
    let stops = other_marsa(ports.admin).await;
    let me = std::process::id();
    let Liveness::Alive { exe, started } = process::identity(me).await else {
        panic!("no identity for this test");
    };
    let record = PidRecord {
        pid: me,
        exe,
        started,
    };
    write_pid_file(&pid_path(dir.path()), &record).unwrap();

    let recorder = Arc::new(Recorder::recording());
    let daemon = daemon_on(dir.path(), "serve", ports, recorder.clone());
    let status = start(&daemon).await;
    assert_eq!(status.state, failed("caddy_other_marsa"));
    assert_eq!(stops.load(Ordering::SeqCst), 0, "no POST /stop to it");
    assert!(recorder.signals().is_empty());
    assert!(!daemon.proc_alive("caddy"));
    assert_eq!(read_pid_file(&pid_path(dir.path())), Some(record));
    let notes = notes(&daemon);
    assert!(
        notes.iter().any(|n| n.contains("another Marsa")),
        "{notes:?}"
    );
    // The pid file matches, so the note names that process: it may be the
    // user's own old Caddy.
    let named = format!(
        "our pid file names pid {me}, started as {}",
        program().display()
    );
    assert!(notes.iter().any(|n| n.contains(&named)), "{notes:?}");

    // A stop leaves it alone too.
    let status = daemon.caddy.run(&daemon, ServiceAction::Stop).await;
    assert_eq!(status.unwrap().state, failed("caddy_other_marsa"));
    assert_eq!(stops.load(Ordering::SeqCst), 0);
    assert!(recorder.signals().is_empty());
}

#[tokio::test]
async fn our_stubborn_orphan_with_a_pid_file_naming_sleep_gets_no_signal() {
    // Our tag answers, but the pid file names an innocent `sleep` (its start
    // time included): `/stop` is all the orphan gets.
    let dir = TempDir::new().unwrap();
    let ports = Ports::free();
    let orphan = orphan_of_a_killed_daemon(dir.path(), "stubborn", ports).await;
    let mut sleeper = Bystander::spawn(Command::new("/bin/sleep").arg("120"));
    let Liveness::Alive { started, .. } = process::identity(sleeper.pid()).await else {
        panic!("no identity for the sleeper");
    };
    let record = PidRecord {
        pid: sleeper.pid(),
        exe: program(),
        started,
    };
    write_pid_file(&pid_path(dir.path()), &record).unwrap();

    let recorder = Arc::new(Recorder::recording());
    let daemon = daemon_on(dir.path(), "serve", ports, recorder.clone());
    let status = start(&daemon).await;
    assert_eq!(status.state, failed("caddy_orphan_stuck"));
    assert!(recorder.signals().is_empty());
    assert!(sleeper.alive());
    assert!(alive(&orphan).await, "only /stop, which it ignores");
    kill_orphan(&orphan).await;
}

#[tokio::test]
async fn r2_our_stubborn_orphan_without_a_pid_file_gets_no_signal() {
    let dir = TempDir::new().unwrap();
    let ports = Ports::free();
    let orphan = orphan_of_a_killed_daemon(dir.path(), "stubborn", ports).await;
    process::remove_pid_file(&pid_path(dir.path())).unwrap();

    let recorder = Arc::new(Recorder::recording());
    let daemon = daemon_on(dir.path(), "serve", ports, recorder.clone());
    let status = start(&daemon).await;
    assert_eq!(status.state, failed("caddy_orphan_stuck"));
    assert!(recorder.signals().is_empty());
    assert!(alive(&orphan).await);
    let notes = notes(&daemon);
    assert!(notes.iter().any(|n| n.contains("no pid file")), "{notes:?}");
    kill_orphan(&orphan).await;
}

#[tokio::test]
async fn stop_with_no_caddy_running_stops_our_own_orphan() {
    let dir = TempDir::new().unwrap();
    let ports = Ports::free();
    let orphan = orphan_of_a_killed_daemon(dir.path(), "serve", ports).await;

    let recorder = Arc::new(Recorder::sending());
    let daemon = daemon_on(dir.path(), "serve", ports, recorder.clone());
    let status = daemon.caddy.run(&daemon, ServiceAction::Stop).await;
    assert_eq!(status.unwrap().state, ServiceState::Stopped);
    assert!(!alive(&orphan).await, "the orphan is gone");
    assert!(recorder.signals().is_empty(), "POST /stop was enough");
    assert_eq!(read_pid_file(&pid_path(dir.path())), None);
    assert!(!daemon.proc_alive("caddy"), "a stop starts nothing");
}

#[tokio::test]
async fn stop_with_no_caddy_running_touches_nothing_else() {
    // A stale pid file and a port held by another program: a start would
    // remove the file or report the port; a stop does neither.
    let dir = TempDir::new().unwrap();
    let ports = Ports::free();
    let mut gone = Command::new("/bin/sleep").arg("0").spawn().unwrap();
    gone.wait().unwrap();
    let record = PidRecord {
        pid: gone.id(),
        exe: program(),
        started: "Wed Nov 4 03:31:27 2026".into(),
    };
    write_pid_file(&pid_path(dir.path()), &record).unwrap();
    let _held = TcpListener::bind((Ipv4Addr::LOCALHOST, ports.http)).unwrap();

    let recorder = Arc::new(Recorder::recording());
    let daemon = daemon_on(dir.path(), "serve", ports, recorder.clone());
    let status = daemon.caddy.run(&daemon, ServiceAction::Stop).await;
    assert_eq!(status.unwrap().state, ServiceState::Stopped);
    assert!(recorder.signals().is_empty());
    assert_eq!(read_pid_file(&pid_path(dir.path())), Some(record));
    assert!(notes(&daemon).is_empty());
}
