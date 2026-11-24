//! Data types for API v1: requests, responses, and events (architecture.md
//! section 6).
//!
//! The daemon, the CLI, and the window all use these types, so the contract
//! cannot drift. Nothing here holds the daemon token: these values reach the UI.
//!
//! Times are milliseconds since the Unix epoch (`u64`), so no time crate is
//! needed.

use serde::{Deserialize, Serialize};

/// Defined once, in `doctor`, which judges them.
pub use crate::doctor::{Check as DoctorCheck, Verdict as DoctorVerdict, Warning as DoctorWarning};
/// Defined once, in `helper`, next to the request they answer.
pub use crate::helper::{OpName, OpResult, OpStatus};
/// Defined once, in `resolver`, which picks the version: `Site::php_source`.
pub use crate::resolver::Source;
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

mod autostart;
mod doctor;
mod dumps;
mod errors;
mod events;
mod jobs;
mod mail;
mod runtimes;
mod services;
mod setup;
mod sites;
mod uninstall;
mod xdebug;

pub use autostart::*;
pub use doctor::*;
pub use dumps::*;
pub use errors::*;
pub use events::*;
pub use jobs::*;
pub use mail::*;
pub use runtimes::*;
pub use services::*;
pub use setup::*;
pub use sites::*;
pub use uninstall::*;
pub use xdebug::*;

#[cfg(test)]
mod tests;
