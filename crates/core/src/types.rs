//! Data types for API v1: requests, responses, and events (architecture.md
//! section 6).
//!
//! The daemon, the CLI, and the window all use these types, so the contract
//! cannot drift. Nothing here holds the daemon token: these values reach the UI.
//!
//! Times are milliseconds since the Unix epoch (`u64`), so no time crate is
//! needed.

use std::fmt;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

use crate::databases::ServerKind;
/// Defined once, in `doctor`, which judges them.
pub use crate::doctor::{Check as DoctorCheck, Verdict as DoctorVerdict, Warning as DoctorWarning};
/// Defined once, in `helper`, next to the request they answer.
pub use crate::helper::{OpName, OpResult, OpStatus};
use crate::php::PhpMinor;
use crate::recipes::Os;
/// Defined once, in `resolver`, which picks the version: `Site::php_source`.
pub use crate::resolver::Source;
use crate::runtimes::Runtime;
/// Defined once, in `sites`, where the daemon builds them.
pub use crate::sites::{Site, SiteKind};

/// The API version this build speaks. A client refuses a daemon whose API
/// version is newer than its own (architecture.md section 6, version handshake).
pub const API_VERSION: u32 = 1;

/// `GET /v1/health`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    /// The daemon's `CARGO_PKG_VERSION`. A client with another version restarts
    /// the daemon.
    pub app_version: String,
    pub api_version: u32,
    pub pid: u32,
}

/// The state of one managed process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ServiceState {
    Stopped,
    Starting,
    Running,
    Stopping,
    /// Stopped by an error. `code` is a stable error name, for example
    /// `port_in_use`, that the UI maps to a message.
    Failed {
        code: String,
    },
}

/// One managed process, as `GET /v1/services` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub id: ServiceId,
    #[serde(flatten)]
    pub state: ServiceState,
    /// The OS process id while the process runs.
    pub pid: Option<u32>,
    /// Restarts after a failure since the last start asked by the user.
    pub restarts: u32,
    /// When the process entered its current state.
    pub since_ms: Option<u64>,
}

/// A service `marsad` runs. On the wire it is its id text: `caddy`, `dns`,
/// `php-fpm-8.4` (`php-cgi-8.4` on Windows, ADR 0016), `postgres`, `mysql`,
/// or `valkey`. The id also names the log file `logs/<id>.log` and the pid
/// file `run/<id>.pid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ServiceId {
    /// The web server that serves every site.
    Caddy,
    /// The DNS server for `.test` names, inside the daemon (macOS, Linux).
    Dns,
    /// The PHP pool of one PHP line, which starts and stops with the sites
    /// that need it.
    Pool(PoolKind, PhpMinor),
    Database(ServerKind),
}

/// The program a PHP pool runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PoolKind {
    /// php-fpm: macOS and Linux.
    Fpm,
    /// php-cgi: Windows (ADR 0016).
    Cgi,
}

impl PoolKind {
    /// The program name, and the start of the pool's id.
    pub const fn program(self) -> &'static str {
        match self {
            Self::Fpm => "php-fpm",
            Self::Cgi => "php-cgi",
        }
    }
}

impl ServiceId {
    /// The pool of `minor` on the system `os`: php-cgi on Windows, php-fpm
    /// on the others.
    pub fn pool(os: Os, minor: PhpMinor) -> Self {
        let kind = match os {
            Os::Windows => PoolKind::Cgi,
            Os::Mac | Os::Linux | Os::Any => PoolKind::Fpm,
        };
        Self::Pool(kind, minor)
    }

    /// Whether this is a PHP pool, which the sites start and stop.
    pub fn is_pool(self) -> bool {
        matches!(self, Self::Pool(..))
    }
}

impl fmt::Display for ServiceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Caddy => f.write_str("caddy"),
            Self::Dns => f.write_str("dns"),
            Self::Pool(kind, minor) => write!(f, "{}-{minor}", kind.program()),
            Self::Database(kind) => f.write_str(kind.id()),
        }
    }
}

/// A service id that names no service of this build.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no service named {0:?}")]
pub struct UnknownService(pub String);

impl FromStr for ServiceId {
    type Err = UnknownService;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let unknown = || UnknownService(text.to_owned());
        match text {
            "caddy" => return Ok(Self::Caddy),
            "dns" => return Ok(Self::Dns),
            _ => {}
        }
        if let Some(kind) = ServerKind::from_id(text) {
            return Ok(Self::Database(kind));
        }
        let (program, line) = text.rsplit_once('-').ok_or_else(unknown)?;
        let kind = [PoolKind::Fpm, PoolKind::Cgi]
            .into_iter()
            .find(|kind| kind.program() == program)
            .ok_or_else(unknown)?;
        let minor = PhpMinor::parse(line).ok_or_else(unknown)?;
        let id = Self::Pool(kind, minor);
        // One text per id: `php-fpm-08.4` is not `php-fpm-8.4`.
        if id.to_string() == text {
            Ok(id)
        } else {
            Err(unknown())
        }
    }
}

impl Serialize for ServiceId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ServiceId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// The `{action}` in `POST /v1/services/{id}/{action}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
}

impl ServiceAction {
    /// The path segment: `start`, `stop`, or `restart`.
    pub fn as_str(self) -> &'static str {
        match self {
            ServiceAction::Start => "start",
            ServiceAction::Stop => "stop",
            ServiceAction::Restart => "restart",
        }
    }
}

impl fmt::Display for ServiceAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An `{action}` path segment that is not `start`, `stop`, or `restart`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown service action {0:?}")]
pub struct UnknownAction(pub String);

impl UnknownAction {
    /// Stable error name for the UI.
    pub fn code(&self) -> &'static str {
        crate::codes::UNKNOWN_ACTION
    }
}

impl FromStr for ServiceAction {
    type Err = UnknownAction;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "start" => Ok(ServiceAction::Start),
            "stop" => Ok(ServiceAction::Stop),
            "restart" => Ok(ServiceAction::Restart),
            other => Err(UnknownAction(other.to_owned())),
        }
    }
}

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
    /// The daemon is stopping; the stream ends after this event.
    ShuttingDown,
}

/// The inside of an error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    /// Stable error name, for example `service_not_found`. The UI shows a
    /// translated text chosen by this code.
    pub code: String,
    /// English, for logs. Never shown to the user as is.
    pub message: String,
}

/// Every error response: `{ "error": { "code": "...", "message": "..." } }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ApiError,
    /// What uses a version, with the codes `version_in_use` and
    /// `sites_lose_php` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_use: Option<VersionInUse>,
}

impl ErrorBody {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: ApiError {
                code: code.into(),
                message: message.into(),
            },
            in_use: None,
        }
    }
}

/// The progress of one runtime install. The daemon sends each
/// one on `/v1/events` inside [`InstallUpdate::Running`], and `marsa install`
/// prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum InstallEvent {
    /// The manifest's signature and serial passed.
    ManifestVerified {
        serial: u64,
    },
    /// The version picked from the manifest, as full semver text (`8.4.23`).
    Selected {
        runtime: Runtime,
        version: String,
    },
    /// Sent at most once per 256 KiB, and at the start and end of each file.
    Downloading {
        artifact: String,
        done: u64,
        total: u64,
    },
    /// The file's size and sha256 matched the manifest.
    Verified {
        artifact: String,
    },
    Extracting {
        artifact: String,
    },
    /// Running the recipe's check (`php -v`).
    Checking,
    /// The version folder is in place.
    Installed {
        dir: PathBuf,
    },
}

/// One runtime and its installed versions, in [`RuntimeList`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeVersions {
    pub runtime: Runtime,
    /// Full semver text (`8.4.23`), oldest first. Empty if none is installed.
    pub installed: Vec<String>,
    /// The default of PHP or Node as saved in `global.toml` (`8.4`), and
    /// left out of the JSON when there is none. Always `None` for Composer
    /// and Caddy: the newest installed version is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// `GET /v1/runtimes`: every runtime, installed or not, in a fixed order
/// ([`Runtime::ALL`]). Read from `runtimes/` on each call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeList {
    pub runtimes: Vec<RuntimeVersions>,
}

/// The body of `POST /v1/runtimes/{tool}/install`. `{}` is valid: the newest
/// stable version, without `force`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRequest {
    /// `8`, `8.4`, or `8.4.23`: the same rules as a `.php-version` file.
    /// `None`: the newest stable version in the manifest.
    #[serde(default)]
    pub version: Option<String>,
    /// Install again even if this version is already there.
    #[serde(default)]
    pub force: bool,
}

/// The body of `PUT /v1/runtimes/{tool}/default`, for PHP and
/// Node only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultRequest {
    /// `8.4` or `8.4.23` for PHP, `22` for Node: the rules of a
    /// `.php-version` file. Saved as written, without the spaces around it.
    /// An installed version must match it.
    pub version: String,
}

/// The answer to `PUT /v1/runtimes/{tool}/default`: the new default, and the
/// installed version it picks now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultVersion {
    pub runtime: Runtime,
    /// As saved in `global.toml` (`8.4`).
    pub default: String,
    /// Full semver text (`8.4.23`). A newer `8.4.x` installed later is
    /// picked then.
    pub version: String,
}

/// What an uninstall of a runtime version ends with: the version whose
/// folder is gone. The `done` of the job, and the answer of the old
/// `DELETE /v1/runtimes/{tool}/{version}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Uninstalled {
    pub runtime: Runtime,
    /// Full semver text (`8.3.12`).
    pub version: String,
}

/// Why an uninstall of a runtime version was refused: the facts
/// of its 409, with the code [`VersionInUse::code`]. At least one of the
/// last four is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionInUse {
    /// Full semver text (`8.3.12`).
    pub version: String,
    /// The default in `global.toml` (`8.3`), when it picks this version now
    /// and another version of the tool is installed. The default of the only
    /// installed version does not keep it: the uninstall clears that default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// The sites whose own setting in `sites.toml` picks this version now, by
    /// name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<String>,
    /// The Laravel and PHP sites that run this version now and would have no
    /// PHP after the uninstall, by name, whatever picked it for them (the
    /// default, a version file, `composer.json`). Set only when this is the
    /// only installed PHP, whether a default picks it or not. A
    /// site already in `sites` is not named again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites_left_without_php: Vec<String>,
    /// Caddy runs from this version now.
    #[serde(default)]
    pub caddy: bool,
    /// A database server runs from this version now: PostgreSQL.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub server: bool,
}

impl VersionInUse {
    /// Stable error name of the refusal. Never changes once released.
    /// `sites_lose_php` when the sites left without PHP are the only use, so
    /// the user knows that another version must be installed first;
    /// `version_in_use` otherwise.
    pub fn code(&self) -> &'static str {
        let nothing_else =
            self.default.is_none() && self.sites.is_empty() && !self.caddy && !self.server;
        if nothing_else && !self.sites_left_without_php.is_empty() {
            crate::codes::SITES_LOSE_PHP
        } else {
            crate::codes::VERSION_IN_USE
        }
    }

    /// Whether anything uses the version, so the uninstall is refused.
    pub fn any(&self) -> bool {
        self.default.is_some()
            || !self.sites.is_empty()
            || !self.sites_left_without_php.is_empty()
            || self.caddy
            || self.server
    }
}

/// The 202 answer to `POST /v1/runtimes/{tool}/install`. The install's events
/// on `/v1/events` carry the same `id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallAccepted {
    pub id: u64,
}

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

/// `GET /v1/sites`, and the answer to every change of the sites.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteList {
    /// The parked folders, in the order they were added.
    pub parked: Vec<PathBuf>,
    /// Every site, sorted by name.
    pub sites: Vec<Site>,
    /// The folders that did not become a site, so the UI can say why.
    pub skipped: Vec<SkippedFolder>,
}

/// A folder that did not become a site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedFolder {
    /// Shown as text only: a path that is not UTF-8 has `U+FFFD` in its place.
    pub path: PathBuf,
    /// `site_name_invalid`, `site_path_not_utf8`, or `site_name_taken` for a
    /// folder in a parked folder; `site_read_dir` for a parked folder that
    /// cannot be read; `site_not_a_folder` for a linked folder that is gone.
    pub code: String,
}

impl SkippedFolder {
    /// `path` as shown: JSON text must be UTF-8, and the path is only shown.
    pub fn new(path: &Path, code: &str) -> Self {
        Self {
            path: PathBuf::from(path.to_string_lossy().into_owned()),
            code: code.to_owned(),
        }
    }
}

/// The body of `POST /v1/sites/park`: every sub-folder of `path` becomes a
/// site. Also the body of `DELETE /v1/sites/park`: `path` is no
/// longer parked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParkRequest {
    /// An absolute folder.
    pub path: PathBuf,
}

/// The body of `POST /v1/sites/link`: `path` becomes the site `name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRequest {
    /// 1 to 63 of `a-z`, `0-9`, and `-`.
    pub name: String,
    /// An absolute folder.
    pub path: PathBuf,
}

/// The body of `PATCH /v1/sites/{name}`. A missing field keeps its value. For
/// `php` and `proxy`, `null` clears the value. `{}` is valid.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SitePatch {
    /// `8.4` or `8.4.23`, with the rules of a `.php-version` file. Cleared:
    /// the version comes from the project's files or the global default.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub php: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
    /// The port of the program that serves the site (`npm run dev`). Set: the
    /// site is a Node proxy. Cleared: the detected kind again.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub proxy: Option<Option<NonZeroU16>>,
}

/// For a field where `null` and a missing key differ. A missing key stays
/// `None` through `#[serde(default)]`; this gives `Some(None)` for `null` and
/// `Some(Some(value))` for a value.
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// The answer to `POST /v1/setup`, and the data of `setup_finished`.
///
/// Setup sends one request to `marsa-helper`, behind the admin prompt. On
/// macOS the daemon then asks the user to approve the trust in Marsa's
/// certificate authority for this user, in the dialog macOS shows (ADR
/// 0015). An operation it cannot do safely is left out of that request and
/// listed in `skipped`; the others still run. On Linux a second request may
/// follow, behind a second prompt: the operations of the
/// hosts file when the helper refused the `marsa0` link, or `hosts_remove`
/// once the link is made.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupReport {
    /// The helper's result for each operation it ran, in order, then, on
    /// macOS, the daemon's own `trust_install` result: the trust for the
    /// current user (ADR 0015). Empty if nothing ran: no operation was left,
    /// or `error` says why. On Linux the results of a second request follow
    /// those of the first, then the daemon's own `nss_install`: the CA root
    /// in the user's NSS databases, for Chrome and Firefox.
    #[serde(default)]
    pub results: Vec<OpResult>,
    /// The operations left out of the request, and why.
    #[serde(default)]
    pub skipped: Vec<SetupSkip>,
    /// The programs that listen on port 80 or 443 and take loopback
    /// connections. Any of them means no `pf_install`: the redirect would
    /// take their traffic without a word (ADR 0008, "Herd conflict").
    #[serde(default)]
    pub port_holders: Vec<PortHolder>,
    /// Why the helper did not run, or why its answer cannot be used:
    /// `setup_cancelled`, `setup_not_authorized`, `setup_timed_out`,
    /// `setup_interrupted`, `setup_helper_missing`, `setup_helper_unsafe`,
    /// `setup_helper_protected_folder`, `setup_prompt_failed`,
    /// `setup_helper_failed`, or `setup_helper_rejected`; on Linux also
    /// `setup_pkexec_missing`, `setup_caddy_ports_failed` when the helper
    /// allowed ports 80 and 443 but Caddy did not take them, and
    /// `setup_hosts_not_written` or `setup_hosts_not_removed` when the
    /// second request did not run (the results of the first stand). `None`
    /// if the helper answered, or was not needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

/// The answer to `GET /v1/hosts`: whether this system puts
/// `.test` names in its hosts file, and which sites Marsa's block there
/// lacks. `POST /v1/jobs/hosts-sync` adds them, behind the password prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostsStatus {
    /// `true` on Linux where this data folder's `marsa setup` put Marsa's
    /// block in the hosts file, and the block is still there. `false`
    /// before that setup, on the `marsa0` link, on macOS, and on Windows
    /// (its helper service keeps the block up to date by itself).
    pub used: bool,
    /// The host names of the sites that are not in the block, sorted. Such
    /// a site does not open until they are added. Empty when `used` is
    /// `false`.
    #[serde(default)]
    pub missing: Vec<String>,
}

/// An operation setup left out of the helper request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupSkip {
    /// The operation, for example `pf_install`.
    pub op: OpName,
    /// Why: `redirect_port_held` and `ports_not_checked` for `pf_install`,
    /// `ca_not_ready`, `ca_unreadable`, `caddy_not_installed`, and
    /// `caddy_not_started` for `trust_install`; on Linux
    /// `resolved_use_hosts` and `resolved_not_running` for
    /// `resolved_install`, `hosts_sites_unknown` and
    /// `hosts_too_many_names` for `hosts_sync`, and `nss_tool_missing` for
    /// `nss_install`.
    pub code: String,
}

/// A program that listens on port 80 or 443, from `netstat` and `ps`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortHolder {
    pub port: u16,
    /// As netstat prints it: `127.0.0.1`, `::1`, or `*` for every address.
    pub address: String,
    /// The process name netstat gives, at most 16 characters
    /// (`nginx-arm64`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// The user of the process, from `ps`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// The program from `ps`: a path, or the title the process set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The app it seems to belong to (`Herd`, `Docker`), a hint only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
}

/// `GET /v1/autostart`, and the answer to `PUT /v1/autostart`:
/// whether the tray icon starts at login. At most one of the two is `true`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutostartState {
    /// The login file is there and starts this Marsa's `marsa-tray`.
    pub enabled: bool,
    /// A login file is there, but not the one this Marsa writes: it starts a
    /// `marsa-tray` at another path (the app moved), or it was changed. Login
    /// may not start Marsa. `PUT` with `enabled: true` writes it again.
    #[serde(default)]
    pub stale: bool,
}

/// `GET /v1/doctor`: the checks of `marsa doctor`, as the daemon sees them.
/// The `PATH` check is not here: it judges a terminal's `PATH`, which only
/// the CLI has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    /// In the order `marsa doctor` prints them.
    pub lines: Vec<DoctorLine>,
    /// Things that work now but will cause trouble; they fail nothing.
    pub warnings: Vec<DoctorWarning>,
}

/// One check and what it found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorLine {
    pub check: DoctorCheck,
    pub verdict: DoctorVerdict,
}

/// The body of `PUT /v1/autostart`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutostartRequest {
    /// `true` writes the login file, `false` deletes it.
    pub enabled: bool,
}

/// The JSON of the autostart types. A module of its own, next to them, so
/// this task and the others that edit this file do not touch the same lines.
#[cfg(test)]
mod autostart_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn autostart_state_and_request() {
        let stale = AutostartState {
            enabled: false,
            stale: true,
        };
        assert_eq!(
            serde_json::to_value(stale).unwrap(),
            json!({ "enabled": false, "stale": true })
        );
        let back: AutostartState =
            serde_json::from_value(json!({ "enabled": false, "stale": true })).unwrap();
        assert_eq!(back, stale);
        // `stale` may be left out.
        let on: AutostartState = serde_json::from_value(json!({ "enabled": true })).unwrap();
        assert_eq!(
            on,
            AutostartState {
                enabled: true,
                stale: false
            }
        );

        let request = AutostartRequest { enabled: true };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({ "enabled": true })
        );
        for bad in [
            json!({}),
            json!({ "enabled": "yes" }),
            json!({ "enabled": 1 }),
        ] {
            assert!(serde_json::from_value::<AutostartRequest>(bad).is_err());
        }
    }
}

/// What `marsa uninstall --all` removed from the system: the `done` of the
/// uninstall job, and the answer of the old `POST /v1/uninstall`.
///
/// One admin prompt runs the helper's `uninstall_all`. Only if the helper
/// ran does the daemon go on: it takes away the current user's trust in
/// Marsa's certificate authority (macOS, ADR 0015) or deletes this data
/// folder's roots from the user's NSS databases (Linux, ADR 0014), deletes
/// the login file where the system has one (no `autostart_remove`
/// otherwise), answers, and then stops, with Caddy, the PHP pools, and the
/// DNS server. The data folder stays: the CLI asks about it on its own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UninstallReport {
    /// One result per step, in order: the helper's removals (`pf_remove`,
    /// `resolver_remove`, `trust_remove` on macOS; `ports_remove`,
    /// `resolved_remove`, `hosts_remove`, `trust_remove` on Linux), then the
    /// daemon's own `user_trust_remove` (macOS) or `nss_remove` (Linux), and
    /// `autostart_remove`. Empty when `error` says
    /// why nothing ran.
    #[serde(default)]
    pub results: Vec<OpResult>,
    /// Why the helper did not run, or why its answer cannot be used:
    /// `uninstall_cancelled`, `uninstall_not_authorized`,
    /// `uninstall_timed_out`, `uninstall_interrupted`,
    /// `uninstall_helper_missing`, `uninstall_helper_unsafe`,
    /// `uninstall_prompt_failed`, `uninstall_helper_failed`, or
    /// `uninstall_helper_rejected`; on Linux also `uninstall_pkexec_missing`.
    /// Nothing after the helper ran then, and the
    /// daemon keeps running. `None` if the helper answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
    /// The daemon stops right after this answer: `true` exactly when `error`
    /// is `None`.
    #[serde(default)]
    pub stopping: bool,
}

/// The JSON of [`UninstallReport`], next to it, like the autostart tests.
#[cfg(test)]
mod uninstall_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_uninstall_report_and_a_cancelled_one() {
        let report = UninstallReport {
            results: vec![OpResult {
                op: OpName::UserTrustRemove,
                status: OpStatus::Unchanged,
                code: None,
                message: None,
            }],
            error: None,
            stopping: true,
        };
        let value = json!({
            "results": [{ "op": "user_trust_remove", "status": "unchanged" }],
            "stopping": true,
        });
        assert_eq!(serde_json::to_value(&report).unwrap(), value);
        assert_eq!(
            serde_json::from_value::<UninstallReport>(value).unwrap(),
            report
        );

        let cancelled = UninstallReport {
            error: Some(ApiError {
                code: "uninstall_cancelled".into(),
                message: "the admin prompt was cancelled".into(),
            }),
            ..UninstallReport::default()
        };
        let value = json!({
            "results": [],
            "error": { "code": "uninstall_cancelled", "message": "the admin prompt was cancelled" },
            "stopping": false,
        });
        assert_eq!(serde_json::to_value(&cancelled).unwrap(), value);
        // Every field may be left out.
        assert_eq!(
            serde_json::from_value::<UninstallReport>(json!({})).unwrap(),
            UninstallReport::default()
        );
    }
}

/// `GET /v1/php/xdebug`, and the answer to `PUT /v1/php/xdebug`:
/// whether the `php.ini` files Marsa writes load Xdebug, and for which PHP.
///
/// Each PHP line (`8.4`) has one `php.ini`, so each list has at most one
/// version per line: the one its php-fpm runs, or the newest installed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct XdebugState {
    /// `[php] xdebug` in `global.toml`.
    pub enabled: bool,
    /// The PHP versions that load Xdebug, oldest first, in sites and in the
    /// terminal. Empty while `enabled` is `false`. Full semver text
    /// (`8.4.23`).
    #[serde(default)]
    pub loaded: Vec<String>,
    /// The PHP versions that have no `ext/xdebug.so` (the official Windows
    /// build, or an older install), oldest first. They run without Xdebug
    /// also while `enabled` is `true`.
    #[serde(default)]
    pub missing: Vec<String>,
}

/// The body of `PUT /v1/php/xdebug`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct XdebugRequest {
    /// `true` loads Xdebug in every PHP that has it, `false` in none.
    pub enabled: bool,
}

/// The JSON of the Xdebug types. A module of its own, like `autostart_tests`.
#[cfg(test)]
mod xdebug_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn xdebug_state_and_request() {
        let state = XdebugState {
            enabled: true,
            loaded: vec!["8.4.23".into()],
            missing: vec!["8.3.1".into()],
        };
        let text = json!({ "enabled": true, "loaded": ["8.4.23"], "missing": ["8.3.1"] });
        assert_eq!(serde_json::to_value(&state).unwrap(), text);
        assert_eq!(serde_json::from_value::<XdebugState>(text).unwrap(), state);
        // The lists may be left out.
        let off: XdebugState = serde_json::from_value(json!({ "enabled": false })).unwrap();
        assert_eq!(off, XdebugState::default());

        let request = XdebugRequest { enabled: true };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({ "enabled": true })
        );
        for bad in [json!({}), json!({ "enabled": "on" })] {
            assert!(serde_json::from_value::<XdebugRequest>(bad).is_err());
        }
    }
}

#[cfg(test)]
mod tests;
