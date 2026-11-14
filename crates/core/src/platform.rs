//! Paths, process spawning, port checks, and service registration. Platform
//! code lives here, including `platform::windows`.
//!
//! One file per M3 task, so the tasks can run in parallel (task M3-01).

pub mod autostart;
pub mod browser;
pub mod daemon_file;
pub mod low_ports;
pub mod process;
pub mod signals;
pub mod spawn;
#[cfg(windows)]
pub mod windows;

pub use browser::{OpenError, open_url};

/// Ties the processes the daemon starts to the daemon's own life. Call it
/// once, early in `marsad`, before any process is started. `marsad` only: a
/// client (the CLI, the window, the tray) that called it would take the
/// `marsad` it starts down with it.
///
/// - Windows: the daemon joins a Job Object that ends its children when it
///   ends, even after a crash or "End task" (ADR 0017).
/// - macOS and Linux: nothing to do here. A child of a crashed daemon stays;
///   on macOS the next daemon finds it (ADR 0009, `process::ORPHAN_RECOVERY`).
///
/// # Errors
///
/// The Win32 error if the job cannot be made or joined. The daemon logs it
/// and goes on: only the clean-up after a crash is lost.
pub fn bind_children_to_daemon() -> std::io::Result<()> {
    #[cfg(windows)]
    return windows::join_kill_on_close_job();
    #[cfg(not(windows))]
    Ok(())
}
