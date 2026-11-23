//! Finds the php-fpm master and workers a crashed daemon left, and stops them
//! only when every identity check passes.

use std::cmp::Ordering;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use marsa_core::caddy::Dial;
use marsa_core::orphan::{self, Liveness, PidRecord, PidfileState};
use marsa_core::platform::process::{self, Signal};
use marsa_core::ps::started_order;
use tracing::{debug, info, warn};

use super::listen::{describe, listen_free};
use super::{ORPHAN_STUCK, POLL, PidFile};
use crate::daemon::services::caddy::orphan::Ladder;
use crate::daemon::services::probe::ProcessProbe;

/// Why a pool must not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    /// [`ORPHAN_STUCK`].
    pub code: &'static str,
    pub reason: String,
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
