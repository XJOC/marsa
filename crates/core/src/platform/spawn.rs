//! Starting `marsad` in the background from the CLI or the window
//! (architecture.md section 5, "Lifecycle"). Task M3-08.
//!
//! The daemon outlives the program that starts it: it shares no pipe and no
//! terminal with it, it gets no console window, and nobody blocks on it.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// `marsad` in the folder of the running program (`marsa` or `marsa-gui`),
/// with `.exe` on Windows. Never looked up on `PATH` (architecture.md
/// section 10).
///
/// # Errors
///
/// The `io::Error` from `current_exe`, or `NotFound` if the program has no
/// folder.
pub fn marsad_path() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "the running program has no folder")
    })?;
    Ok(dir.join(format!("marsad{}", std::env::consts::EXE_SUFFIX)))
}

/// Starts `marsad --data-dir <data_dir>` detached and returns its process id.
/// It does not wait for the daemon to be ready: the caller polls
/// `run/daemon.json` and `/v1/health`.
///
/// - Both paths are made absolute first, so `marsad` is never searched on
///   `PATH` and `data_dir` does not depend on the working folder.
/// - stdin, stdout, and stderr are null: the daemon logs to its own file, and
///   no pipe ties it to this process.
/// - The working folder is the one `marsad` sits in, so the daemon does not
///   keep the caller's folder busy.
/// - On macOS and Linux it gets its own process group, so Ctrl+C in the
///   terminal does not reach it. On Windows it gets no console.
///
/// # Errors
///
/// The `io::Error` from `spawn`, for example `NotFound` if `marsad` is missing.
pub fn spawn_marsad(marsad: &Path, data_dir: &Path) -> io::Result<u32> {
    let marsad = std::path::absolute(marsad)?;
    let data_dir = std::path::absolute(data_dir)?;
    let mut command = Command::new(&marsad);
    command
        .arg("--data-dir")
        .arg(&data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = marsad.parent() {
        command.current_dir(dir);
    }
    detach(&mut command);
    let child = command.spawn()?;
    let pid = child.id();
    reap_on_exit(child);
    Ok(pid)
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // A new process group: the terminal sends Ctrl+C and its hang-up signal
    // to its foreground group only.
    command.process_group(0);
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    // Values from the Win32 "Process Creation Flags" page, written here so the
    // core needs no `windows-sys`. `DETACHED_PROCESS`: no console, so no
    // Ctrl+C from the caller's console. `CREATE_NO_WINDOW`: no window if the
    // first flag is ever dropped.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
}

/// Collects the daemon's exit status on a small background thread when it
/// exits, so it does not stay a zombie on macOS and Linux while a long-lived
/// window runs. The caller never blocks, and the thread ends with the daemon
/// or with this process.
fn reap_on_exit(mut child: Child) {
    let spawned = std::thread::Builder::new()
        .name("marsad-reaper".to_owned())
        .stack_size(64 * 1024)
        .spawn(move || {
            // The status is not needed, only its collection.
            let _ = child.wait();
        });
    if let Err(err) = spawned {
        // The `Child` is dropped, which does not stop the daemon.
        tracing::debug!(error = %err, "cannot start a thread to reap marsad");
    }
}
