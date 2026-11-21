//! A server that a crashed `marsad` left running (ADR 0023, section 8; the
//! rules of ADR 0009). macOS and Linux only
//! ([`process::DATABASE_ORPHAN_RECOVERY`]): on Windows the Job Object ends
//! the servers with the daemon (ADR 0017).
//!
//! `run/<id>.pid` (`run/postgres.pid`) holds the pid, the file it really
//! runs, and its start time. The task of the run writes it as soon as the
//! supervisor says the pid and the process runs our server program
//! ([`RunRecord`]), however long the server takes to answer, and deletes it
//! when that run ends. Before each start, and at a stop when this
//! daemon runs no server, the file is read and the process at that pid is
//! checked again ([`marsa_core::orphan::classify_pidfile`]):
//!
//! - ours (one of the installed server programs of this data folder,
//!   started at that time): stopped with the server's own ladder
//!   ([`ServerKind::stop`]; for PostgreSQL SIGINT, then SIGQUIT, never
//!   SIGTERM), then SIGKILL, with the check again before each signal;
//! - gone, or another program now: the file is deleted, nothing is
//!   signalled;
//! - cannot be told (another user's process, which `lsof` does not show):
//!   no signal, and the file stays. A start goes on to its own checks: our
//!   server would still hold the port and its own pid file, which refuse it.
//!
//! A port, a protocol answer, or the server's own pid file
//! (`postmaster.pid`) never authorizes a signal, and that file is never
//! deleted here.

use marsa_core::codes;
use std::convert::Infallible;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use marsa_core::databases::ServerKind;
use marsa_core::orphan::{Liveness, PidRecord, PidfileState, classify_pidfile};
use marsa_core::platform::process::{self, Signal};
use marsa_core::services::{StopAction, StopMethod};
use marsa_core::types::Event;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::daemon::php::orphan::{accepted, own_executable};
use crate::daemon::services::probe::ProcessProbe;
use crate::daemon::services::runtime::AbortOnDrop;
use crate::daemon::supervisor::{ServiceRuntime, lock};

/// Our old server could not be stopped: nothing was started.
pub(super) fn stuck_code(kind: ServerKind) -> &'static str {
    match kind {
        ServerKind::Postgres => codes::POSTGRES_ORPHAN_STUCK,
        ServerKind::Mysql => codes::MYSQL_ORPHAN_STUCK,
        ServerKind::Valkey => codes::VALKEY_ORPHAN_STUCK,
    }
}

/// How often a wait looks again.
const POLL: Duration = Duration::from_millis(50);
/// How long, after the orphan is gone, the port may take to be free.
const PORT_WAIT: Duration = Duration::from_secs(5);
/// How long a SIGKILL may take to end the process.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// `run/<id>.pid` of `data_dir`.
pub(super) fn pid_path(kind: ServerKind, data_dir: &Path) -> PathBuf {
    data_dir.join("run").join(format!("{}.pid", kind.id()))
}

/// How long the record of a run looks again at a pid that does not run our
/// server yet, or that the system cannot tell this time ([`record`]).
const RECORD_LIMIT: Duration = Duration::from_secs(5);
/// How often it looks again. Short: Valkey takes connections a few
/// milliseconds after its exec, and until the record is written a crash of
/// `marsad` leaves the server unrecorded.
const RECORD_POLL: Duration = Duration::from_millis(5);
/// How long a start waits for the record of its run ([`settled`]): the
/// looks of [`RECORD_LIMIT`], and one more, which on macOS runs `ps`, `lsof`
/// and `ps` again, each for up to 2 s.
const SETTLE_LIMIT: Duration = Duration::from_secs(12);

/// Whether `run/<id>.pid` names a run, for the start that waits for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Recorded {
    /// The pid is not known yet, or the look goes on.
    Pending,
    /// The file names this pid.
    Written(u32),
    /// Not written: the system has no recovery, the pid does not run one of
    /// our server programs, the run ended first, or the file cannot be
    /// written.
    Never,
}

/// One run of a server, from its spawn to its end, for `run/<id>.pid`. Its
/// [`RunRecord::follow`] is the task of the run in `Daemon::procs`, so the
/// record does not wait for the server's first answer, and no start that a
/// stop drops can skip it.
pub(super) struct RunRecord {
    pub(super) kind: ServerKind,
    pub(super) probes: Arc<dyn ProcessProbe>,
    pub(super) data_dir: PathBuf,
    /// The server program of each installed version.
    pub(super) servers: Vec<PathBuf>,
    /// The status the supervisor keeps, with the pid of the run.
    pub(super) runtime: Arc<Mutex<ServiceRuntime>>,
    /// The event bus, subscribed before the spawn, so the supervisor's word
    /// on the pid is not missed.
    pub(super) events: broadcast::Receiver<Event>,
    /// What the record came to, for [`settled`].
    pub(super) recorded: watch::Sender<Recorded>,
}

impl RunRecord {
    /// Waits for the supervisor's task, and meanwhile writes the record as
    /// soon as the supervisor says the pid it spawned ([`record`]). Once the
    /// run has ended, deletes the record it wrote, whatever ended it: a
    /// stop, a shutdown, or the server itself. A server is never restarted,
    /// so the run is the task.
    pub(super) async fn follow(mut self, mut supervisor: JoinHandle<()>) {
        // `stop_procs` aborts this task after its time limit; the abort
        // reaches the supervisor too, so its `Child` is dropped.
        let _abort = AbortOnDrop(supervisor.abort_handle());
        let kind = self.kind;
        tokio::select! {
            ended = &mut supervisor => {
                if let Err(err) = ended {
                    warn!(%err, server = kind.id(), "the server task failed");
                }
            }
            never = self.record_spawn() => match never {},
        }
        let recorded = *self.recorded.borrow();
        if let Recorded::Written(pid) = recorded {
            forget_run(kind, &self.data_dir, pid);
        }
    }

    /// Writes the record of the spawn, then waits for ever: the run's end
    /// drops it.
    async fn record_spawn(&mut self) -> Infallible {
        let pid = self.spawned_pid().await;
        let runtime = &self.runtime;
        let owns = || lock(runtime).status.pid == Some(pid);
        let probes = &*self.probes;
        let (data_dir, servers) = (&self.data_dir, &self.servers);
        let written = record(
            self.kind,
            probes,
            data_dir,
            servers,
            pid,
            &owns,
            RECORD_LIMIT,
        )
        .await;
        // Kept when the start no longer waits for it: `follow` reads it.
        self.recorded.send_replace(written);
        std::future::pending().await
    }

    /// The pid the supervisor spawned, from the first status of this
    /// server with one.
    async fn spawned_pid(&mut self) -> u32 {
        loop {
            let status = match self.events.recv().await {
                Ok(Event::ServiceChanged(status)) if status.id == self.kind.id() => status,
                Ok(_) => continue,
                // Too far behind: the status now has the pid while it runs.
                Err(RecvError::Lagged(_)) => lock(&self.runtime).status.clone(),
                Err(RecvError::Closed) => return std::future::pending().await,
            };
            if let Some(pid) = status.pid {
                return pid;
            }
        }
    }
}

/// Writes `run/<id>.pid` for the run just spawned at `pid`, once the file it
/// really runs is one of `servers`. It looks again every [`RECORD_POLL`], up
/// to `limit` ([`RECORD_LIMIT`]), while the system cannot tell, or while the pid still
/// runs the daemon's own executable: on Linux, with glibc's `posix_spawn`,
/// `spawn` returns before the exec is done ([`own_executable`]). Never
/// written when the pid changes hands (its start time changes), or when
/// `owns` says the supervisor no longer runs it: once reaped, the pid may be
/// anyone's.
async fn record(
    kind: ServerKind,
    probes: &dyn ProcessProbe,
    data_dir: &Path,
    servers: &[PathBuf],
    pid: u32,
    owns: &(dyn Fn() -> bool + Send + Sync),
    limit: Duration,
) -> Recorded {
    if !process::DATABASE_ORPHAN_RECOVERY {
        return Recorded::Never;
    }
    let server = kind.id();
    let own = own_executable();
    let until = Instant::now() + limit;
    let mut first = None;
    loop {
        let now = probes.real_identity(pid).await;
        let again = match &now {
            Liveness::Alive { exe, started } => {
                if *first.get_or_insert(*started) != *started {
                    warn!(
                        pid,
                        server, "the pid record is not written: the pid changed hands"
                    );
                    return Recorded::Never;
                }
                if !owns() {
                    debug!(pid, server, "it ended before its pid record was written");
                    return Recorded::Never;
                }
                if servers.contains(exe) {
                    let record = PidRecord {
                        pid,
                        exe: exe.clone(),
                        started: *started,
                    };
                    return match process::write_pid_file(&pid_path(kind, data_dir), &record) {
                        Ok(()) => {
                            debug!(pid, server, "the pid record is written");
                            Recorded::Written(pid)
                        }
                        Err(err) => {
                            warn!(%err, server, "cannot write the pid record");
                            Recorded::Never
                        }
                    };
                }
                own.contains(exe)
            }
            Liveness::Gone => {
                debug!(pid, server, "it ended before its pid record was written");
                return Recorded::Never;
            }
            // A `ps` or `lsof` that failed once.
            Liveness::Unknown => true,
        };
        if !again || Instant::now() >= until {
            warn!(
                pid,
                ?now,
                server,
                "the pid record is not written: not our server"
            );
            return Recorded::Never;
        }
        tokio::time::sleep(RECORD_POLL).await;
    }
}

/// What the record of a run came to, once it is known, for at most
/// [`SETTLE_LIMIT`]. A run that ended before its record says `Never`.
pub(super) async fn settled(mut recorded: watch::Receiver<Recorded>) -> Recorded {
    let known = recorded.wait_for(|recorded| *recorded != Recorded::Pending);
    match tokio::time::timeout(SETTLE_LIMIT, known).await {
        Ok(Ok(recorded)) => *recorded,
        Ok(Err(_)) | Err(_) => Recorded::Never,
    }
}

/// The steps of `ladder` that are signals, with their waits. A request to the
/// server's port and a stop program are left out: they find the server by its
/// port or its data folder, which does not prove that the process of the pid
/// record is the one that holds them.
fn signal_steps(ladder: &StopMethod) -> Vec<(Signal, Duration)> {
    ladder
        .steps
        .iter()
        .filter_map(|step| match step.action {
            StopAction::Signal(signal) => Some((signal, step.wait)),
            StopAction::Send(_) | StopAction::Run(_) => None,
        })
        .collect()
}

/// Deletes `run/<id>.pid` when it names `pid`, a run that has ended. A
/// record of another pid (a newer run, or one that could not be told) stays.
pub(super) fn forget_run(kind: ServerKind, data_dir: &Path, pid: u32) {
    let path = pid_path(kind, data_dir);
    if process::read_pid_file(&path).is_some_and(|record| record.pid == pid) {
        forget(kind, data_dir);
    }
}

/// Deletes `run/<id>.pid`.
fn forget(kind: ServerKind, data_dir: &Path) {
    if let Err(err) = process::remove_pid_file(&pid_path(kind, data_dir)) {
        warn!(%err, server = kind.id(), "cannot delete the pid record");
    }
}

/// The one of `programs`, the server program of each installed version, that
/// our old server runs, which an earlier `marsad` left running and
/// `run/<id>.pid` names. `None` when there is no record, or its pid is not
/// that server now: gone, another program, or it cannot be told. Only reads:
/// nothing is signalled, and the record stays.
pub(super) async fn orphan_program(
    kind: ServerKind,
    probes: &dyn ProcessProbe,
    data_dir: &Path,
    programs: &[PathBuf],
) -> Option<PathBuf> {
    if !process::DATABASE_ORPHAN_RECOVERY {
        return None;
    }
    let record = process::read_pid_file(&pid_path(kind, data_dir))?;
    let servers = accepted(programs.iter().cloned());
    let state = check(probes, &record, &servers).await;
    debug!(pid = record.pid, %state, server = kind.id(), "checked the pid record");
    if !matches!(state, PidfileState::Ours { .. }) {
        return None;
    }
    // The record holds the path with links resolved: the program it is a
    // form of, which lies in its version's folder.
    let runs = |program: &&PathBuf| accepted([(*program).clone()]).contains(&record.exe);
    programs.iter().find(runs).cloned()
}

/// Why the start cannot go on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Stuck(pub(super) String);

/// How many times a start looks at the pid of the record. It goes on to its
/// own checks after a look that cannot tell: our server would still hold
/// the port and its own pid file, which refuse it.
pub(super) const START_LOOKS: u32 = 1;
/// How many times a stop looks, when the system cannot tell who runs the
/// pid, before it goes on without a signal: a `ps` or `lsof` may fail once,
/// and a stop has no later check to fall back on.
pub(super) const STOP_LOOKS: u32 = 3;
/// How long between two of those looks.
const LOOK_AGAIN: Duration = Duration::from_millis(250);

/// What [`clear`] did, for the recent lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Cleared {
    /// No file: nothing was signalled.
    Nothing,
    /// The system could not tell whether the pid of the file is our old
    /// server, at any look: nothing was signalled, and the file stays.
    Unknown { pid: u32 },
    /// The file named a process that is gone or another program's now: only
    /// the file was deleted.
    Forgotten,
    /// Our old server ended after `signal`, the last one sent (`None`: it
    /// ended before any).
    Stopped { pid: u32, signal: Option<Signal> },
}

/// Finds the server that `run/<id>.pid` names, and stops it when it is
/// ours, by `ladder` ([`ServerKind::stop`]) and a final SIGKILL, then waits
/// for `port`. A first look that cannot tell is made again, up to `looks`
/// looks in all ([`START_LOOKS`], [`STOP_LOOKS`]).
///
/// # Errors
///
/// [`Stuck`], with the reason, when our server still runs after SIGKILL, or
/// cannot be told from another program after a signal. Nothing else is
/// signalled then.
pub(super) async fn clear(
    kind: ServerKind,
    probes: &dyn ProcessProbe,
    data_dir: &Path,
    servers: &[PathBuf],
    ladder: &StopMethod,
    port: u16,
    looks: u32,
) -> Result<Cleared, Stuck> {
    if !process::DATABASE_ORPHAN_RECOVERY {
        return Ok(Cleared::Nothing);
    }
    let Some(record) = process::read_pid_file(&pid_path(kind, data_dir)) else {
        return Ok(Cleared::Nothing);
    };
    let pid = record.pid;
    let mut state = check(probes, &record, servers).await;
    for _ in 1..looks {
        if !matches!(state, PidfileState::Unknown { .. }) {
            break;
        }
        debug!(
            pid,
            server = kind.id(),
            "cannot tell who runs the pid yet; looking again"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
        state = check(probes, &record, servers).await;
    }
    info!(pid, %state, server = kind.id(), "checked the pid record");
    match state {
        PidfileState::Missing | PidfileState::Stale { .. } | PidfileState::Mismatch { .. } => {
            forget(kind, data_dir);
            return Ok(Cleared::Forgotten);
        }
        // Often another user's process at a reused pid, after a reboot.
        PidfileState::Unknown { .. } => {
            warn!(
                pid,
                server = kind.id(),
                "cannot tell whether the pid record names our old server; no signal"
            );
            return Ok(Cleared::Unknown { pid });
        }
        PidfileState::Ours { .. } => {}
    }
    // Signals only. A request to the server's port (Valkey's `SHUTDOWN
    // SAVE`), or a stop program that finds the server by its port or its
    // data folder, would reach whatever is there, which the check of the pid
    // does not prove is this process (ADR 0023, section 8).
    let signals = signal_steps(ladder)
        .into_iter()
        .chain([(Signal::Kill, KILL_WAIT)]);
    // The last signal sent, for the line that says how it stopped.
    let mut sent = None;
    for (signal, wait) in signals {
        match check(probes, &record, servers).await {
            PidfileState::Ours { .. } => {}
            PidfileState::Unknown { .. } => {
                let reason = format!(
                    "our old {} (pid {pid}) did not stop, and the system cannot tell whether \
                     that pid is still it, so it gets no {}",
                    kind.name(),
                    signal.name()
                );
                return Err(Stuck(reason));
            }
            // Gone, or someone else's now: it ended between two checks.
            _ => return Ok(done(kind, data_dir, pid, sent, port).await),
        }
        warn!(
            pid,
            signal = signal.name(),
            server = kind.id(),
            "stopping our old server"
        );
        if let Err(err) = probes.signal(pid, signal).await {
            warn!(pid, signal = signal.name(), %err, "the signal failed");
        }
        sent = Some(signal);
        if gone(probes, &record, servers, wait).await {
            return Ok(done(kind, data_dir, pid, sent, port).await);
        }
    }
    let reason = format!(
        "our old {} (pid {pid}) still runs after SIGKILL",
        kind.name()
    );
    Err(Stuck(reason))
}

/// The file against what the system says now: the file the process really
/// runs, and its start time.
async fn check(probes: &dyn ProcessProbe, record: &PidRecord, servers: &[PathBuf]) -> PidfileState {
    let now = probes.real_identity(record.pid).await;
    classify_pidfile(record, servers, &now)
}

/// Polls until the process is not our server any more, at most `limit`. An
/// answer that cannot be told waits on: it may be a `ps` that failed once.
async fn gone(
    probes: &dyn ProcessProbe,
    record: &PidRecord,
    servers: &[PathBuf],
    limit: Duration,
) -> bool {
    let started = Instant::now();
    loop {
        match check(probes, record, servers).await {
            PidfileState::Ours { .. } | PidfileState::Unknown { .. } => {}
            _ => return true,
        }
        if started.elapsed() >= limit {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// After the orphan ended: the file goes, then a short wait for the port, so
/// the check before a start does not find it busy with a listener that is
/// closing.
async fn done(
    kind: ServerKind,
    data_dir: &Path,
    pid: u32,
    signal: Option<Signal>,
    port: u16,
) -> Cleared {
    info!(
        pid,
        signal = signal.map(Signal::name),
        server = kind.id(),
        "our old server stopped"
    );
    forget(kind, data_dir);
    let started = Instant::now();
    while TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err() && started.elapsed() < PORT_WAIT {
        tokio::time::sleep(POLL).await;
    }
    Cleared::Stopped { pid, signal }
}

/// The line for `marsa logs <id>` after [`clear`] stopped our old server.
pub(super) fn stopped_line(kind: ServerKind, pid: u32, signal: Option<Signal>) -> String {
    let how = match (kind, signal) {
        (_, None) => "it ended before a signal".to_owned(),
        (ServerKind::Postgres, Some(Signal::Kill)) => "it needed SIGKILL, so its backends may \
                                                       still hold its shared memory for a \
                                                       moment; if PostgreSQL then does not \
                                                       start, start it again"
            .to_owned(),
        (_, Some(signal)) => format!("it ended after {}", signal.name()),
    };
    format!(
        "marsad: stopped the {} (pid {pid}) that an earlier marsad left running; {how}.",
        kind.name()
    )
}

/// The line for `marsa logs <id>` after a stop whose [`clear`] could not
/// tell who runs the pid of the record ([`Cleared::Unknown`]).
pub(super) fn unknown_line(kind: ServerKind, pid: u32) -> String {
    format!(
        "marsad: run/{id}.pid names pid {pid}, and the system cannot tell whether it is still \
         the {name} that an earlier marsad left running, so it got no signal; if that {name} \
         still runs, the next start says what holds its port.",
        id = kind.id(),
        name = kind.name()
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use marsa_core::services::{StopCommand, StopStep, TcpRequest};

    use super::*;

    /// An orphan is stopped by signals only: neither a request to the port
    /// nor a stop program, which would reach whatever holds the port or the
    /// data folder, goes in its ladder.
    #[test]
    fn an_orphans_ladder_has_signals_only() {
        let wait = Duration::from_secs(3);
        let ladder = StopMethod {
            steps: vec![
                StopStep {
                    action: StopAction::Send(TcpRequest {
                        port: 6379,
                        bytes: b"x".to_vec(),
                        name: "request",
                        limit: Duration::from_secs(1),
                    }),
                    wait,
                },
                StopStep {
                    action: StopAction::Run(StopCommand {
                        program: PathBuf::from("/bin/stop"),
                        args: Vec::new(),
                        name: "stop program",
                        on_failure: "",
                    }),
                    wait,
                },
                StopStep {
                    action: StopAction::Signal(Signal::Term),
                    wait,
                },
            ],
        };
        assert_eq!(signal_steps(&ladder), [(Signal::Term, wait)]);
        assert!(signal_steps(&StopMethod { steps: Vec::new() }).is_empty());
    }

    /// Each way [`record`] ends, against a fake that answers each look from
    /// a script. Only where the daemon writes records at all
    /// (`DATABASE_ORPHAN_RECOVERY`): elsewhere `record` writes none.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    mod record {
        use std::collections::VecDeque;
        use std::io;
        use std::sync::atomic::{AtomicU32, Ordering};

        use marsa_core::ps::StartTime;
        use tempfile::TempDir;

        use super::*;
        use crate::daemon::services::probe::BoxFuture;

        const KIND: ServerKind = ServerKind::Postgres;
        const PID: u32 = 4242;
        /// Short, so a test of a pid that never runs our server ends soon.
        const SHORT: Duration = Duration::from_millis(60);

        /// Answers each look from `looks`, then `last` for ever, and notes
        /// whether the record was on disk at each look. It never signals.
        #[derive(Debug)]
        struct Script {
            looks: Mutex<VecDeque<Liveness>>,
            last: Liveness,
            file: PathBuf,
            seen: Mutex<Vec<bool>>,
        }

        impl ProcessProbe for Script {
            fn real_identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
                assert_eq!(pid, PID);
                self.seen.lock().unwrap().push(self.file.exists());
                let next = self.looks.lock().unwrap().pop_front();
                Box::pin(std::future::ready(next.unwrap_or(self.last.clone())))
            }

            fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>> {
                panic!("a record sends no signal: {signal:?} to {pid}");
            }
        }

        /// A data folder with its `run/`, and the installed `postgres` in it.
        fn data() -> (TempDir, PathBuf) {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("run")).unwrap();
            let server = dir.path().join("runtimes/postgres/18.7.0/bin/postgres");
            (dir, server)
        }

        fn script(dir: &TempDir, looks: Vec<Liveness>, last: Liveness) -> Script {
            Script {
                looks: Mutex::new(looks.into()),
                last,
                file: pid_path(KIND, dir.path()),
                seen: Mutex::default(),
            }
        }

        fn alive(exe: &Path, ticks: u64) -> Liveness {
            Liveness::Alive {
                exe: exe.to_path_buf(),
                started: StartTime::Ticks(ticks),
            }
        }

        /// What the pid runs before its exec: the daemon's own executable,
        /// here the test binary.
        fn before_exec() -> PathBuf {
            std::env::current_exe().unwrap()
        }

        async fn run(
            dir: &TempDir,
            fake: &Script,
            server: &Path,
            owns: &(dyn Fn() -> bool + Send + Sync),
            limit: Duration,
        ) -> Recorded {
            let servers = [server.to_path_buf()];
            record(KIND, fake, dir.path(), &servers, PID, owns, limit).await
        }

        fn written(dir: &TempDir) -> Option<PidRecord> {
            process::read_pid_file(&pid_path(KIND, dir.path()))
        }

        /// The start time differs between two looks: the pid changed hands
        /// after the first one, so the program it runs now is not our spawn.
        #[tokio::test]
        async fn a_pid_whose_start_time_changes_is_not_recorded() {
            let (dir, server) = data();
            let fake = script(&dir, vec![alive(&before_exec(), 1)], alive(&server, 2));
            let got = run(&dir, &fake, &server, &|| true, RECORD_LIMIT).await;
            assert_eq!(got, Recorded::Never);
            assert_eq!(written(&dir), None);
            assert_eq!(fake.seen.lock().unwrap().len(), 2);
        }

        /// The supervisor no longer runs the pid (it reaped the exit): the
        /// pid may be anyone's, so no record, even of our program.
        #[tokio::test]
        async fn a_pid_the_supervisor_no_longer_owns_is_not_recorded() {
            let (dir, server) = data();
            let fake = script(&dir, Vec::new(), alive(&server, 1));
            let got = run(&dir, &fake, &server, &|| false, RECORD_LIMIT).await;
            assert_eq!(got, Recorded::Never);
            assert_eq!(written(&dir), None);

            // Owned at the first look, before the exec; not at the second.
            let fake = script(&dir, vec![alive(&before_exec(), 1)], alive(&server, 1));
            let looks = AtomicU32::new(0);
            let owns = || looks.fetch_add(1, Ordering::SeqCst) == 0;
            let got = run(&dir, &fake, &server, &owns, RECORD_LIMIT).await;
            assert_eq!(got, Recorded::Never);
            assert_eq!(written(&dir), None);
            assert_eq!(looks.load(Ordering::SeqCst), 2);
        }

        /// A look that cannot tell (a `ps` or `lsof` that failed) is made
        /// again, and the record is written once the pid is told.
        #[tokio::test]
        async fn an_unknown_look_is_made_again_then_recorded() {
            let (dir, server) = data();
            let looks = vec![Liveness::Unknown, Liveness::Unknown];
            let fake = script(&dir, looks, alive(&server, 1));
            let got = run(&dir, &fake, &server, &|| true, RECORD_LIMIT).await;
            assert_eq!(got, Recorded::Written(PID));
            let want = PidRecord {
                pid: PID,
                exe: server.clone(),
                started: StartTime::Ticks(1),
            };
            assert_eq!(written(&dir), Some(want));
            assert_eq!(*fake.seen.lock().unwrap(), [false, false, false]);
        }

        /// Before its exec the pid runs the daemon's own executable: no
        /// record while that holds, and one as soon as it runs our server.
        #[tokio::test]
        async fn a_pid_before_its_exec_is_recorded_once_it_runs_our_server() {
            let (dir, server) = data();
            let looks = vec![alive(&before_exec(), 1), alive(&before_exec(), 1)];
            let fake = script(&dir, looks, alive(&server, 1));
            let got = run(&dir, &fake, &server, &|| true, RECORD_LIMIT).await;
            assert_eq!(got, Recorded::Written(PID));
            assert_eq!(*fake.seen.lock().unwrap(), [false, false, false]);
            assert_eq!(written(&dir).map(|record| record.exe), Some(server));
        }

        /// A pid that never runs our server within the limit, still before
        /// its exec or never told, gets no record at all.
        #[tokio::test]
        async fn a_pid_that_never_runs_our_server_in_time_is_not_recorded() {
            for last in [alive(&before_exec(), 1), Liveness::Unknown] {
                let (dir, server) = data();
                let fake = script(&dir, Vec::new(), last.clone());
                let begun = Instant::now();
                let got = run(&dir, &fake, &server, &|| true, SHORT).await;
                assert_eq!(got, Recorded::Never, "{last:?}");
                assert_eq!(written(&dir), None, "{last:?}");
                assert!(fake.seen.lock().unwrap().len() >= 2, "{last:?}");
                assert!(begun.elapsed() < Duration::from_secs(2), "{last:?}");
            }
        }

        /// Another program, or a pid that is gone: no record, and no second
        /// look.
        #[tokio::test]
        async fn another_program_or_a_gone_pid_is_not_recorded() {
            for last in [alive(Path::new("/bin/sleep"), 1), Liveness::Gone] {
                let (dir, server) = data();
                let fake = script(&dir, Vec::new(), last.clone());
                let got = run(&dir, &fake, &server, &|| true, RECORD_LIMIT).await;
                assert_eq!(got, Recorded::Never, "{last:?}");
                assert_eq!(written(&dir), None, "{last:?}");
                assert_eq!(fake.seen.lock().unwrap().len(), 1, "{last:?}");
            }
        }
    }
}
