//! The site list, and the error of a change to it.

mod build;

use std::error::Error;
use std::fmt;

use marsa_core::codes;
use marsa_core::config::ConfigError;

pub use build::build;

/// Why the site list could not be changed or built. The routes turn it into
/// an `HttpError` (500); the daemon's own code logs [`SitesError::code`].
#[derive(Debug)]
pub enum SitesError {
    /// `sites.toml` cannot be read or written: the request was fine, the
    /// file or the disk is not.
    Config(ConfigError),
    /// The blocking job panicked or was cancelled.
    JobFailed,
}

impl SitesError {
    /// Stable error name: the `config_*` code, or `internal_error`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Config(err) => err.code(),
            Self::JobFailed => codes::INTERNAL_ERROR,
        }
    }
}

impl fmt::Display for SitesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(err) => err.fmt(f),
            Self::JobFailed => f.write_str("the sites job failed"),
        }
    }
}

impl Error for SitesError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Config(err) => Some(err),
            Self::JobFailed => None,
        }
    }
}

impl From<ConfigError> for SitesError {
    fn from(err: ConfigError) -> Self {
        Self::Config(err)
    }
}
