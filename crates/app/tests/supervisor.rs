//! The supervisor against a fake service. The fake is this test binary: it
//! runs itself again with `MARSA_FAKE_SERVICE` set, so no external program is
//! needed and the tests are the same on macOS, Linux, and Windows, except the
//! signal tests.
//!
//! `#[cfg(unix)]` on those tests and their helpers: they send SIGINT and
//! SIGQUIT and record the signals the fake gets, and only Unix has signals.
//! Windows stops a service with a kill, which the other tests cover. The
//! tests of a stop request over TCP (Valkey's `SHUTDOWN SAVE`) are Unix
//! only too: the step after the request is SIGTERM. The tests of a stop
//! program (Windows' way to stop MySQL and PostgreSQL) run everywhere: the
//! program is this test binary too.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use marsa::daemon::supervisor::{ServiceHandle, Supervisor};
use marsa_core::services::{RestartPolicy, ServiceSpec, StopMethod};
use marsa_core::types::{Event, LogLine, LogStream, ServiceState};
use tempfile::TempDir;
use tokio::sync::broadcast;

const FAKE: &str = "MARSA_FAKE_SERVICE";
/// The folder the `watch` fake watches for the file `stop`.
const FAKE_DIR: &str = "MARSA_FAKE_DIR";
/// Arguments of the fake stop program ([`fake_stop_program`]). The test
/// harness reads each as a filter that matches no test.
const STOP_ARG: &str = "marsa-fake-stop=";
const REFUSE_ARG: &str = "marsa-fake-refuse";
const HANG_ARG: &str = "marsa-fake-hang=";
/// The port of the fake TCP server.
#[cfg(unix)]
const FAKE_PORT: &str = "MARSA_FAKE_PORT";
/// Generous: a test binary can start slowly on a busy CI machine.
const WAIT: Duration = Duration::from_secs(30);
const GRACE: Duration = Duration::from_millis(500);

/// Not a check: the fake service's entry point. In a normal test run the
/// variable is unset and this returns at once. The harness prints lines of its
/// own, so the fake starts its lines with `fake `.
#[test]
fn fake_service() {
    let Ok(mode) = std::env::var(FAKE) else {
        return;
    };
    match mode.as_str() {
        "print" => {
            for n in 1..=5 {
                println!("fake {n}");
            }
            // A Windows line ending, which the supervisor strips.
            eprint!("fake err\r\n");
            std::process::exit(0);
        }
        "fail" => {
            println!("fake failing");
            std::process::exit(3);
        }
        // Never exits, and ignores SIGTERM: a tokio handler replaces the
        // default action. Only a kill stops it.
        "hang" => tokio::runtime::Runtime::new().unwrap().block_on(async {
            // Only Unix has SIGTERM. On Windows the supervisor kills at once.
            #[cfg(unix)]
            let _term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("a SIGTERM handler");
            println!("fake ready");
            std::future::pending::<()>().await;
        }),
        // Print each stop signal they get. `pg` stops on SIGINT and ignores
        // SIGTERM, like PostgreSQL with a client connected (ADR 0023).
        // `deaf` ignores them all: only a kill stops it.
        #[cfg(unix)]
        "pg" | "deaf" => tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(print_signals(mode == "pg")),
        // A server on the port in `MARSA_FAKE_PORT` that prints the bytes of
        // the first request it gets. `kv` then exits, as Valkey does at
        // `SHUTDOWN SAVE`; `kv-mute` never answers or closes; `kv-refuse`
        // answers with Valkey's error. The last two exit at SIGTERM.
        #[cfg(unix)]
        "kv" | "kv-mute" | "kv-refuse" => tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(serve_one_request(&mode)),
        // Exits when the file `stop` appears in `MARSA_FAKE_DIR`, as a server
        // that its stop program asked to stop. Nothing else ends it but a
        // kill, on every system; after two minutes it ends by itself, so a
        // failed test leaves nothing behind for long.
        "watch" => {
            let dir = PathBuf::from(std::env::var(FAKE_DIR).expect("the test sets the folder"));
            println!("fake ready");
            let deadline = Instant::now() + Duration::from_secs(120);
            while Instant::now() < deadline {
                if dir.join("stop").exists() {
                    println!("fake got the stop file");
                    std::process::exit(0);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        other => panic!("unknown fake mode {other}"),
    }
}

/// Not a check: the fake stop program's entry point. In a normal test run it
/// has none of its arguments and returns at once.
///
/// - `marsa-fake-stop=<folder>`: says so, then writes `<folder>/stop`, which
///   ends the `watch` fake, and exits 0, as `pg_ctl stop` does.
/// - `marsa-fake-refuse`: says why it cannot, and exits 1, as `mysqladmin`
///   does when `root` has a password.
/// - `marsa-fake-hang=<folder>`: writes its pid to `<folder>/hang.pid`, and
///   never ends by itself (two minutes at most).
#[test]
fn fake_stop_program() {
    let args: Vec<String> = std::env::args().collect();
    let value = |prefix: &str| args.iter().find_map(|arg| arg.strip_prefix(prefix));
    if let Some(dir) = value(STOP_ARG) {
        println!("fake stop asked");
        std::fs::write(Path::new(dir).join("stop"), "").expect("the stop file");
    } else if args.iter().any(|arg| arg == REFUSE_ARG) {
        eprintln!("fake stop: Access denied for user 'root'@'127.0.0.1'");
        std::process::exit(1);
    } else if let Some(dir) = value(HANG_ARG) {
        let pid = std::process::id().to_string();
        std::fs::write(Path::new(dir).join("hang.pid"), pid).expect("the pid file");
        std::thread::sleep(Duration::from_secs(120));
    }
}

#[cfg(unix)]
async fn serve_one_request(mode: &str) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::signal::unix::{SignalKind, signal};
    let port = std::env::var(FAKE_PORT).expect("the test sets the port");
    let port: u16 = port.parse().expect("a port number");
    let mut term = signal(SignalKind::terminate()).expect("a signal handler");
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("the port is free");
    println!("fake ready");
    let (mut stream, _) = listener.accept().await.expect("a client");
    // Everything the client sends until it pauses: one request.
    let mut got = Vec::new();
    let mut chunk = [0_u8; 64];
    let pause = Duration::from_millis(300);
    while let Ok(Ok(read @ 1..)) = tokio::time::timeout(pause, stream.read(&mut chunk)).await {
        got.extend_from_slice(&chunk[..read]);
    }
    println!("fake got {:?}", String::from_utf8_lossy(&got));
    match mode {
        "kv" => std::process::exit(0),
        "kv-refuse" => {
            let refused = b"-ERR Errors trying to SHUTDOWN. Check logs.\r\n";
            stream.write_all(refused).await.expect("the reply");
        }
        _ => {}
    }
    term.recv().await;
    println!("fake got SIGTERM");
    std::process::exit(0);
}

#[cfg(unix)]
async fn print_signals(stop_on_sigint: bool) {
    use tokio::signal::unix::{SignalKind, signal};
    let handler = |kind| signal(kind).expect("a signal handler");
    let mut term = handler(SignalKind::terminate());
    let mut int = handler(SignalKind::interrupt());
    let mut quit = handler(SignalKind::quit());
    println!("fake ready");
    loop {
        // `biased`: a SIGTERM that came with another signal prints first.
        tokio::select! {
            biased;
            _ = term.recv() => println!("fake got SIGTERM"),
            _ = int.recv() => {
                println!("fake got SIGINT");
                if stop_on_sigint {
                    std::process::exit(0);
                }
            }
            _ = quit.recv() => println!("fake got SIGQUIT"),
        }
    }
}

type Started = (ServiceHandle, broadcast::Receiver<Event>, TempDir);

/// Starts the fake in `mode`, stopped like Caddy. Returns the handle, the
/// event bus, and the logs folder.
fn start(mode: &str, restart: RestartPolicy) -> Started {
    start_with(mode, restart, StopMethod::term(GRACE))
}

fn start_with(mode: &str, restart: RestartPolicy, stop: StopMethod) -> Started {
    start_spec(spec(mode, restart, stop))
}

/// The fake in `mode`, run by the supervisor.
fn spec(mode: &str, restart: RestartPolicy, stop: StopMethod) -> ServiceSpec {
    ServiceSpec {
        id: mode.into(),
        program: std::env::current_exe().expect("the test binary has a path"),
        // Runs only the entry point, under libtest and under nextest alike.
        args: ["fake_service", "--exact", "--nocapture"]
            .map(String::from)
            .to_vec(),
        env: BTreeMap::from([(FAKE.to_owned(), mode.to_owned())]),
        cwd: None,
        restart,
        stop,
    }
}

fn start_spec(spec: ServiceSpec) -> Started {
    let (tx, rx) = broadcast::channel(1024);
    let logs = tempfile::tempdir().expect("a temp folder");
    let handle = Supervisor::spawn(spec, tx, logs.path()).expect("the program is absolute");
    (handle, rx, logs)
}

/// Waits for the task to end, then takes the events already on the bus.
async fn finish(handle: &mut ServiceHandle, rx: &mut broadcast::Receiver<Event>) -> Vec<Event> {
    let task = tokio::time::timeout(WAIT, &mut handle.task).await;
    task.expect("the task ended in time").expect("no panic");
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

fn fake_lines(events: &[Event]) -> Vec<LogLine> {
    let fake = |event: &Event| match event {
        Event::Log(line) if line.text.starts_with("fake ") => Some(line.clone()),
        _ => None,
    };
    events.iter().filter_map(fake).collect()
}

fn states(events: &[Event]) -> Vec<ServiceState> {
    let state = |event: &Event| match event {
        Event::ServiceChanged(status) => Some(status.state.clone()),
        _ => None,
    };
    events.iter().filter_map(state).collect()
}

fn exited() -> ServiceState {
    ServiceState::Failed {
        code: "exited".into(),
    }
}

#[tokio::test]
async fn output_lines_arrive_in_order_as_events() {
    let (mut handle, mut rx, logs) = start("print", RestartPolicy::Never);
    let events = finish(&mut handle, &mut rx).await;

    let lines = fake_lines(&events);
    let text = |stream| {
        let of_stream = lines.iter().filter(move |line| line.stream == stream);
        of_stream.map(|line| line.text.as_str()).collect::<Vec<_>>()
    };
    assert_eq!(
        text(LogStream::Stdout),
        ["fake 1", "fake 2", "fake 3", "fake 4", "fake 5"]
    );
    assert_eq!(text(LogStream::Stderr), ["fake err"]);
    assert!(lines.windows(2).all(|w| w[0].seq < w[1].seq));
    // `Never`: the service stays `Failed`, after its last lines.
    let want = [ServiceState::Starting, ServiceState::Running, exited()];
    assert_eq!(states(&events), want);
    assert!(matches!(events.last(), Some(Event::ServiceChanged(_))));
    assert!(handle.recent_logs().iter().any(|l| l.text == "fake 5"));
    let file = std::fs::read_to_string(logs.path().join("print.log")).unwrap();
    assert!(
        file.contains("fake 5\n") && file.contains("fake err\n"),
        "{file:?}"
    );
}

#[tokio::test]
async fn a_failing_process_restarts_until_the_limit() {
    let policy = RestartPolicy::OnFailure {
        max_retries: 2,
        initial: Duration::from_millis(50),
        max: Duration::from_millis(100),
    };
    let (mut handle, mut rx, _logs) = start("fail", policy);
    let events = finish(&mut handle, &mut rx).await;

    let changes = states(&events);
    let runs = changes.iter().filter(|s| **s == ServiceState::Running);
    assert_eq!(runs.count(), 3, "one start and two restarts: {changes:?}");
    assert_eq!(changes.last(), Some(&exited()));
    assert_eq!(fake_lines(&events).len(), 3);
    let status = handle.status();
    assert_eq!(
        (status.state, status.restarts, status.pid),
        (exited(), 2, None)
    );
}

#[tokio::test]
async fn cancel_stops_a_process_that_ignores_sigterm() {
    let (mut handle, mut rx, _logs) = start("hang", RestartPolicy::Never);
    // After this line the fake ignores SIGTERM.
    let ready = async {
        while !handle.recent_logs().iter().any(|l| l.text == "fake ready") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(WAIT, ready)
        .await
        .expect("the fake started");

    let started = Instant::now();
    handle.cancel.cancel();
    let events = finish(&mut handle, &mut rx).await;
    let elapsed = started.elapsed();
    assert!(elapsed < GRACE + Duration::from_secs(1), "{elapsed:?}");
    // On Unix the kill comes only after the grace, because SIGTERM was
    // ignored. Windows has no SIGTERM and kills at once.
    if cfg!(unix) {
        assert!(elapsed >= GRACE, "killed too early: {elapsed:?}");
    }
    let changes = states(&events);
    assert_eq!(
        changes[2..],
        [ServiceState::Stopping, ServiceState::Stopped]
    );
    assert_eq!(handle.status().pid, None);
}

#[test]
fn a_relative_program_is_rejected() {
    let (tx, _rx) = broadcast::channel(8);
    let spec = ServiceSpec {
        id: "caddy".into(),
        program: "caddy".into(),
        args: Vec::new(),
        env: BTreeMap::new(),
        cwd: None,
        restart: RestartPolicy::Never,
        stop: StopMethod::term(GRACE),
    };
    let err = Supervisor::spawn(spec, tx, std::path::Path::new("logs")).unwrap_err();
    assert_eq!(err.code(), "program_not_absolute");
}

/// The texts of the fake's lines, in order.
#[cfg(unix)]
fn fake_texts(events: &[Event]) -> Vec<String> {
    fake_lines(events)
        .into_iter()
        .map(|line| line.text)
        .collect()
}

/// Waits until the fake says it is ready: for the signal fakes, once they
/// have their handlers.
async fn ready(handle: &ServiceHandle) {
    let ready = async {
        while !handle.recent_logs().iter().any(|l| l.text == "fake ready") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(WAIT, ready)
        .await
        .expect("the fake started");
}

/// PostgreSQL's ladder (ADR 0023): SIGINT, then SIGQUIT, then a kill.
#[cfg(unix)]
fn postgres_ladder(wait: Duration) -> StopMethod {
    use marsa_core::services::Signal;
    StopMethod::signals([(Signal::Int, wait), (Signal::Quit, wait)])
}

/// Stops the fake in `mode` with `stop`, and gives its events and how long
/// the stop took.
#[cfg(unix)]
async fn stop_fake(mode: &str, stop: StopMethod) -> (Vec<Event>, Duration) {
    let (mut handle, mut rx, _logs) = start_with(mode, RestartPolicy::Never, stop);
    ready(&handle).await;
    let started = Instant::now();
    handle.cancel.cancel();
    let events = finish(&mut handle, &mut rx).await;
    let elapsed = started.elapsed();
    let changes = states(&events);
    assert_eq!(
        changes[2..],
        [ServiceState::Stopping, ServiceState::Stopped]
    );
    assert_eq!(handle.status().pid, None);
    (events, elapsed)
}

#[cfg(unix)]
#[tokio::test]
async fn the_postgres_ladder_stops_on_sigint_and_never_sends_sigterm() {
    // Long, so a stop that needed a second step would fail the time check.
    let wait = Duration::from_secs(10);
    let (events, elapsed) = stop_fake("pg", postgres_ladder(wait)).await;
    assert!(elapsed < wait, "SIGINT did not stop it: {elapsed:?}");
    // The fake prints every stop signal it gets, SIGTERM too: SIGINT alone
    // came.
    assert_eq!(fake_texts(&events), ["fake ready", "fake got SIGINT"]);
}

#[cfg(unix)]
#[tokio::test]
async fn the_postgres_ladder_ends_in_a_kill_after_each_wait() {
    let (events, elapsed) = stop_fake("deaf", postgres_ladder(GRACE)).await;
    // The fake ignores SIGINT and SIGQUIT: only the kill ends it, after both
    // waits. The margin covers two runs of `/bin/kill`.
    assert!(elapsed >= 2 * GRACE, "killed too early: {elapsed:?}");
    assert!(elapsed < 2 * GRACE + Duration::from_secs(2), "{elapsed:?}");
    let want = ["fake ready", "fake got SIGINT", "fake got SIGQUIT"];
    assert_eq!(fake_texts(&events), want);
}

/// The daemon's shutdown gives each process the time its own stop method
/// needs, even past the limit it was given: `marsad` gives its processes
/// 10 s, and PostgreSQL's ladder takes 20 s. Before task M6-03 the shutdown
/// aborted the task at its limit, so SIGQUIT never came.
#[cfg(unix)]
#[tokio::test]
async fn a_daemon_shutdown_waits_for_the_whole_ladder() {
    use marsa::daemon::state::Daemon;
    use marsa::daemon::supervisor::STOP_MARGIN;

    let ladder = postgres_ladder(GRACE);
    let (handle, mut rx, _logs) = start_with("deaf", RestartPolicy::Never, ladder);
    assert_eq!(handle.stop_limit, 2 * GRACE + STOP_MARGIN);
    ready(&handle).await;
    let data = tempfile::tempdir().expect("a temp folder");
    let daemon = Daemon::new(data.path().to_path_buf());
    daemon.add_proc("deaf", handle);

    // Shorter than the ladder, as `marsad`'s 10 s are shorter than 20 s.
    let started = Instant::now();
    daemon.stop_procs(GRACE / 5).await;
    assert!(started.elapsed() >= 2 * GRACE, "{:?}", started.elapsed());

    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let want = ["fake ready", "fake got SIGINT", "fake got SIGQUIT"];
    assert_eq!(fake_texts(&events), want);
    // The supervisor ended the stop itself: an aborted task says nothing.
    let changes = states(&events);
    assert_eq!(changes.last(), Some(&ServiceState::Stopped), "{changes:?}");
}

/// A port nothing listens on now.
#[cfg(unix)]
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a port");
    listener.local_addr().expect("its address").port()
}

/// Valkey's ladder (ADR 0023), with `wait` after the request: `SHUTDOWN
/// SAVE` to `port`, then SIGTERM, then a kill.
#[cfg(unix)]
fn shutdown_save(port: u16, wait: Duration) -> StopMethod {
    use marsa_core::services::{Signal, StopAction, StopStep, TcpRequest};
    let request = TcpRequest {
        port,
        bytes: SHUTDOWN_SAVE.to_vec(),
        name: "SHUTDOWN SAVE",
        limit: Duration::from_secs(2),
    };
    let mut stop = StopMethod::signals([(Signal::Term, GRACE)]);
    let send = StopStep {
        action: StopAction::Send(request),
        wait,
    };
    stop.steps.insert(0, send);
    stop
}

/// `SHUTDOWN SAVE` in RESP, as Valkey reads it.
#[cfg(unix)]
const SHUTDOWN_SAVE: &[u8] = b"*2\r\n$8\r\nSHUTDOWN\r\n$4\r\nSAVE\r\n";

/// The line the fake prints for the request it got.
#[cfg(unix)]
fn got_shutdown_save() -> String {
    format!("fake got {:?}", String::from_utf8_lossy(SHUTDOWN_SAVE))
}

/// Starts the fake TCP server in `mode` on a free port, stopped with
/// `SHUTDOWN SAVE` and `wait`, then stops it. Gives its events and how long
/// the stop took.
#[cfg(unix)]
async fn stop_kv(mode: &str, wait: Duration) -> (Vec<Event>, Duration) {
    let port = free_port();
    let fake = spec(mode, RestartPolicy::Never, shutdown_save(port, wait));
    let mut vars = fake.env.clone();
    vars.insert(FAKE_PORT.to_owned(), port.to_string());
    let (mut handle, mut rx, _logs) = start_spec(ServiceSpec { env: vars, ..fake });
    ready(&handle).await;
    let started = Instant::now();
    handle.cancel.cancel();
    let events = finish(&mut handle, &mut rx).await;
    let elapsed = started.elapsed();
    let changes = states(&events);
    assert_eq!(
        changes[2..],
        [ServiceState::Stopping, ServiceState::Stopped]
    );
    (events, elapsed)
}

/// The server gets exactly `SHUTDOWN SAVE` in RESP, and its exit ends the
/// stop: no SIGTERM.
#[cfg(unix)]
#[tokio::test]
async fn a_stop_request_reaches_the_port_and_ends_the_stop() {
    // Long, so a stop that needed the SIGTERM would fail the time check.
    let wait = Duration::from_secs(10);
    let (events, elapsed) = stop_kv("kv", wait).await;
    assert!(elapsed < wait, "the request did not stop it: {elapsed:?}");
    let want = ["fake ready".to_owned(), got_shutdown_save()];
    assert_eq!(fake_texts(&events), want);
}

/// A server that takes the request but neither exits nor closes gets
/// SIGTERM after the request's wait, not before.
#[cfg(unix)]
#[tokio::test]
async fn a_server_that_never_closes_gets_sigterm_after_the_wait() {
    let (events, elapsed) = stop_kv("kv-mute", GRACE).await;
    assert!(elapsed >= GRACE, "SIGTERM came too early: {elapsed:?}");
    assert!(elapsed < GRACE + Duration::from_secs(3), "{elapsed:?}");
    let sigterm = "fake got SIGTERM".to_owned();
    let want = ["fake ready".to_owned(), got_shutdown_save(), sigterm];
    assert_eq!(fake_texts(&events), want);
}

/// A reply is a refusal: SIGTERM comes at once, without the wait.
#[cfg(unix)]
#[tokio::test]
async fn a_refused_request_goes_on_to_sigterm_at_once() {
    let wait = Duration::from_secs(10);
    let (events, elapsed) = stop_kv("kv-refuse", wait).await;
    assert!(elapsed < wait, "it waited after the refusal: {elapsed:?}");
    let sigterm = "fake got SIGTERM".to_owned();
    let want = ["fake ready".to_owned(), got_shutdown_save(), sigterm];
    assert_eq!(fake_texts(&events), want);
}

/// Nothing listens on the port: SIGTERM comes at once.
#[cfg(unix)]
#[tokio::test]
async fn a_request_nobody_takes_goes_on_to_sigterm_at_once() {
    let wait = Duration::from_secs(10);
    let (events, elapsed) = stop_fake("pg", shutdown_save(free_port(), wait)).await;
    assert!(
        elapsed < wait,
        "it waited after a failed connect: {elapsed:?}"
    );
    // `pg` ignores SIGTERM: the kill after it ends it.
    assert_eq!(fake_texts(&events), ["fake ready", "fake got SIGTERM"]);
}

/// The daemon's shutdown gives the request its connect and its write too.
#[cfg(unix)]
#[test]
fn a_request_adds_its_limits_to_the_stop_limit() {
    use marsa::daemon::supervisor::{STOP_MARGIN, stop_limit};
    let stop = shutdown_save(6379, Duration::from_secs(10));
    let want = Duration::from_secs(10 + 4) + GRACE + STOP_MARGIN;
    assert_eq!(stop_limit(&stop), want);
}

/// The fake stop program with `arg` as one step of `wait`, then a kill: the
/// shape of Windows' stop of MySQL and PostgreSQL.
fn stop_program(arg: &str, wait: Duration) -> StopMethod {
    let program = std::env::current_exe().expect("the test binary has a path");
    stop_program_at(program, arg, wait)
}

fn stop_program_at(program: PathBuf, arg: &str, wait: Duration) -> StopMethod {
    use marsa_core::services::{StopAction, StopCommand, StopStep};
    let args = ["fake_stop_program", "--exact", "--nocapture", arg];
    let command = StopCommand {
        program,
        args: args.map(String::from).to_vec(),
        name: "fake stop",
        on_failure: "The fake refused.",
    };
    StopMethod {
        steps: vec![StopStep {
            action: StopAction::Run(command),
            wait,
        }],
    }
}

/// What a stop by a program left: every line of the service, how long the
/// stop took, the fake's folder, and the logs folder.
struct Stopped {
    texts: Vec<String>,
    elapsed: Duration,
    dir: TempDir,
    logs: TempDir,
}

impl Stopped {
    fn has(&self, text: &str) -> bool {
        self.texts.iter().any(|line| line == text)
    }

    fn starting_with(&self, text: &str) -> Option<&String> {
        self.texts.iter().find(|line| line.starts_with(text))
    }
}

/// Starts the `watch` fake in a new folder, stopped by the method that
/// `stop` makes for that folder, then stops it.
async fn stop_watch(stop: impl FnOnce(&Path) -> StopMethod) -> Stopped {
    let dir = tempfile::tempdir().expect("a temp folder");
    let fake = spec("watch", RestartPolicy::Never, stop(dir.path()));
    let mut vars = fake.env.clone();
    vars.insert(FAKE_DIR.to_owned(), dir.path().display().to_string());
    let (mut handle, mut rx, logs) = start_spec(ServiceSpec { env: vars, ..fake });
    ready(&handle).await;
    let started = Instant::now();
    handle.cancel.cancel();
    let events = finish(&mut handle, &mut rx).await;
    let elapsed = started.elapsed();
    let changes = states(&events);
    assert_eq!(
        changes[2..],
        [ServiceState::Stopping, ServiceState::Stopped]
    );
    assert_eq!(handle.status().pid, None);
    let texts = events
        .into_iter()
        .filter_map(|event| match event {
            Event::Log(line) => Some(line.text),
            _ => None,
        })
        .collect();
    Stopped {
        texts,
        elapsed,
        dir,
        logs,
    }
}

/// The program asks, the server exits, and the stop ends there: no kill,
/// no wait, no line of a failure.
#[tokio::test]
async fn a_stop_program_that_asks_ends_the_stop() {
    let wait = Duration::from_secs(10);
    let stop = |dir: &Path| stop_program(&format!("{STOP_ARG}{}", dir.display()), wait);
    let stopped = stop_watch(stop).await;
    assert!(stopped.elapsed < wait, "{:?}", stopped.elapsed);
    for line in [
        "marsad: asking watch to stop with fake stop",
        "fake stop asked",
        "fake got the stop file",
    ] {
        assert!(stopped.has(line), "{line}: {:?}", stopped.texts);
    }
    let failed = stopped.starting_with("marsad: fake stop failed");
    assert_eq!(failed, None, "{:?}", stopped.texts);
    assert_eq!(stopped.starting_with("marsad: watch still runs"), None);
}

/// A program that fails says why in the log, and the server still gets the
/// whole wait: the kill comes after it, not before.
#[tokio::test]
async fn a_failing_stop_program_leads_to_the_kill_only_after_the_wait() {
    // Long enough for the program to start and refuse within the wait.
    let wait = Duration::from_secs(3);
    let stopped = stop_watch(|_| stop_program(REFUSE_ARG, wait)).await;
    let elapsed = stopped.elapsed;
    assert!(elapsed >= wait, "killed too early: {elapsed:?}");
    assert!(elapsed < wait + Duration::from_secs(3), "{elapsed:?}");
    let refused = "fake stop: Access denied for user 'root'@'127.0.0.1'";
    assert!(stopped.has(refused), "{:?}", stopped.texts);
    let failed = stopped
        .starting_with("marsad: fake stop failed (")
        .unwrap_or_else(|| panic!("no failure line: {:?}", stopped.texts));
    assert!(failed.ends_with("). The fake refused."), "{failed}");
    assert!(stopped.has("marsad: watch still runs 3 s after fake stop"));
    assert!(!stopped.has("fake got the stop file"));
    // The log file says it too.
    let log = std::fs::read_to_string(stopped.logs.path().join("watch.log")).unwrap();
    assert!(log.contains(failed.as_str()), "{log}");
    assert!(log.contains(refused), "{log}");
}

/// A program that never ends is ended with its step.
#[tokio::test]
async fn a_stop_program_that_hangs_is_ended_with_its_step() {
    let wait = Duration::from_secs(3);
    let stop = |dir: &Path| stop_program(&format!("{HANG_ARG}{}", dir.display()), wait);
    let stopped = stop_watch(stop).await;
    let elapsed = stopped.elapsed;
    assert!(elapsed >= wait, "killed too early: {elapsed:?}");
    assert!(elapsed < wait + Duration::from_secs(3), "{elapsed:?}");
    assert!(stopped.has("marsad: watch still runs 3 s after fake stop"));
    let pid = std::fs::read_to_string(stopped.dir.path().join("hang.pid")).unwrap();
    // Only macOS and Linux tell whether a pid still runs.
    #[cfg(unix)]
    {
        use marsa_core::orphan::Liveness;
        use marsa_core::platform::process::identity;
        let pid: u32 = pid.parse().unwrap();
        let gone = async {
            while identity(pid).await != Liveness::Gone {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), gone)
            .await
            .expect("the stop program was ended");
    }
    #[cfg(not(unix))]
    assert!(!pid.is_empty());
}

/// A program that cannot start asked nothing: the next step, here the kill,
/// comes at once.
#[tokio::test]
async fn a_stop_program_that_cannot_start_skips_its_wait() {
    let wait = Duration::from_secs(10);
    let stop = |dir: &Path| stop_program_at(dir.join("no-such-program"), "x", wait);
    let stopped = stop_watch(stop).await;
    assert!(stopped.elapsed < wait, "{:?}", stopped.elapsed);
    let line = stopped.starting_with("marsad: fake stop could not be started: ");
    assert!(line.is_some(), "{:?}", stopped.texts);
}

/// The daemon's shutdown gives a stop program its wait and its grace.
#[test]
fn a_stop_program_adds_its_grace_to_the_stop_limit() {
    use marsa::daemon::supervisor::{STOP_MARGIN, stop_limit};
    use marsa_core::services::STOP_COMMAND_GRACE;
    let stop = stop_program(REFUSE_ARG, Duration::from_secs(30));
    let want = Duration::from_secs(30) + STOP_COMMAND_GRACE + STOP_MARGIN;
    assert_eq!(stop_limit(&stop), want);
}
