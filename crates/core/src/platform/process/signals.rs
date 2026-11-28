//! Sending a signal to a process or to a process group (`/bin/kill` on
//! Unix, nothing on Windows), and starting a process as the leader of a
//! group of its own.

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
    let pid = plain_pid(pid)?;
    kill(flag(signal), &pid.to_string()).await
}

/// Sends `signal` to every process of the group `pgid`: `/bin/kill -TERM --
/// -<pgid>`. The group is the one a process started by [`own_group`] leads:
/// its id is the leader's pid. Like [`send_signal`] it does not check whose
/// group it is: the caller must, right before. While the leader is the
/// caller's child and not yet waited for, the group is surely the caller's;
/// after that, only while no process has the pid `pgid` (the system never
/// gives a group's id to a new process while the group has a member).
///
/// # Errors
///
/// `InvalidInput` for the ids [`send_signal`] refuses. `TimedOut`, or an
/// error when `/bin/kill` fails, for example because the group is empty.
#[cfg(unix)]
pub async fn signal_group(pgid: u32, signal: Signal) -> io::Result<()> {
    let pgid = plain_pid(pgid)?;
    kill(flag(signal), &format!("-{pgid}")).await
}

/// Whether the group `pgid` has a process that this user may signal:
/// `/bin/kill -0 -- -<pgid>`, which sends nothing. `false` when it is
/// empty, when `pgid` is one [`send_signal`] refuses, or when `/bin/kill`
/// fails.
#[cfg(unix)]
pub async fn group_exists(pgid: u32) -> bool {
    let Ok(pgid) = plain_pid(pgid) else {
        return false;
    };
    kill("-0", &format!("-{pgid}")).await.is_ok()
}

/// Makes `cmd` start its process as the leader of a new process group, so
/// [`signal_group`] reaches it and everything it starts. A Ctrl+C in the
/// terminal of `marsad` then no longer reaches it: `marsad` stops it.
#[cfg(unix)]
pub fn own_group(cmd: &mut Command) {
    cmd.process_group(0);
}

/// `pid` when `/bin/kill` reads it as one process: not 0 or above
/// `i32::MAX` (its group, or every process), not 1, not this process.
#[cfg(unix)]
fn plain_pid(pid: u32) -> io::Result<u32> {
    let plain = matches!(i32::try_from(pid), Ok(p) if p > 1);
    if !plain || pid == std::process::id() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to signal pid {pid}"),
        ));
    }
    Ok(pid)
}

#[cfg(unix)]
fn flag(signal: Signal) -> &'static str {
    match signal {
        Signal::Term => "-TERM",
        Signal::Int => "-INT",
        Signal::Quit => "-QUIT",
        Signal::Kill => "-KILL",
    }
}

/// Runs `/bin/kill <flag> -- <target>`, for up to 2 s. `--` ends the
/// options, so a group's `-<pgid>` is read as a target.
#[cfg(unix)]
async fn kill(flag: &str, target: &str) -> io::Result<()> {
    let mut kill = Command::new("/bin/kill");
    kill.args([flag, "--", target])
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
            "/bin/kill {flag} {target} failed: {status}"
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

/// Windows sends no signals.
///
/// # Errors
///
/// Always `Unsupported`.
#[cfg(not(unix))]
pub async fn signal_group(pgid: u32, _signal: Signal) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("no signals for group {pgid} on this system"),
    ))
}

/// Windows has no process groups to signal: always `false`.
#[cfg(not(unix))]
pub async fn group_exists(_pgid: u32) -> bool {
    false
}

/// Nothing on Windows: the daemon's Job Object holds every process it
/// starts, and their children, and ends them with the daemon (ADR 0017).
#[cfg(not(unix))]
pub fn own_group(_cmd: &mut tokio::process::Command) {}

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

    #[cfg(unix)]
    #[tokio::test]
    async fn every_group_signal_refuses_the_same_ids() {
        for signal in SIGNALS {
            for pgid in [0, 1, u32::MAX, 1 << 31, std::process::id()] {
                let err = signal_group(pgid, signal).await.unwrap_err();
                assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{signal:?} {pgid}");
                assert!(!group_exists(pgid).await, "{pgid}");
            }
        }
    }

    /// A shell that leads its own group and starts a `sleep`: one signal to
    /// the group ends both, and the group is gone.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_group_signal_reaches_the_leader_and_its_children() {
        let mut cmd = Command::new("/bin/sh");
        // A fixed shell line, in a test only: the product never builds one.
        cmd.args(["-c", "sleep 600 & echo $!; wait"])
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        own_group(&mut cmd);
        let mut leader = cmd.spawn().unwrap();
        let pgid = leader.id().unwrap();
        let mut out = tokio::io::BufReader::new(leader.stdout.take().unwrap());
        let mut line = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut out, &mut line)
            .await
            .unwrap();
        let sleep: u32 = line.trim().parse().unwrap();
        assert!(group_exists(pgid).await);
        signal_group(pgid, Signal::Term).await.unwrap();
        leader.wait().await.unwrap();
        let started = std::time::Instant::now();
        while group_exists(pgid).await {
            assert!(started.elapsed() < std::time::Duration::from_secs(10));
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // The `sleep` got it too: a signal to it alone finds no process.
        assert!(send_signal(sleep, Signal::Kill).await.is_err());
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
