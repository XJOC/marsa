//! Linux: Marsa's CA root in the user's NSS databases, for Chrome and
//! Firefox (ADR 0014 point 4).
//!
//! Chrome and Firefox on Linux do not read the system store that the
//! helper's `trust_install` fills (ADR 0011): each reads NSS databases in the
//! user's own folder. Those are this user's files, so `marsad` adds the
//! certificate itself, as the user, with `certutil` (`libnss3-tools` on
//! Ubuntu and Debian, `nss-tools` on Fedora). No root, no helper, no prompt.
//!
//! The databases, found from the home folder only:
//!
//! - Chrome: `~/.pki/nssdb`. Made (mode 0700) when it is not there, so that
//!   a Chrome started later finds the trust; `certutil -A` makes the database
//!   files in it, with no password.
//! - Chromium, the snap of Ubuntu: `~/snap/chromium/current/.pki/nssdb`,
//!   made the same way when `~/snap/chromium` is there. `current` is the
//!   link snap keeps to the folder of the installed revision: it is followed
//!   only when it names a folder right next to it.
//! - Firefox: each profile folder with a `cert9.db` under
//!   `~/.mozilla/firefox/`, `~/.config/mozilla/firefox/` (Firefox 147 and
//!   later), and `~/snap/firefox/common/.mozilla/firefox/` (the snap of
//!   Ubuntu), at most [`MAX_PROFILES`]. A profile is never made.
//!
//! Every other folder on the way from the home folder, and the database
//! files (`cert9.db`, `key4.db`, `pkcs11.txt`), must be real (not a link)
//! and belong to this user; otherwise that database is left alone and
//! reported as `nss_db_refused`. So a link never takes the certificate to
//! another place. Flatpak browsers keep their own databases and are not
//! covered (ADR 0014).
//!
//! **Each data folder touches only its own certificates** (`record`): a
//! nickname carries the certificate's fingerprint, and only the
//! fingerprints this data folder recorded are ever deleted: by setup, an
//! older root of its own; by the uninstall ([`remove`]), all of them.
//!
//! For each database, with separate arguments and no shell, after `certutil
//! -d sql:<dir> -L` gives the nickname and trust of each row:
//!
//! 1. Each recorded fingerprint other than this certificate's whose row is
//!    there: `-D -n <its nickname>`. The older fingerprints leave the
//!    record only when every database was read and none failed; a database
//!    left alone, left out, or that `certutil` could not list may still
//!    hold them. Without `certutil` the record is put back as it was.
//! 2. This certificate's row with `C` in its SSL trust, and the certificate
//!    among `-L -n <nickname> -a` (every certificate with Marsa's subject):
//!    `unchanged`. Its row without `C`: `-M -n <nickname> -t C,,`.
//! 3. No row of its own, but the certificate is there under the plain
//!    nickname `Marsa Local CA` of the first NSS step (`certutil -A` keeps
//!    the nickname of a certificate that is there already): `--rename` it to its
//!    own nickname, then `-M -t C,,`. Only when that row and this
//!    certificate are the only ones the other rows do not account for.
//!    The certificate there under a row that is not its own and not provably
//!    the plain one: `nss_add_failed`, since `-A` would keep that nickname.
//! 4. Otherwise `-A -t C,, -n <nickname> -i <file>`. `<file>` is
//!    `<data>/trust/nss.pem`, the checked text setup sent to the helper,
//!    written for this step and deleted after it. An `-A` that fails is
//!    followed by `-D -n <nickname>`: a database with a password takes the
//!    certificate but not its trust.
//!
//! `-f /dev/null` on every change: an empty password. A database with a
//! password then fails at once; without it `certutil` would ask on the
//! terminal `marsad` was started from (NSS opens `/dev/tty`).
//!
//! One database that fails does not stop the others. The step gives one
//! result, `nss_install`: `done` if any database changed, `unchanged` if
//! none had to, `nss_add_failed` if `certutil` failed on any, and
//! `nss_db_refused` if one was only left alone; each folder is in the
//! message. No `certutil` at all is not a failure: the step is skipped as
//! `nss_tool_missing`. The whole step stays under [`NSS_BUDGET`].
//!
//! The files: `databases` finds the databases and checks every folder and
//! file on the way; `certutil` runs `certutil` and reads its output;
//! `record` keeps the fingerprints; this one decides what to run for each
//! database.

mod certutil;
mod databases;
mod record;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use marsa_core::helper::OpResult;
use marsa_core::types::OpStatus;
use tokio_util::sync::CancellationToken;

use super::Runner;
use super::user_trust::{self, TrustFiles};
pub use certutil::CERTUTIL;
use certutil::{CA_NAME, Calls, Row, add_args, delete_args, rename_args, trust_args};
pub use databases::MAX_PROFILES;
use databases::{Database, databases, make_dir};
// Tests elsewhere write and read the record where the code does.
#[cfg(test)]
pub(crate) use record::record_path;
use record::{Fingerprint, fingerprint_digits, read_record, write_record};

/// The op name of the step in a report.
pub const NSS_INSTALL: &str = "nss_install";
/// `certutil` is not installed: the step is skipped with this code.
pub const NSS_TOOL_MISSING: &str = "nss_tool_missing";
/// At least one database did not take the certificate.
pub const NSS_ADD_FAILED: &str = "nss_add_failed";
/// A database was left alone: a link, or another user's (often after a
/// browser or `certutil` ran with `sudo`). No `certutil` failed.
pub const NSS_DB_REFUSED: &str = "nss_db_refused";
/// The longest the whole step takes, the pipe grace after each `certutil`
/// included. Part of `SLOWEST_SETUP` on Linux.
pub const NSS_BUDGET: Duration = Duration::from_secs(40);
/// The op name of the removal in an uninstall report.
pub const NSS_REMOVE: &str = "nss_remove";
/// A recorded root is still in a database, or `certutil` is not there to
/// take it out. The record keeps it, so the next uninstall tries again.
pub const NSS_REMOVE_FAILED: &str = "nss_remove_failed";
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
    // From the start: the folder scan and the file count too.
    let deadline = Instant::now() + NSS_BUDGET;
    let failed =
        |message: String| Outcome::Ran(user_trust::failed(NSS_INSTALL, NSS_ADD_FAILED, message));
    if !home.is_absolute() || !files.dir().is_absolute() {
        return failed("the home folder or the trust folder is not an absolute path".into());
    }
    let Some(ours) = user_trust::body(pem) else {
        return failed("the CA root is not one certificate".into());
    };
    let Some(fingerprint) = Fingerprint::of(pem) else {
        return failed("the CA root is not one certificate".into());
    };
    let found = databases(home);
    let file = files.dir().join(PEM_FILE);
    // The record first, with this certificate in it: a step stopped half
    // way still leaves what the uninstall needs to find.
    let ready = user_trust::private_dir(files.dir())
        .and_then(|()| read_record(files))
        .and_then(|before| {
            let mut recorded = before.clone();
            recorded.insert(fingerprint.clone());
            write_record(files, &recorded)?;
            user_trust::write_pending(&file, pem)?;
            Ok(before)
        });
    let before = match ready {
        Ok(before) => before,
        Err(problem) => {
            tracing::warn!(problem = %problem, "the trust folder cannot be used; certutil does not run");
            return failed(problem);
        }
    };
    let mut calls = Calls {
        runner,
        stop,
        deadline,
    };
    let ours = Ours {
        body: ours,
        fingerprint,
        file: file.clone(),
    };
    let old: Vec<Fingerprint> = before
        .iter()
        .filter(|fp| **fp != ours.fingerprint)
        .cloned()
        .collect();
    let mut failures = Vec::new();
    let mut changed = 0;
    for database in &found.databases {
        let dir = database.dir.display();
        match add(&mut calls, database, &ours, &old) {
            Ok(true) => {
                tracing::info!(%dir, "Marsa's CA root added to an NSS database");
                changed += 1;
            }
            Ok(false) => tracing::info!(%dir, "Marsa's CA root is in this NSS database already"),
            Err(Failure::ToolMissing) => {
                user_trust::discard(&file);
                // Nothing was added anywhere: the record goes back to what it
                // was, so the uninstall does not look for this root.
                let nothing_added = changed == 0 && failures.is_empty();
                if nothing_added && let Err(problem) = write_record(files, &before) {
                    tracing::warn!(problem = %problem, "cannot put the record of NSS fingerprints back");
                }
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
    // An older fingerprint leaves the record only when every database was
    // read and none still holds it: a database left alone, left out, or
    // one `certutil` failed on may still trust it.
    if failures.is_empty() && found.refused.is_empty() && found.complete {
        let left = BTreeSet::from([ours.fingerprint.clone()]);
        if let Err(problem) = write_record(files, &left) {
            tracing::warn!(problem = %problem, "cannot update the record of NSS fingerprints");
        }
    }
    let refused = found.refused;
    if !failures.is_empty() || !refused.is_empty() {
        // A database `certutil` failed on is the more useful code: the
        // refused ones are in the message too.
        let code = if failures.is_empty() {
            NSS_DB_REFUSED
        } else {
            NSS_ADD_FAILED
        };
        let all: Vec<String> = failures.into_iter().chain(refused).collect();
        let message = format!(
            "{} NSS database(s) did not take Marsa's CA root: {}",
            all.len(),
            all.join("; ")
        );
        return Outcome::Ran(user_trust::failed(NSS_INSTALL, code, message));
    }
    let status = if changed > 0 {
        OpStatus::Done
    } else {
        OpStatus::Unchanged
    };
    Outcome::Ran(user_trust::result(NSS_INSTALL, status))
}

/// `nss_remove`, for the uninstall: deletes each root in this data folder's
/// record from every database that has its row. The record goes only when
/// every database was read and every deletion worked; otherwise it stays
/// whole, for the next uninstall: a database left alone (a link, another
/// user's) or left out (past [`MAX_PROFILES`]) may still hold a root.
/// `unchanged` with no record, or when no database had them; nothing is
/// made. Without `certutil` while roots are recorded: `nss_remove_failed`,
/// since they may still be trusted.
pub fn remove(
    runner: &dyn Runner,
    home: &Path,
    files: &TrustFiles,
    stop: &CancellationToken,
) -> OpResult {
    let deadline = Instant::now() + NSS_BUDGET;
    let failed = |message: String| user_trust::failed(NSS_REMOVE, NSS_REMOVE_FAILED, message);
    if !home.is_absolute() || !files.dir().is_absolute() {
        return failed("the home folder or the trust folder is not an absolute path".into());
    }
    let recorded = match read_record(files) {
        Ok(recorded) => recorded,
        Err(problem) => return failed(problem),
    };
    if recorded.is_empty() {
        return user_trust::result(NSS_REMOVE, OpStatus::Unchanged);
    }
    let found = databases(home);
    for why in &found.refused {
        tracing::warn!(why = %why, "an NSS database is left alone; the record stays");
    }
    let calls = Calls {
        runner,
        stop,
        deadline,
    };
    let mut failures = Vec::new();
    let mut deleted = 0;
    for database in found.databases.iter().filter(|database| !database.new) {
        let dir = &database.dir;
        let rows = match calls.rows(dir) {
            Ok(rows) => rows,
            Err(failure) => {
                tracing::warn!(dir = %dir.display(), why = %failure.text(), "cannot list an NSS database");
                failures.push(format!("{}: {}", dir.display(), failure.text()));
                // Without `certutil`, no other database can be read either.
                if matches!(failure, Failure::ToolMissing) {
                    break;
                }
                continue;
            }
        };
        for fingerprint in &recorded {
            let nickname = fingerprint.nickname();
            if !rows.iter().any(|row| row.nickname == nickname) {
                continue;
            }
            match calls.expect_success(&delete_args(dir, &nickname), "-D") {
                Ok(()) => {
                    tracing::info!(dir = %dir.display(), %nickname, "Marsa's CA root deleted from an NSS database");
                    deleted += 1;
                }
                Err(failure) => {
                    tracing::warn!(dir = %dir.display(), %nickname, why = %failure.text(), "cannot delete Marsa's CA root from an NSS database");
                    failures.push(format!("{}: {}", dir.display(), failure.text()));
                }
            }
        }
    }
    let all_read = failures.is_empty() && found.refused.is_empty() && found.complete;
    if all_read && let Err(problem) = write_record(files, &BTreeSet::new()) {
        tracing::warn!(problem = %problem, "cannot delete the record of NSS fingerprints");
    }
    if !all_read && failures.is_empty() {
        tracing::warn!(
            left_alone = found.refused.len(),
            complete = found.complete,
            "some NSS databases were not read, so the record of Marsa's roots stays; check them \
             by hand"
        );
    }
    if !failures.is_empty() {
        let message = format!(
            "Marsa's CA root is still in {} NSS database(s): {}",
            failures.len(),
            failures.join("; ")
        );
        return failed(message);
    }
    let status = if deleted > 0 {
        OpStatus::Done
    } else {
        OpStatus::Unchanged
    };
    user_trust::result(NSS_REMOVE, status)
}

/// This data folder's certificate.
struct Ours {
    /// Its base64, as `certutil::bodies` gives it.
    body: String,
    fingerprint: Fingerprint,
    /// The checked copy `certutil -i` reads.
    file: PathBuf,
}

/// Why one database did not take the certificate.
#[derive(Debug)]
enum Failure {
    /// `certutil` is not there: no database can take it.
    ToolMissing,
    Other(String),
}

impl Failure {
    /// For a message.
    fn text(&self) -> String {
        match self {
            Failure::ToolMissing => format!("{CERTUTIL} is not installed"),
            Failure::Other(why) => why.clone(),
        }
    }
}

/// `true` once `database` changed, `false` if it had the certificate,
/// trusted, under its own nickname already.
fn add(
    calls: &mut Calls<'_>,
    database: &Database,
    ours: &Ours,
    old: &[Fingerprint],
) -> Result<bool, Failure> {
    let dir = &database.dir;
    // The rows first, before any folder is made: it is also how a missing
    // `certutil` is found. A folder with no database lists none.
    let rows = calls.rows(dir)?;
    let mut changed = false;
    let mut problems = Vec::new();
    for fingerprint in old {
        let nickname = fingerprint.nickname();
        if !rows.iter().any(|row| row.nickname == nickname) {
            continue;
        }
        tracing::info!(dir = %dir.display(), %nickname, "deleting an older CA of this data folder");
        match calls.expect_success(&delete_args(dir, &nickname), "-D") {
            Ok(()) => changed = true,
            Err(Failure::Other(why)) => problems.push(why),
            Err(missing) => return Err(missing),
        }
    }
    let nickname = ours.fingerprint.nickname();
    let own = rows.iter().find(|row| row.nickname == nickname);
    let held = match own {
        Some(_) => calls.holds(dir, &nickname, &ours.body)?,
        None => false,
    };
    let added = if let Some(row) = own.filter(|_| held) {
        if row.trusted() {
            false
        } else {
            calls.expect_success(&trust_args(dir, &nickname), "-M")?;
            true
        }
    } else if own.is_none() && legacy_is_ours(calls, dir, &rows, ours)? {
        tracing::info!(dir = %dir.display(), %nickname, "giving Marsa's CA root its own nickname");
        calls.expect_success(&rename_args(dir, CA_NAME, &nickname), "--rename")?;
        calls.expect_success(&trust_args(dir, &nickname), "-M")?;
        true
    } else {
        if own.is_some() {
            // A row with this certificate's nickname but another
            // certificate: the nickname is this certificate's alone.
            calls.expect_success(&delete_args(dir, &nickname), "-D")?;
        } else {
            // `-A` keeps the nickname of a certificate that is there
            // already, so this one would never get its own row, and no
            // uninstall could find it. Each row left is asked: one with
            // another subject lists other certificates.
            let deleted: Vec<String> = old.iter().map(Fingerprint::nickname).collect();
            for row in rows.iter().filter(|row| !deleted.contains(&row.nickname)) {
                if calls.holds(dir, &row.nickname, &ours.body)? {
                    return Err(Failure::Other(
                        "Marsa's CA root is in this database under another nickname; find the \
                         row that holds it with certutil -L, delete that row with certutil -D, \
                         then run setup again"
                            .into(),
                    ));
                }
            }
        }
        if database.new {
            make_dir(dir).map_err(Failure::Other)?;
        }
        // `-A` makes the database files in an empty folder, with no
        // password.
        if let Err(failure) = calls.expect_success(&add_args(dir, &nickname, &ours.file), "-A") {
            // A database with a password takes the certificate but not its
            // trust: take it out again, so a later setup does not find it
            // there already. Best effort; the failure stands.
            if matches!(failure, Failure::Other(_)) {
                let _ = calls.run(&delete_args(dir, &nickname));
            }
            return Err(failure);
        }
        true
    };
    if problems.is_empty() {
        Ok(changed || added)
    } else {
        Err(Failure::Other(format!(
            "an older CA of this data folder could not be deleted: {}",
            problems.join("; ")
        )))
    }
}

/// Whether the row with the plain nickname of the first NSS step holds this
/// certificate. `-L -n` lists every certificate with Marsa's subject, so
/// the rows named after a fingerprint account for theirs; the plain row
/// holds ours only if it is the one row left and ours the one certificate
/// left.
fn legacy_is_ours(
    calls: &Calls<'_>,
    dir: &Path,
    rows: &[Row],
    ours: &Ours,
) -> Result<bool, Failure> {
    let unnamed: Vec<&Row> = rows
        .iter()
        .filter(|row| fingerprint_digits(&row.nickname).is_none())
        .collect();
    if !matches!(unnamed.as_slice(), [row] if row.nickname == CA_NAME) {
        return Ok(false);
    }
    let named: Vec<&str> = rows
        .iter()
        .filter_map(|row| fingerprint_digits(&row.nickname))
        .collect();
    let left: Vec<String> = calls
        .list(dir, CA_NAME)?
        .into_iter()
        .filter(|body| {
            Fingerprint::of_body(body)
                .is_none_or(|fp| !named.iter().any(|digits| fp.hex().starts_with(digits)))
        })
        .collect();
    Ok(left == [ours.body.clone()])
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::sync::Arc;

    use super::super::{PEM_BEGIN, PIPE_GRACE};
    use super::certutil::tests::listing;
    use super::certutil::{CALL_LIMIT, bodies};
    use super::databases::{CERT_DB, CHROME, under};

    use super::super::RunOutput;
    use super::super::test_support::{Call, Fake, TEST_CA, TEST_CA_2, exit, ok};
    use super::*;
    use tempfile::TempDir;

    /// What `certutil -L` prints for a folder with no database yet.
    const NO_DATABASE: &str = "certutil: function failed: SEC_ERROR_BAD_DATABASE\n";
    /// What `certutil -L -n` prints when no certificate has the nickname.
    const NOT_FOUND: &str = "certutil: Could not find cert: Marsa Local CA\n";

    /// `certutil -L` as on a real system with no Marsa root: an empty table
    /// where there is a database, `SEC_ERROR_BAD_DATABASE` where there is
    /// none; `-L -n` finds nothing.
    fn no_marsa_rows(args: &[String]) -> io::Result<RunOutput> {
        let dir = Path::new(args[1].trim_start_matches("sql:"));
        match (args.len(), dir.join(CERT_DB).is_file()) {
            (3, true) => ok(&listing(&[])),
            (3, false) => exit(255, NO_DATABASE),
            _ => exit(255, NOT_FOUND),
        }
    }

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
                "-L" => no_marsa_rows(args),
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

    /// The nicknames of the two test roots: `openssl x509 -outform der |
    /// shasum -a 256`, first 16 digits.
    const OURS: &str = "Marsa Local CA 29e433200254dfce";
    const SECOND: &str = "Marsa Local CA b103833491ca1d3c";
    const SECOND_SHA256: &str = "b103833491ca1d3cd98f0d66492077a1d4a2ce5ca88b5f826f446c73ddb29ab3";
    /// A root of another data folder of this user.
    const ELSEWHERE: &str = "Marsa Local CA 0123456789abcdef";

    /// One Chrome database that is there, `certutil -L` giving `rows`,
    /// `-L -n <any> -a` giving `pems`, and every change working.
    fn chrome_with(home: &Home, rows: &[(&str, &str)], pems: &[&str]) -> Arc<Fake> {
        fs::create_dir_all(home.chrome()).unwrap();
        fs::write(home.chrome().join(CERT_DB), "").unwrap();
        let rows = listing(rows);
        let pems: String = pems.iter().map(|pem| pem.replace('\n', "\r\n")).collect();
        Fake::answering(move |_, args| match (args[2].as_str(), args.len()) {
            ("-L", 3) => ok(&rows),
            ("-L", _) => ok(&pems),
            _ => ok(""),
        })
    }

    /// The arguments of each call after `-d sql:<db>`.
    fn tails(fake: &Fake) -> Vec<Vec<String>> {
        fake.calls()
            .iter()
            .map(|call| call.args[2..].to_vec())
            .collect()
    }

    fn write_record_of(home: &Home, lines: &str) {
        fs::create_dir_all(home.files().dir()).unwrap();
        fs::write(record_path(&home.files()), lines).unwrap();
    }

    fn record(home: &Home) -> String {
        fs::read_to_string(record_path(&home.files())).unwrap_or_default()
    }

    #[test]
    fn the_certutil_arguments_are_pinned() {
        let home = Home::new();
        let firefox = home.profile(&[".config", "mozilla", "firefox"], "abc.default-release");
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
        let add = |db: &Path| {
            let db = sql(db);
            [
                "-d",
                &db,
                "-A",
                "-t",
                "C,,",
                "-n",
                OURS,
                "-f",
                "/dev/null",
                "-i",
                file,
            ]
            .map(str::to_owned)
            .to_vec()
        };
        let list = |db: &Path| vec!["-d".to_owned(), sql(db), "-L".to_owned()];
        assert_eq!(
            args,
            [list(&chrome), add(&chrome), list(&firefox), add(&firefox)]
        );
        assert!(calls.iter().all(|call| call.limit == CALL_LIMIT));
        assert!(chrome.is_dir());
        #[cfg(unix)]
        for dir in [chrome.parent().unwrap(), &chrome] {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{}", dir.display());
        }
        // The checked copy is gone after the step; the record stays.
        assert!(!Path::new(file).exists());
        assert_eq!(record(&home), format!("{}\n", HASH_OURS));
    }

    /// `openssl x509 -in test-ca.pem -outform der | shasum -a 256`.
    const HASH_OURS: &str = "29e433200254dfce602212438e9d64e306b672db9bc4642a7620cebcc0fd8057";

    /// Its own row, trusted, and the certificate there: nothing changes.
    #[test]
    fn a_database_that_has_the_certificate_is_unchanged() {
        let home = Home::new();
        let fake = chrome_with(&home, &[(OURS, "C,,"), (ELSEWHERE, "C,,")], &[TEST_CA]);
        assert_eq!(result(home.install(&fake)).status, OpStatus::Unchanged);
        assert_eq!(tails(&fake), [vec!["-L"], vec!["-L", "-n", OURS, "-a"]]);
    }

    /// Its own row without `C` (an `-A` a password stopped half way): only
    /// the trust changes.
    #[test]
    fn an_untrusted_row_of_its_own_gets_its_trust() {
        let home = Home::new();
        let fake = chrome_with(&home, &[(OURS, ",,")], &[TEST_CA]);
        assert_eq!(result(home.install(&fake)).status, OpStatus::Done);
        let trust = ["-M", "-n", OURS, "-t", "C,,", "-f", "/dev/null"];
        assert_eq!(tails(&fake).last().unwrap(), &trust);
    }

    /// Only the fingerprints in this data folder's record are deleted: the
    /// root of another data folder stays, whatever its nickname.
    #[test]
    fn only_recorded_fingerprints_are_deleted() {
        let home = Home::new();
        fs::create_dir_all(home.files().dir()).unwrap();
        fs::write(record_path(&home.files()), format!("{SECOND_SHA256}\n")).unwrap();
        let rows = [(SECOND, "C,,"), (ELSEWHERE, "C,,"), (CA_NAME, "C,,")];
        let fake = chrome_with(&home, &rows, &[TEST_CA_2]);
        assert_eq!(result(home.install(&fake)).status, OpStatus::Done);
        let tails = tails(&fake);
        let deletes: Vec<_> = tails.iter().filter(|args| args[0] == "-D").collect();
        assert_eq!(deletes, [&["-D", "-n", SECOND, "-f", "/dev/null"]]);
        assert_eq!(tails.last().unwrap()[..5], ["-A", "-t", "C,,", "-n", OURS]);
        assert_eq!(record(&home), format!("{HASH_OURS}\n"));
    }

    /// An older fingerprint that could not be deleted stays in the record,
    /// and the step says so; this certificate still goes in.
    #[test]
    fn an_older_fingerprint_that_stays_stays_recorded() {
        let home = Home::new();
        fs::create_dir_all(home.files().dir()).unwrap();
        fs::write(
            record_path(&home.files()),
            format!("{SECOND_SHA256}\r\nnot a fingerprint\n"),
        )
        .unwrap();
        fs::create_dir_all(home.chrome()).unwrap();
        let rows = listing(&[(SECOND, "C,,")]);
        let fake = Fake::answering(move |_, args| match args[2].as_str() {
            "-L" => ok(&rows),
            "-D" => exit(255, "SEC_ERROR_TOKEN_NOT_LOGGED_IN"),
            _ => ok(""),
        });
        let failed = result(home.install(&fake));
        assert_eq!(failed.code.as_deref(), Some(NSS_ADD_FAILED));
        assert!(
            failed
                .message
                .unwrap()
                .contains("an older CA of this data folder")
        );
        assert!(tails(&fake).iter().any(|args| args[0] == "-A"));
        assert_eq!(record(&home), format!("{HASH_OURS}\n{SECOND_SHA256}\n"));
    }

    /// An older fingerprint stays recorded while any database may still
    /// hold it: one left alone, or one `certutil` could not list.
    #[test]
    fn older_fingerprints_stay_recorded_until_every_database_was_read() {
        for broken in ["refused", "unlisted"] {
            let home = Home::new();
            write_record_of(&home, &format!("{SECOND_SHA256}\n"));
            let profile = home.profile(&[".mozilla", "firefox"], "a.default");
            let unlisted = sql(&profile);
            if broken == "refused" {
                fs::write(home.home().join(".pki"), "").unwrap();
            }
            let rows = listing(&[(SECOND, "C,,")]);
            let fail_list = broken == "unlisted";
            let fake = Fake::answering(move |_, args| match (args[2].as_str(), args.len()) {
                ("-L", 3) if fail_list && args[1] == unlisted => {
                    exit(255, "SEC_ERROR_BAD_DATABASE")
                }
                ("-L", 3) => ok(&rows),
                ("-L", _) => exit(255, NOT_FOUND),
                _ => ok(""),
            });
            assert_eq!(
                result(home.install(&fake)).status,
                OpStatus::Failed,
                "{broken}"
            );
            assert_eq!(
                record(&home),
                format!("{HASH_OURS}\n{SECOND_SHA256}\n"),
                "{broken}"
            );
        }
    }

    /// The plain nickname of the first NSS step, holding this certificate: it is
    /// renamed, so the uninstall finds it by its fingerprint. Holding
    /// another one, it is left alone. (The fake lists the same roots for
    /// every nickname, as real `-L -n` does for one subject; `TEST_CA`'s own
    /// subject differs, which the code never looks at.)
    #[test]
    fn the_plain_nickname_is_renamed_only_when_it_holds_this_certificate() {
        let home = Home::new();
        let fake = chrome_with(
            &home,
            &[(CA_NAME, "C,,"), (SECOND, "C,,")],
            &[TEST_CA_2, TEST_CA],
        );
        assert_eq!(result(home.install(&fake)).status, OpStatus::Done);
        assert_eq!(
            tails(&fake)[2..],
            [
                vec![
                    "--rename",
                    "-n",
                    CA_NAME,
                    "--new-n",
                    OURS,
                    "-f",
                    "/dev/null"
                ],
                vec!["-M", "-n", OURS, "-t", "C,,", "-f", "/dev/null"],
            ]
        );

        // The plain row holds another root: this one is added beside it.
        let home = Home::new();
        let fake = chrome_with(&home, &[(CA_NAME, "C,,")], &[TEST_CA_2]);
        assert_eq!(result(home.install(&fake)).status, OpStatus::Done);
        let calls = tails(&fake);
        assert!(
            calls
                .iter()
                .all(|args| args[0] != "--rename" && args[0] != "-D"),
            "{calls:?}"
        );
        assert_eq!(calls.last().unwrap()[0], "-A");

        // This root is there, but which row holds it cannot be told: two
        // plain rows (`--rename` would pick either), or an unknown one
        // beside the plain one. `-A` would keep that row's nickname, so the
        // step says so and changes nothing.
        let two: &[(&str, &str)] = &[(CA_NAME, "C,,"), (CA_NAME, "C,,")];
        let unknown: &[(&str, &str)] = &[(CA_NAME, "C,,"), ("Marsa Local CA mine", "C,,")];
        for rows in [two, unknown] {
            let home = Home::new();
            let fake = chrome_with(&home, rows, &[TEST_CA]);
            let failed = result(home.install(&fake));
            assert_eq!(failed.code.as_deref(), Some(NSS_ADD_FAILED), "{rows:?}");
            assert!(failed.message.unwrap().contains("under another nickname"));
            let calls = tails(&fake);
            assert!(calls.iter().all(|args| args[0] == "-L"), "{calls:?}");
        }
    }

    /// An `-A` that fails is followed by a `-D` of its own nickname.
    #[test]
    fn a_failed_add_is_taken_out_again() {
        let home = Home::new();
        let fake = Fake::answering(|_, args| match args[2].as_str() {
            "-L" => no_marsa_rows(args),
            "-A" => exit(255, "SEC_ERROR_TOKEN_NOT_LOGGED_IN"),
            _ => ok(""),
        });
        let failed = result(home.install(&fake));
        assert_eq!(failed.code.as_deref(), Some(NSS_ADD_FAILED));
        assert_eq!(
            tails(&fake).last().unwrap(),
            &["-D", "-n", OURS, "-f", "/dev/null"]
        );
    }

    /// The record is read before any `certutil`: one that is a link stops
    /// the step.
    #[cfg(unix)]
    #[test]
    fn a_record_that_is_a_link_runs_nothing() {
        let home = Home::new();
        fs::create_dir_all(home.files().dir()).unwrap();
        let elsewhere = home.dir.path().join("elsewhere");
        fs::write(&elsewhere, format!("{SECOND_SHA256}\n")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, record_path(&home.files())).unwrap();
        let fake = fresh();
        let failed = result(home.install(&fake));
        assert_eq!(failed.code.as_deref(), Some(NSS_ADD_FAILED));
        assert!(fake.calls().is_empty());
    }

    /// A file where a folder should be is refused, with its own code when
    /// no `certutil` failed.
    #[test]
    fn a_file_in_place_of_a_folder_is_refused() {
        let home = Home::new();
        fs::write(home.home().join(".pki"), "").unwrap();
        let fake = fresh();
        let refused = result(home.install(&fake));
        assert_eq!(refused.code.as_deref(), Some(NSS_DB_REFUSED));
        assert!(refused.message.unwrap().contains(".pki is not a folder"));
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn no_certutil_skips_the_step() {
        let home = Home::new();
        let fake = Fake::answering(|_, _| Err(io::Error::from(io::ErrorKind::NotFound)));
        assert_eq!(home.install(&fake), Outcome::Skipped(NSS_TOOL_MISSING));
        assert_eq!(fake.calls().len(), 1);
        // Nothing was added, so nothing is recorded for the uninstall.
        assert!(!record_path(&home.files()).exists());
        // Nothing made for a step that did not run.
        assert!(!home.home().join(".pki").exists());
        assert!(!home.files().dir().join(PEM_FILE).exists());
    }

    #[test]
    fn one_database_that_fails_does_not_stop_the_others() {
        let home = Home::new();
        let first = home.profile(&[".mozilla", "firefox"], "a.default");
        let second = home.profile(&[".mozilla", "firefox"], "b.default");
        let broken = sql(&first);
        let fake = Fake::answering(move |_, args| match args[2].as_str() {
            "-L" => no_marsa_rows(args),
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
        let all = databases(&home.home());
        assert!(all.refused.is_empty(), "{:?}", all.refused);
        // One profile left out: the scan is not complete.
        assert!(!all.complete);
        let found = all.databases;
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
        // A real `cert9.db` next to a linked `key4.db`.
        let linked_key = home.profile(&snap, "key.default");
        symlink(profile.join(CERT_DB), linked_key.join("key4.db")).unwrap();

        let fake = fresh();
        let failed = result(home.install(&fake));
        assert_eq!(failed.code.as_deref(), Some(NSS_DB_REFUSED));
        let message = failed.message.unwrap();
        assert!(message.starts_with("4 NSS database(s)"), "{message}");
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
        assert!(
            message.contains("key4.db is not a file (a link, or a folder)"),
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

    fn remove_all(home: &Home, fake: &Arc<Fake>) -> OpResult {
        let stop = CancellationToken::new();
        remove(fake.as_ref(), &home.home(), &home.files(), &stop)
    }

    /// No record: nothing of this data folder's to take out, and nothing
    /// runs, not even `certutil -L`.
    #[test]
    fn removal_without_a_record_runs_nothing() {
        let home = Home::new();
        let fake = fresh();
        let unchanged = remove_all(&home, &fake);
        assert_eq!(
            (unchanged.op.as_str(), unchanged.status),
            (NSS_REMOVE, OpStatus::Unchanged)
        );
        assert!(fake.calls().is_empty());
    }

    /// Each recorded root goes from each database that has its row; the
    /// root of another data folder stays, and a database that is not there
    /// is not made.
    #[test]
    fn removal_deletes_only_recorded_roots_and_then_the_record() {
        let home = Home::new();
        write_record_of(&home, &format!("{HASH_OURS}\n{SECOND_SHA256}\n"));
        let firefox = home.profile(&[".mozilla", "firefox"], "a.default");
        let rows = listing(&[(OURS, "C,,"), (ELSEWHERE, "C,,")]);
        let fake = Fake::answering(move |_, args| match args[2].as_str() {
            "-L" => ok(&rows),
            _ => ok(""),
        });
        let done = remove_all(&home, &fake);
        assert_eq!(
            (done.op.as_str(), done.status),
            (NSS_REMOVE, OpStatus::Done)
        );
        // Chrome's folder is not there: only the profile is read.
        assert!(!home.chrome().exists());
        assert_eq!(
            fake.calls()
                .iter()
                .map(|call| call.args.clone())
                .collect::<Vec<_>>(),
            [
                vec!["-d".to_owned(), sql(&firefox), "-L".to_owned()],
                ["-d", &sql(&firefox), "-D", "-n", OURS, "-f", "/dev/null"]
                    .map(str::to_owned)
                    .to_vec(),
            ]
        );
        assert!(!record_path(&home.files()).exists());
    }

    /// Without `certutil`, or with a `-D` that fails, the roots stay
    /// recorded, so the next uninstall tries again.
    #[test]
    fn removal_that_fails_keeps_the_record() {
        let home = Home::new();
        write_record_of(&home, &format!("{HASH_OURS}\n"));
        home.profile(&[".mozilla", "firefox"], "a.default");
        let missing = Fake::answering(|_, _| Err(io::Error::from(io::ErrorKind::NotFound)));
        let failed = remove_all(&home, &missing);
        assert_eq!(failed.code.as_deref(), Some(NSS_REMOVE_FAILED));
        assert!(
            failed
                .message
                .unwrap()
                .contains("certutil is not installed")
        );
        assert_eq!(missing.calls().len(), 1);

        let rows = listing(&[(OURS, "C,,")]);
        let refused = Fake::answering(move |_, args| match args[2].as_str() {
            "-L" => ok(&rows),
            _ => exit(255, "SEC_ERROR_TOKEN_NOT_LOGGED_IN"),
        });
        let failed = remove_all(&home, &refused);
        assert_eq!(failed.code.as_deref(), Some(NSS_REMOVE_FAILED));
        assert_eq!(
            fs::read_to_string(record_path(&home.files())).unwrap(),
            format!("{HASH_OURS}\n")
        );
    }

    /// A database left alone may still hold a root: the record stays,
    /// though every database that was read lost its row.
    #[test]
    fn removal_with_a_database_left_alone_keeps_the_record() {
        let home = Home::new();
        write_record_of(&home, &format!("{HASH_OURS}\n"));
        home.profile(&[".mozilla", "firefox"], "a.default");
        fs::write(home.home().join(".pki"), "").unwrap();
        let rows = listing(&[(OURS, "C,,")]);
        let fake = Fake::answering(move |_, args| match args[2].as_str() {
            "-L" => ok(&rows),
            _ => ok(""),
        });
        assert_eq!(remove_all(&home, &fake).status, OpStatus::Done);
        assert_eq!(record(&home), format!("{HASH_OURS}\n"));
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
