//! [`ActionError`]: why an action on a database server failed.

use marsa_core::codes;
use std::fmt;

use marsa_core::config::ConfigError;
use marsa_core::databases::{ServerKind, ServiceBusy, UnwritablePath};
use marsa_core::services::ProgramNotAbsolute;

use super::super::init::InitError;
use super::super::orphan;

/// Why an action on a server failed. A start that fails also sets the
/// status `Failed` with the same code, and says why in its recent lines.
#[derive(Debug)]
pub enum ActionError {
    /// Marsa has no build of this server for this system.
    Unsupported(ServerKind),
    /// Windows: `marsad` runs with an administrator's token, which the
    /// server refuses (PostgreSQL). Nothing was started.
    Elevated(ServerKind),
    /// `global.toml` cannot be read.
    Config(ConfigError),
    /// No installed version fits the `default` of its table.
    NotInstalled(ServerKind, String),
    Init(InitError),
    /// The check before the start refused: another program holds the port,
    /// or a server runs on the data folder. Nothing was started or stopped.
    InUse(ServiceBusy),
    /// Another action on the server has not finished.
    Busy,
    ProgramNotAbsolute(ProgramNotAbsolute),
    /// The job that writes `enabled` failed (a bug).
    Internal,
    /// Our old server, left by a crashed `marsad`, could not be stopped, or
    /// cannot be told from another program.
    OrphanStuck(ServerKind, String),
    /// A path of the data folder cannot go in MySQL's `my.cnf` or Valkey's
    /// `valkey.conf`.
    Path(UnwritablePath),
}

impl ActionError {
    /// Stable error name for the UI.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unsupported(ServerKind::Postgres) => codes::POSTGRES_UNSUPPORTED_PLATFORM,
            Self::Unsupported(ServerKind::Mysql) => codes::MYSQL_UNSUPPORTED_PLATFORM,
            Self::Unsupported(ServerKind::Valkey) => codes::VALKEY_UNSUPPORTED_PLATFORM,
            Self::Unsupported(ServerKind::Mailpit) => codes::MAILPIT_UNSUPPORTED_PLATFORM,
            Self::Elevated(_) => codes::SERVICE_ELEVATED,
            Self::Config(err) => err.code(),
            Self::NotInstalled(ServerKind::Postgres, _) => codes::POSTGRES_NOT_INSTALLED,
            Self::NotInstalled(ServerKind::Mysql, _) => codes::MYSQL_NOT_INSTALLED,
            Self::NotInstalled(ServerKind::Valkey, _) => codes::VALKEY_NOT_INSTALLED,
            Self::NotInstalled(ServerKind::Mailpit, _) => codes::MAILPIT_NOT_INSTALLED,
            Self::Init(err) => err.code(),
            Self::InUse(busy) => busy.code(),
            Self::Busy => codes::SERVICE_BUSY,
            Self::ProgramNotAbsolute(err) => err.code(),
            Self::Internal => codes::INTERNAL_ERROR,
            Self::OrphanStuck(kind, _) => orphan::stuck_code(*kind),
            Self::Path(err) => err.code(),
        }
    }
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(kind) => {
                write!(f, "Marsa has no {} for this system", kind.name())
            }
            Self::Elevated(kind) => write!(
                f,
                "marsad runs with administrator rights, and {} refuses to run with them; run \
                 Marsa without administrator rights",
                kind.name()
            ),
            Self::Config(err) => err.fmt(f),
            Self::NotInstalled(_, reason) => f.write_str(reason),
            Self::Init(err) => err.fmt(f),
            Self::InUse(busy) => busy.fmt(f),
            Self::Busy => f.write_str("another action on this service is running"),
            Self::ProgramNotAbsolute(err) => err.fmt(f),
            Self::Internal => f.write_str("the job that writes enabled to global.toml failed"),
            Self::OrphanStuck(_, reason) => f.write_str(reason),
            Self::Path(err) => err.fmt(f),
        }
    }
}
