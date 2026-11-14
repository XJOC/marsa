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
//! 3. The daemon stops after its answer; this waits until it has.
//! 4. Asks on its own whether to delete the data folder, `no` by default.
//!    `--delete-data` answers yes; `--yes` alone leaves it as no. Only the
//!    folder `directories` gives, never one from `--data-dir`, and only when
//!    every step went well: the next uninstall needs the record of the
//!    trusted certificate that is in it.
//! 5. Prints what is left to do by hand: the `PATH` line in the shell's
//!    startup file, and the programs if they are outside the data folder.
//!
//! Exit code 1 if anything did not happen as asked.

use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use marsa_core::client::{self, ClientError};
use marsa_core::recipes::Platform;
use marsa_core::types::{OpStatus, UninstallReport};

use super::Failure;
use super::daemon::{ClientFailure, connect, failure};
use super::sites::printable;
use super::strings::{self, Details};

/// The arguments of `marsa uninstall`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::Args)]
pub struct UninstallArgs {
    /// Remove everything Marsa set up in this system. Required.
    #[arg(long, required = true)]
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
pub async fn run(args: &UninstallArgs, data_dir: &Path, data_dir_given: bool) -> ExitCode {
    let mut terminal = Terminal;
    // Before the question: the daemon would answer `uninstall_not_supported`
    // only after the user typed yes.
    if !is_macos(Platform::current()) {
        return own_failure("uninstall_not_supported", None).report();
    }
    eprint!("{}", strings::uninstall_plan(data_dir, !data_dir_given));
    if let Err(failure) = confirm(args, &mut terminal) {
        return failure.report();
    }
    let client = match connect(data_dir).await {
        Ok(client) => client,
        Err(failure) => return failure.report(),
    };
    eprintln!("{}", strings::UNINSTALL_STARTED);
    let report = match client.uninstall().await {
        Ok(report) => report,
        Err(err) => {
            // The daemon refused at once (409, 501): nothing was removed. Any
            // other error came while it may have been working.
            if !matches!(err, ClientError::Api { .. }) {
                eprintln!("{}", strings::UNINSTALL_RESULT_UNKNOWN);
            }
            return failure(&err, data_dir, None).report();
        }
    };
    print!("{}", render(&report));
    if let Some(failure) = error(&report) {
        return failure.report();
    }
    let complete = is_complete(&report);
    let mut ok = complete;
    if !complete {
        println!("{}", strings::UNINSTALL_INCOMPLETE);
    }
    // The daemon stops by itself after its answer; this waits for it.
    let stopped = match client::stop_daemon(data_dir).await {
        Ok(_) => {
            println!("{}", strings::DAEMON_STOPPED);
            true
        }
        Err(err) => {
            eprintln!("{}", failure(&err, data_dir, None).line());
            ok = false;
            false
        }
    };
    let plan = data_folder(args, data_dir_given, complete && stopped, &mut terminal);
    let deleted = match plan {
        DataFolder::Delete => match delete_when_stopped(data_dir).await {
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
        DataFolder::NotOurs => {
            if args.delete_data {
                let failure = own_failure("uninstall_data_dir_not_default", Some(data_dir));
                eprintln!("{}", failure.line());
                ok = false;
            }
            false
        }
    };
    print!(
        "{}",
        leftovers(data_dir, deleted, programs_dir().as_deref())
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
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
fn confirm(args: &UninstallArgs, terminal: &mut impl Ask) -> Result<(), Failure> {
    if args.yes {
        return Ok(());
    }
    if !terminal.is_terminal() {
        return Err(own_failure("uninstall_needs_yes", None));
    }
    match terminal.ask(strings::UNINSTALL_CONFIRM) {
        Some(answer) if answer.trim().eq_ignore_ascii_case("yes") => Ok(()),
        _ => Err(own_failure("uninstall_not_confirmed", None)),
    }
}

/// What happens to the data folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataFolder {
    /// Delete it: `--delete-data`, or the user said yes.
    Delete,
    /// The user said no, or was not asked (`--yes` alone).
    Keep,
    /// A step failed, or the daemon did not stop: running the uninstall
    /// again needs the folder.
    KeepForRetry,
    /// It came from `--data-dir`: Marsa deletes only its own.
    NotOurs,
}

/// Whether to delete the data folder. `ready`: every step went well and the
/// daemon has stopped. Asks only when nothing else decides.
fn data_folder(
    args: &UninstallArgs,
    data_dir_given: bool,
    ready: bool,
    terminal: &mut impl Ask,
) -> DataFolder {
    if data_dir_given {
        return DataFolder::NotOurs;
    }
    if !ready {
        return DataFolder::KeepForRetry;
    }
    if args.delete_data {
        return DataFolder::Delete;
    }
    if args.yes || !terminal.is_terminal() {
        return DataFolder::Keep;
    }
    match terminal.ask(strings::DATA_QUESTION) {
        Some(answer) if is_yes(&answer) => DataFolder::Delete,
        _ => DataFolder::Keep,
    }
}

/// `y` or `yes`, in any case.
fn is_yes(answer: &str) -> bool {
    let answer = answer.trim();
    answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
}

/// Deletes the data folder once no daemon runs on it: the Marsa window may
/// have started one again while the user answered.
async fn delete_when_stopped(data_dir: &Path) -> Result<(), Failure> {
    match client::stop_daemon(data_dir).await {
        Ok(false) => {}
        Ok(true) | Err(_) => {
            return Err(own_failure("uninstall_daemon_restarted", Some(data_dir)));
        }
    }
    let home = std::env::home_dir();
    delete_data_dir(data_dir, home.as_deref()).map(|_| ())
}

/// Deletes the data folder `dir` and everything in it: `true` if it was
/// there. `dir` comes from `config::app_data_dir`, never from input, and is
/// still checked: a full path, not the root, not the home folder or a folder
/// that holds it, and a real folder, not a link. `fs::remove_dir_all` does
/// not follow the links inside it either: it deletes each link, not what it
/// points to.
///
/// # Errors
///
/// `uninstall_data_dir_unsafe` before anything is deleted, and
/// `uninstall_data_delete_failed` if the deletion stopped midway.
fn delete_data_dir(dir: &Path, home: Option<&Path>) -> Result<bool, Failure> {
    let unsafe_dir = || own_failure("uninstall_data_dir_unsafe", Some(dir));
    let holds_home = home.is_some_and(|home| home.starts_with(dir));
    if !dir.is_absolute() || dir.parent().is_none() || holds_home {
        return Err(unsafe_dir());
    }
    let meta = match fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            tracing::debug!(error = %err, path = %dir.display(), "cannot check the data folder");
            return Err(own_failure("uninstall_data_delete_failed", Some(dir)));
        }
    };
    if !meta.file_type().is_dir() {
        return Err(unsafe_dir());
    }
    fs::remove_dir_all(dir).map_err(|err| {
        tracing::debug!(error = %err, path = %dir.display(), "cannot delete the data folder");
        own_failure("uninstall_data_delete_failed", Some(dir))
    })?;
    Ok(true)
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

/// One line per step: done, already done, or failed and why.
fn render(report: &UninstallReport) -> String {
    let mut text = String::new();
    for result in &report.results {
        let step = strings::uninstall_step(&printable(&result.op));
        let line = match result.status {
            OpStatus::Done => strings::setup_done(&step),
            OpStatus::Unchanged => strings::setup_unchanged(&step),
            OpStatus::Failed => {
                let code = printable(result.code.as_deref().unwrap_or_default());
                let reason = strings::message(&code, &Details::default());
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

/// What Marsa does not do itself: the `PATH` line (the install only printed
/// it, ADR 0013), the data folder if it stays, and the programs if they are
/// outside it.
fn leftovers(data_dir: &Path, deleted: bool, programs: Option<&Path>) -> String {
    let mut lines = vec![
        strings::LEFT_TO_DO.to_owned(),
        strings::path_left(&data_dir.join("bin"), &data_dir.join("shims")),
    ];
    if !deleted {
        lines.push(strings::data_left(data_dir));
    }
    if let Some(dir) = programs.filter(|dir| !dir.starts_with(data_dir)) {
        lines.push(strings::programs_left(dir));
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
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

    fn args(yes: bool, delete_data: bool) -> UninstallArgs {
        UninstallArgs {
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

    #[test]
    fn anything_but_yes_removes_nothing() {
        for answer in ["no", "y", "", "yess", "ye s"] {
            let mut script = Script::new(true, &[answer]);
            let refused = confirm(&args(false, false), &mut script).unwrap_err();
            assert_eq!(refused.code, "uninstall_not_confirmed", "{answer:?}");
            assert!(
                refused.message.contains("nothing was removed"),
                "{answer:?}"
            );
            assert_eq!(script.asked, [strings::UNINSTALL_CONFIRM]);
        }
        // The end of the input (Ctrl+D) is no answer either.
        let mut script = Script::new(true, &[]);
        let refused = confirm(&args(false, false), &mut script).unwrap_err();
        assert_eq!(refused.code, "uninstall_not_confirmed");

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
        assert_eq!(refused.code, "uninstall_needs_yes");
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
        type Case = (&'static str, UninstallArgs, bool, bool, Script, DataFolder);
        let cases: Vec<Case> = vec![
            // The question, with no as the default.
            (
                "yes",
                args(false, false),
                false,
                true,
                Script::new(true, &["y\n"]),
                DataFolder::Delete,
            ),
            (
                "Enter",
                args(false, false),
                false,
                true,
                Script::new(true, &["\n"]),
                DataFolder::Keep,
            ),
            (
                "no",
                args(false, false),
                false,
                true,
                Script::new(true, &["n"]),
                DataFolder::Keep,
            ),
            // `--yes` alone answers no, and `--delete-data` yes.
            (
                "--yes",
                args(true, false),
                false,
                true,
                Script::new(true, &[]),
                DataFolder::Keep,
            ),
            (
                "--delete-data",
                args(true, true),
                false,
                true,
                Script::new(false, &[]),
                DataFolder::Delete,
            ),
            // Never after a failed step, and never a `--data-dir` folder.
            (
                "a failed step",
                args(true, true),
                false,
                false,
                Script::new(true, &["y"]),
                DataFolder::KeepForRetry,
            ),
            (
                "--data-dir",
                args(true, true),
                true,
                true,
                Script::new(true, &["y"]),
                DataFolder::NotOurs,
            ),
        ];
        for (name, args, given, ready, mut script, want) in cases {
            assert_eq!(
                data_folder(&args, given, ready, &mut script),
                want,
                "{name}"
            );
            let asked = if matches!(name, "yes" | "Enter" | "no") {
                vec![strings::DATA_QUESTION.to_owned()]
            } else {
                Vec::new()
            };
            assert_eq!(script.asked, asked, "{name}");
        }
        assert_eq!(
            strings::DATA_QUESTION,
            "Delete the data folder (runtimes, settings, the CA)? [y/N] "
        );
    }

    #[test]
    fn delete_data_dir_deletes_the_folder_but_not_what_its_links_point_to() {
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
        assert!(!data.exists());
        assert!(outside.join("index.php").is_file());
        // Nothing there: fine.
        assert_eq!(delete_data_dir(&data, None), Ok(false));
    }

    #[test]
    fn a_data_folder_that_is_not_a_plain_folder_is_kept() {
        let tmp = TempDir::new().unwrap();
        let unsafe_dir = |dir: &Path, home: Option<&Path>| {
            let refused = delete_data_dir(dir, home).unwrap_err();
            assert_eq!(
                refused.code,
                "uninstall_data_dir_unsafe",
                "{}",
                dir.display()
            );
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
    fn each_step_has_a_line_and_a_failure_says_why() {
        let report = UninstallReport {
            results: vec![
                result("pf_remove", OpStatus::Done, None),
                result("resolver_remove", OpStatus::Unchanged, None),
                result("trust_remove", OpStatus::Unchanged, None),
                result(
                    "user_trust_remove",
                    OpStatus::Failed,
                    Some("trust_remove_failed"),
                ),
                result("autostart_remove", OpStatus::Done, None),
            ],
            error: None,
            stopping: true,
        };
        let text = render(&report);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 5, "{text}");
        assert_eq!(lines[0], "Remove the redirect of ports 80 and 443: done.");
        assert_eq!(lines[1], "Stop sending .test names to Marsa: already done.");
        assert_eq!(
            lines[2],
            "Remove the trust in Marsa's certificates for all users: already done."
        );
        assert!(
            lines[3].starts_with("Remove the trust in Marsa's certificates for you: failed.")
                && lines[3].ends_with("[trust_remove_failed]"),
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
    fn a_cancelled_prompt_is_an_error_and_nothing_else() {
        let report = UninstallReport {
            error: Some(ApiError {
                code: "uninstall_cancelled".into(),
                message: "x".into(),
            }),
            ..UninstallReport::default()
        };
        assert_eq!(render(&report), "");
        assert!(!is_complete(&report));
        assert_eq!(
            error(&report).unwrap().line(),
            "marsa: The password prompt was cancelled, so nothing was removed. Run \"marsa \
             uninstall --all\" again when you are ready. [uninstall_cancelled]"
        );
    }

    #[test]
    fn what_is_left_names_the_path_line_the_data_and_the_programs() {
        let data = Path::new("/Users/dev/Library/Application Support/dev.marsa.Marsa");
        let inside = data.join("bin");
        let outside = Path::new("/Users/dev/code/marsa/target/release");

        let text = leftovers(data, true, Some(&inside));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Left to do by hand:");
        assert!(lines[1].contains(&inside.display().to_string()), "{text}");
        assert!(lines[1].contains("shims") && lines[1].contains("~/.zprofile"));
        assert_eq!(lines.len(), 2, "{text}");

        let text = leftovers(data, false, Some(outside));
        assert!(text.contains("Delete the data folder"), "{text}");
        assert!(
            text.contains(&format!(
                "Delete Marsa's programs in {}.",
                outside.display()
            )),
            "{text}"
        );
    }

    #[test]
    fn the_plan_names_every_step_and_the_data_folder() {
        let data = Path::new("/data");
        let given = strings::uninstall_plan(data, false);
        assert!(
            given.contains("given with --data-dir stays: /data"),
            "{given}"
        );
        let plan = strings::uninstall_plan(data, true);
        assert!(plan.contains("unless you choose to delete it"), "{plan}");
        for part in [
            "ports 80 and 443",
            "/etc/resolver/test",
            "for all users and for you",
            "start at login",
            "administrator password",
            "/data",
        ] {
            assert!(plan.contains(part), "{part}: {plan}");
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
