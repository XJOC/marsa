//! The supervisor against a fake service. The fake is this test binary: it
//! runs itself again with `MARSA_FAKE_SERVICE` set, so no external program is
//! needed and the tests are the same on macOS, Linux, and Windows.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use marsa::daemon::supervisor::{ServiceHandle, Supervisor};
use marsa_core::services::{RestartPolicy, ServiceSpec};
use marsa_core::types::{Event, LogLine, LogStream, ServiceState};
use tempfile::TempDir;
use tokio::sync::broadcast;

const FAKE: &str = "MARSA_FAKE_SERVICE";
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
        other => panic!("unknown fake mode {other}"),
    }
}

type Started = (ServiceHandle, broadcast::Receiver<Event>, TempDir);

/// Starts the fake in `mode`. Returns the handle, the event bus, and the logs
/// folder.
fn start(mode: &str, restart: RestartPolicy) -> Started {
    let spec = ServiceSpec {
        id: mode.into(),
        program: std::env::current_exe().expect("the test binary has a path"),
        // Runs only the entry point, under libtest and under nextest alike.
        args: ["fake_service", "--exact", "--nocapture"]
            .map(String::from)
            .to_vec(),
        env: BTreeMap::from([(FAKE.to_owned(), mode.to_owned())]),
        cwd: None,
        restart,
        stop_grace: GRACE,
    };
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
        stop_grace: GRACE,
    };
    let err = Supervisor::spawn(spec, tx, std::path::Path::new("logs")).unwrap_err();
    assert_eq!(err.code(), "program_not_absolute");
}
