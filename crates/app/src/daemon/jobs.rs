//! The jobs table and its routes (ADR 0026): every long operation is a job.
//! A `POST /v1/jobs/...` answers `202 JobAccepted` at once; the progress and
//! the end come as `job` events on `/v1/events`, and the table keeps them so
//! that a client that lost the stream, or came late, still finds the result.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/jobs` | `JobList`: the jobs that run now |
//! | `GET /v1/jobs/{id}` | `JobEvent`: the last event of a running job, or of one of the last 16 that ended; 404 for any other |
//! | `DELETE /v1/jobs/{id}` | `202`: a running `db_query` job is cancelled (ADR 0028); 409 `job_not_cancellable` for any other kind; 404 if it does not run |
//!
//! The table lives in memory: a restart of the daemon empties it. It holds
//! reports, never the token.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use marsa_core::codes;
use marsa_core::types::{
    JobAccepted, JobEvent, JobFailure, JobKind, JobList, JobOutcome, JobStep, JobUpdate,
};
use serde::Serialize;
use tokio::sync::oneshot;

use super::http;
use super::http::Rejected as _;
use super::http::routes::HttpError;
use super::state::Daemon;

/// How many ended jobs the table keeps.
const KEEP_ENDED: usize = 16;

/// The id counter for every job and every install, and the last event of
/// each job. An install is not in the table: it keeps its own `install`
/// events.
#[derive(Debug, Default)]
pub struct Jobs {
    last_id: AtomicU64,
    table: Mutex<Table>,
}

#[derive(Debug, Default)]
struct Table {
    running: BTreeMap<u64, JobEvent>,
    /// Oldest first.
    ended: VecDeque<JobEvent>,
}

impl Jobs {
    /// A new id. No two jobs, of any kind, get the same one.
    pub fn next_id(&self) -> u64 {
        self.last_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Keeps `event` as the last of its job.
    pub(super) fn record(&self, event: &JobEvent) {
        // A panic while the lock is held can only leave a map that is
        // missing one update, so a poisoned lock is still safe to use.
        let mut table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        if event.is_last() {
            table.running.remove(&event.id);
            if table.ended.len() == KEEP_ENDED {
                table.ended.pop_front();
            }
            table.ended.push_back(event.clone());
        } else {
            table.running.insert(event.id, event.clone());
        }
    }

    /// The jobs that run now, oldest first.
    pub fn running(&self) -> Vec<JobEvent> {
        let table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        table.running.values().cloned().collect()
    }

    /// The last event of job `id`, if it runs or ended recently.
    pub fn get(&self, id: u64) -> Option<JobEvent> {
        let table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        table
            .running
            .get(&id)
            .or_else(|| table.ended.iter().find(|event| event.id == id))
            .cloned()
    }
}

/// What the old waiting route of a job answers, sent after the job's last
/// event.
pub(super) type Ended<T> = oneshot::Receiver<Result<T, HttpError>>;

/// Starts a job of `kind`: publishes its first event, runs `work` in its own
/// task (so a client that goes away loses nothing, and a panic still ends
/// the job with `failed`), then publishes `done` or `failed`. Answers the
/// id and the receiver the old waiting route reads.
///
/// `work` frees the one-at-a-time flag before it ends, so a client that
/// reads the last event can start the next job at once. `after` runs once
/// the last event is out, before the old route has its answer, only if the
/// job ended well: the uninstall stops the daemon there, so a reader gets `done`
/// before `shutting_down`. `what` names the job in a panic's answer.
pub(super) fn start<T, W, A>(
    daemon: &Arc<Daemon>,
    kind: JobKind,
    what: &'static str,
    outcome: fn(T) -> JobOutcome,
    work: W,
    after: A,
) -> (u64, Ended<T>)
where
    T: Clone + Send + 'static,
    W: Future<Output = Result<T, HttpError>> + Send + 'static,
    A: FnOnce(&Daemon, &T) + Send + 'static,
{
    let id = daemon.jobs().next_id();
    (id, start_as(daemon, id, kind, what, outcome, work, after))
}

/// [`start`], for a caller that needs the job's id, from [`Jobs::next_id`],
/// before the job is visible: what it prepares under that id is in place
/// when the first request that names the job arrives.
pub(super) fn start_as<T, W, A>(
    daemon: &Arc<Daemon>,
    id: u64,
    kind: JobKind,
    what: &'static str,
    outcome: fn(T) -> JobOutcome,
    work: W,
    after: A,
) -> Ended<T>
where
    T: Clone + Send + 'static,
    W: Future<Output = Result<T, HttpError>> + Send + 'static,
    A: FnOnce(&Daemon, &T) + Send + 'static,
{
    let event = move |update| JobEvent { id, kind, update };
    daemon.publish_job(event(JobUpdate::Running {
        step: JobStep::Started,
    }));
    let (send, ended) = oneshot::channel();
    let daemon = Arc::clone(daemon);
    tokio::spawn(async move {
        let result = tokio::spawn(work)
            .await
            .unwrap_or_else(|err| Err(http::job_failed(what, &err)));
        let update = match &result {
            Ok(done) => JobUpdate::Done(outcome(done.clone())),
            Err(err) => JobUpdate::Failed(JobFailure {
                code: err.code.to_owned(),
                message: err.message.clone(),
                http_status: Some(err.status.as_u16()),
                detail: err.detail.clone(),
            }),
        };
        daemon.publish_job(event(update));
        // Before the old route's answer: the uninstall stops the daemon here,
        // and the server's graceful stop waits for that answer.
        if let Ok(done) = &result {
            after(&daemon, done);
        }
        // Nobody waits when the job came from a `/v1/jobs/...` route.
        let _ = send.send(result);
    });
    ended
}

/// The answer of a `POST /v1/jobs/...` route: `202` and the job's id. The
/// receiver is dropped: nobody waits for the old route's answer.
pub(super) fn accepted<T>((id, _): (u64, Ended<T>)) -> (StatusCode, Json<JobAccepted>) {
    (StatusCode::ACCEPTED, Json(JobAccepted { id }))
}

/// What the old waiting route of a job answers: the job's result, or its
/// error, once its last event is out. `what` names the job if its task is
/// gone (only when the daemon's runtime is going away).
pub(super) async fn answer<T: Serialize>(
    ended: Ended<T>,
    what: &str,
) -> Result<Json<T>, HttpError> {
    let result = ended.await.unwrap_or_else(|_| {
        Err(HttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            codes::INTERNAL_ERROR,
            format!("the {what} job did not end"),
        ))
    });
    result.map(Json)
}

/// `GET /v1/jobs`, `GET /v1/jobs/{id}`, and `DELETE /v1/jobs/{id}`.
pub fn routes() -> Router<Arc<Daemon>> {
    Router::new()
        .route("/v1/jobs", get(list))
        .route("/v1/jobs/{id}", get(one).delete(cancel))
}

async fn list(State(daemon): State<Arc<Daemon>>) -> Json<JobList> {
    Json(JobList {
        jobs: daemon.jobs().running(),
    })
}

async fn one(
    State(daemon): State<Arc<Daemon>>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<JobEvent>, HttpError> {
    let Path(id) = path.map_err(|err| err.reject(codes::INVALID_PATH))?;
    let not_found = || HttpError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND, "no such job");
    // A name that is not a number is no job.
    let id: u64 = id.parse().map_err(|_| not_found())?;
    daemon.jobs().get(id).map(Json).ok_or_else(not_found)
}

/// Only a statement of the database browser's user can be cancelled: the
/// other jobs change the system, and stopping one half way could leave it
/// half changed.
async fn cancel(
    State(daemon): State<Arc<Daemon>>,
    path: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, HttpError> {
    let Path(id) = path.map_err(|err| err.reject(codes::INVALID_PATH))?;
    let not_running =
        || HttpError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND, "no such job runs");
    let id: u64 = id.parse().map_err(|_| not_running())?;
    let event = daemon
        .jobs()
        .get(id)
        .filter(|event| !event.is_last())
        .ok_or_else(not_running)?;
    if event.kind != JobKind::DbQuery {
        return Err(HttpError::new(
            StatusCode::CONFLICT,
            codes::JOB_NOT_CANCELLABLE,
            "only a running SQL statement can be cancelled",
        ));
    }
    crate::daemon::db_browser::cancel_query(&daemon, id);
    Ok(StatusCode::ACCEPTED)
}

#[cfg(test)]
mod tests {
    use marsa_core::types::{JobKind, JobOutcome, JobStep, JobUpdate, SetupReport};

    use super::*;
    use crate::daemon::http::test_router::TestRouter;

    fn event(id: u64, update: JobUpdate) -> JobEvent {
        JobEvent {
            id,
            kind: JobKind::Setup,
            update,
        }
    }

    fn running(id: u64) -> JobEvent {
        event(
            id,
            JobUpdate::Running {
                step: JobStep::Started,
            },
        )
    }

    fn done(id: u64) -> JobEvent {
        event(
            id,
            JobUpdate::Done(JobOutcome::Setup(SetupReport::default())),
        )
    }

    #[tokio::test]
    async fn the_routes_list_running_jobs_and_find_one_by_id() {
        let daemon = Arc::new(Daemon::new(std::path::PathBuf::from("data")));
        daemon.publish_job(running(1));
        daemon.publish_job(done(2));
        let app = TestRouter::new(daemon);

        let (status, json) = app.answer("GET", "/v1/jobs", None).await;
        assert_eq!(status, StatusCode::OK);
        let list: JobList = serde_json::from_value(json).unwrap();
        assert_eq!(list.jobs, [running(1)]);

        let (status, json) = app.answer("GET", "/v1/jobs/2", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(serde_json::from_value::<JobEvent>(json).unwrap(), done(2));

        // No such job, and not a number: the same answer. A name that is not
        // even valid UTF-8 once decoded has the same error body.
        for uri in ["/v1/jobs/3", "/v1/jobs/x", "/v1/jobs/-1"] {
            let (status, json) = app.answer("GET", uri, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(json["error"]["code"], "not_found");
        }
        let (status, json) = app.answer("GET", "/v1/jobs/%FF", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"]["code"], "invalid_path");
    }

    /// A job's result is for the client that has the token (ADR 0026), and
    /// only through `Host` of the daemon, with no `Origin`.
    #[tokio::test]
    async fn the_routes_need_the_token_and_refuse_an_origin() {
        let daemon = Arc::new(Daemon::new(std::path::PathBuf::from("data")));
        daemon.publish_job(done(1));
        let app = TestRouter::new(daemon);
        for uri in ["/v1/jobs", "/v1/jobs/1"] {
            app.assert_refused_without_auth("GET", uri).await;
            // The same route with the token answers: the refusals came from
            // the auth layer.
            assert_eq!(
                app.answer("GET", uri, None).await.0,
                StatusCode::OK,
                "{uri}"
            );
        }
    }

    #[test]
    fn ids_never_repeat() {
        let jobs = Jobs::default();
        assert_eq!([jobs.next_id(), jobs.next_id()], [1, 2]);
    }

    #[test]
    fn a_job_runs_then_ends() {
        let jobs = Jobs::default();
        jobs.record(&running(1));
        assert_eq!(jobs.running(), [running(1)]);
        assert_eq!(jobs.get(1), Some(running(1)));
        jobs.record(&done(1));
        assert!(jobs.running().is_empty());
        assert_eq!(jobs.get(1), Some(done(1)));
        assert_eq!(jobs.get(2), None);
    }

    #[test]
    fn only_the_last_sixteen_ended_jobs_are_kept() {
        let jobs = Jobs::default();
        for id in 1..=17 {
            jobs.record(&done(id));
        }
        assert_eq!(jobs.get(1), None);
        assert_eq!(jobs.get(2), Some(done(2)));
        assert_eq!(jobs.get(17), Some(done(17)));
    }
}
