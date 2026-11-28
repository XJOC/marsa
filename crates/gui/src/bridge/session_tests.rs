//! What a lost session leaves behind: nothing reaches the next one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::routing::{delete, get, post};
use axum::{Json, Router};
use marsa_core::types::{
    DbQueryRequest, DbSource, DbSources, JobAccepted, JobEvent, JobKind, JobStep, JobUpdate,
};
use tokio::sync::Notify;

use super::fake::{Fake, PATIENCE};
use super::*;

/// How long a request that must not come is waited for.
const QUIET: Duration = Duration::from_millis(500);

/// Reads updates until one passes `wanted`, and gives them all.
fn until(handle: &Handle, wanted: impl Fn(&Stamped<Update>) -> bool) -> Vec<Stamped<Update>> {
    let end = Instant::now() + PATIENCE;
    let mut seen = Vec::new();
    loop {
        let left = end.saturating_duration_since(Instant::now());
        let update = match handle.updates.recv_timeout(left) {
            Ok(update) => update,
            Err(err) => panic!("{err}; the updates so far: {seen:?}"),
        };
        let done = wanted(&update);
        seen.push(update);
        if done {
            return seen;
        }
    }
}

/// The number of the next session that connects.
fn connected(handle: &Handle) -> u64 {
    let seen = until(handle, |m| matches!(m.body, Update::Connected(_)));
    seen.last().expect("the connection").session
}

fn sources_answer(message: &Stamped<Update>, wanted: u64) -> bool {
    matches!(
        &message.body,
        Update::Db(DbUpdate::Sources { id, .. }) if *id == wanted
    )
}

fn read_sources(id: u64) -> Command {
    Command::Db(DbCommand::ReadSources { id })
}

#[test]
fn an_answer_of_a_lost_session_never_reaches_the_window() {
    // The first read waits until the test lets it go.
    let release = Arc::new(Notify::new());
    let held = Arc::clone(&release);
    let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let sources = get(move || {
        let held = Arc::clone(&held);
        let wait = first.swap(false, std::sync::atomic::Ordering::SeqCst);
        async move {
            if wait {
                held.notified().await;
            }
            Json(DbSources::default())
        }
    });
    let fake = Fake::start(Router::new().route("/v1/db/sources", sources));
    let handle = attach(fake.data_dir().to_path_buf()).expect("a thread");
    let old = connected(&handle);
    handle.send(old, read_sources(1));
    fake.wait_for("GET /v1/db/sources");
    fake.end_events();
    let new = connected(&handle);
    assert_ne!(old, new);
    release.notify_one();
    std::thread::sleep(QUIET);
    handle.send(new, read_sources(2));
    let seen = until(&handle, |m| sources_answer(m, 2));
    assert!(
        !seen.iter().any(|m| sources_answer(m, 1)),
        "the old session's answer came: {seen:?}"
    );
}

#[test]
fn a_command_made_for_another_session_is_not_run() {
    let routes = Router::new()
        .route(
            "/v1/db/sources",
            get(|| async { Json(DbSources::default()) }),
        )
        .route("/v1/db/close", post(|| async {}));
    let fake = Fake::start(routes);
    let handle = attach(fake.data_dir().to_path_buf()).expect("a thread");
    let session = connected(&handle);
    // Made by a window that has not heard of this session yet.
    handle.send(session - 1, Command::Db(DbCommand::Close));
    handle.send(session, read_sources(1));
    until(&handle, |m| sources_answer(m, 1));
    std::thread::sleep(QUIET);
    assert_eq!(fake.count("POST /v1/db/close"), 0);
}

#[test]
fn a_statement_of_a_lost_session_is_cancelled() {
    let running = || JobEvent {
        id: 7,
        kind: JobKind::DbQuery,
        update: JobUpdate::Running {
            step: JobStep::Started,
        },
    };
    let routes = Router::new()
        .route(
            "/v1/jobs/db/query",
            post(|| async { Json(JobAccepted { id: 7 }) }),
        )
        .route(
            "/v1/jobs/7",
            get(move || async move { Json(running()) }).merge(delete(|| async {})),
        );
    let fake = Fake::start(routes);
    let handle = attach(fake.data_dir().to_path_buf()).expect("a thread");
    let session = connected(&handle);
    let request = DbQueryRequest {
        source: DbSource::SiteFile {
            site: "shop".into(),
        },
        sql: "select 1".into(),
    };
    handle.send(session, Command::Db(DbCommand::RunQuery { id: 1, request }));
    // The statement is followed: its stream is open and its job was asked.
    fake.wait_for("GET /v1/jobs/7");
    assert_eq!(fake.count("DELETE /v1/jobs/7"), 0);
    fake.end_events();
    fake.wait_for("DELETE /v1/jobs/7");
}
