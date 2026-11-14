//! Deletes an installed version safely (task M5-07), in two steps:
//!
//! 1. [`retire`] takes the install lock, so no install runs meanwhile, and
//!    renames `runtimes/<runtime>/<version>` to `runtimes/<runtime>/del-<8
//!    hex>` in one atomic step. That name is not a version, so the version is
//!    no longer installed for anyone: the daemon, the shims, the resolver.
//! 2. [`Retired::remove`] deletes that folder. If the delete stops half way,
//!    what is left still has the `del-` name, never a version name, and
//!    [`clean_retired`] deletes it when `marsad` starts.
//!
//! The delete never leaves the folder through a symlink:
//! `std::fs::remove_dir_all` removes a link itself, never what it points to.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use semver::Version;

use super::InstallError;
use super::execute::{lock, random_hex, rename};
use crate::runtimes::{Runtime, RuntimesDir};

/// A version folder moved aside to `runtimes/<runtime>/del-<8 hex>`. It holds
/// the install lock until it is dropped.
#[derive(Debug)]
pub struct Retired {
    path: PathBuf,
    _lock: File,
}

/// Moves the folder of `version` aside: see the module text. Blocking.
///
/// # Errors
///
/// `install_busy` while an install holds the lock, `version_not_installed`
/// when the folder is not there, and `uninstall_io` when it cannot be moved
/// (on Windows: a program from it still runs). Nothing is changed then.
pub fn retire(
    runtimes: &RuntimesDir,
    runtime: Runtime,
    version: &Version,
) -> Result<Retired, UninstallError> {
    let lock = lock(runtimes).map_err(UninstallError::from)?;
    let dir = runtimes.version_dir(runtime, version);
    if let Err(source) = fs::symlink_metadata(&dir) {
        return Err(match source.kind() {
            io::ErrorKind::NotFound => UninstallError::NotInstalled { path: dir },
            _ => UninstallError::Io { path: dir, source },
        });
    }
    // A free name, never one in use: on Unix a rename replaces an empty
    // folder. Under the lock, so no other uninstall takes it meanwhile.
    let mut free = None;
    for _ in 0..5 {
        let path = runtimes
            .runtime_dir(runtime)
            .join(format!("del-{}", random_hex()?));
        if fs::symlink_metadata(&path).is_err() {
            free = Some(path);
            break;
        }
    }
    let path = free.ok_or_else(|| UninstallError::Io {
        path: dir.clone(),
        source: io::ErrorKind::AlreadyExists.into(),
    })?;
    rename(&dir, &path)?;
    Ok(Retired { path, _lock: lock })
}

impl Retired {
    /// `runtimes/<runtime>/del-<8 hex>`.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Deletes the folder, then frees the install lock. Blocking.
    ///
    /// # Errors
    ///
    /// `uninstall_io`: the rest stays as `del-<8 hex>` until the next start
    /// of `marsad`. The version is gone either way.
    pub fn remove(self) -> Result<(), UninstallError> {
        fs::remove_dir_all(&self.path).map_err(|source| UninstallError::Io {
            path: self.path.clone(),
            source,
        })
    }
}

/// Deletes each `del-<8 hex>` entry under `runtimes/<runtime>/`: what an
/// uninstall left when its delete stopped half way. `marsad` calls it once at
/// start-up. A failure is only logged; the next start tries again.
pub fn clean_retired(runtimes: &RuntimesDir) {
    for runtime in Runtime::ALL {
        // A missing folder has nothing to clean.
        let Ok(entries) = fs::read_dir(runtimes.runtime_dir(runtime)) else {
            continue;
        };
        for entry in entries.flatten() {
            if !is_retired(&entry.file_name()) {
                continue;
            }
            let path = entry.path();
            match fs::remove_dir_all(&path) {
                Ok(()) => tracing::info!(path = %path.display(), "deleted an unfinished uninstall"),
                Err(err) => {
                    tracing::warn!(path = %path.display(), error = %err, "cannot delete an unfinished uninstall");
                }
            }
        }
    }
}

/// `del-` and 8 hex digits: the names [`retire`] gives.
fn is_retired(name: &OsStr) -> bool {
    let hex = name.to_str().and_then(|name| name.strip_prefix("del-"));
    hex.is_some_and(|hex| hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Why a version was not uninstalled. [`UninstallError::code`] gives a
/// stable name that the UI maps to a message.
#[derive(Debug, thiserror::Error)]
pub enum UninstallError {
    #[error("an install is running")]
    Busy,

    #[error("{} is not installed", .path.display())]
    NotInstalled { path: PathBuf },

    #[error("cannot move or delete {}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl UninstallError {
    /// Stable error name for the UI. Never changes once released. The busy
    /// lock has the install's own code.
    pub fn code(&self) -> &'static str {
        match self {
            UninstallError::Busy => "install_busy",
            UninstallError::NotInstalled { .. } => "version_not_installed",
            UninstallError::Io { .. } => "uninstall_io",
        }
    }
}

/// The lock, the rename, and the random name give only these two.
impl From<InstallError> for UninstallError {
    fn from(err: InstallError) -> Self {
        match err {
            InstallError::Busy => UninstallError::Busy,
            InstallError::Io { path, source } => UninstallError::Io { path, source },
            other => UninstallError::Io {
                path: PathBuf::new(),
                source: io::Error::other(other.to_string()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    /// `runtimes/php/<version>/bin/php` for each version.
    fn installed(versions: &[&str]) -> (TempDir, RuntimesDir) {
        let data = TempDir::new().unwrap();
        let runtimes = RuntimesDir::in_dir(data.path());
        for version in versions {
            let bin = runtimes.version_dir(Runtime::Php, &v(version)).join("bin");
            fs::create_dir_all(&bin).unwrap();
            fs::write(bin.join("php"), "php").unwrap();
        }
        (data, runtimes)
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn retire_hides_the_version_at_once_and_remove_deletes_it() {
        let (_data, runtimes) = installed(&["8.3.12", "8.4.1"]);
        let retired = retire(&runtimes, Runtime::Php, &v("8.3.12")).unwrap();
        assert!(is_retired(retired.path().file_name().unwrap()));
        assert_eq!(runtimes.installed(Runtime::Php).unwrap(), [v("8.4.1")]);
        // The install lock is held until the folder is gone.
        let busy = retire(&runtimes, Runtime::Php, &v("8.4.1")).unwrap_err();
        assert_eq!(busy.code(), "install_busy");

        retired.remove().unwrap();
        assert_eq!(names(&runtimes.runtime_dir(Runtime::Php)), ["8.4.1"]);
        let err = retire(&runtimes, Runtime::Php, &v("8.3.12")).unwrap_err();
        assert_eq!(err.code(), "version_not_installed");
    }

    #[test]
    fn an_install_that_holds_the_lock_makes_it_busy() {
        let (_data, runtimes) = installed(&["8.3.12"]);
        let _install = lock(&runtimes).unwrap();
        let err = retire(&runtimes, Runtime::Php, &v("8.3.12")).unwrap_err();
        assert!(matches!(err, UninstallError::Busy), "{err:?}");
        assert_eq!(runtimes.installed(Runtime::Php).unwrap(), [v("8.3.12")]);
    }

    // `cfg(unix)` outside `platform`: making symlinks on Windows needs admin
    // rights or developer mode.
    #[cfg(unix)]
    #[test]
    fn the_delete_never_follows_a_link_out_of_the_folder() {
        use std::os::unix::fs::symlink;
        let (data, runtimes) = installed(&["8.3.12"]);
        let outside = data.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep.txt"), "mine").unwrap();
        let dir = runtimes.version_dir(Runtime::Php, &v("8.3.12"));
        symlink(&outside, dir.join("lib")).unwrap();
        symlink(outside.join("keep.txt"), dir.join("bin").join("keep")).unwrap();
        // A version folder that is itself a link: only the link goes.
        symlink(&outside, runtimes.version_dir(Runtime::Php, &v("8.4.1"))).unwrap();

        for version in ["8.3.12", "8.4.1"] {
            let retired = retire(&runtimes, Runtime::Php, &v(version)).unwrap();
            retired.remove().unwrap();
        }
        assert!(names(&runtimes.runtime_dir(Runtime::Php)).is_empty());
        assert_eq!(
            fs::read_to_string(outside.join("keep.txt")).unwrap(),
            "mine"
        );
    }

    // `cfg(unix)` outside `platform`: a folder without write permission is
    // the simplest way to stop a delete half way.
    #[cfg(unix)]
    #[test]
    fn a_delete_that_stops_half_way_leaves_no_version_name() {
        use std::os::unix::fs::PermissionsExt;
        let (_data, runtimes) = installed(&["8.3.12"]);
        let bin = runtimes.version_dir(Runtime::Php, &v("8.3.12")).join("bin");
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o500)).unwrap();
        let retired = retire(&runtimes, Runtime::Php, &v("8.3.12")).unwrap();
        let left = retired.path().to_path_buf();
        let result = retired.remove();
        fs::set_permissions(left.join("bin"), fs::Permissions::from_mode(0o755)).unwrap();
        // Root may delete anyway; then there is nothing half done to check.
        if let Err(err) = result {
            assert_eq!(err.code(), "uninstall_io");
            let name = left.file_name().unwrap().to_string_lossy().into_owned();
            assert_eq!(names(&runtimes.runtime_dir(Runtime::Php)), [name]);
            assert!(runtimes.installed(Runtime::Php).unwrap().is_empty());
        }
        clean_retired(&runtimes);
        assert!(names(&runtimes.runtime_dir(Runtime::Php)).is_empty());
    }

    #[test]
    fn clean_retired_deletes_only_del_folders() {
        let (_data, runtimes) = installed(&["8.3.12"]);
        let php = runtimes.runtime_dir(Runtime::Php);
        let node = runtimes.runtime_dir(Runtime::Node);
        for dir in [
            php.join("del-0a1b2c3d").join("bin"),
            node.join("del-FFFFFFFF"),
            php.join("del-xyz"),
            php.join("del-0a1b2c3d4"),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::create_dir_all(runtimes.tmp_dir()).unwrap();
        clean_retired(&runtimes);
        assert_eq!(names(&php), ["8.3.12", "del-0a1b2c3d4", "del-xyz"]);
        assert!(names(&node).is_empty());
        assert!(runtimes.tmp_dir().is_dir());
    }
}
