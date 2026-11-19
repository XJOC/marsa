//! The `certutil` calls of the NSS step: each with its own arguments, run
//! through [`Runner`] with no shell, and what their output says.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::super::user_trust::{self, PEM_END};
use super::super::{PEM_BEGIN, PIPE_GRACE, RunOutput, Runner};
use super::Failure;

/// The tool, by its absolute path (AGENTS.md section 6).
pub const CERTUTIL: &str = "/usr/bin/certutil";
/// `certutil` reads no password from here: a database with one fails at
/// once instead of asking on the terminal `marsad` may still have.
const NO_PASSWORD: &str = "/dev/null";
/// The most one `certutil` may take. Each takes milliseconds.
pub(super) const CALL_LIMIT: Duration = Duration::from_secs(10);
/// The nickname of Marsa's root in each database: `CA_NAME` in
/// `marsa_core::caddy`, as in `user_trust`.
pub(super) const NICKNAME: &str = "Marsa Local CA";

/// The `certutil` calls of one step, all within one deadline.
pub(super) struct Calls<'a> {
    pub(super) runner: &'a dyn Runner,
    pub(super) stop: &'a CancellationToken,
    pub(super) deadline: Instant,
}

impl Calls<'_> {
    /// Runs `certutil` with `args`, at most [`CALL_LIMIT`] and never past
    /// the deadline, its pipe grace included.
    pub(super) fn run(&self, args: &[OsString]) -> Result<RunOutput, Failure> {
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
    pub(super) fn expect_success(&self, args: &[OsString], command: &str) -> Result<(), Failure> {
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

    /// Whether a certificate named [`NICKNAME`] in `dir` is trusted to issue
    /// server certificates: `C` in the SSL field of its row in `certutil
    /// -L` (`c` is only "valid", not trusted).
    pub(super) fn trusted(&self, dir: &Path) -> Result<bool, Failure> {
        let output = self.run(
            &database_args(dir)
                .into_iter()
                .chain(["-L".into()])
                .collect::<Vec<_>>(),
        )?;
        if output.code != Some(0) {
            return Ok(false);
        }
        Ok(trust_rows(&String::from_utf8_lossy(&output.stdout)).any(|ssl| ssl.contains('C')))
    }

    /// The certificates named [`NICKNAME`] in `dir`, as base64. A listing
    /// that fails (the usual "Could not find cert") lists none.
    pub(super) fn list(&self, dir: &Path) -> Result<Vec<String>, Failure> {
        let output = self.run(&list_args(dir))?;
        if output.code != Some(0) {
            return Ok(Vec::new());
        }
        Ok(bodies(&String::from_utf8_lossy(&output.stdout)))
    }
}

/// `-d sql:<dir>`: the database, in the SQL format.
fn database_args(dir: &Path) -> Vec<OsString> {
    let mut database = OsString::from("sql:");
    database.push(dir.as_os_str());
    vec!["-d".into(), database]
}

pub(super) fn list_args(dir: &Path) -> Vec<OsString> {
    let mut args = database_args(dir);
    args.extend(["-L".into(), "-n".into(), NICKNAME.into(), "-a".into()]);
    args
}

pub(super) fn delete_args(dir: &Path) -> Vec<OsString> {
    let mut args = database_args(dir);
    args.extend(["-D", "-n", NICKNAME, "-f", NO_PASSWORD].map(OsString::from));
    args
}

pub(super) fn add_args(dir: &Path, file: &Path) -> Vec<OsString> {
    let mut args = database_args(dir);
    let rest = ["-A", "-t", "C,,", "-n", NICKNAME, "-f", NO_PASSWORD, "-i"];
    args.extend(rest.map(OsString::from));
    args.push(file.into());
    args
}

/// The SSL trust field of each row named [`NICKNAME`] in the output of
/// `certutil -L`: `Marsa Local CA      C,,` gives `C`.
pub(super) fn trust_rows(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter_map(|line| {
        let (name, trust) = line.trim_end().rsplit_once(char::is_whitespace)?;
        (name.trim() == NICKNAME).then(|| trust.split(',').next().unwrap_or_default())
    })
}

/// The base64 of each certificate in `text`, without line breaks.
pub(super) fn bodies(text: &str) -> Vec<String> {
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
