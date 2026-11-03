//! Where installed runtimes live, and which versions are there.
//!
//! Each version has its own folder (architecture.md section 7):
//!
//! ```text
//! <app data dir>/runtimes/
//! ├── php/8.4.3/bin/php
//! ├── node/22.11.0/bin/node
//! └── composer/2.8.4/composer.phar
//! ```
//!
//! The list of installed versions is the folder names that are valid
//! `semver` versions. [`parse_installed`] is the pure part; [`RuntimesDir::installed`]
//! reads the folder.
//!
//! This module is not behind the `full` feature: the shim reads it.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use semver::Version;

use crate::resolver::Tool;

/// A program that has its own folder under `runtimes/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
    Php,
    Node,
    /// `composer.phar`. Its versions do not follow PHP's, and one copy serves
    /// every PHP version.
    Composer,
}

impl Runtime {
    /// The folder name under `runtimes/`.
    pub fn dir_name(self) -> &'static str {
        match self {
            Runtime::Php => "php",
            Runtime::Node => "node",
            Runtime::Composer => "composer",
        }
    }
}

impl From<Tool> for Runtime {
    fn from(tool: Tool) -> Self {
        match tool {
            Tool::Php => Runtime::Php,
            Tool::Node => Runtime::Node,
        }
    }
}

/// The `runtimes/` folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimesDir {
    root: PathBuf,
}

impl RuntimesDir {
    /// `<app_data_dir>/runtimes`.
    pub fn in_dir(app_data_dir: &Path) -> Self {
        Self {
            root: app_data_dir.join("runtimes"),
        }
    }

    /// `runtimes/`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `runtimes/<runtime>/`, for example `runtimes/php`.
    pub fn runtime_dir(&self, runtime: Runtime) -> PathBuf {
        self.root.join(runtime.dir_name())
    }

    /// `runtimes/<runtime>/<version>/`, for example `runtimes/php/8.4.3`.
    ///
    /// Pass a version from [`RuntimesDir::installed`], never one built from
    /// user text: then a value like `../../x` cannot reach the path.
    pub fn version_dir(&self, runtime: Runtime, version: &Version) -> PathBuf {
        self.runtime_dir(runtime).join(version.to_string())
    }

    /// The installed versions of `runtime`, oldest first.
    ///
    /// Skipped: names that are not a full `semver` version (`8.4`, `.tmp`,
    /// `abc`), names that are not UTF-8, and entries that are not folders. A
    /// symlink to a folder counts as a folder.
    ///
    /// # Errors
    ///
    /// `runtimes_read` if the folder exists but cannot be read. A missing
    /// folder is not an error: it gives an empty list.
    pub fn installed(&self, runtime: Runtime) -> Result<Vec<Version>, RuntimesError> {
        let dir = self.runtime_dir(runtime);
        let read_error = |path: &Path| {
            let path = path.to_path_buf();
            move |source| RuntimesError::Read { path, source }
        };

        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(read_error(&dir)(err)),
        };

        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(read_error(&dir))?;
            let file_type = entry.file_type().map_err(read_error(&entry.path()))?;
            // `file_type` does not follow symlinks; one extra `stat` only for links.
            // A broken link is skipped like any other non-folder.
            let is_dir = file_type.is_dir() || (file_type.is_symlink() && entry.path().is_dir());
            if is_dir {
                names.push(entry.file_name());
            }
        }
        Ok(parse_installed(names))
    }
}

/// Turns folder names into versions, oldest first. Names that are not UTF-8 or
/// not a full `semver` version are skipped.
pub fn parse_installed<I>(names: I) -> Vec<Version>
where
    I: IntoIterator,
    I::Item: AsRef<OsStr>,
{
    let mut versions: Vec<Version> = names
        .into_iter()
        .filter_map(|name| name.as_ref().to_str().and_then(|s| Version::parse(s).ok()))
        .collect();
    versions.sort();
    versions
}

/// Why `runtimes/` could not be read. [`RuntimesError::code`] gives a stable
/// name that the UI maps to a message.
#[derive(Debug, thiserror::Error)]
pub enum RuntimesError {
    #[error("cannot read {}", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl RuntimesError {
    /// Stable error name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            RuntimesError::Read { .. } => "runtimes_read",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn v(text: &str) -> Version {
        Version::parse(text).expect("test versions are valid")
    }

    fn runtimes_with(php_entries: &[&str]) -> (TempDir, RuntimesDir) {
        let data = TempDir::new().expect("create temp dir");
        let runtimes = RuntimesDir::in_dir(data.path());
        let php = runtimes.runtime_dir(Runtime::Php);
        fs::create_dir_all(&php).expect("create php dir");
        for name in php_entries {
            fs::create_dir(php.join(name)).expect("create version dir");
        }
        (data, runtimes)
    }

    #[test]
    fn paths_follow_the_disk_layout() {
        let data = Path::new("data");
        let runtimes = RuntimesDir::in_dir(data);
        assert_eq!(runtimes.root(), data.join("runtimes"));
        assert_eq!(
            runtimes.runtime_dir(Runtime::Composer),
            data.join("runtimes").join("composer")
        );
        assert_eq!(
            runtimes.version_dir(Runtime::Node, &v("22.11.0")),
            data.join("runtimes").join("node").join("22.11.0")
        );
    }

    #[test]
    fn version_dir_round_trips_the_folder_name() {
        let runtimes = RuntimesDir::in_dir(Path::new("data"));
        for name in ["8.4.3", "8.5.0-rc1", "1.0.0+build.7"] {
            let dir = runtimes.version_dir(Runtime::Php, &v(name));
            assert_eq!(dir.file_name(), Some(OsStr::new(name)));
        }
    }

    #[test]
    fn runtime_from_tool() {
        assert_eq!(Runtime::from(Tool::Php), Runtime::Php);
        assert_eq!(Runtime::from(Tool::Node), Runtime::Node);
    }

    #[test]
    fn parse_skips_names_that_are_not_full_versions() {
        let names = ["8.4.3", "8.4", ".tmp", "abc", "v8.3.0", "", "8.3.14"];
        assert_eq!(parse_installed(names), [v("8.3.14"), v("8.4.3")]);
    }

    #[test]
    fn parse_sorts_oldest_first() {
        let names = ["10.0.0", "8.5.0-rc1", "8.5.0", "9.1.2"];
        assert_eq!(
            parse_installed(names),
            [v("8.5.0-rc1"), v("8.5.0"), v("9.1.2"), v("10.0.0")]
        );
    }

    // Only Unix lets a file name hold bytes that are not UTF-8.
    #[cfg(unix)]
    #[test]
    fn parse_skips_names_that_are_not_utf8() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let names = [OsString::from_vec(vec![b'8', b'.', 0xff]), "8.4.3".into()];
        assert_eq!(parse_installed(names), [v("8.4.3")]);
    }

    #[test]
    fn installed_lists_valid_folders() {
        let (_data, runtimes) = runtimes_with(&["8.4.3", "8.3.14", "8.4", ".tmp", "abc"]);
        assert_eq!(
            runtimes.installed(Runtime::Php).expect("read runtimes"),
            [v("8.3.14"), v("8.4.3")]
        );
    }

    #[test]
    fn installed_skips_files() {
        let (_data, runtimes) = runtimes_with(&["8.3.14"]);
        let php = runtimes.runtime_dir(Runtime::Php);
        fs::write(php.join("8.4.3"), "not a folder").expect("write file");
        assert_eq!(
            runtimes.installed(Runtime::Php).expect("read runtimes"),
            [v("8.3.14")]
        );
    }

    // Creating symlinks on Windows needs admin rights or developer mode.
    #[cfg(unix)]
    #[test]
    fn installed_follows_symlinks_to_folders() {
        let (data, runtimes) = runtimes_with(&[]);
        let php = runtimes.runtime_dir(Runtime::Php);
        let elsewhere = data.path().join("php-build");
        fs::create_dir(&elsewhere).expect("create target dir");
        std::os::unix::fs::symlink(&elsewhere, php.join("8.4.3")).expect("link dir");
        std::os::unix::fs::symlink(data.path().join("gone"), php.join("8.3.0"))
            .expect("link to nothing");
        assert_eq!(
            runtimes.installed(Runtime::Php).expect("read runtimes"),
            [v("8.4.3")]
        );
    }

    #[test]
    fn installed_is_empty_when_the_folder_is_missing() {
        let data = TempDir::new().expect("create temp dir");
        let runtimes = RuntimesDir::in_dir(data.path());
        assert!(runtimes.installed(Runtime::Node).expect("read").is_empty());
    }

    #[test]
    fn installed_fails_when_the_runtime_path_is_a_file() {
        let data = TempDir::new().expect("create temp dir");
        let runtimes = RuntimesDir::in_dir(data.path());
        fs::create_dir(runtimes.root()).expect("create runtimes dir");
        fs::write(runtimes.runtime_dir(Runtime::Php), "").expect("write file");

        let err = runtimes
            .installed(Runtime::Php)
            .expect_err("a file is not a folder");
        assert_eq!(err.code(), "runtimes_read");
        match err {
            RuntimesError::Read { path, .. } => {
                assert_eq!(path, runtimes.runtime_dir(Runtime::Php));
            }
        }
    }
}
