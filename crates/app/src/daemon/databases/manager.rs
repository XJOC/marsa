//! [`PostgresManager`]: starts and stops the one PostgreSQL under the
//! supervisor, one action at a time.

use std::fmt;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use marsa_core::config::{ConfigError, ServerSettings};
use marsa_core::databases::{
    LockConflict, POSTGRES_PORT, PostgresDirs, ServiceBusy, StartDecision, decide_start,
    line_version, postgres_stop, postmaster_pid, socket_for, startup_lock_conflict,
};
use marsa_core::orphan::Liveness;
use marsa_core::platform::process;
use marsa_core::recipes::Platform;
use marsa_core::runtimes::{Runtime, RuntimesDir};
use marsa_core::services::{ProgramNotAbsolute, ServiceSpec, StopMethod};
use marsa_core::types::{Event, LogLine, LogStream, ServiceAction, ServiceState, ServiceStatus};
use semver::Version;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::facts::{Probes, SystemProbes, start_facts};
use super::init::{InitError, Programs, ensure_cluster, installed_servers, programs};
use super::orphan;
use super::settings::{any_installed, read_settings, save_enabled};
use super::{POSTGRES_ID, postgres_spec};
use crate::daemon::caddy::{AbortOnDrop, note, set_failed};
use crate::daemon::state::Daemon;
use crate::daemon::supervisor::{EXITED, ServiceHandle, ServiceRuntime, Supervisor, lock};

/// How long a start waits for the server to take connections. S-12
/// measured 50 to 690 ms. The postmaster answers before it recovers its data
/// after a crash, so a long recovery does not count against this.
const READY_LIMIT: Duration = Duration::from_secs(15);
const READY_POLL: Duration = Duration::from_millis(100);
/// For each attempt of the readiness check.
const CONNECT_LIMIT: Duration = Duration::from_millis(500);
/// An `SSLRequest`: its length, 8, then the code 80877103. The first message
/// a client may send, before any user name.
pub(super) const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f];

/// Why an action on PostgreSQL failed. A start that fails also sets the
/// status `Failed` with the same code, and says why in its recent lines.
#[derive(Debug)]
pub enum ActionError {
    /// Marsa runs PostgreSQL on macOS and Linux only for now (Windows is
    /// task M6-07).
    Unsupported,
    /// `global.toml` cannot be read.
    Config(ConfigError),
    /// No installed version fits `[postgres] default`.
    NotInstalled(String),
    Init(InitError),
    /// The check before the start refused: another program holds the port,
    /// or a server runs on the data folder. Nothing was started or stopped.
    InUse(ServiceBusy),
    /// Another action on PostgreSQL has not finished.
    Busy,
    ProgramNotAbsolute(ProgramNotAbsolute),
    /// The job that writes `[postgres] enabled` failed (a bug).
    Internal,
    /// Our old PostgreSQL, left by a crashed `marsad`, could not be stopped,
    /// or cannot be told from another program.
    OrphanStuck(String),
}

impl ActionError {
    /// Stable error name for the UI.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unsupported => "postgres_unsupported_platform",
            Self::Config(err) => err.code(),
            Self::NotInstalled(_) => "postgres_not_installed",
            Self::Init(err) => err.code(),
            Self::InUse(busy) => busy.code(),
            Self::Busy => "service_busy",
            Self::ProgramNotAbsolute(err) => err.code(),
            Self::Internal => "internal_error",
            Self::OrphanStuck(_) => orphan::ORPHAN_STUCK,
        }
    }
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("Marsa runs PostgreSQL on macOS and Linux only"),
            Self::Config(err) => err.fmt(f),
            Self::NotInstalled(reason) => f.write_str(reason),
            Self::Init(err) => err.fmt(f),
            Self::InUse(busy) => busy.fmt(f),
            Self::Busy => f.write_str("another action on this service is running"),
            Self::ProgramNotAbsolute(err) => err.fmt(f),
            Self::Internal => f.write_str("the job that writes [postgres] enabled failed"),
            Self::OrphanStuck(reason) => f.write_str(reason),
        }
    }
}

/// Starts, stops, and watches the one PostgreSQL. It lives in
/// `Daemon::postgres`; its task, while it runs, is in `Daemon::procs`, so a
/// shutdown stops it, with the time its stop needs
/// (`ServiceHandle::stop_limit`).
#[derive(Debug)]
pub struct PostgresManager {
    /// Who runs under a pid and who holds a port. Tests put a fake here.
    pub(super) probes: Arc<dyn Probes>,
    /// How long a start waits for the server to take connections.
    ready_limit: Duration,
    /// The system whose programs run: [`Platform::current`].
    platform: Option<Platform>,
    /// How an old server that a crashed `marsad` left running is stopped
    /// before a start ([`orphan::clear`]): [`postgres_stop`], then SIGKILL.
    pub(super) orphan_ladder: StopMethod,
    /// The program of the last start, so an uninstall never deletes the
    /// version that runs. Use [`PostgresManager::running_program`].
    started: Mutex<Option<PathBuf>>,
    /// The pid the run that started last wrote to `run/postgres.pid`, so its
    /// end deletes only its own record.
    recorded: Mutex<Arc<Mutex<Option<u32>>>>,
    /// The status and recent lines, kept across runs, so `seq` never goes
    /// back and the lines of a failed run stay readable.
    runtime: Arc<Mutex<ServiceRuntime>>,
    /// The start that runs now and the stops that wait for their turn, so
    /// a stop never waits for a start: a first start may run `initdb` for
    /// up to 2 minutes.
    pub(super) starting: Mutex<Starting>,
    /// Held for the whole of each action, so two starts never run two
    /// servers. A tokio lock held across `.await` as a serialization token,
    /// not a guard of data (manager decision 2026-11-14). In an `Arc`, so a
    /// task can own its guard.
    pub(super) busy: Arc<tokio::sync::Mutex<()>>,
}

impl Default for PostgresManager {
    /// The real system. Nothing is read or started.
    fn default() -> Self {
        Self {
            probes: Arc::new(SystemProbes),
            ready_limit: READY_LIMIT,
            platform: Platform::current(),
            orphan_ladder: postgres_stop(),
            started: Mutex::default(),
            recorded: Mutex::default(),
            starting: Mutex::default(),
            runtime: Arc::new(Mutex::new(ServiceRuntime::new(POSTGRES_ID))),
            busy: Arc::default(),
        }
    }
}

/// The start that runs now, and how many stops wait for their turn. One
/// lock for both, so a start never begins between a stop's count and its
/// cancel.
#[derive(Debug, Default)]
pub(super) struct Starting {
    pub(super) token: Option<CancellationToken>,
    pub(super) stops_waiting: u32,
}

/// How an action ended, for `[postgres] enabled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    /// It ran: a start found the server running or ran it (its status says
    /// whether it then failed), or a stop.
    Done,
    /// A stop dropped the start: the stop decides.
    Dropped,
}

/// A stop in line: counted until it has its turn or gives up.
struct StopWaiting<'a>(&'a Mutex<Starting>);

impl Drop for StopWaiting<'_> {
    fn drop(&mut self) {
        let mut starting = lock(self.0);
        starting.stops_waiting = starting.stops_waiting.saturating_sub(1);
    }
}

/// Clears the token of a start when it ends, also when its future is
/// dropped.
struct ClearStart<'a>(&'a Mutex<Starting>);

impl Drop for ClearStart<'_> {
    fn drop(&mut self) {
        lock(self.0).token = None;
    }
}

/// What a start that got as far as a run came to.
enum Run {
    /// Running, or it ended by itself (its status says why).
    Over,
    /// It took no connections in time: the caller stops it.
    TimedOut { port: u16 },
}

/// What a start runs, after the check before it and the first init.
struct Prepared {
    spec: ServiceSpec,
    /// The `postgres` of each installed version, for `run/postgres.pid`.
    servers: Vec<PathBuf>,
    /// The cluster, for its `postmaster.pid`.
    data: PathBuf,
    /// The socket, for the name of its lock file: `None` for TCP only.
    socket: Option<PathBuf>,
    port: u16,
}

enum Ready {
    Yes,
    /// The run ended by itself first: the status says why.
    Ended,
    TimedOut,
}

impl PostgresManager {
    pub fn status(&self) -> ServiceStatus {
        lock(&self.runtime).status.clone()
    }

    /// The last lines, oldest first.
    pub fn recent_logs(&self) -> Vec<LogLine> {
        lock(&self.runtime).logs.recent()
    }

    /// PostgreSQL's turn, if no action runs now. An uninstall holds it, so
    /// no start picks the version while its folder is moved aside.
    pub fn try_turn(&self) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        Arc::clone(&self.busy).try_lock_owned().ok()
    }

    /// Whether `GET /v1/services` lists PostgreSQL: when a version is
    /// installed, or when its status says more than `stopped` (a start
    /// that failed because none is installed).
    pub fn listed(&self, data_dir: &Path) -> bool {
        self.status().state != ServiceState::Stopped || any_installed(data_dir)
    }

    /// The program PostgreSQL runs from now, or `None` while it does not run.
    pub fn running_program(&self, daemon: &Daemon) -> Option<PathBuf> {
        if !daemon.proc_alive(POSTGRES_ID) {
            return None;
        }
        lock(&self.started).clone()
    }

    /// Runs `action` and returns the status after it. An action that finds
    /// another one running waits up to `wait` for it in line, as for Caddy
    /// (`CaddyManager::run_waiting`). `start` on a running PostgreSQL and
    /// `stop` on a stopped one change nothing. Once the daemon begins to
    /// stop, nothing runs: the shutdown stops PostgreSQL itself.
    ///
    /// A `stop` does not wait for a start ([`Self::stop_waits`]): it drops
    /// the start that runs (with the daemon, of the API, or after an
    /// install), and every start that would come before it in line, then
    /// stops what a dropped start spawned.
    ///
    /// # Errors
    ///
    /// `service_busy` when the wait runs out or the daemon stops; the errors
    /// of a start ([`ActionError`]).
    pub async fn run_waiting(
        &self,
        daemon: &Daemon,
        action: ServiceAction,
        wait: Duration,
    ) -> Result<ServiceStatus, ActionError> {
        self.run(daemon, action, wait, None).await
    }

    /// [`Self::run_waiting`], then `[postgres] enabled` as the action leaves
    /// it, still in this action's turn, so the writes keep the order of the
    /// actions (ADR 0023, section 5): off after a stop; on after a start or
    /// restart that ran, even when the server then failed (its status says
    /// why), or found it running. A refusal before any run, or a start that
    /// a stop dropped, changes nothing. For `marsa services`.
    ///
    /// # Errors
    ///
    /// Those of [`Self::run_waiting`], and the `config_*` codes when
    /// `global.toml` cannot be written.
    pub async fn run_and_save(
        &self,
        daemon: &Arc<Daemon>,
        action: ServiceAction,
        wait: Duration,
    ) -> Result<ServiceStatus, ActionError> {
        self.run(daemon, action, wait, Some(daemon)).await
    }

    /// `save`: the daemon, to write `[postgres] enabled` with.
    async fn run(
        &self,
        daemon: &Daemon,
        action: ServiceAction,
        wait: Duration,
        save: Option<&Arc<Daemon>>,
    ) -> Result<ServiceStatus, ActionError> {
        let waiting = (action == ServiceAction::Stop).then(|| self.stop_waits());
        let busy = match Arc::clone(&self.busy).try_lock_owned() {
            Ok(busy) => busy,
            Err(_) => {
                info!(%action, "another action on postgres runs; waiting for it");
                let turn = tokio::time::timeout(wait, Arc::clone(&self.busy).lock_owned());
                tokio::select! {
                    () = daemon.shutdown.cancelled() => return Err(ActionError::Busy),
                    turn = turn => turn.map_err(|_| ActionError::Busy)?,
                }
            }
        };
        // This stop has its turn: starts after it in line run.
        drop(waiting);
        // Also when the turn came at the same moment as the shutdown.
        if daemon.shutdown.is_cancelled() {
            return Err(ActionError::Busy);
        }
        let outcome = self.act(daemon, action, &busy).await?;
        let enabled = match (action, outcome) {
            (ServiceAction::Stop, _) => Some(false),
            (_, Outcome::Done) => Some(true),
            (_, Outcome::Dropped) => None,
        };
        if let (Some(daemon), Some(enabled)) = (save, enabled) {
            save_enabled(daemon, enabled).await?;
        }
        // Before the next action can change it.
        let status = self.status();
        drop(busy);
        Ok(status)
    }

    /// Runs `action`. `_busy` proves that no other action runs.
    pub(super) async fn act(
        &self,
        daemon: &Daemon,
        action: ServiceAction,
        _busy: &tokio::sync::OwnedMutexGuard<()>,
    ) -> Result<Outcome, ActionError> {
        match action {
            ServiceAction::Start => self.start_or_drop(daemon).await,
            ServiceAction::Stop => {
                self.stop(daemon).await;
                Ok(Outcome::Done)
            }
            ServiceAction::Restart => {
                self.stop(daemon).await;
                self.start_or_drop(daemon).await
            }
        }
    }

    /// Counts a stop in line and drops the start that runs. Until the
    /// count goes back down (the stop has its turn, or gives up), no start
    /// begins ([`Self::start_or_drop`]).
    fn stop_waits(&self) -> StopWaiting<'_> {
        let mut starting = lock(&self.starting);
        starting.stops_waiting += 1;
        if let Some(token) = starting.token.take() {
            info!("a stop drops the start of postgres that runs");
            token.cancel();
        }
        StopWaiting(&self.starting)
    }

    /// [`Self::start`], unless a stop waits in line, until a stop drops it
    /// or the daemon stops. A dropped start ends at its next `.await`: a
    /// step of the first init is killed (`kill_on_drop`) and leaves a temp
    /// folder that the next init deletes; a server it spawned is in
    /// `Daemon::procs` already, because the spawn and `add_proc` have no
    /// `.await` between them, so the stop that dropped it, or the
    /// shutdown, stops it. The stop of a server that took no connections in
    /// time runs after, and no stop drops it.
    async fn start_or_drop(&self, daemon: &Daemon) -> Result<Outcome, ActionError> {
        let token = {
            let mut starting = lock(&self.starting);
            if starting.stops_waiting > 0 {
                info!("a stop waits in line; the start of postgres is dropped");
                return Ok(Outcome::Dropped);
            }
            let token = CancellationToken::new();
            starting.token = Some(token.clone());
            token
        };
        let clear = ClearStart(&self.starting);
        let run = tokio::select! {
            biased;
            () = token.cancelled() => {
                info!("a stop dropped the start of postgres");
                return Ok(Outcome::Dropped);
            }
            () = daemon.shutdown.cancelled() => return Err(ActionError::Busy),
            run = self.start(daemon) => run?,
        };
        drop(clear);
        if let Run::TimedOut { port } = run {
            warn!(limit = ?self.ready_limit, "postgres takes no connections; stopping it");
            self.stop(daemon).await;
            let reason = format!(
                "it took no connections on 127.0.0.1:{port} within {} s",
                self.ready_limit.as_secs()
            );
            self.fail(&daemon.events, "postgres_start_timeout", &reason);
        }
        Ok(Outcome::Done)
    }

    async fn start(&self, daemon: &Daemon) -> Result<Run, ActionError> {
        // Our own supervisor runs it already. It holds the port and
        // `postmaster.pid`, so the check before a start would call it busy.
        if daemon.proc_alive(POSTGRES_ID) {
            return Ok(Run::Over);
        }
        // No task runs from here on, so these writes cannot race the
        // supervisor.
        let prepared = match self.prepare(daemon).await {
            Ok(prepared) => prepared,
            Err(err) => {
                self.fail(&daemon.events, err.code(), &err.to_string());
                return Err(err);
            }
        };
        let Prepared {
            spec,
            servers,
            data,
            socket,
            port,
        } = prepared;
        // This run's lines come after the ones there now.
        let first_seq = self.recent_logs().last().map_or(0, |line| line.seq + 1);
        self.spawn(daemon, spec)?;
        match self.wait_ready(daemon, port, &servers).await {
            Ready::Yes => {
                info!(port, "postgres takes connections");
                let slot = Arc::clone(&lock(&self.recorded));
                if process::DATABASE_ORPHAN_RECOVERY && lock(&slot).is_none() {
                    warn!(
                        "postgres runs, but not as an installed postgres the system could tell, \
                         so run/postgres.pid was not written: a crash of marsad now leaves it \
                         unrecorded"
                    );
                }
            }
            Ready::Ended => {
                // Only a run that exited by itself: a shutdown that took the
                // handle and stopped it leaves `Stopped`, and its lines are
                // not read.
                let state = self.status().state;
                let exited = ServiceState::Failed {
                    code: EXITED.to_owned(),
                };
                if state == exited {
                    let refused = self.lock_refusal(first_seq, &data, socket.as_deref(), port);
                    if let Some((busy, reason)) = refused.await {
                        self.fail(&daemon.events, busy.code(), &reason);
                        return Err(ActionError::InUse(busy));
                    }
                }
                warn!(?state, "postgres ended before it was ready");
            }
            Ready::TimedOut => return Ok(Run::TimedOut { port }),
        }
        Ok(Run::Over)
    }

    /// The version, the check before the start (ADR 0023, section 7), then
    /// the first init when the cluster is new. The check comes first, so a
    /// busy port makes no cluster.
    async fn prepare(&self, daemon: &Daemon) -> Result<Prepared, ActionError> {
        let settings = read_settings(&daemon.data_dir)?;
        let (platform, version, programs) = self.installed(&daemon.data_dir, &settings)?;
        let port = settings.port.unwrap_or(POSTGRES_PORT);
        let dirs = PostgresDirs::new(&daemon.data_dir, version.major);
        let servers = installed_servers(&daemon.data_dir, platform);
        let ladder = &self.orphan_ladder;
        let cleared = orphan::clear(&*self.probes, &daemon.data_dir, &servers, ladder, port);
        match cleared.await {
            Ok(orphan::Cleared::Stopped { pid, signal }) => {
                let text = orphan::stopped_line(pid, signal);
                note(&self.runtime, &daemon.events, &text);
            }
            Ok(orphan::Cleared::Nothing | orphan::Cleared::Forgotten) => {}
            Err(orphan::Stuck(reason)) => return Err(ActionError::OrphanStuck(reason)),
        }
        let facts = start_facts(&*self.probes, port, &dirs.data, &servers).await;
        info!(?facts, "checked before the start of postgres");
        if let StartDecision::Refuse(busy) = decide_start(&facts) {
            return Err(ActionError::InUse(busy));
        }
        ensure_cluster(&programs, &dirs)
            .await
            .map_err(ActionError::Init)?;
        info!(%version, port, "starting postgres");
        // `pg_hba.conf` lets the superuser in over the socket only, so
        // without one nobody can connect as `postgres` (ADR 0023, "Access").
        let socket = socket_for(&dirs, port);
        if let Err(why) = &socket {
            let folder = dirs.sockets.display();
            warn!(%folder, %why, "no Unix socket for postgres: TCP only, no superuser");
            let text = format!(
                "marsad: PostgreSQL gets no Unix socket in {folder}: {why}. So it takes TCP \
                 connections only, and its superuser postgres cannot connect."
            );
            note(&self.runtime, &daemon.events, &text);
        }
        Ok(Prepared {
            spec: postgres_spec(programs.postgres, &dirs, port),
            servers,
            socket: socket.ok(),
            data: dirs.data,
            port,
        })
    }

    /// The programs to run for `settings`: the newest installed version of
    /// the `default` line, or of all lines when it is not set.
    pub(super) fn installed(
        &self,
        data_dir: &Path,
        settings: &ServerSettings,
    ) -> Result<(Platform, Version, Programs), ActionError> {
        let platform = match self.platform {
            Some(platform) if !matches!(platform, Platform::WindowsX64 | Platform::Any) => platform,
            _ => return Err(ActionError::Unsupported),
        };
        let line = settings.default.as_deref();
        let missing = || {
            let what = line.map_or_else(String::new, |line| format!(" {line}"));
            ActionError::NotInstalled(format!("no PostgreSQL{what} is installed"))
        };
        let installed = RuntimesDir::in_dir(data_dir)
            .installed(Runtime::Postgres)
            .map_err(|err| ActionError::NotInstalled(err.to_string()))?;
        let version = line_version(line, &installed).ok_or_else(missing)?;
        let programs = programs(data_dir, platform, &version)
            .filter(|programs| programs.postgres.is_file())
            .ok_or_else(missing)?;
        Ok((platform, version, programs))
    }

    /// Spawns the server under the supervisor, and registers its task in
    /// `Daemon::procs`.
    fn spawn(&self, daemon: &Daemon, spec: ServiceSpec) -> Result<(), ActionError> {
        let logs_dir = daemon.data_dir.join("logs");
        let program = spec.program.clone();
        let runtime = Arc::clone(&self.runtime);
        let handle = Supervisor::spawn_on(spec, daemon.events.clone(), &logs_dir, runtime)
            .map_err(|err| {
                self.fail(&daemon.events, err.code(), &err.to_string());
                ActionError::ProgramNotAbsolute(err)
            })?;
        *lock(&self.started) = Some(program);
        // This run's own record, set once `wait_ready` writes it.
        let recorded = Arc::new(Mutex::new(None));
        *lock(&self.recorded) = Arc::clone(&recorded);
        // The run's end deletes its `run/postgres.pid`, whatever ended it: a
        // stop, a shutdown, or the server itself. PostgreSQL is never
        // restarted, so the run is the task.
        let ServiceHandle {
            cancel,
            task,
            runtime,
            stop_limit,
        } = handle;
        let data_dir = daemon.data_dir.clone();
        let task = tokio::spawn(async move {
            // `stop_procs` aborts this task after its time limit; the abort
            // reaches the supervisor too, so its `Child` is dropped.
            let _abort = AbortOnDrop(task.abort_handle());
            if let Err(err) = task.await {
                warn!(%err, "the postgres task failed");
            }
            if let Some(pid) = *lock(&recorded) {
                orphan::forget_run(&data_dir, pid);
            }
        });
        let handle = ServiceHandle {
            cancel,
            task,
            runtime,
            stop_limit,
        };
        // A handle this replaces belongs to a run that has ended.
        daemon.add_proc(POSTGRES_ID, handle);
        Ok(())
    }

    /// Asks `127.0.0.1:<port>` ([`answers`]) until our server answers, its
    /// run ends, or the limit. The check before the start found the port
    /// free, so what answers is our server.
    /// Its pid goes to `run/postgres.pid` as soon as it runs one of
    /// `servers`.
    async fn wait_ready(&self, daemon: &Daemon, port: u16, servers: &[PathBuf]) -> Ready {
        let mut recorded = false;
        let poll = async {
            loop {
                if !daemon.proc_alive(POSTGRES_ID) {
                    return Ready::Ended;
                }
                if !recorded && let Some(pid) = self.status().pid {
                    let probes = &*self.probes;
                    recorded = orphan::record(probes, &daemon.data_dir, servers, pid).await;
                    if recorded {
                        let slot = Arc::clone(&lock(&self.recorded));
                        *lock(&slot) = Some(pid);
                    }
                }
                if answers(port).await {
                    return Ready::Yes;
                }
                tokio::time::sleep(READY_POLL).await;
            }
        };
        tokio::time::timeout(self.ready_limit, poll)
            .await
            .unwrap_or(Ready::TimedOut)
    }

    /// Why a run that ended by itself before it took connections did not
    /// start, when PostgreSQL refused because a live process holds one of
    /// its lock files ([`startup_lock_conflict`] on this run's stderr, from
    /// `first_seq`), with the program at that pid now:
    ///
    /// - `postmaster.pid`: `service_data_dir_busy`, as the check before the
    ///   start reports it. The pid comes from PostgreSQL's hint, else from
    ///   `postmaster.pid` (read only), and may stay unknown;
    /// - the lock of the socket: `service_port_busy`, the server that uses
    ///   the socket of this port.
    ///
    /// Only read once the run has ended: a line never stops a server. That
    /// process gets no signal, and `postmaster.pid` stays: the pid may be
    /// another program's now (`orphan::clear` handles our own).
    async fn lock_refusal(
        &self,
        first_seq: u64,
        data: &Path,
        socket: Option<&Path>,
        port: u16,
    ) -> Option<(ServiceBusy, String)> {
        let recent = self.recent_logs();
        let stderr: Vec<&str> = recent
            .iter()
            .filter(|line| line.seq >= first_seq && line.stream == LogStream::Stderr)
            .map(|line| line.text.as_str())
            .collect();
        let conflict = startup_lock_conflict(&stderr, socket)?;
        let no_signal = "and Marsa sends it no signal";
        match conflict {
            LockConflict::DataDir { pid } => {
                let pid = pid.or_else(|| {
                    let text = process::read_small_file(&data.join("postmaster.pid"))?;
                    postmaster_pid(&text)
                });
                let busy = ServiceBusy::DataDir { pid };
                let reason = match pid {
                    Some(pid) => {
                        let program = self.program_at(pid).await;
                        format!("{busy}; pid {pid} runs {program} now, {no_signal}")
                    }
                    None => format!("{busy}; PostgreSQL named no pid, {no_signal}"),
                };
                Some((busy, reason))
            }
            LockConflict::Socket { pid } => {
                let owner = match pid {
                    Some(pid) => Some(format!("{} (pid {pid})", self.program_at(pid).await)),
                    None => None,
                };
                let busy = ServiceBusy::Port { port, owner };
                let lock = socket.map(|socket| socket.display().to_string());
                let lock = lock.unwrap_or_default();
                let reason = format!("{busy}: it holds the lock of the socket {lock}, {no_signal}");
                Some((busy, reason))
            }
        }
    }

    /// The program at `pid` now, in words, for a message.
    async fn program_at(&self, pid: u32) -> String {
        match self.probes.real_identity(pid).await {
            Liveness::Alive { exe, .. } => exe.display().to_string(),
            Liveness::Gone => "nothing (it has ended since)".to_owned(),
            Liveness::Unknown => "a program that cannot be told".to_owned(),
        }
    }

    /// Stops the task: SIGINT, then SIGQUIT, then a kill, by the spec. When
    /// this daemon runs none, our old server that an earlier `marsad` left
    /// running is stopped instead ([`orphan::clear`]). If that one cannot be
    /// stopped, the recent lines say why; the next start refuses then. Any
    /// other PostgreSQL is never touched.
    async fn stop(&self, daemon: &Daemon) {
        // `None`: nothing runs, or a shutdown took the handle and stops it.
        if let Some(handle) = daemon.take_proc(POSTGRES_ID) {
            if let Err(err) = handle.stop().await {
                warn!(%err, "the postgres task failed");
            }
            return;
        }
        if daemon.shutdown.is_cancelled() {
            return;
        }
        let Some(platform) = self.platform else {
            return;
        };
        let port = read_settings(&daemon.data_dir)
            .ok()
            .and_then(|settings| settings.port)
            .unwrap_or(POSTGRES_PORT);
        let servers = installed_servers(&daemon.data_dir, platform);
        let ladder = &self.orphan_ladder;
        let cleared = orphan::clear(&*self.probes, &daemon.data_dir, &servers, ladder, port);
        match cleared.await {
            Ok(orphan::Cleared::Stopped { pid, signal }) => {
                let text = orphan::stopped_line(pid, signal);
                note(&self.runtime, &daemon.events, &text);
            }
            Ok(orphan::Cleared::Nothing | orphan::Cleared::Forgotten) => {}
            Err(orphan::Stuck(reason)) => {
                warn!(%reason, "our old postgres did not stop");
                note(&self.runtime, &daemon.events, &format!("marsad: {reason}."));
            }
        }
    }

    /// Says why in the recent lines, and sets `Failed { code }`. Only while
    /// no task runs: the supervisor owns the status then.
    pub(super) fn fail(&self, events: &broadcast::Sender<Event>, code: &str, reason: &str) {
        warn!(code, %reason, "postgres was not started");
        let text = format!("marsad: PostgreSQL was not started: {reason}.");
        note(&self.runtime, events, &text);
        set_failed(&self.runtime, events, code);
    }
}

/// Whether PostgreSQL takes connections on `127.0.0.1:<port>`: it answers
/// an [`SSL_REQUEST`] with one byte, `N` or `S`. Only a postmaster whose
/// start is done answers: it has taken its lock files and made all its
/// sockets. A bare TCP connection is not enough, because the TCP listener
/// is made before the socket and its lock file. PostgreSQL logs nothing for
/// this: the connection closes before any startup message.
async fn answers(port: u16) -> bool {
    let ask = async {
        let address = (Ipv4Addr::LOCALHOST, port);
        let mut stream = tokio::net::TcpStream::connect(address).await.ok()?;
        stream.write_all(&SSL_REQUEST).await.ok()?;
        let mut answer = [0_u8; 1];
        stream.read_exact(&mut answer).await.ok()?;
        Some(matches!(&answer, b"N" | b"S"))
    };
    matches!(
        tokio::time::timeout(CONNECT_LIMIT, ask).await,
        Ok(Some(true))
    )
}
