//! The installed programs of a server, and the first init of its data
//! folder, in a temp folder that is renamed into place at the end.

use marsa_core::codes;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use marsa_core::databases::ServerKind;
use marsa_core::platform::{probe, process};
use marsa_core::recipes::{self, Platform};
use marsa_core::runtimes::RuntimesDir;
use semver::Version;
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;
use tracing::{info, warn};

use crate::daemon::php::orphan::accepted;

/// The last lines of a failed step's output kept in its error.
const ERROR_LINES: usize = 10;

/// `program` of the installed `version` of `kind`, where its recipe puts it
/// under `runtimes/<runtime>/<version>/`. Absolute: `marsad` never searches
/// `PATH`. `None` if the recipe has no such program for `platform`.
pub(super) fn program(
    kind: ServerKind,
    data_dir: &Path,
    platform: Platform,
    version: &Version,
    program: &str,
) -> Option<PathBuf> {
    // A test parses every built-in recipe, so an error here never ships.
    let recipe = recipes::builtin(kind.runtime()).ok()?.platform(platform)?;
    let dir = RuntimesDir::in_dir(data_dir).version_dir(kind.runtime(), version);
    recipe.program_path(&dir, program)
}

/// The server program of `version`: [`ServerKind::server_program`].
pub(super) fn server(
    kind: ServerKind,
    data_dir: &Path,
    platform: Platform,
    version: &Version,
) -> Option<PathBuf> {
    program(kind, data_dir, platform, version, kind.server_program())
}

/// The server program of every installed version of `kind`, and the
/// resolved form of each: the programs that a pid file of this data folder
/// may name. An older version may still run on the folder. A failure to list
/// the versions gives fewer paths, so fewer pids count as ours.
pub(super) fn installed_servers(
    kind: ServerKind,
    data_dir: &Path,
    platform: Platform,
) -> Vec<PathBuf> {
    let versions = RuntimesDir::in_dir(data_dir)
        .installed(kind.runtime())
        .unwrap_or_else(|err| {
            warn!(code = err.code(), %err, server = kind.id(), "cannot list the versions");
            Vec::new()
        });
    let servers = versions
        .iter()
        .filter_map(|version| server(kind, data_dir, platform, version));
    accepted(servers)
}

/// Why the first init of a data folder failed. Nothing is left behind: no
/// data folder, and no temp folder. Except when `in_place`: then the folder
/// was moved into place, and only the sync of its parent failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitError {
    pub kind: ServerKind,
    pub reason: String,
    /// The new data folder is in place, and the next start uses it. Only the
    /// sync of the folder that holds it failed, so a crash before the OS
    /// writes that folder could still undo the rename.
    pub in_place: bool,
}

impl InitError {
    /// Stable error name for the UI.
    pub fn code(&self) -> &'static str {
        match self.kind {
            ServerKind::Postgres => codes::POSTGRES_INIT_FAILED,
        }
    }

    pub(super) fn new(kind: ServerKind, reason: String) -> Self {
        Self {
            kind,
            reason,
            in_place: false,
        }
    }

    pub(super) fn io(kind: ServerKind, what: &str, err: &io::Error) -> Self {
        Self::new(kind, format!("{what}: {err}"))
    }
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (name, reason) = (self.kind.name(), &self.reason);
        if self.in_place {
            write!(
                f,
                "the {name} data folder was made and is in place, but {reason}; the next \
                 start uses it"
            )
        } else {
            write!(f, "the {name} data folder was not made: {reason}")
        }
    }
}

impl std::error::Error for InitError {}

/// Makes the data folder `data` with `make` if nothing is there yet, and
/// returns whether it made one. Anything at that path, even an empty folder
/// or a link, counts as there: it is never read, run, or changed here, and
/// the server says at its start whether it is its data.
///
/// `make` fills a new temp folder `.<name>.init-<random>` next to the
/// target. Then a rename to `data`, on the same volume, so the folder
/// appears whole or not at all, then a sync of the folder that holds it.
///
/// Temp folders that an earlier init left (a crash, a power cut) are deleted
/// first: none of them was ever a data folder in use.
///
/// # Errors
///
/// The init error of `kind`, with the step and the end of its output. The
/// temp folder is deleted then.
pub(super) async fn ensure_data(
    kind: ServerKind,
    data: &Path,
    make: impl AsyncFnOnce(&Path) -> Result<(), InitError>,
) -> Result<bool, InitError> {
    match tokio::fs::symlink_metadata(data).await {
        Ok(_) => return Ok(false),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(InitError::io(kind, "cannot look at the data folder", &err)),
    }
    let (Some(parent), Some(name)) = (data.parent(), data.file_name()) else {
        let reason = format!("{} has no parent folder", data.display());
        return Err(InitError::new(kind, reason));
    };
    let prefix = format!(".{}.init-", name.to_string_lossy());
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|err| InitError::io(kind, "cannot create the databases folder", &err))?;
    remove_leftovers(parent, &prefix).await;
    let temp = parent.join(format!("{prefix}{:016x}", probe::random()));
    info!(data = %data.display(), server = kind.id(), "making the data folder");
    let made = async {
        make(&temp).await?;
        move_into_place(kind, &temp, data).await
    };
    let made = made.await;
    if made.is_err()
        && let Err(err) = remove_dir(&temp).await
    {
        warn!(temp = %temp.display(), %err, "cannot delete the temp folder of a failed init");
    }
    made.map(|()| true)
}

/// The rename of the full `temp` to `data`, and the sync of its parent.
async fn move_into_place(kind: ServerKind, temp: &Path, data: &Path) -> Result<(), InitError> {
    // Checked again: a rename would replace an empty folder made meanwhile.
    if tokio::fs::symlink_metadata(data).await.is_ok() {
        let reason = format!("{} appeared during the init", data.display());
        return Err(InitError::new(kind, reason));
    }
    tokio::fs::rename(temp, data)
        .await
        .map_err(|err| InitError::io(kind, "cannot move the new data folder into place", &err))?;
    // The rename is on disk only once its folder is: after a crash before
    // that, the data could be back under its temp name, which the next init
    // deletes as a leftover.
    let parent = data.parent().map(Path::to_path_buf);
    tokio::task::spawn_blocking(move || {
        parent.map_or(Ok(()), |parent| marsa_core::fs::sync_dir(&parent))
    })
    .await
    .map_err(io::Error::other)
    .flatten()
    .map_err(|err| InitError {
        kind,
        reason: format!("its folder could not be synced to disk: {err}"),
        in_place: true,
    })?;
    info!(data = %data.display(), server = kind.id(), "the data folder is ready");
    Ok(())
}

/// One step of an init.
pub(super) struct Step<'a> {
    pub kind: ServerKind,
    /// Its name in the error.
    pub what: &'a str,
    /// Written to its stdin, which is then closed.
    pub input: Option<&'a str>,
    pub limit: Duration,
    /// A line with one of these fails the step, whatever its exit code: in
    /// case a session went on after an error.
    pub severe: &'a [&'a str],
}

/// Runs `cmd` as `step`, and waits up to its limit. A step that runs longer
/// is killed (`kill_on_drop`).
///
/// It fails on an exit code other than 0, or on a line with one of the
/// step's severe words.
pub(super) async fn run_step(mut cmd: Command, step: Step<'_>) -> Result<(), InitError> {
    let Step {
        kind,
        what,
        input,
        limit,
        severe,
    } = step;
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    process::hide_console_window(&mut cmd);
    let failed = |reason: String| InitError::new(kind, format!("{what} {reason}"));
    let mut child = cmd
        .spawn()
        .map_err(|err| failed(format!("did not start: {err}")))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        // Dropped at the end of the block: the step reads to the end.
        stdin
            .write_all(input.as_bytes())
            .await
            .map_err(|err| failed(format!("did not read its input: {err}")))?;
    }
    let output = tokio::time::timeout(limit, child.wait_with_output())
        .await
        .map_err(|_| failed(format!("took longer than {} s", limit.as_secs())))?
        .map_err(|err| failed(format!("cannot be waited for: {err}")))?;
    let text = [&output.stderr, &output.stdout]
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .join("\n");
    let error_line = text
        .lines()
        .any(|line| severe.iter().any(|s| line.contains(s)));
    if output.status.success() && !error_line {
        return Ok(());
    }
    let lines: Vec<_> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let tail = lines[lines.len().saturating_sub(ERROR_LINES)..].join(" | ");
    Err(failed(format!("failed ({}): {tail}", output.status)))
}

/// Deletes the temp folders of earlier inits of this line in `parent`: the
/// folders whose name starts with `prefix`. Never one of another line, and
/// never a link (`file_type` does not follow it).
async fn remove_leftovers(parent: &Path, prefix: &str) {
    let Ok(mut entries) = tokio::fs::read_dir(parent).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let ours = entry.file_name().to_string_lossy().starts_with(prefix);
        if !ours || !entry.file_type().await.is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        match remove_dir(&path).await {
            Ok(()) => info!(temp = %path.display(), "deleted the temp folder of an earlier init"),
            Err(err) => warn!(temp = %path.display(), %err, "cannot delete an old temp folder"),
        }
    }
}

/// Deletes `dir` and what is in it. Nothing there is not an error.
async fn remove_dir(dir: &Path) -> io::Result<()> {
    match tokio::fs::remove_dir_all(dir).await {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}
