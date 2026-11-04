//! Starts Caddy through the supervisor and talks to its admin API. Task M3-07.
//!
//! `marsa_core::caddy` builds the config as data. This module writes it to
//! `caddy/caddy.json`, runs `bin/caddy run --config caddy/caddy.json` under
//! the supervisor, and waits until the admin API answers. Like a Laravel job
//! that starts a queue worker, then checks that it is healthy.
//!
//! Before each start, [`orphan`] stops a Caddy that a crashed daemon left
//! behind, or refuses to start when another program holds our ports. A
//! `stop` with no Caddy running stops such an orphan too. After each spawn
//! the watcher writes `run/caddy.pid`, and removes it once Caddy has exited
//! (ADR 0009). The `admin` object in `caddy.json` carries this data folder's
//! tag, `marsa-instance-<id>`, so the daemon of another folder never takes
//! our Caddy for its own.

pub mod orphan;

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use marsa_core::caddy::{CaddyInput, CaddyPaths, caddy_config, caddy_env};
use marsa_core::orphan::instance_tag;
use marsa_core::platform::process;
use marsa_core::services::{ProgramNotAbsolute, RestartPolicy, ServiceSpec};
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

/// What Caddy prints when an address is taken: Go's text on macOS and Linux,
/// and the English Windows text. Compared in lower case.
const PORT_IN_USE: [&str; 2] = [
    "address already in use",
    "only one usage of each socket address",
];

/// How the supervisor runs Caddy: `bin/caddy run --config caddy/caddy.json`,
/// with Caddy's own data inside `caddy/`.
pub fn caddy_spec(paths: &CaddyPaths) -> ServiceSpec {
    ServiceSpec {
        id: CADDY_ID.to_owned(),
        program: paths.program.clone(),
        // `to_string_lossy` as in `caddy_env`: the app data dir comes from the
        // OS and is valid Unicode in practice.
        args: vec![
            "run".to_owned(),
            "--config".to_owned(),
            paths.config_file.to_string_lossy().into_owned(),
        ],
        env: caddy_env(paths),
        cwd: None,
        restart: RestartPolicy::OnFailure {
            max_retries: 3,
            initial: Duration::from_millis(500),
            max: Duration::from_secs(5),
        },
        stop_grace: Duration::from_secs(2),
    }
}

/// Why a service action failed. The route turns it into an `ErrorBody`.
#[derive(Debug)]
pub enum ActionError {
    /// No program at `bin/caddy`. In M3 the owner puts it there by hand.
    NotInstalled(PathBuf),
    /// Another action on the service has not finished.
    Busy,
    /// `caddy.json` cannot be written.
    ConfigWrite(io::Error),
    ProgramNotAbsolute(ProgramNotAbsolute),
}

impl ActionError {
    /// Stable error name for the UI.
    pub fn code(&self) -> &'static str {
        match self {
            ActionError::NotInstalled(_) => "caddy_not_installed",
            ActionError::Busy => "service_busy",
            ActionError::ConfigWrite(_) => "caddy_config_write",
            ActionError::ProgramNotAbsolute(err) => err.code(),
        }
    }
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
    /// How Caddy runs: [`caddy_spec`]. Tests put a fake program here.
    pub spec: ServiceSpec,
    /// How long `start` waits for the admin API.
    pub ready_limit: Duration,
    /// Who runs under a pid, and signals, for the orphan check. Tests put a
    /// recorder here.
    pub processes: Arc<dyn Processes>,
    /// How long each step of an orphan's stop waits.
    pub ladder: Ladder,
    /// The status and recent lines. Kept across runs, so `seq` never goes
    /// back and the lines of a failed run stay readable.
    runtime: Arc<Mutex<ServiceRuntime>>,
    /// Set while an action runs.
    busy: AtomicBool,
}

impl CaddyManager {
    /// Caddy in `data_dir`, on Marsa's ports. Nothing is read or started.
    pub fn new(data_dir: &Path) -> Self {
        let paths = CaddyPaths::in_dir(data_dir);
        Self {
            spec: caddy_spec(&paths),
            paths,
            input: CaddyInput::default(),
            ready_limit: READY_LIMIT,
            processes: Arc::new(SystemProcesses),
            ladder: Ladder::default(),
            runtime: Arc::new(Mutex::new(ServiceRuntime::new(CADDY_ID))),
            busy: AtomicBool::new(false),
        }
    }

    pub fn status(&self) -> ServiceStatus {
        lock(&self.runtime).status.clone()
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
    /// `service_busy` while another action runs: a flag, not a lock held
    /// across `.await`, keeps two starts from running two Caddys.
    /// `caddy_not_installed`, `caddy_config_write`, and `program_not_absolute`
    /// from `start`: the status is then `Failed` with the same code.
    pub async fn run(
        &self,
        daemon: &Daemon,
        action: ServiceAction,
    ) -> Result<ServiceStatus, ActionError> {
        if self.busy.swap(true, Ordering::AcqRel) {
            return Err(ActionError::Busy);
        }
        let _busy = ActionGuard(&self.busy);
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
        if !self.spec.program.is_file() {
            self.fail("caddy_not_installed", &daemon.events);
            return Err(ActionError::NotInstalled(self.spec.program.clone()));
        }
        // The tag goes into `caddy.json`, and the check below looks for it.
        let tag = match self.instance_tag() {
            Ok(tag) => tag,
            Err(err) => {
                self.fail("caddy_config_write", &daemon.events);
                return Err(ActionError::ConfigWrite(err));
            }
        };
        // ADR 0009: stop our own orphan; touch nothing that is not ours.
        let pid_file = PidFile::new(&daemon.data_dir, &self.spec.program);
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
        if let Err(err) = write_config(&self.paths.config_file, &self.input, &tag).await {
            self.fail("caddy_config_write", &daemon.events);
            return Err(ActionError::ConfigWrite(err));
        }
        // Before the process starts, so the watcher cannot miss its first line.
        let lines = daemon.events.subscribe();
        let first_seq = self.recent_logs().last().map_or(0, |line| line.seq + 1);
        let spec = ServiceSpec {
            id: CADDY_ID.to_owned(),
            ..self.spec.clone()
        };
        let logs_dir = daemon.data_dir.join("logs");
        let runtime = Arc::clone(&self.runtime);
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
        match self.wait_ready(daemon, admin).await {
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

    /// Polls `GET /config/` until it answers, the task ends, or the limit.
    async fn wait_ready(&self, daemon: &Daemon, admin: &Admin) -> Ready {
        let poll = async {
            loop {
                if !daemon.proc_alive(CADDY_ID) {
                    return Ready::Ended;
                }
                if admin.ready().await {
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
    async fn stop(&self, daemon: &Daemon, admin: &Admin) -> bool {
        if !daemon.proc_alive(CADDY_ID) {
            return self.stop_orphan(daemon, admin).await;
        }
        admin.stop().await;
        // `None`: a shutdown took the handle and stops it.
        if let Some(handle) = daemon.take_proc(CADDY_ID)
            && let Err(err) = handle.stop().await
        {
            warn!(%err, "the caddy task failed");
        }
        true
    }

    /// Stops our orphan from a crashed daemon, and nothing else: a conflict
    /// or another Marsa's Caddy is left alone, and so is a stale pid file.
    /// `false` when the orphan would not stop.
    async fn stop_orphan(&self, daemon: &Daemon, admin: &Admin) -> bool {
        // Without the program there is no Caddy of ours to recognize.
        if !self.spec.program.is_file() {
            return true;
        }
        let tag = match self.instance_tag() {
            Ok(tag) => tag,
            Err(err) => {
                warn!(%err, "cannot read the instance id; not looking for an orphan");
                return true;
            }
        };
        let pid_file = PidFile::new(&daemon.data_dir, &self.spec.program);
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

/// The line before a reap, in Caddy's recent lines.
const STOPPING_ORPHAN: &str = "marsad: Stopping an old Caddy left by a crashed marsad.";

/// Clears the busy flag when the action ends, also when the request is
/// dropped half way.
struct ActionGuard<'a>(&'a AtomicBool);

impl Drop for ActionGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

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

/// The config for `input`, with this data folder's `tag` as the `@id` of the
/// `admin` object. `GET /id/<tag>` then answers only on our own Caddy.
/// Without an `admin` object there is no tag, and the Caddy counts as not
/// ours, which is the safe side.
fn tagged_config(input: &CaddyInput, tag: &str) -> Value {
    let mut config = caddy_config(input);
    if let Some(admin) = config.get_mut("admin").and_then(Value::as_object_mut) {
        admin.insert("@id".to_owned(), Value::String(tag.to_owned()));
    }
    config
}

/// Writes `caddy.json` through a temp file in the same folder and a rename,
/// so Caddy never reads half a file.
async fn write_config(path: &Path, input: &CaddyInput, tag: &str) -> io::Result<()> {
    let config = tagged_config(input, tag);
    let mut body = serde_json::to_vec_pretty(&config).map_err(io::Error::other)?;
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

    /// `GET /config/` answers with a success.
    async fn ready(&self) -> bool {
        let Some(http) = &self.http else {
            return false;
        };
        let url = format!("{}/config/", self.base);
        let response = http.get(url).send().await;
        response.is_ok_and(|response| response.status().is_success())
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::consts::EXE_SUFFIX;

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
    fn caddy_spec_runs_bin_caddy_with_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let paths = CaddyPaths::in_dir(dir.path());
        let spec = caddy_spec(&paths);

        let program = dir.path().join("bin").join(format!("caddy{EXE_SUFFIX}"));
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
        assert_eq!(spec.stop_grace, Duration::from_secs(2));
    }

    #[test]
    fn a_taken_port_is_found_on_stderr_only() {
        let go = "Error: loading initial config: loading new config: http app module: start: \
                  listening on 127.0.0.1:18080: listen tcp 127.0.0.1:18080: bind: address already in use";
        let windows = "listen tcp 127.0.0.1:18080: bind: Only one usage of each socket \
                       address (protocol/network address/port) is normally permitted.";
        assert!(is_port_conflict(&line(LogStream::Stderr, go)));
        assert!(is_port_conflict(&line(LogStream::Stderr, windows)));
        assert!(!is_port_conflict(&line(LogStream::Stdout, go)));
        let other = "bind: permission denied";
        assert!(!is_port_conflict(&line(LogStream::Stderr, other)));
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
        write_config(&path, &input, tag).await.unwrap();
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
}
