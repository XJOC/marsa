//! Jobs (ADR 0026) and the install progress they carry.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::runtimes::Runtime;

use super::{DbQueryDone, InstallEvent, ServiceStatus, SetupReport, UninstallReport, Uninstalled};

/// The answer to every `POST /v1/jobs/...` (ADR 0026): `202` at once, then
/// `job` events with the same `id` on `/v1/events`, and `GET /v1/jobs/{id}`.
/// An install has its own answer ([`InstallAccepted`]) and its own events,
/// but its `id` comes from the same counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobAccepted {
    pub id: u64,
}

/// Which long operation a job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Setup,
    Uninstall,
    HostsSync,
    ServiceAction,
    RuntimeUninstall,
    /// A SQL statement of the database browser's user (ADR 0028).
    DbQuery,
    /// A kind a newer daemon knows. An older client still follows the job to
    /// its end by id.
    #[serde(other)]
    Other,
}

/// A step a job reports while it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStep {
    /// The job was accepted and its work began.
    Started,
    /// A step a newer daemon names (ADR 0026: later steps such as the admin
    /// prompt). An older client reads it as "still running".
    #[serde(other)]
    Other,
}

/// What a finished job produced. One variant per [`JobKind`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum JobOutcome {
    Setup(SetupReport),
    Uninstall(UninstallReport),
    HostsSync(SetupReport),
    ServiceAction(ServiceStatus),
    RuntimeUninstall(Uninstalled),
    /// A summary only: the rows wait in the daemon for
    /// `GET /v1/db/results/{id}`, because every reader of the stream gets
    /// this event.
    DbQuery(DbQueryDone),
}

/// The `status` of a [`JobEvent`]. The last one of a job is `done` or
/// `failed`, unless the daemon stops first (then `shutting_down` ends the
/// stream instead).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum JobUpdate {
    Running {
        step: JobStep,
    },
    Done(JobOutcome),
    /// What the old waiting route answers as an error body.
    Failed(JobFailure),
}

/// Why a job failed: the error body the old waiting route answers, and its
/// HTTP status (the key `status` is the tag of [`JobUpdate`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobFailure {
    pub code: String,
    pub message: String,
    /// The status of the old route's answer. Absent from a daemon that does
    /// not send it: the client then reads 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// What a database server said, for a `db_query` job only. Shown as
    /// data; never in `message`, which may be logged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `Event::Job`, and the answer of `GET /v1/jobs/{id}`: what job `id` did
/// last.
///
/// ```json
/// { "id": 4, "kind": "setup", "status": "running", "step": "started" }
/// { "id": 4, "kind": "setup", "status": "done", "result": "setup", "data": { "results": [] } }
/// { "id": 4, "kind": "setup", "status": "failed", "code": "internal_error", "message": "...", "http_status": 500 }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobEvent {
    /// From [`JobAccepted::id`].
    pub id: u64,
    pub kind: JobKind,
    #[serde(flatten)]
    pub update: JobUpdate,
}

impl JobEvent {
    /// Whether this is the last event of its job.
    pub fn is_last(&self) -> bool {
        !matches!(self.update, JobUpdate::Running { .. })
    }
}

/// `GET /v1/jobs`: the jobs that run now, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobList {
    pub jobs: Vec<JobEvent>,
}

/// `Event::Install`: what the install `id` did. The last event of each install
/// is `done` or `failed`, unless the daemon stops first (then
/// `shutting_down` ends the stream instead).
///
/// ```json
/// { "id": 1, "status": "running", "step": "downloading", "artifact": "cli", "done": 1, "total": 2 }
/// { "id": 1, "status": "failed", "code": "checksum_mismatch", "message": "..." }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallProgress {
    /// From [`InstallAccepted::id`].
    pub id: u64,
    #[serde(flatten)]
    pub update: InstallUpdate,
}

/// The `status` of an [`InstallProgress`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum InstallUpdate {
    /// A step of the installer.
    Running(InstallEvent),
    /// The install ended well.
    Done(InstallDone),
    /// The install ended with an error.
    Failed(InstallFailure),
}

/// The end of a successful install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallDone {
    pub runtime: Runtime,
    /// Full semver text (`8.4.23`).
    pub version: String,
    /// `runtimes/<runtime>/<version>`.
    pub dir: PathBuf,
    /// The version was there already and nothing was downloaded.
    pub already: bool,
}

/// Why an install failed: the error's stable `code`, an English `message` for
/// logs, and the facts a translated message may show. A fact the error does
/// not have is left out of the JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallFailure {
    /// For example `checksum_mismatch`. The UI picks its text by this.
    pub code: String,
    /// English, for logs. Never shown to the user as is.
    pub message: String,
    /// The version text that was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested: Option<String>,
    /// Versions in the manifest that could be asked for instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available: Vec<String>,
    /// A platform key, such as `darwin-arm64`, or "this system".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The code of the error under this one, such as the last mirror's error
    /// under `download_failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    /// What the installed program printed in its check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// For `install_os_too_old`. Boxed so that this type stays small.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<Box<OsMismatch>>,
}

/// The system version a program needs, and the one this system has. Both read
/// like `macOS 15.0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsMismatch {
    pub needs: String,
    pub found: String,
}
