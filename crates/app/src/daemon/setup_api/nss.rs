//! Linux: Marsa's CA root in the user's NSS databases, for Chrome and
//! Firefox (ADR 0014 point 4, task M4-18g).
//!
//! Chrome and Firefox on Linux do not read the system store that the
//! helper's `trust_install` fills (ADR 0011): each reads NSS databases in the
//! user's own folder. Those are this user's files, so `marsad` adds the
//! certificate itself, as the user, with `certutil` (`libnss3-tools` on
//! Ubuntu and Debian, `nss-tools` on Fedora). No root, no helper, no prompt.
//!
//! The databases, found from the home folder only:
//!
//! - Chrome: `~/.pki/nssdb`. Made (mode 0700, `certutil -N
//!   --empty-password`) when it is not there, so that a Chrome started later
//!   finds the trust.
//! - Firefox: each profile folder with a `cert9.db` under
//!   `~/.mozilla/firefox/` and under `~/snap/firefox/common/.mozilla/firefox/`
//!   (the snap of Ubuntu), at most [`MAX_PROFILES`]. A profile is never made.
//!
//! Every folder on the way from the home folder, and `cert9.db` itself,
//! must be real (not a link) and belong to this user; otherwise that
//! database is left alone and counts as failed. So a link never takes the
//! certificate to another place.
//!
//! For each database, with separate arguments and no shell:
//!
//! 1. `certutil -d sql:<dir> -L -n "Marsa Local CA" -a` lists the
//!    certificates with Marsa's nickname. This certificate among them:
//!    `unchanged`.
//! 2. Others with the nickname (an older CA of this data folder, or of an
//!    earlier install): `certutil -d sql:<dir> -D -n "Marsa Local CA"`, then
//!    the list again, until none is left.
//! 3. `certutil -d sql:<dir> -A -t C,, -n "Marsa Local CA" -i <file>`: trusted
//!    to issue server certificates. `<file>` is `<data>/trust/nss.pem`, the
//!    checked text setup sent to the helper, written for this step and
//!    deleted after it.
//!
//! One database that fails does not stop the others. The step gives one
//! result, `nss_install`: `done` if any database took the certificate,
//! `unchanged` if all had it, and `nss_add_failed` if any failed, with each
//! failed folder in the message. No `certutil` at all is not a failure: the
//! step is skipped as `nss_tool_missing`. The whole step stays under
//! [`NSS_BUDGET`].
//!
//! There is no removal yet: `POST /v1/uninstall` does not run on Linux.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use marsa_core::helper::OpResult;
use marsa_core::types::OpStatus;
use tokio_util::sync::CancellationToken;

use super::user_trust::{self, PEM_END, TrustFiles};
use super::{PEM_BEGIN, PIPE_GRACE, RunOutput, Runner};

/// The tool, by its absolute path (AGENTS.md section 6).
pub const CERTUTIL: &str = "/usr/bin/certutil";
/// The op name of the step in a report.
pub const NSS_INSTALL: &str = "nss_install";
/// `certutil` is not installed: the step is skipped with this code.
pub const NSS_TOOL_MISSING: &str = "nss_tool_missing";
/// At least one database did not take the certificate.
pub const NSS_ADD_FAILED: &str = "nss_add_failed";
/// The longest the whole step takes, the pipe grace after each `certutil`
/// included. Part of `SLOWEST_SETUP` on Linux.
pub const NSS_BUDGET: Duration = Duration::from_secs(40);
/// The most one `certutil` may take. Each takes milliseconds.
const CALL_LIMIT: Duration = Duration::from_secs(10);
/// The most Firefox profiles that get the certificate.
pub const MAX_PROFILES: usize = 32;
/// The most older certificates with Marsa's nickname deleted in one database.
const MAX_DELETES: usize = 8;
/// The nickname of Marsa's root in each database: `CA_NAME` in
/// `marsa_core::caddy`, as in `user_trust`.
const NICKNAME: &str = "Marsa Local CA";
/// Chrome's database, under the home folder.
const CHROME: [&str; 2] = [".pki", "nssdb"];
/// The folders that hold Firefox profiles, under the home folder.
const FIREFOX: [&[&str]; 2] = [
    &[".mozilla", "firefox"],
    &["snap", "firefox", "common", ".mozilla", "firefox"],
];
/// The file of a database in the SQL format, the only one used here.
const CERT_DB: &str = "cert9.db";
/// The checked certificate for `certutil -i`, in the trust folder.
const PEM_FILE: &str = "nss.pem";

/// What [`install`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The result of `nss_install`.
    Ran(OpResult),
    /// The step did not run: [`NSS_TOOL_MISSING`].
    Skipped(&'static str),
}

/// `nss_install`: the certificate whose checked text is `pem` into every
/// NSS database of the user whose home folder is `home` (see the module
/// notes).
pub fn install(
    runner: &dyn Runner,
    home: &Path,
    pem: &str,
    files: &TrustFiles,
    stop: &CancellationToken,
) -> Outcome {
    let failed =
        |message: String| Outcome::Ran(user_trust::failed(NSS_INSTALL, NSS_ADD_FAILED, message));
    if !home.is_absolute() || !files.dir().is_absolute() {
        return failed("the home folder or the trust folder is not an absolute path".into());
    }
    let Some(ours) = user_trust::body(pem) else {
        return failed("the CA root is not one certificate".into());
    };
    let (databases, mut failures) = databases(home);
    let file = files.dir().join(PEM_FILE);
    let written =
        user_trust::private_dir(files.dir()).and_then(|()| user_trust::write_pending(&file, pem));
    if let Err(problem) = written {
        tracing::warn!(problem = %problem, "the trust folder cannot be used; certutil does not run");
        return failed(problem);
    }
    let mut calls = Calls {
        runner,
        stop,
        deadline: Instant::now() + NSS_BUDGET,
    };
    let mut added = 0;
    for database in &databases {
        let dir = database.dir.display();
        match add(&mut calls, database, &file, &ours) {
            Ok(true) => {
                tracing::info!(%dir, "Marsa's CA root added to an NSS database");
                added += 1;
            }
            Ok(false) => tracing::info!(%dir, "Marsa's CA root is in this NSS database already"),
            Err(Failure::ToolMissing) => {
                user_trust::discard(&file);
                tracing::warn!(
                    "{CERTUTIL} is not installed; Chrome and Firefox do not trust Marsa's CA"
                );
                return Outcome::Skipped(NSS_TOOL_MISSING);
            }
            Err(Failure::Other(why)) => {
                tracing::warn!(%dir, why = %why, "an NSS database did not take Marsa's CA root");
                failures.push(format!("{dir}: {why}"));
            }
        }
    }
    user_trust::discard(&file);
    if !failures.is_empty() {
        let message = format!(
            "{} NSS database(s) did not take Marsa's CA root: {}",
            failures.len(),
            failures.join("; ")
        );
        return failed(message);
    }
    let status = if added > 0 {
        OpStatus::Done
    } else {
        OpStatus::Unchanged
    };
    Outcome::Ran(user_trust::result(NSS_INSTALL, status))
}

/// One NSS database.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Database {
    dir: PathBuf,
    /// Not there yet: Chrome's, made before the certificate goes in.
    new: bool,
}

/// Why one database did not take the certificate.
#[derive(Debug)]
enum Failure {
    /// `certutil` is not there: no database can take it.
    ToolMissing,
    Other(String),
}

/// `true` once `certutil -A` added the certificate to `database`, `false`
/// if it was there already.
fn add(
    calls: &mut Calls<'_>,
    database: &Database,
    file: &Path,
    ours: &str,
) -> Result<bool, Failure> {
    let dir = &database.dir;
    if database.new {
        make_dir(dir).map_err(Failure::Other)?;
        calls.expect_success(&new_args(dir), "-N")?;
    }
    let mut listed = calls.list(dir)?;
    if listed.iter().any(|body| body == ours) {
        return Ok(false);
    }
    let mut deleted = 0;
    while !listed.is_empty() {
        if deleted == MAX_DELETES {
            let why = format!(
                "{MAX_DELETES} older certificates named {NICKNAME} were deleted, and more are left"
            );
            return Err(Failure::Other(why));
        }
        tracing::info!(dir = %dir.display(), "deleting an older certificate named {NICKNAME}");
        calls.expect_success(&delete_args(dir), "-D")?;
        deleted += 1;
        listed = calls.list(dir)?;
    }
    calls.expect_success(&add_args(dir, file), "-A")?;
    Ok(true)
}

/// The `certutil` calls of one step, all within one deadline.
struct Calls<'a> {
    runner: &'a dyn Runner,
    stop: &'a CancellationToken,
    deadline: Instant,
}

impl Calls<'_> {
    /// Runs `certutil` with `args`, at most [`CALL_LIMIT`] and never past
    /// the deadline, its pipe grace included.
    fn run(&self, args: &[OsString]) -> Result<RunOutput, Failure> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        let limit = left.saturating_sub(PIPE_GRACE).min(CALL_LIMIT);
        if limit.is_zero() {
            return Err(Failure::Other("out of time".into()));
        }
        match self.runner.run(CERTUTIL, args, limit, self.stop) {
            Ok(output) => Ok(output),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Err(Failure::ToolMissing),
            Err(err) => Err(Failure::Other(format!(
                "certutil did not run: {}",
                err.kind()
            ))),
        }
    }

    /// Runs `certutil`; an exit code other than 0 is a failure of `command`.
    fn expect_success(&self, args: &[OsString], command: &str) -> Result<(), Failure> {
        let output = self.run(args)?;
        if output.code == Some(0) {
            return Ok(());
        }
        let exit = output
            .code
            .map_or_else(|| "a signal".to_owned(), |code| format!("exit code {code}"));
        Err(Failure::Other(format!(
            "certutil {command} ended with {exit}"
        )))
    }

    /// The certificates named [`NICKNAME`] in `dir`, as base64. A listing
    /// that fails (the usual "Could not find cert") lists none.
    fn list(&self, dir: &Path) -> Result<Vec<String>, Failure> {
        let output = self.run(&list_args(dir))?;
        if output.code != Some(0) {
            return Ok(Vec::new());
        }
        Ok(bodies(&String::from_utf8_lossy(&output.stdout)))
    }
}

/// The databases under `home`, and why each refused one cannot be used.
fn databases(home: &Path) -> (Vec<Database>, Vec<String>) {
    let mut found = Vec::new();
    let mut refused = Vec::new();
    let chrome = under(home, &CHROME);
    match real_folders(home, &CHROME) {
        Folders::Real => match file_kind(&chrome.join(CERT_DB)) {
            Ok(true) => found.push(Database {
                dir: chrome,
                new: false,
            }),
            Ok(false) => found.push(Database {
                dir: chrome,
                new: true,
            }),
            Err(why) => refused.push(why),
        },
        Folders::Missing => found.push(Database {
            dir: chrome,
            new: true,
        }),
        Folders::Refused(why) => refused.push(why),
    }
    let mut profiles = 0;
    for parts in FIREFOX {
        match real_folders(home, parts) {
            Folders::Real => {}
            Folders::Missing => continue,
            Folders::Refused(why) => {
                refused.push(why);
                continue;
            }
        }
        for dir in profile_dirs(&under(home, parts)) {
            if profiles == MAX_PROFILES {
                tracing::warn!(dir = %dir.display(), "more than {MAX_PROFILES} Firefox profiles; this one is left out");
                continue;
            }
            match file_kind(&dir.join(CERT_DB)) {
                Ok(true) => {
                    profiles += 1;
                    found.push(Database { dir, new: false });
                }
                // A folder that is not a profile, such as `Crash Reports`.
                Ok(false) => {}
                Err(why) => refused.push(why),
            }
        }
    }
    (found, refused)
}

/// The folders in `dir` that are real and this user's, sorted.
fn profile_dirs(dir: &Path) -> Vec<PathBuf> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(dir = %dir.display(), error = %err, "cannot list the Firefox profiles");
            return Vec::new();
        }
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            // Neither follows a link.
            entry.file_type().is_ok_and(|kind| kind.is_dir())
                && entry.metadata().is_ok_and(|meta| owned(&meta))
        })
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    dirs
}

/// `home` joined with `parts`.
fn under(home: &Path, parts: &[&str]) -> PathBuf {
    parts
        .iter()
        .fold(home.to_path_buf(), |path, part| path.join(part))
}

/// What [`real_folders`] found.
#[derive(Debug, PartialEq, Eq)]
enum Folders {
    /// Each one is a real folder of this user's.
    Real,
    /// One is not there; each one before it is real.
    Missing,
    /// One is a link, a file, or another user's.
    Refused(String),
}

/// Checks each folder from `home` down through `parts`. `home` itself is
/// not checked: the system says where it is.
fn real_folders(home: &Path, parts: &[&str]) -> Folders {
    let mut path = home.to_path_buf();
    for part in parts {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Folders::Missing,
            Err(err) => {
                return Folders::Refused(format!(
                    "{} cannot be checked: {}",
                    path.display(),
                    err.kind()
                ));
            }
            Ok(meta) if !meta.file_type().is_dir() => {
                return Folders::Refused(format!(
                    "{} is not a folder (a link, or a file)",
                    path.display()
                ));
            }
            Ok(meta) if !owned(&meta) => {
                return Folders::Refused(format!("{} belongs to another user", path.display()));
            }
            Ok(_) => {}
        }
    }
    Folders::Real
}

/// `true` for a real file of this user's, `false` if there is none.
///
/// # Errors
///
/// A link, a folder, another user's file, or one that cannot be checked.
fn file_kind(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!(
            "{} cannot be checked: {}",
            path.display(),
            err.kind()
        )),
        Ok(meta) if !meta.file_type().is_file() => Err(format!(
            "{} is not a file (a link, or a folder)",
            path.display()
        )),
        Ok(meta) if !owned(&meta) => Err(format!("{} belongs to another user", path.display())),
        Ok(_) => Ok(true),
    }
}

/// Whether the file belongs to the user `marsad` runs as.
// `#[cfg]` outside `platform`: Unix owners, as in `user_trust`. The step
// runs on Linux only; elsewhere this compiles for the tests.
#[cfg(unix)]
fn owned(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    super::current_uid().is_ok_and(|me| meta.uid() == me)
}

#[cfg(not(unix))]
fn owned(_meta: &fs::Metadata) -> bool {
    true
}

/// Makes Chrome's folder, and `~/.pki` if needed, for this user only.
fn make_dir(dir: &Path) -> Result<(), String> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    // `#[cfg]` outside `platform`: the Unix mode of the new folders.
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(dir)
        .map_err(|err| format!("cannot make {}: {}", dir.display(), err.kind()))
}

/// `-d sql:<dir>`: the database, in the SQL format.
fn database_args(dir: &Path) -> Vec<OsString> {
    let mut database = OsString::from("sql:");
    database.push(dir.as_os_str());
    vec!["-d".into(), database]
}

fn new_args(dir: &Path) -> Vec<OsString> {
    let mut args = database_args(dir);
    args.extend(["-N".into(), "--empty-password".into()]);
    args
}

fn list_args(dir: &Path) -> Vec<OsString> {
    let mut args = database_args(dir);
    args.extend(["-L".into(), "-n".into(), NICKNAME.into(), "-a".into()]);
    args
}

fn delete_args(dir: &Path) -> Vec<OsString> {
    let mut args = database_args(dir);
    args.extend(["-D".into(), "-n".into(), NICKNAME.into()]);
    args
}

fn add_args(dir: &Path, file: &Path) -> Vec<OsString> {
    let mut args = database_args(dir);
    let rest = ["-A", "-t", "C,,", "-n", NICKNAME, "-i"];
    args.extend(rest.map(OsString::from));
    args.push(file.into());
    args
}

/// The base64 of each certificate in `text`, without line breaks.
fn bodies(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(PEM_BEGIN) {
        let Some(end) = rest[start..].find(PEM_END) else {
            break;
        };
        let end = start + end + PEM_END.len();
        found.extend(user_trust::body(&rest[start..end]));
        rest = &rest[end..];
    }
    found
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::super::tests::{Call, Fake, TEST_CA, exit, ok};
    use super::*;
    use tempfile::TempDir;

    /// What `certutil -L` prints when the nickname is not there.
    const NOT_FOUND: &str = "certutil: Could not find cert: Marsa Local CA\n";

    /// A home folder and a data folder.
    struct Home {
        dir: TempDir,
    }

    impl Home {
        fn new() -> Self {
            let home = Self {
                dir: TempDir::new().unwrap(),
            };
            fs::create_dir_all(home.home()).unwrap();
            home
        }

        fn home(&self) -> PathBuf {
            self.dir.path().join("home")
        }

        fn files(&self) -> TrustFiles {
            TrustFiles::in_dir(&self.dir.path().join("data"))
        }

        /// A Firefox profile with a database, under `parts`.
        fn profile(&self, parts: &[&str], name: &str) -> PathBuf {
            let dir = under(&self.home(), parts).join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(CERT_DB), "").unwrap();
            dir
        }

        fn chrome(&self) -> PathBuf {
            under(&self.home(), &CHROME)
        }

        fn install(&self, fake: &Arc<Fake>) -> Outcome {
            let stop = CancellationToken::new();
            install(fake.as_ref(), &self.home(), TEST_CA, &self.files(), &stop)
        }
    }

    /// `certutil` on databases where Marsa's nickname is not there yet:
    /// `-L` finds nothing, everything else works.
    fn fresh() -> Arc<Fake> {
        Fake::answering(|program, args| {
            assert_eq!(program, CERTUTIL);
            match args[2].as_str() {
                "-L" => exit(255, NOT_FOUND),
                _ => ok(""),
            }
        })
    }

    fn sql(dir: &Path) -> String {
        format!("sql:{}", dir.display())
    }

    /// Each call as `-d <db> <command>`, the rest left out.
    fn commands(calls: &[Call]) -> Vec<(String, String)> {
        calls
            .iter()
            .map(|call| (call.args[1].clone(), call.args[2].clone()))
            .collect()
    }

    fn result(outcome: Outcome) -> OpResult {
        match outcome {
            Outcome::Ran(result) => result,
            Outcome::Skipped(code) => panic!("skipped: {code}"),
        }
    }

    /// The test CA's base64 with one character changed: another
    /// certificate with Marsa's nickname.
    fn other_ca() -> String {
        let at = TEST_CA.find(PEM_BEGIN).unwrap() + PEM_BEGIN.len() + 20;
        let mut other = TEST_CA.to_owned();
        let changed = if &other[at..=at] == "A" { "B" } else { "A" };
        other.replace_range(at..=at, changed);
        other
    }

    #[test]
    fn the_certutil_arguments_are_pinned() {
        let home = Home::new();
        let firefox = home.profile(&[".mozilla", "firefox"], "abc.default-release");
        let fake = fresh();
        let done = result(home.install(&fake));
        assert_eq!(
            (done.op.as_str(), done.status),
            (NSS_INSTALL, OpStatus::Done)
        );

        let chrome = home.chrome();
        let file = home.files().dir().join(PEM_FILE);
        let calls = fake.calls();
        let args: Vec<Vec<String>> = calls.iter().map(|call| call.args.clone()).collect();
        let file = file.to_str().unwrap();
        assert_eq!(
            args,
            [
                vec!["-d", &sql(&chrome), "-N", "--empty-password"],
                vec!["-d", &sql(&chrome), "-L", "-n", "Marsa Local CA", "-a"],
                vec![
                    "-d",
                    &sql(&chrome),
                    "-A",
                    "-t",
                    "C,,",
                    "-n",
                    "Marsa Local CA",
                    "-i",
                    file
                ],
                vec!["-d", &sql(&firefox), "-L", "-n", "Marsa Local CA", "-a"],
                vec![
                    "-d",
                    &sql(&firefox),
                    "-A",
                    "-t",
                    "C,,",
                    "-n",
                    "Marsa Local CA",
                    "-i",
                    file
                ],
            ]
        );
        assert!(calls.iter().all(|call| call.limit == CALL_LIMIT));
        assert!(chrome.is_dir());
        #[cfg(unix)]
        for dir in [chrome.parent().unwrap(), &chrome] {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{}", dir.display());
        }
        // The checked copy is gone after the step.
        assert!(!Path::new(file).exists());
    }

    #[test]
    fn databases_that_have_the_certificate_are_unchanged() {
        let home = Home::new();
        fs::create_dir_all(home.chrome()).unwrap();
        fs::write(home.chrome().join(CERT_DB), "").unwrap();
        home.profile(
            &["snap", "firefox", "common", ".mozilla", "firefox"],
            "x.default",
        );
        // An older certificate with the nickname too: ours is there, so it
        // stays.
        let listing = format!("{}\r\n{TEST_CA}", other_ca());
        let fake = Fake::answering(move |_, args| match args[2].as_str() {
            "-L" => ok(&listing),
            other => panic!("certutil {other}"),
        });
        let unchanged = result(home.install(&fake));
        assert_eq!(unchanged.status, OpStatus::Unchanged);
        assert_eq!(fake.calls().len(), 2);
    }

    #[test]
    fn an_older_certificate_with_the_nickname_is_deleted_first() {
        let home = Home::new();
        fs::create_dir_all(home.chrome()).unwrap();
        fs::write(home.chrome().join(CERT_DB), "").unwrap();
        let older = other_ca();
        let lists = AtomicUsize::new(0);
        let fake = Fake::answering(move |_, args| match args[2].as_str() {
            // Two older ones: one goes with each `-D`.
            "-L" => match lists.fetch_add(1, Ordering::SeqCst) {
                0 => ok(&format!("{older}{older}")),
                1 => ok(&older),
                _ => exit(255, NOT_FOUND),
            },
            "-D" | "-A" => ok(""),
            other => panic!("certutil {other}"),
        });
        let done = result(home.install(&fake));
        assert_eq!(done.status, OpStatus::Done);
        let db = sql(&home.chrome());
        let steps: Vec<_> = ["-L", "-D", "-L", "-D", "-L", "-A"]
            .map(|command| (db.clone(), command.to_owned()))
            .into();
        assert_eq!(commands(&fake.calls()), steps);
    }

    #[test]
    fn deletes_that_never_end_are_a_failure() {
        let home = Home::new();
        fs::create_dir_all(home.chrome()).unwrap();
        fs::write(home.chrome().join(CERT_DB), "").unwrap();
        let older = other_ca();
        let fake = Fake::answering(move |_, args| match args[2].as_str() {
            "-L" => ok(&older),
            "-D" => ok(""),
            other => panic!("certutil {other}"),
        });
        let failed = result(home.install(&fake));
        assert_eq!(failed.code.as_deref(), Some(NSS_ADD_FAILED));
        let deletes = fake
            .calls()
            .iter()
            .filter(|call| call.args[2] == "-D")
            .count();
        assert_eq!(deletes, MAX_DELETES);
    }

    #[test]
    fn no_certutil_skips_the_step() {
        let home = Home::new();
        let fake = Fake::answering(|_, _| Err(io::Error::from(io::ErrorKind::NotFound)));
        assert_eq!(home.install(&fake), Outcome::Skipped(NSS_TOOL_MISSING));
        assert_eq!(fake.calls().len(), 1);
        assert!(!home.files().dir().join(PEM_FILE).exists());
    }

    #[test]
    fn one_database_that_fails_does_not_stop_the_others() {
        let home = Home::new();
        let first = home.profile(&[".mozilla", "firefox"], "a.default");
        let second = home.profile(&[".mozilla", "firefox"], "b.default");
        let broken = sql(&first);
        let fake = Fake::answering(move |_, args| match args[2].as_str() {
            "-L" => exit(255, NOT_FOUND),
            "-A" if args[1] == broken => exit(255, "SEC_ERROR_READ_ONLY"),
            _ => ok(""),
        });
        let failed = result(home.install(&fake));
        assert_eq!(failed.status, OpStatus::Failed);
        assert_eq!(failed.code.as_deref(), Some(NSS_ADD_FAILED));
        let message = failed.message.unwrap();
        assert!(message.starts_with("1 NSS database(s)"), "{message}");
        assert!(message.contains(&format!(
            "{}: certutil -A ended with exit code 255",
            first.display()
        )));
        // The second profile still got it.
        let added: Vec<_> = commands(&fake.calls())
            .into_iter()
            .filter(|(_, command)| command == "-A")
            .map(|(db, _)| db)
            .collect();
        assert_eq!(added, [sql(&home.chrome()), sql(&first), sql(&second)]);
    }

    #[test]
    fn folders_without_a_database_are_not_profiles_and_the_count_is_capped() {
        let home = Home::new();
        let base = [".mozilla", "firefox"];
        fs::create_dir_all(under(&home.home(), &base).join("Crash Reports")).unwrap();
        for n in 0..=MAX_PROFILES {
            home.profile(&base, &format!("p{n:02}"));
        }
        let (found, refused) = databases(&home.home());
        assert!(refused.is_empty(), "{refused:?}");
        // Chrome's, then the first profiles by name.
        assert_eq!(found.len(), 1 + MAX_PROFILES);
        assert!(found[1].dir.ends_with("p00"));
        assert!(
            found
                .last()
                .unwrap()
                .dir
                .ends_with(format!("p{:02}", MAX_PROFILES - 1))
        );
    }

    #[test]
    fn a_step_out_of_time_runs_nothing_more() {
        let fake = fresh();
        let stop = CancellationToken::new();
        let calls = Calls {
            runner: fake.as_ref(),
            stop: &stop,
            deadline: Instant::now() + PIPE_GRACE,
        };
        let Err(Failure::Other(why)) = calls.run(&[]) else {
            panic!("ran");
        };
        assert_eq!(why, "out of time");
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn a_relative_home_runs_nothing() {
        let fake = fresh();
        let stop = CancellationToken::new();
        let files = TrustFiles::in_dir(Path::new("/data"));
        let outcome = install(fake.as_ref(), Path::new("home"), TEST_CA, &files, &stop);
        assert_eq!(result(outcome).code.as_deref(), Some(NSS_ADD_FAILED));
        assert!(fake.calls().is_empty());
    }

    /// A link anywhere on the way, or in place of `cert9.db`, keeps that
    /// database out: the certificate never goes where a link points.
    #[cfg(unix)]
    #[test]
    fn links_are_not_followed() {
        use std::os::unix::fs::symlink;

        let home = Home::new();
        let elsewhere = home.dir.path().join("elsewhere");
        let profile = elsewhere.join("p.default");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join(CERT_DB), "").unwrap();
        // `~/.pki` is a link; `~/.mozilla` is a link; in the snap folder,
        // one profile is a link and one has a linked `cert9.db`.
        symlink(&elsewhere, home.home().join(".pki")).unwrap();
        symlink(&elsewhere, home.home().join(".mozilla")).unwrap();
        let snap = ["snap", "firefox", "common", ".mozilla", "firefox"];
        let real = home.profile(&snap, "real.default");
        symlink(&profile, under(&home.home(), &snap).join("linked.default")).unwrap();
        let linked_db = under(&home.home(), &snap).join("db.default");
        fs::create_dir_all(&linked_db).unwrap();
        symlink(profile.join(CERT_DB), linked_db.join(CERT_DB)).unwrap();

        let fake = fresh();
        let failed = result(home.install(&fake));
        assert_eq!(failed.code.as_deref(), Some(NSS_ADD_FAILED));
        let message = failed.message.unwrap();
        assert!(message.starts_with("3 NSS database(s)"), "{message}");
        assert!(
            message.contains(".pki is not a folder (a link, or a file)"),
            "{message}"
        );
        assert!(
            message.contains(".mozilla is not a folder (a link, or a file)"),
            "{message}"
        );
        assert!(
            message.contains("cert9.db is not a file (a link, or a folder)"),
            "{message}"
        );
        // Only the real profile was touched.
        let touched: Vec<_> = commands(&fake.calls())
            .into_iter()
            .map(|(db, _)| db)
            .collect();
        assert_eq!(touched, [sql(&real), sql(&real)]);
        assert!(!elsewhere.join("nssdb").exists());
    }

    #[test]
    fn bodies_reads_every_certificate() {
        let text = format!("{TEST_CA}\r\n{}", other_ca());
        let found = bodies(&text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0], user_trust::body(TEST_CA).unwrap());
        assert_ne!(found[0], found[1]);
        assert!(bodies("no certificate").is_empty());
        assert!(bodies(&TEST_CA[..TEST_CA.len() / 2]).is_empty());
    }
}
