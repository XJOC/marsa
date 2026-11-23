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
use crate::types::{ErrorBody, Event, JobAccepted, JobEvent, JobOutcome, JobUpdate, SetupReport};

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
        let JobOutcome::Setup(report) = self
            .run_job(&["v1", "jobs", "setup"], codes::SETUP_INTERRUPTED)
            .await?;
        Ok(report)
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
                // Lost events may include the end; a broken connection ends
                // the stream. Either way: open it again, then ask.
                Some(Ok(StreamItem::Lagged(_)) | Err(_)) | None => {
                    resyncs += 1;
                    if resyncs > MAX_RESYNCS {
                        return Err(lost_job(lost));
                    }
                    stream = self.events().await?;
                    match self.job(id).await {
                        Ok(event) => {
                            if let Some(end) = ended(event) {
                                return end;
                            }
                        }
                        // The daemon restarted: the table is empty.
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
        JobUpdate::Failed(error) => {
            let body = ErrorBody {
                error,
                in_use: None,
            };
            Some(Err(ClientError::api(500, &body)))
        }
    }
}

fn lost_job(code: &'static str) -> ClientError {
    ClientError::api(
        500,
        &ErrorBody::new(code, "the daemon stopped before the job ended"),
    )
}
