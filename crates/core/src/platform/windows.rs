//! Windows-only process code (architecture.md section 9, ADR 0017). For
//! `marsad` and for `spawn_marsad` only, so it is not public outside the
//! crate.
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
//! - [`current_user_sid`] and [`helper_pipe`]: what `marsad` needs to use
//!   the helper service (ADR 0018), public for the `marsa` crate.
//!
//! The only `unsafe` code of `marsa-core` is in this module and its
//! `helper_pipe` (AGENTS.md section 6). The blocks are short, and every
//! Win32 result is checked.

pub mod helper_pipe;

use std::ffi::OsStr;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;
use std::ptr::{null, null_mut};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessW, DETACHED_PROCESS, GetCurrentProcess, GetExitCodeProcess,
    OpenProcessToken, PROCESS_INFORMATION, STARTF_FORCEOFFFEEDBACK, STARTUPINFOW,
    WaitForSingleObject,
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
fn check_program(program: &Path) -> io::Result<()> {
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

/// The SID of the user this process runs as, in the form Windows prints
/// (`S-1-5-21-...`). The helper service serves this user only (ADR 0018).
///
/// # Errors
///
/// The Win32 error.
pub fn current_user_sid() -> io::Result<String> {
    let mut token = null_mut();
    // SAFETY: the pseudo handle of this process needs no close; `token` is a
    // valid out pointer, and the token is checked, then owned.
    let token = unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        OwnedHandle::from_raw_handle(token)
    };
    // A TOKEN_USER and its SID fit in 256 bytes; `u64` keeps the buffer
    // aligned for the struct.
    let mut buffer = [0u64; 32];
    let mut used = 0;
    // SAFETY: the buffer and the size passed match; on success it holds a
    // TOKEN_USER whose SID pointer points into the same buffer.
    let read = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size_of::<[u64; 32]>() as u32,
            &mut used,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut text: *mut u16 = null_mut();
    // SAFETY: `buffer` holds a TOKEN_USER (above) and outlives these lines.
    // The string `ConvertSidToStringSidW` allocates is read up to its NUL,
    // copied, and freed with `LocalFree`.
    unsafe {
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut len = 0;
        while *text.add(len) != 0 {
            len += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        Ok(sid)
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

/// The command line for `CreateProcessW`: the program between quotes as it
/// is, then every argument, quoted the way the Microsoft C runtime (and
/// Rust's `std::env`) reads them back. NUL-terminated.
///
/// The program is `argv[0]`, which the C runtime reads by its own rule: it
/// runs from the opening quote to the next quote, and backslashes are never
/// escapes there. So it is written as it is, like std does, and a quote in it
/// is refused ([`check_program`]; here too, for the tests).
fn command_line(program: &OsStr, args: &[&OsStr]) -> io::Result<Vec<u16>> {
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
fn append_arg(line: &mut Vec<u16>, arg: &OsStr) -> io::Result<()> {
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

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::io::Read;
    use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
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

    /// `argv[0]` has its own rule: between quotes, as it is.
    #[test]
    fn the_program_is_written_as_it_is_and_a_quote_in_it_is_refused() {
        assert_eq!(line(r"C:\my data\x.exe", &[]), r#""C:\my data\x.exe""#);
        let err = command_line(OsStr::new(r#"C:\a"b.exe"#), &[]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// A batch file would run through `cmd.exe` (CVE-2024-24576), and a
    /// relative path would depend on the working folder.
    #[test]
    fn only_an_absolute_exe_without_quotes_is_started() {
        for good in [r"C:\marsa\marsad.exe", r"C:\marsa\MARSAD.EXE"] {
            assert!(check_program(Path::new(good)).is_ok(), "{good}");
        }
        let bad = [
            "marsad.exe",
            r"marsa\marsad.exe",
            r"C:\marsa\run.bat",
            r"C:\marsa\run.cmd",
            r"C:\marsa\run.CMD",
            r"C:\marsa\marsad",
            r"C:\marsa\marsad.exe.txt",
            r#"C:\ma"rsa\marsad.exe"#,
        ];
        for bad in bad {
            let err = spawn_detached(Path::new(bad), &[], None).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad}");
        }
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
            // A second call gives the same answer (`OnceLock`: no second job).
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

    /// Whether something accepts on `port`. With a short limit: on Windows a
    /// refused loopback connect is retried for about 2 s, and a live
    /// listener answers at once.
    fn accepts(port: u16) -> bool {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_ok()
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
        let accepts = || accepts(port);
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
        // The folder is the child's working folder until it has exited, and
        // Windows keeps a folder in use. Gone here, so the drop leaves none.
        wait_for("the folder to be free", || fs::remove_dir_all(dir).is_ok());
    }

    /// Role `exiting_child`: says it runs, waits for `stop`, and exits with
    /// the code of a second `marsad` that found the lock taken, which the
    /// client reads through `try_wait`.
    #[test]
    fn role_exiting_child() {
        let Some(dir) = role_dir() else {
            return;
        };
        fs::write(dir.join("started"), "").unwrap();
        stay_until_stopped(&dir);
        std::process::exit(super::super::spawn::EXIT_ALREADY_RUNNING);
    }

    #[test]
    fn try_wait_reports_the_exit_code_only_after_the_exit() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let args = role_args("exiting_child", dir);
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        let mut child = spawn_detached(&env::current_exe().unwrap(), &args, None).unwrap();
        assert_ne!(child.id(), 0);
        wait_for("the child's start", || dir.join("started").is_file());
        assert!(child.try_wait().unwrap().is_none());

        fs::write(dir.join("stop"), "").unwrap();
        let mut status = None;
        wait_for("the child's exit", || {
            status = child.try_wait().unwrap();
            status.is_some()
        });
        assert_eq!(status.unwrap().code(), Some(3));
    }

    /// Arguments that the quoting must carry. None starts with `-`: the
    /// test harness would read it as an option.
    const AWKWARD: [&str; 10] = [
        "",
        "a b",
        "a\tb",
        r#"say "hi""#,
        r#"a\"b"#,
        r"C:\my data\",
        r"C:\data\",
        r#"a\\"b c"#,
        "\u{645}\u{631}\u{633}\u{649} \u{fc}",
        "end",
    ];

    /// Role `echo_args`: writes the arguments after its own four to
    /// `args.json`, then exits.
    #[test]
    fn role_echo_args() {
        let Some(dir) = role_dir() else {
            return;
        };
        let args: Vec<String> = env::args().skip(5).collect();
        let json = serde_json::to_string(&args).unwrap();
        fs::write(dir.join("args.json.tmp"), json).unwrap();
        fs::rename(dir.join("args.json.tmp"), dir.join("args.json")).unwrap();
    }

    /// The round trip: what `spawn_detached` sends is what the program reads.
    #[test]
    fn arguments_reach_the_program_as_they_were_sent() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        let mut args = role_args("echo_args", dir);
        args.extend(AWKWARD.map(str::to_owned));
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        let mut child = spawn_detached(&env::current_exe().unwrap(), &args, None).unwrap();
        wait_for("the child's exit", || child.try_wait().unwrap().is_some());
        let json = fs::read_to_string(dir.join("args.json")).unwrap();
        let read: Vec<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(read, AWKWARD);
    }

    /// Read only: this process's user, in the form the helper service
    /// compares (ADR 0018).
    #[test]
    fn the_current_user_sid_is_a_user_sid() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
        assert!(
            sid.bytes()
                .all(|b| b.is_ascii_digit() || b == b'S' || b == b'-')
        );
    }
}
