//! Finds and stops a php-fpm that a crashed `marsad` left running,
//! with the rules of ADR 0009 that `caddy::orphan` follows for Caddy.
//!
//! After a SIGKILL of the daemon, php-fpm lives on and keeps its socket, so
//! the next php-fpm of the same PHP line fails ("Another FPM instance seems
//! to already listen") and the line's sites answer 502. So:
//!
//! - after each spawn of a pool, its [`watch`]er writes
//!   `run/php-fpm-<minor>.pid` (pid, real executable, start time) once the
//!   new process runs php-fpm and the process list shows it as a direct
//!   child of the daemon with that start time, and deletes it after each
//!   exit. It looks again for a moment ([`RECORD_LIMIT`]) while the process
//!   still runs the daemon's own executable, as on Linux right after `spawn`
//!   ([`Recorded::NotYet`]), and stops at a change of its start time: the
//!   pid changed hands;
//! - before a pool starts, [`Orphans`] reads that file. A pid counts as ours
//!   only if it is alive, really runs the php-fpm of an installed PHP version
//!   of this data folder (an older version counts: an update may have come in
//!   between), and started at the recorded time. It gets SIGTERM, then
//!   SIGKILL after a limit, each only after the same check passes again.
//!   The pool starts once the process is gone and its socket or port is
//!   free.
//! - php-fpm's workers end with a master that stops on SIGTERM. A master
//!   that only dies from SIGKILL cannot end them, and macOS has no
//!   parent-death signal, so they live on under pid 1 and keep the socket.
//!   So, right before the master's SIGKILL, its direct
//!   children are listed (`ps`), and the ones that run the master's very
//!   executable and started no earlier than it are remembered
//!   ([`orphan::worker_of`]). If they cannot be listed, the master gets no
//!   SIGKILL: its workers would live on with no record of whose they are.
//!   Once the master is gone, each remembered worker gets SIGTERM, then
//!   SIGKILL after a limit, each only right after the same check passes
//!   again (same executable, same start time).
//! - After a SIGKILL of the master, after its pid changed hands, and when
//!   the socket or port is still in use once the master is gone, the
//!   children of pid 1 are searched too: a worker forked after the list was
//!   made is there. Under pid 1 the executable and the start time prove too
//!   little: a php-fpm of this data folder that the user started by hand
//!   sits there as well (ADR 0009, R5e). So a child of pid 1 must also hold
//!   a listener at this pool's socket path or port (`lsof` on macOS,
//!   `/proc` on Linux). That is a filter, not a proof: it shows who holds a
//!   listener there, not who
//!   inherited it from our master. A child of pid 1 that passes it gets the
//!   same two signals after the same checks.
//! - Anything else is never signalled. A stale file is deleted, and the log
//!   says why. What cannot be told is never signalled either: a pid whose
//!   identity cannot be read, or a list that cannot be read (a tool
//!   failed). Then the pool waits, and the file stays, while the socket or
//!   port is in use; once it is free, the file goes and the pool starts.
//!   When a search that worked finds nothing of ours and no signal went
//!   out, the file goes and the pool starts: php-fpm itself then says what
//!   holds its socket.
//!
//! Known limits:
//!
//! - A process whose identity cannot be read at the moment it is listed is
//!   taken for a bystander, since most are, unless it holds the pool's
//!   listener. It gets no signal and does not block the pool; if it was a
//!   worker, php-fpm says that its socket is in use. A worker that was
//!   verified and cannot be read later does block the pool, with
//!   `php_fpm_orphan_stuck`, and so does a holder of the listener under
//!   pid 1 that cannot be read.
//! - The listener filter cannot tell our worker from a php-fpm of this data
//!   folder, under pid 1 and started after our master, that listens at the
//!   very same place: one that bound this pool's socket path itself, or one
//!   on the TCP fallback port, which is not private to Marsa. Such a
//!   process gets the signals of a worker.
//! - Linux: workers under a subreaper that is not an ancestor of this
//!   daemon are not found. The daemon starts detached, so its ancestors can
//!   differ from those of the daemon that crashed (started from a terminal
//!   once, from the tray the next time). The search works and finds
//!   nothing, so the file goes and php-fpm says that its socket is in use.
//! - Linux: `/proc/net/unix` prints socket paths as they are, so another
//!   user can bind a path with a newline that reads as one more line. A
//!   forged line that repeats a real socket's inode is dropped
//!   ([`orphan::parse_proc_net_unix`]); one that repeats our own
//!   listener's makes the holders unknown, so the pool waits with
//!   `php_fpm_orphan_stuck` and nothing is signalled. One that names the
//!   inode of a TCP socket of this user may add its holder to the list.
//!   That holder still needs every check of a worker.
//!
//! On Linux (ADR 0025) an orphan goes to its nearest subreaper, for example
//! `systemd --user`, not always to pid 1. So "pid 1" above means each of
//! [`process::adopters`]: pid 1 on macOS, and on Linux the ancestors of this
//! daemon up to pid 1. Most of those adopt nothing (a shell, the tray);
//! their children are searched with the same checks, so a php-fpm of this
//! data folder that the user started by hand under one of them, on this
//! pool's socket or port, is the known limit above. The holders of a
//! listener come from `/proc` there ([`process::listener_holders`]).
//!
//! php-fpm writes its own `argv[0]` (`php-fpm: master process (...)`), so
//! `ps` shows no path for it. Identity is the file it really runs instead
//! ([`process::real_identity`]: `lsof` on macOS, `/proc` on Linux).
//!
//! All of this runs only where `process::FPM_ORPHAN_RECOVERY` is on (macOS
//! and Linux).
//! Elsewhere no pid file is written, nothing is signalled, and a file left
//! from before is deleted.

use marsa_core::codes;
use marsa_core::data_dir::DataDir;
use std::cmp::Ordering;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use marsa_core::caddy::Dial;
use marsa_core::orphan::{self, Liveness, PidRecord, PidfileState};
use marsa_core::php::PhpMinor;
use marsa_core::platform::process::{self, Signal};
use marsa_core::ps::{StartTime, started_order};
use marsa_core::recipes::Platform;
use marsa_core::runtimes::{Runtime, RuntimesDir};
use marsa_core::types::{Event, ServiceId, ServiceState};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::{fpm_id, fpm_program};
use crate::daemon::services::caddy::orphan::Ladder;
use crate::daemon::services::probe::ProcessProbe;
use crate::daemon::services::runtime::AbortOnDrop;
use crate::daemon::services::supervisor::{ServiceHandle, ServiceRuntime, lock};

/// Our old php-fpm did not stop, or cannot be verified, while its socket or
/// port stays in use.
pub const ORPHAN_STUCK: &str = codes::PHP_FPM_ORPHAN_STUCK;

/// The line before a stop, in the pool's recent lines.
pub const STOPPING_ORPHAN: &str = "marsad: Stopping an old php-fpm left by a crashed marsad.";

/// How often a wait checks again.
const POLL: Duration = Duration::from_millis(50);

/// Pid 1 (`launchd` on macOS), which adopts a process whose parent died.
/// Only the tests' fake OS uses it: the code asks [`ProcessProbe::adopters`].
#[cfg(test)]
const INIT: u32 = 1;

/// How long a probe of the socket may wait for a connection.
// Only the Unix socket probe uses it, and Unix sockets exist on Unix only.
#[cfg(unix)]
const CONNECT_LIMIT: Duration = Duration::from_millis(500);

/// Why a pool must not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    /// [`ORPHAN_STUCK`].
    pub code: &'static str,
    pub reason: String,
}

/// The php-fpm of each installed PHP version of `data_dir`. An installed
/// list that cannot be read gives none, so nothing counts as ours.
pub fn installed_programs(data_dir: &Path, platform: Platform) -> Vec<PathBuf> {
    let versions = RuntimesDir::in_dir(data_dir)
        .installed(Runtime::Php)
        .unwrap_or_else(|err| {
            warn!(code = err.code(), %err, "cannot list the PHP versions");
            Vec::new()
        });
    let programs = versions
        .iter()
        .filter_map(|version| fpm_program(data_dir, platform, version));
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

/// What [`Orphans::inspect`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// No pid file, or one that names nothing of ours to stop.
    Nothing,
    /// Our php-fpm master, alive, started at the recorded time.
    Orphan(PidRecord),
    /// The master the file names is gone, and the pool's socket or port is
    /// still in use: its workers may live on, since a master that died from
    /// SIGKILL cannot end them. The record says which executable they run
    /// and the time they cannot have started before.
    Workers(PidRecord),
}

/// What one [`Orphans::reap`] has done so far.
struct Reap<'h> {
    /// Called once, right before the first signal.
    before_signal: &'h (dyn Fn() + Sync),
    /// A signal went out, to the master or to a worker.
    signalled: bool,
    /// Something could not be told: a list that could not be read, a holder
    /// of the listener under pid 1 whose identity could not be read, or a
    /// verified worker whose identity could not be read later. Never a
    /// reason for a signal; a reason to keep the pool waiting, and the pid
    /// file, while the socket or port is in use.
    unsure: bool,
}

/// The check before one pool starts.
pub struct Orphans<'a> {
    pub pid_file: &'a PidFile,
    pub processes: &'a dyn ProcessProbe,
    /// Every php-fpm of this data folder that may still run
    /// ([`installed_programs`]). Nothing else is ever signalled.
    pub accepted: &'a [PathBuf],
    /// Where the pool listens.
    pub listen: &'a Dial,
    /// `term` and `kill` for the master. `kill` is also how long the socket
    /// or port may take to be let go at the end (`Orphans::wait_free`).
    /// Caddy's `stop` step does not apply: php-fpm has no admin API.
    pub ladder: Ladder,
    /// `term` and `kill` for the workers the master left.
    pub worker_ladder: Ladder,
}

impl Orphans<'_> {
    /// Reads the pid file and checks the process it names. A file that
    /// names a process that is gone, or not our php-fpm, is deleted, unless
    /// the socket or port is still in use: then the master's workers may
    /// live on, and the answer is [`Found::Workers`]. A process that cannot
    /// be described gets no signal: while the socket or port is in use, the
    /// start is blocked and the file stays for the next try; once it is
    /// free, the file is deleted.
    ///
    /// # Errors
    ///
    /// [`ORPHAN_STUCK`] for a pid that cannot be told while the socket or
    /// port is in use.
    pub async fn inspect(&self) -> Result<Found, Blocked> {
        let Some(record) = process::read_pid_file(&self.pid_file.path) else {
            return Ok(Found::Nothing);
        };
        let pid = record.pid;
        let state = self.check(&record).await;
        match &state {
            PidfileState::Ours { .. } => {
                info!(pid, exe = %record.exe.display(), "an old php-fpm of ours still runs");
                return Ok(Found::Orphan(record));
            }
            PidfileState::Stale { .. } => {
                debug!(pid, "the old php-fpm master has exited");
            }
            PidfileState::Mismatch { found, .. } => {
                warn!(
                    pid,
                    %found,
                    "the php-fpm pid file names a process that is not our php-fpm; no signal"
                );
            }
            PidfileState::Unknown { .. } | PidfileState::Missing => {
                warn!(
                    pid,
                    "cannot tell who runs under the pid in the php-fpm pid file; no signal"
                );
                self.without_signal(&record).await?;
                return Ok(Found::Nothing);
            }
        }
        // The master is gone. Its workers may live on and hold the socket.
        if !listen_free(self.listen).await {
            info!(
                pid,
                listen = %describe(self.listen),
                "the old php-fpm master is gone, but its socket or port is in use: its workers \
                 may live on"
            );
            return Ok(Found::Workers(record));
        }
        debug!(pid, "deleting the php-fpm pid file");
        self.pid_file.remove();
        Ok(Found::Nothing)
    }

    /// Stops what [`Orphans::inspect`] found, then looks at the socket or
    /// port. `Ok` once the pool may start. `before_signal` is called once,
    /// right before the first signal goes out, and not at all when none
    /// does.
    pub async fn reap(
        &self,
        found: Found,
        before_signal: &(dyn Fn() + Sync),
    ) -> Result<(), Blocked> {
        let mut reap = Reap {
            before_signal,
            signalled: false,
            unsure: false,
        };
        match found {
            Found::Nothing => Ok(()),
            Found::Orphan(record) => self.reap_master(&record, &mut reap).await,
            Found::Workers(record) => self.after_master(&record, &[], false, &mut reap).await,
        }
    }

    /// Stops the master `record` names: SIGTERM, then SIGKILL, each only
    /// right after the pid is checked again. Its workers end with it on
    /// SIGTERM; SIGKILL cannot end them, so right before it they are listed,
    /// and stopped once the master is gone ([`Orphans::after_master`]). A
    /// master whose children cannot be listed gets no SIGKILL.
    async fn reap_master(&self, record: &PidRecord, reap: &mut Reap<'_>) -> Result<(), Blocked> {
        let pid = record.pid;
        for (signal, wait) in [
            (Signal::Term, self.ladder.term),
            (Signal::Kill, self.ladder.kill),
        ] {
            // A master that dies from SIGKILL cannot end its workers, and
            // once it is gone nothing names them: find them first. The
            // master itself is checked after that, right before its signal.
            let workers = match signal {
                Signal::Kill => self.workers_under(record, pid, None, reap).await,
                Signal::Term | Signal::Int | Signal::Quit => Some(Vec::new()),
            };
            let state = self.check(record).await;
            match state {
                PidfileState::Ours { .. } => {}
                PidfileState::Unknown { .. } | PidfileState::Missing => {
                    warn!(pid, ?signal, %state, "no signal: the pid is not verified as our php-fpm");
                    return self.without_signal(record).await;
                }
                PidfileState::Stale { .. } => {
                    info!(pid, ?signal, %state, "the old php-fpm master is gone before the signal");
                    // Gone, and its pid not taken again: its children as
                    // listed a moment ago are the first to stop. It got no
                    // SIGKILL, so it may have ended them.
                    reap.unsure |= workers.is_none();
                    let workers = workers.unwrap_or_default();
                    return self.after_master(record, &workers, false, reap).await;
                }
                PidfileState::Mismatch { .. } => {
                    info!(pid, ?signal, %state, "the old php-fpm master's pid runs another process");
                    // The pid changed hands, maybe before its children were
                    // listed: they may be another process's. Only the search
                    // under pid 1, which also needs the listener, may name
                    // the master's workers now, whatever the probe says.
                    return self.after_master(record, &[], true, reap).await;
                }
            }
            let Some(workers) = workers else {
                warn!(
                    pid,
                    "no SIGKILL: the children of the old php-fpm cannot be listed, and its \
                     workers would live on with no record of whose they are"
                );
                return Err(Blocked {
                    code: ORPHAN_STUCK,
                    reason: format!(
                        "the children of our old php-fpm (pid {pid}) cannot be listed, so it gets \
                         no SIGKILL: its workers would live on with no record of whose they are"
                    ),
                });
            };
            warn!(pid, ?signal, "the old php-fpm still runs; signalling it");
            self.send(reap, pid, signal).await;
            if self.wait_gone(record, wait).await {
                info!(pid, ?signal, "the old php-fpm master stopped");
                // After a SIGKILL nothing told its workers to stop.
                let search = signal == Signal::Kill;
                return self.after_master(record, &workers, search, reap).await;
            }
        }
        Err(Blocked {
            code: ORPHAN_STUCK,
            reason: format!(
                "our old php-fpm (pid {pid}) still runs, or cannot be told, after SIGKILL"
            ),
        })
    }

    /// The master is gone: it exited, or its pid changed hands. Stops the
    /// workers it could not end: first `remembered`, its children verified
    /// right before its SIGKILL, then the ones pid 1 adopted
    /// ([`Orphans::adopted_workers`]). With `search` (the master got SIGKILL
    /// from us, or its pid changed hands) pid 1 is searched whatever the
    /// probe of the socket or port says: a full backlog reads as free on
    /// macOS. Otherwise it is searched when the socket or port is still in
    /// use.
    ///
    /// The end: once nothing holds the socket or port, the file is deleted
    /// and the pool may start. Each look waits for that up to `ladder.kill`
    /// after a signal, or while something cannot be told
    /// ([`Orphans::wait_free`]). A socket or port that stays in use never
    /// ends a run before the search under pid 1 has run. While it is in use
    /// after the search, the start is blocked and the file stays for the
    /// next try, if a signal went out or something could not be told. If
    /// the search worked, found nothing of ours, and no signal went out, the
    /// file is deleted and the pool starts: php-fpm itself then says what
    /// holds its socket.
    async fn after_master(
        &self,
        record: &PidRecord,
        remembered: &[PidRecord],
        search: bool,
        reap: &mut Reap<'_>,
    ) -> Result<(), Blocked> {
        let pid = record.pid;
        self.stop_workers(remembered, reap).await;
        if !search && self.wait_free(self.settle(reap)).await {
            self.pid_file.remove();
            return Ok(());
        }
        // In use, or a search is due whatever the probe says.
        match self.adopted_workers(record, reap).await {
            Some(adopted) => {
                // Not through the two signals twice: the pids that were
                // remembered had their turn above, whatever it did.
                let handled = |pid: u32| remembered.iter().any(|worker| worker.pid == pid);
                let adopted: Vec<PidRecord> = adopted
                    .into_iter()
                    .filter(|worker| !handled(worker.pid))
                    .collect();
                self.stop_workers(&adopted, reap).await;
            }
            None => reap.unsure = true,
        }
        if self.wait_free(self.settle(reap)).await {
            self.pid_file.remove();
            return Ok(());
        }
        let listen = describe(self.listen);
        if reap.unsure {
            return Err(Blocked {
                code: ORPHAN_STUCK,
                reason: format!(
                    "our old php-fpm (pid {pid}) has exited, {listen} is still in use, and what \
                     holds it cannot be verified as a worker of that php-fpm, so it gets no \
                     signal"
                ),
            });
        }
        if reap.signalled {
            return Err(Blocked {
                code: ORPHAN_STUCK,
                reason: format!(
                    "our old php-fpm (pid {pid}) has exited, and {listen} is still in use"
                ),
            });
        }
        info!(
            pid,
            %listen,
            "nothing of ours holds the socket or port; deleting the php-fpm pid file"
        );
        self.pid_file.remove();
        Ok(())
    }

    /// How long a look at the socket or port at the end of a run may wait
    /// for it to be let go: `ladder.kill` when the answer can block the
    /// start (a signal went out, or something cannot be told), else none.
    fn settle(&self, reap: &Reap<'_>) -> Duration {
        if reap.signalled || reap.unsure {
            self.ladder.kill
        } else {
            Duration::ZERO
        }
    }

    /// The workers of the master `record` names that pid 1 adopted: the
    /// children of pid 1 (each of [`ProcessProbe::adopters`]) that hold a
    /// listener at this pool's socket path or port and pass
    /// [`orphan::worker_of`]. The listener is a filter, not a proof: the
    /// list shows who holds a listener at that path or port, not
    /// who inherited it from our master. It is what a php-fpm of this data
    /// folder that the user started by hand fails, as long as it listens
    /// elsewhere (ADR 0009, R5e); the same executable and a later start do
    /// not tell it from a worker. `None` when the holders or the children
    /// cannot be listed: nothing is proven then, and nothing is signalled.
    async fn adopted_workers(
        &self,
        record: &PidRecord,
        reap: &mut Reap<'_>,
    ) -> Option<Vec<PidRecord>> {
        let Some(holders) = self.processes.listener_holders(self.listen).await else {
            warn!(
                listen = %describe(self.listen),
                "cannot list who holds the socket or port; the old php-fpm's workers, if any, \
                 get no signal"
            );
            return None;
        };
        let mut workers: Vec<PidRecord> = Vec::new();
        for adopter in self.processes.adopters().await {
            let found = self
                .workers_under(record, adopter, Some(&holders), reap)
                .await?;
            for worker in found {
                if !workers.iter().any(|known| known.pid == worker.pid) {
                    workers.push(worker);
                }
            }
        }
        workers.sort_by_key(|worker| worker.pid);
        Some(workers)
    }

    /// The children of `ppid` that are workers of the master `record` names
    /// ([`orphan::worker_of`]: alive, the master's very executable, started
    /// no earlier than it), by pid, each named by a record of its own for
    /// the checks before its signals. The master's own pid is never one: it
    /// is judged by the pid file alone. With `holders`, only the children
    /// among them, and one of those whose identity cannot be read now makes
    /// the reap unsure: it may be a worker. `None` when the children cannot
    /// be listed: nothing is signalled. Any other child whose identity
    /// cannot be read now is no worker: it was never verified, and most such
    /// are bystanders.
    async fn workers_under(
        &self,
        record: &PidRecord,
        ppid: u32,
        holders: Option<&[u32]>,
        reap: &mut Reap<'_>,
    ) -> Option<Vec<PidRecord>> {
        let Some(children) = self.processes.children(ppid).await else {
            warn!(
                ppid,
                "cannot list the children of the process; the old php-fpm's workers, if any, \
                 get no signal"
            );
            return None;
        };
        let mut workers = Vec::new();
        for child in children {
            if child.pid == record.pid {
                continue;
            }
            let holds = holders.is_some_and(|holders| holders.contains(&child.pid));
            if holders.is_some() && !holds {
                continue;
            }
            // By the listed start time first, which is cheap. Times that
            // cannot be compared say nothing.
            let order = started_order(child.started, record.started);
            let now = match order {
                Some(Ordering::Less) => continue,
                Some(_) => self.processes.real_identity(child.pid).await,
                None => Liveness::Unknown,
            };
            // The listed process exited, and its pid changed hands since.
            if let Liveness::Alive { started, .. } = &now
                && *started != child.started
            {
                debug!(
                    pid = child.pid,
                    ppid, "the pid changed hands since it was listed; no signal"
                );
                continue;
            }
            match orphan::worker_of(record, self.accepted, child.pid, &now) {
                Some(worker) => {
                    info!(
                        pid = worker.pid,
                        ppid, "a worker of the old php-fpm still runs"
                    );
                    workers.push(worker);
                }
                None if holds && now == Liveness::Unknown => {
                    warn!(
                        pid = child.pid,
                        ppid, "a process on the pool's socket or port cannot be told; no signal"
                    );
                    reap.unsure = true;
                }
                None => {
                    debug!(
                        pid = child.pid,
                        ppid,
                        ?now,
                        "not a worker of the old php-fpm; no signal"
                    );
                }
            }
        }
        workers.sort_by_key(|worker| worker.pid);
        Some(workers)
    }

    /// Stops `workers`: SIGTERM to each, then SIGKILL after a limit to the
    /// ones still there, each only right after the pid is checked again
    /// (same executable, same start time). A pid that is not verified at
    /// that moment gets no signal, then or later; if it cannot be told, the
    /// reap is unsure from then on.
    async fn stop_workers(&self, workers: &[PidRecord], reap: &mut Reap<'_>) {
        let mut pending: Vec<&PidRecord> = workers.iter().collect();
        for (signal, wait) in [
            (Signal::Term, self.worker_ladder.term),
            (Signal::Kill, self.worker_ladder.kill),
        ] {
            if pending.is_empty() {
                break;
            }
            let mut sent = Vec::new();
            for worker in pending {
                let pid = worker.pid;
                let state = self.check(worker).await;
                match state {
                    PidfileState::Ours { .. } => {}
                    PidfileState::Unknown { .. } | PidfileState::Missing => {
                        // It was verified a moment ago, so it may still be
                        // the worker.
                        warn!(
                            pid,
                            ?signal,
                            "no signal: the old php-fpm's worker cannot be told"
                        );
                        reap.unsure = true;
                        continue;
                    }
                    PidfileState::Stale { .. } | PidfileState::Mismatch { .. } => {
                        debug!(pid, ?signal, %state, "no signal: the old php-fpm's worker is gone");
                        continue;
                    }
                }
                warn!(
                    pid,
                    ?signal,
                    "a worker of the old php-fpm still runs; signalling it"
                );
                self.send(reap, pid, signal).await;
                sent.push(worker);
            }
            pending = self.wait_workers_gone(sent, wait).await;
        }
        for worker in pending {
            warn!(
                pid = worker.pid,
                "a worker of the old php-fpm did not stop, or cannot be told"
            );
        }
    }

    /// Sends `signal` to `pid`, which the caller verified right before.
    async fn send(&self, reap: &mut Reap<'_>, pid: u32, signal: Signal) {
        if !reap.signalled {
            (reap.before_signal)();
        }
        reap.signalled = true;
        if let Err(err) = self.processes.signal(pid, signal).await {
            warn!(pid, ?signal, %err, "the signal failed");
        }
    }

    /// The pid cannot be told (a tool failed): no signal. Once nothing holds
    /// the socket or port, the file is deleted and the pool may start. While
    /// something does, the start is blocked and the file stays for the next
    /// try.
    async fn without_signal(&self, record: &PidRecord) -> Result<(), Blocked> {
        let pid = record.pid;
        if listen_free(self.listen).await {
            let exe = record.exe.display();
            warn!(pid, %exe, "the old pid may still be our php-fpm; forgetting it");
            self.pid_file.remove();
            return Ok(());
        }
        Err(Blocked {
            code: ORPHAN_STUCK,
            reason: format!(
                "pid {pid} of the php-fpm pid file cannot be verified as our php-fpm, so it gets \
                 no signal, and {} is still in use",
                describe(self.listen)
            ),
        })
    }

    /// What the OS says about `record` now: the master's, or a worker's.
    async fn check(&self, record: &PidRecord) -> PidfileState {
        let now = self.processes.real_identity(record.pid).await;
        orphan::classify_pidfile(record, self.accepted, &now)
    }

    /// Polls until `record` is not our php-fpm any more (it exited, or its
    /// pid changed hands), or `limit` passed. The socket or port says
    /// nothing here: a master that closed it may still be shutting down, and
    /// must not get a signal it does not need; and its workers may hold it
    /// after its death, which [`Orphans::after_master`] handles. `Unknown`
    /// may be a tool that failed once: wait on.
    async fn wait_gone(&self, record: &PidRecord, limit: Duration) -> bool {
        let started = Instant::now();
        loop {
            match self.check(record).await {
                PidfileState::Ours { .. } | PidfileState::Unknown { .. } => {}
                _ => return true,
            }
            if started.elapsed() >= limit {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Polls until nothing listens at the socket or port, or `limit`
    /// passed; with no limit, looks once. A process that reads as gone may
    /// still hold it for a moment: on Linux its `exe` link is gone, and its
    /// pid may show a zombie, before its last thread has closed its files.
    /// Never a reason for a signal: it only decides whether the pool may
    /// start, or the search under pid 1 must run first.
    async fn wait_free(&self, limit: Duration) -> bool {
        let started = Instant::now();
        loop {
            if listen_free(self.listen).await {
                return true;
            }
            if started.elapsed() >= limit {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Polls until each of `workers` is not the verified worker any more (it
    /// exited, or its pid changed hands), or `limit` passed: the ones that
    /// still are, or cannot be told.
    async fn wait_workers_gone<'w>(
        &self,
        mut workers: Vec<&'w PidRecord>,
        limit: Duration,
    ) -> Vec<&'w PidRecord> {
        let started = Instant::now();
        loop {
            let mut left = Vec::with_capacity(workers.len());
            for worker in workers {
                match self.check(worker).await {
                    // `Unknown` may be a tool that failed once: wait on.
                    PidfileState::Ours { .. } | PidfileState::Unknown { .. } => left.push(worker),
                    _ => {}
                }
            }
            workers = left;
            if workers.is_empty() || started.elapsed() >= limit {
                return workers;
            }
            tokio::time::sleep(POLL).await;
        }
    }
}

/// `the socket <path>` or `port 127.0.0.1:<port>`, for messages.
fn describe(listen: &Dial) -> String {
    match listen {
        Dial::Unix(path) => format!("the socket {}", path.display()),
        Dial::Tcp(port) => format!("port 127.0.0.1:{port}"),
    }
}

/// Whether nothing listens at `listen` now.
pub async fn listen_free(listen: &Dial) -> bool {
    match listen {
        // The listener closes at once.
        Dial::Tcp(port) => TcpListener::bind((Ipv4Addr::LOCALHOST, *port)).is_ok(),
        Dial::Unix(path) => socket_free(path).await,
    }
}

/// Free when a connection is refused or there is no socket file. A
/// connection, or one that does not complete in time, is busy. On Linux a
/// frozen listener with a full backlog makes the connection wait, so it is
/// busy; on macOS a full backlog refuses the connection, so it reads as
/// free. So this probe is never the proof that our php-fpm is gone: a master
/// is waited for by its pid ([`Orphans::wait_gone`]), and after a SIGKILL of
/// the master, or once its pid changed hands, its workers are searched for
/// whatever the probe says ([`Orphans::after_master`]). The probe alone
/// decides only whether that search runs otherwise, and whether the pid
/// file is deleted at the end.
// Unix sockets exist on Unix only, and only Unix runs php-fpm pools.
#[cfg(unix)]
async fn socket_free(path: &Path) -> bool {
    let connect = tokio::net::UnixStream::connect(path);
    match tokio::time::timeout(CONNECT_LIMIT, connect).await {
        Ok(Err(err)) => matches!(
            err.kind(),
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
        ),
        Ok(Ok(_)) | Err(_) => false,
    }
}

/// No Unix sockets, and no php-fpm pools: nothing listens.
#[cfg(not(unix))]
async fn socket_free(_path: &Path) -> bool {
    true
}

/// Puts a watcher between the daemon and the supervisor's task of one pool
/// run: it writes the pid file after each spawn of `program` and deletes it
/// after each exit. Its task replaces the supervisor's in the handle, so a
/// stop waits for it. `lines` must be subscribed before the spawn, so the
/// first `running` is not missed.
pub fn watch(
    handle: ServiceHandle,
    lines: broadcast::Receiver<Event>,
    pid_file: PidFile,
    program: PathBuf,
    processes: Arc<dyn ProcessProbe>,
) -> ServiceHandle {
    let ServiceHandle {
        cancel,
        task,
        runtime,
        stop_limit,
    } = handle;
    let watcher = Watcher {
        lines,
        runtime: Arc::clone(&runtime),
        file: Recording {
            pid_file,
            program,
            processes,
            spawn: None,
        },
    };
    let task = tokio::spawn(watcher.run(task));
    ServiceHandle {
        cancel,
        task,
        runtime,
        stop_limit,
    }
}

struct Watcher {
    lines: broadcast::Receiver<Event>,
    runtime: Arc<Mutex<ServiceRuntime>>,
    file: Recording,
}

impl Watcher {
    async fn run(self, mut supervisor: JoinHandle<()>) {
        // `stop_procs` aborts this task after its time limit; the abort must
        // reach the supervisor too, so the `Child` is dropped and killed.
        let _abort = AbortOnDrop(supervisor.abort_handle());
        let Watcher {
            mut lines,
            runtime,
            mut file,
        } = self;
        let id = lock(&runtime).status.id;
        loop {
            let status = tokio::select! {
                result = &mut supervisor => {
                    report(id, result);
                    break;
                }
                event = lines.recv() => match event {
                    Ok(Event::ServiceChanged(status)) if status.id == id => status,
                    Ok(_) => continue,
                    // Too far behind: the status is current.
                    Err(RecvError::Lagged(_)) => lock(&runtime).status.clone(),
                    // The daemon is gone: wait for the process alone.
                    Err(RecvError::Closed) => {
                        report(id, (&mut supervisor).await);
                        break;
                    }
                },
                () = file.due() => {
                    file.look().await;
                    continue;
                }
            };
            match (&status.state, status.pid) {
                // The supervisor says `running` right after each spawn.
                (ServiceState::Running, Some(pid)) => file.spawned(pid).await,
                // php-fpm is about to exit: no file for it any more.
                (ServiceState::Stopping, _) => file.stopping(),
                // php-fpm has exited, so its pid may be anyone's from now on.
                (ServiceState::Failed { .. } | ServiceState::Stopped, _) => file.exited(),
                _ => {}
            }
        }
        file.exited();
    }
}

/// The pid file of one pool run, as its watcher keeps it.
struct Recording {
    pid_file: PidFile,
    program: PathBuf,
    processes: Arc<dyn ProcessProbe>,
    /// The last spawn, until it exits.
    spawn: Option<Spawn>,
}

/// One spawn of php-fpm, and how far its pid file got. A `running` with its
/// pid again (after `RecvError::Lagged`, say) is the same spawn: it starts
/// no new look.
struct Spawn {
    pid: u32,
    /// Its start time, once a look read it: a later look that shows another
    /// one means that the pid changed hands.
    started: Option<StartTime>,
    file: FileState,
}

enum FileState {
    /// Not written yet: the next look is due at `next`. No look is planned
    /// once `until` has passed, so the last one may start up to [`POLL`]
    /// after it.
    Looking {
        next: tokio::time::Instant,
        until: tokio::time::Instant,
    },
    /// The file names it.
    Written,
    /// No file, and no more looks.
    Never,
}

impl Recording {
    /// After a spawn of `pid`: writes the file, or looks again soon.
    async fn spawned(&mut self, pid: u32) {
        if self.spawn.as_ref().is_some_and(|spawn| spawn.pid == pid) {
            return;
        }
        // The run before it has exited, even if its end was missed.
        self.exited();
        let now = tokio::time::Instant::now();
        self.spawn = Some(Spawn {
            pid,
            started: None,
            file: FileState::Looking {
                next: now,
                until: now + RECORD_LIMIT,
            },
        });
        self.look().await;
    }

    /// Ready when the next look is due; never while none is planned.
    async fn due(&self) {
        match &self.spawn {
            Some(Spawn {
                file: FileState::Looking { next, .. },
                ..
            }) => tokio::time::sleep_until(*next).await,
            _ => std::future::pending().await,
        }
    }

    /// One look at the spawn ([`PidFile::record`]). Another one is due after
    /// [`POLL`] while it is [`Recorded::NotYet`], until [`RECORD_LIMIT`]
    /// after the spawn.
    async fn look(&mut self) {
        let Some(spawn) = &mut self.spawn else {
            return;
        };
        let FileState::Looking { until, .. } = spawn.file else {
            return;
        };
        let (pid, then) = (spawn.pid, spawn.started);
        let found = self
            .pid_file
            .record(&*self.processes, pid, &self.program, then);
        spawn.file = match found.await {
            Recorded::Written(record) => {
                spawn.started = Some(record.started);
                FileState::Written
            }
            Recorded::Never => FileState::Never,
            Recorded::NotYet(now) => {
                if let Liveness::Alive { started, .. } = &now {
                    spawn.started.get_or_insert(*started);
                }
                let at = tokio::time::Instant::now();
                if at < until {
                    let next = at + POLL;
                    FileState::Looking { next, until }
                } else {
                    warn!(
                        pid,
                        ?now,
                        "php-fpm pid file not written: the pid is not verified as our php-fpm"
                    );
                    FileState::Never
                }
            }
        };
    }

    /// A stop is under way: php-fpm is about to exit, so no more looks.
    fn stopping(&mut self) {
        if let Some(spawn) = &mut self.spawn
            && matches!(spawn.file, FileState::Looking { .. })
        {
            spawn.file = FileState::Never;
        }
    }

    /// The php-fpm has exited, or a new one replaced it: its pid may be
    /// anyone's from now on. Deletes only a file this watcher wrote.
    fn exited(&mut self) {
        if let Some(Spawn {
            file: FileState::Written,
            ..
        }) = self.spawn.take()
        {
            self.pid_file.remove();
        }
    }
}

fn report(id: ServiceId, result: Result<(), tokio::task::JoinError>) {
    if let Err(err) = result {
        warn!(service = %id, %err, "the php-fpm supervisor task failed");
    }
}

#[cfg(test)]
mod tests;
