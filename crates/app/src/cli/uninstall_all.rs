//! `marsa uninstall --all`: take Marsa out of this system (macOS). Task
//! M5-13.
//!
//! 1. Prints what will be removed, and asks for `yes` unless `--yes` is
//!    given. Without a terminal to ask on, and without `--yes`, it stops
//!    there: nothing has changed, and no daemon has started.
//! 2. One call to `marsad` (`POST /v1/uninstall`, in
//!    `daemon/uninstall_api.rs`), started if needed. The daemon asks macOS
//!    for the administrator password once and runs `marsa-helper`, then takes
//!    the user's trust away and deletes the login file. This prints one line
//!    per step. A cancelled prompt ends the command: nothing was removed.
//!    Ctrl+C while it waits stops only this command: the uninstall goes on in
//!    `marsad`, which writes each step to `logs/marsad.log`.
//! 3. The daemon stops after its answer; this waits until it has, and then
//!    watches a few seconds more: the Marsa window starts a daemon again
//!    every second, and the menu-bar icon on a click.
//! 4. Asks on its own whether to delete the data folder, `no` by default.
//!    `--delete-data` answers yes; `--yes` alone leaves it as no. Only the
//!    folder `directories` gives, never one from `--data-dir`, only when
//!    every step went well (the next uninstall needs the record of the
//!    trusted certificate that is in it), and only with no daemon on it. The
//!    folder is renamed first, in one step, then deleted under the new name,
//!    so a daemon that starts meanwhile cannot write into it.
//! 5. Prints what is left to do by hand: quitting the window and the
//!    menu-bar icon, the `PATH` line in the shell's startup file, and the
//!    programs if they are outside the data folder.
//!
//! Exit code 1 if anything did not happen as asked.

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use marsa_core::client::{self, ClientError};
use marsa_core::config;
use marsa_core::recipes::Platform;
use marsa_core::types::{OpStatus, UninstallReport};

use super::Failure;
use super::daemon::{ClientFailure, connect, failure};
use super::sites::printable;
use super::strings::{self, Details};
use crate::daemon::setup_api::user_trust;

/// How long a stopped daemon is watched for a restart. The Marsa window
/// tries to start one every second.
const RESTART_WATCH: Duration = Duration::from_secs(3);
/// How often the daemon lock is looked at meanwhile.
const RESTART_POLL: Duration = Duration::from_millis(100);

/// The arguments of `marsa uninstall --all`. `bin/marsa.rs` flattens them
/// into `marsa uninstall`, next to the `<TOOL> <VERSION>` of task M5-07, and
/// requires either `--all` or a tool, never both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::Args)]
pub struct UninstallAllArgs {
    /// Remove Marsa from this system, instead of one version.
    #[arg(long)]
    pub all: bool,
    /// Do not ask before removing Marsa. The data folder is still kept
    /// unless --delete-data is given too.
    #[arg(long, requires = "all")]
    pub yes: bool,
    /// Also delete the data folder: the installed runtimes, the settings,
    /// and the certificate authority. Never a folder given with --data-dir.
    #[arg(long, requires = "all")]
    pub delete_data: bool,
}

/// Runs the uninstall. `data_dir_given`: the folder came from `--data-dir`,
/// so it is never deleted.
pub async fn run(args: &UninstallAllArgs, data_dir: &Path, data_dir_given: bool) -> ExitCode {
    let mut terminal = Terminal;
    // Before the question: the daemon would answer `uninstall_not_supported`
    // only after the user typed yes.
    if !is_macos(Platform::current()) {
        return own_failure("uninstall_not_supported", None).report();
    }
    let deletable = !data_dir_given;
    let plan = strings::uninstall_all_plan(data_dir, deletable, deletable && args.delete_data);
    eprint!("{plan}");
    // Read before `until_ctrl_c` takes Ctrl+C over: here it still ends the
    // process at once, before anything changed.
    if let Err(failure) = confirm(args, &mut terminal) {
        return failure.report();
    }
    let client = match connect(data_dir).await {
        Ok(client) => client,
        Err(failure) => return failure.report(),
    };
    eprintln!("{}", strings::UNINSTALL_ALL_STARTED);
    let report = match until_ctrl_c(client.uninstall_all()).await {
        Some(Ok(report)) => report,
        Some(Err(err)) => {
            if !refused_at_once(&err) {
                eprintln!("{}", strings::UNINSTALL_ALL_RESULT_UNKNOWN);
            }
            return failure(&err, data_dir, None).report();
        }
        None => {
            let log = data_dir.join("logs").join("marsad.log");
            eprintln!("{}", strings::uninstall_all_ctrl_c(&log));
            return ExitCode::FAILURE;
        }
    };
    print!("{}", render(&report, data_dir));
    if let Some(failure) = error(&report) {
        return failure.report();
    }
    let complete = is_complete(&report);
    let mut ok = complete;
    if !complete {
        println!("{}", strings::UNINSTALL_ALL_INCOMPLETE);
    }
    // From here on Ctrl+C is this process's to handle (`until_ctrl_c`
    // installed the handler), so every wait races it.
    let Some(end) = until_ctrl_c(wait_for_exit(data_dir)).await else {
        eprintln!("\n{}", strings::UNINSTALL_ALL_STOP_CTRL_C);
        return ExitCode::FAILURE;
    };
    match &end {
        DaemonEnd::Stopped => println!("{}", strings::DAEMON_STOPPED),
        DaemonEnd::Back => eprintln!("{}", strings::UNINSTALL_ALL_DAEMON_BACK),
        DaemonEnd::NotStopped(failure) => eprintln!("{}", failure.line()),
    }
    let stopped = end == DaemonEnd::Stopped;
    ok &= stopped;
    let decided = data_folder(
        args,
        data_dir_given,
        complete,
        stopped,
        terminal.is_terminal(),
    );
    let folder = match decided {
        Some(folder) => folder,
        None => match ask_until_ctrl_c(strings::UNINSTALL_ALL_DATA_QUESTION).await {
            Asked::Line(answer) => folder_for(&answer),
            Asked::Closed => DataFolder::Keep,
            Asked::CtrlC => {
                eprintln!();
                println!("{}", strings::data_kept_ctrl_c(data_dir));
                DataFolder::Keep
            }
        },
    };
    let deleted = match folder {
        DataFolder::Delete => match delete_when_stopped(data_dir) {
            Ok(()) => {
                println!("{}", strings::data_deleted(data_dir));
                true
            }
            Err(failure) => {
                eprintln!("{}", failure.line());
                ok = false;
                false
            }
        },
        DataFolder::Keep => false,
        DataFolder::KeepForRetry => {
            println!("{}", strings::data_kept_for_retry(data_dir));
            false
        }
        DataFolder::KeepDaemonRuns => {
            println!("{}", strings::data_kept_daemon_runs(data_dir));
            false
        }
        DataFolder::NotOurs => {
            if args.delete_data {
                let code = "uninstall_all_data_dir_not_default";
                eprintln!("{}", own_failure(code, Some(data_dir)).line());
                ok = false;
            }
            false
        }
    };
    let programs = programs_dir();
    let default_dir = config::app_data_dir().ok();
    let left = Leftovers {
        data_dir,
        data_left: !deleted
            && matches!(
                folder,
                DataFolder::Keep | DataFolder::KeepDaemonRuns | DataFolder::NotOurs
            ),
        programs: programs.as_deref(),
        default_dir: default_dir.as_deref(),
    };
    print!("{}", left.render());
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// `answer`, or `None` once the user pressed Ctrl+C. If Ctrl+C cannot be
/// listened for, it keeps its default and ends the process at once, which
/// stops only this command too.
async fn until_ctrl_c<T>(answer: impl Future<Output = T>) -> Option<T> {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %err, "cannot listen for Ctrl+C");
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        () = ctrl_c => None,
        answer = answer => Some(answer),
    }
}

/// The daemon refused before it did anything: another setup or uninstall
/// runs (409), or this system has no uninstall (501). Any other answer, a
/// 500 from a job that panicked for one, may come after some steps ran.
fn refused_at_once(err: &ClientError) -> bool {
    matches!(
        err,
        ClientError::Api {
            status: 409 | 501,
            ..
        }
    )
}

/// How the daemon ended after its answer.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DaemonEnd {
    /// It stopped, and none started again meanwhile.
    Stopped,
    /// It stopped, and another started on the same data folder: the Marsa
    /// window or the menu-bar icon.
    Back,
    /// It did not stop in time.
    NotStopped(ClientFailure),
}

/// Waits until the daemon has stopped, then watches [`RESTART_WATCH`] for
/// one that starts again.
async fn wait_for_exit(data_dir: &Path) -> DaemonEnd {
    if let Err(err) = client::stop_daemon(data_dir).await {
        return DaemonEnd::NotStopped(failure(&err, data_dir, None));
    }
    let deadline = tokio::time::Instant::now() + RESTART_WATCH;
    while tokio::time::Instant::now() < deadline {
        if client::is_daemon_running(data_dir) {
            return DaemonEnd::Back;
        }
        tokio::time::sleep(RESTART_POLL).await;
    }
    if client::is_daemon_running(data_dir) {
        DaemonEnd::Back
    } else {
        DaemonEnd::Stopped
    }
}

/// Where the questions go and the answers come from.
trait Ask {
    /// Whether a person can answer: stdin is a terminal.
    fn is_terminal(&self) -> bool;
    /// Shows `question` and reads one line. `None` at the end of the input
    /// or on a read error.
    fn ask(&mut self, question: &str) -> Option<String>;
}

/// The user's terminal: questions on stderr, answers from stdin.
struct Terminal;

impl Ask for Terminal {
    fn is_terminal(&self) -> bool {
        io::stdin().is_terminal()
    }

    fn ask(&mut self, question: &str) -> Option<String> {
        let mut stderr = io::stderr().lock();
        // Best effort: the answer is read even if stderr is closed.
        let _ = write!(stderr, "{question}");
        let _ = stderr.flush();
        let mut line = String::new();
        match io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    }
}

fn is_macos(platform: Option<Platform>) -> bool {
    matches!(platform, Some(Platform::DarwinArm64 | Platform::DarwinX64))
}

/// Goes on with `--yes`, or once the user typed `yes`. Refuses without a
/// terminal to ask on: a script must say `--yes` itself.
fn confirm(args: &UninstallAllArgs, terminal: &mut impl Ask) -> Result<(), Failure> {
    if args.yes {
        return Ok(());
    }
    if !terminal.is_terminal() {
        return Err(own_failure("uninstall_all_needs_yes", None));
    }
    match terminal.ask(strings::UNINSTALL_ALL_CONFIRM) {
        Some(answer) if answer.trim().eq_ignore_ascii_case("yes") => Ok(()),
        _ => Err(own_failure("uninstall_all_not_confirmed", None)),
    }
}

/// What happens to the data folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataFolder {
    /// Delete it: `--delete-data`, or the user said yes.
    Delete,
    /// The user said no, or was not asked (`--yes` alone).
    Keep,
    /// A step failed: running the uninstall again needs the folder.
    KeepForRetry,
    /// Every step went well, but a daemon still runs on the folder.
    KeepDaemonRuns,
    /// It came from `--data-dir`: Marsa deletes only its own.
    NotOurs,
}

/// Whether to delete the data folder, or `None` when only the user can say:
/// then the question is asked ([`ask_until_ctrl_c`], [`folder_for`]).
/// `complete`: every step went well; `stopped`: no daemon runs on it;
/// `is_terminal`: someone can answer.
fn data_folder(
    args: &UninstallAllArgs,
    data_dir_given: bool,
    complete: bool,
    stopped: bool,
    is_terminal: bool,
) -> Option<DataFolder> {
    let folder = if data_dir_given {
        DataFolder::NotOurs
    } else if !complete {
        DataFolder::KeepForRetry
    } else if !stopped {
        DataFolder::KeepDaemonRuns
    } else if args.delete_data {
        DataFolder::Delete
    } else if args.yes || !is_terminal {
        DataFolder::Keep
    } else {
        return None;
    };
    Some(folder)
}

/// The data folder for the user's answer: `y` or `yes` deletes it, anything
/// else keeps it.
fn folder_for(answer: &str) -> DataFolder {
    if is_yes(answer) {
        DataFolder::Delete
    } else {
        DataFolder::Keep
    }
}

/// An answer read while Ctrl+C is this process's to handle.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Asked {
    Line(String),
    /// The end of the input, or a read error.
    Closed,
    CtrlC,
}

/// Shows `question` on stderr and reads one line from stdin, or stops at
/// Ctrl+C. std's `read_line` retries after the signal, so the read runs on a
/// thread of its own and races Ctrl+C. A plain thread, not `spawn_blocking`:
/// after Ctrl+C the read stays blocked, and the runtime would wait for a
/// task of its blocking pool when `main` returns; the end of the process
/// ends a plain thread.
async fn ask_until_ctrl_c(question: &str) -> Asked {
    {
        let mut stderr = io::stderr().lock();
        // Best effort: the answer is read even if stderr is closed.
        let _ = write!(stderr, "{question}");
        let _ = stderr.flush();
    }
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let reader = std::thread::Builder::new()
        .name("answer".to_owned())
        .spawn(move || {
            let mut line = String::new();
            let read = io::stdin().lock().read_line(&mut line);
            // Nobody waits any more after Ctrl+C, which is fine.
            let _ = sender.send(matches!(read, Ok(1..)).then_some(line));
        });
    if let Err(err) = reader {
        tracing::debug!(error = %err, "cannot start the thread that reads the answer");
        return Asked::Closed;
    }
    match until_ctrl_c(receiver).await {
        Some(Ok(Some(line))) => Asked::Line(line),
        Some(_) => Asked::Closed,
        None => Asked::CtrlC,
    }
}

/// `y` or `yes`, in any case.
fn is_yes(answer: &str) -> bool {
    let answer = answer.trim();
    answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
}

/// Deletes the data folder once no daemon runs on it: the Marsa window may
/// have started one again while the user answered.
fn delete_when_stopped(data_dir: &Path) -> Result<(), Failure> {
    if client::is_daemon_running(data_dir) {
        return Err(own_failure(
            "uninstall_all_daemon_restarted",
            Some(data_dir),
        ));
    }
    let home = std::env::home_dir();
    delete_data_dir(data_dir, home.as_deref()).map(|_| ())
}

/// Deletes the data folder `dir` and everything in it: `true` if it was
/// there. `dir` comes from `config::app_data_dir`, never from input, and is
/// still checked: a full path, not the root, not the home folder or a folder
/// that holds it, and a real folder, not a link.
///
/// It is first renamed, in one step, to a sibling with a random name on the
/// same volume (`<name>.deleting-<hex>`), then deleted under that name: a
/// daemon that starts meanwhile makes a new folder at `dir` and never writes
/// into the one being deleted. `fs::remove_dir_all` does not follow the
/// links inside: it deletes each link, not what it points to.
///
/// # Errors
///
/// `uninstall_all_data_dir_unsafe` before anything is deleted,
/// `uninstall_all_data_delete_failed` if the rename or the deletion failed,
/// and `uninstall_all_data_dir_back` if `dir` was made again meanwhile.
fn delete_data_dir(dir: &Path, home: Option<&Path>) -> Result<bool, Failure> {
    let unsafe_dir = || own_failure("uninstall_all_data_dir_unsafe", Some(dir));
    let holds_home = home.is_some_and(|home| home.starts_with(dir));
    let name = dir.file_name().filter(|_| dir.parent().is_some());
    let Some(name) = name.filter(|_| dir.is_absolute() && !holds_home) else {
        return Err(unsafe_dir());
    };
    let meta = match fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            tracing::debug!(error = %err, path = %dir.display(), "cannot check the data folder");
            return Err(own_failure("uninstall_all_data_delete_failed", Some(dir)));
        }
    };
    if !meta.file_type().is_dir() {
        return Err(unsafe_dir());
    }
    let mut doomed = name.to_os_string();
    doomed.push(format!(".deleting-{}", random_hex()));
    let doomed = dir.with_file_name(doomed);
    if let Err(err) = fs::rename(dir, &doomed) {
        tracing::debug!(error = %err, path = %dir.display(), "cannot rename the data folder");
        return Err(own_failure("uninstall_all_data_delete_failed", Some(dir)));
    }
    fs::remove_dir_all(&doomed).map_err(|err| {
        tracing::debug!(error = %err, path = %doomed.display(), "cannot delete the data folder");
        own_failure("uninstall_all_data_delete_failed", Some(&doomed))
    })?;
    if fs::symlink_metadata(dir).is_ok() {
        return Err(own_failure("uninstall_all_data_dir_back", Some(dir)));
    }
    Ok(true)
}

/// 16 hex digits nobody can guess: std's `RandomState` is seeded from the
/// system's random source. The process and the time make two calls differ
/// for sure.
fn random_hex() -> String {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
        hasher.write_u128(now.as_nanos());
    }
    format!("{:016x}", hasher.finish())
}

/// A failure of this command, with its text from [`strings`].
fn own_failure(code: &'static str, path: Option<&Path>) -> Failure {
    let details = Details {
        path,
        ..Details::default()
    };
    Failure {
        code,
        message: strings::message(code, &details),
    }
}

/// One line per step: done, already done, or failed and why. A record the
/// daemon could not use is named by its path in the data folder.
fn render(report: &UninstallReport, data_dir: &Path) -> String {
    let record = user_trust::record_path(data_dir);
    let mut text = String::new();
    for result in &report.results {
        let step = strings::uninstall_all_step(&printable(&result.op));
        let line = match result.status {
            OpStatus::Done => strings::setup_done(&step),
            OpStatus::Unchanged => strings::setup_unchanged(&step),
            OpStatus::Failed => {
                let code = printable(result.code.as_deref().unwrap_or_default());
                let details = Details {
                    path: (code == user_trust::TRUST_RECORD_UNUSABLE).then_some(record.as_path()),
                    ..Details::default()
                };
                let reason = strings::message(&code, &details);
                strings::setup_failed(&step, &reason, &code)
            }
        };
        text.push_str(&line);
        text.push('\n');
    }
    text
}

/// Why the helper did not run, ready for stderr.
fn error(report: &UninstallReport) -> Option<ClientFailure> {
    let error = report.error.as_ref()?;
    let code = printable(&error.code);
    Some(ClientFailure {
        message: strings::message(&code, &Details::default()),
        code,
    })
}

/// Every step is done or was done already.
fn is_complete(report: &UninstallReport) -> bool {
    report.error.is_none()
        && report
            .results
            .iter()
            .all(|result| result.status != OpStatus::Failed)
}

/// The folder of this `marsa`, for the programs left to delete.
fn programs_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe()
        .inspect_err(|err| tracing::debug!(error = %err, "cannot find this program"))
        .ok()?;
    exe.parent().map(Path::to_path_buf)
}

/// What Marsa does not do itself.
struct Leftovers<'a> {
    /// The data folder this command worked on.
    data_dir: &'a Path,
    /// Whether to say the data folder may be deleted by hand: not when it
    /// is gone, and not when it is kept for a retry.
    data_left: bool,
    /// The folder of this program.
    programs: Option<&'a Path>,
    /// The folder `directories` gives: the install script printed a `PATH`
    /// line for it (ADR 0013), whatever `--data-dir` says.
    default_dir: Option<&'a Path>,
}

impl Leftovers<'_> {
    fn render(&self) -> String {
        let mut lines = vec![
            strings::UNINSTALL_ALL_LEFT_TO_DO.to_owned(),
            strings::QUIT_WINDOW_LEFT.to_owned(),
            strings::path_left(self.default_dir),
        ];
        if self.data_left {
            lines.push(strings::data_left(self.data_dir));
        }
        if let Some(dir) = self.programs.filter(|dir| !dir.starts_with(self.data_dir)) {
            lines.push(strings::programs_left(dir));
        }
        let mut text = lines.join("\n");
        text.push('\n');
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::types::{ApiError, OpResult};
    use tempfile::TempDir;

    /// Scripted answers, and the questions asked.
    struct Script {
        terminal: bool,
        answers: Vec<&'static str>,
        asked: Vec<String>,
    }

    impl Script {
        fn new(terminal: bool, answers: &[&'static str]) -> Self {
            Self {
                terminal,
                answers: answers.iter().rev().copied().collect(),
                asked: Vec::new(),
            }
        }
    }

    impl Ask for Script {
        fn is_terminal(&self) -> bool {
            self.terminal
        }

        fn ask(&mut self, question: &str) -> Option<String> {
            self.asked.push(question.to_owned());
            self.answers.pop().map(str::to_owned)
        }
    }

    fn args(yes: bool, delete_data: bool) -> UninstallAllArgs {
        UninstallAllArgs {
            all: true,
            yes,
            delete_data,
        }
    }

    fn result(op: &str, status: OpStatus, code: Option<&str>) -> OpResult {
        OpResult {
            op: op.into(),
            status,
            code: code.map(str::to_owned),
            message: code.map(|_| "x".to_owned()),
        }
    }

    /// The names in `dir`.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn anything_but_yes_removes_nothing() {
        for answer in ["no", "y", "", "yess", "ye s"] {
            let mut script = Script::new(true, &[answer]);
            let refused = confirm(&args(false, false), &mut script).unwrap_err();
            assert_eq!(refused.code, "uninstall_all_not_confirmed", "{answer:?}");
            assert!(
                refused.message.contains("nothing was removed"),
                "{answer:?}"
            );
            assert_eq!(script.asked, [strings::UNINSTALL_ALL_CONFIRM]);
        }
        // The end of the input (Ctrl+D) is no answer either.
        let mut script = Script::new(true, &[]);
        let refused = confirm(&args(false, false), &mut script).unwrap_err();
        assert_eq!(refused.code, "uninstall_all_not_confirmed");

        for answer in ["yes\n", "YES\r\n", " yes "] {
            let mut script = Script::new(true, &[answer]);
            assert_eq!(
                confirm(&args(false, false), &mut script),
                Ok(()),
                "{answer:?}"
            );
        }
    }

    #[test]
    fn without_a_terminal_and_without_yes_nothing_is_asked_or_removed() {
        let mut script = Script::new(false, &["yes"]);
        let refused = confirm(&args(false, false), &mut script).unwrap_err();
        assert_eq!(refused.code, "uninstall_all_needs_yes");
        assert!(refused.message.contains("--yes"), "{}", refused.message);
        assert!(script.asked.is_empty());
        // `--yes` goes on without a question, with or without a terminal.
        for terminal in [false, true] {
            let mut script = Script::new(terminal, &[]);
            assert_eq!(confirm(&args(true, false), &mut script), Ok(()));
            assert!(script.asked.is_empty());
        }
    }

    #[test]
    fn the_data_folder_is_deleted_only_when_asked_and_safe() {
        use DataFolder::{Delete, Keep, KeepDaemonRuns, KeepForRetry, NotOurs};
        // The case, the args, `--data-dir`, every step went well, no daemon
        // runs, a terminal, and the decision (`None`: ask).
        type Case = (
            &'static str,
            UninstallAllArgs,
            bool,
            bool,
            bool,
            bool,
            Option<DataFolder>,
        );
        let cases: [Case; 8] = [
            // The question.
            ("ask", args(false, false), false, true, true, true, None),
            // No terminal to ask on, and `--yes` alone: no.
            (
                "no terminal",
                args(false, false),
                false,
                true,
                true,
                false,
                Some(Keep),
            ),
            (
                "--yes",
                args(true, false),
                false,
                true,
                true,
                true,
                Some(Keep),
            ),
            // `--delete-data`: yes.
            (
                "--delete-data",
                args(true, true),
                false,
                true,
                true,
                false,
                Some(Delete),
            ),
            (
                "--delete-data, terminal",
                args(false, true),
                false,
                true,
                true,
                true,
                Some(Delete),
            ),
            // Never after a failed step, with a daemon on it, or for a
            // `--data-dir` folder.
            (
                "a failed step",
                args(true, true),
                false,
                false,
                true,
                true,
                Some(KeepForRetry),
            ),
            (
                "a daemon back",
                args(true, true),
                false,
                true,
                false,
                true,
                Some(KeepDaemonRuns),
            ),
            (
                "--data-dir",
                args(true, true),
                true,
                true,
                true,
                true,
                Some(NotOurs),
            ),
        ];
        for (name, args, given, complete, stopped, terminal, want) in cases {
            let got = data_folder(&args, given, complete, stopped, terminal);
            assert_eq!(got, want, "{name}");
        }
        // The answer, with no as the default.
        for (answer, want) in [
            ("y\n", Delete),
            ("YES\r\n", Delete),
            ("\n", Keep),
            ("n", Keep),
            ("delete", Keep),
        ] {
            assert_eq!(folder_for(answer), want, "{answer:?}");
        }
        assert_eq!(
            strings::UNINSTALL_ALL_DATA_QUESTION,
            "Delete the data folder (runtimes, settings, the CA)? [y/N] "
        );
        let kept = strings::data_kept_ctrl_c(Path::new("/data"));
        assert!(kept.contains("Kept the data folder /data"), "{kept}");
        assert!(strings::UNINSTALL_ALL_STOP_CTRL_C.contains("may still be stopping"));
    }

    #[test]
    fn the_data_folder_is_renamed_then_deleted_and_its_links_are_not_followed() {
        let tmp = TempDir::new().unwrap();
        let data = tmp.path().join("dev.marsa.Marsa");
        fs::create_dir_all(data.join("runtimes").join("php")).unwrap();
        fs::write(data.join("runtimes").join("php").join("x"), "x").unwrap();
        let outside = tmp.path().join("code");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("index.php"), "<?php").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, data.join("link")).unwrap();

        assert_eq!(delete_data_dir(&data, None), Ok(true));
        // Gone, the renamed copy too, and what the link pointed to stays.
        assert_eq!(names(tmp.path()), ["code"]);
        assert!(outside.join("index.php").is_file());
        // Nothing there: fine.
        assert_eq!(delete_data_dir(&data, None), Ok(false));
    }

    #[test]
    fn the_name_of_the_renamed_folder_is_a_random_sibling() {
        let dir = Path::new("/Users/dev/Library/Application Support/dev.marsa.Marsa");
        let (one, two) = (random_hex(), random_hex());
        assert_eq!(one.len(), 16);
        assert!(one.bytes().all(|byte| byte.is_ascii_hexdigit()), "{one}");
        assert_ne!(one, two);
        let mut doomed = dir.file_name().unwrap().to_os_string();
        doomed.push(format!(".deleting-{one}"));
        let doomed = dir.with_file_name(doomed);
        assert_eq!(doomed.parent(), dir.parent());
    }

    #[test]
    fn a_data_folder_that_is_not_a_plain_folder_is_kept() {
        let tmp = TempDir::new().unwrap();
        let unsafe_dir = |dir: &Path, home: Option<&Path>| {
            let refused = delete_data_dir(dir, home).unwrap_err();
            let code = "uninstall_all_data_dir_unsafe";
            assert_eq!(refused.code, code, "{}", dir.display());
        };
        // Not a full path, the root, and the home folder or one above it.
        unsafe_dir(Path::new("relative/data"), None);
        let root = Path::new(if cfg!(windows) { "C:\\" } else { "/" });
        unsafe_dir(root, None);
        let home = tmp.path().join("home");
        fs::create_dir(&home).unwrap();
        unsafe_dir(&home, Some(&home));
        unsafe_dir(tmp.path(), Some(&home));
        assert!(home.is_dir());

        // A file.
        let file = tmp.path().join("file");
        fs::write(&file, "x").unwrap();
        unsafe_dir(&file, None);
        assert!(file.is_file());

        // A link to a folder: neither the link nor the folder goes.
        #[cfg(unix)]
        {
            let target = tmp.path().join("target");
            fs::create_dir(&target).unwrap();
            fs::write(target.join("keep"), "x").unwrap();
            let link = tmp.path().join("dev.marsa.Marsa");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            unsafe_dir(&link, None);
            assert!(target.join("keep").is_file());
            assert!(fs::symlink_metadata(&link).is_ok());
        }
    }

    #[test]
    fn only_409_and_501_mean_nothing_was_removed() {
        let api = |status| ClientError::Api {
            status,
            code: "x".into(),
            message: "x".into(),
        };
        assert!(refused_at_once(&api(409)));
        assert!(refused_at_once(&api(501)));
        for status in [500, 400, 401, 404] {
            assert!(!refused_at_once(&api(status)), "{status}");
        }
        assert!(!refused_at_once(&ClientError::NotRunning));
    }

    #[tokio::test]
    async fn ctrl_c_is_not_waited_for_while_the_answer_comes() {
        // The answer that comes first is returned; Ctrl+C itself cannot be
        // sent in a test without stopping the test runner.
        assert_eq!(until_ctrl_c(async { 7 }).await, Some(7));
        let log = Path::new("/data/logs/marsad.log");
        let text = strings::uninstall_all_ctrl_c(log);
        for part in [
            "goes on in marsad",
            "can still be cancelled",
            "/data/logs/marsad.log",
        ] {
            assert!(text.contains(part), "{part}: {text}");
        }
    }

    #[test]
    fn each_step_has_a_line_and_a_failure_says_why() {
        let report = UninstallReport {
            results: vec![
                result("pf_remove", OpStatus::Done, None),
                result("resolver_remove", OpStatus::Unchanged, None),
                result("trust_remove", OpStatus::Unchanged, None),
                result(
                    "user_trust_remove",
                    OpStatus::Failed,
                    Some("trust_record_unusable"),
                ),
                result("autostart_remove", OpStatus::Done, None),
            ],
            error: None,
            stopping: true,
        };
        let data = Path::new("/data");
        let text = render(&report, data);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 5, "{text}");
        assert_eq!(lines[0], "Remove the redirect of ports 80 and 443: done.");
        assert_eq!(lines[1], "Stop sending .test names to Marsa: already done.");
        assert_eq!(
            lines[2],
            "Remove the trust in Marsa's certificates for all users: already done."
        );
        // The record is named, and the user may delete it.
        let record = user_trust::record_path(data);
        assert!(
            lines[3].starts_with("Remove the trust in Marsa's certificates for you: failed.")
                && lines[3].contains(&record.display().to_string())
                && lines[3].contains("delete it by hand")
                && lines[3].ends_with("[trust_record_unusable]"),
            "{}",
            lines[3]
        );
        assert_eq!(lines[4], "Stop starting Marsa at login: done.");
        assert!(!is_complete(&report));
        assert!(error(&report).is_none());

        let all_done = UninstallReport {
            results: vec![result("pf_remove", OpStatus::Done, None)],
            error: None,
            stopping: true,
        };
        assert!(is_complete(&all_done));
    }

    #[test]
    fn removal_failures_give_no_setup_advice() {
        for code in [
            "resolver_read",
            "trust_read",
            "autostart_write",
            "trust_record_unusable",
        ] {
            let text = strings::message(code, &Details::default());
            assert!(!text.contains("marsa setup"), "{code}: {text}");
            assert!(!text.contains("could not add"), "{code}: {text}");
            assert!(!text.contains("Cannot write"), "{code}: {text}");
        }
    }

    #[test]
    fn a_cancelled_prompt_is_an_error_and_nothing_else() {
        let report = UninstallReport {
            error: Some(ApiError {
                code: "uninstall_cancelled".into(),
                message: "x".into(),
            }),
            ..UninstallReport::default()
        };
        assert_eq!(render(&report, Path::new("/data")), "");
        assert!(!is_complete(&report));
        assert_eq!(
            error(&report).unwrap().line(),
            "marsa: The password prompt was cancelled, so nothing was removed. Run \"marsa \
             uninstall --all\" again when you are ready. [uninstall_cancelled]"
        );
    }

    #[test]
    fn what_is_left_says_to_quit_names_the_default_path_and_the_programs() {
        let default = Path::new("/Users/dev/Library/Application Support/dev.marsa.Marsa");
        let inside = default.join("bin");
        let outside = Path::new("/Users/dev/code/marsa/target/release");
        let left = |data_dir, data_left, programs| Leftovers {
            data_dir,
            data_left,
            programs,
            default_dir: Some(default),
        };

        // All gone.
        let text = left(default, false, Some(inside.as_path())).render();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Left to do by hand:");
        assert!(lines[1].contains("menu-bar icon"), "{text}");
        assert!(
            lines[2].starts_with("  If you added Marsa to PATH"),
            "{text}"
        );
        assert!(lines[2].contains(&inside.display().to_string()), "{text}");
        assert!(lines[2].contains("shims") && lines[2].contains("~/.zprofile"));
        assert_eq!(lines.len(), 3, "{text}");

        // The data folder kept by choice, and programs elsewhere.
        let text = left(default, true, Some(outside)).render();
        assert!(text.contains("Delete the data folder"), "{text}");
        assert!(
            text.contains(&format!(
                "Delete Marsa's programs in {}.",
                outside.display()
            )),
            "{text}"
        );

        // `--data-dir`: the PATH line still names the default folder, which
        // is the one the install told the user to add.
        let custom = Path::new("/tmp/marsa-data");
        let text = left(custom, true, None).render();
        let path_line = text.lines().nth(2).unwrap();
        assert!(path_line.contains(&default.join("shims").display().to_string()));
        assert!(!path_line.contains("/tmp/marsa-data"), "{path_line}");
        assert!(
            text.contains("Delete the data folder /tmp/marsa-data"),
            "{text}"
        );

        // No default folder known: the line names none.
        let none = Leftovers {
            default_dir: None,
            ..left(custom, false, None)
        };
        let text = none.render();
        let path_line = text.lines().nth(2).unwrap();
        assert!(path_line.ends_with("~/.zshrc."), "{path_line}");
        assert!(!path_line.contains("it names"), "{path_line}");
    }

    #[test]
    fn the_plan_names_every_step_and_what_happens_to_the_data_folder() {
        let data = Path::new("/data");
        let given = strings::uninstall_all_plan(data, false, true);
        assert!(
            given.contains("given with --data-dir stays: /data"),
            "{given}"
        );
        let kept = strings::uninstall_all_plan(data, true, false);
        assert!(
            kept.contains("unless you choose to delete it: /data"),
            "{kept}"
        );
        let deleted = strings::uninstall_all_plan(data, true, true);
        assert!(
            deleted
                .contains("The data folder /data will be deleted once every step has succeeded."),
            "{deleted}"
        );
        for part in [
            "ports 80 and 443",
            "/etc/resolver/test",
            "for all users and for you",
            "start at login",
            "administrator password",
            "menu-bar icon",
        ] {
            assert!(kept.contains(part), "{part}: {kept}");
        }
    }

    #[test]
    fn only_macos_runs_the_uninstall() {
        assert!(is_macos(Some(Platform::DarwinArm64)));
        assert!(is_macos(Some(Platform::DarwinX64)));
        for platform in [
            Some(Platform::LinuxX64),
            Some(Platform::LinuxArm64),
            Some(Platform::WindowsX64),
            None,
        ] {
            assert!(!is_macos(platform), "{platform:?}");
        }
    }
}
