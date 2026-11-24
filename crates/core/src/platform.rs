//! Paths, process spawning, port checks, and service registration. Platform
//! code lives here, including `platform::windows`.

pub mod autostart;
pub mod browser;
pub mod checkup;
pub mod daemon_file;
pub mod files;
pub mod linux_dns;
pub mod low_ports;
pub mod ports;
pub mod probe;
pub mod process;
pub mod runner;
pub mod signals;
pub mod spawn;
pub mod trust;
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
///   the next daemon finds an orphan Caddy (ADR 0009 and 0025,
///   `process::CADDY_ORPHAN_RECOVERY`), and an orphan php-fpm too
///   (`process::FPM_ORPHAN_RECOVERY`).
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

/// Whether this process runs with an administrator's token on Windows
/// ("Run as administrator", or an administrator while UAC is off), which
/// `postgres.exe` refuses (ADR 0023, section 7). `false` on macOS and Linux,
/// and when Windows cannot say: then `postgres.exe` decides, with its own
/// message.
pub fn is_elevated() -> bool {
    #[cfg(windows)]
    return windows::is_elevated().unwrap_or_else(|err| {
        tracing::warn!(%err, "cannot read whether this process is elevated");
        false
    });
    #[cfg(not(windows))]
    false
}

/// [`is_elevated`] for the process `pid`, such as `marsad` for `marsa
/// doctor`. `None` when Windows cannot say, and on macOS and Linux.
pub fn process_is_elevated(pid: u32) -> Option<bool> {
    #[cfg(windows)]
    return windows::process_is_elevated(pid).ok();
    #[cfg(not(windows))]
    {
        let _ = pid;
        None
    }
}
