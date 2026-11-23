//! Runtime installs and versions: the events and bodies of the runtime endpoints.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::runtimes::Runtime;

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
