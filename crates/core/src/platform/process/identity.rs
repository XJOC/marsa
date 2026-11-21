//! Who runs under a pid: its path, its start time, its real executable, its
//! children, and the parent of this process.

use std::path::PathBuf;

use crate::orphan::{ChildProcess, Liveness};

#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::{fs, io};

#[cfg(target_os = "macos")]
use tokio::process::Command;

#[cfg(target_os = "macos")]
use super::run_tool;

#[cfg(unix)]
use crate::ps::StartTime;

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
            crate::ps::parse_ps_line(&String::from_utf8_lossy(&out.stdout))
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
    proc_identity(&Path::new("/proc").join(pid.to_string()))
}

/// [`identity`] from `dir`, the process's folder in `/proc`.
#[cfg(target_os = "linux")]
fn proc_identity(dir: &Path) -> Liveness {
    let stat = || match fs::read_to_string(dir.join("stat")) {
        Ok(text) => crate::orphan::parse_proc_stat(&text).ok_or(Liveness::Unknown),
        Err(err) => Err(read_failed(&err, dir)),
    };
    let read = || -> Result<Liveness, Liveness> {
        let crate::orphan::ProcStat { state, started, .. } = stat()?;
        if matches!(state, 'Z' | 'X') {
            return Ok(Liveness::Gone);
        }
        let exe = fs::read_link(dir.join("exe")).map_err(|err| read_failed(&err, dir))?;
        // Read again: the pid may have changed hands between the two reads.
        if stat()?.started != started {
            return Ok(Liveness::Unknown);
        }
        Ok(Liveness::Alive {
            exe,
            started: StartTime::Ticks(started),
        })
    };
    match read() {
        Ok(state) | Err(state) => state,
    }
}

/// What a failed read in `dir` says. A process that is reaped during a read
/// may fail it with another error than "not found" (task M4-25c): `stat`
/// opened before the reap and read after gives ESRCH, and the `exe` link
/// found before it gives EACCES (ENOENT on newer kernels). So such an error
/// is `Gone` once `dir` itself is not found any more: only where a new
/// [`identity`] a moment later would say `Gone` too. Anything else stays
/// `Unknown`: a pid that changed hands since then has a folder again.
#[cfg(target_os = "linux")]
fn read_failed(err: &io::Error, dir: &Path) -> Liveness {
    let not_found = |err: &io::Error| err.kind() == io::ErrorKind::NotFound;
    if not_found(err) || fs::symlink_metadata(dir).is_err_and(|lookup| not_found(&lookup)) {
        Liveness::Gone
    } else {
        Liveness::Unknown
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
async fn start_time(pid: u32) -> Result<StartTime, Liveness> {
    if pid == 0 {
        return Err(Liveness::Unknown);
    }
    let mut ps = Command::new("/bin/ps");
    ps.args(["-ww", "-p", &pid.to_string(), "-o", "stat=,lstart="])
        .env("LC_ALL", "C")
        .env("TZ", "UTC0");
    match run_tool(ps).await {
        Some(out) if out.status.success() => {
            match crate::ps::parse_ps_start(&String::from_utf8_lossy(&out.stdout)) {
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
    Some(crate::ps::parse_ps_children(&table, ppid))
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
                started: StartTime::Ticks(stat.started),
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

/// The process that started this one, for the log only (task M5-21).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentProcess {
    pub pid: u32,
    /// Its name, without control characters. `None` when it cannot be read.
    pub name: Option<String>,
}

/// The process that started this one, if it still runs: its pid from `std`,
/// and its name from `/bin/ps` on macOS or `/proc` on Linux.
#[cfg(unix)]
pub async fn parent_process() -> Option<ParentProcess> {
    let pid = std::os::unix::process::parent_id();
    Some(ParentProcess {
        pid,
        name: process_name(pid).await,
    })
}

/// Not known on Windows: `std` gives no parent pid there, and Win32 gives it
/// only through `unsafe` calls.
#[cfg(not(unix))]
pub async fn parent_process() -> Option<ParentProcess> {
    None
}

/// The `comm` column of `pid`: the path it was started with.
#[cfg(target_os = "macos")]
async fn process_name(pid: u32) -> Option<String> {
    let mut ps = Command::new("/bin/ps");
    ps.args(["-ww", "-p", &pid.to_string(), "-o", "comm="])
        .env("LC_ALL", "C");
    let out = run_tool(ps).await?;
    if !out.status.success() {
        return None;
    }
    printable_name(&String::from_utf8_lossy(&out.stdout))
}

/// `/proc/<pid>/comm`: the name of the program, cut to 15 bytes.
#[cfg(target_os = "linux")]
async fn process_name(pid: u32) -> Option<String> {
    let path = Path::new("/proc").join(pid.to_string()).join("comm");
    printable_name(&fs::read_to_string(path).ok()?)
}

/// Not known on this system.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
async fn process_name(_pid: u32) -> Option<String> {
    None
}

/// `text` without control characters, which could forge a log line, and at
/// most 256 characters. `None` when nothing is left.
#[cfg(unix)]
fn printable_name(text: &str) -> Option<String> {
    let name: String = text
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect();
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    use std::fs;

    #[cfg(unix)]
    use super::super::after_exec;

    // The systems where `identity` reads a process.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn identity_describes_a_child_and_then_sees_it_gone() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let is_sleep = |now: &Liveness| matches!(now, Liveness::Alive { exe, .. } if exe.file_name().is_some_and(|n| n == "sleep"));
        let Liveness::Alive { exe, started } =
            after_exec(async || identity(pid).await, is_sleep).await
        else {
            panic!("no identity for {pid}");
        };
        assert!(!started.to_string().is_empty());
        // The same answer again: the start time is stable.
        let again = identity(pid).await;
        assert_eq!(again, Liveness::Alive { exe, started });
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(identity(pid).await, Liveness::Gone);
    }

    /// `proc_identity` on a fake `/proc/<pid>` folder (task M4-25c).
    #[cfg(target_os = "linux")]
    mod proc_folder {
        use super::*;

        // `pid (comm) state ppid`, 17 more fields, then the start time.
        const STAT: &str = "4145 (php-fpm) S 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 98765 0 0\n";
        const EXE: &str = "/opt/php/sbin/php-fpm";

        fn folder(stat: &str) -> tempfile::TempDir {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("stat"), stat).unwrap();
            std::os::unix::fs::symlink(EXE, dir.path().join("exe")).unwrap();
            dir
        }

        #[test]
        fn a_readable_folder_is_alive() {
            let dir = folder(STAT);
            let want = Liveness::Alive {
                exe: EXE.into(),
                started: StartTime::Ticks(98_765),
            };
            assert_eq!(proc_identity(dir.path()), want);
        }

        #[test]
        fn a_zombie_and_a_missing_folder_are_gone() {
            let zombie = folder(&STAT.replace(" S ", " Z "));
            assert_eq!(proc_identity(zombie.path()), Liveness::Gone);
            let missing = zombie.path().join("4146");
            assert_eq!(proc_identity(&missing), Liveness::Gone);
        }

        #[test]
        fn a_failed_read_in_a_folder_that_is_still_there_is_unknown() {
            // `exe` is a file, not a link: `read_link` fails with EINVAL.
            let dir = folder(STAT);
            fs::remove_file(dir.path().join("exe")).unwrap();
            fs::write(dir.path().join("exe"), "").unwrap();
            assert_eq!(proc_identity(dir.path()), Liveness::Unknown);
            // `stat` is a folder: reading it fails with EISDIR.
            let dir = folder(STAT);
            fs::remove_file(dir.path().join("stat")).unwrap();
            fs::create_dir(dir.path().join("stat")).unwrap();
            assert_eq!(proc_identity(dir.path()), Liveness::Unknown);
        }

        #[test]
        fn a_failed_read_is_gone_once_the_folder_is_gone() {
            // What a reap during the read gives: ESRCH (3) from `stat`, EACCES
            // from the `exe` link.
            let reaped = [
                io::Error::from_raw_os_error(3),
                io::Error::from(io::ErrorKind::PermissionDenied),
            ];
            let dir = tempfile::tempdir().unwrap();
            let gone = dir.path().join("4145");
            for err in &reaped {
                assert_eq!(read_failed(err, &gone), Liveness::Gone, "{err}");
                assert_eq!(read_failed(err, dir.path()), Liveness::Unknown, "{err}");
            }
        }
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
        let sleep = fs::canonicalize("/bin/sleep").unwrap();
        let is_sleep = |now: &Liveness| matches!(now, Liveness::Alive { exe, .. } if *exe == sleep);
        let Liveness::Alive { started, .. } =
            after_exec(async || real_identity(pid).await, is_sleep).await
        else {
            panic!("no identity for {pid}");
        };
        assert!(!started.to_string().is_empty());
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
        let sleep = fs::canonicalize("/bin/sleep").unwrap();
        let is_sleep = |exe: &Option<PathBuf>| exe.as_ref() == Some(&sleep);
        after_exec(async || executable(pid).await, is_sleep).await;
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

    #[cfg(unix)]
    #[tokio::test]
    async fn the_parent_process_is_the_one_that_started_this_one() {
        let parent = parent_process().await.expect("a parent on Unix");
        assert_eq!(parent.pid, std::os::unix::process::parent_id());
        // The test runner, which waits for this process.
        assert!(parent.name.is_some_and(|name| !name.is_empty()));
    }

    #[cfg(unix)]
    #[test]
    fn a_printable_name_has_no_control_characters() {
        assert_eq!(printable_name(" /bin/zsh\n").as_deref(), Some("/bin/zsh"));
        assert_eq!(
            printable_name("evil\nmarsad started\r").as_deref(),
            Some("evilmarsad started")
        );
        assert_eq!(
            printable_name(&"x".repeat(300)).map(|name| name.len()),
            Some(256)
        );
        assert_eq!(printable_name(" \n"), None);
    }
}
