//! Starting a program in the background (`spawn_detached`) and the Job Object
//! that ends it with the daemon (`join_kill_on_close_job`).

use std::ffi::OsStr;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;
use std::ptr::null;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessW, DETACHED_PROCESS, GetCurrentProcess, GetExitCodeProcess,
    PROCESS_INFORMATION, STARTF_FORCEOFFFEEDBACK, STARTUPINFOW, WaitForSingleObject,
};

use super::{invalid, wide};

/// The longest command line `CreateProcessW` takes, in UTF-16 units, with
/// the final NUL.
pub(super) const MAX_COMMAND_LINE: usize = 32_767;

/// Puts the calling process in a new Job Object with
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Every process it starts afterwards
/// joins the job, and so do their children. When the calling process ends
/// (a clean exit, a crash, or "End task"), Windows closes the job's only
/// handle and ends every process in the job.
///
/// For `marsad` only: a client (the CLI, the window, the tray) that called it
/// would take the `marsad` it starts down with it.
///
/// Only the first call makes a job; a later call returns the same result. A
/// process that is already in a job (a terminal, a CI runner) gets a nested
/// job; Windows 8 and later allow that.
///
/// # Errors
///
/// The Win32 error if the job cannot be made or joined. The caller goes on
/// without it: only the clean-up after a crash is lost.
pub(crate) fn join_kill_on_close_job() -> io::Result<()> {
    // The OS error of the first call, so the answer can be given again.
    static JOINED: OnceLock<Result<(), Option<i32>>> = OnceLock::new();
    let joined = JOINED.get_or_init(|| create_and_join_job().map_err(|err| err.raw_os_error()));
    match joined {
        Ok(()) => Ok(()),
        Err(Some(code)) => Err(io::Error::from_raw_os_error(*code)),
        // Every error above comes from `last_os_error`, which has a code.
        Err(None) => Err(io::Error::other("cannot join a Job Object")),
    }
}

fn create_and_join_job() -> io::Result<()> {
    // SAFETY: `CreateJobObjectW` takes two optional pointers, both null (no
    // security attributes, no name). `limits` is a zeroed plain-data struct
    // that lives through `SetInformationJobObject`, which gets its exact
    // size. `GetCurrentProcess` returns a pseudo handle that needs no close.
    // `job` is checked for null before any use.
    unsafe {
        let job = CreateJobObjectW(null(), null());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let limited = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if limited == 0 || AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
            let err = io::Error::last_os_error();
            // This process is not in the job, so closing it ends nobody.
            CloseHandle(job);
            return Err(err);
        }
        // `job` is never closed, on purpose. It is the job's only handle:
        // with null security attributes it cannot be inherited, so no child
        // holds a copy. Closing it is what ends every process in the job,
        // this one too, so Windows closes it when this process ends. If the
        // handle were ever made inheritable, every child would keep the job
        // open and kill-on-close would stop working without a word.
    }
    Ok(())
}

/// A process started by [`spawn_detached`]. Dropping it closes the handle and
/// leaves the process running.
#[derive(Debug)]
pub(crate) struct DetachedProcess {
    process: OwnedHandle,
    pid: u32,
}

impl DetachedProcess {
    /// The process id, like `Child::id`.
    pub(crate) fn id(&self) -> u32 {
        self.pid
    }

    /// How the process ended, if it already has. Never blocks. Takes
    /// `&mut self` like `Child::try_wait`, so callers read the same on every
    /// system.
    ///
    /// # Errors
    ///
    /// The Win32 error if Windows cannot say.
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let process = self.process.as_raw_handle();
        // SAFETY: `process` is the open process handle this struct owns, and
        // `code` is a valid place for the exit code. A wait of 0 ms returns
        // at once.
        unsafe {
            match WaitForSingleObject(process, 0) {
                WAIT_OBJECT_0 => {
                    let mut code = 0;
                    if GetExitCodeProcess(process, &mut code) == 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(Some(ExitStatus::from_raw(code)))
                }
                WAIT_TIMEOUT => Ok(None),
                _ => Err(io::Error::last_os_error()),
            }
        }
    }
}

/// Starts `program` with `args` in the background: no console, no window,
/// and **no handle of the caller**, not even stdin, stdout, or stderr
/// (`bInheritHandles = FALSE`). What the new process prints is lost. It
/// inherits the caller's environment.
///
/// `program` must be an absolute path to an `.exe`: it is never searched on
/// `PATH`, and nothing goes through a shell. A `.bat` or `.cmd` file is
/// refused, because Windows would run it through `cmd.exe`, which reads the
/// command line by other rules than the ones written here (CVE-2024-24576).
/// `cwd` is the working folder of the new process, or the caller's own if
/// `None`.
///
/// # Errors
///
/// `InvalidInput` for a program that is not an absolute `.exe` path or holds
/// a quote, a NUL inside `program`, an argument, or `cwd`, or a command line
/// longer than 32,767 characters. Otherwise the Win32 error of
/// `CreateProcessW`, for example `NotFound`.
pub(crate) fn spawn_detached(
    program: &Path,
    args: &[&OsStr],
    cwd: Option<&Path>,
) -> io::Result<DetachedProcess> {
    check_program(program)?;
    let application = wide(program.as_os_str())?;
    let mut command_line = command_line(program.as_os_str(), args)?;
    let cwd = cwd.map(|dir| wide(dir.as_os_str())).transpose()?;
    // SAFETY: `application`, `command_line`, and `cwd` are NUL-terminated
    // UTF-16 buffers that live through the call; `command_line` is mutable,
    // as `CreateProcessW` may write to it. `startup` is zeroed plain data
    // with its size set: it passes no handles, and its null desktop means the
    // caller's desktop. `info` is filled only on success; then both of its
    // handles are open and owned here: the thread handle is closed, and the
    // process handle moves into an `OwnedHandle`, which closes it on drop.
    unsafe {
        let mut startup: STARTUPINFOW = zeroed();
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        // No busy cursor while a program that never shows a window starts.
        startup.dwFlags = STARTF_FORCEOFFFEEDBACK;
        let mut info: PROCESS_INFORMATION = zeroed();
        let created = CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            // bInheritHandles = FALSE: the whole point of this function.
            0,
            DETACHED_PROCESS | CREATE_NO_WINDOW,
            // The caller's environment and, without `cwd`, its folder.
            null(),
            cwd.as_ref().map_or(null(), |dir| dir.as_ptr()),
            &startup,
            &mut info,
        );
        if created == 0 {
            return Err(io::Error::last_os_error());
        }
        CloseHandle(info.hThread);
        Ok(DetachedProcess {
            process: OwnedHandle::from_raw_handle(info.hProcess),
            pid: info.dwProcessId,
        })
    }
}

/// An absolute path to an `.exe` (any letter case), with no quote in it.
pub(super) fn check_program(program: &Path) -> io::Result<()> {
    if !program.is_absolute() {
        return Err(invalid("the program is not an absolute path"));
    }
    let exe = program
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"));
    if !exe {
        return Err(invalid("the program is not an .exe file"));
    }
    if program
        .as_os_str()
        .encode_wide()
        .any(|unit| unit == u16::from(b'"'))
    {
        return Err(invalid("the program path holds a quote"));
    }
    Ok(())
}

/// The command line for `CreateProcessW`: the program between quotes as it
/// is, then every argument, quoted the way the Microsoft C runtime (and
/// Rust's `std::env`) reads them back. NUL-terminated.
///
/// The program is `argv[0]`, which the C runtime reads by its own rule: it
/// runs from the opening quote to the next quote, and backslashes are never
/// escapes there. So it is written as it is, like std does, and a quote in it
/// is refused ([`check_program`]; here too, for the tests).
pub(super) fn command_line(program: &OsStr, args: &[&OsStr]) -> io::Result<Vec<u16>> {
    const QUOTE: u16 = b'"' as u16;

    let program: Vec<u16> = program.encode_wide().collect();
    if program.contains(&QUOTE) {
        return Err(invalid("the program path holds a quote"));
    }
    if program.contains(&0) {
        return Err(invalid("a NUL character in a path or an argument"));
    }
    let mut line = Vec::with_capacity(program.len() + 2);
    line.push(QUOTE);
    line.extend(program);
    line.push(QUOTE);
    for arg in args {
        line.push(u16::from(b' '));
        append_arg(&mut line, arg)?;
    }
    line.push(0);
    if line.len() > MAX_COMMAND_LINE {
        return Err(invalid("the command line is longer than 32767 characters"));
    }
    Ok(line)
}

/// Adds one argument. The rules of "Parsing C command-line arguments": an
/// argument with a space or a tab, and an empty one, goes in quotes; a quote
/// is written `\"`; backslashes are literal unless a quote follows them, and
/// then each one is doubled.
pub(super) fn append_arg(line: &mut Vec<u16>, arg: &OsStr) -> io::Result<()> {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;

    let arg: Vec<u16> = arg.encode_wide().collect();
    if arg.contains(&0) {
        return Err(invalid("a NUL character in a path or an argument"));
    }
    let quotes = arg.is_empty()
        || arg
            .iter()
            .any(|unit| *unit == u16::from(b' ') || *unit == u16::from(b'\t'));
    if quotes {
        line.push(QUOTE);
    }
    let mut backslashes = 0;
    for unit in arg {
        if unit == BACKSLASH {
            backslashes += 1;
        } else {
            if unit == QUOTE {
                // 2n + 1 backslashes, then the quote: n literal backslashes
                // and a literal quote.
                line.extend(std::iter::repeat_n(BACKSLASH, backslashes + 1));
            }
            backslashes = 0;
        }
        line.push(unit);
    }
    if quotes {
        // 2n backslashes before the closing quote: n literal backslashes.
        line.extend(std::iter::repeat_n(BACKSLASH, backslashes));
        line.push(QUOTE);
    }
    Ok(())
}
