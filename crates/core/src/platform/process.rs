//! Stopping a managed process, and the options every child of `marsad` gets.
//!
//! The `#[cfg]` for these lives here, so the supervisor has none.

use std::io;

use tokio::process::Command;

/// Asks process `pid` to stop, so it can clean up first.
///
/// macOS and Linux: runs `/bin/kill -TERM <pid>`, with an absolute path and
/// an argument array (ADR 0009), and returns `Ok(true)`. Windows has no
/// SIGTERM: returns `Ok(false)` at once, and the caller kills the process.
///
/// # Errors
///
/// `InvalidInput` for pid 0 or a pid above `i32::MAX` (`kill` would read them
/// as "my process group" or "every process"). An error if `/bin/kill` cannot
/// start or fails, for example because the process is gone.
#[cfg(unix)]
pub async fn terminate(pid: u32) -> io::Result<bool> {
    use std::process::Stdio;

    if !matches!(i32::try_from(pid), Ok(p) if p > 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to signal pid {pid}"),
        ));
    }
    let status = Command::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;
    if status.success() {
        Ok(true)
    } else {
        Err(io::Error::other(format!(
            "/bin/kill -TERM {pid} failed: {status}"
        )))
    }
}

/// Asks process `pid` to stop. Windows has no SIGTERM: always `Ok(false)`, and
/// the caller kills the process instead.
///
/// # Errors
///
/// None on Windows. The `Result` matches the macOS and Linux version.
#[cfg(not(unix))]
pub async fn terminate(_pid: u32) -> io::Result<bool> {
    Ok(false)
}

/// On Windows, sets `CREATE_NO_WINDOW`, so no console window opens for a child
/// of `marsad` (architecture.md section 9). Does nothing on macOS and Linux.
/// Not for the shim: its children must print to the user's terminal.
pub fn hide_console_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    // Nothing to set on other systems; this only marks `cmd` as used.
    #[cfg(not(windows))]
    let _ = cmd;
}

// Only Unix sends signals; on Windows `terminate` does nothing.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn terminate_refuses_pids_that_kill_reads_as_groups() {
        for pid in [0, u32::MAX, 1 << 31] {
            let err = terminate(pid).await.unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{pid}");
        }
    }
}
