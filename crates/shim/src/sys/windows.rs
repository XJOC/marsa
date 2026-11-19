//! Windows has no `exec`, so the shim runs the real program as a child
//! process that shares the console, waits for it, and exits with its code
//! (architecture.md section 7).
//!
//! The shim's only `unsafe` code is here: Win32 features that std does not
//! wrap, the console's Ctrl+C handler and a Job Object (AGENTS.md section 6).

use std::env;
use std::ffi::OsStr;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, Child, Command};
use std::ptr::null;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE, TRUE};
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, GetConsoleCP, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::DETACHED_PROCESS;
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
///
/// - Ctrl+C and Ctrl+Break reach every process on the console, the child
///   too. The shim ignores them ([`on_console_event`]), so the child alone
///   decides: a server that catches Ctrl+C finishes its work, and the shim
///   still waits for it and returns its code.
/// - If something ends the shim first (an IDE's stop button, Task Manager),
///   the child ends with it ([`Job`]), as on Unix, where `exec` makes the
///   two one process.
/// - A shim with no console (its parent used `DETACHED_PROCESS`) starts the
///   child with none either. Otherwise Windows would give the child a new
///   console window, which the program run directly would not have.
///
/// It returns only if the child did not start, or if waiting for it failed.
/// In the second case the job closes as `exec` returns, which ends the
/// child.
pub fn exec(mut command: Command) -> ShimError {
    ignore_ctrl_c();
    if !has_console() {
        command.creation_flags(DETACHED_PROCESS);
    }
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
        Err(source) => exec_error(&command, source),
    }
}

fn exec_error(command: &Command, source: io::Error) -> ShimError {
    ShimError::Exec {
        path: PathBuf::from(command.get_program()),
        source,
    }
}

fn has_console() -> bool {
    // SAFETY: `GetConsoleCP` takes no arguments and only reads this
    // process's state. It returns 0 when the process has no console.
    unsafe { GetConsoleCP() != 0 }
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
/// passes the event on, in the end to Windows' default handler.
extern "system" fn on_console_event(event: u32) -> BOOL {
    match event {
        // The child got the same event and decides.
        CTRL_C_EVENT | CTRL_BREAK_EVENT => TRUE,
        // The console window is closing. Windows ends the shim as soon as
        // this returns, and the job would then end the child at once,
        // before its own handler is done. So this waits: the main thread
        // exits with the child's code when the child ends, or Windows ends
        // both at its time limit (5 s).
        CTRL_CLOSE_EVENT => loop {
            thread::sleep(Duration::from_secs(60));
        },
        // Logoff and shutdown reach only programs run as a service (Windows
        // ends the others itself), and a service's default handler keeps
        // the process running: the same as for the program run directly.
        _ => FALSE,
    }
}

/// A Job Object that ends the child when the shim ends, however it ends.
/// Only the child joins it: processes the child starts stay outside
/// (`JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK`), so a server it leaves running
/// in the background keeps running, as on Unix. A child that asks to leave
/// the job may (`JOB_OBJECT_LIMIT_BREAKAWAY_OK`), as it may without the
/// shim.
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
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_BREAKAWAY_OK
            | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
        // SAFETY: `limits` lives through the call, which gets its exact
        // size, and `job.0` is an open handle that `job` owns.
        let limited = unsafe {
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
    /// The child runs for a moment before it joins: std gives no handle to
    /// resume a process started suspended. If the shim is killed in that
    /// moment, the child
    /// outlives it; a process the child starts then is outside the job, as
    /// every later one is anyway.
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
    use std::fs::{self, OpenOptions};
    use std::io::{BufRead, Write};
    use std::process::{ExitStatus, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    use windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent;
    use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;

    /// The part a copy of this test program plays.
    const ROLE: &str = "MARSA_SHIM_TEST_ROLE";
    /// A folder through which the parts report to the test.
    const DIR: &str = "MARSA_SHIM_TEST_DIR";
    /// How the child in `ctrl_c_reaches_the_child_and_not_the_shim` ends:
    /// it saw Ctrl+C, it did not, or it could not send it.
    const SAW_CTRL_C: i32 = 42;
    const NO_CTRL_C: i32 = 43;
    const NOT_SENT: i32 = 44;

    /// This test program, running only [`role`] as `part`. A child the part
    /// starts gets the same stdin, stdout, and stderr.
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
            .env(ROLE, part);
        command
    }

    /// [`this_program`], with no stdin, stdout, or stderr.
    fn quiet(part: &str) -> Command {
        let mut command = this_program(part);
        command
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

    /// Adds a byte to `<DIR>/<name>` every 100 ms for 30 s, and stops early
    /// once `<DIR>/stop` exists.
    fn beat(name: &str) -> ! {
        let dir = PathBuf::from(env::var_os(DIR).expect("DIR is set"));
        for _ in 0..300 {
            if dir.join("stop").exists() {
                break;
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(name))
                .expect("open the beat");
            file.write_all(b"x").expect("write the beat");
            thread::sleep(Duration::from_millis(100));
        }
        process::exit(0);
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
            // The shim, running a child that starts a grandchild; both beat.
            "shim parent" => {
                exec(this_program("parent"));
            }
            #[expect(
                clippy::zombie_processes,
                reason = "the grandchild must outlive this process; Windows has no zombies"
            )]
            "parent" => {
                this_program("beat grandchild")
                    .spawn()
                    .expect("start the grandchild");
                beat("child");
            }
            "beat grandchild" => beat("grandchild"),
            // The shim, running a child that echoes a line of stdin.
            "shim echo" => {
                exec(this_program("echo"));
            }
            "echo" => {
                let mut line = String::new();
                io::stdin().lock().read_line(&mut line).expect("read stdin");
                let line = line.trim_end();
                println!("out:{line}");
                eprintln!("err:{line}");
                process::exit(0);
            }
            // The shim, in a console of its own, running a child that sends
            // Ctrl+C to that console. The child does not clear the ignore
            // flag: it gets the shim's state, as a real program would.
            "shim ctrl-c" => {
                take_ctrl_c();
                exec(this_program("ctrl-c"));
            }
            "ctrl-c" => {
                // SAFETY: `note_ctrl_c` is a plain function that lives as long
                // as the program and only stores to an atomic.
                // `GenerateConsoleCtrlEvent` takes two numbers: Ctrl+C, and 0
                // for every process on this console (the shim and this one).
                let sent = unsafe {
                    SetConsoleCtrlHandler(Some(note_ctrl_c), TRUE);
                    GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0)
                };
                if sent == 0 {
                    process::exit(NOT_SENT);
                }
                let start = Instant::now();
                while !SEEN.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(10) {
                    thread::sleep(Duration::from_millis(50));
                }
                // Time for the shim to end, if Ctrl+C ended it.
                thread::sleep(Duration::from_secs(1));
                let seen = SEEN.load(Ordering::SeqCst);
                process::exit(if seen { SAW_CTRL_C } else { NO_CTRL_C });
            }
            // The shim with no console, running a child that says whether it
            // has one.
            "shim no console" => {
                exec(this_program("console?"));
            }
            "console?" => process::exit(i32::from(has_console())),
            other => panic!("unknown part {other:?}"),
        }
        // `exec` returned: the child did not start.
        process::exit(5);
    }

    #[test]
    fn the_exit_code_passes_through_all_32_bits() {
        // -1_073_741_510 is 0xC000013A, STATUS_CONTROL_C_EXIT, as an i32.
        for code in [0, 7, 255, 256, -1_073_741_510] {
            let status = status(quiet(&format!("shim exit {code}")));
            assert_eq!(status.code(), Some(code));
        }
    }

    #[test]
    fn a_program_that_does_not_start_is_shim_exec() {
        assert_eq!(status(quiet("shim missing")).code(), Some(3));
    }

    #[test]
    fn stdin_stdout_and_stderr_pass_through() {
        let mut shim = this_program("shim echo")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start the shim");
        let mut stdin = shim.stdin.take().expect("piped stdin");
        stdin.write_all(b"hello\n").expect("write stdin");
        drop(stdin);
        let out = shim.wait_with_output().expect("wait for the shim");
        assert_eq!(out.status.code(), Some(0));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stdout.contains("out:hello"), "{stdout}");
        assert!(stderr.contains("err:hello"), "{stderr}");
    }

    #[test]
    fn killing_the_shim_ends_the_child_but_not_its_children() {
        let dir = tempfile::tempdir().expect("create a temp dir");
        let length = |name: &str| fs::metadata(dir.path().join(name)).map_or(0, |m| m.len());
        let mut shim = quiet("shim parent")
            .env(DIR, dir.path())
            .spawn()
            .expect("start the shim");
        let start = Instant::now();
        while length("child") < 3 || length("grandchild") < 3 {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "the child and the grandchild did not start"
            );
            thread::sleep(Duration::from_millis(50));
        }
        // The shim still runs, so the kill below is what ends the child.
        assert!(shim.try_wait().expect("look at the shim").is_none());
        shim.kill().expect("kill the shim");
        shim.wait().expect("wait for the shim");
        thread::sleep(Duration::from_millis(500));
        let (child, grandchild) = (length("child"), length("grandchild"));
        // At least 1 s (about ten beats), so a child the kill missed shows;
        // up to 5 s for the grandchild's next beat on a slow runner.
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(1)
            || (length("grandchild") == grandchild && start.elapsed() < Duration::from_secs(5))
        {
            thread::sleep(Duration::from_millis(50));
        }
        let still_beating = length("grandchild") > grandchild;
        let child_after = length("child");
        // The grandchild stops at its next beat; the folder goes only after.
        fs::write(dir.path().join("stop"), "").expect("stop the grandchild");
        let mut last = length("grandchild");
        let start = Instant::now();
        loop {
            thread::sleep(Duration::from_millis(300));
            let now = length("grandchild");
            if now == last || start.elapsed() > Duration::from_secs(5) {
                break;
            }
            last = now;
        }
        assert_eq!(child_after, child, "the child still runs");
        assert!(still_beating, "the grandchild ended with the shim");
    }

    #[test]
    fn ctrl_c_reaches_the_child_and_not_the_shim() {
        let mut shim = quiet("shim ctrl-c");
        shim.creation_flags(CREATE_NEW_CONSOLE);
        // Without the handler the shim ends at once with 0xC000013A.
        assert_eq!(status(shim).code(), Some(SAW_CTRL_C));
    }

    #[test]
    fn a_shim_with_no_console_gives_the_child_none() {
        let mut shim = quiet("shim no console");
        shim.creation_flags(DETACHED_PROCESS);
        assert_eq!(status(shim).code(), Some(0), "the child got a console");
    }
}
