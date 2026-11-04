//! Stopping a managed process, the options every child of `marsad` gets, and
//! what the daemon needs to find an orphan Caddy (ADR 0009): who runs under a
//! pid, who listens on a port, signals, and `run/caddy.pid`.
//!
//! The `#[cfg]` for these lives here, so the supervisor has none. Every tool
//! runs with an absolute path and an argument array, never through a shell.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::Path;
#[cfg(unix)]
use std::process::Stdio;

use tokio::process::Command;

use crate::orphan::{Liveness, PidRecord};

/// Whether the daemon looks for an orphan Caddy before it starts one. Not on
/// Windows: Job Objects will stop Caddy with the daemon there (architecture.md
/// section 9), and nothing here reads a Windows process.
pub const ORPHAN_RECOVERY: bool = cfg!(any(target_os = "macos", target_os = "linux"));

/// The longest `run/caddy.pid` that is read.
const PID_FILE_MAX: u64 = 4096;

/// A signal for [`send_signal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Term,
    Kill,
}

/// Asks process `pid` to stop, so it can clean up first.
///
/// macOS and Linux: runs `/bin/kill -TERM <pid>`, with an absolute path and
/// an argument array (ADR 0009), and returns `Ok(true)`. Windows has no
/// SIGTERM: returns `Ok(false)` at once, and the caller kills the process.
///
/// # Errors
///
/// `InvalidInput` for pid 0 or a pid above `i32::MAX` (`kill` would read them
/// as "my process group" or "every process"), for pid 1, and for this
/// process. An error if `/bin/kill` cannot start or fails, for example
/// because the process is gone.
#[cfg(unix)]
pub async fn terminate(pid: u32) -> io::Result<bool> {
    send_signal(pid, Signal::Term).await.map(|()| true)
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

/// Sends `signal` to `pid` with `/bin/kill`. It does not check who the
/// process is: the caller must, right before (ADR 0009).
///
/// # Errors
///
/// As [`terminate`].
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
        Signal::Kill => "-KILL",
    };
    let status = Command::new("/bin/kill")
        .args([flag, &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;
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
/// Always `Unsupported`.
#[cfg(not(unix))]
pub async fn send_signal(pid: u32, _signal: Signal) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("no signals for pid {pid} on this system"),
    ))
}

/// Who runs under `pid` now: its executable path, as it was started, and its
/// start time. macOS asks `/bin/ps` with `LC_ALL=C`, so the time always has
/// the same shape. The path is the last column: it may hold spaces, and a
/// last column is never cut.
#[cfg(target_os = "macos")]
pub async fn identity(pid: u32) -> Liveness {
    if pid == 0 {
        return Liveness::Unknown;
    }
    let mut ps = Command::new("/bin/ps");
    ps.args(["-ww", "-p", &pid.to_string(), "-o", "stat=,lstart=,comm="])
        .env("LC_ALL", "C");
    match run_tool(ps).await {
        Some(out) if out.status.success() => {
            crate::orphan::parse_ps_line(&String::from_utf8_lossy(&out.stdout))
        }
        // `ps` prints nothing and fails for a pid with no process.
        Some(out) if out.stdout.is_empty() => Liveness::Gone,
        _ => Liveness::Unknown,
    }
}

/// Who runs under `pid` now, from `/proc` with `std` only: the path is the
/// `exe` link, the start time the clock ticks since boot in `stat`.
#[cfg(target_os = "linux")]
pub async fn identity(pid: u32) -> Liveness {
    if pid == 0 {
        return Liveness::Unknown;
    }
    let dir = Path::new("/proc").join(pid.to_string());
    let stat = || match fs::read_to_string(dir.join("stat")) {
        Ok(text) => crate::orphan::parse_proc_stat(&text).ok_or(Liveness::Unknown),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Err(Liveness::Gone),
        Err(_) => Err(Liveness::Unknown),
    };
    let read = || -> Result<Liveness, Liveness> {
        let (state, started) = stat()?;
        if matches!(state, 'Z' | 'X') {
            return Ok(Liveness::Gone);
        }
        let exe = fs::read_link(dir.join("exe")).map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => Liveness::Gone,
            _ => Liveness::Unknown,
        })?;
        // Read again: the pid may have changed hands between the two reads.
        if stat()?.1 != started {
            return Ok(Liveness::Unknown);
        }
        Ok(Liveness::Alive {
            exe,
            started: started.to_string(),
        })
    };
    match read() {
        Ok(state) | Err(state) => state,
    }
}

/// Not supported on this system: never ours.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub async fn identity(_pid: u32) -> Liveness {
    Liveness::Unknown
}

/// Who listens on TCP `port`, as `name(pid)`, for messages only: never use it
/// to pick a pid to signal. macOS asks `/usr/sbin/lsof`.
#[cfg(target_os = "macos")]
pub async fn port_owner(port: u16) -> Option<String> {
    let mut lsof = Command::new("/usr/sbin/lsof");
    // `+c 0`: the whole command name, not its first 9 characters.
    lsof.args([
        "+c",
        "0",
        "-nP",
        &format!("-iTCP:{port}"),
        "-sTCP:LISTEN",
        "-Fpc",
    ]);
    let out = run_tool(lsof).await?;
    crate::orphan::parse_lsof(&String::from_utf8_lossy(&out.stdout))
}

/// Not known on this system (Linux often has no `lsof`).
#[cfg(not(target_os = "macos"))]
pub async fn port_owner(_port: u16) -> Option<String> {
    None
}

/// Runs a tool with no input, for at most 2 s. `None` if it cannot start or
/// takes too long; `kill_on_drop` then stops it.
#[cfg(target_os = "macos")]
async fn run_tool(mut command: Command) -> Option<std::process::Output> {
    const TOOL_LIMIT: std::time::Duration = std::time::Duration::from_secs(2);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(TOOL_LIMIT, command.output())
        .await
        .ok()?
        .ok()
}

/// Writes `run/caddy.pid` whole: a temp file next to it, then a rename, so a
/// reader never sees half a file. On Unix the file is `0600` from the start.
/// There is no `fsync`: the file only has to survive a crash of the daemon,
/// not of the OS, which ends Caddy too.
///
/// # Errors
///
/// The folder or the file cannot be written.
pub fn write_pid_file(path: &Path, record: &PidRecord) -> io::Result<()> {
    let mut body = serde_json::to_vec(record).map_err(io::Error::other)?;
    body.push(b'\n');
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = Path::new(&temp);
    let result = create_new(temp)
        .or_else(|err| {
            if err.kind() != io::ErrorKind::AlreadyExists {
                return Err(err);
            }
            // Left by a write that stopped half way.
            fs::remove_file(temp)?;
            create_new(temp)
        })
        .and_then(|mut file| file.write_all(&body))
        // The file is closed here: Windows cannot rename an open file.
        .and_then(|()| fs::rename(temp, path));
    if result.is_err() {
        // Best effort: the write already failed, and that is the error to report.
        let _ = fs::remove_file(temp);
    }
    result
}

fn create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    // `create_new` also refuses a symlink that is already there.
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Reads `run/caddy.pid`. `None` if it is missing, too long, or not a pid
/// record: then it names no pid.
pub fn read_pid_file(path: &Path) -> Option<PidRecord> {
    let mut text = String::new();
    File::open(path)
        .ok()?
        .take(PID_FILE_MAX + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > PID_FILE_MAX {
        return None;
    }
    serde_json::from_str(&text).ok()
}

/// Deletes `run/caddy.pid`. No file is not an error.
///
/// # Errors
///
/// The file exists and cannot be deleted.
pub fn remove_pid_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // Only Unix sends signals; on Windows `terminate` does nothing.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_refuses_pids_that_kill_reads_as_groups() {
        for pid in [0, 1, u32::MAX, 1 << 31, std::process::id()] {
            let err = terminate(pid).await.unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{pid}");
            let err = send_signal(pid, Signal::Kill).await.unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{pid}");
        }
    }

    // The systems where `identity` reads a process.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn identity_describes_a_child_and_then_sees_it_gone() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let Liveness::Alive { exe, started } = identity(pid).await else {
            panic!("no identity for {pid}");
        };
        assert_eq!(exe.file_name().unwrap(), "sleep");
        assert!(!started.is_empty());
        // The same answer again: the start time is stable.
        let again = identity(pid).await;
        assert_eq!(again, Liveness::Alive { exe, started });
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(identity(pid).await, Liveness::Gone);
    }

    #[test]
    fn the_pid_file_round_trips_and_replaces_a_stale_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run").join("caddy.pid");
        assert_eq!(read_pid_file(&path), None);
        let record = PidRecord {
            pid: 42,
            exe: PathBuf::from("/data/bin/caddy"),
            started: "Wed Nov  4 03:31:27 2026".into(),
        };
        write_pid_file(&path, &record).unwrap();
        fs::write(dir.path().join("run").join("caddy.pid.tmp"), "half").unwrap();
        write_pid_file(&path, &record).unwrap();
        assert_eq!(read_pid_file(&path), Some(record));
        let names: Vec<_> = fs::read_dir(dir.path().join("run"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["caddy.pid"], "no temp file left");
        // Mode bits exist on Unix only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        remove_pid_file(&path).unwrap();
        remove_pid_file(&path).unwrap();
        assert_eq!(read_pid_file(&path), None);
    }

    #[test]
    fn a_pid_file_that_is_not_a_record_names_no_pid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caddy.pid");
        let long = format!(
            "{{\"pid\":42,\"exe\":\"/x\",\"started\":\"{}\"}}",
            "x".repeat(5000)
        );
        for text in ["", "42", "{\"pid\":42}", &long] {
            fs::write(&path, text).unwrap();
            assert_eq!(read_pid_file(&path), None, "{text:?}");
        }
    }
}
