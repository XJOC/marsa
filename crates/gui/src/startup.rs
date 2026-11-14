//! What the window and the menu-bar icon do when they start: read
//! `--data-dir`, set up logging, and (the window only) take the lock that
//! keeps a second window from opening.
//!
//! `--data-dir <PATH>` is for tests; the default is the OS app data folder.
//! No environment variable overrides it: the shim never reads one, so the
//! programs and the shim always share one folder.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use marsa_core::config::ConfigError;
use tracing_subscriber::EnvFilter;

/// The lock file of the window, in the daemon's `run/` folder. The window
/// holds it while it is open, so a second `marsa-gui` (for example from the
/// tray's "Open Marsa") finds it taken and exits at once (task M5-04).
pub const WINDOW_LOCK: &str = "gui.lock";

/// The default log filter of both programs: `MARSA_LOG` overrides it. It
/// keeps the graphics and GUI crates quiet.
const DEFAULT_LOG: &str = "warn,marsa_gui=info,marsa_core=info";

/// Why the program cannot start.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("--data-dir needs a path")]
    MissingPath,
    #[error("unknown argument {0}")]
    UnknownArgument(String),
    /// The OS gives no app data folder.
    #[error(transparent)]
    DataDir(#[from] ConfigError),
    #[error("cannot resolve {}", .path.display())]
    Resolve {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// The data folder from the program's arguments (without the program name):
/// `--data-dir <PATH>` or `--data-dir=<PATH>`, else the OS app data folder.
/// Made absolute, so it does not depend on the working folder.
///
/// # Errors
///
/// An unknown argument, a missing path, or no app data folder.
pub fn data_dir(mut args: impl Iterator<Item = OsString>) -> Result<PathBuf, StartupError> {
    let mut flag = None;
    while let Some(arg) = args.next() {
        if arg == "--data-dir" {
            flag = Some(args.next().ok_or(StartupError::MissingPath)?);
        } else if let Some(value) = arg.to_str().and_then(|a| a.strip_prefix("--data-dir=")) {
            flag = Some(value.into());
        } else {
            return Err(StartupError::UnknownArgument(arg.display().to_string()));
        }
    }
    let dir = match flag {
        Some(dir) => PathBuf::from(dir),
        None => marsa_core::config::app_data_dir()?,
    };
    // Not `canonicalize`: that gives `\\?\` paths on Windows, and the path is
    // passed to `marsad`.
    std::path::absolute(&dir).map_err(|source| StartupError::Resolve { path: dir, source })
}

/// Logs to stderr. `MARSA_LOG` sets the filter, as in `marsad`.
pub fn init_logging() {
    let filter =
        EnvFilter::try_from_env("MARSA_LOG").unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG));
    // Fails only if a logger is already set, which cannot happen here.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Takes the window lock of `data_dir`: `Some` with the open file, which
/// holds the lock until it is dropped, or `None` when another window holds
/// it. The OS frees the lock when the window exits, even after a crash.
///
/// `run/` is created if needed; the daemon closes it to other users when it
/// writes its own file there.
///
/// # Errors
///
/// `run/` or the lock file cannot be created or opened.
pub fn window_lock(data_dir: &Path) -> io::Result<Option<File>> {
    let run = data_dir.join("run");
    fs::create_dir_all(&run)?;
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(run.join(WINDOW_LOCK))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> impl Iterator<Item = OsString> {
        list.iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn the_data_dir_flag_in_both_forms_gives_an_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().to_str().unwrap();
        assert_eq!(data_dir(args(&["--data-dir", text])).unwrap(), dir.path());
        let joined = format!("--data-dir={text}");
        assert_eq!(data_dir(args(&[&joined])).unwrap(), dir.path());
        // A relative path is resolved against the working folder.
        assert!(
            data_dir(args(&["--data-dir", "rel"]))
                .unwrap()
                .is_absolute()
        );
    }

    #[test]
    fn bad_arguments_are_refused() {
        assert!(matches!(
            data_dir(args(&["--data-dir"])),
            Err(StartupError::MissingPath)
        ));
        assert!(matches!(
            data_dir(args(&["--help"])),
            Err(StartupError::UnknownArgument(arg)) if arg == "--help"
        ));
    }

    #[test]
    fn one_window_holds_the_lock_until_it_closes() {
        let dir = tempfile::tempdir().unwrap();
        let first = window_lock(dir.path()).unwrap();
        assert!(first.is_some(), "the first window takes the lock");
        assert!(dir.path().join("run").join(WINDOW_LOCK).is_file());
        assert!(
            window_lock(dir.path()).unwrap().is_none(),
            "a second window finds it taken"
        );
        drop(first);
        assert!(
            window_lock(dir.path()).unwrap().is_some(),
            "the lock is free once the window is gone"
        );
    }
}
