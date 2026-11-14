//! Starts Caddy through the supervisor and talks to its admin API. Task M3-07.
//!
//! `marsa_core::caddy` builds the config as data. This module writes it to
//! `caddy/caddy.json`, runs the newest installed Caddy
//! (`runtimes/caddy/<version>/caddy run --config caddy/caddy.json`) under the
//! supervisor, and waits until the admin API answers. Like a Laravel job
//! that starts a queue worker, then checks that it is healthy.
//!
//! Before each start, [`orphan`] stops a Caddy that a crashed daemon left
//! behind, or refuses to start when another program holds our ports. A
//! `stop` with no Caddy running stops such an orphan too. After each spawn
//! the watcher writes `run/caddy.pid`, and removes it once Caddy has exited
//! (ADR 0009). The `admin` object in `caddy.json` carries this data folder's
//! tag, `marsa-instance-<id>`, so the daemon of another folder never takes
//! our Caddy for its own.
//!
//! The config is always `sites_config` of the sites and PHP pools that the
//! sync task (`sites_sync`) last gave [`CaddyManager::apply`]. While Caddy
//! runs, each change goes to its admin API (`POST /load`), so Caddy changes
//! without a restart (task M4-11). The ports stay the same: no new listener.
//!
//! When `marsad` starts, [`start_at_launch`] starts an installed Caddy in the
//! background, through the same start (task M5-16).

pub mod orphan;

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use marsa_core::caddy::{CaddyError, CaddyInput, CaddyPaths, PhpPools, caddy_env, sites_config};
use marsa_core::orphan::instance_tag;
use marsa_core::platform::process;
use marsa_core::services::{ProgramNotAbsolute, RestartPolicy, ServiceSpec};
use marsa_core::sites::Site;
use marsa_core::types::{Event, LogLine, LogStream, ServiceAction, ServiceState, ServiceStatus};
use reqwest::redirect;
use serde_json::Value;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::task::{AbortHandle, JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use self::orphan::{Blocked, Ladder, Orphans, PidFile, Processes, SystemProcesses};
use super::state::Daemon;
use super::supervisor::{ServiceHandle, ServiceRuntime, Supervisor, lock, now_ms};

/// The service id: the key in `Daemon::procs`, the `{id}` in the routes, and
/// the log file `logs/caddy.log`.
pub const CADDY_ID: &str = "caddy";

/// How long `start` waits for the admin API by default.
const READY_LIMIT: Duration = Duration::from_secs(5);
const READY_POLL: Duration = Duration::from_millis(100);
/// For each admin API call, `/stop` included.
const ADMIN_TIMEOUT: Duration = Duration::from_secs(1);
/// For `POST /load`: Caddy answers once the new config runs.
const LOAD_TIMEOUT: Duration = Duration::from_secs(5);

/// How Caddy and each php-fpm pool restart after a failure: up to 3 times,
/// after 0.5 s, then twice as long each time, at most 5 s.
pub const RESTART: RestartPolicy = RestartPolicy::OnFailure {
    max_retries: 3,
    initial: Duration::from_millis(500),
    max: Duration::from_secs(5),
};
/// How long a stop of Caddy or of a php-fpm pool waits after SIGTERM.
pub const STOP_GRACE: Duration = Duration::from_secs(2);

/// What Caddy prints when an address is taken: Go's text on macOS and Linux,
/// and the two English Windows texts. Windows says "forbidden by its access
/// permissions" when the holder took the port for itself alone, as IIS does
/// with port 80 (spike S-11). Compared in lower case.
const PORT_IN_USE: [&str; 3] = [
    "address already in use",
    "only one usage of each socket address",
    "forbidden by its access permissions",
];

/// How the supervisor runs Caddy: the newest installed Caddy
/// ([`CaddyPaths::program`]) with `run --config caddy/caddy.json`, and
/// Caddy's own data inside `caddy/`.
///
/// # Errors
///
/// The error of [`CaddyPaths::program`]: `caddy_not_installed` when
/// `runtimes/caddy` has no version.
pub fn caddy_spec(paths: &CaddyPaths) -> Result<ServiceSpec, CaddyError> {
    Ok(caddy_spec_for(paths, paths.program()?))
}

/// [`caddy_spec`] with `program` as Caddy. Tests run a fake this way.
pub fn caddy_spec_for(paths: &CaddyPaths, program: PathBuf) -> ServiceSpec {
    ServiceSpec {
        id: CADDY_ID.to_owned(),
        program,
        // `to_string_lossy` as in `caddy_env`: the app data dir comes from the
        // OS and is valid Unicode in practice.
        args: vec![
            "run".to_owned(),
            "--config".to_owned(),
            paths.config_file.to_string_lossy().into_owned(),
        ],
        env: caddy_env(paths),
        cwd: None,
        restart: RESTART,
        stop_grace: STOP_GRACE,
    }
}

/// Why a service action failed. The route turns it into an `ErrorBody`.
#[derive(Debug)]
pub enum ActionError {
    /// [`CaddyPaths::program`] found no Caddy to run: no version in
    /// `runtimes/caddy`, or, rarely, no Caddy for this system or a broken
    /// recipe.
    NoCaddy(CaddyError),
    /// The program to run is not a file: a version folder without its
    /// `caddy`.
    NotInstalled(PathBuf),
    /// Another action on the service has not finished.
    Busy,
    /// `caddy.json` cannot be written.
    ConfigWrite(io::Error),
    ProgramNotAbsolute(ProgramNotAbsolute),
}

impl ActionError {
    /// Stable error name for the UI. Every [`ActionError::NoCaddy`] is
    /// `caddy_not_installed`, the code the CLI and GUI know, with
    /// `marsa install caddy` as the fix; its text keeps the exact reason.
    pub fn code(&self) -> &'static str {
        match self {
            ActionError::NoCaddy(_) | ActionError::NotInstalled(_) => "caddy_not_installed",
            ActionError::Busy => "service_busy",
            ActionError::ConfigWrite(_) => "caddy_config_write",
            ActionError::ProgramNotAbsolute(err) => err.code(),
        }
    }
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActionError::NoCaddy(err) => err.fmt(f),
            ActionError::NotInstalled(path) => write!(f, "no Caddy at {}", path.display()),
            ActionError::Busy => f.write_str("another action on this service is running"),
            ActionError::ConfigWrite(err) => write!(f, "cannot write caddy.json: {err}"),
            ActionError::ProgramNotAbsolute(err) => err.fmt(f),
        }
    }
}

/// Starts, stops, and watches the one Caddy. It lives in `Daemon::caddy`; its
/// task, while it runs, is in `Daemon::procs`, so a shutdown stops it.
#[derive(Debug)]
pub struct CaddyManager {
    pub paths: CaddyPaths,
    /// The ports in `caddy.json`. `start` and `stop` call the admin port.
    pub input: CaddyInput,
    /// Whether Caddy may take ports 80 and 443 itself, asked again for every
    /// config (Linux, ADR 0014). Never here, so tests never depend on the
    /// machine they run on; `server::run` sets
    /// [`marsa_core::platform::low_ports::low_ports_allowed`].
    pub low_ports: fn() -> bool,
    /// How Caddy runs. `None`, the default: [`caddy_spec`], built again at
    /// each start, so the newest Caddy installed runs, also one installed
    /// while `marsad` runs. Tests put a fake program here.
    pub spec: Option<ServiceSpec>,
    /// How long `start` waits for the admin API.
    pub ready_limit: Duration,
    /// Who runs under a pid, and signals, for the orphan check. Tests put a
    /// recorder here.
    pub processes: Arc<dyn Processes>,
    /// How long each step of an orphan's stop waits.
    pub ladder: Ladder,
    /// The program of the last start, so an uninstall never deletes the
    /// Caddy that runs (task M5-07). Use [`CaddyManager::running_program`].
    pub started: Mutex<Option<PathBuf>>,
    /// The status and recent lines. Kept across runs, so `seq` never goes
    /// back and the lines of a failed run stay readable.
    runtime: Arc<Mutex<ServiceRuntime>>,
    /// Held for the whole of each action. [`CaddyManager::run`] only tries
    /// it, [`CaddyManager::run_waiting`] waits for it in line. A tokio lock:
    /// it is held across `.await`, and it serves the waits in order.
    busy: tokio::sync::Mutex<()>,
    /// The sites and PHP pools of the config. [`CaddyManager::apply`] sets
    /// them; `start` writes the config of the latest ones.
    wanted: Mutex<Wanted>,
    /// Held while a config goes to Caddy: by each `apply`, and by `start`
    /// from writing `caddy.json` until Caddy is ready. So configs reach
    /// Caddy in order, and never a Caddy that is still starting. A tokio
    /// lock: it is held across `.await`.
    pushing: tokio::sync::Mutex<()>,
}

/// What the config is built from, besides the ports.
#[derive(Debug, Default)]
struct Wanted {
    sites: Vec<Site>,
    pools: PhpPools,
}

/// Where [`CaddyManager::apply`] put the new config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// Caddy runs it now.
    Loaded,
    /// Caddy does not run: it reads `caddy.json` when it starts.
    Written,
}

/// Why a new config did not reach Caddy. Caddy keeps its old config then.
#[derive(Debug)]
pub enum LoadError {
    /// The config cannot be built.
    Config(CaddyError),
    /// `caddy.json` or the instance id cannot be written.
    Write(io::Error),
    /// Caddy cannot listen on an address of the new config: another program
    /// holds it, or it is not allowed (ADR 0008, finding 11). `address` comes
    /// from `listening on <addr>:` in Caddy's answer.
    PortInUse {
        address: Option<String>,
        message: String,
    },
    /// Caddy refused the config for another reason.
    Rejected { status: u16, message: String },
    /// The admin API did not answer.
    Unreachable(String),
}

impl LoadError {
    /// Stable error name for the logs and the UI.
    pub fn code(&self) -> &'static str {
        match self {
            LoadError::Config(err) => err.code(),
            LoadError::Write(_) => "caddy_config_write",
            LoadError::PortInUse { .. } => orphan::PORT_IN_USE,
            LoadError::Rejected { .. } => "caddy_config_rejected",
            LoadError::Unreachable(_) => "caddy_admin_unreachable",
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Config(err) => err.fmt(f),
            LoadError::Write(err) => write!(f, "cannot write caddy.json: {err}"),
            LoadError::PortInUse {
                address: Some(address),
                ..
            } => write!(
                f,
                "Caddy cannot listen on {address}: it is in use or not allowed"
            ),
            LoadError::PortInUse { address: None, .. } => {
                f.write_str("Caddy cannot listen on an address: it is in use or not allowed")
            }
            LoadError::Rejected { status, message } => {
                write!(f, "Caddy refused the config ({status}): {message}")
            }
            LoadError::Unreachable(err) => write!(f, "Caddy's admin API did not answer: {err}"),
        }
    }
}

impl CaddyManager {
    /// Caddy in `data_dir`, on Marsa's ports. Nothing is read or started.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            paths: CaddyPaths::in_dir(data_dir),
            spec: None,
            input: CaddyInput::default(),
            low_ports: || false,
            ready_limit: READY_LIMIT,
            processes: Arc::new(SystemProcesses),
            ladder: Ladder::default(),
            started: Mutex::default(),
            runtime: Arc::new(Mutex::new(ServiceRuntime::new(CADDY_ID))),
            busy: tokio::sync::Mutex::default(),
            wanted: Mutex::default(),
            pushing: tokio::sync::Mutex::default(),
        }
    }

    /// Makes `sites` and `pools` the config: writes `caddy.json`, and while
    /// Caddy runs, sends it to the admin API (`POST /load`), so Caddy changes
    /// without a restart. A Caddy that does not run reads the file when it
    /// starts. Calls run one at a time, in order, so the last one wins.
    ///
    /// # Errors
    ///
    /// See [`LoadError`]. A refused config is also noted in Caddy's recent
    /// lines, so `marsa logs caddy` shows why.
    pub async fn apply(
        &self,
        daemon: &Daemon,
        sites: Vec<Site>,
        pools: PhpPools,
    ) -> Result<Applied, LoadError> {
        let _pushing = self.pushing.lock().await;
        *lock(&self.wanted) = Wanted { sites, pools };
        let tag = self.instance_tag().map_err(LoadError::Write)?;
        let config = self.config(&tag).map_err(LoadError::Config)?;
        write_config(&self.paths.config_file, &config)
            .await
            .map_err(LoadError::Write)?;
        if !daemon.proc_alive(CADDY_ID) {
            return Ok(Applied::Written);
        }
        match Admin::new(self.input.admin_port).load(&config).await {
            Ok(()) => Ok(Applied::Loaded),
            // Caddy stopped in the meantime: it reads the file when it starts.
            Err(LoadError::Unreachable(_)) if !daemon.proc_alive(CADDY_ID) => Ok(Applied::Written),
            Err(err) => {
                let text = format!("marsad: The new config was not loaded: {err}.");
                note(&self.runtime, &daemon.events, &text);
                Err(err)
            }
        }
    }

    /// The ports of the next config: those of `input`, or 80 and 443 where
    /// the system now lets Caddy bind them. Linux only: the helper's
    /// `ports_install` allows it at setup, so the next config after setup
    /// moves a running Caddy to 80 and 443, with no restart.
    ///
    /// The orphan check still looks at the ports of `input`. It runs on
    /// macOS only; a Linux orphan check (spike S-06b) must ask here.
    pub fn ports(&self) -> CaddyInput {
        if (self.low_ports)() {
            self.input.on_low_ports()
        } else {
            self.input
        }
    }

    /// The config of the latest sites and pools, with `tag` on `admin`.
    fn config(&self, tag: &str) -> Result<Value, CaddyError> {
        let mut config = {
            let wanted = lock(&self.wanted);
            sites_config(&self.ports(), &wanted.sites, &wanted.pools)?
        };
        tag_config(&mut config, tag);
        Ok(config)
    }

    /// Whether there is a Caddy to run: the check `start` makes first, here
    /// without marking the status failed. Setup asks it before it starts
    /// Caddy to make the CA (task M5-15).
    pub fn installed(&self) -> bool {
        self.spec_to_run().is_ok_and(|spec| spec.program.is_file())
    }

    /// The spec to run now: the fake of a test, else [`caddy_spec`].
    fn spec_to_run(&self) -> Result<ServiceSpec, CaddyError> {
        match &self.spec {
            Some(spec) => Ok(spec.clone()),
            None => caddy_spec(&self.paths),
        }
    }

    pub fn status(&self) -> ServiceStatus {
        lock(&self.runtime).status.clone()
    }

    /// The program Caddy runs from now, or `None` while it does not run.
    pub fn running_program(&self, daemon: &Daemon) -> Option<PathBuf> {
        if !daemon.proc_alive(CADDY_ID) {
            return None;
        }
        lock(&self.started).clone()
    }

    /// The last lines, oldest first.
    pub fn recent_logs(&self) -> Vec<LogLine> {
        lock(&self.runtime).logs.recent()
    }

    /// Runs `action` and returns the status after it. `start` on a running
    /// Caddy and `stop` on a stopped one change nothing. `restart` is `stop`,
    /// then `start`.
    ///
    /// # Errors
    ///
    /// `service_busy` while another action runs, or another waits for its
    /// turn: one action at a time keeps two starts from running two Caddys.
    /// `caddy_not_installed`, `caddy_config_write`, and `program_not_absolute`
    /// from `start`: the status is then `Failed` with the same code.
    pub async fn run(
        &self,
        daemon: &Daemon,
        action: ServiceAction,
    ) -> Result<ServiceStatus, ActionError> {
        let busy = self.busy.try_lock().map_err(|_| ActionError::Busy)?;
        self.act(daemon, action, busy).await
    }

    /// [`CaddyManager::run`], but an action that finds another one running
    /// waits up to `wait` for it to end, then runs. Waits end in the order
    /// they began. So a `stop` sent while the daemon starts Caddy (task
    /// M5-16) stops Caddy once it has started, and is never lost.
    ///
    /// # Errors
    ///
    /// `service_busy` when the wait runs out; the others as for
    /// [`CaddyManager::run`].
    pub async fn run_waiting(
        &self,
        daemon: &Daemon,
        action: ServiceAction,
        wait: Duration,
    ) -> Result<ServiceStatus, ActionError> {
        let busy = match self.busy.try_lock() {
            Ok(busy) => busy,
            Err(_) => {
                info!(%action, "another action on caddy runs; waiting for it");
                tokio::time::timeout(wait, self.busy.lock())
                    .await
                    .map_err(|_| ActionError::Busy)?
            }
        };
        self.act(daemon, action, busy).await
    }

    /// Runs `action`. `_busy` proves that no other action runs; it is let go
    /// when this ends, also when the caller is dropped half way.
    async fn act(
        &self,
        daemon: &Daemon,
        action: ServiceAction,
        _busy: tokio::sync::MutexGuard<'_, ()>,
    ) -> Result<ServiceStatus, ActionError> {
        let admin = Admin::new(self.input.admin_port);
        match action {
            ServiceAction::Start => self.start(daemon, &admin).await?,
            ServiceAction::Stop => {
                self.stop(daemon, &admin).await;
            }
            ServiceAction::Restart => {
                // An orphan that would not stop now would not stop for the
                // start either: the status already says why.
                if self.stop(daemon, &admin).await {
                    self.start(daemon, &admin).await?;
                }
            }
        }
        Ok(self.status())
    }

    async fn start(&self, daemon: &Daemon, admin: &Admin) -> Result<(), ActionError> {
        if daemon.proc_alive(CADDY_ID) {
            return Ok(());
        }
        // No task runs from here on, so these writes cannot race the supervisor.
        let spec = match self.spec_to_run() {
            Ok(spec) if spec.program.is_file() => spec,
            Ok(spec) => {
                self.fail("caddy_not_installed", &daemon.events);
                return Err(ActionError::NotInstalled(spec.program));
            }
            Err(err) => {
                warn!(code = err.code(), %err, "no caddy to run");
                self.fail("caddy_not_installed", &daemon.events);
                return Err(ActionError::NoCaddy(err));
            }
        };
        // The tag goes into `caddy.json`, and the check below looks for it.
        let tag = match self.instance_tag() {
            Ok(tag) => tag,
            Err(err) => {
                self.fail("caddy_config_write", &daemon.events);
                return Err(ActionError::ConfigWrite(err));
            }
        };
        // ADR 0009: stop our own orphan; touch nothing that is not ours.
        let pid_file = PidFile::new(&daemon.data_dir, &self.our_programs(&spec.program));
        {
            let orphans = self.orphans(&pid_file, admin, &tag);
            if let Some(found) = orphans.inspect().await {
                if found.is_reap() {
                    note(&self.runtime, &daemon.events, STOPPING_ORPHAN);
                }
                if let Err(blocked) = orphans.clear_for_start(found).await {
                    self.blocked("Caddy was not started", &blocked, &daemon.events);
                    return Ok(());
                }
            }
        }
        // Held until the end of the start, so `apply` waits for a Caddy that
        // answers.
        let _pushing = self.pushing.lock().await;
        let config = match self.config(&tag) {
            Ok(config) => config,
            Err(err) => {
                self.fail("caddy_config_write", &daemon.events);
                let err = io::Error::new(io::ErrorKind::InvalidData, err);
                return Err(ActionError::ConfigWrite(err));
            }
        };
        if let Err(err) = write_config(&self.paths.config_file, &config).await {
            self.fail("caddy_config_write", &daemon.events);
            return Err(ActionError::ConfigWrite(err));
        }
        // Before the process starts, so the watcher cannot miss its first line.
        let lines = daemon.events.subscribe();
        let first_seq = self.recent_logs().last().map_or(0, |line| line.seq + 1);
        let spec = ServiceSpec {
            id: CADDY_ID.to_owned(),
            ..spec
        };
        let logs_dir = daemon.data_dir.join("logs");
        let runtime = Arc::clone(&self.runtime);
        *lock(&self.started) = Some(spec.program.clone());
        let handle = Supervisor::spawn_on(spec, daemon.events.clone(), &logs_dir, runtime)
            .map_err(|err| {
                self.fail(err.code(), &daemon.events);
                ActionError::ProgramNotAbsolute(err)
            })?;

        let ServiceHandle {
            cancel,
            task,
            runtime,
        } = handle;
        let watcher = Watcher {
            lines,
            first_seq,
            cancel: cancel.clone(),
            runtime: Arc::clone(&runtime),
            events: daemon.events.clone(),
            pid_file,
            processes: Arc::clone(&self.processes),
        };
        let task = tokio::spawn(watcher.run(task));
        // A handle this replaces belongs to a run that has ended.
        daemon.add_proc(
            CADDY_ID,
            ServiceHandle {
                cancel,
                task,
                runtime,
            },
        );

        info!("caddy started; waiting for its admin API");
        match self.wait_ready(daemon, admin, &tag).await {
            Ready::Yes => info!("caddy is ready"),
            // The status already says why.
            Ready::Ended => warn!(state = ?self.status().state, "caddy ended before it was ready"),
            Ready::TimedOut => {
                warn!(limit = ?self.ready_limit, "caddy's admin API did not answer; stopping it");
                self.stop(daemon, admin).await;
                // A port conflict found at the same moment says more: keep it.
                if self.status().state == ServiceState::Stopped {
                    self.fail("caddy_start_timeout", &daemon.events);
                }
            }
        }
        Ok(())
    }

    /// Polls `GET /id/<tag>` until our Caddy answers, the task ends, or the
    /// limit. Only our tag counts: another program on the admin port, a
    /// second user's Caddy for one, is never taken for ours, so no config
    /// goes to it. Where no orphan check runs first (Linux and Windows) this
    /// is the only check.
    async fn wait_ready(&self, daemon: &Daemon, admin: &Admin, tag: &str) -> Ready {
        let poll = async {
            loop {
                if !daemon.proc_alive(CADDY_ID) {
                    return Ready::Ended;
                }
                if admin.answers_as(tag).await {
                    return Ready::Yes;
                }
                tokio::time::sleep(READY_POLL).await;
            }
        };
        tokio::time::timeout(self.ready_limit, poll)
            .await
            .unwrap_or(Ready::TimedOut)
    }

    /// Asks Caddy to exit through its admin API (the clean way on Windows,
    /// which has no SIGTERM), then stops its task. With no task, stops our
    /// orphan, if there is one. `false` when that orphan would not stop.
    ///
    /// `POST /stop` goes only to a Caddy that answers with our tag: another
    /// program on the admin port never gets it. Without it, the task's stop
    /// ends our Caddy.
    async fn stop(&self, daemon: &Daemon, admin: &Admin) -> bool {
        if !daemon.proc_alive(CADDY_ID) {
            return self.stop_orphan(daemon, admin).await;
        }
        match self.instance_tag() {
            Ok(tag) if admin.answers_as(&tag).await => admin.stop().await,
            Ok(_) => debug!("the admin port does not answer with our tag; no /stop"),
            Err(err) => warn!(%err, "cannot read the instance id; no /stop"),
        }
        // `None`: a shutdown took the handle and stops it.
        if let Some(handle) = daemon.take_proc(CADDY_ID)
            && let Err(err) = handle.stop().await
        {
            warn!(%err, "the caddy task failed");
        }
        true
    }

    /// The Caddy programs an orphan of this data folder may run: `program`
    /// (the one this start uses) and every installed version, because an
    /// orphan left by a crash may be older than the newest version (ADR 0009).
    /// A failure to list the versions leaves only `program`: fewer paths
    /// means fewer processes that can ever be signalled.
    fn our_programs(&self, program: &Path) -> Vec<PathBuf> {
        let mut programs = self.paths.installed_programs().unwrap_or_else(|err| {
            warn!(%err, "cannot list the installed Caddy versions");
            Vec::new()
        });
        if !programs.iter().any(|known| known == program) {
            programs.push(program.to_path_buf());
        }
        programs
    }

    /// Stops our orphan from a crashed daemon, and nothing else: a conflict
    /// or another Marsa's Caddy is left alone, and so is a stale pid file.
    /// `false` when the orphan would not stop.
    async fn stop_orphan(&self, daemon: &Daemon, admin: &Admin) -> bool {
        // No check on this OS, so no instance id file either. Without the
        // program there is no Caddy of ours to recognize.
        if !process::ORPHAN_RECOVERY {
            return true;
        }
        let program = match self.spec_to_run() {
            Ok(spec) if spec.program.is_file() => spec.program,
            _ => return true,
        };
        let tag = match self.instance_tag() {
            Ok(tag) => tag,
            Err(err) => {
                warn!(%err, "cannot read the instance id; not looking for an orphan");
                return true;
            }
        };
        let pid_file = PidFile::new(&daemon.data_dir, &self.our_programs(&program));
        let orphans = self.orphans(&pid_file, admin, &tag);
        let Some(found) = orphans.inspect().await else {
            return true;
        };
        if !found.is_reap() {
            return true;
        }
        note(&self.runtime, &daemon.events, STOPPING_ORPHAN);
        match orphans.reap(found).await {
            Ok(()) => true,
            Err(blocked) => {
                self.blocked("The old Caddy was not stopped", &blocked, &daemon.events);
                false
            }
        }
    }

    /// This data folder's admin tag, `marsa-instance-<id>`. The id is made
    /// once, in `caddy/instance-id`.
    fn instance_tag(&self) -> io::Result<String> {
        let id = process::instance_id(&self.paths.home.join("instance-id"))?;
        Ok(instance_tag(&id))
    }

    /// The orphan check for one start or stop.
    fn orphans<'a>(&'a self, pid_file: &'a PidFile, admin: &'a Admin, tag: &'a str) -> Orphans<'a> {
        Orphans {
            pid_file,
            processes: &*self.processes,
            admin,
            input: &self.input,
            ladder: self.ladder,
            tag,
        }
    }

    /// Says why in Caddy's recent lines, and sets `Failed { code }`.
    fn blocked(&self, what: &str, blocked: &Blocked, events: &broadcast::Sender<Event>) {
        warn!(code = blocked.code, reason = %blocked.reason, "{what}");
        let text = format!("marsad: {what}: {}.", blocked.reason);
        note(&self.runtime, events, &text);
        self.fail(blocked.code, events);
    }

    /// Sets `Failed { code }`. Only while no task runs: the supervisor owns
    /// the status then.
    fn fail(&self, code: &str, events: &broadcast::Sender<Event>) {
        set_failed(&self.runtime, events, code);
    }
}

/// How the start of Caddy with the daemon ended (task M5-16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launched {
    /// No Caddy is installed: nothing was tried, and the status is unchanged.
    NotInstalled,
    /// The start ran. Caddy's status after it: `Running`, or `Failed` with
    /// the reason.
    Ran(ServiceStatus),
    /// Another action on Caddy ran first, a request for example. It decides.
    Busy,
    /// The start failed before Caddy ran, with this code. The status is
    /// `Failed` with the same code.
    Failed(&'static str),
    /// The daemon began to stop first, and the start was dropped. The
    /// shutdown stops a Caddy it had spawned (see [`start_at_launch`]).
    ShuttingDown,
}

/// The start of Caddy with the daemon. Its task ends after the start, or at
/// once when the daemon's shutdown token is cancelled.
#[derive(Debug)]
pub struct Launch {
    task: JoinHandle<Launched>,
}

impl Launch {
    /// Waits for the start to end, at most `limit`, then aborts it. `None`
    /// when it did not end in time or its task failed.
    pub async fn wait(self, limit: Duration) -> Option<Launched> {
        let abort = self.task.abort_handle();
        match tokio::time::timeout(limit, self.task).await {
            Ok(Ok(launched)) => Some(launched),
            Ok(Err(err)) => {
                warn!(error = %err, "the start of caddy with the daemon failed");
                None
            }
            Err(_) => {
                warn!(
                    ?limit,
                    "the start of caddy with the daemon still runs; aborting it"
                );
                abort.abort();
                None
            }
        }
    }
}

/// Starts Caddy in the background when it is installed (task M5-16), so the
/// sites answer after a reboot or a `marsa daemon stop` with no step by hand.
/// The path of `marsa services start caddy`: [`CaddyManager::run`], with the
/// orphan check (ADR 0009) and one action at a time, so a request meanwhile
/// waits for it ([`CaddyManager::run_waiting`]) and never starts a second
/// Caddy. The API serves meanwhile.
///
/// The config is the latest that the sites sync gave
/// [`CaddyManager::apply`]: `start` and `apply` take turns on `pushing`, so a
/// pass that comes after the start sends its config with `POST /load`. Caddy's
/// `Running` also asks for a pass.
///
/// A shutdown drops the start at its next `.await`. The spawn of Caddy and
/// `add_proc` have no `.await` between them, so a Caddy it spawned is in
/// `Daemon::procs`, which the shutdown stops; `server::run` sweeps `procs`
/// again at its end for one added after the first sweep. A Caddy that the
/// start was stopping after a time-out was told to stop before that `.await`.
/// `kill_on_drop` is the last guard, as for every Caddy.
pub fn start_at_launch(daemon: &Arc<Daemon>) -> Launch {
    let daemon = Arc::clone(daemon);
    let task = tokio::spawn(async move {
        tokio::select! {
            // A daemon that already stops starts nothing.
            biased;
            () = daemon.shutdown.cancelled() => Launched::ShuttingDown,
            launched = launch(&daemon) => launched,
        }
    });
    Launch { task }
}

async fn launch(daemon: &Daemon) -> Launched {
    let caddy = &daemon.caddy;
    // First, so a missing Caddy does not mark Caddy's status failed.
    if !caddy.installed() {
        info!("no caddy is installed; it does not start with the daemon");
        return Launched::NotInstalled;
    }
    info!("starting caddy with the daemon");
    match caddy.run(daemon, ServiceAction::Start).await {
        Ok(status) => {
            match &status.state {
                ServiceState::Running => info!("caddy started with the daemon"),
                // `marsa services` and `marsa doctor` show the code too.
                ServiceState::Failed { code } => {
                    warn!(%code, "caddy did not start with the daemon");
                }
                state => warn!(
                    ?state,
                    "caddy is not running after its start with the daemon"
                ),
            }
            Launched::Ran(status)
        }
        Err(ActionError::Busy) => {
            info!("another action on caddy runs; it decides instead of the start with the daemon");
            Launched::Busy
        }
        Err(err) => {
            warn!(code = err.code(), %err, "caddy did not start with the daemon");
            Launched::Failed(err.code())
        }
    }
}

/// The line before a reap, in Caddy's recent lines.
const STOPPING_ORPHAN: &str = "marsad: Stopping an old Caddy left by a crashed marsad.";

enum Ready {
    Yes,
    /// The task ended first: Caddy failed for good.
    Ended,
    TimedOut,
}

/// Watches one run of Caddy. It owns the supervisor's task, and its own task
/// is the one in `Daemon::procs`.
struct Watcher {
    /// The event bus, subscribed before the process started.
    lines: broadcast::Receiver<Event>,
    /// The `seq` of this run's first line.
    first_seq: u64,
    cancel: CancellationToken,
    runtime: Arc<Mutex<ServiceRuntime>>,
    events: broadcast::Sender<Event>,
    /// `run/caddy.pid`: written after each spawn, removed after the exit.
    pid_file: PidFile,
    processes: Arc<dyn Processes>,
}

impl Watcher {
    /// Waits for the supervisor's task. If Caddy first says that a port is
    /// taken, stops it and sets `Failed { port_in_use }`: a restart would
    /// fail the same way (ADR 0008).
    async fn run(self, mut supervisor: JoinHandle<()>) {
        // `stop_procs` aborts this task after its time limit; the abort must
        // reach the supervisor too, so the `Child` is dropped and killed.
        let _abort = AbortOnDrop(supervisor.abort_handle());
        let Watcher {
            lines,
            first_seq,
            cancel,
            runtime,
            events,
            pid_file,
            processes,
        } = self;
        let conflict = tokio::select! {
            result = &mut supervisor => {
                report(result);
                false
            }
            () = watch(lines, first_seq, &runtime, &pid_file, &*processes) => true,
        };
        if conflict {
            warn!("a port caddy needs is in use; stopping it without a restart");
            cancel.cancel();
            report(supervisor.await);
        }
        // Caddy has exited, so its pid may be anyone's from now on.
        pid_file.remove();
        if conflict {
            // After the supervisor's last change, so this one stays.
            set_failed(&runtime, &events, "port_in_use");
        }
    }
}

/// Writes `run/caddy.pid` after each spawn of this run. Ends when a stderr
/// line of this run says that a port is taken; never ends otherwise.
async fn watch(
    mut lines: broadcast::Receiver<Event>,
    first_seq: u64,
    runtime: &Mutex<ServiceRuntime>,
    pid_file: &PidFile,
    processes: &dyn Processes,
) {
    let mut recorded = None;
    loop {
        let status = match lines.recv().await {
            Ok(Event::Log(line)) if line.service == CADDY_ID && is_port_conflict(&line) => return,
            Ok(Event::ServiceChanged(status)) if status.id == CADDY_ID => status,
            Ok(_) => continue,
            // Too far behind: the skipped lines are still in the buffer, and
            // the status is current.
            Err(RecvError::Lagged(_)) => {
                let (recent, status) = {
                    let runtime = lock(runtime);
                    (runtime.logs.recent(), runtime.status.clone())
                };
                let mut this_run = recent.iter().filter(|line| line.seq >= first_seq);
                if this_run.any(is_port_conflict) {
                    return;
                }
                status
            }
            Err(RecvError::Closed) => std::future::pending().await,
        };
        // The supervisor says `Running` right after each spawn.
        if status.state == ServiceState::Running
            && let Some(pid) = status.pid
            && recorded != Some(pid)
        {
            pid_file.record(processes, pid).await;
            recorded = Some(pid);
        }
    }
}

fn is_port_conflict(line: &LogLine) -> bool {
    if line.stream != LogStream::Stderr {
        return false;
    }
    let text = line.text.to_ascii_lowercase();
    PORT_IN_USE.iter().any(|needle| text.contains(needle))
}

fn report(result: Result<(), JoinError>) {
    if let Err(err) = result {
        warn!(%err, "the caddy supervisor task failed");
    }
}

struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Adds a line from the daemon to Caddy's recent lines, so `marsa logs caddy`
/// shows why Caddy did not start.
fn note(runtime: &Mutex<ServiceRuntime>, events: &broadcast::Sender<Event>, text: &str) {
    let mut runtime = lock(runtime);
    let line = runtime.logs.push(LogStream::Stderr, text, now_ms());
    // Under the lock, like the supervisor, so events come in `seq` order.
    let _ = events.send(Event::Log(line));
}

/// Sets `Failed { code }` and tells every reader, under the lock like the
/// supervisor, so events come in the order of the changes.
fn set_failed(runtime: &Mutex<ServiceRuntime>, events: &broadcast::Sender<Event>, code: &str) {
    let mut runtime = lock(runtime);
    let status = &mut runtime.status;
    status.state = ServiceState::Failed {
        code: code.to_owned(),
    };
    status.pid = None;
    status.since_ms = Some(now_ms());
    // An error only means that nobody listens right now.
    let _ = events.send(Event::ServiceChanged(status.clone()));
}

/// Puts this data folder's `tag` as the `@id` of the `admin` object.
/// `GET /id/<tag>` then answers only on our own Caddy. Without an `admin`
/// object there is no tag, and the Caddy counts as not ours, which is the
/// safe side.
fn tag_config(config: &mut Value, tag: &str) {
    if let Some(admin) = config.get_mut("admin").and_then(Value::as_object_mut) {
        admin.insert("@id".to_owned(), Value::String(tag.to_owned()));
    }
}

/// Writes `caddy.json` through a temp file in the same folder and a rename,
/// so Caddy never reads half a file.
async fn write_config(path: &Path, config: &Value) -> io::Result<()> {
    let mut body = serde_json::to_vec_pretty(config).map_err(io::Error::other)?;
    body.push(b'\n');
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let temp = path.with_extension("json.tmp");
    let written = async {
        let mut file = tokio::fs::File::create(&temp).await?;
        file.write_all(&body).await?;
        file.sync_all().await?;
        // Closed before the rename: Windows cannot rename an open file.
        drop(file);
        tokio::fs::rename(&temp, path).await
    }
    .await;
    if written.is_err() {
        // Best effort: the write already failed, and that is the error to report.
        let _ = tokio::fs::remove_file(&temp).await;
    }
    written
}

/// Caddy's admin API on `127.0.0.1:<admin port>`.
struct Admin {
    /// `None` if the client cannot be built: every call then fails.
    http: Option<reqwest::Client>,
    base: String,
}

impl Admin {
    fn new(port: u16) -> Self {
        // `no_proxy`: a proxy from `HTTP_PROXY` must never get these calls.
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(redirect::Policy::none())
            .timeout(ADMIN_TIMEOUT)
            .build()
            .inspect_err(|err| warn!(%err, "cannot build the Caddy admin client"))
            .ok();
        Self {
            http,
            base: format!("http://127.0.0.1:{port}"),
        }
    }

    /// `POST /stop`: Caddy stops its servers and exits.
    async fn stop(&self) {
        let Some(http) = &self.http else {
            return;
        };
        match http.post(format!("{}/stop", self.base)).send().await {
            Ok(response) => debug!(status = %response.status(), "caddy admin /stop"),
            Err(err) => debug!(%err, "caddy admin /stop failed"),
        }
    }

    /// `POST /load` with `config`: Caddy runs it from now on, or keeps its
    /// old config and says why.
    async fn load(&self, config: &Value) -> Result<(), LoadError> {
        let Some(http) = &self.http else {
            return Err(LoadError::Unreachable("no HTTP client".to_owned()));
        };
        let response = http
            .post(format!("{}/load", self.base))
            .json(config)
            .timeout(LOAD_TIMEOUT)
            .send()
            .await
            .map_err(|err| LoadError::Unreachable(err.to_string()))?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        // A body that cannot be read leaves the status to say what happened.
        let body = response.text().await.unwrap_or_default();
        Err(load_error(status.as_u16(), &body))
    }
}

/// What Caddy says, in lower case, when it cannot listen on an address of a
/// new config: Go's texts on macOS and Linux (ADR 0008, finding 11), and the
/// two English Windows texts (see [`PORT_IN_USE`]).
const LOAD_PORT_ERRORS: [&str; 4] = [
    "bind: address already in use",
    "bind: permission denied",
    "only one usage of each socket address",
    "forbidden by its access permissions",
];

/// The error of a refused `POST /load`. Caddy answers `{"error": "..."}`; a
/// 400 that says an address is taken or not allowed is `port_in_use`.
fn load_error(status: u16, body: &str) -> LoadError {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|body| body.get("error")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| body.trim().to_owned());
    let lower = message.to_ascii_lowercase();
    if status == 400 && LOAD_PORT_ERRORS.iter().any(|text| lower.contains(text)) {
        let address = listen_address(&message);
        return LoadError::PortInUse { address, message };
    }
    LoadError::Rejected { status, message }
}

/// The `<addr>` of `listening on <addr>: ...` in Caddy's error text, for
/// example `127.0.0.1:18443`.
fn listen_address(message: &str) -> Option<String> {
    let (_, rest) = message.split_once("listening on ")?;
    let (address, _) = rest.split_once(": ")?;
    (!address.is_empty()).then(|| address.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::consts::EXE_SUFFIX;
    use std::path::PathBuf;

    use axum::http::StatusCode;
    use axum::routing::post;
    use marsa_core::caddy::caddy_config;
    use marsa_core::sites::SiteKind;
    use tokio::sync::mpsc;

    fn line(stream: LogStream, text: &str) -> LogLine {
        LogLine {
            service: CADDY_ID.into(),
            seq: 0,
            at_ms: 0,
            stream,
            text: text.into(),
        }
    }

    #[test]
    fn caddy_spec_runs_the_newest_caddy_with_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let caddy = dir.path().join("runtimes").join("caddy");
        for version in ["2.11.8", "2.11.10", "2.11.9"] {
            std::fs::create_dir_all(caddy.join(version)).unwrap();
        }
        let paths = CaddyPaths::in_dir(dir.path());
        let spec = caddy_spec(&paths).unwrap();

        let program = caddy.join("2.11.10").join(format!("caddy{EXE_SUFFIX}"));
        assert_eq!(spec.program, program);
        assert!(spec.program.is_absolute());
        let config = dir.path().join("caddy").join("caddy.json");
        let config = config.to_str().unwrap();
        assert_eq!(spec.args, ["run", "--config", config]);
        assert_eq!(spec.env, caddy_env(&paths));
        assert_eq!(spec.id, CADDY_ID);
        assert_eq!(spec.cwd, None);
        let restart = RestartPolicy::OnFailure {
            max_retries: 3,
            initial: Duration::from_millis(500),
            max: Duration::from_secs(5),
        };
        assert_eq!(spec.restart, restart);
        assert_eq!(RESTART, restart);
        assert_eq!(spec.stop_grace, Duration::from_secs(2));
        assert_eq!(caddy_spec_for(&paths, program), spec);
    }

    #[test]
    fn caddy_spec_without_caddy_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = CaddyPaths::in_dir(dir.path());

        let err = caddy_spec(&paths).unwrap_err();
        assert_eq!(err.code(), "caddy_not_installed");
        // Every reason for no Caddy is the code the CLI and GUI know.
        for err in [
            err,
            CaddyError::UnsupportedPlatform,
            CaddyError::RecipeInvalid { reason: "x".into() },
        ] {
            assert_eq!(ActionError::NoCaddy(err).code(), "caddy_not_installed");
        }
        let missing = ActionError::NotInstalled(paths.runtime_dir().join("2.11.8"));
        assert_eq!(missing.code(), "caddy_not_installed");
    }

    #[test]
    fn a_taken_port_is_found_on_stderr_only() {
        let go = "Error: loading initial config: loading new config: http app module: start: \
                  listening on 127.0.0.1:18080: listen tcp 127.0.0.1:18080: bind: address already in use";
        let windows = "listen tcp 127.0.0.1:18080: bind: Only one usage of each socket \
                       address (protocol/network address/port) is normally permitted.";
        // Port 80 on a Windows machine where IIS runs (spike S-11).
        let held_alone = "listening on 127.0.0.1:80: listen tcp 127.0.0.1:80: bind: An attempt \
                          was made to access a socket in a way forbidden by its access permissions.";
        assert!(is_port_conflict(&line(LogStream::Stderr, go)));
        assert!(is_port_conflict(&line(LogStream::Stderr, windows)));
        assert!(is_port_conflict(&line(LogStream::Stderr, held_alone)));
        assert!(!is_port_conflict(&line(LogStream::Stdout, go)));
        let other = "bind: permission denied";
        assert!(!is_port_conflict(&line(LogStream::Stderr, other)));
    }

    /// Linux after setup (ADR 0014): the next config has ports 80 and 443.
    /// The manager asks at every config, so a running Caddy moves with no
    /// restart of the daemon.
    #[test]
    fn the_config_takes_ports_80_and_443_once_the_system_allows_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut caddy = CaddyManager::new(dir.path());
        let tag = "marsa-instance-0123456789abcdef0123456789abcdef";
        let ports = |config: &Value| {
            let http = &config["apps"]["http"];
            (
                http["http_port"].as_u64(),
                http["https_port"].as_u64(),
                config["admin"]["listen"].as_str().map(str::to_owned),
            )
        };
        let admin = Some("127.0.0.1:18019".to_owned());

        // A new manager never asks the system: tests stay on the high ports.
        assert_eq!(caddy.ports(), CaddyInput::default());
        let high = caddy.config(tag).unwrap();
        assert_eq!(ports(&high), (Some(18080), Some(18443), admin.clone()));

        caddy.low_ports = || true;
        assert_eq!(caddy.ports(), CaddyInput::default().on_low_ports());
        let low = caddy.config(tag).unwrap();
        assert_eq!(ports(&low), (Some(80), Some(443), admin.clone()));

        // And back, if the system stops allowing them.
        caddy.low_ports = || false;
        assert_eq!(
            ports(&caddy.config(tag).unwrap()),
            (Some(18080), Some(18443), admin)
        );
    }

    #[tokio::test]
    async fn write_config_replaces_caddy_json_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caddy").join("caddy.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "old").unwrap();

        let input = CaddyInput {
            admin_port: 1,
            ..CaddyInput::default()
        };
        let tag = "marsa-instance-0123456789abcdef0123456789abcdef";
        let mut config = caddy_config(&input);
        tag_config(&mut config, tag);
        write_config(&path, &config).await.unwrap();
        let mut written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // The core config, plus our tag on the admin object, and nothing else.
        assert_eq!(written["admin"]["@id"], tag);
        let admin = written["admin"].as_object_mut().unwrap();
        admin.remove("@id");
        assert_eq!(written, caddy_config(&input));
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["caddy.json"], "no temp file left");
    }

    /// What Caddy v2.11.7 answered when another program held its HTTPS
    /// address (ADR 0008, finding 11, T9), verbatim.
    const T9: &str = "{\"error\":\"loading config: loading new config: http app module: start: \
                      listening on 127.0.0.1:18443: listen tcp 127.0.0.1:18443: bind: address already in use\"}";

    #[test]
    fn a_taken_address_in_a_load_answer_is_port_in_use() {
        let err = load_error(400, T9);
        assert_eq!(err.code(), "port_in_use");
        let LoadError::PortInUse { address, message } = err else {
            panic!("not port_in_use");
        };
        assert_eq!(address.as_deref(), Some("127.0.0.1:18443"));
        assert!(message.starts_with("loading config:"), "{message}");

        // The redirect port (T9b), and a low port Caddy may not use.
        let t9b = T9.replace("18443", "18080");
        let LoadError::PortInUse { address, .. } = load_error(400, &t9b) else {
            panic!("not port_in_use");
        };
        assert_eq!(address.as_deref(), Some("127.0.0.1:18080"));
        let denied = "{\"error\":\"loading config: loading new config: http app module: start: \
                      listening on [::1]:443: listen tcp [::1]:443: bind: permission denied\"}";
        let LoadError::PortInUse { address, .. } = load_error(400, denied) else {
            panic!("not port_in_use");
        };
        assert_eq!(address.as_deref(), Some("[::1]:443"));
        // Windows, with IIS on port 80 (spike S-11).
        let held_alone = "{\"error\":\"loading config: loading new config: http app module: start: \
                          listening on 127.0.0.1:80: listen tcp 127.0.0.1:80: bind: An attempt was \
                          made to access a socket in a way forbidden by its access permissions.\"}";
        let LoadError::PortInUse { address, .. } = load_error(400, held_alone) else {
            panic!("not port_in_use");
        };
        assert_eq!(address.as_deref(), Some("127.0.0.1:80"));
    }

    #[test]
    fn other_load_answers_are_rejected_with_their_text() {
        let bad = "{\"error\":\"loading config: decoding request body: unexpected EOF\"}";
        let err = load_error(400, bad);
        assert_eq!(err.code(), "caddy_config_rejected");
        assert!(err.to_string().contains("unexpected EOF"), "{err}");
        // Only a 400 says that Caddy refused the config itself.
        assert_eq!(load_error(500, T9).code(), "caddy_config_rejected");
        // A body that is not JSON is kept as text.
        let LoadError::Rejected { status, message } = load_error(502, " bad gateway\n") else {
            panic!("not rejected");
        };
        assert_eq!((status, message.as_str()), (502, "bad gateway"));
        // No `listening on`: still the code, without an address.
        let short = "{\"error\":\"bind: address already in use\"}";
        let LoadError::PortInUse { address, .. } = load_error(400, short) else {
            panic!("not port_in_use");
        };
        assert_eq!(address, None);
    }

    /// A fake admin API: `POST /load` sends each body on the channel and
    /// answers `status` with `body`.
    async fn fake_admin(
        status: StatusCode,
        body: &'static str,
    ) -> (u16, mpsc::UnboundedReceiver<Value>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let app = axum::Router::new().route(
            "/load",
            post(move |axum::Json(config): axum::Json<Value>| async move {
                tx.send(config).unwrap();
                (status, body)
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (port, rx)
    }

    /// Registers a task as the running Caddy, so `apply` sends to the admin API.
    fn pretend_caddy_runs(daemon: &Daemon) {
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let cancel = cancel.clone();
            async move { cancel.cancelled().await }
        });
        let runtime = Arc::new(Mutex::new(ServiceRuntime::new(CADDY_ID)));
        daemon.add_proc(
            CADDY_ID,
            ServiceHandle {
                cancel,
                task,
                runtime,
            },
        );
    }

    fn shop(root: PathBuf) -> Site {
        Site {
            name: "shop".into(),
            docroot: root.clone(),
            root,
            kind: SiteKind::Static,
            php: None,
            php_source: None,
            secure: true,
        }
    }

    #[tokio::test]
    async fn apply_writes_the_file_only_while_caddy_does_not_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::new(dir.path().to_path_buf());
        // Nothing listens there: a call would fail.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        daemon.caddy.input.admin_port = free.local_addr().unwrap().port();
        drop(free);
        let sites = vec![shop(dir.path().join("shop"))];
        let applied = daemon
            .caddy
            .apply(&daemon, sites, PhpPools::default())
            .await;
        assert_eq!(applied.unwrap(), Applied::Written);

        let text = std::fs::read_to_string(&daemon.caddy.paths.config_file).unwrap();
        let written: Value = serde_json::from_str(&text).unwrap();
        let hosts = &written["apps"]["http"]["servers"]["marsa-tls"]["routes"][0]["match"][0];
        assert_eq!(hosts["host"][0], "shop.test");
        let tag = written["admin"]["@id"].as_str().unwrap();
        assert!(tag.starts_with("marsa-instance-"), "{tag}");
    }

    #[tokio::test]
    async fn apply_loads_the_config_into_a_running_caddy() {
        let (port, mut loads) = fake_admin(StatusCode::OK, "").await;
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::new(dir.path().to_path_buf());
        daemon.caddy.input.admin_port = port;
        pretend_caddy_runs(&daemon);

        let sites = vec![shop(dir.path().join("shop"))];
        let applied = daemon
            .caddy
            .apply(&daemon, sites, PhpPools::default())
            .await;
        assert_eq!(applied.unwrap(), Applied::Loaded);
        let loaded = loads.recv().await.unwrap();
        let text = std::fs::read_to_string(&daemon.caddy.paths.config_file).unwrap();
        let written: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(loaded, written, "the file and Caddy have the same config");
        daemon.stop_procs(Duration::from_secs(5)).await;
    }

    #[tokio::test]
    async fn a_refused_load_is_port_in_use_and_noted_in_the_log() {
        let (port, mut loads) = fake_admin(StatusCode::BAD_REQUEST, T9).await;
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::new(dir.path().to_path_buf());
        daemon.caddy.input.admin_port = port;
        pretend_caddy_runs(&daemon);

        let sites = vec![shop(dir.path().join("shop"))];
        let err = daemon
            .caddy
            .apply(&daemon, sites, PhpPools::default())
            .await
            .unwrap_err();
        assert!(loads.recv().await.is_some());
        assert_eq!(err.code(), "port_in_use");
        assert!(err.to_string().contains("127.0.0.1:18443"), "{err}");
        let last = daemon.caddy.recent_logs().pop().unwrap();
        assert!(
            last.text
                .starts_with("marsad: The new config was not loaded"),
            "{}",
            last.text
        );
        assert!(last.text.contains("127.0.0.1:18443"), "{}", last.text);
        daemon.stop_procs(Duration::from_secs(5)).await;
    }
}
