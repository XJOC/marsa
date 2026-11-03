//! Install plans: verify the manifest signature, download to a temp folder,
//! verify the checksum, extract, then move atomically into place.
//!
//! One submodule per step, so the M2 tasks can work on them in parallel. The
//! recipes they read live in [`crate::recipes`].

pub mod execute;
pub mod extract;
pub mod fetch;
pub mod manifest;
pub mod plan;

use std::io;
use std::path::PathBuf;

use crate::recipes::RecipeError;
use crate::resolver::BadRequest;
use crate::runtimes::RuntimesError;
use extract::ExtractError;
use fetch::FetchError;
use manifest::ManifestError;

/// Why an install failed. [`InstallError::code`] gives a stable name that the
/// UI maps to a message. Errors of the steps keep their own codes.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("{what} has no build for {platform}")]
    UnsupportedPlatform { what: String, platform: String },

    #[error("another install is running")]
    Busy,

    #[error("the runtimes manifest has no {runtime} version for {requested:?}")]
    NoSuchVersion {
        runtime: &'static str,
        requested: String,
    },

    #[error(
        "the recipe and the manifest list different files for {what} \
         (not in the manifest: {missing:?}; not in the recipe: {extra:?})"
    )]
    ManifestMismatch {
        what: String,
        missing: Vec<String>,
        extra: Vec<String>,
    },

    /// A name that becomes a file name is empty, hidden, or has other
    /// characters than `[a-z0-9._-]`.
    #[error("refusing the file name {name:?}")]
    UnsafeName { name: String },

    /// `output` is the first 500 characters of what the program printed, or
    /// why it could not run.
    #[error("the installed program failed its check ({}): {output}", exit_text(*.exit_code))]
    CheckFailed {
        output: String,
        exit_code: Option<i32>,
    },

    #[error(
        "the installed program needs the Microsoft Visual C++ Redistributable (x64); \
         install it from https://aka.ms/vs/17/release/vc_redist.x64.exe"
    )]
    MissingVcRuntime,

    #[error("disk error at {}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error(transparent)]
    Manifest(#[from] ManifestError),

    #[error(transparent)]
    Fetch(#[from] FetchError),

    #[error(transparent)]
    Extract(#[from] ExtractError),

    #[error(transparent)]
    Version(#[from] BadRequest),

    #[error(transparent)]
    Recipe(#[from] RecipeError),

    #[error(transparent)]
    Runtimes(#[from] RuntimesError),
}

impl InstallError {
    /// Stable error name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            InstallError::UnsupportedPlatform { .. } => "install_unsupported_platform",
            InstallError::Busy => "install_busy",
            InstallError::NoSuchVersion { .. } => "install_no_such_version",
            InstallError::ManifestMismatch { .. } => "install_manifest_mismatch",
            InstallError::UnsafeName { .. } => "install_unsafe_name",
            InstallError::CheckFailed { .. } => "install_check_failed",
            InstallError::MissingVcRuntime => "install_missing_vc_runtime",
            InstallError::Io { .. } => "install_io",
            InstallError::Manifest(err) => err.code(),
            InstallError::Fetch(err) => err.code(),
            InstallError::Extract(err) => err.code(),
            InstallError::Version(err) => err.code(),
            InstallError::Recipe(err) => err.code(),
            InstallError::Runtimes(err) => err.code(),
        }
    }
}

fn exit_text(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("exit code {code}"),
        None => "no exit code".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_has_its_code() {
        let cases = [
            (
                InstallError::UnsupportedPlatform {
                    what: "php".into(),
                    platform: "linux-x64".into(),
                },
                "install_unsupported_platform",
            ),
            (InstallError::Busy, "install_busy"),
            (
                InstallError::NoSuchVersion {
                    runtime: "php",
                    requested: "8.2".into(),
                },
                "install_no_such_version",
            ),
            (
                InstallError::ManifestMismatch {
                    what: "php 8.4.23".into(),
                    missing: vec!["fpm".into()],
                    extra: vec![],
                },
                "install_manifest_mismatch",
            ),
            (
                InstallError::UnsafeName { name: ".x".into() },
                "install_unsafe_name",
            ),
            (
                InstallError::CheckFailed {
                    output: "PHP 8.4.22".into(),
                    exit_code: Some(0),
                },
                "install_check_failed",
            ),
            (InstallError::MissingVcRuntime, "install_missing_vc_runtime"),
            (
                InstallError::Io {
                    path: PathBuf::from("x"),
                    source: io::Error::other("full"),
                },
                "install_io",
            ),
            (ManifestError::BadSignature.into(), "manifest_bad_signature"),
            (BadRequest::Alias.into(), "unsupported_alias"),
            (
                ExtractError::Duplicate("a".into()).into(),
                "extract_duplicate",
            ),
        ];
        for (err, code) in cases {
            assert_eq!(err.code(), code, "{err}");
        }
    }
}
