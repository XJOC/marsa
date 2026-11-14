//! Stopping a managed process, the options every child of `marsad` gets, and
//! what the daemon needs to find an orphan Caddy or php-fpm (ADR 0009): who
//! runs under a pid, its real executable, who listens on a port, signals, the
//! pid files in `run/`, and the data folder's instance id.
//!
//! The `#[cfg]` for these lives here, so the supervisor has none. Every tool
//! runs with an absolute path and an argument array, never through a shell.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::time::Duration;

use tokio::process::Command;

use crate::caddy::Dial;
use crate::orphan::{ChildProcess, Liveness, PidRecord, is_instance_id};

/// Whether the daemon looks for an orphan Caddy before it starts one: macOS
/// only, where ADR 0009 is accepted. Linux waits for spike S-06b. On Windows,
/// Job Objects will stop Caddy with the daemon (architecture.md section 9).
pub const ORPHAN_RECOVERY: bool = cfg!(target_os = "macos");

/// How long `kill`, `ps`, or `lsof` may run.
#[cfg(unix)]
const TOOL_LIMIT: Duration = Duration::from_secs(2);

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

/// Sends `signal` to `pid` with `/bin/kill`, which may run for 2 s. It does
/// not check who the process is: the caller must, right before (ADR 0009).
///
/// # Errors
///
/// As [`terminate`], and `TimedOut` if `/bin/kill` takes longer.
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
/// Always `Unsupported`.
#[cfg(not(unix))]
pub async fn send_signal(pid: u32, _signal: Signal) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("no signals for pid {pid} on this system"),
    ))
}

/// Who runs under `pid` now: the path it was started with and its start time.
/// macOS asks `/bin/ps`. Its `comm` column is `argv[0]`, which the process
/// chose, so it is no proof of the executable: see [`executable`]. With
/// `LC_ALL=C` and `TZ=UTC0` the time always has the same shape and zone. The
/// path is the last column: it may hold spaces, and a last column is never cut.
#[cfg(target_os = "macos")]
pub async fn identity(pid: u32) -> Liveness {
    if pid == 0 {
        return Liveness::Unknown;
    }
    let mut ps = Command::new("/bin/ps");
    ps.args(["-ww", "-p", &pid.to_string(), "-o", "stat=,lstart=,comm="])
        .env("LC_ALL", "C")
        .env("TZ", "UTC0");
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
        let crate::orphan::ProcStat { state, started, .. } = stat()?;
        if matches!(state, 'Z' | 'X') {
            return Ok(Liveness::Gone);
        }
        let exe = fs::read_link(dir.join("exe")).map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => Liveness::Gone,
            _ => Liveness::Unknown,
        })?;
        // Read again: the pid may have changed hands between the two reads.
        if stat()?.started != started {
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

/// Who runs under `pid`, by the file it really runs (links resolved) and its
/// start time. For a program that rewrites its own `argv[0]`, such as
/// php-fpm (`php-fpm: master process (...)`): there [`identity`] finds no
/// path on macOS. macOS reads the start time with `/bin/ps`, the file with
/// [`executable`], then the start time again: a pid that changed hands in
/// between is `Unknown`.
#[cfg(target_os = "macos")]
pub async fn real_identity(pid: u32) -> Liveness {
    let started = match start_time(pid).await {
        Ok(started) => started,
        Err(state) => return state,
    };
    let exe = executable(pid).await;
    match (start_time(pid).await, exe) {
        (Ok(again), Some(exe)) if again == started => Liveness::Alive { exe, started },
        (Err(Liveness::Gone), _) => Liveness::Gone,
        _ => Liveness::Unknown,
    }
}

/// Who runs under `pid`, by the file it really runs and its start time:
/// [`identity`], which reads the `exe` link already.
#[cfg(target_os = "linux")]
pub async fn real_identity(pid: u32) -> Liveness {
    identity(pid).await
}

/// Not supported on this system: never ours.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub async fn real_identity(_pid: u32) -> Liveness {
    Liveness::Unknown
}

/// The start time of `pid` from `/bin/ps`, in the form [`identity`] gives.
/// `Err(Gone)` for no process or a zombie, `Err(Unknown)` when `ps` fails.
#[cfg(target_os = "macos")]
async fn start_time(pid: u32) -> Result<String, Liveness> {
    if pid == 0 {
        return Err(Liveness::Unknown);
    }
    let mut ps = Command::new("/bin/ps");
    ps.args(["-ww", "-p", &pid.to_string(), "-o", "stat=,lstart="])
        .env("LC_ALL", "C")
        .env("TZ", "UTC0");
    match run_tool(ps).await {
        Some(out) if out.status.success() => {
            match crate::orphan::parse_ps_start(&String::from_utf8_lossy(&out.stdout)) {
                Some(('Z', _)) => Err(Liveness::Gone),
                Some((_, started)) => Ok(started),
                None => Err(Liveness::Unknown),
            }
        }
        // `ps` prints nothing and fails for a pid with no process.
        Some(out) if out.stdout.is_empty() => Err(Liveness::Gone),
        _ => Err(Liveness::Unknown),
    }
}

/// The file `pid` really runs, links resolved: not `argv[0]`. macOS asks
/// `/usr/sbin/lsof` for the text files of the process: the first one is the
/// executable, then `dyld`. Linux reads the `exe` link. `None` when it
/// cannot be told. The caller compares the path exactly, so a difference in
/// case alone (APFS ignores case) fails closed.
#[cfg(target_os = "macos")]
pub async fn executable(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    let mut lsof = Command::new("/usr/sbin/lsof");
    lsof.args(["-p", &pid.to_string(), "-a", "-d", "txt", "-Fn"])
        .env("LC_ALL", "C");
    let out = run_tool(lsof).await?;
    crate::orphan::parse_lsof_executable(&String::from_utf8_lossy(&out.stdout))
}

/// The file `pid` really runs: the `/proc/<pid>/exe` link.
#[cfg(target_os = "linux")]
pub async fn executable(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    fs::read_link(Path::new("/proc").join(pid.to_string()).join("exe")).ok()
}

/// Not known on this system.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub async fn executable(_pid: u32) -> Option<PathBuf> {
    None
}

/// The children of `ppid` now, each with its start time in the form
/// [`identity`] gives. For the workers of a php-fpm master that died from
/// SIGKILL (task M4-25): a list, never a proof. The caller checks each pid
/// with [`real_identity`] before it acts. macOS asks `/bin/ps` for the whole
/// process table once, with `LC_ALL=C` and `TZ=UTC0` as [`identity`] does,
/// so the times compare. `None` when the table cannot be read: then nothing
/// is signalled.
#[cfg(target_os = "macos")]
pub async fn children(ppid: u32) -> Option<Vec<ChildProcess>> {
    let mut ps = Command::new("/bin/ps");
    ps.args(["-axo", "pid=,ppid=,lstart="])
        .env("LC_ALL", "C")
        .env("TZ", "UTC0");
    let out = run_tool(ps).await?;
    if !out.status.success() {
        return None;
    }
    let table = String::from_utf8_lossy(&out.stdout);
    Some(crate::orphan::parse_ps_children(&table, ppid))
}

/// The children of `ppid` now, from `/proc` with `std` only: a list, never a
/// proof, as on macOS. A process that cannot be read is skipped, so it is
/// never signalled. `None` when `/proc` cannot be listed.
#[cfg(target_os = "linux")]
pub async fn children(ppid: u32) -> Option<Vec<ChildProcess>> {
    let mut children = Vec::new();
    for entry in fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(text) = fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(stat) = crate::orphan::parse_proc_stat(&text) else {
            continue;
        };
        if stat.ppid == ppid && !matches!(stat.state, 'Z' | 'X') {
            children.push(ChildProcess {
                pid,
                started: stat.started.to_string(),
            });
        }
    }
    Some(children)
}

/// Not known on this system: nothing is signalled.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub async fn children(_ppid: u32) -> Option<Vec<ChildProcess>> {
    None
}

/// The processes that hold a listener at `listen` open now: a Unix socket
/// bound to that path, or a TCP listener on `127.0.0.1:<port>`. A child that
/// inherited the listener from its parent holds it too. It says who holds a
/// listener there, not who inherited it from whom: a filter for the workers
/// of a php-fpm (task M4-25), never a proof alone. The caller checks each
/// pid with [`real_identity`] too. macOS asks `/usr/sbin/lsof` once, and in
/// the same run for the files of this process: if `lsof` does not describe
/// this process, or a signal ended it, its answer proves nothing. `None`
/// when it cannot be told: then nothing is signalled.
#[cfg(target_os = "macos")]
pub async fn listener_holders(listen: &Dial) -> Option<Vec<u32>> {
    let own = std::process::id();
    let mut lsof = Command::new("/usr/sbin/lsof");
    // No `-a`: the files of this process, or the sockets asked for.
    lsof.args(["-nP", "-p", &own.to_string()]);
    match listen {
        Dial::Unix(_) => lsof.arg("-U"),
        Dial::Tcp(port) => {
            let address = format!("-iTCP@127.0.0.1:{port}");
            lsof.args([address.as_str(), "-sTCP:LISTEN"])
        }
    };
    lsof.arg("-FptPnT").env("LC_ALL", "C");
    let out = run_tool(lsof).await?;
    // 1: a selection matched nothing. No code at all: a signal ended `lsof`,
    // so its list may be cut short.
    if !matches!(out.status.code(), Some(0 | 1)) {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    crate::orphan::parse_lsof_holders(&text, listen, own)
}

/// Not known on this system: nothing is signalled. Linux, where `lsof` is
/// often absent, would read `/proc/net/unix`, `/proc/net/tcp`, and
/// `/proc/<pid>/fd` (spike S-06b).
#[cfg(not(target_os = "macos"))]
pub async fn listener_holders(_listen: &Dial) -> Option<Vec<u32>> {
    None
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
    write_private(path, &body)
}

/// The instance id of a data folder, kept in `path` (`caddy/instance-id`):
/// read when the file holds one, else 16 random bytes in hex, written once,
/// `0600`, through a temp file and a rename. Only a file whose content is no
/// id is replaced: a Caddy tagged with the lost id then counts as another
/// Marsa's, which is never touched. A failed read is an error, so a valid id
/// is never replaced because of it.
///
/// # Errors
///
/// The file cannot be read or written, or the OS gives no random bytes.
pub fn instance_id(path: &Path) -> io::Result<String> {
    match File::open(path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            // Longer than an id is not an id: no need to read it all.
            file.take(64).read_to_end(&mut bytes)?;
            let text = std::str::from_utf8(&bytes).unwrap_or_default();
            let text = text.trim_end_matches(['\r', '\n']);
            if is_instance_id(text) {
                return Ok(text.to_owned());
            }
            tracing::warn!(path = %path.display(), "the instance id file holds no id; making a new one");
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|err| io::Error::other(err.to_string()))?;
    let id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    write_private(path, format!("{id}\n").as_bytes())?;
    Ok(id)
}

/// Writes `body` to `path` whole and owner-only, through a temp file next to
/// it and a rename.
fn write_private(path: &Path, body: &[u8]) -> io::Result<()> {
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
        .and_then(|mut file| file.write_all(body))
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

/// Reads a pid file (`run/caddy.pid`, `run/php-fpm-<minor>.pid`). `None` if
/// it is missing, not a regular file, too long, or not a pid record: then it
/// names no pid. A link, even to a regular file, is refused, and so is a FIFO,
/// whose `open` would wait for a writer without end.
pub fn read_pid_file(path: &Path) -> Option<PidRecord> {
    if !fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let file = File::open(path).ok()?;
    // Checked again on the open file: the path may have changed in between.
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(PID_FILE_MAX + 1).read_to_string(&mut text).ok()?;
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

    // The systems where `real_identity` reads a process.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn real_identity_is_the_real_file_whatever_argv0_says() {
        use std::os::unix::process::CommandExt as _;

        // Like php-fpm, which writes `php-fpm: master process (...)` there.
        let mut child = std::process::Command::new("/bin/sleep")
            .arg0("php-fpm: master process (/x)")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let Liveness::Alive { exe, started } = real_identity(pid).await else {
            panic!("no identity for {pid}");
        };
        assert_eq!(exe, fs::canonicalize("/bin/sleep").unwrap());
        assert!(!started.is_empty());
        // `identity` and `real_identity` give the same start time.
        if let Liveness::Alive { started: same, .. } = identity(pid).await {
            assert_eq!(same, started);
        }
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(real_identity(pid).await, Liveness::Gone);
    }

    // The systems where `executable` reads a process.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn executable_is_the_real_file_and_none_once_gone() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let exe = executable(pid).await.expect("an executable");
        assert_eq!(exe, fs::canonicalize("/bin/sleep").unwrap());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(executable(pid).await, None);
    }

    // The systems where `children` reads the process table.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn children_lists_a_child_with_the_start_time_identity_gives() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let Liveness::Alive { started, .. } = identity(pid).await else {
            panic!("no identity for {pid}");
        };
        // Other tests spawn children of this process too: only ours matters.
        let listed = children(std::process::id()).await.expect("the table");
        let want = ChildProcess { pid, started };
        assert!(listed.contains(&want), "{want:?} in {listed:?}");
        // Every entry is a child of ours, so none is pid 1 or this process.
        assert!(
            listed
                .iter()
                .all(|c| c.pid > 1 && c.pid != std::process::id())
        );
        child.kill().unwrap();
        child.wait().unwrap();
        let listed = children(std::process::id()).await.expect("the table");
        assert!(listed.iter().all(|c| c.pid != pid), "{listed:?}");
    }

    // Only macOS lists the holders of a listener (`lsof`).
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn listener_holders_are_the_processes_with_the_socket_or_port_open() {
        let own = std::process::id();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("php-8.4.sock");
        let socket = Dial::Unix(path.clone());
        // Nobody holds it yet: an empty list, which is not `None`.
        assert_eq!(listener_holders(&socket).await, Some(Vec::new()));
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        // A process that does not hold it is not named.
        let mut sleeper = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        assert_eq!(listener_holders(&socket).await, Some(vec![own]));
        sleeper.kill().unwrap();
        sleeper.wait().unwrap();
        drop(listener);
        // The file is still there, and nobody holds the socket.
        assert!(path.exists());
        assert_eq!(listener_holders(&socket).await, Some(Vec::new()));

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = Dial::Tcp(listener.local_addr().unwrap().port());
        assert_eq!(listener_holders(&port).await, Some(vec![own]));
        drop(listener);
        assert_eq!(listener_holders(&port).await, Some(Vec::new()));
    }

    #[test]
    fn the_instance_id_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caddy").join("instance-id");
        let id = instance_id(&path).unwrap();
        assert!(is_instance_id(&id), "{id}");
        assert_eq!(fs::read_to_string(&path).unwrap(), format!("{id}\n"));
        assert_eq!(instance_id(&path).unwrap(), id, "read, not made again");
        // Mode bits exist on Unix only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Another folder gets another id.
        let other = tempfile::tempdir().unwrap();
        let other = instance_id(&other.path().join("instance-id")).unwrap();
        assert_ne!(other, id);
    }

    #[test]
    fn a_file_without_an_id_gets_a_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("instance-id");
        for bad in [&b"not an id\n"[..], &[0xff; 32][..]] {
            fs::write(&path, bad).unwrap();
            let id = instance_id(&path).unwrap();
            assert!(is_instance_id(&id), "{id}");
            assert_eq!(instance_id(&path).unwrap(), id);
        }
    }

    #[test]
    fn a_failed_read_is_an_error_and_replaces_nothing() {
        // A folder where the file should be: it opens on Unix and fails to
        // read, and fails to open on Windows. Either way nothing is written.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("instance-id");
        fs::create_dir(&path).unwrap();
        assert!(instance_id(&path).is_err());
        assert!(path.is_dir());
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

    // FIFOs and symlinks to them are a Unix matter.
    #[cfg(unix)]
    #[test]
    fn a_fifo_or_a_link_at_the_pid_file_path_names_no_pid_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("caddy.pid");
        let made = std::process::Command::new("/usr/bin/mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        let real = dir.path().join("real.pid");
        let record = PidRecord {
            pid: 42,
            exe: PathBuf::from("/data/bin/caddy"),
            started: "Wed Nov  4 03:31:27 2026".into(),
        };
        write_pid_file(&real, &record).unwrap();
        let to_real = dir.path().join("to-real.pid");
        let to_fifo = dir.path().join("to-fifo.pid");
        std::os::unix::fs::symlink(&real, &to_real).unwrap();
        std::os::unix::fs::symlink(&fifo, &to_fifo).unwrap();

        // On a thread: a `read_pid_file` that opens the FIFO never returns,
        // and must not hang the test run.
        let (sender, answers) = std::sync::mpsc::channel();
        let paths = [fifo, to_fifo, to_real];
        std::thread::spawn(move || {
            for path in paths {
                let _ = sender.send(read_pid_file(&path));
            }
        });
        for _ in 0..3 {
            let answer = answers.recv_timeout(std::time::Duration::from_secs(10));
            assert_eq!(answer, Ok(None));
        }
        assert_eq!(read_pid_file(&real), Some(record));
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
