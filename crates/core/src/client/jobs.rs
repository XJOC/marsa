//! Following a job to its end (ADR 0026), the one place that does it for
//! the CLI, the GUI, and the panel.
//!
//! A job can end while no one reads the event stream: the connection broke,
//! the reader lagged, or the client came late. So the client first opens
//! the stream, then starts the job, and follows its events. When the stream
//! breaks or lags, it opens the stream again and only then asks
//! `GET /v1/jobs/{id}`: an end that falls in the gap is found there, and an
//! end after it comes on the new stream.

use reqwest::Method;

use super::{ClientError, DaemonClient, REQUEST_TIMEOUT, StreamItem};
use crate::codes;
use crate::types::{
    ErrorBody, Event, JobAccepted, JobEvent, JobOutcome, JobUpdate, ServiceAction, ServiceStatus,
    SetupReport, UninstallReport,
};

/// How many times in a row the stream may break, or lag, with no event
/// between, before the client gives up.
const MAX_RESYNCS: u32 = 5;

impl DaemonClient {
    /// `POST /v1/jobs/setup`, then follows the job until it ends. On macOS
    /// the user is asked twice: for an administrator password, then to
    /// approve the trust in Marsa's certificate authority for this user; on
    /// Windows UAC may ask once. The wait has no time limit of its own: the
    /// user may take a while at each prompt, and the daemon ends the job.
    /// The same report also comes as `Event::SetupFinished`.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `setup_running` (409) while another setup runs,
    /// and `setup_not_supported` (501) on a system Marsa has no setup for. A
    /// cancelled prompt is not an error: it is in [`SetupReport::error`].
    /// `setup_interrupted` if the daemon stopped before the job ended.
    pub async fn run_setup(&self) -> Result<SetupReport, ClientError> {
        match self
            .run_job(&["v1", "jobs", "setup"], codes::SETUP_INTERRUPTED)
            .await?
        {
            JobOutcome::Setup(report) => Ok(report),
            _ => Err(wrong_outcome()),
        }
    }

    /// `POST /v1/jobs/uninstall`, then follows the job until it ends: one
    /// admin prompt for the helper's removals, then the user's trust and the
    /// login file. When [`UninstallReport::stopping`] is `true` the daemon
    /// stops right after the job's last event: [`stop_daemon`](super::stop_daemon)
    /// waits until it has.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `uninstall_running` or `setup_running` (409)
    /// while one of them runs, and `uninstall_not_supported` (501) on a
    /// system Marsa has no uninstall for. A cancelled prompt is not an
    /// error: it is in [`UninstallReport::error`]. `uninstall_interrupted`
    /// if the daemon stopped before the job's end reached this client.
    pub async fn run_uninstall_all(&self) -> Result<UninstallReport, ClientError> {
        match self
            .run_job(&["v1", "jobs", "uninstall"], codes::UNINSTALL_INTERRUPTED)
            .await?
        {
            JobOutcome::Uninstall(report) => Ok(report),
            _ => Err(wrong_outcome()),
        }
    }

    /// `POST /v1/jobs/hosts-sync`, then follows the job until it ends: the
    /// names of every site go into Marsa's block in the hosts file, behind
    /// the password prompt, if any is missing; with none missing, nothing
    /// runs. Only for a command of the user: never call it from a
    /// background task. The helper's result, or why it did not run, is in
    /// the report, as for setup.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `hosts_not_used` (409) where this system does
    /// not use the hosts file, and `setup_running` or `uninstall_running`
    /// (409) while one of those runs; `setup_interrupted` if the daemon
    /// stopped before the job ended.
    pub async fn run_sync_hosts(&self) -> Result<SetupReport, ClientError> {
        match self
            .run_job(&["v1", "jobs", "hosts-sync"], codes::SETUP_INTERRUPTED)
            .await?
        {
            JobOutcome::HostsSync(report) => Ok(report),
            _ => Err(wrong_outcome()),
        }
    }

    /// `POST /v1/jobs/services/{id}/{action}`, then follows the job until
    /// it ends: the status of the service after the action. A start of a
    /// database server may run for minutes (`initdb`).
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `service_not_found` (404) and
    /// `unknown_action` (400) at once, `service_managed_by_sites` (409) for a
    /// php-fpm pool, and the codes of the action itself (`service_busy`,
    /// `postgres_not_installed`, ...) when the job ends. `daemon_unreachable`
    /// if the daemon stopped before the job ended.
    pub async fn run_service_action(
        &self,
        id: &str,
        action: ServiceAction,
    ) -> Result<ServiceStatus, ClientError> {
        match self
            .run_job(
                &["v1", "jobs", "services", id, action.as_str()],
                codes::DAEMON_UNREACHABLE,
            )
            .await?
        {
            JobOutcome::ServiceAction(status) => Ok(status),
            _ => Err(wrong_outcome()),
        }
    }

    /// `GET /v1/jobs/{id}`: the last event of a running job, or of one of
    /// the last sixteen that ended. `not_found` (404) for any other.
    pub async fn job(&self, id: u64) -> Result<JobEvent, ClientError> {
        let id = id.to_string();
        self.call(Method::GET, &["v1", "jobs", &id], REQUEST_TIMEOUT)
            .await
    }

    /// Starts the job with `POST` to `segments` and follows it. `lost` is
    /// the code for a job that cannot be followed to its end: the daemon
    /// stopped or restarted.
    async fn run_job(
        &self,
        segments: &[&str],
        lost: &'static str,
    ) -> Result<JobOutcome, ClientError> {
        // An id that cannot be a path segment is refused before any request.
        self.url(segments)?;
        // Open before the start, so no event of this job is missed.
        let mut stream = self.events().await?;
        let accepted: JobAccepted = self.call(Method::POST, segments, REQUEST_TIMEOUT).await?;
        let id = accepted.id;
        let mut resyncs = 0;
        loop {
            match stream.next().await {
                Some(Ok(StreamItem::Item(Event::Job(event)))) if event.id == id => {
                    resyncs = 0;
                    if let Some(end) = ended(event) {
                        return end;
                    }
                }
                // The daemon is stopping, and the job stops with it.
                Some(Ok(StreamItem::Item(Event::ShuttingDown))) => return Err(lost_job(lost)),
                Some(Ok(StreamItem::Item(_))) => resyncs = 0,
                // An event this client cannot read (a newer daemon's other
                // job, say): the stream goes on.
                Some(Err(ClientError::BadResponse { .. })) => {}
                // Lost events may include the end; a broken connection ends
                // the stream. Either way: open it again, then ask.
                Some(Ok(StreamItem::Lagged(_)) | Err(_)) | None => {
                    resyncs += 1;
                    if resyncs > MAX_RESYNCS {
                        return Err(lost_job(lost));
                    }
                    // No stream: the daemon stopped, or restarted with a new
                    // token. The job cannot be followed.
                    stream = match self.events().await {
                        Ok(stream) => stream,
                        Err(err) => {
                            tracing::warn!(
                                code = err.code(),
                                "cannot follow the job: no event stream"
                            );
                            return Err(lost_job(lost));
                        }
                    };
                    match self.job(id).await {
                        Ok(event) => {
                            if let Some(end) = ended(event) {
                                return end;
                            }
                        }
                        // The daemon restarted with the same token: the
                        // table is empty.
                        Err(ClientError::Api { status: 404, .. }) => return Err(lost_job(lost)),
                        Err(err) => return Err(err),
                    }
                }
            }
        }
    }
}

/// The result if `event` is the last of its job.
fn ended(event: JobEvent) -> Option<Result<JobOutcome, ClientError>> {
    match event.update {
        JobUpdate::Running { .. } => None,
        JobUpdate::Done(outcome) => Some(Ok(outcome)),
        JobUpdate::Failed(failure) => {
            let body = ErrorBody::new(failure.code, failure.message);
            Some(Err(ClientError::api(
                failure.http_status.unwrap_or(500),
                &body,
            )))
        }
    }
}

/// A job that ends with the outcome of another kind: not a daemon of this
/// API.
fn wrong_outcome() -> ClientError {
    ClientError::bad_response("the job ended with the result of another kind of job")
}

fn lost_job(code: &'static str) -> ClientError {
    ClientError::api(
        500,
        &ErrorBody::new(code, "the daemon stopped before the job ended"),
    )
}
