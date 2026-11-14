//! Finds and stops a php-fpm that a crashed `marsad` left running. Task
//! M4-23, with the rules of ADR 0009 that `caddy::orphan` follows for Caddy.
//!
//! After a SIGKILL of the daemon, php-fpm lives on and keeps its socket, so
//! the next php-fpm of the same PHP line fails ("Another FPM instance seems
//! to already listen") and the line's sites answer 502. So:
//!
//! - after each spawn of a pool, its [`watch`]er writes
//!   `run/php-fpm-<minor>.pid` (pid, real executable, start time), and
//!   deletes it after each exit;
//! - before a pool starts, [`Orphans`] reads that file. A pid counts as ours
//!   only if it is alive, really runs the php-fpm of an installed PHP version
//!   of this data folder (an older version counts: an update may have come in
//!   between), and started at the recorded time. It gets SIGTERM, then
//!   SIGKILL after a limit, each only after the same check passes again.
//!   The pool starts once the process is gone and its socket or port is
//!   free.
//! - php-fpm's workers end with a master that stops on SIGTERM. A master
//!   that only dies from SIGKILL cannot end them, and macOS has no
//!   parent-death signal, so they live on under pid 1 and keep the socket
//!   (task M4-25). So, right before the master's SIGKILL, its direct
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
//!   a listener at this pool's socket path or port (`lsof`). That is a
//!   filter, not a proof: `lsof` shows who holds a listener there, not who
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
//!
//! For Linux (spike S-06b; none of this runs there today): an orphan goes to
//! the nearest subreaper, for example `systemd --user`, not always to pid 1,
//! and the holders of a listener are not listed there yet
//! ([`process::listener_holders`]).
//!
//! php-fpm writes its own `argv[0]` (`php-fpm: master process (...)`), so
//! `ps` shows no path for it. Identity is the file it really runs instead
//! ([`process::real_identity`]: `lsof` on macOS, `/proc` on Linux).
//!
//! All of this runs only where `process::ORPHAN_RECOVERY` is on (macOS now).
//! Elsewhere no pid file is written, nothing is signalled, and a file left
//! from before is deleted.

use std::cmp::Ordering;
use std::fmt;
use std::io;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use marsa_core::caddy::Dial;
use marsa_core::orphan::{self, ChildProcess, Liveness, PidRecord, PidfileState};
use marsa_core::php::PhpMinor;
use marsa_core::platform::process::{self, Signal};
use marsa_core::recipes::Platform;
use marsa_core::runtimes::{Runtime, RuntimesDir};
use marsa_core::types::{Event, ServiceState};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::task::{AbortHandle, JoinHandle};
use tracing::{debug, info, warn};

use super::{fpm_id, fpm_program};
use crate::daemon::caddy::orphan::{BoxFuture, Ladder};
use crate::daemon::supervisor::{ServiceHandle, ServiceRuntime, lock};

/// Our old php-fpm did not stop, or cannot be verified, while its socket or
/// port stays in use.
pub const ORPHAN_STUCK: &str = "php_fpm_orphan_stuck";

/// The line before a stop, in the pool's recent lines.
pub const STOPPING_ORPHAN: &str = "marsad: Stopping an old php-fpm left by a crashed marsad.";

/// How often a wait checks again.
const POLL: Duration = Duration::from_millis(50);

/// Pid 1 (`launchd` on macOS), which adopts a process whose parent died.
const INIT: u32 = 1;

/// How long a probe of the socket may wait for a connection.
// Only the Unix socket probe uses it, and Unix sockets exist on Unix only.
#[cfg(unix)]
const CONNECT_LIMIT: Duration = Duration::from_millis(500);

/// Who runs under a pid, and signals. [`SystemFpm`] asks the OS; a test
/// records the signals instead.
pub trait FpmProcesses: fmt::Debug + Send + Sync {
    /// The file the process really runs, and its start time.
    fn identity(&self, pid: u32) -> BoxFuture<'_, Liveness>;
    /// The children of `ppid` now, each with its start time: a list, never
    /// a proof. `None` when they cannot be listed: then none is signalled.
    fn children(&self, ppid: u32) -> BoxFuture<'_, Option<Vec<ChildProcess>>>;
    /// The processes that hold the listener at `listen` open now, an
    /// inherited one too: a list, never a proof alone. `None` when they
    /// cannot be listed: then no child of pid 1 is signalled.
    fn listener_holders<'a>(&'a self, listen: &'a Dial) -> BoxFuture<'a, Option<Vec<u32>>>;
    /// Sends `signal` to `pid`. The caller checked the pid right before.
    fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>>;
}

/// The real OS: `marsa_core::platform::process`.
#[derive(Debug)]
pub struct SystemFpm;

impl FpmProcesses for SystemFpm {
    fn identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
        Box::pin(process::real_identity(pid))
    }

    fn children(&self, ppid: u32) -> BoxFuture<'_, Option<Vec<ChildProcess>>> {
        Box::pin(process::children(ppid))
    }

    fn listener_holders<'a>(&'a self, listen: &'a Dial) -> BoxFuture<'a, Option<Vec<u32>>> {
        Box::pin(process::listener_holders(listen))
    }

    fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(process::send_signal(pid, signal))
    }
}

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

/// `run/php-fpm-<minor>.pid`.
#[derive(Debug, Clone)]
pub struct PidFile {
    pub path: PathBuf,
}

impl PidFile {
    pub fn new(data_dir: &Path, minor: PhpMinor) -> Self {
        let name = format!("{}.pid", fpm_id(minor));
        Self {
            path: data_dir.join("run").join(name),
        }
    }

    /// Writes the file for a php-fpm just spawned, only if `pid` really runs
    /// `program`. `true` when the file was written.
    pub async fn record(&self, processes: &dyn FpmProcesses, pid: u32, program: &Path) -> bool {
        let spellings = accepted([program.to_path_buf()]);
        match processes.identity(pid).await {
            Liveness::Alive { exe, started } if spellings.contains(&exe) => {
                let record = PidRecord { pid, exe, started };
                match process::write_pid_file(&self.path, &record) {
                    Ok(()) => {
                        debug!(pid, path = %self.path.display(), "php-fpm pid file written");
                        true
                    }
                    Err(err) => {
                        warn!(%err, path = %self.path.display(), "cannot write the php-fpm pid file");
                        false
                    }
                }
            }
            // It exited at once; the supervisor reports why.
            Liveness::Gone => {
                debug!(pid, "php-fpm exited before its pid file was written");
                false
            }
            other => {
                warn!(
                    pid,
                    ?other,
                    "php-fpm pid file not written: the pid is not our php-fpm"
                );
                false
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
    pub processes: &'a dyn FpmProcesses,
    /// Every php-fpm of this data folder that may still run
    /// ([`installed_programs`]). Nothing else is ever signalled.
    pub accepted: &'a [PathBuf],
    /// Where the pool listens.
    pub listen: &'a Dial,
    /// `term` and `kill` for the master. `kill` is also how long the socket
    /// or port may take to be let go at the end ([`Orphans::wait_free`]).
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
                Signal::Term => Some(Vec::new()),
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
    /// children of pid 1 that hold a listener at this pool's socket path or
    /// port and pass [`orphan::worker_of`]. The listener is a filter, not a
    /// proof: `lsof` shows who holds a listener at that path or port, not
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
        self.workers_under(record, INIT, Some(&holders), reap).await
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
            let order = orphan::started_order(&child.started, &record.started);
            let now = match order {
                Some(Ordering::Less) => continue,
                Some(_) => self.processes.identity(child.pid).await,
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
        let now = self.processes.identity(record.pid).await;
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
            io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
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
    processes: Arc<dyn FpmProcesses>,
) -> ServiceHandle {
    let ServiceHandle {
        cancel,
        task,
        runtime,
    } = handle;
    let watcher = Watcher {
        lines,
        runtime: Arc::clone(&runtime),
        pid_file,
        program,
        processes,
    };
    let task = tokio::spawn(watcher.run(task));
    ServiceHandle {
        cancel,
        task,
        runtime,
    }
}

struct Watcher {
    lines: broadcast::Receiver<Event>,
    runtime: Arc<Mutex<ServiceRuntime>>,
    pid_file: PidFile,
    program: PathBuf,
    processes: Arc<dyn FpmProcesses>,
}

impl Watcher {
    async fn run(self, mut supervisor: JoinHandle<()>) {
        // `stop_procs` aborts this task after its time limit; the abort must
        // reach the supervisor too, so the `Child` is dropped and killed.
        let _abort = AbortOnDrop(supervisor.abort_handle());
        let Watcher {
            mut lines,
            runtime,
            pid_file,
            program,
            processes,
        } = self;
        let id = lock(&runtime).status.id.clone();
        let mut recorded = None;
        loop {
            let status = tokio::select! {
                result = &mut supervisor => {
                    report(&id, result);
                    break;
                }
                event = lines.recv() => match event {
                    Ok(Event::ServiceChanged(status)) if status.id == id => status,
                    Ok(_) => continue,
                    // Too far behind: the status is current.
                    Err(RecvError::Lagged(_)) => lock(&runtime).status.clone(),
                    // The daemon is gone: wait for the process alone.
                    Err(RecvError::Closed) => {
                        report(&id, (&mut supervisor).await);
                        break;
                    }
                },
            };
            match (&status.state, status.pid) {
                // The supervisor says `running` right after each spawn.
                (ServiceState::Running, Some(pid)) if recorded != Some(pid) => {
                    let written = pid_file.record(&*processes, pid, &program).await;
                    recorded = written.then_some(pid);
                }
                // php-fpm has exited, so its pid may be anyone's from now on.
                (ServiceState::Failed { .. } | ServiceState::Stopped, _) if recorded.is_some() => {
                    pid_file.remove();
                    recorded = None;
                }
                _ => {}
            }
        }
        // Only a file this watcher wrote, for a php-fpm that has exited.
        if recorded.is_some() {
            pid_file.remove();
        }
    }
}

fn report(id: &str, result: Result<(), tokio::task::JoinError>) {
    if let Err(err) = result {
        warn!(service = %id, %err, "the php-fpm supervisor task failed");
    }
}

struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    use semver::Version;

    use marsa_core::php::PhpFiles;

    use super::super::{PhpManager, Pool, fpm_spec};
    use crate::daemon::state::Daemon;

    const PROGRAM: &str = "/data/runtimes/php/8.4.3/bin/php-fpm";
    const STARTED: &str = "Wed Nov 4 03:31:27 2026";
    /// A second before and after `STARTED`.
    const EARLIER: &str = "Wed Nov 4 03:31:26 2026";
    const LATER: &str = "Wed Nov 4 03:31:28 2026";
    /// The pid the pid file names.
    const MASTER: u32 = 4242;
    const PHP84: PhpMinor = PhpMinor { major: 8, minor: 4 };

    /// Short waits for the fakes, which change state at once.
    const SHORT: Ladder = Ladder {
        stop: Duration::ZERO,
        term: Duration::from_millis(200),
        kill: Duration::from_millis(500),
    };

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    /// One process the fake OS knows.
    #[derive(Debug, Clone, Default)]
    struct Proc {
        ppid: u32,
        /// What the OS says: the front of the queue, and the last one again
        /// once one is left.
        now: VecDeque<Liveness>,
        /// The start time the process table lists for it: the first `Alive`
        /// of `now`, unless set.
        listed: String,
        /// What the OS says after a signal, as the death of the process
        /// would. `None`: the signal changes nothing.
        after_term: Option<Liveness>,
        after_kill: Option<Liveness>,
        /// Forked after its parent's children were listed: the process
        /// table shows it only once its parent is gone, under pid 1.
        late: bool,
    }

    /// A process that says `now`, with no parent of note.
    fn proc(now: impl IntoIterator<Item = Liveness>) -> Proc {
        let now: VecDeque<Liveness> = now.into_iter().collect();
        let listed = now.iter().find_map(|state| match state {
            Liveness::Alive { started, .. } => Some(started.clone()),
            _ => None,
        });
        Proc {
            now,
            listed: listed.unwrap_or_default(),
            ..Proc::default()
        }
    }

    /// A child of the master that runs `exe`, started at `started`.
    fn child(exe: &str, started: &str) -> Proc {
        Proc {
            ppid: MASTER,
            ..proc([alive(exe, started)])
        }
    }

    /// The master M4-25 is about: it ignores SIGTERM and dies from SIGKILL.
    fn stubborn_master() -> Proc {
        Proc {
            after_kill: Some(Liveness::Gone),
            ..proc([ours()])
        }
    }

    /// A worker of the master: its program, started at `started`, ends on
    /// SIGTERM.
    fn worker(started: &str) -> Proc {
        Proc {
            after_term: Some(Liveness::Gone),
            ..child(PROGRAM, started)
        }
    }

    /// The fake OS. A signal is recorded, never sent, and may change what the
    /// OS says next, as the death of the process would: its listener closes,
    /// and pid 1 adopts its children.
    #[derive(Debug, Default)]
    struct Fake {
        procs: Mutex<BTreeMap<u32, Proc>>,
        /// The parents whose children cannot be listed (`ps` fails).
        unlisted: Vec<u32>,
        /// `lsof` fails: the holders of the listener cannot be listed.
        no_holders: bool,
        /// How often the holders of the listener were asked for: once per
        /// search under pid 1.
        searches: Mutex<usize>,
        /// The pids that hold the pool's listener, each until a signal ends
        /// it.
        holders: Mutex<BTreeSet<u32>>,
        /// The port they hold, let go with the last of them.
        port: Mutex<Option<TcpListener>>,
        signals: Mutex<Vec<(u32, Signal)>>,
        /// How often the hook before the first signal was called.
        announced: Mutex<usize>,
    }

    impl Fake {
        /// The master, pid 4242, says `now`.
        fn new(now: impl IntoIterator<Item = Liveness>) -> Self {
            Self::of(proc(now))
        }

        /// `master` as pid 4242. The daemon that started it is gone, so pid
        /// 1 adopted it, as on macOS: the search under pid 1 lists it too.
        fn of(master: Proc) -> Self {
            Self::default().with(
                MASTER,
                Proc {
                    ppid: INIT,
                    ..master
                },
            )
        }

        fn with(self, pid: u32, proc: Proc) -> Self {
            self.procs.lock().unwrap().insert(pid, proc);
            self
        }

        /// `pids` hold the pool's listener: `listener`'s port stays in use
        /// until a signal has ended each of them.
        fn holding(self, pids: impl IntoIterator<Item = u32>, listener: TcpListener) -> Self {
            *self.port.lock().unwrap() = Some(listener);
            self.holding_unseen(pids)
        }

        /// `pids` hold the pool's listener, and the probe reads it as free,
        /// as it reads a full backlog on macOS.
        fn holding_unseen(self, pids: impl IntoIterator<Item = u32>) -> Self {
            self.holders.lock().unwrap().extend(pids);
            self
        }

        fn signals(&self) -> Vec<(u32, Signal)> {
            self.signals.lock().unwrap().clone()
        }

        fn announced(&self) -> usize {
            *self.announced.lock().unwrap()
        }

        fn searches(&self) -> usize {
            *self.searches.lock().unwrap()
        }
    }

    impl FpmProcesses for Fake {
        fn identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
            let mut procs = self.procs.lock().unwrap();
            let next = match procs.get_mut(&pid) {
                Some(proc) => {
                    let next = if proc.now.len() > 1 {
                        proc.now.pop_front()
                    } else {
                        proc.now.front().cloned()
                    };
                    next.unwrap_or(Liveness::Unknown)
                }
                None => Liveness::Gone,
            };
            Box::pin(async move { next })
        }

        fn children(&self, ppid: u32) -> BoxFuture<'_, Option<Vec<ChildProcess>>> {
            let children = (!self.unlisted.contains(&ppid)).then(|| {
                let procs = self.procs.lock().unwrap();
                procs
                    .iter()
                    .filter(|(_, proc)| proc.ppid == ppid && !proc.late)
                    .map(|(pid, proc)| ChildProcess {
                        pid: *pid,
                        started: proc.listed.clone(),
                    })
                    .collect()
            });
            Box::pin(async move { children })
        }

        fn listener_holders<'a>(&'a self, _listen: &'a Dial) -> BoxFuture<'a, Option<Vec<u32>>> {
            *self.searches.lock().unwrap() += 1;
            let holders =
                (!self.no_holders).then(|| self.holders.lock().unwrap().iter().copied().collect());
            Box::pin(async move { holders })
        }

        fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>> {
            self.signals.lock().unwrap().push((pid, signal));
            let mut procs = self.procs.lock().unwrap();
            let after = procs.get(&pid).and_then(|proc| match signal {
                Signal::Term => proc.after_term.clone(),
                Signal::Kill => proc.after_kill.clone(),
            });
            if let Some(state) = after {
                if state == Liveness::Gone {
                    // Its listener closes, and pid 1 adopts its children.
                    let mut holders = self.holders.lock().unwrap();
                    holders.remove(&pid);
                    if holders.is_empty() {
                        *self.port.lock().unwrap() = None;
                    }
                    for orphan in procs.values_mut().filter(|proc| proc.ppid == pid) {
                        orphan.ppid = 1;
                        orphan.late = false;
                    }
                }
                if let Some(proc) = procs.get_mut(&pid) {
                    proc.now = VecDeque::from([state]);
                }
            }
            Box::pin(async { Ok(()) })
        }
    }

    fn alive(exe: &str, started: &str) -> Liveness {
        Liveness::Alive {
            exe: PathBuf::from(exe),
            started: started.into(),
        }
    }

    fn ours() -> Liveness {
        alive(PROGRAM, STARTED)
    }

    fn free_port() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    /// A pid file that names pid 4242, our php-fpm.
    fn pid_file_of_ours() -> (tempfile::TempDir, PidFile) {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = PidFile::new(dir.path(), PHP84);
        let record = PidRecord {
            pid: 4242,
            exe: PathBuf::from(PROGRAM),
            started: STARTED.into(),
        };
        process::write_pid_file(&pid_file.path, &record).unwrap();
        (dir, pid_file)
    }

    /// The check before a start, as `PhpManager::clear_orphan` runs it.
    async fn run(pid_file: &PidFile, fake: &Fake, listen: &Dial) -> Result<(), Blocked> {
        run_with(pid_file, fake, listen, SHORT).await
    }

    /// [`run`] with other waits. `ladder.term` of zero makes each wait
    /// after a SIGTERM ask the OS exactly once.
    async fn run_with(
        pid_file: &PidFile,
        fake: &Fake,
        listen: &Dial,
        ladder: Ladder,
    ) -> Result<(), Blocked> {
        let accepted = [PathBuf::from(PROGRAM)];
        let orphans = Orphans {
            pid_file,
            processes: fake,
            accepted: &accepted,
            listen,
            ladder,
            worker_ladder: ladder,
        };
        let found = orphans.inspect().await?;
        let announce = || *fake.announced.lock().unwrap() += 1;
        orphans.reap(found, &announce).await
    }

    /// A port something holds: `Dial::Tcp` of it, and the listener to hand
    /// to the process that holds it.
    fn held_port() -> (TcpListener, Dial) {
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let listen = Dial::Tcp(held.local_addr().unwrap().port());
        (held, listen)
    }

    /// The 8.4 pool of `data` on `listen`, run as `program args`. Without
    /// php-fpm's config text, so it builds from a Windows path too.
    fn bare_pool(data: &Path, listen: Dial, program: &str, args: &[&str]) -> Pool {
        let files = PhpFiles::new(data, &v("8.4.3"));
        let mut spec = fpm_spec(PathBuf::from(program), &files);
        spec.args = args.iter().map(|arg| (*arg).to_owned()).collect();
        Pool {
            files,
            fpm_conf: String::new(),
            php_ini: String::new(),
            spec,
            dial: listen,
        }
    }

    /// A manager that checks for orphans with `processes`, on any system.
    fn manager_with(data: &Path, processes: Arc<dyn FpmProcesses>) -> PhpManager {
        let mut php = PhpManager::new(data, Some(Platform::LinuxX64));
        php.recovery = true;
        php.processes = processes;
        php.ladder = SHORT;
        php.worker_ladder = SHORT;
        php
    }

    #[tokio::test]
    async fn a_stubborn_orphan_gets_sigterm_then_sigkill_and_its_file_goes() {
        let (_dir, pid_file) = pid_file_of_ours();
        let fake = Fake::of(stubborn_master());
        let listen = Dial::Tcp(free_port());
        assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
        let want = [(4242, Signal::Term), (4242, Signal::Kill)];
        assert_eq!(fake.signals(), want);
        assert!(!pid_file.path.exists());
    }

    #[tokio::test]
    async fn an_orphan_that_stops_on_sigterm_gets_no_sigkill() {
        let (_dir, pid_file) = pid_file_of_ours();
        let fake = Fake::of(Proc {
            after_term: Some(Liveness::Gone),
            ..proc([ours()])
        });
        let listen = Dial::Tcp(free_port());
        assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
        assert_eq!(fake.signals(), [(4242, Signal::Term)]);
        assert!(!pid_file.path.exists());
    }

    #[tokio::test]
    async fn a_socket_let_go_a_moment_after_the_pid_is_gone_does_not_block_the_pool() {
        // On Linux a process reads as gone (no `exe` link, or a zombie)
        // before its last thread has closed its files, so its socket or port
        // stays in use for a moment after SIGTERM ended it. The pool waits
        // for that instead of failing.
        let (_dir, pid_file) = pid_file_of_ours();
        let (held, listen) = held_port();
        let fake = Fake::of(Proc {
            after_term: Some(Liveness::Gone),
            ..proc([ours()])
        });
        let closing = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(held);
        });
        // A long last step, so a slow machine cannot fail the test; the wait
        // ends as soon as the port is free.
        let ladder = Ladder {
            kill: Duration::from_secs(10),
            ..SHORT
        };
        assert_eq!(run_with(&pid_file, &fake, &listen, ladder).await, Ok(()));
        assert_eq!(fake.signals(), [(MASTER, Signal::Term)]);
        assert!(!pid_file.path.exists());
        closing.join().unwrap();
    }

    #[tokio::test]
    async fn the_pid_is_checked_again_before_each_signal() {
        // After SIGTERM the pid runs another program, and the port stays
        // busy: that program gets no SIGKILL.
        let (_dir, pid_file) = pid_file_of_ours();
        let (_held, listen) = held_port();
        let fake = Fake::of(Proc {
            after_term: Some(alive("/bin/sleep", STARTED)),
            ..proc([ours()])
        });
        let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
        assert_eq!(blocked.code, ORPHAN_STUCK);
        assert!(blocked.reason.contains("is still in use"), "{blocked:?}");
        assert_eq!(fake.signals(), [(4242, Signal::Term)], "no SIGKILL");
    }

    #[tokio::test]
    async fn the_workers_of_a_master_killed_by_sigkill_are_stopped_in_order() {
        // Two workers, one of them on the port. The second ignores SIGTERM
        // too. A child that runs another program is not a worker.
        let (_dir, pid_file) = pid_file_of_ours();
        let (held, listen) = held_port();
        let fake = Fake::of(stubborn_master())
            .with(4301, worker(STARTED))
            .with(
                4302,
                Proc {
                    after_kill: Some(Liveness::Gone),
                    ..child(PROGRAM, LATER)
                },
            )
            .with(4303, child("/bin/sleep", LATER))
            .holding([MASTER, 4301, 4302], held);
        assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
        let want = [
            (MASTER, Signal::Term),
            (MASTER, Signal::Kill),
            (4301, Signal::Term),
            (4302, Signal::Term),
            (4302, Signal::Kill),
        ];
        assert_eq!(fake.signals(), want);
        assert_eq!(fake.announced(), 1, "once, before the first signal");
        assert!(!pid_file.path.exists());
    }

    #[tokio::test]
    async fn a_child_that_is_not_the_masters_worker_gets_no_signal() {
        let cases = [
            ("another program", child("/bin/sleep", STARTED)),
            (
                "a php-fpm of another data folder",
                child("/other/runtimes/php/8.4.3/bin/php-fpm", STARTED),
            ),
            ("older than the master", child(PROGRAM, EARLIER)),
            (
                "an identity that cannot be read",
                Proc {
                    listed: STARTED.into(),
                    ppid: MASTER,
                    ..proc([Liveness::Unknown])
                },
            ),
        ];
        for (what, decoy) in cases {
            let (_dir, pid_file) = pid_file_of_ours();
            // The decoy holds the port, so a signal would have been the only
            // way on; the master's SIGKILL alone leaves it in use. Pid 1
            // adopts the decoy, and the search there turns it down again.
            let (held, listen) = held_port();
            let fake = Fake::of(stubborn_master())
                .with(4300, decoy)
                .holding([MASTER, 4300], held);
            let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
            assert_eq!(blocked.code, ORPHAN_STUCK, "{what}");
            assert!(blocked.reason.contains("has exited"), "{what}: {blocked:?}");
            let want = [(MASTER, Signal::Term), (MASTER, Signal::Kill)];
            assert_eq!(fake.signals(), want, "{what}");
            assert!(pid_file.path.exists(), "{what}: kept for the next try");
        }
    }

    #[tokio::test]
    async fn a_master_whose_children_cannot_be_listed_gets_no_sigkill() {
        // Killed, it would leave its workers with no record of whose they
        // are. It keeps running, and the file stays for the next try.
        let (_dir, pid_file) = pid_file_of_ours();
        let (held, listen) = held_port();
        let fake = Fake {
            unlisted: vec![MASTER],
            ..Fake::of(stubborn_master())
        }
        .with(4301, worker(STARTED))
        .holding([MASTER, 4301], held);
        let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
        assert_eq!(blocked.code, ORPHAN_STUCK);
        assert!(blocked.reason.contains("cannot be listed"), "{blocked:?}");
        assert_eq!(fake.signals(), [(MASTER, Signal::Term)], "no SIGKILL");
        assert!(pid_file.path.exists(), "kept for the next try");
    }

    #[tokio::test]
    async fn a_master_gone_at_the_sigkill_step_has_its_listed_workers_stopped() {
        // The master ignores SIGTERM, then exits by itself right before its
        // SIGKILL, without ending its worker. With no wait, the OS is asked
        // about it exactly four times: at the inspection, before SIGTERM,
        // once after it, and before SIGKILL.
        let (_dir, pid_file) = pid_file_of_ours();
        let (held, listen) = held_port();
        let master = proc([ours(), ours(), ours(), Liveness::Gone]);
        let fake = Fake::of(master)
            .with(4301, worker(STARTED))
            .holding([4301], held);
        let ladder = Ladder {
            term: Duration::ZERO,
            ..SHORT
        };
        assert_eq!(run_with(&pid_file, &fake, &listen, ladder).await, Ok(()));
        // Listed as its child a moment before, and stopped by that list:
        // here it never shows under pid 1.
        assert_eq!(
            fake.signals(),
            [(MASTER, Signal::Term), (4301, Signal::Term)]
        );
        assert!(!pid_file.path.exists());
    }

    #[tokio::test]
    async fn a_master_gone_at_the_sigkill_step_whose_children_were_not_listed_keeps_the_file() {
        // As above, but its children could not be listed a moment before.
        // Its worker still holds the port and does not show under pid 1
        // yet, so the search there, which works, finds nothing. What holds
        // the port cannot be verified: no signal to it, and the file stays.
        let (_dir, pid_file) = pid_file_of_ours();
        let (held, listen) = held_port();
        let master = proc([ours(), ours(), ours(), Liveness::Gone]);
        let fake = Fake {
            unlisted: vec![MASTER],
            ..Fake::of(master)
        }
        .with(4301, worker(STARTED))
        .holding([4301], held);
        let ladder = Ladder {
            term: Duration::ZERO,
            ..SHORT
        };
        let blocked = run_with(&pid_file, &fake, &listen, ladder)
            .await
            .unwrap_err();
        assert_eq!(blocked.code, ORPHAN_STUCK);
        assert!(blocked.reason.contains("cannot be verified"), "{blocked:?}");
        assert_eq!(fake.signals(), [(MASTER, Signal::Term)]);
        assert!(pid_file.path.exists(), "kept for the next try");
    }

    #[tokio::test]
    async fn a_master_whose_pid_changed_hands_at_the_sigkill_step_has_pid_1_searched() {
        // The master ignores SIGTERM; right before its SIGKILL its pid runs
        // another program. Its children were listed a moment before, but
        // they may be that program's: 4302 runs our php-fpm and started
        // after the master, and gets no signal. The search under pid 1 runs
        // even though the port reads free, and finds 4301, the master's
        // worker, on it.
        let (_dir, pid_file) = pid_file_of_ours();
        let listen = Dial::Tcp(free_port());
        let master = proc([ours(), ours(), ours(), alive("/bin/sleep", LATER)]);
        let adopted = Proc {
            ppid: 1,
            ..worker(LATER)
        };
        let fake = Fake::of(master)
            .with(4301, adopted)
            .with(4302, worker(LATER))
            .holding_unseen([4301]);
        let ladder = Ladder {
            term: Duration::ZERO,
            ..SHORT
        };
        assert_eq!(run_with(&pid_file, &fake, &listen, ladder).await, Ok(()));
        assert_eq!(
            fake.signals(),
            [(MASTER, Signal::Term), (4301, Signal::Term)]
        );
        assert!(!pid_file.path.exists());
    }

    #[tokio::test]
    async fn a_worker_forked_after_the_list_is_found_under_pid_1_once_the_master_is_killed() {
        // 4301 is listed before the SIGKILL. 4302 is forked after that. Pid 1
        // adopts both, and the search there finds the one the list missed.
        let (_dir, pid_file) = pid_file_of_ours();
        let (held, listen) = held_port();
        let late = Proc {
            late: true,
            ..worker(LATER)
        };
        let fake = Fake::of(stubborn_master())
            .with(4301, worker(STARTED))
            .with(4302, late)
            .holding([MASTER, 4301, 4302], held);
        assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
        let want = [
            (MASTER, Signal::Term),
            (MASTER, Signal::Kill),
            (4301, Signal::Term),
            (4302, Signal::Term),
        ];
        assert_eq!(fake.signals(), want);
        assert!(!pid_file.path.exists());
    }

    #[tokio::test]
    async fn after_a_sigkill_pid_1_is_searched_even_if_the_port_reads_free() {
        // A full backlog reads as free on macOS. The worker was forked after
        // the master's children were listed, so only the search finds it.
        let (_dir, pid_file) = pid_file_of_ours();
        let listen = Dial::Tcp(free_port());
        let late = Proc {
            late: true,
            ..worker(LATER)
        };
        let fake = Fake::of(stubborn_master())
            .with(4302, late)
            .holding_unseen([4302]);
        assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
        let want = [
            (MASTER, Signal::Term),
            (MASTER, Signal::Kill),
            (4302, Signal::Term),
        ];
        assert_eq!(fake.signals(), want);
        assert!(!pid_file.path.exists());
    }

    #[tokio::test]
    async fn a_remembered_worker_does_not_get_its_signals_twice() {
        // The worker survives both signals. Pid 1 adopts it, and it still
        // holds the port, but the search there leaves it out: it had its
        // turn.
        let (_dir, pid_file) = pid_file_of_ours();
        let (held, listen) = held_port();
        let fake = Fake::of(stubborn_master())
            .with(4301, child(PROGRAM, STARTED))
            .holding([MASTER, 4301], held);
        let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
        assert_eq!(blocked.code, ORPHAN_STUCK);
        let want = [
            (MASTER, Signal::Term),
            (MASTER, Signal::Kill),
            (4301, Signal::Term),
            (4301, Signal::Kill),
        ];
        assert_eq!(fake.signals(), want);
        assert!(pid_file.path.exists(), "kept for the next try");
    }

    #[tokio::test]
    async fn a_worker_whose_identity_changes_before_sigkill_gets_none() {
        // The worker ignores SIGTERM; right before SIGKILL its pid runs
        // another program. With no wait, the OS is asked exactly four times:
        // listed, before SIGTERM, once after it, and before SIGKILL.
        let (_dir, pid_file) = pid_file_of_ours();
        let listen = Dial::Tcp(free_port());
        let same = alive(PROGRAM, STARTED);
        let changed = Proc {
            ppid: MASTER,
            ..proc([same.clone(), same.clone(), same, alive("/bin/sleep", LATER)])
        };
        let fake = Fake::of(stubborn_master()).with(4300, changed);
        let ladder = Ladder {
            term: Duration::ZERO,
            ..SHORT
        };
        assert_eq!(run_with(&pid_file, &fake, &listen, ladder).await, Ok(()));
        let want = [
            (MASTER, Signal::Term),
            (MASTER, Signal::Kill),
            (4300, Signal::Term),
        ];
        assert_eq!(fake.signals(), want, "no SIGKILL to the worker");
        assert!(!pid_file.path.exists());
    }

    #[tokio::test]
    async fn workers_under_pid_1_are_stopped_when_the_master_is_already_gone() {
        // The file names a master that is gone; two of its workers live on
        // under pid 1, on the port.
        let (_dir, pid_file) = pid_file_of_ours();
        let (held, listen) = held_port();
        let adopted = |proc: Proc| Proc { ppid: 1, ..proc };
        let fake = Fake::new([Liveness::Gone])
            .with(4301, adopted(worker(STARTED)))
            .with(4302, adopted(worker(LATER)))
            .holding([4301, 4302], held);
        assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
        assert_eq!(fake.signals(), [(4301, Signal::Term), (4302, Signal::Term)]);
        assert_eq!(fake.announced(), 1, "once, before the first signal");
        assert!(!pid_file.path.exists());
    }

    #[tokio::test]
    async fn a_child_of_pid_1_that_does_not_hold_the_listener_gets_no_signal() {
        // ADR 0009, R5e: our very php-fpm, started after the master, under
        // pid 1, as one that the user started by hand would be. It is not on
        // this pool's port, which another process holds.
        let by_hand = || Proc {
            ppid: 1,
            ..worker(LATER)
        };
        let (_dir, pid_file) = pid_file_of_ours();
        let (held, listen) = held_port();
        let fake = Fake::new([Liveness::Gone])
            .with(4300, by_hand())
            .holding([4400], held);
        assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
        assert!(fake.signals().is_empty());
        assert_eq!(fake.announced(), 0);
        // The port is still in use: the file goes only after a search.
        assert_eq!(fake.searches(), 1);
        assert!(!pid_file.path.exists());

        // Nor after a SIGKILL of the master, when pid 1 is always searched.
        let (_dir, pid_file) = pid_file_of_ours();
        let listen = Dial::Tcp(free_port());
        let fake = Fake::of(stubborn_master()).with(4300, by_hand());
        assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
        let want = [(MASTER, Signal::Term), (MASTER, Signal::Kill)];
        assert_eq!(fake.signals(), want);
    }

    #[tokio::test]
    async fn a_search_under_pid_1_that_cannot_list_ends_stuck_with_the_file_kept() {
        // A worker under pid 1 holds the port, and a tool fails: `ps` for
        // the children, or `lsof` for the holders. Nothing is proven, so
        // nothing is signalled, and nothing is forgotten.
        for (what, unlisted, no_holders) in [("ps", vec![INIT], false), ("lsof", vec![], true)] {
            let (_dir, pid_file) = pid_file_of_ours();
            let (held, listen) = held_port();
            let adopted = Proc {
                ppid: 1,
                ..worker(LATER)
            };
            let fake = Fake {
                unlisted,
                no_holders,
                ..Fake::new([Liveness::Gone])
            }
            .with(4301, adopted)
            .holding([4301], held);
            let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
            assert_eq!(blocked.code, ORPHAN_STUCK, "{what}");
            let unsure = blocked.reason.contains("cannot be verified");
            assert!(unsure, "{what}: {blocked:?}");
            assert!(fake.signals().is_empty(), "{what}");
            assert_eq!(fake.announced(), 0, "{what}");
            assert!(pid_file.path.exists(), "{what}: kept for the next try");
        }
    }

    #[tokio::test]
    async fn a_worker_under_pid_1_that_cannot_be_told_before_its_signal_gets_none() {
        // Verified when it is listed; right before SIGTERM its identity
        // cannot be read. It may still be the worker: no signal, and the
        // file stays.
        let (_dir, pid_file) = pid_file_of_ours();
        let (held, listen) = held_port();
        let unsure = Proc {
            ppid: 1,
            ..proc([alive(PROGRAM, LATER), Liveness::Unknown])
        };
        let fake = Fake::new([Liveness::Gone])
            .with(4301, unsure)
            .holding([4301], held);
        let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
        assert_eq!(blocked.code, ORPHAN_STUCK);
        assert!(blocked.reason.contains("cannot be verified"), "{blocked:?}");
        assert!(fake.signals().is_empty());
        assert!(pid_file.path.exists(), "kept for the next try");
    }

    #[tokio::test]
    async fn a_child_of_pid_1_that_is_not_the_masters_worker_gets_no_signal() {
        // A second after `LATER`.
        const LATEST: &str = "Wed Nov 4 03:31:29 2026";
        // What the process table lists, and what the OS says right after.
        let cases = [
            (
                "a php-fpm of another data folder",
                LATER,
                alive("/other/runtimes/php/8.4.3/bin/php-fpm", LATER),
            ),
            ("another program", LATER, alive("/bin/sleep", LATER)),
            (
                "our php-fpm from before the master",
                EARLIER,
                alive(PROGRAM, EARLIER),
            ),
            // The listed process exited, and our php-fpm took its pid.
            (
                "a pid that changed hands after the list",
                LATER,
                alive(PROGRAM, LATEST),
            ),
        ];
        for (what, listed, now) in cases {
            let (_dir, pid_file) = pid_file_of_ours();
            let (held, listen) = held_port();
            let decoy = Proc {
                ppid: 1,
                listed: listed.into(),
                ..proc([now])
            };
            // It holds the port, so only the checks of its executable and
            // start time stand between it and a signal.
            let fake = Fake::new([Liveness::Gone])
                .with(4300, decoy)
                .holding([4300], held);
            // No signal at all, after a search that worked: the pool starts,
            // and php-fpm itself says what holds its port.
            assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()), "{what}");
            assert!(fake.signals().is_empty(), "{what}");
            assert_eq!(fake.announced(), 0, "{what}");
            assert_eq!(
                fake.searches(),
                1,
                "{what}: the file goes only after a search"
            );
            assert!(!pid_file.path.exists(), "{what}");
        }
    }

    #[tokio::test]
    async fn a_holder_under_pid_1_that_cannot_be_told_keeps_the_pool_waiting() {
        // Under pid 1, on the port, and its identity cannot be read: it may
        // be the worker. No signal, and the file stays.
        let unknown = || Proc {
            ppid: 1,
            listed: LATER.into(),
            ..proc([Liveness::Unknown])
        };
        let (_dir, pid_file) = pid_file_of_ours();
        let (held, listen) = held_port();
        let fake = Fake::new([Liveness::Gone])
            .with(4300, unknown())
            .holding([4300], held);
        let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
        assert_eq!(blocked.code, ORPHAN_STUCK);
        assert!(blocked.reason.contains("cannot be verified"), "{blocked:?}");
        assert!(fake.signals().is_empty());
        assert!(pid_file.path.exists(), "kept for the next try");

        // Not on the port: a bystander, which never blocks the pool.
        let (_dir, pid_file) = pid_file_of_ours();
        let (held, listen) = held_port();
        let fake = Fake::new([Liveness::Gone])
            .with(4300, unknown())
            .holding([4400], held);
        assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
        assert!(fake.signals().is_empty());
        assert!(!pid_file.path.exists());
    }

    #[tokio::test]
    async fn the_masters_own_pid_under_pid_1_is_never_taken_for_its_worker() {
        // The master's pid reads gone at the inspection (a `ps` that missed
        // it), then as our php-fpm again, under pid 1 and on the port, as a
        // live master is. The search there passes it over: the master is
        // judged by the pid file alone. Its worker gets SIGTERM; the master
        // still holds the port, so the pool waits.
        let (_dir, pid_file) = pid_file_of_ours();
        let (held, listen) = held_port();
        let adopted = Proc {
            ppid: 1,
            ..worker(LATER)
        };
        let fake = Fake::new([Liveness::Gone, ours()])
            .with(4301, adopted)
            .holding([MASTER, 4301], held);
        let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
        assert_eq!(blocked.code, ORPHAN_STUCK);
        assert_eq!(fake.signals(), [(4301, Signal::Term)], "none to the master");
        assert!(pid_file.path.exists(), "kept for the next try");
    }

    #[tokio::test]
    async fn the_stop_is_noted_in_the_pools_lines_only_when_a_signal_goes_out() {
        // Through `PhpManager::clear_orphan`, with an installed php-fpm that
        // the pid file names, so a worker of it counts as ours.
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        let program = fpm_program(data, Platform::LinuxX64, &v("8.4.3")).unwrap();
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, "not run").unwrap();
        let pid_file = PidFile::new(data, PHP84);
        let write_pid_file = || {
            let record = PidRecord {
                pid: MASTER,
                exe: program.clone(),
                started: STARTED.into(),
            };
            process::write_pid_file(&pid_file.path, &record).unwrap();
        };
        let noted = |daemon: &Daemon| {
            let lines = lock(&daemon.pools.runtime(PHP84)).logs.recent();
            lines.iter().filter(|l| l.text == STOPPING_ORPHAN).count()
        };

        // A stale file, and a port that something of another kind holds:
        // nothing is stopped, so nothing is noted.
        write_pid_file();
        let (held, listen) = held_port();
        let fake = Arc::new(Fake::new([Liveness::Gone]).holding([4400], held));
        let php = manager_with(data, fake.clone());
        let daemon = Daemon::new(data.to_path_buf());
        let pool = bare_pool(data, listen, PROGRAM, &[]);
        let result = php.clear_orphan(&daemon, &pool, &pid_file).await;
        assert_eq!(result, Ok(()));
        assert!(fake.signals().is_empty());
        assert_eq!(noted(&daemon), 0);

        // A worker of the master under pid 1, on the port: noted once.
        write_pid_file();
        let (held, listen) = held_port();
        let worker = Proc {
            ppid: 1,
            after_term: Some(Liveness::Gone),
            ..proc([Liveness::Alive {
                exe: program.clone(),
                started: LATER.into(),
            }])
        };
        let fake = Fake::new([Liveness::Gone])
            .with(4301, worker)
            .holding([4301], held);
        let fake = Arc::new(fake);
        let php = manager_with(data, fake.clone());
        let daemon = Daemon::new(data.to_path_buf());
        let pool = bare_pool(data, listen, PROGRAM, &[]);
        let result = php.clear_orphan(&daemon, &pool, &pid_file).await;
        assert_eq!(result, Ok(()));
        assert_eq!(fake.signals(), [(4301, Signal::Term)]);
        assert_eq!(noted(&daemon), 1);
    }

    #[tokio::test]
    async fn a_pid_file_that_names_no_orphan_of_ours_gets_no_signal() {
        let later = "Wed Nov 4 03:31:28 2026";
        let cases = [
            // Another program, our start time.
            alive("/bin/sleep", STARTED),
            // Our program, another start time: a reused pid (ADR 0009, R5e).
            alive(PROGRAM, later),
            // A php-fpm of another data folder.
            alive("/other/runtimes/php/8.4.3/bin/php-fpm", STARTED),
            Liveness::Gone,
        ];
        for now in cases {
            let (_dir, pid_file) = pid_file_of_ours();
            // Busy, so a signal would have been the only way on.
            let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let listen = Dial::Tcp(held.local_addr().unwrap().port());
            let fake = Fake::new([now.clone()]);
            assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()), "{now:?}");
            assert!(fake.signals().is_empty(), "{now:?}");
            assert!(!pid_file.path.exists(), "{now:?}: a stale file goes");
        }
    }

    #[tokio::test]
    async fn a_pid_that_cannot_be_told_blocks_while_busy_and_is_forgotten_once_free() {
        // Busy: blocked, no signal, and the file stays for the next try.
        let (_dir, pid_file) = pid_file_of_ours();
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let listen = Dial::Tcp(held.local_addr().unwrap().port());
        let fake = Fake::new([Liveness::Unknown]);
        let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
        assert_eq!(blocked.code, ORPHAN_STUCK);
        assert!(blocked.reason.contains("cannot be verified"), "{blocked:?}");
        assert!(fake.signals().is_empty());
        assert!(pid_file.path.exists());

        // Free: the pool may start, and the pid is forgotten.
        drop(held);
        assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
        assert!(fake.signals().is_empty());
        assert!(!pid_file.path.exists());
    }

    #[tokio::test]
    async fn a_blocked_start_pool_fails_with_a_note_and_keeps_the_file() {
        let (dir, pid_file) = pid_file_of_ours();
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let listen = Dial::Tcp(held.local_addr().unwrap().port());
        let fake = Arc::new(Fake::new([Liveness::Unknown]));
        let php = manager_with(dir.path(), fake.clone());
        let daemon = Daemon::new(dir.path().to_path_buf());
        // Never run: the start is blocked first.
        let pool = bare_pool(dir.path(), listen, "/never/run/php-fpm", &[]);

        assert!(!php.start_pool(&daemon, &pool).await);
        let runtime = daemon.pools.find(&fpm_id(PHP84)).expect("listed");
        let (status, lines) = {
            let runtime = lock(&runtime);
            (runtime.status.clone(), runtime.logs.recent())
        };
        let failed = ServiceState::Failed {
            code: ORPHAN_STUCK.into(),
        };
        assert_eq!((status.state, status.pid), (failed, None));
        let note = "marsad: php-fpm was not started: pid 4242 of the php-fpm pid file";
        assert!(lines.iter().any(|l| l.text.starts_with(note)), "{lines:?}");
        assert!(fake.signals().is_empty());
        assert!(pid_file.path.exists(), "kept for the next try");
        assert!(!daemon.proc_alive(&fpm_id(PHP84)));
    }

    // Runs `/bin/sleep` as the pool, which Windows does not have.
    #[cfg(unix)]
    #[tokio::test]
    async fn start_pool_forgets_a_pid_that_cannot_be_told_once_the_port_is_free() {
        let (dir, pid_file) = pid_file_of_ours();
        let fake = Arc::new(Fake::new([Liveness::Unknown]));
        let php = manager_with(dir.path(), fake.clone());
        let daemon = Daemon::new(dir.path().to_path_buf());
        let listen = Dial::Tcp(free_port());
        let pool = bare_pool(dir.path(), listen, "/bin/sleep", &["30"]);

        assert!(php.start_pool(&daemon, &pool).await);
        assert!(daemon.proc_alive(&fpm_id(PHP84)));
        assert!(!pid_file.path.exists(), "forgotten");
        assert!(fake.signals().is_empty());
        super::super::stop_pool(&daemon, PHP84).await;
        assert!(!daemon.proc_alive(&fpm_id(PHP84)));
    }

    #[tokio::test]
    async fn a_pid_that_cannot_be_verified_right_before_sigterm_gets_none() {
        // Busy: blocked, and the file stays.
        let (_dir, pid_file) = pid_file_of_ours();
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let listen = Dial::Tcp(held.local_addr().unwrap().port());
        let fake = Fake::new([ours(), Liveness::Unknown]);
        let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
        assert_eq!(blocked.code, ORPHAN_STUCK);
        assert!(fake.signals().is_empty());
        assert!(pid_file.path.exists());

        // Free: the pool may start, and the pid is forgotten.
        drop(held);
        let fake = Fake::new([ours(), Liveness::Unknown]);
        assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
        assert!(fake.signals().is_empty());
        assert!(!pid_file.path.exists());
    }

    #[tokio::test]
    async fn the_pid_file_is_written_only_for_our_program() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = PidFile::new(dir.path(), PHP84);
        assert!(
            pid_file
                .path
                .ends_with(Path::new("run").join(format!("{}.pid", fpm_id(PHP84))))
        );
        let program = Path::new(PROGRAM);
        for now in [
            alive("/bin/sleep", STARTED),
            Liveness::Gone,
            Liveness::Unknown,
        ] {
            assert!(!pid_file.record(&Fake::new([now]), 4242, program).await);
            assert!(!pid_file.path.exists());
        }
        assert!(pid_file.record(&Fake::new([ours()]), 4242, program).await);
        let record = process::read_pid_file(&pid_file.path).unwrap();
        let want = PidRecord {
            pid: 4242,
            exe: program.to_path_buf(),
            started: STARTED.into(),
        };
        assert_eq!(record, want);
    }

    #[tokio::test]
    async fn without_recovery_a_left_pid_file_goes_without_a_check() {
        let (dir, pid_file) = pid_file_of_ours();
        let fake = Arc::new(Fake::new([ours()]));
        let mut php = PhpManager::new(dir.path(), Some(Platform::LinuxX64));
        php.recovery = false;
        php.processes = fake.clone();
        let daemon = Daemon::new(dir.path().to_path_buf());
        let pool = bare_pool(dir.path(), Dial::Tcp(free_port()), PROGRAM, &[]);
        let result = php.clear_orphan(&daemon, &pool, &pid_file).await;
        assert_eq!(result, Ok(()));
        assert!(fake.signals().is_empty());
        assert!(!pid_file.path.exists());
    }

    #[test]
    fn every_installed_php_fpm_is_accepted_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let platform = Platform::DarwinArm64;
        let mut programs = Vec::new();
        for version in ["8.3.1", "8.4.2", "8.4.3"] {
            let program = fpm_program(dir.path(), platform, &v(version)).unwrap();
            std::fs::create_dir_all(program.parent().unwrap()).unwrap();
            std::fs::write(&program, "not run").unwrap();
            programs.push(program);
        }
        // Not a version: never a program.
        let other = dir.path().join("runtimes").join("php").join("8.4");
        std::fs::create_dir_all(other.join("bin")).unwrap();

        let found = installed_programs(dir.path(), platform);
        assert_eq!(found, accepted(programs.clone()));
        for program in &programs {
            assert!(found.contains(program), "{program:?}");
            let real = std::fs::canonicalize(program).unwrap();
            assert!(found.contains(&real), "{real:?}");
        }
        assert!(found.iter().all(|path| !path.starts_with(&other)));
        // Each once.
        let twice = accepted([programs[0].clone(), programs[0].clone()]);
        assert!(twice.len() <= 2, "{twice:?}");
        assert!(installed_programs(&dir.path().join("none"), platform).is_empty());
    }

    #[tokio::test]
    async fn a_port_or_socket_is_free_only_when_nothing_listens() {
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        assert!(!listen_free(&Dial::Tcp(port)).await);
        drop(held);
        assert!(listen_free(&Dial::Tcp(port)).await);

        // Unix sockets exist on Unix only.
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("php-8.4.sock");
            let socket = Dial::Unix(path.clone());
            assert!(listen_free(&socket).await, "no file");
            let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
            assert!(!listen_free(&socket).await);
            drop(listener);
            assert!(path.exists());
            assert!(listen_free(&socket).await, "a file nobody listens on");
        }
    }

    /// The real OS: a fake php-fpm is a copy of this test binary at the
    /// exact path of an installed php-fpm, so its real executable is ours.
    /// Only the systems where `process::real_identity` reads a process.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    mod real {
        use super::*;
        use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
        use std::process::{Child, Command, ExitStatus, Stdio};

        use super::super::super::{pool_for, stop_pool, write_pool};

        /// The fake's mode: `hold` listens on `$MARSA_FPM_SOCKET`, `idle`
        /// does nothing, `once` exits after a second the first time (until
        /// `$MARSA_FPM_MARKER` exists), then does nothing, and `master` runs
        /// a `hold` and an `idle` child, writes their pids to
        /// `$MARSA_FPM_CHILDREN`, and ignores SIGTERM. Each ends by itself
        /// after two minutes.
        const FAKE: &str = "MARSA_FPM_FAKE";
        const SOCKET: &str = "MARSA_FPM_SOCKET";
        /// For `once`: the file that says the first run is over.
        const MARKER: &str = "MARSA_FPM_MARKER";
        /// For `master`: the file with the pids of its children, one per line.
        const CHILDREN: &str = "MARSA_FPM_CHILDREN";
        /// Set for a fake on a leash (`Bystander::leashed`): it ends when its
        /// stdin does, in any mode.
        const LEASH: &str = "MARSA_FPM_LEASH";
        const FAKE_TEST: &str = "daemon::php::orphan::tests::real::fake_php_fpm";
        const FAKE_LIFE: Duration = Duration::from_secs(120);
        /// Generous: a test binary can start slowly on a busy CI machine.
        const WAIT: Duration = Duration::from_secs(30);

        /// Not a check: the fake php-fpm's entry point. In a normal test run
        /// the variable is unset and this returns at once.
        #[test]
        fn fake_php_fpm() {
            let Ok(mode) = std::env::var(FAKE) else {
                return;
            };
            if std::env::var_os(LEASH).is_some() {
                // Ends with its input: the test let go of the other end, or
                // is gone.
                std::thread::spawn(|| {
                    let _ = io::copy(&mut io::stdin(), &mut io::sink());
                    std::process::exit(0);
                });
            }
            if mode == "master" {
                return fake_master();
            }
            let _listener = (mode == "hold").then(|| {
                let path = std::env::var(SOCKET).expect("the socket path");
                std::os::unix::net::UnixListener::bind(path).expect("the socket")
            });
            if mode == "once" {
                let marker = PathBuf::from(std::env::var(MARKER).expect("the marker path"));
                if !marker.exists() {
                    std::fs::write(&marker, "").expect("the marker");
                    std::thread::sleep(Duration::from_secs(1));
                    std::process::exit(1);
                }
            }
            std::thread::sleep(FAKE_LIFE);
        }

        /// A php-fpm whose master only dies from SIGKILL: it catches SIGTERM,
        /// starts two children of this very program, one on the socket as a
        /// worker that serves, and sleeps. The children hold the socket after
        /// its death, as php-fpm's workers do on macOS.
        fn fake_master() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime");
            runtime.block_on(async {
                use tokio::signal::unix::{SignalKind, signal};
                // A handler, not SIG_IGN: the children start with the default
                // again, as php-fpm's workers do. Nothing reads it; the
                // process just lives on.
                let _term = signal(SignalKind::terminate()).expect("the SIGTERM handler");
                let me = std::env::current_exe().expect("this program");
                let socket = std::env::var(SOCKET).expect("the socket path");
                let pids_file = std::env::var(CHILDREN).expect("the children file");
                let mut children = Vec::new();
                for mode in ["hold", "idle"] {
                    let mut command = fake_command(&me, mode);
                    // This process's stdin, so the children are on its leash.
                    command.env(SOCKET, &socket).stdin(Stdio::inherit());
                    children.push(command.spawn().expect("a child"));
                }
                let pids: Vec<String> = children.iter().map(|c| c.id().to_string()).collect();
                std::fs::write(&pids_file, pids.join("\n") + "\n").expect("the children file");
                // Kept, not waited for: they outlive this process.
                tokio::time::sleep(FAKE_LIFE).await;
                drop(children);
            });
        }

        /// Records each signal. Only `send` passes it on to the OS.
        #[derive(Debug, Default)]
        struct Recorder {
            send: bool,
            signals: Mutex<Vec<(u32, Signal)>>,
        }

        impl Recorder {
            fn signals(&self) -> Vec<(u32, Signal)> {
                self.signals.lock().unwrap().clone()
            }
        }

        impl FpmProcesses for Recorder {
            fn identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
                Box::pin(process::real_identity(pid))
            }

            fn children(&self, ppid: u32) -> BoxFuture<'_, Option<Vec<ChildProcess>>> {
                Box::pin(process::children(ppid))
            }

            fn listener_holders<'a>(&'a self, listen: &'a Dial) -> BoxFuture<'a, Option<Vec<u32>>> {
                Box::pin(process::listener_holders(listen))
            }

            fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>> {
                self.signals.lock().unwrap().push((pid, signal));
                if self.send {
                    Box::pin(process::send_signal(pid, signal))
                } else {
                    Box::pin(async { Ok(()) })
                }
            }
        }

        /// A process this test started. Dropping it kills it, so its pid
        /// cannot be reused before the `wait`.
        struct Bystander(Child);

        impl Bystander {
            fn spawn(command: &mut Command) -> Self {
                Self(command.stdin(Stdio::null()).spawn().expect("a process"))
            }

            /// Like `spawn`, on a leash: its stdin is a pipe that its
            /// children inherit, and each of them ends when the other end
            /// closes. So none outlives the test, whatever fails in it: that
            /// end is dropped with the test, and closed if the test process
            /// dies. Nothing has to signal a child of the fake by pid.
            fn leashed(command: &mut Command) -> (Self, io::PipeWriter) {
                let (stdin, leash) = io::pipe().expect("a pipe");
                let child = command.env(LEASH, "1").stdin(stdin).spawn();
                (Self(child.expect("a process")), leash)
            }

            fn pid(&self) -> u32 {
                self.0.id()
            }

            fn alive(&mut self) -> bool {
                self.0.try_wait().unwrap().is_none()
            }

            fn wait(&mut self) -> ExitStatus {
                self.0.wait().unwrap()
            }
        }

        impl Drop for Bystander {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        fn platform() -> Platform {
            Platform::current().expect("Marsa has builds for this system")
        }

        /// `version`'s php-fpm under `data`: a copy of this test binary.
        fn install_fake(data: &Path, version: &str) -> PathBuf {
            let program = placeholder(data, version);
            std::fs::copy(std::env::current_exe().unwrap(), &program).unwrap();
            program
        }

        /// `version`'s php-fpm under `data`: a file that never runs.
        fn placeholder(data: &Path, version: &str) -> PathBuf {
            let program = fpm_program(data, platform(), &v(version)).unwrap();
            std::fs::create_dir_all(program.parent().unwrap()).unwrap();
            std::fs::write(&program, "not run").unwrap();
            program
        }

        fn fake_command(program: &Path, mode: &str) -> Command {
            let mut command = Command::new(program);
            command
                .args([FAKE_TEST, "--exact", "--nocapture"])
                .env(FAKE, mode)
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            command
        }

        /// The 8.4 pool of `version`, with its files, run as the fake.
        fn fake_pool(data: &Path, version: &str, mode: &str) -> Pool {
            let mut pool = pool_for(data, platform(), &v(version), None).unwrap();
            write_pool(&pool).unwrap();
            pool.spec.args = [FAKE_TEST, "--exact", "--nocapture"]
                .map(String::from)
                .to_vec();
            pool.spec.env = BTreeMap::from([(FAKE.to_owned(), mode.to_owned())]);
            pool
        }

        fn manager(data: &Path, processes: Arc<dyn FpmProcesses>) -> PhpManager {
            let mut php = PhpManager::new(data, Some(platform()));
            php.recovery = true;
            php.processes = processes;
            php.ladder = Ladder {
                stop: Duration::ZERO,
                term: WAIT,
                kill: Duration::from_secs(5),
            };
            php.worker_ladder = php.ladder;
            php
        }

        async fn wait_until(what: &str, mut done: impl AsyncFnMut() -> bool) {
            let started = Instant::now();
            while !done().await {
                assert!(started.elapsed() < WAIT, "{what}");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }

        /// Starts `pool`, or fails with what tells why it did not start: the
        /// pool's status, its recent lines (the reason of the orphan check
        /// is one of them), the signals sent, whether the socket is in use
        /// now, and what the OS says about each of `pids` now.
        async fn start(
            php: &PhpManager,
            daemon: &Daemon,
            pool: &Pool,
            recorder: &Recorder,
            pids: &[u32],
        ) {
            if php.start_pool(daemon, pool).await {
                return;
            }
            let (status, lines) = match daemon.pools.find("php-fpm-8.4") {
                Some(runtime) => {
                    let runtime = lock(&runtime);
                    let lines: Vec<String> =
                        runtime.logs.recent().into_iter().map(|l| l.text).collect();
                    (format!("{:?}", runtime.status), lines)
                }
                None => ("no status".to_owned(), Vec::new()),
            };
            let free = listen_free(&pool.dial).await;
            let mut now = Vec::new();
            for pid in pids {
                now.push((*pid, process::real_identity(*pid).await));
            }
            panic!(
                "the pool did not start\nstatus: {status}\nlines: {lines:#?}\nsignals: {:?}\n\
                 socket free now: {free}\nprocesses now: {now:?}",
                recorder.signals()
            );
        }

        /// Runs the check before a start of the 8.4 pool of `data`, with
        /// `record` in the pid file, signals recorded and none sent.
        async fn check_recording(data: &Path, record: &PidRecord) -> Vec<(u32, Signal)> {
            let pid_file = PidFile::new(data, PHP84);
            process::write_pid_file(&pid_file.path, record).unwrap();
            let recorder = Arc::new(Recorder::default());
            let php = manager(data, recorder.clone());
            let daemon = Daemon::new(data.to_path_buf());
            let pool = pool_for(data, platform(), &v("8.4.3"), None).unwrap();
            let result = php.clear_orphan(&daemon, &pool, &pid_file).await;
            assert_eq!(result, Ok(()));
            // Not ours, so the file was stale.
            assert!(!pid_file.path.exists());
            recorder.signals()
        }

        #[tokio::test]
        async fn the_watcher_deletes_the_file_at_an_exit_and_writes_it_after_the_restart() {
            let dir = tempfile::tempdir().unwrap();
            let data = dir.path();
            install_fake(data, "8.4.3");
            let recorder = Arc::new(Recorder::default());
            let php = manager(data, recorder.clone());
            let daemon = Daemon::new(data.to_path_buf());
            let mut pool = fake_pool(data, "8.4.3", "once");
            let marker = data.join("first-run-over");
            let marker = marker.to_string_lossy().into_owned();
            pool.spec.env.insert(MARKER.to_owned(), marker);
            let pid_file = PidFile::new(data, PHP84);
            let read = || process::read_pid_file(&pid_file.path);

            start(&php, &daemon, &pool, &recorder, &[]).await;
            let mut first = None;
            wait_until("the first pid file", async || {
                first = read();
                first.is_some()
            })
            .await;
            let first = first.expect("a record");
            // The first run exits by itself: its file goes.
            wait_until("no file after the exit", async || read().is_none()).await;
            // The supervisor's restart gets a file of its own.
            wait_until("the file of the restart", async || {
                read().is_some_and(|record| record.pid != first.pid)
            })
            .await;
            let second = read().expect("a record");
            let runtime = daemon.pools.find(&fpm_id(PHP84)).expect("listed");
            let status = lock(&runtime).status.clone();
            assert_eq!((status.pid, status.restarts), (Some(second.pid), 1));

            stop_pool(&daemon, PHP84).await;
            assert_eq!(read(), None);
            assert!(recorder.signals().is_empty());
        }

        #[tokio::test]
        async fn our_orphan_is_stopped_then_the_pool_starts_with_a_new_pid_file() {
            let dir = tempfile::tempdir().unwrap();
            let data = dir.path();
            // The orphan runs the older 8.4.2, as after an update.
            let old = install_fake(data, "8.4.2");
            let new = install_fake(data, "8.4.3");
            let recorder = Arc::new(Recorder {
                send: true,
                ..Recorder::default()
            });
            let php = manager(data, recorder.clone());
            let daemon = Daemon::new(data.to_path_buf());
            let pool = fake_pool(data, "8.4.3", "idle");
            let Dial::Unix(socket) = &pool.dial else {
                panic!("a short socket path: {:?}", pool.dial);
            };

            // As a crashed daemon leaves it: on the pool's socket, with a
            // pid file.
            let mut hold = fake_command(&old, "hold");
            let mut orphan = Bystander::spawn(hold.env(SOCKET, socket));
            wait_until("the orphan listens", async || {
                !listen_free(&pool.dial).await
            })
            .await;
            let pid_file = PidFile::new(data, PHP84);
            pid_file.record(&SystemFpm, orphan.pid(), &old).await;
            let record = process::read_pid_file(&pid_file.path).expect("the orphan's file");
            assert_eq!(record.pid, orphan.pid());

            start(&php, &daemon, &pool, &recorder, &[orphan.pid()]).await;
            // Only SIGTERM, only to the orphan, which it ended.
            assert_eq!(recorder.signals(), [(orphan.pid(), Signal::Term)]);
            assert_eq!(orphan.wait().signal(), Some(15));
            let runtime = daemon.pools.find(&fpm_id(PHP84)).expect("listed");
            let lines = lock(&runtime).logs.recent();
            assert!(lines.iter().any(|line| line.text == STOPPING_ORPHAN));

            // The new pool runs, and its pid file names it.
            let mut pid = None;
            wait_until("the pool runs", async || {
                let status = lock(&runtime).status.clone();
                pid = status.pid.filter(|_| status.state == ServiceState::Running);
                pid.is_some()
            })
            .await;
            let pid = pid.expect("a pid");
            wait_until("the new pid file", async || {
                process::read_pid_file(&pid_file.path).is_some_and(|r| r.pid == pid)
            })
            .await;
            let record = process::read_pid_file(&pid_file.path).unwrap();
            assert!(accepted([new]).contains(&record.exe), "{record:?}");

            // A clean stop deletes it.
            stop_pool(&daemon, PHP84).await;
            assert!(!pid_file.path.exists());
            assert_eq!(lock(&runtime).status.state, ServiceState::Stopped);
        }

        #[tokio::test]
        async fn a_pid_file_naming_a_live_sleep_gets_no_signal() {
            // R4: the pid now runs `sleep`. The file has its start time, so
            // only the path tells it apart.
            let dir = tempfile::tempdir().unwrap();
            let program = placeholder(dir.path(), "8.4.3");
            let mut sleeper = Bystander::spawn(Command::new("/bin/sleep").arg("120"));
            let Liveness::Alive { started, .. } = process::real_identity(sleeper.pid()).await
            else {
                panic!("no identity for the sleeper");
            };
            let record = PidRecord {
                pid: sleeper.pid(),
                exe: std::fs::canonicalize(&program).unwrap(),
                started,
            };
            assert!(check_recording(dir.path(), &record).await.is_empty());
            assert!(sleeper.alive());
        }

        #[tokio::test]
        async fn our_program_with_another_start_time_gets_no_signal() {
            // R5e: the pid runs our very php-fpm, but not the one we started.
            let dir = tempfile::tempdir().unwrap();
            let program = install_fake(dir.path(), "8.4.3");
            let mut idle = Bystander::spawn(&mut fake_command(&program, "idle"));
            let Liveness::Alive { exe, .. } = process::real_identity(idle.pid()).await else {
                panic!("no identity for the idle php-fpm");
            };
            assert!(accepted([program]).contains(&exe), "only the time differs");
            let record = PidRecord {
                pid: idle.pid(),
                exe,
                started: "Thu Jan 1 00:00:00 1970".into(),
            };
            assert!(check_recording(dir.path(), &record).await.is_empty());
            assert!(idle.alive());
        }

        #[tokio::test]
        async fn a_process_that_only_claims_our_path_gets_no_signal() {
            // `argv[0]` is chosen by whoever starts a process: `ps` shows our
            // path for this `sleep` on macOS. Its real executable is not ours.
            let dir = tempfile::tempdir().unwrap();
            let program = placeholder(dir.path(), "8.4.3");
            let mut sleeper =
                Bystander::spawn(Command::new("/bin/sleep").arg0(&program).arg("120"));
            // `ps` reads `argv[0]` on macOS only; Linux reads the `exe` link.
            #[cfg(target_os = "macos")]
            {
                let claimed = process::identity(sleeper.pid()).await;
                assert!(matches!(&claimed, Liveness::Alive { exe, .. } if *exe == program));
            }
            let Liveness::Alive { started, .. } = process::real_identity(sleeper.pid()).await
            else {
                panic!("no identity for the sleeper");
            };
            let record = PidRecord {
                pid: sleeper.pid(),
                exe: std::fs::canonicalize(&program).unwrap(),
                started,
            };
            assert!(check_recording(dir.path(), &record).await.is_empty());
            assert!(sleeper.alive());
        }

        #[tokio::test]
        async fn a_dead_pid_gets_no_signal() {
            // R3: the file names a process that has exited.
            let dir = tempfile::tempdir().unwrap();
            let program = placeholder(dir.path(), "8.4.3");
            let mut gone = Command::new("/bin/sleep").arg("0").spawn().unwrap();
            gone.wait().unwrap();
            let record = PidRecord {
                pid: gone.id(),
                exe: std::fs::canonicalize(&program).unwrap(),
                started: STARTED.into(),
            };
            assert!(check_recording(dir.path(), &record).await.is_empty());
        }

        #[tokio::test]
        async fn the_workers_of_a_master_that_only_dies_from_sigkill_are_stopped_too() {
            let dir = tempfile::tempdir().unwrap();
            let data = dir.path();
            let program = install_fake(data, "8.4.3");
            let recorder = Arc::new(Recorder {
                send: true,
                ..Recorder::default()
            });
            let mut php = manager(data, recorder.clone());
            // The master ignores SIGTERM, so that step waits its whole limit:
            // short. Every other wait ends early, the workers' too: theirs
            // stay long (`manager`), so a slow machine cannot turn a
            // worker's SIGTERM into a SIGKILL.
            php.ladder = Ladder {
                stop: Duration::ZERO,
                term: Duration::from_secs(1),
                kill: Duration::from_secs(10),
            };
            let daemon = Daemon::new(data.to_path_buf());
            let pool = fake_pool(data, "8.4.3", "idle");
            let Dial::Unix(socket) = &pool.dial else {
                panic!("a short socket path: {:?}", pool.dial);
            };

            // As a crashed daemon leaves it: a master with two workers, one of
            // them on the pool's socket, and a pid file.
            let pids_file = data.join("children");
            let mut command = fake_command(&program, "master");
            command.env(SOCKET, socket).env(CHILDREN, &pids_file);
            // On a leash, so the workers end even if a check below fails.
            let (mut master, _leash) = Bystander::leashed(&mut command);
            let mut workers: Vec<u32> = Vec::new();
            wait_until("the master's children", async || {
                let text = std::fs::read_to_string(&pids_file).unwrap_or_default();
                workers = text.lines().filter_map(|line| line.parse().ok()).collect();
                workers.len() == 2
            })
            .await;
            workers.sort_unstable();
            wait_until("a worker listens", async || !listen_free(&pool.dial).await).await;
            // Both are listed under the master and run its program.
            let listed = process::children(master.pid()).await.expect("the table");
            for pid in &workers {
                assert!(listed.iter().any(|c| c.pid == *pid), "{pid} in {listed:?}");
                let Liveness::Alive { exe, .. } = process::real_identity(*pid).await else {
                    panic!("no identity for worker {pid}");
                };
                assert!(accepted([program.clone()]).contains(&exe), "{exe:?}");
            }
            let pid_file = PidFile::new(data, PHP84);
            assert!(pid_file.record(&SystemFpm, master.pid(), &program).await);

            let watched = [&[master.pid()][..], &workers].concat();
            start(&php, &daemon, &pool, &recorder, &watched).await;
            // SIGTERM did nothing and SIGKILL ended the master; then each
            // worker got SIGTERM, which ended it. Nothing else was signalled.
            assert_eq!(master.wait().signal(), Some(9));
            let mut want = vec![(master.pid(), Signal::Term), (master.pid(), Signal::Kill)];
            want.extend(workers.iter().map(|pid| (*pid, Signal::Term)));
            assert_eq!(recorder.signals(), want);
            for pid in &workers {
                assert_eq!(process::real_identity(*pid).await, Liveness::Gone, "{pid}");
            }
            let runtime = daemon.pools.find("php-fpm-8.4").expect("listed");
            let lines = lock(&runtime).logs.recent();
            assert!(lines.iter().any(|line| line.text == STOPPING_ORPHAN));

            // The new pool runs, and its pid file names it.
            let mut pid = None;
            wait_until("the pool runs", async || {
                let status = lock(&runtime).status.clone();
                pid = status.pid.filter(|_| status.state == ServiceState::Running);
                pid.is_some()
            })
            .await;
            let pid = pid.expect("a pid");
            wait_until("the new pid file", async || {
                process::read_pid_file(&pid_file.path).is_some_and(|r| r.pid == pid)
            })
            .await;
            stop_pool(&daemon, PHP84).await;
            assert!(!pid_file.path.exists());
            assert_eq!(lock(&runtime).status.state, ServiceState::Stopped);
        }

        // The search under pid 1 needs the holders of the listener, which
        // only macOS lists (`process::listener_holders`), and only there do
        // orphans always go to pid 1. Linux waits for spike S-06b.
        #[cfg(target_os = "macos")]
        #[tokio::test]
        async fn under_pid_1_only_the_worker_on_the_pools_socket_is_stopped() {
            let dir = tempfile::tempdir().unwrap();
            let data = dir.path();
            let program = install_fake(data, "8.4.3");
            let recorder = Arc::new(Recorder {
                send: true,
                ..Recorder::default()
            });
            let php = manager(data, recorder.clone());
            let daemon = Daemon::new(data.to_path_buf());
            let pool = fake_pool(data, "8.4.3", "idle");
            let Dial::Unix(socket) = &pool.dial else {
                panic!("a short socket path: {:?}", pool.dial);
            };

            // A master with two children of its program. The first is on the
            // pool's socket: a worker. The second is not: it stands for a
            // php-fpm of this data folder that someone started by hand (ADR
            // 0009, R5e). Both are on a leash, which ends the second one.
            let pids_file = data.join("children");
            let mut command = fake_command(&program, "master");
            command.env(SOCKET, socket).env(CHILDREN, &pids_file);
            let (master, leash) = Bystander::leashed(&mut command);
            let mut children: Vec<u32> = Vec::new();
            wait_until("the master's children", async || {
                let text = std::fs::read_to_string(&pids_file).unwrap_or_default();
                children = text.lines().filter_map(|line| line.parse().ok()).collect();
                children.len() == 2
            })
            .await;
            let (on_socket, by_hand) = (children[0], children[1]);
            wait_until("a worker listens", async || !listen_free(&pool.dial).await).await;
            let pid_file = PidFile::new(data, PHP84);
            assert!(pid_file.record(&SystemFpm, master.pid(), &program).await);

            // The master dies as from a SIGKILL, and pid 1 adopts both.
            drop(master);
            wait_until("pid 1 adopts both", async || {
                let adopted = process::children(1).await.unwrap_or_default();
                let under_init = |pid: &u32| adopted.iter().any(|c| c.pid == *pid);
                children.iter().all(under_init)
            })
            .await;

            start(&php, &daemon, &pool, &recorder, &[on_socket, by_hand]).await;
            // Only the one on the socket, which SIGTERM ended.
            assert_eq!(recorder.signals(), [(on_socket, Signal::Term)]);
            assert_eq!(process::real_identity(on_socket).await, Liveness::Gone);
            // The same program, a later start, under pid 1, and not on the
            // socket: still there.
            let Liveness::Alive { exe, .. } = process::real_identity(by_hand).await else {
                panic!("the php-fpm that is not on the socket was stopped");
            };
            assert!(accepted([program.clone()]).contains(&exe), "{exe:?}");
            let runtime = daemon.pools.find("php-fpm-8.4").expect("listed");
            let lines = lock(&runtime).logs.recent();
            assert!(lines.iter().any(|line| line.text == STOPPING_ORPHAN));

            // The new pool runs, and its pid file names it.
            let mut pid = None;
            wait_until("the pool runs", async || {
                let status = lock(&runtime).status.clone();
                pid = status.pid.filter(|_| status.state == ServiceState::Running);
                pid.is_some()
            })
            .await;
            let pid = pid.expect("a pid");
            wait_until("the new pid file", async || {
                process::read_pid_file(&pid_file.path).is_some_and(|r| r.pid == pid)
            })
            .await;
            stop_pool(&daemon, PHP84).await;
            assert!(!pid_file.path.exists());

            // Off the leash, the other one ends by itself: no process is left.
            drop(leash);
            wait_until("the other php-fpm ends", async || {
                process::real_identity(by_hand).await == Liveness::Gone
            })
            .await;
        }
    }
}
