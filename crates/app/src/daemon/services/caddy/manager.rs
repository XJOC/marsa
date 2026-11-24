//! The one Caddy: its actions, its config, and the orphan check before a start.

use super::admin::{Admin, LoadError};
use super::orphan::{Blocked, Ladder, Orphans, PidFile};
use super::watch::Watcher;
use super::{ActionError, caddy_spec};
use crate::daemon::services::actions::{Busy, take_turn};
use crate::daemon::services::probe::{ProcessProbe, SystemProbe};
use crate::daemon::services::supervisor::{ServiceHandle, ServiceRuntime, Supervisor, lock};
use crate::daemon::state::Daemon;
use marsa_core::caddy::{CaddyError, CaddyInput, CaddyPaths, PhpPools, sites_config};
use marsa_core::codes;
use marsa_core::data_dir::DataDir;
use marsa_core::orphan::instance_tag;
use marsa_core::platform::process;
use marsa_core::services::ServiceSpec;
use marsa_core::sites::Site;
use marsa_core::types::ServiceId;
use marsa_core::types::{Event, LogLine, ServiceAction, ServiceState, ServiceStatus};
use serde_json::Value;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast::{self};
use tracing::{debug, info, warn};

/// How long `start` waits for the admin API by default.
const READY_LIMIT: Duration = Duration::from_secs(5);
const READY_POLL: Duration = Duration::from_millis(100);
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
    pub processes: Arc<dyn ProcessProbe>,
    /// How long each step of an orphan's stop waits.
    pub ladder: Ladder,
    /// The program of the last start, so an uninstall never deletes the
    /// Caddy that runs. Use [`CaddyManager::running_program`].
    pub started: Mutex<Option<PathBuf>>,
    /// The status and recent lines. Kept across runs, so `seq` never goes
    /// back and the lines of a failed run stay readable.
    runtime: Arc<Mutex<ServiceRuntime>>,
    /// Held for the whole of each action. [`CaddyManager::run`] only tries
    /// it, [`CaddyManager::run_waiting`] waits for it in line, and
    /// [`start_at_launch`] takes it before its task runs. A tokio lock: it is
    /// held across `.await`, and it serves the waits in order. In an `Arc`,
    /// so a task can own its guard.
    pub(super) busy: Arc<tokio::sync::Mutex<()>>,
    /// The sites and PHP pools of the config. [`CaddyManager::apply`] sets
    /// them; `start` writes the config of the latest ones.
    wanted: Mutex<Wanted>,
    /// Where the dump server listens, while it does: the next config sends
    /// the `dump()` of every PHP site there (ADR 0027). [`CaddyManager::set_dumps`].
    dumps: Mutex<Option<SocketAddr>>,
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

impl CaddyManager {
    /// Caddy in `data_dir`, on Marsa's ports. Nothing is read or started.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            paths: CaddyPaths::in_dir(data_dir),
            spec: None,
            input: CaddyInput::default(),
            low_ports: || false,
            ready_limit: READY_LIMIT,
            processes: Arc::new(SystemProbe),
            ladder: Ladder::default(),
            started: Mutex::default(),
            runtime: Arc::new(Mutex::new(ServiceRuntime::new(ServiceId::Caddy))),
            busy: Arc::default(),
            wanted: Mutex::default(),
            dumps: Mutex::default(),
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
        // A serialization token across the push (manager decision
        // 2026-11-14), not a guard of data.
        let _pushing = self.pushing.lock().await;
        *lock(&self.wanted) = Wanted { sites, pools };
        self.push(daemon).await
    }

    /// Writes the config of the wanted sites and pools, and loads it into a
    /// running Caddy. The caller holds `pushing`.
    async fn push(&self, daemon: &Daemon) -> Result<Applied, LoadError> {
        let tag = self.instance_tag().map_err(LoadError::Write)?;
        let config = self.config(&tag).map_err(LoadError::Config)?;
        write_config(&self.paths.config_file, &config)
            .await
            .map_err(LoadError::Write)?;
        if !daemon.proc_alive(ServiceId::Caddy) {
            return Ok(Applied::Written);
        }
        match Admin::new(self.input.admin_port).load(&config).await {
            Ok(()) => Ok(Applied::Loaded),
            // Caddy stopped in the meantime: it reads the file when it starts.
            Err(LoadError::Unreachable(_)) if !daemon.proc_alive(ServiceId::Caddy) => {
                Ok(Applied::Written)
            }
            Err(err) => {
                let text = format!("marsad: The new config was not loaded: {err}.");
                lock(&self.runtime).note(daemon.events(), &text);
                Err(err)
            }
        }
    }

    /// The ports of the next config: those of `input`, or 80 and 443 where
    /// the system now lets Caddy bind them. Linux only: the helper's
    /// `ports_install` allows it at setup, so the next config after setup
    /// moves a running Caddy to 80 and 443, with no restart.
    ///
    /// The orphan check looks at these ports and at those of `input`
    /// (`CaddyManager::orphans`): an orphan may have started before setup.
    pub fn ports(&self) -> CaddyInput {
        if (self.low_ports)() {
            self.input.on_low_ports()
        } else {
            self.input
        }
    }

    /// Sends the config of the latest sites and pools again, for a change
    /// that is not in them: the ports, after setup on Linux let Caddy bind 80
    /// and 443. See [`CaddyManager::apply`].
    ///
    /// # Errors
    ///
    /// See [`LoadError`].
    pub async fn reapply(&self, daemon: &Daemon) -> Result<Applied, LoadError> {
        // The wanted state is read only after the turn comes: a change that
        // waited on `pushing` first is kept, not undone by an older copy.
        // A serialization token, as in `apply`.
        let _pushing = self.pushing.lock().await;
        self.push(daemon).await
    }

    /// Where the dump server listens now, or `None`. The next config uses it:
    /// call [`CaddyManager::reapply`] to send it to a running Caddy. Returns
    /// whether it changed.
    pub fn set_dumps(&self, addr: Option<SocketAddr>) -> bool {
        let mut held = lock(&self.dumps);
        let changed = *held != addr;
        *held = addr;
        changed
    }

    /// Where the next config sends `dump()`: see [`CaddyManager::set_dumps`].
    pub fn dumps_target(&self) -> Option<SocketAddr> {
        *lock(&self.dumps)
    }

    /// The config of the latest sites and pools, with `tag` on `admin`.
    pub(super) fn config(&self, tag: &str) -> Result<Value, CaddyError> {
        let mut config = {
            let wanted = lock(&self.wanted);
            let input = CaddyInput {
                dumps: *lock(&self.dumps),
                ..self.ports()
            };
            sites_config(&input, &wanted.sites, &wanted.pools)?
        };
        tag_config(&mut config, tag);
        Ok(config)
    }

    /// Whether there is a Caddy to run: the check `start` makes first, here
    /// without marking the status failed. Setup asks it before it starts
    /// Caddy to make the CA.
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
        if !daemon.proc_alive(ServiceId::Caddy) {
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
        let busy = Arc::clone(&self.busy)
            .try_lock_owned()
            .map_err(|_| ActionError::Busy)?;
        self.act(daemon, action, busy).await
    }

    /// [`CaddyManager::run`], but an action that finds another one running
    /// waits up to `wait` for it to end, then runs. Waits end in the order
    /// they began. So a `stop` sent while the daemon starts Caddy
    /// stops Caddy once that start has ended, if it ends within
    /// `wait`; otherwise the answer is still `service_busy`.
    ///
    /// Once the daemon begins to stop, nothing runs: the shutdown stops
    /// Caddy itself, and a start then would take our stopping Caddy for an
    /// orphan and start another one.
    ///
    /// # Errors
    ///
    /// `service_busy` when the wait runs out or the daemon stops; the others
    /// as for [`CaddyManager::run`].
    pub async fn run_waiting(
        &self,
        daemon: &Daemon,
        action: ServiceAction,
        wait: Duration,
    ) -> Result<ServiceStatus, ActionError> {
        let busy = take_turn(&self.busy, daemon.shutdown(), wait, || {
            info!(%action, "another action on caddy runs; waiting for it");
        })
        .await
        .map_err(|Busy| {
            if daemon.shutdown().is_cancelled() {
                info!(%action, "the daemon is stopping; the caddy action does not run");
            }
            ActionError::Busy
        })?;
        self.act(daemon, action, busy).await
    }

    /// Runs `action`. `_busy` proves that no other action runs; it is let go
    /// when this ends, also when the caller is dropped half way.
    pub(super) async fn act(
        &self,
        daemon: &Daemon,
        action: ServiceAction,
        _busy: tokio::sync::OwnedMutexGuard<()>,
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
        if daemon.proc_alive(ServiceId::Caddy) {
            return Ok(());
        }
        // No task runs from here on, so these writes cannot race the supervisor.
        let spec = match self.spec_to_run() {
            Ok(spec) if spec.program.is_file() => spec,
            Ok(spec) => {
                self.fail(codes::CADDY_NOT_INSTALLED, daemon.events());
                return Err(ActionError::NotInstalled(spec.program));
            }
            Err(err) => {
                warn!(code = err.code(), %err, "no caddy to run");
                self.fail(codes::CADDY_NOT_INSTALLED, daemon.events());
                return Err(ActionError::NoCaddy(err));
            }
        };
        // The tag goes into `caddy.json`, and the check below looks for it.
        let tag = match self.instance_tag() {
            Ok(tag) => tag,
            Err(err) => {
                self.fail(codes::CADDY_CONFIG_WRITE, daemon.events());
                return Err(ActionError::ConfigWrite(err));
            }
        };
        // ADR 0009: stop our own orphan; touch nothing that is not ours.
        let pid_file = PidFile::new(daemon.data_dir(), &self.our_programs(&spec.program));
        {
            let orphans = self.orphans(&pid_file, admin, &tag);
            if let Some(found) = orphans.inspect().await {
                if found.is_reap() {
                    lock(&self.runtime).note(daemon.events(), STOPPING_ORPHAN);
                }
                if let Err(blocked) = orphans.clear_for_start(found).await {
                    self.blocked("Caddy was not started", &blocked, daemon.events());
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
                self.fail(codes::CADDY_CONFIG_WRITE, daemon.events());
                let err = io::Error::new(io::ErrorKind::InvalidData, err);
                return Err(ActionError::ConfigWrite(err));
            }
        };
        if let Err(err) = write_config(&self.paths.config_file, &config).await {
            self.fail(codes::CADDY_CONFIG_WRITE, daemon.events());
            return Err(ActionError::ConfigWrite(err));
        }
        // Before the process starts, so the watcher cannot miss its first line.
        let lines = daemon.events().subscribe();
        let first_seq = self.recent_logs().last().map_or(0, |line| line.seq + 1);
        let spec = ServiceSpec {
            id: ServiceId::Caddy,
            ..spec
        };
        let logs_dir = DataDir::new(daemon.data_dir()).logs();
        let runtime = Arc::clone(&self.runtime);
        *lock(&self.started) = Some(spec.program.clone());
        let handle = Supervisor::spawn_on(spec, daemon.events().clone(), &logs_dir, runtime)
            .map_err(|err| {
                self.fail(err.code(), daemon.events());
                ActionError::ProgramNotAbsolute(err)
            })?;

        let ServiceHandle {
            cancel,
            task,
            runtime,
            stop_limit,
        } = handle;
        let watcher = Watcher {
            lines,
            first_seq,
            cancel: cancel.clone(),
            runtime: Arc::clone(&runtime),
            events: daemon.events().clone(),
            pid_file,
            processes: Arc::clone(&self.processes),
        };
        let task = tokio::spawn(watcher.run(task));
        // A handle this replaces belongs to a run that has ended.
        daemon.add_proc(
            ServiceId::Caddy,
            ServiceHandle {
                cancel,
                task,
                runtime,
                stop_limit,
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
                    self.fail(codes::CADDY_START_TIMEOUT, daemon.events());
                }
            }
        }
        Ok(())
    }

    /// Polls `GET /id/<tag>` until our Caddy answers, the task ends, or the
    /// limit. Only our tag counts: another program on the admin port, a
    /// second user's Caddy for one, is never taken for ours, so no config
    /// goes to it. Where no orphan check runs first (Windows) this is the
    /// only check.
    async fn wait_ready(&self, daemon: &Daemon, admin: &Admin, tag: &str) -> Ready {
        let poll = async {
            loop {
                if !daemon.proc_alive(ServiceId::Caddy) {
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
        if !daemon.proc_alive(ServiceId::Caddy) {
            return self.stop_orphan(daemon, admin).await;
        }
        match self.instance_tag() {
            Ok(tag) if admin.answers_as(&tag).await => admin.stop().await,
            Ok(_) => debug!("the admin port does not answer with our tag; no /stop"),
            Err(err) => warn!(%err, "cannot read the instance id; no /stop"),
        }
        // `None`: a shutdown took the handle and stops it.
        if let Some(handle) = daemon.take_proc(ServiceId::Caddy)
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
        if !process::CADDY_ORPHAN_RECOVERY {
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
        let pid_file = PidFile::new(daemon.data_dir(), &self.our_programs(&program));
        let orphans = self.orphans(&pid_file, admin, &tag);
        let Some(found) = orphans.inspect().await else {
            return true;
        };
        if !found.is_reap() {
            return true;
        }
        lock(&self.runtime).note(daemon.events(), STOPPING_ORPHAN);
        match orphans.reap(found).await {
            Ok(()) => true,
            Err(blocked) => {
                self.blocked("The old Caddy was not stopped", &blocked, daemon.events());
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

    /// The orphan check for one start or stop: the ports the next Caddy
    /// needs, and the high ones too when it takes 80 and 443 (Linux).
    fn orphans<'a>(&'a self, pid_file: &'a PidFile, admin: &'a Admin, tag: &'a str) -> Orphans<'a> {
        let input = self.ports();
        Orphans {
            pid_file,
            processes: &*self.processes,
            admin,
            input,
            other: (input != self.input).then_some(self.input),
            ladder: self.ladder,
            tag,
        }
    }

    /// Says why in Caddy's recent lines, and sets `Failed { code }`.
    fn blocked(&self, what: &str, blocked: &Blocked, events: &broadcast::Sender<Event>) {
        warn!(code = blocked.code, reason = %blocked.reason, "{what}");
        let text = format!("marsad: {what}: {}.", blocked.reason);
        lock(&self.runtime).note(events, &text);
        self.fail(blocked.code, events);
    }

    /// Sets `Failed { code }`. Only while no task runs: the supervisor owns
    /// the status then.
    fn fail(&self, code: &str, events: &broadcast::Sender<Event>) {
        lock(&self.runtime).set_failed(events, code);
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

/// Puts this data folder's `tag` as the `@id` of the `admin` object.
/// `GET /id/<tag>` then answers only on our own Caddy. Without an `admin`
/// object there is no tag, and the Caddy counts as not ours, which is the
/// safe side.
pub(super) fn tag_config(config: &mut Value, tag: &str) {
    if let Some(admin) = config.get_mut("admin").and_then(Value::as_object_mut) {
        admin.insert("@id".to_owned(), Value::String(tag.to_owned()));
    }
}

/// Writes `caddy.json` through a temp file in the same folder and a rename,
/// so Caddy never reads half a file.
pub(super) async fn write_config(path: &Path, config: &Value) -> io::Result<()> {
    let mut body = serde_json::to_vec_pretty(config).map_err(io::Error::other)?;
    body.push(b'\n');
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || marsa_core::fs::write_atomic(&path, &body, None))
        .await
        .map_err(io::Error::other)?
}
