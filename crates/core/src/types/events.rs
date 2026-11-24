//! Log lines and the event stream of `GET /v1/events`.

use serde::{Deserialize, Serialize};

use super::{
    DefaultVersion, InstallProgress, JobEvent, ServiceId, ServiceStatus, SetupReport, SiteList,
};

/// Which output a log line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    Stdout,
    Stderr,
}

/// One line of a managed process's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    pub service: ServiceId,
    /// Grows by one per line of this service, so a client can find gaps.
    pub seq: u64,
    pub at_ms: u64,
    pub stream: LogStream,
    /// The line without its line ending.
    pub text: String,
}

/// Everything `GET /v1/events` sends. Each change is published once and
/// reaches the CLI and the window the same way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Event {
    ServiceChanged(ServiceStatus),
    Log(LogLine),
    /// A step of an install that `POST /v1/runtimes/{tool}/install` started.
    Install(InstallProgress),
    /// The whole new site list: after each park, link, or patch, and after a
    /// rebuild that found a different list.
    SitesChanged(SiteList),
    /// The default version of PHP or Node, after `PUT
    /// /v1/runtimes/{tool}/default` saved it or the first install set it.
    /// A changed site list follows as `sites_changed`.
    DefaultChanged(DefaultVersion),
    /// The end of a setup, with the same report as the job's `done` and as the
    /// answer of the old `POST /v1/setup`.
    SetupFinished(SetupReport),
    /// A step or the end of a long operation that `POST /v1/jobs/...`
    /// started (ADR 0026).
    Job(JobEvent),
    /// The dump server kept a new dump, or the list was cleared. At most a
    /// few per second, however many dumps arrive (ADR 0027). A client asks
    /// `GET /v1/dumps?after=<its latest id>` for the new ones.
    DumpsChanged {
        /// The id of the newest dump ever kept.
        latest_id: u64,
        /// How many dumps the daemon holds now.
        count: u64,
    },
    /// The daemon is stopping; the stream ends after this event.
    ShuttingDown,
}
