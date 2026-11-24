//! The pid file of a pool's php-fpm master: what it records, how it is written and
//! deleted, and which php-fpm programs count as ours.

use marsa_core::data_dir::DataDir;
use std::path::{Path, PathBuf};
use std::time::Duration;

use marsa_core::orphan::{Liveness, PidRecord};
use marsa_core::php::PhpMinor;
use marsa_core::platform::process;
use marsa_core::ps::StartTime;
use marsa_core::recipes::Platform;
use marsa_core::runtimes::{Runtime, RuntimesDir};
use tracing::{debug, warn};

use super::super::{fpm_id, php_program};
use crate::daemon::services::probe::ProcessProbe;

/// Program `name` (`php-fpm`, `php`) of each installed PHP version of
/// `data_dir`. An installed list that cannot be read gives none, so nothing
/// counts as ours.
pub fn installed_programs(data_dir: &Path, platform: Platform, name: &str) -> Vec<PathBuf> {
    let versions = RuntimesDir::in_dir(data_dir)
        .installed(Runtime::Php)
        .unwrap_or_else(|err| {
            warn!(code = err.code(), %err, "cannot list the PHP versions");
            Vec::new()
        });
    let programs = versions
        .iter()
        .filter_map(|version| php_program(data_dir, platform, version, name));
    accepted(programs)
}

/// Each of `programs`, then its path with links resolved (what `lsof` and
/// `/proc` report), each once. Only compared, never passed to a program
/// (architecture.md section 10).
pub fn accepted(programs: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut accepted = Vec::new();
    for program in programs {
        let real = std::fs::canonicalize(&program).ok();
        for path in [Some(program), real].into_iter().flatten() {
            if !accepted.contains(&path) {
                accepted.push(path);
            }
        }
    }
    accepted
}

/// What one look at a php-fpm just spawned did ([`PidFile::record`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Recorded {
    /// The file names it now, with this record.
    Written(PidRecord),
    /// Not yet, and what the OS says now: `Unknown`, the daemon's own
    /// executable, or our php-fpm while the daemon's children cannot be
    /// listed. On Linux, with glibc's `posix_spawn`, `spawn` returns before
    /// the exec has switched the child to its new program: the kernel wakes
    /// the parent first (`exec_mmap` in `fs/exec.c`), and until the switch
    /// the child's `exe` link names the daemon. std's fork and exec, and
    /// musl, wait for the exec, so they have no such moment. The watcher
    /// looks again ([`RECORD_LIMIT`]).
    NotYet(Liveness),
    /// Never: it is gone, runs another program, is not a child of the
    /// daemon, its pid changed hands, or the file cannot be written.
    Never,
}

/// How long the watcher looks again at a php-fpm just spawned whose pid
/// file is not written yet ([`Recorded::NotYet`]), every `POLL`.
pub const RECORD_LIMIT: Duration = Duration::from_secs(5);

/// The daemon's own executable, in each spelling: what the `exe` link of a
/// child names until its exec is done ([`Recorded::NotYet`]).
pub(crate) fn own_executable() -> Vec<PathBuf> {
    accepted(std::env::current_exe().ok())
}

/// `run/php-fpm-<minor>.pid`.
#[derive(Debug, Clone)]
pub struct PidFile {
    pub path: PathBuf,
}

impl PidFile {
    pub fn new(data_dir: &Path, minor: PhpMinor) -> Self {
        let name = format!("{}.pid", fpm_id(minor));
        Self {
            path: DataDir::new(data_dir).run().join(name),
        }
    }

    /// One look at a php-fpm just spawned: writes the file only if `pid`
    /// really runs `program` now, and is a direct child of this daemon with
    /// the same start time in the process list. `started` is its start time
    /// from an earlier look, if one was read: a pid that shows another one
    /// now has changed hands, and is never written.
    pub async fn record(
        &self,
        processes: &dyn ProcessProbe,
        pid: u32,
        program: &Path,
        started: Option<StartTime>,
    ) -> Recorded {
        let now = processes.real_identity(pid).await;
        let (exe, now_started) = match &now {
            Liveness::Alive { exe, started } => (exe.clone(), *started),
            // It exited at once; the supervisor reports why.
            Liveness::Gone => {
                debug!(pid, "php-fpm exited before its pid file was written");
                return Recorded::Never;
            }
            Liveness::Unknown => {
                debug!(pid, "cannot tell yet who runs under the pid of php-fpm");
                return Recorded::NotYet(now);
            }
        };
        if started.is_some_and(|then| then != now_started) {
            warn!(pid, "php-fpm pid file not written: the pid changed hands");
            return Recorded::Never;
        }
        if !accepted([program.to_path_buf()]).contains(&exe) {
            if own_executable().contains(&exe) {
                debug!(pid, "the exec of php-fpm is not done yet");
                return Recorded::NotYet(now);
            }
            let exe = exe.display();
            warn!(pid, %exe, "php-fpm pid file not written: the pid runs another program");
            return Recorded::Never;
        }
        // Our program, and our own child: not a process of the same program
        // that took the pid over.
        let Some(children) = processes.children(std::process::id()).await else {
            debug!(pid, "cannot list the children of the daemon yet");
            return Recorded::NotYet(now);
        };
        match children.iter().find(|child| child.pid == pid) {
            Some(child) if child.started == now_started => {}
            Some(_) => {
                warn!(
                    pid,
                    "php-fpm pid file not written: the process list gives another start time"
                );
                return Recorded::Never;
            }
            None => {
                warn!(
                    pid,
                    "php-fpm pid file not written: not a child of the daemon"
                );
                return Recorded::Never;
            }
        }
        let record = PidRecord {
            pid,
            exe,
            started: now_started,
        };
        match process::write_pid_file(&self.path, &record) {
            Ok(()) => {
                debug!(pid, path = %self.path.display(), "php-fpm pid file written");
                Recorded::Written(record)
            }
            Err(err) => {
                warn!(%err, path = %self.path.display(), "cannot write the php-fpm pid file");
                Recorded::Never
            }
        }
    }

    /// After php-fpm exited: its pid may belong to anyone from now on.
    pub fn remove(&self) {
        if let Err(err) = process::remove_pid_file(&self.path) {
            warn!(%err, path = %self.path.display(), "cannot delete the php-fpm pid file");
        }
    }

    /// Where orphan recovery is off: deletes a file left from before,
    /// without a check and without a signal.
    pub fn discard(&self) {
        if let Some(record) = process::read_pid_file(&self.path) {
            warn!(
                pid = record.pid,
                path = %self.path.display(),
                "orphan recovery is off on this system; deleting the php-fpm pid file, no signal"
            );
        }
        self.remove();
    }
}
