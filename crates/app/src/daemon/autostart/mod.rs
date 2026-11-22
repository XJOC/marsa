//! Where the login file goes and what it starts: the domain behind the
//! autostart routes. The file itself is `marsa_core::platform::autostart`.

use marsa_core::codes;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

pub mod api;

use axum::http::StatusCode;
use marsa_core::platform::autostart::{
    self, AutostartError, AutostartFile, AutostartKind, tray_path,
};
use marsa_core::types::AutostartState;

use super::api::HttpError;

/// Where the login file goes and what it starts. It lives in
/// `Daemon::autostart`. `Daemon::new` gives [`Autostart::unset`], so no test
/// reaches the user's real folders; `server::run` puts
/// [`Autostart::this_user`] in. Tests put a temp folder in with
/// [`Autostart::with`].
#[derive(Debug)]
pub struct Autostart {
    /// `None`: no login start on this system yet (Windows).
    kind: Option<AutostartKind>,
    /// The folder of the login file. `None`: the system gives no home folder.
    dir: Option<PathBuf>,
    /// `marsa-tray`. `None`: the daemon's own path is not known.
    tray: Option<PathBuf>,
    /// Held while the file is read, written, or deleted, so two requests
    /// never share the temp file. Taken on a blocking thread only.
    lock: Mutex<()>,
}

impl Autostart {
    /// This system's kind, with no folder and no `marsa-tray`: every request
    /// gets `autostart_no_home` (`autostart_unsupported` on Windows).
    pub fn unset() -> Self {
        Self::with(AutostartKind::current(), None, None)
    }

    /// The daemon's: this system's kind, the folder of this user (from
    /// `directories`), and `marsa-tray` next to the running `marsad`.
    pub fn this_user() -> Self {
        let kind = AutostartKind::current();
        let dir = kind.and_then(autostart::user_dir);
        let tray = tray_path()
            .inspect_err(|err| {
                tracing::warn!(error = %err, "cannot find the running program, so not marsa-tray");
            })
            .ok();
        Self::with(kind, dir, tray)
    }

    /// For tests: any kind, folder, and `marsa-tray` path.
    pub fn with(kind: Option<AutostartKind>, dir: Option<PathBuf>, tray: Option<PathBuf>) -> Self {
        Self {
            kind,
            dir,
            tray,
            lock: Mutex::default(),
        }
    }

    /// What the login file says now. Blocking: it reads the file.
    pub(super) fn state(&self) -> Result<AutostartState, HttpError> {
        let (kind, dir) = self.target()?;
        let tray = self.tray.as_deref().ok_or_else(tray_unknown)?;
        let _one = self.lock();
        let file = autostart::read(kind, dir, tray).map_err(file_error)?;
        Ok(match file {
            AutostartFile::Absent => AutostartState::default(),
            AutostartFile::Current => AutostartState {
                enabled: true,
                stale: false,
            },
            AutostartFile::Stale => AutostartState {
                enabled: false,
                stale: true,
            },
        })
    }

    /// Writes (`true`) or deletes (`false`) the login file. Blocking.
    pub(super) fn set(&self, enabled: bool) -> Result<AutostartState, HttpError> {
        let (kind, dir) = self.target()?;
        if !enabled {
            let _one = self.lock();
            autostart::remove(kind, dir).map_err(file_error)?;
            tracing::info!(dir = %dir.display(), "Marsa no longer starts at login");
            return Ok(AutostartState::default());
        }
        let tray = self.tray.as_deref().ok_or_else(tray_unknown)?;
        if !tray.is_file() {
            let message = format!("{} is not there", tray.display());
            return Err(HttpError::new(
                StatusCode::CONFLICT,
                codes::TRAY_NOT_FOUND,
                message,
            ));
        }
        let _one = self.lock();
        let path = autostart::write(kind, dir, tray).map_err(file_error)?;
        tracing::info!(file = %path.display(), "Marsa starts at login");
        Ok(AutostartState {
            enabled: true,
            stale: false,
        })
    }

    /// Deletes the login file for `marsa uninstall --all`:
    /// `true` if there was one, `false` if not. Blocking.
    pub(super) fn remove_for_uninstall(&self) -> Result<bool, HttpError> {
        let (kind, dir) = self.target()?;
        let _one = self.lock();
        // `symlink_metadata`: a link in its place counts, and is deleted, not
        // followed.
        let there = std::fs::symlink_metadata(kind.file_in(dir)).is_ok();
        autostart::remove(kind, dir).map_err(file_error)?;
        if there {
            tracing::info!(dir = %dir.display(), "deleted the login file for the uninstall");
        }
        Ok(there)
    }

    fn target(&self) -> Result<(AutostartKind, &Path), HttpError> {
        let kind = self.kind.ok_or_else(|| {
            let message = "start at login works on macOS and Linux only for now";
            HttpError::new(
                StatusCode::NOT_IMPLEMENTED,
                codes::AUTOSTART_UNSUPPORTED,
                message,
            )
        })?;
        let dir = self.dir.as_deref().ok_or_else(|| {
            let message = "the system gives no home folder, so the login file has no place";
            HttpError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                codes::AUTOSTART_NO_HOME,
                message,
            )
        })?;
        Ok((kind, dir))
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        // The lock guards no data, so a poisoned one is still good.
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn tray_unknown() -> HttpError {
    let message = "the daemon's own path is not known, so neither is marsa-tray's";
    HttpError::new(StatusCode::CONFLICT, codes::TRAY_NOT_FOUND, message)
}

/// The login file or its folder failed. A path the file cannot hold is the
/// install's state (409); the disk is a server error (500).
fn file_error(err: AutostartError) -> HttpError {
    let status = match err {
        AutostartError::InvalidPath { .. } => StatusCode::CONFLICT,
        AutostartError::Read { .. } | AutostartError::Write { .. } => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    let message = match std::error::Error::source(&err) {
        Some(source) => format!("{err}: {source}"),
        None => err.to_string(),
    };
    tracing::warn!(code = err.code(), error = %message, "login file");
    HttpError::new(status, err.code(), message)
}
