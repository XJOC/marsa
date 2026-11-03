//! Install plans: verify the manifest signature, download to a temp folder,
//! verify the checksum, extract, then move atomically into place.
//!
//! One submodule per step. [`install`] runs them in order. The recipes they
//! read live in [`crate::recipes`].

pub mod execute;
pub mod extract;
pub mod fetch;
pub mod manifest;
pub mod plan;

use std::io;
use std::path::{Path, PathBuf};

use semver::Version;

use crate::recipes::{self, Platform, Recipe, RecipeError};
use crate::resolver::BadRequest;
use crate::runtimes::{Runtime, RuntimesError};
use crate::types::InstallEvent;
use extract::ExtractError;
use fetch::{Fetch, FetchError, HttpFetch};
use manifest::{Manifest, ManifestError};

/// Where the signed manifest is published (task M2-08). Its signature is the
/// same URL with `.minisig` added.
pub const MANIFEST_URL: &str =
    "https://github.com/XJOC/marsa/releases/download/runtimes-manifest/manifest.json";

/// What to install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallRequest<'a> {
    pub runtime: Runtime,
    /// `8.4`, `8.4.23`, or `None` for the newest stable version in the
    /// manifest. The same rules as a `.php-version` file.
    pub version: Option<&'a str>,
    /// Install again even if this version is already there.
    pub force: bool,
}

/// The result of [`install`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledVersion {
    pub runtime: Runtime,
    pub version: Version,
    /// `runtimes/<runtime>/<version>`.
    pub dir: PathBuf,
    /// The version was there already and nothing was downloaded.
    pub already: bool,
}

/// Everything that differs between a real install and a test. Outside this
/// crate the only constructor is [`InstallEnv::production`], so the manifest
/// key and URL cannot be swapped.
pub struct InstallEnv<F> {
    app_data_dir: PathBuf,
    fetch: F,
    manifest_url: String,
    platform: Option<Platform>,
    /// `None`: the built-in recipe of the runtime.
    recipe: Option<Recipe>,
    trust: Trust,
}

impl InstallEnv<HttpFetch> {
    /// The real downloader, the published manifest, the key built into the
    /// binary, this system's platform, and the built-in recipes.
    ///
    /// # Errors
    ///
    /// `download_failed` if the HTTP client cannot be set up.
    pub fn production(app_data_dir: &Path) -> Result<Self, InstallError> {
        Ok(InstallEnv {
            app_data_dir: app_data_dir.to_path_buf(),
            fetch: HttpFetch::new()?,
            manifest_url: MANIFEST_URL.to_owned(),
            platform: Platform::current(),
            recipe: None,
            trust: Trust::Embedded,
        })
    }
}

impl<F> InstallEnv<F> {
    fn recipe(&self, runtime: Runtime) -> Result<&Recipe, RecipeError> {
        match &self.recipe {
            Some(recipe) => Ok(recipe),
            None => recipes::builtin(runtime),
        }
    }
}

/// How the manifest's signature is checked. Outside this crate's own tests
/// only [`Trust::Embedded`] exists.
enum Trust {
    /// [`Manifest::verify_embedded`]: the key built into the binary.
    Embedded,
    /// The test key of the signed fixtures.
    #[cfg(test)]
    Key(minisign_verify::PublicKey),
    /// No signature check, for manifests that list archives a test builds.
    #[cfg(test)]
    Unsigned,
}

impl Trust {
    fn verify(
        &self,
        json: &[u8],
        minisig: &str,
        last_seen: Option<u64>,
    ) -> Result<Manifest, ManifestError> {
        match self {
            Trust::Embedded => Manifest::verify_embedded(json, minisig, last_seen),
            #[cfg(test)]
            Trust::Key(key) => Manifest::verify_and_parse(json, minisig, key, last_seen),
            #[cfg(test)]
            Trust::Unsigned => Manifest::unsigned_for_tests(json, last_seen),
        }
    }
}

/// Installs one version of a runtime into `runtimes/<runtime>/<version>/`.
///
/// The steps, in order: take the install lock, clean `runtimes/.tmp/`,
/// download and verify the manifest, save its serial, pick the version, then
/// download, verify, and unpack each file into a new temp folder, run the
/// recipe's check, and move the folder into place with one `rename`. The
/// version folder appears whole or not at all.
///
/// Writes only inside `runtimes/`. `events` gets the progress.
///
/// # Errors
///
/// Any [`InstallError`]; [`InstallError::code`] gives its stable name.
pub async fn install<F: Fetch>(
    req: &InstallRequest<'_>,
    env: &InstallEnv<F>,
    events: &mut (dyn FnMut(InstallEvent) + Send),
) -> Result<InstalledVersion, InstallError> {
    execute::run(req, env, events).await
}

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
