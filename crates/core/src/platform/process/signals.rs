//! Sending a signal to a process (`/bin/kill` on Unix, nothing on Windows).

use std::io;

use super::Signal;

#[cfg(unix)]
use std::process::Stdio;

#[cfg(unix)]
use tokio::process::Command;

#[cfg(unix)]
use super::TOOL_LIMIT;

/// Sends `signal` to `pid`: runs `/bin/kill -TERM|-INT|-QUIT|-KILL <pid>`,
/// with an absolute path and an argument array (ADR 0009), for up to 2 s. It
/// does not check who the process is: the caller must, right before
/// (ADR 0009).
///
/// # Errors
///
/// `InvalidInput` for pid 0 or a pid above `i32::MAX` (`kill` would read them
/// as "my process group" or "every process"), for pid 1, and for this
/// process. `TimedOut` if `/bin/kill` takes longer. An error if `/bin/kill`
/// cannot start or fails, for example because the process is gone.
#[cfg(unix)]
pub async fn send_signal(pid: u32, signal: Signal) -> io::Result<()> {
    let plain = matches!(i32::try_from(pid), Ok(p) if p > 1);
    if !plain || pid == std::process::id() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to signal pid {pid}"),
        ));
    }
    let flag = match signal {
        Signal::Term => "-TERM",
        Signal::Int => "-INT",
        Signal::Quit => "-QUIT",
        Signal::Kill => "-KILL",
    };
    let mut kill = Command::new("/bin/kill");
    kill.args([flag, &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let status = tokio::time::timeout(TOOL_LIMIT, kill.status())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "/bin/kill took too long"))??;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "/bin/kill {flag} {pid} failed: {status}"
        )))
    }
}

/// Windows sends no signals.
///
/// # Errors
///
/// Always `Unsupported`: the caller kills the process instead.
#[cfg(not(unix))]
pub async fn send_signal(pid: u32, _signal: Signal) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("no signals for pid {pid} on this system"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGNALS: [Signal; 4] = [Signal::Term, Signal::Int, Signal::Quit, Signal::Kill];

    // Only Unix sends signals.
    #[cfg(unix)]
    #[tokio::test]
    async fn every_signal_refuses_pids_that_kill_reads_as_groups() {
        for signal in SIGNALS {
            for pid in [0, 1, u32::MAX, 1 << 31, std::process::id()] {
                let err = send_signal(pid, signal).await.unwrap_err();
                assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{signal:?} {pid}");
            }
        }
    }

    // Nothing is sent, whatever the pid. The supervisor reads `Unsupported`
    // as "kill at once".
    #[cfg(not(unix))]
    #[tokio::test]
    async fn windows_sends_no_signal() {
        for signal in SIGNALS {
            let err = send_signal(4242, signal).await.unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::Unsupported, "{signal:?}");
        }
    }
}
