//! [`ServerManager`]: starts and stops one database server under the
//! supervisor, one action at a time.

use marsa_core::codes;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use marsa_core::config::{ConfigError, ServerSettings};
use marsa_core::databases::mysql::{MysqlDirs, decide_mysql_start, my_cnf, mysqld_pid};
use marsa_core::databases::valkey::{ValkeyDirs, valkey_conf};
use marsa_core::databases::{
    PostgresDirs, ServerKind, ServiceBusy, StartDecision, StartFacts, UnwritablePath, decide_start,
    postmaster_pid, socket_for,
};
use marsa_core::platform::process;
use marsa_core::recipes::Platform;
use marsa_core::runtimes::RuntimesDir;
use marsa_core::services::{ProgramNotAbsolute, ServiceSpec, StopMethod};
use marsa_core::types::{Event, LogLine, LogStream, ServiceAction, ServiceState, ServiceStatus};
use semver::Version;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::facts::{port_facts, start_facts};
use super::init::{InitError, installed_servers};
use super::settings::{any_installed, read_settings, save_enabled};
use super::{mysql, orphan, postgres, valkey};
use crate::daemon::services::actions::{Busy, take_turn};
use crate::daemon::services::probe::{ProcessProbe, SystemProbe};
use crate::daemon::services::runtime::AbortOnDrop;
use crate::daemon::state::Daemon;
use crate::daemon::supervisor::{EXITED, ServiceHandle, ServiceRuntime, Supervisor, lock};

/// How long a start waits for the server to take connections. S-12
/// measured 50 to 690 ms. The postmaster answers before it recovers its data
/// after a crash, so a long recovery does not count against this.
const READY_LIMIT: Duration = Duration::from_secs(15);
const READY_POLL: Duration = Duration::from_millis(100);

/// Why an action on a server failed. A start that fails also sets the
/// status `Failed` with the same code, and says why in its recent lines.
#[derive(Debug)]
pub enum ActionError {
    /// Marsa runs this server on macOS and Linux only for now (Windows comes later).
    Unsupported(ServerKind),
    /// `global.toml` cannot be read.
    Config(ConfigError),
    /// No installed version fits the `default` of its table.
    NotInstalled(ServerKind, String),
    Init(InitError),
    /// The check before the start refused: another program holds the port,
    /// or a server runs on the data folder. Nothing was started or stopped.
    InUse(ServiceBusy),
    /// Another action on the server has not finished.
    Busy,
    ProgramNotAbsolute(ProgramNotAbsolute),
    /// The job that writes `enabled` failed (a bug).
    Internal,
    /// Our old server, left by a crashed `marsad`, could not be stopped, or
    /// cannot be told from another program.
    OrphanStuck(ServerKind, String),
    /// A path of the data folder cannot go in MySQL's `my.cnf` or Valkey's
    /// `valkey.conf`.
    Path(UnwritablePath),
}

impl ActionError {
    /// Stable error name for the UI.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unsupported(ServerKind::Postgres) => codes::POSTGRES_UNSUPPORTED_PLATFORM,
            Self::Unsupported(ServerKind::Mysql) => codes::MYSQL_UNSUPPORTED_PLATFORM,
            Self::Unsupported(ServerKind::Valkey) => codes::VALKEY_UNSUPPORTED_PLATFORM,
            Self::Config(err) => err.code(),
            Self::NotInstalled(ServerKind::Postgres, _) => codes::POSTGRES_NOT_INSTALLED,
            Self::NotInstalled(ServerKind::Mysql, _) => codes::MYSQL_NOT_INSTALLED,
            Self::NotInstalled(ServerKind::Valkey, _) => codes::VALKEY_NOT_INSTALLED,
            Self::Init(err) => err.code(),
            Self::InUse(busy) => busy.code(),
            Self::Busy => codes::SERVICE_BUSY,
            Self::ProgramNotAbsolute(err) => err.code(),
            Self::Internal => codes::INTERNAL_ERROR,
            Self::OrphanStuck(kind, _) => orphan::stuck_code(*kind),
            Self::Path(err) => err.code(),
        }
    }
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(kind) => {
                write!(f, "Marsa runs {} on macOS and Linux only", kind.name())
            }
            Self::Config(err) => err.fmt(f),
            Self::NotInstalled(_, reason) => f.write_str(reason),
            Self::Init(err) => err.fmt(f),
            Self::InUse(busy) => busy.fmt(f),
            Self::Busy => f.write_str("another action on this service is running"),
            Self::ProgramNotAbsolute(err) => err.fmt(f),
            Self::Internal => f.write_str("the job that writes enabled to global.toml failed"),
            Self::OrphanStuck(_, reason) => f.write_str(reason),
            Self::Path(err) => err.fmt(f),
        }
    }
}

/// Starts, stops, and watches one database server: one per [`ServerKind`],
/// from `Daemon::server`. Its task, while it runs, is in `Daemon::procs`, so
/// a shutdown stops it, with the time its stop needs
/// (`ServiceHandle::stop_limit`).
#[derive(Debug)]
pub struct ServerManager {
    kind: ServerKind,
    /// Who runs under a pid and who holds a port. Tests put a fake here.
    pub(super) probes: Arc<dyn ProcessProbe>,
    /// How long a start waits for the server to take connections.
    ready_limit: Duration,
    /// The system whose programs run: [`Platform::current`].
    platform: Option<Platform>,
    /// How an old server that a crashed `marsad` left running is stopped
    /// before a start ([`orphan::clear`]): [`ServerKind::stop`], then
    /// SIGKILL.
    pub(super) orphan_ladder: StopMethod,
    /// The program of the last start, so an uninstall never deletes the
    /// version that runs. Use [`ServerManager::running_program`].
    started: Mutex<Option<PathBuf>>,
    /// The pid the run that started last wrote to `run/<id>.pid`, so its
    /// end deletes only its own record.
    recorded: Mutex<Arc<Mutex<Option<u32>>>>,
    /// The status and recent lines, kept across runs, so `seq` never goes
    /// back and the lines of a failed run stay readable.
    runtime: Arc<Mutex<ServiceRuntime>>,
    /// The start that runs now and the stops that wait for their turn, so
    /// a stop never waits for a start: a first start may run its init (for
    /// PostgreSQL `initdb`) for minutes.
    pub(super) starting: Mutex<Starting>,
    /// Held for the whole of each action, so two starts never run two
    /// servers. A tokio lock held across `.await` as a serialization token,
    /// not a guard of data (manager decision 2026-11-14). In an `Arc`, so a
    /// task can own its guard.
    pub(super) busy: Arc<tokio::sync::Mutex<()>>,
}

impl ServerManager {
    /// `kind` on the real system. Nothing is read or started.
    pub fn new(kind: ServerKind) -> Self {
        Self {
            kind,
            probes: Arc::new(SystemProbe),
            ready_limit: READY_LIMIT,
            platform: Platform::current(),
            orphan_ladder: kind.stop(),
            started: Mutex::default(),
            recorded: Mutex::default(),
            starting: Mutex::default(),
            runtime: Arc::new(Mutex::new(ServiceRuntime::new(kind.id()))),
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

/// How an action ended, for `enabled` in the server's table.
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
    /// The server program of each installed version, for `run/<id>.pid`.
    servers: Vec<PathBuf>,
    /// What a run that ends by itself is read for.
    refusal: Refusal,
    port: u16,
}

/// How a run that ended before it took connections says that a lock file
/// is held.
enum Refusal {
    /// PostgreSQL: its cluster, for its `postmaster.pid`, and its socket, for
    /// the name of its lock file (`None` for TCP only).
    Postgres {
        data: PathBuf,
        socket: Option<PathBuf>,
    },
    /// Not read: MySQL's own check before the start covers its data
    /// folder, and Valkey takes no lock on its own.
    None,
}

enum Ready {
    Yes,
    /// The run ended by itself first: the status says why.
    Ended,
    TimedOut,
}

impl ServerManager {
    pub fn kind(&self) -> ServerKind {
        self.kind
    }

    pub fn status(&self) -> ServiceStatus {
        lock(&self.runtime).status.clone()
    }

    /// The last lines, oldest first.
    pub fn recent_logs(&self) -> Vec<LogLine> {
        lock(&self.runtime).logs.recent()
    }

    /// The server's turn, if no action runs now. An uninstall holds it, so
    /// no start picks the version while its folder is moved aside.
    pub fn try_turn(&self) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        Arc::clone(&self.busy).try_lock_owned().ok()
    }

    /// Whether `GET /v1/services` lists the server: when a version is
    /// installed, or when its status says more than `stopped` (a start
    /// that failed because none is installed).
    pub fn listed(&self, data_dir: &Path) -> bool {
        self.status().state != ServiceState::Stopped || any_installed(self.kind, data_dir)
    }

    /// The program the server runs from now, or `None` while it does not
    /// run.
    pub fn running_program(&self, daemon: &Daemon) -> Option<PathBuf> {
        if !daemon.proc_alive(self.kind.id()) {
            return None;
        }
        lock(&self.started).clone()
    }

    /// Runs `action` and returns the status after it. An action that finds
    /// another one running waits up to `wait` for it in line, as for Caddy
    /// (`CaddyManager::run_waiting`). `start` on a running server and
    /// `stop` on a stopped one change nothing. Once the daemon begins to
    /// stop, nothing runs: the shutdown stops the server itself.
    ///
    /// A `stop` does not wait for a start (`Self::stop_waits`): it drops
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

    /// [`Self::run_waiting`], then `enabled` in the server's table as the
    /// action leaves it, still in this action's turn, so the writes keep the order of the
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

    /// `save`: the daemon, to write `enabled` with.
    async fn run(
        &self,
        daemon: &Daemon,
        action: ServiceAction,
        wait: Duration,
        save: Option<&Arc<Daemon>>,
    ) -> Result<ServiceStatus, ActionError> {
        let waiting = (action == ServiceAction::Stop).then(|| self.stop_waits());
        let busy = take_turn(&self.busy, &daemon.shutdown, wait, || {
            info!(%action, server = self.kind.id(), "another action runs; waiting for it");
        })
        .await
        .map_err(|Busy| ActionError::Busy)?;
        // This stop has its turn: starts after it in line run.
        drop(waiting);
        let outcome = self.act(daemon, action, &busy).await?;
        let enabled = match (action, outcome) {
            (ServiceAction::Stop, _) => Some(false),
            (_, Outcome::Done) => Some(true),
            (_, Outcome::Dropped) => None,
        };
        if let (Some(daemon), Some(enabled)) = (save, enabled) {
            save_enabled(self.kind, daemon, enabled).await?;
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
            info!(server = self.kind.id(), "a stop drops the start that runs");
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
                info!(
                    server = self.kind.id(),
                    "a stop waits in line; the start is dropped"
                );
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
                info!(server = self.kind.id(), "a stop dropped the start");
                return Ok(Outcome::Dropped);
            }
            () = daemon.shutdown.cancelled() => return Err(ActionError::Busy),
            run = self.start(daemon) => run?,
        };
        drop(clear);
        if let Run::TimedOut { port } = run {
            let server = self.kind.id();
            warn!(limit = ?self.ready_limit, server, "it takes no connections; stopping it");
            self.stop(daemon).await;
            let reason = format!(
                "it took no connections on 127.0.0.1:{port} within {} s",
                self.ready_limit.as_secs()
            );
            let code = match self.kind {
                ServerKind::Postgres => codes::POSTGRES_START_TIMEOUT,
                ServerKind::Mysql => codes::MYSQL_START_TIMEOUT,
                ServerKind::Valkey => codes::VALKEY_START_TIMEOUT,
            };
            self.fail(&daemon.events, code, &reason);
        }
        Ok(Outcome::Done)
    }

    async fn start(&self, daemon: &Daemon) -> Result<Run, ActionError> {
        // Our own supervisor runs it already. It holds the port and its pid
        // file, so the check before a start would call it busy.
        if daemon.proc_alive(self.kind.id()) {
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
            refusal,
            port,
        } = prepared;
        // This run's lines come after the ones there now.
        let first_seq = self.recent_logs().last().map_or(0, |line| line.seq + 1);
        self.spawn(daemon, spec)?;
        match self.wait_ready(daemon, port, &servers).await {
            Ready::Yes => {
                let server = self.kind.id();
                info!(port, server, "it takes connections");
                let slot = Arc::clone(&lock(&self.recorded));
                if process::DATABASE_ORPHAN_RECOVERY && lock(&slot).is_none() {
                    warn!(
                        server,
                        "it runs, but not as an installed server the system could tell, so its \
                         pid record was not written: a crash of marsad now leaves it unrecorded"
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
                if state == exited
                    && let Some((busy, reason)) = self.lock_refusal(first_seq, &refusal, port).await
                {
                    self.fail(&daemon.events, busy.code(), &reason);
                    return Err(ActionError::InUse(busy));
                }
                warn!(
                    ?state,
                    server = self.kind.id(),
                    "it ended before it was ready"
                );
            }
            Ready::TimedOut => return Ok(Run::TimedOut { port }),
        }
        Ok(Run::Over)
    }

    /// The version, the check before the start (ADR 0023, section 7), then
    /// the first init when the data folder is new. The check comes first,
    /// so a busy port makes no data folder.
    async fn prepare(&self, daemon: &Daemon) -> Result<Prepared, ActionError> {
        let kind = self.kind;
        let settings = read_settings(kind, &daemon.data_dir)?;
        let (platform, version, server) = self.installed(&daemon.data_dir, &settings)?;
        let port = settings.port.unwrap_or(kind.default_port());
        let servers = installed_servers(kind, &daemon.data_dir, platform);
        let ladder = &self.orphan_ladder;
        let probes = &*self.probes;
        let cleared = orphan::clear(kind, probes, &daemon.data_dir, &servers, ladder, port);
        match cleared.await {
            Ok(orphan::Cleared::Stopped { pid, signal }) => {
                let text = orphan::stopped_line(kind, pid, signal);
                lock(&self.runtime).note(&daemon.events, &text);
            }
            Ok(orphan::Cleared::Nothing | orphan::Cleared::Forgotten) => {}
            Err(orphan::Stuck(reason)) => return Err(ActionError::OrphanStuck(kind, reason)),
        }
        match kind {
            ServerKind::Postgres => {
                self.prepare_postgres(daemon, platform, &version, servers, port)
                    .await
            }
            ServerKind::Mysql => {
                self.prepare_mysql(daemon, &version, server, servers, port)
                    .await
            }
            ServerKind::Valkey => {
                self.prepare_valkey(daemon, &version, server, servers, port)
                    .await
            }
        }
    }

    /// The check before the start, from the server's own pid file
    /// `pid_file`, by `decide`.
    async fn check(
        &self,
        port: u16,
        pid_file: &Path,
        pid_of: fn(&str) -> Option<u32>,
        servers: &[PathBuf],
        decide: fn(&StartFacts) -> StartDecision,
    ) -> Result<(), ActionError> {
        let facts = start_facts(&*self.probes, port, pid_file, pid_of, servers).await;
        self.decide(&facts, decide)
    }

    /// `decide` on `facts`, the facts before the start.
    fn decide(
        &self,
        facts: &StartFacts,
        decide: fn(&StartFacts) -> StartDecision,
    ) -> Result<(), ActionError> {
        info!(?facts, server = self.kind.id(), "checked before the start");
        match decide(facts) {
            StartDecision::Refuse(busy) => Err(ActionError::InUse(busy)),
            StartDecision::Start => Ok(()),
        }
    }

    /// [`Self::prepare`] for PostgreSQL: the check, `initdb` for a new
    /// cluster, and its socket.
    async fn prepare_postgres(
        &self,
        daemon: &Daemon,
        platform: Platform,
        version: &Version,
        servers: Vec<PathBuf>,
        port: u16,
    ) -> Result<Prepared, ActionError> {
        let dirs = PostgresDirs::new(&daemon.data_dir, version.major);
        let pid_file = dirs.data.join("postmaster.pid");
        self.check(port, &pid_file, postmaster_pid, &servers, decide_start)
            .await?;
        let programs = postgres::programs(&daemon.data_dir, platform, version)
            .ok_or_else(|| self.missing(Some(&self.kind.line(version))))?;
        postgres::ensure_cluster(&programs, &dirs)
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
            lock(&self.runtime).note(&daemon.events, &text);
        }
        Ok(Prepared {
            spec: postgres::spec(programs.postgres, &dirs, port),
            servers,
            refusal: Refusal::Postgres {
                data: dirs.data,
                socket: socket.ok(),
            },
            port,
        })
    }

    /// [`Self::prepare`] for MySQL: `my.cnf`, the check (a pid that cannot
    /// be told refuses too: [`decide_mysql_start`]), and the first init
    /// for a new data folder.
    async fn prepare_mysql(
        &self,
        daemon: &Daemon,
        version: &Version,
        mysqld: PathBuf,
        servers: Vec<PathBuf>,
        port: u16,
    ) -> Result<Prepared, ActionError> {
        let kind = self.kind;
        let dirs = MysqlDirs::new(&daemon.data_dir, &kind.line(version));
        let socket = dirs.socket();
        let basedir = RuntimesDir::in_dir(&daemon.data_dir).version_dir(kind.runtime(), version);
        let text =
            my_cnf(&dirs, &basedir, port, socket.as_deref().ok()).map_err(ActionError::Path)?;
        let pid_file = dirs.pid_file();
        self.check(port, &pid_file, mysqld_pid, &servers, decide_mysql_start)
            .await?;
        mysql::write_config(&dirs, text)
            .await
            .map_err(|err| ActionError::Init(InitError::io(kind, "cannot write my.cnf", &err)))?;
        mysql::ensure_data_dir(&mysqld, &dirs)
            .await
            .map_err(ActionError::Init)?;
        info!(%version, port, "starting mysql");
        // Without a socket nobody reaches `root@localhost`, the account
        // with every privilege (ADR 0023, "Access").
        if let Err(why) = &socket {
            let folder = dirs.run.display();
            warn!(%folder, %why, "no Unix socket for mysql: TCP only, no root@localhost");
            let text = format!(
                "marsad: MySQL gets no Unix socket in {folder}: {why}. So it takes TCP \
                 connections only, and root@localhost cannot connect."
            );
            lock(&self.runtime).note(&daemon.events, &text);
        }
        Ok(Prepared {
            spec: mysql::spec(mysqld, &dirs),
            servers,
            refusal: Refusal::None,
            port,
        })
    }

    /// [`Self::prepare`] for Valkey: `valkey.conf`, and the check of its
    /// port. Valkey has no pid file of its own and no lock on its data
    /// folder, so the port is the whole check, after our own record of its
    /// pid ([`orphan::clear`]). A program that holds the port, Homebrew's
    /// Redis or Herd's, refuses the start with its name. No first init:
    /// Valkey starts on an empty folder.
    async fn prepare_valkey(
        &self,
        daemon: &Daemon,
        version: &Version,
        server: PathBuf,
        servers: Vec<PathBuf>,
        port: u16,
    ) -> Result<Prepared, ActionError> {
        let kind = self.kind;
        let dirs = ValkeyDirs::new(&daemon.data_dir);
        let text = valkey_conf(&dirs, port).map_err(ActionError::Path)?;
        let facts = port_facts(&*self.probes, port).await;
        self.decide(&facts, decide_start)?;
        valkey::write_config(&dirs, text).await.map_err(|err| {
            ActionError::Init(InitError::io(kind, "cannot write valkey.conf", &err))
        })?;
        info!(%version, port, "starting valkey");
        Ok(Prepared {
            spec: valkey::spec(server, &dirs, port),
            servers,
            refusal: Refusal::None,
            port,
        })
    }

    /// The server program to run for `settings`: the newest installed
    /// version of the `default` line, or of all lines when it is not set.
    pub(super) fn installed(
        &self,
        data_dir: &Path,
        settings: &ServerSettings,
    ) -> Result<(Platform, Version, PathBuf), ActionError> {
        let kind = self.kind;
        let platform = match self.platform {
            Some(platform) if !matches!(platform, Platform::WindowsX64 | Platform::Any) => platform,
            _ => return Err(ActionError::Unsupported(kind)),
        };
        let line = settings.default.as_deref();
        let installed = RuntimesDir::in_dir(data_dir)
            .installed(kind.runtime())
            .map_err(|err| ActionError::NotInstalled(kind, err.to_string()))?;
        let version = kind
            .line_version(line, &installed)
            .ok_or_else(|| self.missing(line))?;
        let server = super::init::server(kind, data_dir, platform, &version)
            .filter(|server| server.is_file())
            .ok_or_else(|| self.missing(line))?;
        Ok((platform, version, server))
    }

    /// No version of `line` (or of any line) is installed.
    fn missing(&self, line: Option<&str>) -> ActionError {
        let what = line.map_or_else(String::new, |line| format!(" {line}"));
        let reason = format!("no {}{what} is installed", self.kind.name());
        ActionError::NotInstalled(self.kind, reason)
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
        // The run's end deletes its `run/<id>.pid`, whatever ended it: a
        // stop, a shutdown, or the server itself. A server is never
        // restarted, so the run is the task.
        let ServiceHandle {
            cancel,
            task,
            runtime,
            stop_limit,
        } = handle;
        let data_dir = daemon.data_dir.clone();
        let kind = self.kind;
        let task = tokio::spawn(async move {
            // `stop_procs` aborts this task after its time limit; the abort
            // reaches the supervisor too, so its `Child` is dropped.
            let _abort = AbortOnDrop(task.abort_handle());
            if let Err(err) = task.await {
                warn!(%err, server = kind.id(), "the server task failed");
            }
            if let Some(pid) = *lock(&recorded) {
                orphan::forget_run(kind, &data_dir, pid);
            }
        });
        let handle = ServiceHandle {
            cancel,
            task,
            runtime,
            stop_limit,
        };
        // A handle this replaces belongs to a run that has ended.
        daemon.add_proc(self.kind.id(), handle);
        Ok(())
    }

    /// Asks `127.0.0.1:<port>` ([`Self::answers`]) until our server answers,
    /// its run ends, or the limit. The check before the start found the port
    /// free, so what answers is our server.
    /// Its pid goes to `run/<id>.pid` as soon as it runs one of `servers`.
    async fn wait_ready(&self, daemon: &Daemon, port: u16, servers: &[PathBuf]) -> Ready {
        let mut recorded = false;
        let kind = self.kind;
        let poll = async {
            loop {
                if !daemon.proc_alive(kind.id()) {
                    return Ready::Ended;
                }
                if !recorded && let Some(pid) = self.status().pid {
                    let probes = &*self.probes;
                    recorded = orphan::record(kind, probes, &daemon.data_dir, servers, pid).await;
                    if recorded {
                        let slot = Arc::clone(&lock(&self.recorded));
                        *lock(&slot) = Some(pid);
                    }
                }
                if self.answers(port).await {
                    return Ready::Yes;
                }
                tokio::time::sleep(READY_POLL).await;
            }
        };
        tokio::time::timeout(self.ready_limit, poll)
            .await
            .unwrap_or(Ready::TimedOut)
    }

    /// Whether the server takes connections on `127.0.0.1:<port>`, by its
    /// own protocol ([`postgres::answers`]).
    async fn answers(&self, port: u16) -> bool {
        match self.kind {
            ServerKind::Postgres => postgres::answers(port).await,
            ServerKind::Mysql => mysql::answers(port).await,
            ServerKind::Valkey => valkey::answers(port).await,
        }
    }

    /// Why a run that ended by itself before it took connections did not
    /// start, when the server refused because a live process holds one of
    /// its lock files ([`postgres::lock_refusal`]), from this run's stderr
    /// lines, from `first_seq`.
    async fn lock_refusal(
        &self,
        first_seq: u64,
        refusal: &Refusal,
        port: u16,
    ) -> Option<(ServiceBusy, String)> {
        let recent = self.recent_logs();
        let stderr: Vec<&str> = recent
            .iter()
            .filter(|line| line.seq >= first_seq && line.stream == LogStream::Stderr)
            .map(|line| line.text.as_str())
            .collect();
        match refusal {
            Refusal::Postgres { data, socket } => {
                let socket = socket.as_deref();
                postgres::lock_refusal(&*self.probes, &stderr, data, socket, port).await
            }
            Refusal::None => None,
        }
    }

    /// Stops the task by the spec (for PostgreSQL SIGINT, then SIGQUIT),
    /// then a kill. When this daemon runs none, our old server that an
    /// earlier `marsad` left running is stopped instead ([`orphan::clear`]).
    /// If that one cannot be stopped, the recent lines say why; the next
    /// start refuses then. Any other server is never touched.
    async fn stop(&self, daemon: &Daemon) {
        let kind = self.kind;
        // `None`: nothing runs, or a shutdown took the handle and stops it.
        if let Some(handle) = daemon.take_proc(kind.id()) {
            if let Err(err) = handle.stop().await {
                warn!(%err, server = kind.id(), "the server task failed");
            }
            return;
        }
        if daemon.shutdown.is_cancelled() {
            return;
        }
        let Some(platform) = self.platform else {
            return;
        };
        let port = read_settings(kind, &daemon.data_dir)
            .ok()
            .and_then(|settings| settings.port)
            .unwrap_or(kind.default_port());
        let servers = installed_servers(kind, &daemon.data_dir, platform);
        let ladder = &self.orphan_ladder;
        let probes = &*self.probes;
        let cleared = orphan::clear(kind, probes, &daemon.data_dir, &servers, ladder, port);
        match cleared.await {
            Ok(orphan::Cleared::Stopped { pid, signal }) => {
                let text = orphan::stopped_line(kind, pid, signal);
                lock(&self.runtime).note(&daemon.events, &text);
            }
            Ok(orphan::Cleared::Nothing | orphan::Cleared::Forgotten) => {}
            Err(orphan::Stuck(reason)) => {
                warn!(%reason, server = kind.id(), "our old server did not stop");
                lock(&self.runtime).note(&daemon.events, &format!("marsad: {reason}."));
            }
        }
    }

    /// Says why in the recent lines, and sets `Failed { code }`. Only while
    /// no task runs: the supervisor owns the status then.
    pub(super) fn fail(&self, events: &broadcast::Sender<Event>, code: &str, reason: &str) {
        warn!(code, %reason, server = self.kind.id(), "it was not started");
        let text = format!("marsad: {} was not started: {reason}.", self.kind.name());
        lock(&self.runtime).note(events, &text);
        lock(&self.runtime).set_failed(events, code);
    }
}
