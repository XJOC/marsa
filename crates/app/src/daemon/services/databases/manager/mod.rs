//! [`ServerManager`]: starts and stops one database server under the
//! supervisor, one action at a time.
//!
//! One file per job: `prepare` (the version, the check before a start, and
//! the first init), `run` (one run under the supervisor), and `error`
//! ([`ActionError`]).

mod error;
mod prepare;
mod run;

pub use error::ActionError;

use marsa_core::codes;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use marsa_core::databases::ServerKind;
use marsa_core::platform::process;
use marsa_core::recipes::Platform;
use marsa_core::services::StopMethod;
use marsa_core::types::{Event, LogLine, ServiceAction, ServiceId, ServiceState, ServiceStatus};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::init::{installed_programs, installed_servers};
use super::orphan;
use super::settings::{any_installed, read_settings, save_enabled};
use crate::daemon::services::actions::{Busy, take_turn};
use crate::daemon::services::probe::{ProcessProbe, SystemProbe};
use crate::daemon::services::supervisor::{EXITED, ServiceRuntime, lock};
use crate::daemon::state::Daemon;
use prepare::Prepared;
use run::Ready;

/// How long a start waits for the server to take connections. S-12
/// measured 50 to 690 ms. The postmaster answers before it recovers its data
/// after a crash, so a long recovery does not count against this.
const READY_LIMIT: Duration = Duration::from_secs(15);
/// MySQL's: its InnoDB start can take 20 s or more on a slow disk, after a
/// forced kill (Windows, where the servers end with `marsad`) or on a first
/// start after an install. The server is still starting then, not failing.
const MYSQL_READY_LIMIT: Duration = Duration::from_secs(60);
const READY_POLL: Duration = Duration::from_millis(100);

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
    /// Whether `marsad` runs with an administrator's token, asked at each
    /// start ([`refuses_elevation`]): `platform::is_elevated`. Tests put an
    /// answer here, never the runner's own.
    pub(super) elevated: fn() -> bool,
    /// The program of the last start, so an uninstall never deletes the
    /// version that runs. Use [`ServerManager::running_program`].
    started: Mutex<Option<PathBuf>>,
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
            ready_limit: match kind {
                ServerKind::Mysql => MYSQL_READY_LIMIT,
                ServerKind::Postgres | ServerKind::Valkey => READY_LIMIT,
            },
            platform: Platform::current(),
            orphan_ladder: kind.stop(),
            elevated: marsa_core::platform::is_elevated,
            started: Mutex::default(),
            starting: Mutex::default(),
            runtime: Arc::new(Mutex::new(ServiceRuntime::new(ServiceId::Database(kind)))),
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
        if !daemon.proc_alive(ServiceId::Database(self.kind)) {
            return None;
        }
        lock(&self.started).clone()
    }

    /// [`Self::running_program`], or else the installed program that our
    /// old server runs, which a crashed `marsad` left running, or may run
    /// when the system cannot tell (`orphan::orphan_program`), for an
    /// uninstall. Removing that version would make its record name no
    /// installed server, so the next start would drop the record and could
    /// never stop it.
    pub async fn program_in_use(&self, daemon: &Daemon) -> Option<PathBuf> {
        if let Some(program) = self.running_program(daemon) {
            return Some(program);
        }
        let platform = self.platform?;
        let programs = installed_programs(self.kind, &daemon.data_dir, platform);
        orphan::orphan_program(self.kind, &*self.probes, &daemon.data_dir, &programs).await
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
        if daemon.proc_alive(ServiceId::Database(self.kind)) {
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
        let recorded = self.spawn(daemon, spec, servers)?;
        match self.wait_ready(daemon, port).await {
            Ready::Yes => {
                let server = self.kind.id();
                info!(port, server, "it takes connections");
                // The record comes first, so a crash of `marsad` once a
                // start said "running" leaves the server recorded.
                let settled = orphan::settled(recorded).await;
                if process::DATABASE_ORPHAN_RECOVERY && settled == orphan::Recorded::Never {
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

    /// Stops the task by the spec (for PostgreSQL SIGINT, then SIGQUIT),
    /// then a kill. When this daemon runs none, our old server that an
    /// earlier `marsad` left running is stopped instead ([`orphan::clear`]),
    /// with a few looks when the system cannot tell who runs its pid. If it
    /// cannot be stopped or told, the recent lines say why, and the stop
    /// still ends as one: the next start refuses while it holds the port.
    /// Any other server is never touched.
    async fn stop(&self, daemon: &Daemon) {
        let kind = self.kind;
        // `None`: nothing runs, or a shutdown took the handle and stops it.
        if let Some(handle) = daemon.take_proc(ServiceId::Database(kind)) {
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
        let looks = orphan::STOP_LOOKS;
        let cleared = orphan::clear(
            kind,
            probes,
            &daemon.data_dir,
            &servers,
            ladder,
            port,
            looks,
        );
        match cleared.await {
            Ok(orphan::Cleared::Stopped { pid, signal }) => {
                let text = orphan::stopped_line(kind, pid, signal);
                lock(&self.runtime).note(&daemon.events, &text);
            }
            Ok(orphan::Cleared::Nothing | orphan::Cleared::Forgotten) => {}
            Ok(orphan::Cleared::Unknown { pid }) => {
                let text = orphan::unknown_line(kind, pid);
                lock(&self.runtime).note(&daemon.events, &text);
            }
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
