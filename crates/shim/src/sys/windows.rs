//! Windows has no `exec`, so the shim runs the real program as a child
//! process that shares the console, waits for it, and exits with its code
//! (architecture.md section 7).
//!
//! The shim's only `unsafe` code is here: two Win32 features that std does
//! not wrap, the console's Ctrl+C handler and a Job Object (AGENTS.md
//! section 6, task M1-07).

use std::env;
use std::ffi::OsStr;
use std::io;
use std::mem::zeroed;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{self, Child, Command};
use std::ptr::null;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE, TRUE};
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::core::BOOL;

use crate::error::ShimError;
use crate::name::ShimName;

/// The tool, from the program file that runs (`shims\php.exe`): `argv[0]` is
/// whatever the caller wrote (`php`, `PHP`, or another word), and Windows
/// shims are copies, not links. `argv[0]` only when Windows cannot say.
pub fn own_name(argv0: &OsStr) -> Option<ShimName> {
    match env::current_exe() {
        Ok(path) => ShimName::from_exe_path(&path),
        Err(_) => ShimName::from_exe_path(Path::new(argv0)),
    }
}

/// Runs `command` as a child that shares this console and its stdin,
/// stdout, and stderr, waits for it, and exits this process with the
/// child's exit code, all 32 bits of it (`0xC000013A` after Ctrl+C, say).
/// It returns only if the child did not start.
///
/// - Ctrl+C and Ctrl+Break reach every process on the console, the child
///   too. The shim ignores them ([`on_console_event`]), so the child alone
///   decides: a server that catches Ctrl+C finishes its work, and the shim
///   still waits for it and returns its code.
/// - If something ends the shim first (an IDE's stop button, Task Manager),
///   the child ends with it ([`Job`]), as on Unix, where `exec` makes the
///   two one process.
pub fn exec(mut command: Command) -> ShimError {
    ignore_ctrl_c();
    let job = Job::new();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(source) => return exec_error(&command, source),
    };
    if let Some(job) = &job {
        job.add(&child);
    }
    match child.wait() {
        // `code()` is always `Some` on Windows.
        Ok(status) => process::exit(status.code().unwrap_or(1)),
        // The child may still run: the shim's exit ends it through the job.
        Err(source) => exec_error(&command, source),
    }
}

fn exec_error(command: &Command, source: io::Error) -> ShimError {
    ShimError::Exec {
        path: PathBuf::from(command.get_program()),
        source,
    }
}

/// Keeps Ctrl+C and Ctrl+Break from ending the shim. A handler, not
/// `SetConsoleCtrlHandler(None, TRUE)`: the child would inherit that flag
/// and ignore Ctrl+C as well.
fn ignore_ctrl_c() {
    // SAFETY: `on_console_event` is a plain function that lives as long as
    // the program and touches no shared state, as a handler must: Windows
    // calls it on a thread of its own. A failed call changes nothing; the
    // shim then works as before, only Ctrl+C ends it before the child.
    unsafe {
        SetConsoleCtrlHandler(Some(on_console_event), TRUE);
    }
}

/// The shim's answer to a console event. `TRUE` means handled; `FALSE`
/// passes the event on, in the end to Windows' default, which ends the
/// process.
extern "system" fn on_console_event(event: u32) -> BOOL {
    match event {
        // The child got the same event and decides.
        CTRL_C_EVENT | CTRL_BREAK_EVENT => TRUE,
        // The console is closing, or the user logs off: Windows ends the
        // shim as soon as this returns, and the job would end the child at
        // once, before its own handler is done. So this waits: the main
        // thread exits with the child's code when the child ends, or
        // Windows ends both at its own time limit.
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => loop {
            thread::sleep(Duration::from_secs(60));
        },
        _ => FALSE,
    }
}

/// A Job Object that ends the child when the shim ends, however it ends.
/// Only the child joins it: processes the child starts stay outside
/// (`JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK`), so a server it leaves running
/// in the background keeps running, as on Unix.
struct Job(HANDLE);

impl Job {
    /// `None` if Windows refuses; the child then runs without this
    /// guarantee.
    fn new() -> Option<Self> {
        // SAFETY: `CreateJobObjectW` takes two optional pointers, both null:
        // no security attributes (so the handle is not inheritable, and no
        // child holds a copy that would keep the job open) and no name.
        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            return None;
        }
        let job = Job(handle);
        // SAFETY: `limits` is a plain-data struct; all zeros is a valid
        // value. It lives through the call, which gets its exact size, and
        // `job.0` is an open handle that `job` owns.
        let limited = unsafe {
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            limits.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        (limited != 0).then_some(job)
    }

    /// Puts `child` in the job. A child that cannot join still runs.
    ///
    /// The child runs for a moment before it joins: a process it starts in
    /// that moment is outside the job, as every later one is anyway.
    fn add(&self, child: &Child) {
        // SAFETY: both handles are open: `self` owns the job's, and `child`,
        // borrowed for the call, owns the process's.
        unsafe {
            AssignProcessToJobObject(self.0, child.as_raw_handle());
        }
    }
}

impl Drop for Job {
    /// Closing the last handle ends the processes in the job. On the normal
    /// path this never runs: `process::exit` ends the shim, and Windows
    /// closes the handle then.
    fn drop(&mut self) {
        // SAFETY: `self.0` is an open handle that this `Job` owns, closed
        // once, here.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    //! Each test starts this test program again, which then plays a part
    //! ([`role`]): `exec` ends the process it runs in, so it cannot run
    //! inside the test itself.

    use super::*;
    use std::fs;
    use std::os::windows::process::CommandExt;
    use std::process::{ExitStatus, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    use windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent;
    use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;

    /// The part a copy of this test program plays.
    const ROLE: &str = "MARSA_SHIM_TEST_ROLE";
    /// A file through which a part reports to the test.
    const FILE: &str = "MARSA_SHIM_TEST_FILE";
    /// What the child in `ctrl_c_reaches_the_child_and_not_the_shim` exits
    /// with once it saw Ctrl+C, and without it.
    const SAW_CTRL_C: i32 = 42;
    const NO_CTRL_C: i32 = 43;

    /// This test program, running only [`role`] as `part`, with no output.
    fn this_program(part: &str) -> Command {
        let program = env::current_exe().expect("the test program's path");
        let mut command = Command::new(program);
        command
            .args([
                "sys::windows::tests::role",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ROLE, part)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    fn status(mut command: Command) -> ExitStatus {
        command.status().expect("start the test program")
    }

    /// Lets Ctrl+C end this process again, in case it inherited the flag
    /// that ignores it.
    fn take_ctrl_c() {
        // SAFETY: no handler is given, so nothing must outlive the call.
        unsafe {
            SetConsoleCtrlHandler(None, FALSE);
        }
    }

    static SEEN: AtomicBool = AtomicBool::new(false);

    extern "system" fn note_ctrl_c(event: u32) -> BOOL {
        if event == CTRL_C_EVENT {
            SEEN.store(true, Ordering::SeqCst);
            return TRUE;
        }
        FALSE
    }

    /// The parts. A normal test run does not set [`ROLE`], and then this
    /// test does nothing.
    #[test]
    fn role() {
        let Ok(part) = env::var(ROLE) else {
            return;
        };
        match part.as_str() {
            // The shim, running a child that exits with a code.
            shim if shim.starts_with("shim exit ") => {
                exec(this_program(&shim["shim ".len()..]));
            }
            exit if exit.starts_with("exit ") => {
                let code = exit["exit ".len()..].parse().expect("a number");
                process::exit(code);
            }
            "shim missing" => {
                let err = exec(Command::new(r"C:\marsa-nowhere\php.exe"));
                process::exit(if err.code() == "shim_exec" { 3 } else { 4 });
            }
            // The shim, running a child that adds a byte to `FILE` every
            // 100 ms for 30 s.
            "shim beat" => {
                exec(this_program("beat"));
            }
            "beat" => {
                let file = PathBuf::from(env::var_os(FILE).expect("FILE is set"));
                for _ in 0..300 {
                    let mut text = fs::read_to_string(&file).unwrap_or_default();
                    text.push('x');
                    fs::write(&file, text).expect("write the beat");
                    thread::sleep(Duration::from_millis(100));
                }
                process::exit(0);
            }
            // The shim, in a console of its own, running a child that sends
            // Ctrl+C to that console.
            "shim ctrl-c" => {
                take_ctrl_c();
                exec(this_program("ctrl-c"));
            }
            "ctrl-c" => {
                take_ctrl_c();
                // SAFETY: `note_ctrl_c` is a plain function that lives as long
                // as the program and only stores to an atomic.
                // `GenerateConsoleCtrlEvent` takes two numbers: Ctrl+C, and 0
                // for every process on this console (the shim and this one).
                let sent = unsafe {
                    SetConsoleCtrlHandler(Some(note_ctrl_c), TRUE);
                    GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0)
                };
                let start = Instant::now();
                while sent != 0
                    && !SEEN.load(Ordering::SeqCst)
                    && start.elapsed() < Duration::from_secs(10)
                {
                    thread::sleep(Duration::from_millis(50));
                }
                // Time for the shim to end, if Ctrl+C ended it.
                thread::sleep(Duration::from_secs(1));
                let seen = SEEN.load(Ordering::SeqCst);
                process::exit(if seen { SAW_CTRL_C } else { NO_CTRL_C });
            }
            other => panic!("unknown part {other:?}"),
        }
        // `exec` returned: the child did not start.
        process::exit(5);
    }

    #[test]
    fn the_exit_code_passes_through_all_32_bits() {
        // -1_073_741_510 is 0xC000013A, STATUS_CONTROL_C_EXIT, as an i32.
        for code in [0, 7, 255, 256, -1_073_741_510] {
            let status = status(this_program(&format!("shim exit {code}")));
            assert_eq!(status.code(), Some(code));
        }
    }

    #[test]
    fn a_program_that_does_not_start_is_shim_exec() {
        assert_eq!(status(this_program("shim missing")).code(), Some(3));
    }

    #[test]
    fn the_child_ends_when_the_shim_is_killed() {
        let dir = tempfile::tempdir().expect("create a temp dir");
        let beat = dir.path().join("beat");
        let mut shim = this_program("shim beat")
            .env(FILE, &beat)
            .spawn()
            .expect("start the shim");
        let start = Instant::now();
        while fs::read_to_string(&beat).map_or(0, |text| text.len()) < 3 {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "the child did not start"
            );
            thread::sleep(Duration::from_millis(50));
        }
        shim.kill().expect("kill the shim");
        shim.wait().expect("wait for the shim");
        thread::sleep(Duration::from_millis(500));
        let after_kill = fs::read_to_string(&beat).expect("read the beat").len();
        thread::sleep(Duration::from_secs(1));
        let later = fs::read_to_string(&beat).expect("read the beat").len();
        assert_eq!(after_kill, later, "the child still runs");
    }

    #[test]
    fn ctrl_c_reaches_the_child_and_not_the_shim() {
        let mut shim = this_program("shim ctrl-c");
        shim.creation_flags(CREATE_NEW_CONSOLE);
        // Without the handler the shim ends at once with 0xC000013A.
        assert_eq!(status(shim).code(), Some(SAW_CTRL_C));
    }
}
