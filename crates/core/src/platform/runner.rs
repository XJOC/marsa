//! Every program Marsa runs goes through [`Runner`], so the tests run none:
//! they pass a fake that records each call and answers from fixtures.
//! [`RealRunner`] runs a program with an absolute path, a safe `PATH`, a time
//! limit, and at most [`MAX_OUTPUT`] bytes read from each pipe.

use std::ffi::OsString;
use std::io;
use std::io::Read;
use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

/// The most read from each pipe of a program. The rest is read and dropped,
/// so the program never waits on a full pipe.
pub const MAX_OUTPUT: u64 = 4 * 1024 * 1024;

/// The only `PATH` a program sees. Every program here is an absolute path, so
/// this only keeps the daemon's own `PATH` away from them.
const SAFE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// How often [`RealRunner`] checks whether the program has finished.
const POLL: Duration = Duration::from_millis(10);

/// How long the pipes may stay open after the program ended. Its output is
/// read all along, so only the last bytes are left by then.
pub const PIPE_GRACE: Duration = Duration::from_secs(2);

/// What a program printed, and how it ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOutput {
    /// The exit code, or `None` if a signal stopped the program.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl RunOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Runs the programs of setup. The daemon uses [`RealRunner`]; tests use a
/// fake, like `Process::fake()` in Laravel.
pub trait Runner: Send + Sync {
    /// Runs `program` with `args`, never through a shell, and waits at most
    /// `limit`. `&'static str`: a constant path in this file, never one from
    /// a request.
    ///
    /// # Errors
    ///
    /// The error of the start; kind `TimedOut` after `limit`, and kind
    /// `Interrupted` once `stop` is cancelled. In both cases the program is
    /// killed first.
    fn run(
        &self,
        program: &'static str,
        args: &[OsString],
        limit: Duration,
        stop: &CancellationToken,
    ) -> io::Result<RunOutput>;
}

/// `std::process::Command`, with an empty environment except [`SAFE_PATH`],
/// no stdin, and the working folder `/`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealRunner;

impl Runner for RealRunner {
    fn run(
        &self,
        program: &'static str,
        args: &[OsString],
        limit: Duration,
        stop: &CancellationToken,
    ) -> io::Result<RunOutput> {
        if !Path::new(program).is_absolute() {
            let reason = "the program is not an absolute path";
            return Err(io::Error::new(io::ErrorKind::InvalidInput, reason));
        }
        // Checked before the start too, so a stopping daemon never opens the
        // admin prompt, not even for a moment.
        if stop.is_cancelled() {
            let reason = "the daemon is stopping";
            return Err(io::Error::new(io::ErrorKind::Interrupted, reason));
        }
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .env("PATH", SAFE_PATH)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Not the daemon's working folder. `/` is a Unix path, and setup runs
        // on macOS and Linux only for now (Windows in task M4-19).
        #[cfg(unix)]
        command.current_dir("/");
        let mut child = command.spawn()?;
        finish(&mut child, limit, stop)
    }
}

/// Waits for `child` at most `limit`, or until `stop`, and collects its
/// output. On any error the child is killed and reaped before this returns.
fn finish(child: &mut Child, limit: Duration, stop: &CancellationToken) -> io::Result<RunOutput> {
    let result = collect(child, Instant::now() + limit, stop);
    if result.is_err() {
        // Best effort: the first error is the one to report.
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

/// Waits for `child` until `deadline` or `stop`, and collects its output.
///
/// At `deadline` or `stop` the child is killed. A kill that is refused
/// (`PermissionDenied`) means the child now runs as root: on Linux `pkexec`
/// becomes the helper after the password, and the daemon cannot stop it.
/// Then the wait goes on until it ends, within its own limits, and its
/// answer is used: reporting a timeout would hide what it did (review of
/// #120).
fn collect(
    child: &mut Child,
    deadline: Instant,
    stop: &CancellationToken,
) -> io::Result<RunOutput> {
    let stdout = read_in_background(child.stdout.take())?;
    let stderr = read_in_background(child.stderr.take())?;
    let mut unstoppable = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let ended = if unstoppable {
            None
        } else if stop.is_cancelled() {
            let reason = "the daemon is stopping";
            Some(io::Error::new(io::ErrorKind::Interrupted, reason))
        } else if Instant::now() >= deadline {
            Some(timed_out())
        } else {
            None
        };
        if let Some(err) = ended {
            match child.kill() {
                Err(kill) if kill.kind() == io::ErrorKind::PermissionDenied => {
                    tracing::warn!(
                        "the helper runs as root and cannot be stopped; waiting for it to end"
                    );
                    unstoppable = true;
                }
                _ => return Err(err),
            }
        }
        thread::sleep(POLL);
    };
    // From the end of the program, not from `deadline`: a program that ends
    // just before its limit still gives its output. A program it started may
    // hold a pipe open, so the wait is short.
    let pipes = Instant::now() + PIPE_GRACE;
    Ok(RunOutput {
        code: status.code(),
        stdout: receive(&stdout, pipes)?,
        stderr: receive(&stderr, pipes)?,
    })
}

/// Reads a pipe on its own thread, so a program that fills one pipe never
/// blocks while the other is read. Keeps the first [`MAX_OUTPUT`] bytes.
fn read_in_background<R>(pipe: Option<R>) -> io::Result<Receiver<io::Result<Vec<u8>>>>
where
    R: Read + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("setup-pipe".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            let result = match pipe {
                Some(mut pipe) => {
                    let kept = (&mut pipe).take(MAX_OUTPUT).read_to_end(&mut bytes);
                    kept.and_then(|_| io::copy(&mut pipe, &mut io::sink()))
                        .map(|_| bytes)
                }
                None => Ok(bytes),
            };
            // After a timeout nobody waits for this, so a failed send is fine.
            let _ = sender.send(result);
        })?;
    Ok(receiver)
}

/// The output of one pipe, read after the program ended. Not `TimedOut` if
/// the pipe stays open past `deadline`: the program itself finished in time.
fn receive(output: &Receiver<io::Result<Vec<u8>>>, deadline: Instant) -> io::Result<Vec<u8>> {
    let left = deadline.saturating_duration_since(Instant::now());
    match output.recv_timeout(left) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(io::Error::other(
            "the program ended, but its output stayed open",
        )),
        Err(RecvTimeoutError::Disconnected) => Err(io::Error::other("the output reader stopped")),
    }
}

fn timed_out() -> io::Error {
    let reason = "the program did not finish in time";
    io::Error::new(io::ErrorKind::TimedOut, reason)
}

#[cfg(test)]
mod tests {

    use super::*;

    #[cfg(unix)]
    use std::ffi::OsString;
    use std::io;
    #[cfg(unix)]
    use std::process::Command;
    #[cfg(unix)]
    use std::process::Stdio;
    #[cfg(unix)]
    use std::thread;
    use std::time::Duration;
    #[cfg(unix)]
    use std::time::Instant;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn a_relative_program_is_refused() {
        let stop = CancellationToken::new();
        let err = RealRunner.run("echo", &[], Duration::from_secs(10), &stop);
        assert_eq!(err.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    /// [`RealRunner`] with real programs: only `/bin/sleep`, `/bin/echo`, and
    /// `/usr/bin/false`, which change nothing.
    #[cfg(unix)]
    mod real_runner {
        use super::*;
        use std::os::unix::process::ExitStatusExt;

        fn args(args: &[&str]) -> Vec<OsString> {
            args.iter().map(OsString::from).collect()
        }

        #[test]
        fn a_program_past_its_limit_is_killed_and_reaped() {
            let stop = CancellationToken::new();
            let limit = Duration::from_millis(100);
            let started = Instant::now();
            let err = RealRunner.run("/bin/sleep", &args(&["5"]), limit, &stop);
            assert_eq!(err.unwrap_err().kind(), io::ErrorKind::TimedOut);
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "{:?}",
                started.elapsed()
            );

            // The same wait on a child the test holds, to see it is gone.
            let mut child = Command::new("/bin/sleep")
                .arg("5")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let err = finish(&mut child, limit, &stop).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::TimedOut);
            // Reaped (its status is known) and ended by SIGKILL.
            let status = child.try_wait().unwrap().expect("the child was reaped");
            assert_eq!(status.signal(), Some(9));
        }

        #[test]
        fn a_stop_before_the_call_starts_nothing() {
            let stop = CancellationToken::new();
            stop.cancel();
            let started = Instant::now();
            let limit = Duration::from_secs(10);
            let err = RealRunner.run("/bin/sleep", &args(&["5"]), limit, &stop);
            assert_eq!(err.unwrap_err().kind(), io::ErrorKind::Interrupted);
            assert!(started.elapsed() < Duration::from_secs(1));
            // Not `NotFound`: the stop is seen before any start.
            let err = RealRunner.run("/marsa-test/no-such-program", &[], limit, &stop);
            assert_eq!(err.unwrap_err().kind(), io::ErrorKind::Interrupted);
        }

        #[test]
        fn a_stop_during_the_call_ends_it() {
            let stop = CancellationToken::new();
            let canceller = thread::spawn({
                let stop = stop.clone();
                move || {
                    thread::sleep(Duration::from_millis(100));
                    stop.cancel();
                }
            });
            let started = Instant::now();
            let limit = Duration::from_secs(10);
            let err = RealRunner.run("/bin/sleep", &args(&["5"]), limit, &stop);
            assert_eq!(err.unwrap_err().kind(), io::ErrorKind::Interrupted);
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "{:?}",
                started.elapsed()
            );
            canceller.join().unwrap();
        }

        #[test]
        fn output_and_exit_code_come_back() {
            let stop = CancellationToken::new();
            let limit = Duration::from_secs(10);
            let echo = RealRunner.run("/bin/echo", &args(&["hello", "marsa"]), limit, &stop);
            let want = RunOutput {
                code: Some(0),
                stdout: b"hello marsa\n".to_vec(),
                stderr: Vec::new(),
            };
            assert_eq!(echo.unwrap(), want);
            let failed = RealRunner.run("/usr/bin/false", &[], limit, &stop).unwrap();
            assert_eq!(failed.code, Some(1));
            assert!(!failed.success());
        }
    }
}
