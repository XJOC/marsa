//! Runs the `marsa` binary with `--data-dir` in a temp folder. No test here
//! uses the network: the installer downloads the manifest before it checks
//! the key, so `manifest_no_trusted_key` cannot be reached offline. The
//! install test stops earlier, at the install lock. `tests/install.rs` runs
//! whole installs through the daemon, with a fake installer.
//!
//! The daemon tests (task M3-09) run the real `marsa` and `marsad`: `marsa`
//! starts the `marsad` next to it in `target/debug/`, as it does for a user.
//! `marsa runtimes` and `marsa install` are daemon calls too (task M3-12).
//! No fixed sleeps: each wait polls with a generous limit.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use marsa_core::caddy::CaddyPaths;
use marsa_core::platform::daemon_file::{DaemonFile, read_daemon_file};
use tempfile::TempDir;

fn marsa(data_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_marsa"))
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .env_remove("MARSA_LOG")
        .output()
        .expect("run marsa")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

#[test]
fn runtimes_on_an_empty_data_dir() {
    let data = DaemonDir::new();
    data.expect(
        &["runtimes"],
        0,
        "PHP       none installed\n\
         Node.js   none installed\n\
         Composer  none installed\n\
         Caddy     none installed\n",
        "",
    );
}

#[test]
fn runtimes_lists_installed_versions() {
    let data = DaemonDir::new();
    let runtimes = data.path().join("runtimes");
    for dir in ["php/8.4.23", "php/8.3.27", "node/22.11.0", "composer/2.8.4"] {
        fs::create_dir_all(runtimes.join(dir)).expect("mkdir");
    }
    data.expect(
        &["runtimes"],
        0,
        "PHP       8.3.27, 8.4.23\n\
         Node.js   22.11.0\n\
         Composer  2.8.4\n\
         Caddy     none installed\n",
        "",
    );
    data.expect(&["runtimes", "php"], 0, "PHP       8.3.27, 8.4.23\n", "");
}

/// The real installer in the real `marsad`, up to its file lock: another
/// process (this test) holds it, so the daemon's install ends as `failed`.
#[test]
fn install_fails_with_its_code_when_another_install_runs() {
    let data = DaemonDir::new();
    let lock_dir = data.path().join("runtimes").join(".tmp");
    fs::create_dir_all(&lock_dir).expect("mkdir");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_dir.join("install.lock"))
        .expect("open the lock");
    lock.try_lock().expect("take the lock");

    data.expect(
        &["install", "php", "8.4"],
        1,
        "",
        "marsa: Another install is running. Wait for it to end, then try again. \
         [install_busy]\n",
    );
    drop(lock);
    assert!(!data.path().join("runtimes").join("php").exists());
}

#[test]
fn bad_arguments_exit_with_code_2() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let out = marsa(tmp.path(), &["install", "ruby"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("ruby"), "{}", text(&out.stderr));
}

/// `marsa uninstall --all` with no terminal to ask on and no `--yes`
/// (`Command::output` gives the child no stdin): it refuses before anything,
/// so no daemon starts and the data folder stays empty. Off macOS it says the
/// uninstall is not supported there, also before anything.
#[test]
fn uninstall_without_a_terminal_and_without_yes_changes_nothing() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let data = tmp.path().join("data");
    fs::create_dir(&data).expect("mkdir");
    for args in [
        &["uninstall", "--all"][..],
        &["uninstall", "--all", "--delete-data"],
    ] {
        let out = marsa(&data, args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let stderr = text(&out.stderr);
        let code = if cfg!(target_os = "macos") {
            "[uninstall_all_needs_yes]"
        } else {
            "[uninstall_not_supported]"
        };
        assert!(stderr.contains(code), "{args:?}: {stderr}");
        assert_eq!(text(&out.stdout), "", "{args:?}");
        let left: Vec<_> = fs::read_dir(&data).expect("read").collect();
        assert!(left.is_empty(), "{args:?}: {left:?}");
    }
    // Without --all it is not a command yet.
    let out = marsa(&data, &["uninstall", "--yes"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn help_and_version_work() {
    let tmp = tempfile::tempdir().expect("temp dir");
    for args in [
        &["install", "--help"][..],
        &["runtimes", "--help"],
        &["--version"],
    ] {
        let out = marsa(tmp.path(), args);
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
    }
    let out = marsa(tmp.path(), &["--version"]);
    assert_eq!(
        text(&out.stdout),
        format!("marsa {}\n", env!("CARGO_PKG_VERSION"))
    );
}

/// For the daemon to start or stop, and for a followed log to end. Longer
/// than `marsa daemon stop`'s own 17 s.
const LIMIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(20);

/// A data folder for tests that start `marsad`. Dropping it stops the daemon,
/// also when the test failed, then deletes the folder.
struct DaemonDir {
    data: TempDir,
    /// The output files of each `marsa` run, outside the data folder.
    outputs: TempDir,
}

impl DaemonDir {
    fn new() -> Self {
        Self {
            data: TempDir::new().expect("temp data folder"),
            outputs: TempDir::new().expect("temp output folder"),
        }
    }

    fn path(&self) -> &Path {
        self.data.path()
    }

    /// Runs `marsa`, with stdout and stderr in files rather than pipes. On
    /// Windows a `marsad` that `marsa` starts inherits the handles `marsa`
    /// inherited, so it would hold a pipe open until it exits, and
    /// `Command::output` would wait for it. A file has no end to wait for.
    fn marsa(&self, args: &[&str]) -> Output {
        let (stdout, stderr) = self.output_files();
        let status = self
            .command(args, &stdout, &stderr)
            .status()
            .expect("run marsa");
        Output {
            status,
            stdout: fs::read(&stdout).expect("read stdout"),
            stderr: fs::read(&stderr).expect("read stderr"),
        }
    }

    /// Starts `marsa` and returns at once. Output in files, as in
    /// [`DaemonDir::marsa`].
    fn spawn(&self, args: &[&str]) -> Running {
        let (stdout, stderr) = self.output_files();
        let child = self
            .command(args, &stdout, &stderr)
            .spawn()
            .expect("start marsa");
        Running {
            child,
            stdout,
            stderr,
        }
    }

    fn command(&self, args: &[&str], stdout: &Path, stderr: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_marsa"));
        command
            .arg("--data-dir")
            .arg(self.path())
            .args(args)
            .env_remove("MARSA_LOG")
            .stdin(Stdio::null())
            .stdout(File::create(stdout).expect("stdout file"))
            .stderr(File::create(stderr).expect("stderr file"));
        command
    }

    /// Two new files: each run gets its own, as an old `marsad` may still
    /// hold the files of the run that started it.
    fn output_files(&self) -> (PathBuf, PathBuf) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = self.outputs.path();
        (dir.join(format!("{n}.out")), dir.join(format!("{n}.err")))
    }

    fn daemon_file(&self) -> DaemonFile {
        read_daemon_file(&self.path().join("run"))
            .unwrap_or_else(|err| panic!("no daemon.json ({err}); log:\n{}", self.log()))
    }

    fn log(&self) -> String {
        let path = self.path().join("logs").join("marsad.log");
        fs::read_to_string(path).unwrap_or_default()
    }

    /// Checks one run: its exit code, stdout, and stderr.
    #[track_caller]
    fn expect(&self, args: &[&str], code: i32, stdout: &str, stderr: &str) {
        let out = self.marsa(args);
        let context = format!(
            "marsa {args:?}\nstdout:\n{}\nstderr:\n{}\nmarsad.log:\n{}",
            text(&out.stdout),
            text(&out.stderr),
            self.log()
        );
        assert_eq!(out.status.code(), Some(code), "{context}");
        assert_eq!(text(&out.stdout), stdout, "{context}");
        assert_eq!(text(&out.stderr), stderr, "{context}");
    }
}

impl Drop for DaemonDir {
    fn drop(&mut self) {
        // `daemon stop` waits until the daemon has let go of its lock, and
        // exits with 0 when none runs. Its result cannot fail the test here.
        let _ = Command::new(env!("CARGO_BIN_EXE_marsa"))
            .arg("--data-dir")
            .arg(self.data.path())
            .args(["daemon", "stop"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// A `marsa` that runs on. Dropping it kills the process.
struct Running {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl Running {
    fn stderr(&self) -> String {
        text(&fs::read(&self.stderr).unwrap_or_default())
    }

    /// Waits until stderr holds `wanted`.
    fn wait_for_stderr(&mut self, wanted: &str) {
        let started = Instant::now();
        while !self.stderr().contains(wanted) {
            if let Some(status) = self.child.try_wait().expect("check marsa") {
                panic!("marsa exited with {status}; stderr:\n{}", self.stderr());
            }
            assert!(
                started.elapsed() < LIMIT,
                "no {wanted:?} on stderr:\n{}",
                self.stderr()
            );
            thread::sleep(POLL);
        }
    }

    /// Waits until the process exits, then gives its output.
    fn wait(&mut self) -> Output {
        let started = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("check marsa") {
                break status;
            }
            assert!(
                started.elapsed() < LIMIT,
                "marsa still runs; stderr:\n{}",
                self.stderr()
            );
            thread::sleep(POLL);
        };
        Output {
            status,
            stdout: fs::read(&self.stdout).expect("read stdout"),
            stderr: fs::read(&self.stderr).expect("read stderr"),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // Errors mean the process has already exited.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn running(file: &DaemonFile) -> String {
    format!(
        "marsad is running: version {}, PID {}, port {}.\n",
        env!("CARGO_PKG_VERSION"),
        file.pid,
        file.port
    )
}

const NOT_RUNNING: &str = "marsad is not running.\n";

#[test]
fn daemon_start_status_services_and_stop() {
    let data = DaemonDir::new();

    // Neither `status` nor `stop` starts the daemon.
    data.expect(&["daemon", "status"], 0, NOT_RUNNING, "");
    data.expect(&["daemon", "stop"], 0, NOT_RUNNING, "");
    assert!(!data.path().join("run").join("daemon.json").exists());

    let out = data.marsa(&["daemon", "start"]);
    assert!(
        out.status.success(),
        "{}\nlog:\n{}",
        text(&out.stderr),
        data.log()
    );
    let file = data.daemon_file();
    assert_eq!(text(&out.stdout), running(&file));
    assert_eq!(text(&out.stderr), "");
    let bearer = file.token.bearer_header();
    let token = bearer.trim_start_matches("Bearer ");
    assert_eq!(token.len(), 64);
    assert!(!text(&out.stdout).contains(token));

    data.expect(&["daemon", "status"], 0, &running(&file), "");
    // A second start finds the same daemon.
    data.expect(&["daemon", "start"], 0, &running(&file), "");

    data.expect(
        &["services"],
        0,
        "SERVICE  STATE    PID  RESTARTS\n\
         caddy    stopped  -    0\n",
        "",
    );

    let caddy = CaddyPaths::in_dir(data.path()).runtime_dir();
    let not_installed = format!(
        "Caddy is not installed in {}. Run \"marsa install caddy\", then try again.",
        caddy.display()
    );
    data.expect(
        &["services", "start", "caddy"],
        1,
        "",
        &format!("marsa: {not_installed} [caddy_not_installed]\n"),
    );
    // The table shows the failed start, and why.
    data.expect(
        &["services"],
        0,
        &format!(
            "SERVICE  STATE   PID  RESTARTS\n\
             caddy    failed  -    0\n\
             caddy failed: {not_installed} [caddy_not_installed]\n"
        ),
        "",
    );
    data.expect(
        &["services", "restart", "nope"],
        1,
        "",
        "marsa: There is no service named \"nope\". Run \"marsa services\" to see them. \
         [service_not_found]\n",
    );
    data.expect(&["logs", "caddy"], 0, "", "No lines from caddy yet.\n");

    data.expect(&["daemon", "stop"], 0, "Stopped marsad.\n", "");
    data.expect(&["daemon", "status"], 0, NOT_RUNNING, "");
}

#[test]
fn a_followed_log_ends_when_the_daemon_stops() {
    let data = DaemonDir::new();
    // `logs` starts the daemon itself.
    let mut follow = data.spawn(&["logs", "caddy", "--follow"]);
    follow.wait_for_stderr("Press Ctrl+C to stop.");
    let file = data.daemon_file();
    data.expect(&["daemon", "status"], 0, &running(&file), "");

    data.expect(&["daemon", "stop"], 0, "Stopped marsad.\n", "");
    let out = follow.wait();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(
        text(&out.stderr),
        "Showing new lines from caddy as they come. Press Ctrl+C to stop.\n\
         The daemon closed the log.\n"
    );
}
