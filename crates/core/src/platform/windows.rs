//! Windows-only process code (architecture.md section 9, ADR 0017).
//!
//! - [`join_kill_on_close_job`]: the daemon puts itself in a Job Object that
//!   Windows closes when the daemon ends, however it ends. Every process the
//!   daemon starts afterwards is in that job, so Caddy and PHP never outlive
//!   a crashed daemon.
//! - [`spawn_detached`]: starts a program in the background and hands it none
//!   of the caller's handles. std starts every child with
//!   `bInheritHandles = TRUE`, so a daemon started by `marsa park . | more`
//!   kept the pipe open, and the reader waited until the daemon stopped.
//!
//! The only `unsafe` code of `marsa-core` is in this file (AGENTS.md
//! section 6). Each block calls one or two Win32 functions and checks every
//! result.

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
    PROCESS_INFORMATION, STARTUPINFOW, WaitForSingleObject,
};

/// The longest command line `CreateProcessW` takes, in UTF-16 units, with
/// the final NUL.
const MAX_COMMAND_LINE: usize = 32_767;

/// Puts the calling process in a new Job Object with
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Every process it starts afterwards
/// joins the job, and so do their children. When the calling process ends
/// (a clean exit, a crash, or "End task"), Windows closes the job's only
/// handle and ends every process in the job.
///
/// The first call decides: a later call returns the same result and makes no
/// second job. A process that is already in a job (a terminal, a CI runner)
/// gets a nested job; Windows 8 and later allow that.
///
/// # Errors
///
/// The Win32 error if the job cannot be made or joined. The caller goes on
/// without it: only the clean-up after a crash is lost.
pub fn join_kill_on_close_job() -> io::Result<()> {
    // The raw OS error of the first call, so the answer can be copied.
    static JOINED: OnceLock<Option<i32>> = OnceLock::new();
    let failed = JOINED.get_or_init(|| create_and_join_job().err().map(|err| os_code(&err)));
    match failed {
        None => Ok(()),
        Some(code) => Err(io::Error::from_raw_os_error(*code)),
    }
}

fn os_code(err: &io::Error) -> i32 {
    // Every error here comes from `last_os_error`, so it has a code.
    err.raw_os_error().unwrap_or(0)
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
        // `job` is never closed, on purpose. It is the job's only handle, and
        // closing it is what ends every process in the job, this one too.
        // Windows closes it when this process ends.
    }
    Ok(())
}

/// A process started by [`spawn_detached`]. Dropping it closes the handle and
/// leaves the process running.
#[derive(Debug)]
pub struct DetachedProcess {
    process: OwnedHandle,
    pid: u32,
}

impl DetachedProcess {
    /// The process id, like `Child::id`.
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// How the process ended, if it already has. Never blocks. Takes
    /// `&mut self` like `Child::try_wait`, so callers read the same on every
    /// system.
    ///
    /// # Errors
    ///
    /// The Win32 error if Windows cannot say.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
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
/// (`bInheritHandles = FALSE`). The new process has no standard handles, so
/// what it prints goes nowhere. It inherits the caller's environment.
///
/// `program` is the exact file to run: it is never searched on `PATH`, and
/// nothing goes through a shell. `cwd` is the working folder of the new
/// process, or the caller's own if `None`.
///
/// # Errors
///
/// `InvalidInput` for a NUL inside `program`, an argument, or `cwd`, or a
/// command line longer than 32,767 characters. Otherwise the Win32 error of
/// `CreateProcessW`, for example `NotFound`.
pub fn spawn_detached(
    program: &Path,
    args: &[&OsStr],
    cwd: Option<&Path>,
) -> io::Result<DetachedProcess> {
    let application = wide(program.as_os_str())?;
    let mut command_line = command_line(program.as_os_str(), args)?;
    let cwd = cwd.map(|dir| wide(dir.as_os_str())).transpose()?;
    // SAFETY: `application`, `command_line`, and `cwd` are NUL-terminated
    // UTF-16 buffers that live through the call; `command_line` is mutable,
    // as `CreateProcessW` may write to it. `startup` is zeroed plain data
    // with its size set, so it names no handles and no desktop. `info` is
    // filled only on success; then both of its handles are open and owned
    // here: the thread handle is closed, and the process handle moves into
    // an `OwnedHandle`, which closes it on drop.
    unsafe {
        let mut startup: STARTUPINFOW = zeroed();
        startup.cb = size_of::<STARTUPINFOW>() as u32;
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

/// `text` as NUL-terminated UTF-16.
fn wide(text: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = text.encode_wide().collect();
    if wide.contains(&0) {
        return Err(invalid("a NUL character in a path or an argument"));
    }
    wide.push(0);
    Ok(wide)
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, reason)
}

/// The command line for `CreateProcessW`: the program in quotes, then every
/// argument, quoted the way the Microsoft C runtime (and Rust's `std::env`)
/// reads them back. NUL-terminated.
fn command_line(program: &OsStr, args: &[&OsStr]) -> io::Result<Vec<u16>> {
    let mut line = Vec::new();
    // Always quoted, so a space in the folder name cannot split the path.
    append_arg(&mut line, program, true)?;
    for arg in args {
        line.push(u16::from(b' '));
        append_arg(&mut line, arg, false)?;
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
fn append_arg(line: &mut Vec<u16>, arg: &OsStr, force_quotes: bool) -> io::Result<()> {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;

    let arg: Vec<u16> = arg.encode_wide().collect();
    if arg.contains(&0) {
        return Err(invalid("a NUL character in a path or an argument"));
    }
    let quotes = force_quotes
        || arg.is_empty()
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

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::io::Read;
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use super::*;

    /// The tests below start this test program again in a role, the way
    /// `marsa`, `marsad`, and a child of `marsad` run. A role is a "test"
    /// named `role_<name>`, started with `--exact` and one more argument,
    /// `marsa-test-dir=<folder>`. The test harness reads that argument as a
    /// filter that matches no test. Without it (a normal test run), a role
    /// passes at once.
    const DIR_ARG: &str = "marsa-test-dir=";
    /// With this argument, the `job_parent` role joins no job.
    const NO_JOB_ARG: &str = "marsa-test-no-job";

    fn line(program: &str, args: &[&str]) -> String {
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        let line = command_line(OsStr::new(program), &args).unwrap();
        assert_eq!(line.last(), Some(&0));
        String::from_utf16(&line[..line.len() - 1]).unwrap()
    }

    #[test]
    fn the_program_is_always_quoted_and_plain_arguments_are_not() {
        assert_eq!(
            line(r"C:\marsa\marsad.exe", &["--data-dir", r"C:\data"]),
            r#""C:\marsa\marsad.exe" --data-dir C:\data"#
        );
    }

    #[test]
    fn arguments_are_quoted_like_the_c_runtime_reads_them() {
        let cases = [
            ("", r#""""#),
            ("a b", r#""a b""#),
            ("a\tb", "\"a\tb\""),
            (r"C:\Program Files\x", r#""C:\Program Files\x""#),
            // A backslash before the closing quote is doubled.
            (r"C:\my data\", r#""C:\my data\\""#),
            // Without quotes, backslashes stay as they are.
            (r"C:\data\", r"C:\data\"),
            (r#"say "hi""#, r#""say \"hi\"""#),
            (r#"a\"b"#, r#"a\\\"b"#),
            (r#"a\\"b c"#, r#""a\\\\\"b c""#),
        ];
        for (arg, expected) in cases {
            assert_eq!(line("p", &[arg]), format!(r#""p" {expected}"#), "{arg:?}");
        }
    }

    #[test]
    fn a_nul_or_a_too_long_command_line_is_refused() {
        let nul = OsStr::new("a\0b");
        let long = "a".repeat(MAX_COMMAND_LINE);
        for err in [
            command_line(nul, &[]).unwrap_err(),
            command_line(OsStr::new("p"), &[nul]).unwrap_err(),
            wide(nul).unwrap_err(),
            command_line(OsStr::new("p"), &[OsStr::new(&long)]).unwrap_err(),
        ] {
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn a_missing_program_is_not_found() {
        let temp = TempDir::new().unwrap();
        let missing = temp.path().join("no-such-program.exe");
        let err = spawn_detached(&missing, &[], None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    /// The arguments that make this test program run only the role `role`
    /// in `dir`.
    fn role_args(role: &str, dir: &Path) -> Vec<String> {
        vec![
            "--exact".to_owned(),
            format!("platform::windows::tests::role_{role}"),
            "--nocapture".to_owned(),
            format!("{DIR_ARG}{}", dir.display()),
        ]
    }

    /// The folder of the running role, or `None` in a normal test run.
    fn role_dir() -> Option<PathBuf> {
        env::args().find_map(|arg| arg.strip_prefix(DIR_ARG).map(PathBuf::from))
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Stays until the file `stop` appears in `dir`, at most a minute, so a
    /// failed test leaves no process behind for long.
    fn stay_until_stopped(dir: &Path) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !dir.join("stop").exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Role `job_parent`: joins the job, starts `job_child` the way the
    /// supervisor starts Caddy, and stays, like the daemon.
    #[test]
    fn role_job_parent() {
        let Some(dir) = role_dir() else {
            return;
        };
        if !env::args().any(|arg| arg == NO_JOB_ARG) {
            join_kill_on_close_job().unwrap();
            // The second call makes no second job.
            join_kill_on_close_job().unwrap();
        }
        let mut child = Command::new(env::current_exe().unwrap())
            .args(role_args("job_child", &dir))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        stay_until_stopped(&dir);
        // Reached only when the test did not kill this role.
        let _ = child.wait();
    }

    /// Role `job_child`: listens on a loopback port, writes the port to the
    /// file `port`, and stays. While it lives, the port accepts.
    #[test]
    fn role_job_child() {
        let Some(dir) = role_dir() else {
            return;
        };
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        fs::write(dir.join("port.tmp"), port.to_string()).unwrap();
        fs::rename(dir.join("port.tmp"), dir.join("port")).unwrap();
        stay_until_stopped(&dir);
    }

    /// Kills the parent and says whether the child's port still accepts two
    /// seconds later.
    fn child_outlives_a_killed_parent(with_job: bool) -> bool {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let mut parent = Command::new(env::current_exe().unwrap());
        parent.args(role_args("job_parent", dir));
        if !with_job {
            parent.arg(NO_JOB_ARG);
        }
        let mut parent = parent
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let port_file = dir.join("port");
        wait_for("the child's port", || port_file.is_file());
        let port: u16 = fs::read_to_string(&port_file).unwrap().parse().unwrap();
        let accepts = || TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok();
        assert!(accepts(), "the child listens before the parent is killed");

        // `TerminateProcess`, like "End task": the parent runs no clean-up.
        parent.kill().unwrap();
        parent.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while accepts() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        let outlived = accepts();

        // A child that is still there ends now, before the folder goes.
        fs::write(dir.join("stop"), "").unwrap();
        wait_for("the child's exit", || !accepts());
        outlived
    }

    #[test]
    fn a_killed_process_takes_its_children_with_it_through_the_job() {
        assert!(!child_outlives_a_killed_parent(true));
    }

    /// The control: without the job, the same child stays.
    #[test]
    fn without_the_job_a_child_outlives_its_killed_parent() {
        assert!(child_outlives_a_killed_parent(false));
    }

    /// Role `piped_parent`: prints a line, starts `detached_child` with
    /// [`spawn_detached`], and exits at once, like `marsa` starting `marsad`.
    #[test]
    fn role_piped_parent() {
        let Some(dir) = role_dir() else {
            return;
        };
        println!("parent started");
        let args = role_args("detached_child", &dir);
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        spawn_detached(&env::current_exe().unwrap(), &args, Some(&dir)).unwrap();
    }

    /// Role `detached_child`: prints without standard handles, writes its
    /// working folder to `child-cwd`, stays until `stop` appears, and then
    /// writes `child-done`.
    #[test]
    fn role_detached_child() {
        let Some(dir) = role_dir() else {
            return;
        };
        println!("this goes nowhere");
        eprintln!("this goes nowhere");
        let cwd = env::current_dir().unwrap();
        fs::write(dir.join("child-cwd.tmp"), cwd.to_string_lossy().as_bytes()).unwrap();
        fs::rename(dir.join("child-cwd.tmp"), dir.join("child-cwd")).unwrap();
        stay_until_stopped(&dir);
        fs::write(dir.join("child-done"), "done").unwrap();
    }

    /// The M3 follow-up: a reader of the parent's stdout sees its end when
    /// the parent exits, while the detached child still runs.
    #[test]
    fn a_detached_process_does_not_hold_the_callers_pipe() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let mut parent = Command::new(env::current_exe().unwrap())
            .args(role_args("piped_parent", dir))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = parent.stdout.take().unwrap();
        let started = Instant::now();
        let mut output = String::new();
        // Returns when every copy of the pipe's write end is closed.
        stdout.read_to_string(&mut output).unwrap();
        let waited = started.elapsed();
        assert!(parent.wait().unwrap().success());
        assert!(output.contains("parent started"), "{output}");

        // The child runs on, in the folder it was given, and printing
        // without standard handles did not stop it.
        wait_for("the child's folder", || dir.join("child-cwd").is_file());
        assert!(!dir.join("child-done").exists());
        assert!(
            waited < Duration::from_secs(15),
            "the reader waited {waited:?}: the child held the pipe"
        );
        // Compared only, never handed to a program (`\\?\` paths).
        let cwd = fs::read_to_string(dir.join("child-cwd")).unwrap();
        assert_eq!(
            fs::canonicalize(cwd).unwrap(),
            fs::canonicalize(dir).unwrap()
        );

        fs::write(dir.join("stop"), "").unwrap();
        wait_for("the child's exit", || dir.join("child-done").is_file());
    }

    #[test]
    fn try_wait_reports_the_exit_code_only_after_the_exit() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let args = role_args("detached_child", dir);
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        let mut child = spawn_detached(&env::current_exe().unwrap(), &args, None).unwrap();
        assert_ne!(child.id(), 0);
        wait_for("the child's folder", || dir.join("child-cwd").is_file());
        assert!(child.try_wait().unwrap().is_none());

        fs::write(dir.join("stop"), "").unwrap();
        let mut status = None;
        wait_for("the child's exit", || {
            status = child.try_wait().unwrap();
            status.is_some()
        });
        // A test program whose one test passed exits with 0.
        assert_eq!(status.unwrap().code(), Some(0));
    }
}
