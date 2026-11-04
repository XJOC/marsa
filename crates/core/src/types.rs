//! Data types for API v1: requests, responses, and events (architecture.md
//! section 6).
//!
//! The daemon, the CLI, and the window all use these types, so the contract
//! cannot drift. Nothing here holds the daemon token: these values reach the UI.
//!
//! Times are milliseconds since the Unix epoch (`u64`), so no time crate is
//! needed.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::runtimes::Runtime;

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
    /// For example `caddy`.
    pub id: String,
    #[serde(flatten)]
    pub state: ServiceState,
    /// The OS process id while the process runs.
    pub pid: Option<u32>,
    /// Restarts after a failure since the last start asked by the user.
    pub restarts: u32,
    /// When the process entered its current state.
    pub since_ms: Option<u64>,
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
        "unknown_action"
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
    pub service: String,
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
}

impl ErrorBody {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: ApiError {
                code: code.into(),
                message: message.into(),
            },
        }
    }
}

/// The progress of one runtime install (task M2-05). The daemon sends each
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
}

/// `GET /v1/runtimes`: every runtime, installed or not, in a fixed order
/// (PHP, Node.js, Composer). Read from `runtimes/` on each call.
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

/// The 202 answer to `POST /v1/runtimes/{tool}/install`. The install's events
/// on `/v1/events` carry the same `id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallAccepted {
    pub id: u64,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    /// The value serializes to `expected` exactly, and `expected` reads back
    /// into the same value.
    fn round_trip<T>(value: &T, expected: Value)
    where
        T: Serialize + DeserializeOwned + PartialEq + fmt::Debug,
    {
        assert_eq!(serde_json::to_value(value).unwrap(), expected);
        let back: T = serde_json::from_value(expected).unwrap();
        assert_eq!(&back, value);
    }

    fn running_caddy() -> ServiceStatus {
        ServiceStatus {
            id: "caddy".into(),
            state: ServiceState::Running,
            pid: Some(4242),
            restarts: 0,
            since_ms: Some(1_762_000_000_000),
        }
    }

    #[test]
    fn health() {
        let health = Health {
            app_version: "0.1.0".into(),
            api_version: API_VERSION,
            pid: 7,
        };
        round_trip(
            &health,
            json!({ "app_version": "0.1.0", "api_version": 1, "pid": 7 }),
        );
    }

    #[test]
    fn service_status_running() {
        round_trip(
            &running_caddy(),
            json!({
                "id": "caddy",
                "state": "running",
                "pid": 4242,
                "restarts": 0,
                "since_ms": 1_762_000_000_000_u64,
            }),
        );
    }

    #[test]
    fn service_status_failed_carries_the_code() {
        let status = ServiceStatus {
            id: "caddy".into(),
            state: ServiceState::Failed {
                code: "port_in_use".into(),
            },
            pid: None,
            restarts: 3,
            since_ms: None,
        };
        round_trip(
            &status,
            json!({
                "id": "caddy",
                "state": "failed",
                "code": "port_in_use",
                "pid": null,
                "restarts": 3,
                "since_ms": null,
            }),
        );
    }

    #[test]
    fn every_plain_state_is_snake_case() {
        for (state, name) in [
            (ServiceState::Stopped, "stopped"),
            (ServiceState::Starting, "starting"),
            (ServiceState::Running, "running"),
            (ServiceState::Stopping, "stopping"),
        ] {
            round_trip(&state, json!({ "state": name }));
        }
    }

    #[test]
    fn service_action_parses_and_prints_path_segments() {
        for action in [
            ServiceAction::Start,
            ServiceAction::Stop,
            ServiceAction::Restart,
        ] {
            assert_eq!(action.as_str().parse::<ServiceAction>(), Ok(action));
            assert_eq!(action.to_string(), action.as_str());
            round_trip(&action, json!(action.as_str()));
        }
    }

    #[test]
    fn service_action_rejects_unknown_and_wrong_case() {
        for bad in ["kill", "Start", "", "start "] {
            let err = bad.parse::<ServiceAction>().unwrap_err();
            assert_eq!(err, UnknownAction(bad.to_owned()));
            assert_eq!(err.code(), "unknown_action");
        }
    }

    #[test]
    fn log_line() {
        let line = LogLine {
            service: "caddy".into(),
            seq: 12,
            at_ms: 5,
            stream: LogStream::Stderr,
            text: "serving initial configuration".into(),
        };
        round_trip(
            &line,
            json!({
                "service": "caddy",
                "seq": 12,
                "at_ms": 5,
                "stream": "stderr",
                "text": "serving initial configuration",
            }),
        );
    }

    #[test]
    fn events_are_tagged() {
        round_trip(
            &Event::ServiceChanged(running_caddy()),
            json!({
                "type": "service_changed",
                "data": {
                    "id": "caddy",
                    "state": "running",
                    "pid": 4242,
                    "restarts": 0,
                    "since_ms": 1_762_000_000_000_u64,
                },
            }),
        );
        round_trip(
            &Event::Log(LogLine {
                service: "caddy".into(),
                seq: 0,
                at_ms: 1,
                stream: LogStream::Stdout,
                text: "hi".into(),
            }),
            json!({
                "type": "log",
                "data": { "service": "caddy", "seq": 0, "at_ms": 1, "stream": "stdout", "text": "hi" },
            }),
        );
        round_trip(&Event::ShuttingDown, json!({ "type": "shutting_down" }));
    }

    #[test]
    fn error_body() {
        round_trip(
            &ErrorBody::new("service_not_found", "no service named php"),
            json!({ "error": { "code": "service_not_found", "message": "no service named php" } }),
        );
    }

    #[test]
    fn install_events() {
        round_trip(
            &InstallEvent::Selected {
                runtime: Runtime::Php,
                version: "8.4.23".into(),
            },
            json!({ "step": "selected", "runtime": "php", "version": "8.4.23" }),
        );
        round_trip(
            &InstallEvent::Downloading {
                artifact: "cli".into(),
                done: 1,
                total: 2,
            },
            json!({ "step": "downloading", "artifact": "cli", "done": 1, "total": 2 }),
        );
        round_trip(&InstallEvent::Checking, json!({ "step": "checking" }));
    }

    #[test]
    fn runtime_list() {
        let list = RuntimeList {
            runtimes: vec![
                RuntimeVersions {
                    runtime: Runtime::Php,
                    installed: vec!["8.3.27".into(), "8.4.23".into()],
                },
                RuntimeVersions {
                    runtime: Runtime::Composer,
                    installed: vec![],
                },
            ],
        };
        round_trip(
            &list,
            json!({ "runtimes": [
                { "runtime": "php", "installed": ["8.3.27", "8.4.23"] },
                { "runtime": "composer", "installed": [] },
            ] }),
        );
    }

    #[test]
    fn install_request_and_accepted() {
        round_trip(
            &InstallRequest {
                version: Some("8.4".into()),
                force: false,
            },
            json!({ "version": "8.4", "force": false }),
        );
        // Both fields may be left out.
        let empty: InstallRequest = serde_json::from_value(json!({})).unwrap();
        assert_eq!(empty, InstallRequest::default());
        round_trip(&InstallAccepted { id: 7 }, json!({ "id": 7 }));
    }

    #[test]
    fn install_progress_is_one_flat_object_in_an_install_event() {
        let step = |update| Event::Install(InstallProgress { id: 3, update });
        round_trip(
            &step(InstallUpdate::Running(InstallEvent::Downloading {
                artifact: "cli".into(),
                done: 1,
                total: 2,
            })),
            json!({
                "type": "install",
                "data": {
                    "id": 3, "status": "running",
                    "step": "downloading", "artifact": "cli", "done": 1, "total": 2,
                },
            }),
        );
        round_trip(
            &step(InstallUpdate::Running(InstallEvent::Checking)),
            json!({ "type": "install", "data": { "id": 3, "status": "running", "step": "checking" } }),
        );
        round_trip(
            &step(InstallUpdate::Done(InstallDone {
                runtime: Runtime::Node,
                version: "22.11.0".into(),
                dir: PathBuf::from("/d/runtimes/node/22.11.0"),
                already: false,
            })),
            json!({
                "type": "install",
                "data": {
                    "id": 3, "status": "done", "runtime": "node", "version": "22.11.0",
                    "dir": "/d/runtimes/node/22.11.0", "already": false,
                },
            }),
        );
    }

    #[test]
    fn a_failed_install_carries_its_code_and_only_the_facts_it_has() {
        let failed = |failure| {
            Event::Install(InstallProgress {
                id: 9,
                update: InstallUpdate::Failed(failure),
            })
        };
        round_trip(
            &failed(InstallFailure {
                code: "install_busy".into(),
                message: "another install is running".into(),
                ..InstallFailure::default()
            }),
            json!({
                "type": "install",
                "data": {
                    "id": 9, "status": "failed",
                    "code": "install_busy", "message": "another install is running",
                },
            }),
        );
        round_trip(
            &failed(InstallFailure {
                code: "install_no_such_version".into(),
                message: "no match".into(),
                requested: Some("8.9".into()),
                available: vec!["8.3.27".into(), "8.4.23".into()],
                ..InstallFailure::default()
            }),
            json!({
                "type": "install",
                "data": {
                    "id": 9, "status": "failed",
                    "code": "install_no_such_version", "message": "no match",
                    "requested": "8.9", "available": ["8.3.27", "8.4.23"],
                },
            }),
        );
    }
}
