//! The site lists and the bodies that change them.

use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

use super::{Site, WorkerKind};

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
    /// Run `php artisan queue:work` for the site, or stop it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<bool>,
    /// Run `php artisan schedule:work` for the site, or stop it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<bool>,
}

impl SitePatch {
    /// Turns worker `kind` of the site on or off, and keeps every other
    /// setting: `{"queue": true}`.
    pub fn worker(kind: WorkerKind, on: bool) -> Self {
        let mut patch = Self::default();
        match kind {
            WorkerKind::Queue => patch.queue = Some(on),
            WorkerKind::Schedule => patch.schedule = Some(on),
        }
        patch
    }
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
