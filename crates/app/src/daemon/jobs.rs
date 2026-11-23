//! The jobs table and its routes (ADR 0026): every long operation is a job.
//! A `POST /v1/jobs/...` answers `202 JobAccepted` at once; the progress and
//! the end come as `job` events on `/v1/events`, and the table keeps them so
//! that a client that lost the stream, or came late, still finds the result.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/jobs` | `JobList`: the jobs that run now |
//! | `GET /v1/jobs/{id}` | `JobEvent`: the last event of a running job, or of one of the last 16 that ended; 404 for any other |
//!
//! The table lives in memory: a restart of the daemon empties it. It holds
//! reports, never the token.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use marsa_core::codes;
use marsa_core::types::{JobEvent, JobList};

use super::http::routes::HttpError;
use super::state::Daemon;

/// How many ended jobs the table keeps.
const KEEP_ENDED: usize = 16;

/// The id counter for every job, installs included, and the last event of
/// each job.
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

/// `GET /v1/jobs` and `GET /v1/jobs/{id}`.
pub fn routes() -> Router<Arc<Daemon>> {
    Router::new()
        .route("/v1/jobs", get(list))
        .route("/v1/jobs/{id}", get(one))
}

async fn list(State(daemon): State<Arc<Daemon>>) -> Json<JobList> {
    Json(JobList {
        jobs: daemon.jobs().running(),
    })
}

async fn one(
    State(daemon): State<Arc<Daemon>>,
    Path(id): Path<String>,
) -> Result<Json<JobEvent>, HttpError> {
    let not_found = || HttpError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND, "no such job");
    // A name that is not a number is no job.
    let id: u64 = id.parse().map_err(|_| not_found())?;
    daemon.jobs().get(id).map(Json).ok_or_else(not_found)
}

#[cfg(test)]
mod tests {
    use marsa_core::types::{JobKind, JobOutcome, JobStep, JobUpdate, SetupReport};

    use super::*;

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

    async fn get(daemon: &Arc<Daemon>, uri: &str) -> (StatusCode, serde_json::Value) {
        use axum::body::{Body, to_bytes};
        use axum::http::Request;
        use tower::ServiceExt as _;

        let router = routes().with_state(Arc::clone(daemon));
        let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn the_routes_list_running_jobs_and_find_one_by_id() {
        let daemon = Arc::new(Daemon::new(std::path::PathBuf::from("data")));
        daemon.publish_job(running(1));
        daemon.publish_job(done(2));

        let (status, json) = get(&daemon, "/v1/jobs").await;
        assert_eq!(status, StatusCode::OK);
        let list: JobList = serde_json::from_value(json).unwrap();
        assert_eq!(list.jobs, [running(1)]);

        let (status, json) = get(&daemon, "/v1/jobs/2").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(serde_json::from_value::<JobEvent>(json).unwrap(), done(2));

        // No such job, and not a number: the same answer.
        for uri in ["/v1/jobs/3", "/v1/jobs/x", "/v1/jobs/-1"] {
            let (status, json) = get(&daemon, uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(json["error"]["code"], "not_found");
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
