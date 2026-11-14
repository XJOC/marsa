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
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

/// Defined once, in `helper`, next to the request they answer.
pub use crate::helper::{OpResult, OpStatus};
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
    /// The whole new site list: after each park, link, or patch, and after a
    /// rebuild that found a different list.
    SitesChanged(SiteList),
    /// The default version of PHP or Node, after `PUT
    /// /v1/runtimes/{tool}/default` saved it or the first install set it
    /// (task M5-02). A changed site list follows as `sites_changed`.
    DefaultChanged(DefaultVersion),
    /// The end of a `POST /v1/setup`, with the same report as its answer.
    SetupFinished(SetupReport),
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
    /// What uses a version, with the code `version_in_use` only (task M5-07).
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
    /// The default of PHP or Node as saved in `global.toml` (`8.4`), and
    /// left out of the JSON when there is none. Always `None` for Composer
    /// and Caddy: the newest installed version is used (task M5-02).
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

/// The body of `PUT /v1/runtimes/{tool}/default` (task M5-01), for PHP and
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

/// The answer to `DELETE /v1/runtimes/{tool}/{version}` (task M5-07): the
/// version whose folder is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Uninstalled {
    pub runtime: Runtime,
    /// Full semver text (`8.3.12`).
    pub version: String,
}

/// Why `DELETE /v1/runtimes/{tool}/{version}` refused a version: the facts
/// of its 409 `version_in_use`. At least one of the last three is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionInUse {
    /// Full semver text (`8.3.12`).
    pub version: String,
    /// The default in `global.toml` (`8.3`), when it picks this version now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// The sites whose own setting in `sites.toml` picks this version now, by
    /// name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<String>,
    /// Caddy runs from this version now.
    #[serde(default)]
    pub caddy: bool,
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

/// The body of `POST /v1/sites/park`: every sub-folder of `path` becomes a
/// site. Also the body of `DELETE /v1/sites/park` (task M5-09): `path` is no
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

/// The answer to `POST /v1/setup`, and the data of `setup_finished` (task
/// M4-13).
///
/// Setup sends one request to `marsa-helper`, behind the admin prompt. On
/// macOS the daemon then asks the user to approve the trust in Marsa's
/// certificate authority for this user, in the dialog macOS shows (ADR
/// 0015). An operation it cannot do safely is left out of that request and
/// listed in `skipped`; the others still run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupReport {
    /// The helper's result for each operation it ran, in order, then, on
    /// macOS, the daemon's own `trust_install` result: the trust for the
    /// current user (ADR 0015). Empty if nothing ran: no operation was left,
    /// or `error` says why.
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
    /// `setup_helper_protected_folder`, `setup_helper_failed`, or
    /// `setup_helper_rejected`. `None` if the helper answered, or was not
    /// needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

/// An operation setup left out of the helper request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupSkip {
    /// The helper operation, for example `pf_install`.
    pub op: String,
    /// Why: `redirect_port_held` and `ports_not_checked` for `pf_install`,
    /// `ca_not_ready` and `ca_unreadable` for `trust_install`.
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

/// `GET /v1/autostart`, and the answer to `PUT /v1/autostart` (task M5-03):
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

/// `GET /v1/php/xdebug`, and the answer to `PUT /v1/php/xdebug` (task M5-11):
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
        round_trip(
            &Event::DefaultChanged(DefaultVersion {
                runtime: Runtime::Php,
                default: "8.3".into(),
                version: "8.3.27".into(),
            }),
            json!({
                "type": "default_changed",
                "data": { "runtime": "php", "default": "8.3", "version": "8.3.27" },
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
                    default: Some("8.4".into()),
                },
                RuntimeVersions {
                    runtime: Runtime::Composer,
                    installed: vec![],
                    default: None,
                },
            ],
        };
        round_trip(
            &list,
            json!({ "runtimes": [
                { "runtime": "php", "installed": ["8.3.27", "8.4.23"], "default": "8.4" },
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
    fn default_request_and_answer() {
        round_trip(
            &DefaultRequest {
                version: "8.3".into(),
            },
            json!({ "version": "8.3" }),
        );
        // The version is required: there is no default for the default.
        assert!(serde_json::from_value::<DefaultRequest>(json!({})).is_err());
        round_trip(
            &DefaultVersion {
                runtime: Runtime::Node,
                default: "22".into(),
                version: "22.11.0".into(),
            },
            json!({ "runtime": "node", "default": "22", "version": "22.11.0" }),
        );
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

    #[test]
    fn site_list_in_a_sites_changed_event() {
        let list = SiteList {
            parked: vec![PathBuf::from("/code")],
            sites: vec![Site {
                name: "shop".into(),
                root: PathBuf::from("/code/shop"),
                docroot: PathBuf::from("/code/shop/public"),
                kind: SiteKind::Laravel,
                php: Some(semver::Version::new(8, 4, 23)),
                php_source: Some(Source::GlobalDefault),
                secure: true,
            }],
            skipped: vec![SkippedFolder {
                path: PathBuf::from("/code/متجر"),
                code: "site_name_invalid".into(),
            }],
        };
        round_trip(
            &Event::SitesChanged(list),
            json!({
                "type": "sites_changed",
                "data": {
                    "parked": ["/code"],
                    "sites": [{
                        "name": "shop", "root": "/code/shop", "docroot": "/code/shop/public",
                        "kind": { "type": "laravel" }, "php": "8.4.23",
                        "php_source": { "type": "global_default" }, "secure": true,
                    }],
                    "skipped": [{ "path": "/code/متجر", "code": "site_name_invalid" }],
                },
            }),
        );
    }

    #[test]
    fn park_and_link_requests() {
        round_trip(
            &ParkRequest {
                path: PathBuf::from("/code"),
            },
            json!({ "path": "/code" }),
        );
        round_trip(
            &LinkRequest {
                name: "api".into(),
                path: PathBuf::from("/work/api"),
            },
            json!({ "name": "api", "path": "/work/api" }),
        );
    }

    #[test]
    fn a_site_patch_tells_null_from_a_missing_field() {
        round_trip(&SitePatch::default(), json!({}));
        round_trip(
            &SitePatch {
                php: Some(Some("8.3".into())),
                secure: Some(false),
                proxy: Some(NonZeroU16::new(5173)),
            },
            json!({ "php": "8.3", "secure": false, "proxy": 5173 }),
        );
        round_trip(
            &SitePatch {
                php: Some(None),
                secure: None,
                proxy: Some(None),
            },
            json!({ "php": null, "proxy": null }),
        );
        // `null` for `secure` keeps it, like a missing field.
        let patch: SitePatch = serde_json::from_value(json!({ "secure": null })).unwrap();
        assert_eq!(patch, SitePatch::default());
        for bad in [json!({ "proxy": 0 }), json!({ "proxy": 70000 })] {
            assert!(serde_json::from_value::<SitePatch>(bad).is_err());
        }
    }

    #[test]
    fn a_setup_report_in_a_setup_finished_event() {
        let report = SetupReport {
            results: vec![OpResult {
                op: "resolver_install".into(),
                status: OpStatus::Done,
                code: None,
                message: None,
            }],
            skipped: vec![SetupSkip {
                op: "pf_install".into(),
                code: "redirect_port_held".into(),
            }],
            port_holders: vec![PortHolder {
                port: 80,
                address: "127.0.0.1".into(),
                process: Some("nginx-arm64".into()),
                pid: Some(15464),
                user: None,
                command: None,
                app: Some("Herd".into()),
            }],
            error: None,
        };
        round_trip(
            &Event::SetupFinished(report),
            json!({
                "type": "setup_finished",
                "data": {
                    "results": [{ "op": "resolver_install", "status": "done" }],
                    "skipped": [{ "op": "pf_install", "code": "redirect_port_held" }],
                    "port_holders": [{
                        "port": 80, "address": "127.0.0.1", "process": "nginx-arm64",
                        "pid": 15464, "app": "Herd",
                    }],
                },
            }),
        );
        let failed = SetupReport {
            error: Some(ApiError {
                code: "setup_cancelled".into(),
                message: "the admin prompt was cancelled".into(),
            }),
            ..SetupReport::default()
        };
        round_trip(
            &failed,
            json!({
                "results": [], "skipped": [], "port_holders": [],
                "error": { "code": "setup_cancelled", "message": "the admin prompt was cancelled" },
            }),
        );
    }
}
