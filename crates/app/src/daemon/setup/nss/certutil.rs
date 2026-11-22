//! The `certutil` calls of the NSS step: each with its own arguments, run
//! through [`Runner`] with no shell, and what their output says.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::super::user_trust::{self, PEM_END};
use super::super::{PEM_BEGIN, PIPE_GRACE, RunOutput, Runner};
use super::Failure;
use super::databases::CERT_DB;

/// The tool, by its absolute path (AGENTS.md section 6).
pub const CERTUTIL: &str = "/usr/bin/certutil";
/// `certutil` reads no password from here: a database with one fails at
/// once instead of asking on the terminal `marsad` may still have.
const NO_PASSWORD: &str = "/dev/null";
/// The most one `certutil` may take. Each takes milliseconds.
pub(super) const CALL_LIMIT: Duration = Duration::from_secs(10);
/// The common name of every Marsa root (`CA_NAME` in `marsa_core::caddy`),
/// and the nickname of the first NSS step, before nicknames carried a
/// fingerprint.
pub(super) const CA_NAME: &str = "Marsa Local CA";

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
        Err(Failure::Other(ended(command, &output)))
    }

    /// The rows of `certutil -L` whose nickname starts with Marsa's name. A
    /// folder with no `cert9.db` has none; a listing that fails where there
    /// is one is a failure, so no fingerprint counts as gone from it.
    pub(super) fn rows(&self, dir: &Path) -> Result<Vec<Row>, Failure> {
        let mut args = database_args(dir);
        args.push("-L".into());
        let output = self.run(&args)?;
        if output.code != Some(0) {
            let database = fs::symlink_metadata(dir.join(CERT_DB)).is_ok_and(|meta| meta.is_file());
            if database {
                return Err(Failure::Other(ended("-L", &output)));
            }
            return Ok(Vec::new());
        }
        Ok(rows(&String::from_utf8_lossy(&output.stdout)))
    }

    /// The certificates `-L -n <nickname> -a` gives, as base64: every one
    /// with the subject of the certificate named `nickname`. A listing that
    /// fails (the usual "Could not find cert") lists none.
    pub(super) fn list(&self, dir: &Path, nickname: &str) -> Result<Vec<String>, Failure> {
        let output = self.run(&list_args(dir, nickname))?;
        if output.code != Some(0) {
            return Ok(Vec::new());
        }
        Ok(bodies(&String::from_utf8_lossy(&output.stdout)))
    }

    /// Whether the certificate whose base64 is `body` is among those of
    /// [`Calls::list`] for `nickname`.
    pub(super) fn holds(&self, dir: &Path, nickname: &str, body: &str) -> Result<bool, Failure> {
        Ok(self
            .list(dir, nickname)?
            .iter()
            .any(|listed| listed == body))
    }
}

/// How `certutil <command>` ended, for a message.
fn ended(command: &str, output: &RunOutput) -> String {
    let exit = output
        .code
        .map_or_else(|| "a signal".to_owned(), |code| format!("exit code {code}"));
    format!("certutil {command} ended with {exit}")
}

/// `-d sql:<dir>`: the database, in the SQL format.
fn database_args(dir: &Path) -> Vec<OsString> {
    let mut database = OsString::from("sql:");
    database.push(dir.as_os_str());
    vec!["-d".into(), database]
}

/// `database_args`, then `rest`.
fn args(dir: &Path, rest: &[&str]) -> Vec<OsString> {
    let mut args = database_args(dir);
    args.extend(rest.iter().map(OsString::from));
    args
}

fn list_args(dir: &Path, nickname: &str) -> Vec<OsString> {
    args(dir, &["-L", "-n", nickname, "-a"])
}

pub(super) fn delete_args(dir: &Path, nickname: &str) -> Vec<OsString> {
    args(dir, &["-D", "-n", nickname, "-f", NO_PASSWORD])
}

pub(super) fn trust_args(dir: &Path, nickname: &str) -> Vec<OsString> {
    args(dir, &["-M", "-n", nickname, "-t", "C,,", "-f", NO_PASSWORD])
}

pub(super) fn rename_args(dir: &Path, from: &str, to: &str) -> Vec<OsString> {
    args(
        dir,
        &["--rename", "-n", from, "--new-n", to, "-f", NO_PASSWORD],
    )
}

pub(super) fn add_args(dir: &Path, nickname: &str, file: &Path) -> Vec<OsString> {
    let mut args = args(
        dir,
        &["-A", "-t", "C,,", "-n", nickname, "-f", NO_PASSWORD, "-i"],
    );
    args.push(file.into());
    args
}

/// One row of `certutil -L`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Row {
    pub(super) nickname: String,
    /// The trust fields, `SSL,S/MIME,JAR/XPI`: `C,,`.
    pub(super) trust: String,
}

impl Row {
    /// `C` in the SSL field: trusted to issue server certificates (`c` is
    /// only "valid").
    pub(super) fn trusted(&self) -> bool {
        self.trust
            .split(',')
            .next()
            .is_some_and(|ssl| ssl.contains('C'))
    }
}

/// The rows of `certutil -L` whose nickname is Marsa's name or starts with
/// it and a space: `Marsa Local CA 29e433200254dfce      C,,`.
pub(super) fn rows(text: &str) -> Vec<Row> {
    text.lines()
        .filter_map(|line| {
            let (nickname, trust) = line.trim_end().rsplit_once(char::is_whitespace)?;
            let nickname = nickname.trim();
            let ours = nickname == CA_NAME
                || nickname
                    .strip_prefix(CA_NAME)
                    .is_some_and(|rest| rest.starts_with(' '));
            ours.then(|| Row {
                nickname: nickname.to_owned(),
                trust: trust.to_owned(),
            })
        })
        .collect()
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

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    const OURS: &str = "Marsa Local CA 29e433200254dfce";

    /// `certutil -L` with `rows`, each `(nickname, trust)`.
    pub(in super::super) fn listing(rows: &[(&str, &str)]) -> String {
        let mut text = "\nCertificate Nickname                                         Trust Attributes\n\
                        \x20                                                            SSL,S/MIME,JAR/XPI\n\n"
            .to_owned();
        for (nickname, trust) in rows {
            text.push_str(&format!("{nickname:<61}{trust:<5}\n"));
        }
        text
    }

    #[test]
    fn rows_keep_marsas_nicknames_and_their_trust() {
        let text = listing(&[
            ("Other CA", "CT,C,C"),
            (CA_NAME, ",,"),
            (OURS, "C,,"),
            ("Marsa Local CAx", "C,,"),
        ]);
        let rows = rows(&text);
        let names: Vec<_> = rows.iter().map(|row| row.nickname.as_str()).collect();
        assert_eq!(names, [CA_NAME, OURS]);
        assert!(!rows[0].trusted());
        assert!(rows[1].trusted());
        let valid_only = Row {
            nickname: OURS.into(),
            trust: "c,,".into(),
        };
        assert!(!valid_only.trusted());
    }
}
