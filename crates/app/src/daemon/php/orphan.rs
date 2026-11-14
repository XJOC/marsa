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
//!   Only the master is signalled: its workers end with it. The pool starts
//!   once the process is gone and its socket or port is free.
//! - Anything else is never signalled. A stale file is deleted, and the log
//!   says why. A pid that cannot be told (a tool failed) is never signalled
//!   either: the pool waits, and the file stays, while the socket or port is
//!   in use.
//!
//! Known limit: php-fpm's workers end with a master that stops on SIGTERM.
//! A master that only dies from SIGKILL cannot end them, and macOS has no
//! parent-death signal, so workers that were serving a request can live on
//! and keep the socket. The pool then ends `php_fpm_orphan_stuck`; the
//! workers are not signalled, because no pid file names them.
//!
//! php-fpm writes its own `argv[0]` (`php-fpm: master process (...)`), so
//! `ps` shows no path for it. Identity is the file it really runs instead
//! ([`process::real_identity`]: `lsof` on macOS, `/proc` on Linux).
//!
//! All of this runs only where `process::ORPHAN_RECOVERY` is on (macOS now).
//! Elsewhere no pid file is written, nothing is signalled, and a file left
//! from before is deleted.

use std::fmt;
use std::io;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use marsa_core::caddy::Dial;
use marsa_core::orphan::{self, Liveness, PidRecord, PidfileState};
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

/// How long a probe of the socket may wait for a connection.
// Only the Unix socket probe uses it, and Unix sockets exist on Unix only.
#[cfg(unix)]
const CONNECT_LIMIT: Duration = Duration::from_millis(500);

/// Who runs under a pid, and signals. [`SystemFpm`] asks the OS; a test
/// records the signals instead.
pub trait FpmProcesses: fmt::Debug + Send + Sync {
    /// The file the process really runs, and its start time.
    fn identity(&self, pid: u32) -> BoxFuture<'_, Liveness>;
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
    /// Our php-fpm, alive, started at the recorded time.
    Orphan(PidRecord),
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
    /// `term` and `kill`. Caddy's `stop` step does not apply: php-fpm has no
    /// admin API.
    pub ladder: Ladder,
}

impl Orphans<'_> {
    /// Reads the pid file and checks the process it names. A file that
    /// names a process that is gone, or not our php-fpm, is deleted. A
    /// process that cannot be described gets no signal: while the socket or
    /// port is in use, the start is blocked and the file stays for the next
    /// try; once it is free, the file is deleted.
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
                debug!(pid, "the old php-fpm has exited; deleting its pid file");
                self.pid_file.remove();
            }
            PidfileState::Mismatch { found, .. } => {
                warn!(
                    pid,
                    %found,
                    "the php-fpm pid file names a process that is not our php-fpm; no signal, \
                     deleting the file"
                );
                self.pid_file.remove();
            }
            PidfileState::Unknown { .. } | PidfileState::Missing => {
                warn!(
                    pid,
                    "cannot tell who runs under the pid in the php-fpm pid file; no signal"
                );
                self.without_signal(&record, &state, false).await?;
            }
        }
        Ok(Found::Nothing)
    }

    /// Stops `record`, which [`Orphans::inspect`] found: SIGTERM, then
    /// SIGKILL, each only right after the pid is checked again, then waits
    /// for the socket or port. `Ok` once the pool may start.
    pub async fn reap(&self, record: PidRecord) -> Result<(), Blocked> {
        let pid = record.pid;
        let mut signalled = false;
        for (signal, wait) in [
            (Signal::Term, self.ladder.term),
            (Signal::Kill, self.ladder.kill),
        ] {
            let state = self.check(&record).await;
            if !matches!(state, PidfileState::Ours { .. }) {
                warn!(pid, ?signal, %state, "no signal: the pid is not verified as our php-fpm");
                return self.without_signal(&record, &state, signalled).await;
            }
            warn!(pid, ?signal, "the old php-fpm still runs; signalling it");
            if let Err(err) = self.processes.signal(pid, signal).await {
                warn!(pid, ?signal, %err, "the signal failed");
            }
            signalled = true;
            if self.wait_stopped(&record, wait).await {
                info!(pid, ?signal, "the old php-fpm stopped");
                self.pid_file.remove();
                return Ok(());
            }
        }
        Err(Blocked {
            code: ORPHAN_STUCK,
            reason: format!(
                "our old php-fpm (pid {pid}) did not stop after SIGKILL, or {} stays in use",
                describe(self.listen)
            ),
        })
    }

    /// The pid is not ours now (it exited, or changed hands), or it cannot
    /// be told: no signal. Once nothing holds the socket or port, the file
    /// is deleted and the pool may start. While something does, a pid that
    /// cannot be told blocks the start and its file stays.
    async fn without_signal(
        &self,
        record: &PidRecord,
        state: &PidfileState,
        signalled: bool,
    ) -> Result<(), Blocked> {
        let pid = record.pid;
        let unsure = matches!(state, PidfileState::Unknown { .. } | PidfileState::Missing);
        // Never verified before any signal: nothing of ours to wait for.
        // php-fpm says so itself if its socket is still in use.
        if (!signalled && !unsure) || listen_free(self.listen).await {
            if unsure {
                let exe = record.exe.display();
                warn!(pid, %exe, "the old pid may still be our php-fpm; forgetting it");
            }
            self.pid_file.remove();
            return Ok(());
        }
        let listen = describe(self.listen);
        let reason = if unsure {
            format!(
                "pid {pid} of the php-fpm pid file cannot be verified as our php-fpm, so it gets \
                 no signal, and {listen} is still in use"
            )
        } else {
            format!("our old php-fpm (pid {pid}) has exited, and {listen} is still in use")
        };
        Err(Blocked {
            code: ORPHAN_STUCK,
            reason,
        })
    }

    /// What the OS says about `record` now.
    async fn check(&self, record: &PidRecord) -> PidfileState {
        let now = self.processes.identity(record.pid).await;
        orphan::classify_pidfile(record, self.accepted, &now)
    }

    /// Polls until the socket or port is free and `record` is not our
    /// php-fpm any more. A free socket alone is not enough: a master that
    /// closed it may still be shutting down, and must not get a signal it
    /// does not need, nor a second php-fpm beside it.
    async fn wait_stopped(&self, record: &PidRecord, limit: Duration) -> bool {
        let started = Instant::now();
        loop {
            if listen_free(self.listen).await {
                match self.check(record).await {
                    // `Unknown` may be a tool that failed once: wait on.
                    PidfileState::Ours { .. } | PidfileState::Unknown { .. } => {}
                    _ => return true,
                }
            }
            if started.elapsed() >= limit {
                return false;
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
/// free. So this probe never decides alone whether our php-fpm still runs:
/// [`Orphans::wait_stopped`] also checks the pid.
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
    use std::collections::VecDeque;

    use semver::Version;

    use marsa_core::php::PhpFiles;

    use super::super::{PhpManager, Pool, fpm_spec};
    use crate::daemon::state::Daemon;

    const PROGRAM: &str = "/data/runtimes/php/8.4.3/bin/php-fpm";
    const STARTED: &str = "Wed Nov 4 03:31:27 2026";
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

    /// What the OS says: the front of the queue, and the last one again once
    /// one is left. A signal is recorded, never sent, and may change what the
    /// OS says next, as the death of the process would.
    #[derive(Debug, Default)]
    struct Fake {
        now: Mutex<VecDeque<Liveness>>,
        after_term: Option<Liveness>,
        after_kill: Option<Liveness>,
        signals: Mutex<Vec<(u32, Signal)>>,
    }

    impl Fake {
        fn new(now: impl IntoIterator<Item = Liveness>) -> Self {
            Self {
                now: Mutex::new(now.into_iter().collect()),
                ..Self::default()
            }
        }

        fn signals(&self) -> Vec<(u32, Signal)> {
            self.signals.lock().unwrap().clone()
        }
    }

    impl FpmProcesses for Fake {
        fn identity(&self, _pid: u32) -> BoxFuture<'_, Liveness> {
            let mut now = self.now.lock().unwrap();
            let next = if now.len() > 1 {
                now.pop_front()
            } else {
                now.front().cloned()
            };
            let next = next.unwrap_or(Liveness::Unknown);
            Box::pin(async move { next })
        }

        fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>> {
            self.signals.lock().unwrap().push((pid, signal));
            let after = match signal {
                Signal::Term => &self.after_term,
                Signal::Kill => &self.after_kill,
            };
            if let Some(state) = after {
                *self.now.lock().unwrap() = VecDeque::from([state.clone()]);
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
        let accepted = [PathBuf::from(PROGRAM)];
        let orphans = Orphans {
            pid_file,
            processes: fake,
            accepted: &accepted,
            listen,
            ladder: SHORT,
        };
        match orphans.inspect().await? {
            Found::Nothing => Ok(()),
            Found::Orphan(record) => orphans.reap(record).await,
        }
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
        php
    }

    #[tokio::test]
    async fn a_stubborn_orphan_gets_sigterm_then_sigkill_and_its_file_goes() {
        let (_dir, pid_file) = pid_file_of_ours();
        let fake = Fake {
            after_kill: Some(Liveness::Gone),
            ..Fake::new([ours()])
        };
        let listen = Dial::Tcp(free_port());
        assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
        let want = [(4242, Signal::Term), (4242, Signal::Kill)];
        assert_eq!(fake.signals(), want);
        assert!(!pid_file.path.exists());
    }

    #[tokio::test]
    async fn an_orphan_that_stops_on_sigterm_gets_no_sigkill() {
        let (_dir, pid_file) = pid_file_of_ours();
        let fake = Fake {
            after_term: Some(Liveness::Gone),
            ..Fake::new([ours()])
        };
        let listen = Dial::Tcp(free_port());
        assert_eq!(run(&pid_file, &fake, &listen).await, Ok(()));
        assert_eq!(fake.signals(), [(4242, Signal::Term)]);
        assert!(!pid_file.path.exists());
    }

    #[tokio::test]
    async fn the_pid_is_checked_again_before_each_signal() {
        // After SIGTERM the pid runs another program, and the port stays
        // busy: that program gets no SIGKILL.
        let (_dir, pid_file) = pid_file_of_ours();
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let listen = Dial::Tcp(held.local_addr().unwrap().port());
        let fake = Fake {
            after_term: Some(alive("/bin/sleep", STARTED)),
            ..Fake::new([ours()])
        };
        let blocked = run(&pid_file, &fake, &listen).await.unwrap_err();
        assert_eq!(blocked.code, ORPHAN_STUCK);
        assert!(blocked.reason.contains("is still in use"), "{blocked:?}");
        assert_eq!(fake.signals(), [(4242, Signal::Term)], "no SIGKILL");
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
        use std::collections::BTreeMap;
        use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
        use std::process::{Child, Command, ExitStatus, Stdio};

        use super::super::super::{pool_for, stop_pool, write_pool};

        /// The fake's mode: `hold` listens on `$MARSA_FPM_SOCKET`, `idle`
        /// does nothing, and `once` exits after a second the first time
        /// (until `$MARSA_FPM_MARKER` exists), then does nothing. Each ends by
        /// itself after two minutes.
        const FAKE: &str = "MARSA_FPM_FAKE";
        const SOCKET: &str = "MARSA_FPM_SOCKET";
        /// For `once`: the file that says the first run is over.
        const MARKER: &str = "MARSA_FPM_MARKER";
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
            php
        }

        async fn wait_until(what: &str, mut done: impl AsyncFnMut() -> bool) {
            let started = Instant::now();
            while !done().await {
                assert!(started.elapsed() < WAIT, "{what}");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
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

            assert!(php.start_pool(&daemon, &pool).await);
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

            assert!(php.start_pool(&daemon, &pool).await);
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
    }
}
