//! One tokio task per managed process (architecture.md section 5).
//!
//! The task owns the `Child`: it starts the process, reads its output, waits
//! for it to exit, restarts it after a failure, and stops it when its
//! `CancellationToken` is cancelled. Like Supervisor running `queue:work` for
//! Laravel, but inside the daemon.

use marsa_core::codes;
use marsa_core::types::ServiceId;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use marsa_core::platform::process;
use marsa_core::services::{
    LogBuffer, ProgramNotAbsolute, RestartPolicy, STOP_COMMAND_GRACE, ServiceSpec, StopAction,
    StopCommand, StopMethod, TcpRequest, restart_delay,
};
use marsa_core::types::{Event, LogLine, LogStream, ServiceState, ServiceStatus};
use std::net::Ipv4Addr;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt as _, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::broadcast;
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use group::Group;

mod group;

/// A longer line is split, so a line without `\n` cannot grow without limit.
const MAX_LINE_BYTES: u64 = 8 * 1024;

/// How long to wait for the last output after an exit. A grandchild that keeps
/// a pipe open must not block a restart: its reader keeps draining anyway.
const DRAIN_WAIT: Duration = Duration::from_secs(1);

/// A run this long counts as healthy: the restart count starts again from 0,
/// so failures days apart do not use up the retries.
const STABLE_RUN: Duration = Duration::from_secs(60);

/// Under [`RestartPolicy::Always`], a clean exit after a run this long is
/// not counted: `queue:restart` after a deploy, or `--max-time`. A shorter
/// one counts as a failure, so a program that exits 0 at once ends `Failed`
/// instead of starting again each second without end.
pub const CLEAN_RUN_MIN: Duration = Duration::from_secs(5);

/// The failure code of a run that exited by itself, with no stop request.
pub const EXITED: &str = "exited";

/// The size at which a log file moves to `<name>.1`: 10 MiB.
pub const LOG_FILE_LIMIT: u64 = 10 * 1024 * 1024;

/// What a stop needs besides the waits of its method and the limits of its
/// requests: a `/bin/kill` per step (milliseconds, but up to 2 s each if one
/// hangs: 4 s for the two of PostgreSQL), the kill, and up to `DRAIN_WAIT`
/// (1 s) for the last lines, of the server and of a stop program.
pub const STOP_MARGIN: Duration = Duration::from_secs(5);

/// The most a shutdown waits for one process, whatever its stop method says,
/// so a stop method added later cannot hold the shutdown for long.
pub const STOP_LIMIT_MAX: Duration = Duration::from_secs(60);

/// The state and recent output of one service.
#[derive(Debug)]
pub struct ServiceRuntime {
    pub status: ServiceStatus,
    pub logs: LogBuffer,
    /// A run is `Starting`, not `Running`, until [`ServiceRuntime::set_ready`]:
    /// for a program that takes no clients for a while after its start.
    pub(crate) awaits_ready: bool,
}

impl ServiceRuntime {
    /// A service that has not run yet: `Stopped`, with no lines.
    pub fn new(id: ServiceId) -> Self {
        Self {
            status: ServiceStatus {
                id,
                state: ServiceState::Stopped,
                pid: None,
                restarts: 0,
                since_ms: None,
            },
            logs: LogBuffer::new(id),
            awaits_ready: false,
        }
    }

    /// [`ServiceRuntime::new`] for a service whose owner checks when it is
    /// ready.
    pub(crate) fn awaiting_ready(id: ServiceId) -> Self {
        Self {
            awaits_ready: true,
            ..Self::new(id)
        }
    }
}

/// What the daemon keeps per service. Dropping it does not stop the process:
/// call [`ServiceHandle::stop`].
#[derive(Debug)]
pub struct ServiceHandle {
    pub cancel: CancellationToken,
    /// Ends after a stop, or when the restarts run out.
    pub task: JoinHandle<()>,
    /// Short reads only: never hold the lock across `.await`.
    pub runtime: Arc<Mutex<ServiceRuntime>>,
    /// How long a stop may take: [`stop_limit`] of the spec's stop method. A
    /// shutdown waits at least this long before it aborts the task
    /// (`Daemon::stop_procs`), so PostgreSQL gets its SIGQUIT.
    pub stop_limit: Duration,
}

impl ServiceHandle {
    pub fn status(&self) -> ServiceStatus {
        lock(&self.runtime).status.clone()
    }

    pub fn recent_logs(&self) -> Vec<LogLine> {
        lock(&self.runtime).logs.recent()
    }

    /// Stops the process by its spec's `stop` method, and waits for its task.
    ///
    /// # Errors
    ///
    /// The task panicked. The process is killed anyway (`kill_on_drop`).
    pub async fn stop(self) -> Result<(), JoinError> {
        self.cancel.cancel();
        self.task.await
    }
}

/// How long a stop by `method` may take: the waits of its steps plus
/// [`STOP_MARGIN`], at most [`STOP_LIMIT_MAX`].
pub fn stop_limit(method: &StopMethod) -> Duration {
    let limit = method.total_wait().saturating_add(STOP_MARGIN);
    limit.min(STOP_LIMIT_MAX)
}

/// Starts managed processes.
pub struct Supervisor;

impl Supervisor {
    /// Starts the task that runs `spec`. Call it inside a tokio runtime.
    ///
    /// Each state change is sent on `events` as `Event::ServiceChanged`, and
    /// each output line as `Event::Log`. Lines also go to `<logs_dir>/<id>.log`.
    ///
    /// # Errors
    ///
    /// `program_not_absolute`: `marsad` never searches `PATH`.
    pub fn spawn(
        spec: ServiceSpec,
        events: broadcast::Sender<Event>,
        logs_dir: &Path,
    ) -> Result<ServiceHandle, ProgramNotAbsolute> {
        let runtime = Arc::new(Mutex::new(ServiceRuntime::new(spec.id)));
        Self::spawn_on(spec, events, logs_dir, runtime)
    }

    /// Like [`Supervisor::spawn`], on the runtime of an earlier run of the
    /// same service: its lines stay, and `seq` goes on from them, so a client
    /// never sees `seq` go back. The earlier task must have ended.
    ///
    /// # Errors
    ///
    /// `program_not_absolute`: `marsad` never searches `PATH`.
    pub fn spawn_on(
        spec: ServiceSpec,
        events: broadcast::Sender<Event>,
        logs_dir: &Path,
        runtime: Arc<Mutex<ServiceRuntime>>,
    ) -> Result<ServiceHandle, ProgramNotAbsolute> {
        if !spec.program.is_absolute() {
            return Err(ProgramNotAbsolute(spec.program));
        }
        // A stop program is looked up like the server: never in `PATH` or
        // the working folder, which Windows would search for a bare name.
        let relative = spec.stop.steps.iter().find_map(|step| match &step.action {
            StopAction::Run(command) if !command.program.is_absolute() => {
                Some(command.program.clone())
            }
            _ => None,
        });
        if let Some(program) = relative {
            return Err(ProgramNotAbsolute(program));
        }
        lock(&runtime).status = status(spec.id, ServiceState::Starting, None, 0);
        let shared = Shared {
            id: spec.id,
            runtime: Arc::clone(&runtime),
            events,
            log: Arc::new(Mutex::new(open_log(spec.id, logs_dir))),
        };
        let stop_limit = group::stop_limit(&spec);
        let cancel = CancellationToken::new();
        let task = tokio::spawn(supervise(spec, shared, cancel.clone()));
        Ok(ServiceHandle {
            cancel,
            task,
            runtime,
            stop_limit,
        })
    }
}

/// What the task and its two pipe readers share.
#[derive(Clone)]
struct Shared {
    id: ServiceId,
    runtime: Arc<Mutex<ServiceRuntime>>,
    events: broadcast::Sender<Event>,
    /// `logs/<id>.log`, shared by both readers so one writer moves it to
    /// `.1`. `None` if it cannot be opened: lines still reach the buffer and
    /// the events.
    log: Arc<Mutex<Option<ServiceLog>>>,
}

struct ServiceLog {
    file: LogFile,
    /// The last write failed: warn once, not for every line.
    failing: bool,
}

impl Shared {
    fn set_state(&self, state: ServiceState, pid: Option<u32>, restarts: u32) {
        let mut runtime = lock(&self.runtime);
        runtime.status = status(self.id, state, pid, restarts);
        // Sent under the lock, so events come in the order of the changes.
        // An error only means that nobody listens right now.
        let _ = self
            .events
            .send(Event::ServiceChanged(runtime.status.clone()));
    }

    fn record(&self, stream: LogStream, text: &str) -> LogLine {
        let mut runtime = lock(&self.runtime);
        let line = runtime.logs.push(stream, text, now_ms());
        // Under the lock, so events come in `seq` order.
        let _ = self.events.send(Event::Log(line.clone()));
        line
    }

    /// A line of the daemon's own, in the recent lines and the log file.
    fn note(&self, text: &str) {
        let line = self.record(LogStream::Stderr, text);
        self.write_log(&line.text);
    }

    /// Appends one line to the log file. A short synchronous write, like the
    /// `tracing` writer of `marsad.log`. After an error the next line tries
    /// again: a failed move to `.1` can be brief, for example while a virus
    /// scanner holds the file on Windows.
    fn write_log(&self, text: &str) {
        let mut log = lock(&self.log);
        let Some(log) = log.as_mut() else {
            return;
        };
        match log.file.write_all(format!("{text}\n").as_bytes()) {
            Ok(()) => log.failing = false,
            Err(err) => {
                if !log.failing {
                    warn!(service = %self.id, %err, "cannot write the log file");
                }
                log.failing = true;
            }
        }
    }
}

/// How one run of the process ended.
enum Exit {
    Stopped,
    /// By itself; `None` when the wait failed.
    Exited(Option<ExitStatus>),
}

async fn supervise(spec: ServiceSpec, shared: Shared, cancel: CancellationToken) {
    let mut restarts = 0;
    loop {
        shared.set_state(ServiceState::Starting, None, restarts);
        let started = Instant::now();
        // The code of the failure, and how the run ended in words.
        let (code, how) = match command(&spec).spawn() {
            Ok(child) => match run(child, &spec, &shared, &cancel, restarts).await {
                Exit::Stopped => break,
                Exit::Exited(status) => {
                    let ran = started.elapsed();
                    if ran >= STABLE_RUN {
                        restarts = 0;
                    }
                    let clean = status.is_some_and(|status| status.success());
                    if let RestartPolicy::Always { initial, .. } = spec.restart
                        && clean
                        && ran >= CLEAN_RUN_MIN
                    {
                        shared.note(&format!(
                            "marsad: exited cleanly, starting again in {}",
                            seconds(initial)
                        ));
                        tokio::select! {
                            () = tokio::time::sleep(initial) => continue,
                            () = cancel.cancelled() => break,
                        }
                    }
                    let how = status.map_or("exited".to_owned(), |status| status.to_string());
                    (EXITED, how)
                }
            },
            Err(err) => {
                warn!(service = %spec.id, %err, "cannot start");
                (codes::SPAWN_FAILED, "could not start".to_owned())
            }
        };
        let delay = restart_delay(spec.restart, restarts);
        // A worker whose restarts ran out says so: its log says why.
        let code = match delay {
            None if spec.id.is_worker() && code == EXITED => codes::WORKER_FAILED,
            _ => code,
        };
        let failed = ServiceState::Failed { code: code.into() };
        shared.set_state(failed, None, restarts);
        let Some(delay) = delay else {
            return;
        };
        if matches!(spec.restart, RestartPolicy::Always { .. }) {
            shared.note(&format!(
                "marsad: {how}, starting again in {}",
                seconds(delay)
            ));
        }
        tokio::select! {
            () = tokio::time::sleep(delay) => restarts += 1,
            () = cancel.cancelled() => break,
        }
    }
    shared.set_state(ServiceState::Stopped, None, restarts);
}

/// Runs the process once: until it exits, or until a stop request.
async fn run(
    mut child: Child,
    spec: &ServiceSpec,
    shared: &Shared,
    cancel: &CancellationToken,
    restarts: u32,
) -> Exit {
    let pid = child.id();
    let out = child
        .stdout
        .take()
        .map(|p| read_lines(p, LogStream::Stdout, shared));
    let err = child
        .stderr
        .take()
        .map(|p| read_lines(p, LogStream::Stderr, shared));
    info!(service = %spec.id, pid, "started");
    let state = if lock(&shared.runtime).awaits_ready {
        ServiceState::Starting
    } else {
        ServiceState::Running
    };
    shared.set_state(state, pid, restarts);
    let exit = tokio::select! {
        result = child.wait() => {
            match result {
                Ok(status) => {
                    warn!(service = %spec.id, %status, "exited");
                    Exit::Exited(Some(status))
                }
                Err(err) => {
                    warn!(service = %spec.id, %err, "cannot wait for the process");
                    Exit::Exited(None)
                }
            }
        }
        () = cancel.cancelled() => {
            shared.set_state(ServiceState::Stopping, pid, restarts);
            stop(&mut child, spec, shared).await;
            info!(service = %spec.id, "stopped");
            Exit::Stopped
        }
    };
    // The last lines come before the next state change.
    let drained = async {
        for reader in [out, err].into_iter().flatten() {
            let _ = reader.await;
        }
    };
    let _ = tokio::time::timeout(DRAIN_WAIT, drained).await;
    exit
}

/// Follows `spec.stop`: each step asks the process to stop, then waits up to
/// its limit for the exit. A step whose asking fails goes on to the next at
/// once. After the last step, a kill (`TerminateProcess` on Windows).
/// Windows sends no signals, so there a signal step is skipped.
///
/// With `own_group`, each signal and the kill go to the child's whole group
/// ([`Group`]), and what is left of it once the child has exited gets the
/// rest of that step's wait, then SIGKILL.
async fn stop(child: &mut Child, spec: &ServiceSpec, shared: &Shared) {
    let group = child
        .id()
        .filter(|_| spec.own_group)
        .map(|pid| Group::led_by(pid, spec.id));
    // A `Send` step goes to the port that was fixed at the spawn. Another
    // program could take it in the instant between our child's start and its
    // bind; the check before the start and the readiness probe make that
    // narrow, and only the fixed request of the stop could reach it.
    for step in &spec.stop.steps {
        // `None` once the exit is collected: the pid may be another
        // process's by then.
        let Some(pid) = child.id() else {
            break;
        };
        let deadline = tokio::time::Instant::now() + step.wait;
        let stopped = match &step.action {
            StopAction::Signal(signal) => match group::signal_child(pid, group, *signal).await {
                Ok(()) => Asked::Yes(tokio::time::timeout(step.wait, child.wait()).await.is_ok()),
                // Windows: no signals. The next step comes, then the kill.
                Err(err) if err.kind() == io::ErrorKind::Unsupported => Asked::No,
                Err(err) => {
                    warn!(service = %spec.id, %err, "{} failed", signal.name());
                    Asked::No
                }
            },
            StopAction::Send(request) => ask(child, request, step.wait, spec.id).await,
            StopAction::Run(command) => run_command(child, command, step.wait, shared).await,
        };
        match stopped {
            Asked::Yes(true) => {
                if let Some(group) = group {
                    group.end_after_leader(deadline).await;
                }
                return;
            }
            Asked::Yes(false) => {
                let name = step.action.name();
                warn!(service = %spec.id, wait = ?step.wait, "still running after {name}");
            }
            Asked::No => {}
        }
    }
    // The group first, while the child is not yet waited for: `kill` waits.
    if let (Some(group), Some(_)) = (group, child.id()) {
        group.kill_with_leader().await;
    }
    if let Err(err) = child.kill().await {
        warn!(service = %spec.id, %err, "cannot kill the process");
    }
    if let Some(group) = group {
        group.end_after_kill().await;
    }
}

/// Whether a step reached the process, and if so whether it exited within
/// the step's wait.
enum Asked {
    Yes(bool),
    No,
}

/// Sends `request` ([`send_request`]), then waits up to `wait` for the
/// exit. A reply ends the wait early: the server refused (Valkey answers
/// `-ERR Errors trying to SHUTDOWN` when it cannot save), so the next step
/// need not wait for an exit that will not come. A closed connection is the
/// server on its way out, and the wait goes on. The connection stays open
/// until the step ends, so the server reads the request to its end.
async fn ask(child: &mut Child, request: &TcpRequest, wait: Duration, id: ServiceId) -> Asked {
    let name = request.name;
    let mut stream = match send_request(request).await {
        Ok(stream) => stream,
        Err(err) => {
            warn!(service = %id, %err, port = request.port, "{name} could not be sent");
            return Asked::No;
        }
    };
    tokio::select! {
        exited = tokio::time::timeout(wait, child.wait()) => Asked::Yes(exited.is_ok()),
        reply = refusal(&mut stream) => {
            warn!(service = %id, ?reply, "{name} was refused");
            Asked::No
        }
    }
}

/// Connects to `127.0.0.1:<port>` and writes the request's bytes, each
/// within the request's limit. Only loopback: [`TcpRequest`] has no host.
async fn send_request(request: &TcpRequest) -> io::Result<TcpStream> {
    let limit = request.limit;
    let timed_out = |what: &str| {
        let text = format!("the {what} took longer than {} ms", limit.as_millis());
        io::Error::new(io::ErrorKind::TimedOut, text)
    };
    let address = (Ipv4Addr::LOCALHOST, request.port);
    let mut stream = tokio::time::timeout(limit, TcpStream::connect(address))
        .await
        .map_err(|_| timed_out("connect"))??;
    tokio::time::timeout(limit, stream.write_all(&request.bytes))
        .await
        .map_err(|_| timed_out("write"))??;
    Ok(stream)
}

/// The first bytes the server sends back, as text, for the log. Never ends
/// when the server closes the connection or it breaks: that is the server
/// stopping, and the caller waits for its exit instead.
async fn refusal(stream: &mut TcpStream) -> String {
    let mut reply = [0_u8; 256];
    match stream.read(&mut reply).await {
        Ok(read) if read > 0 => {
            let text = String::from_utf8_lossy(&reply[..read]);
            text.trim_end().to_owned()
        }
        _ => std::future::pending().await,
    }
}

/// Runs `command` ([`StopCommand`]) and waits up to `wait`, from its start,
/// for the server's exit. What the program prints goes to the service's log,
/// and so do the daemon's own lines about it, so `marsa logs <id>` says why
/// a server was ended. Once the server has exited, the program gets
/// [`STOP_COMMAND_GRACE`] for its last lines; one that still runs then, or
/// at the end of the wait, is killed (`kill_on_drop`).
async fn run_command(
    child: &mut Child,
    command: &StopCommand,
    wait: Duration,
    shared: &Shared,
) -> Asked {
    let name = command.name;
    let deadline = tokio::time::Instant::now() + wait;
    let mut cmd = Command::new(&command.program);
    cmd.args(&command.args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Not the daemon's environment: the program is only asked to stop a
    // server.
    process::keep_start_variable(&mut cmd);
    process::hide_console_window(&mut cmd);
    shared.note(&format!("marsad: asking {} to stop with {name}", shared.id));
    let mut program = match cmd.spawn() {
        Ok(program) => program,
        Err(err) => {
            warn!(service = %shared.id, %err, "{name} could not be started");
            shared.note(&format!("marsad: {name} could not be started: {err}"));
            return Asked::No;
        }
    };
    let readers = [
        program
            .stdout
            .take()
            .map(|pipe| read_lines(pipe, LogStream::Stdout, shared)),
        program
            .stderr
            .take()
            .map(|pipe| read_lines(pipe, LogStream::Stderr, shared)),
    ];
    let mut ended = false;
    let exited = loop {
        tokio::select! {
            status = program.wait(), if !ended => {
                ended = true;
                report(command, shared, &status);
            }
            _ = child.wait() => break true,
            () = tokio::time::sleep_until(deadline) => break false,
        }
    };
    if exited
        && !ended
        && let Ok(status) = tokio::time::timeout(STOP_COMMAND_GRACE, program.wait()).await
    {
        report(command, shared, &status);
    }
    if !exited {
        let secs = wait.as_secs_f32();
        shared.note(&format!(
            "marsad: {} still runs {secs} s after {name}",
            shared.id
        ));
    }
    // Killed here if it still runs; its pipes close with it.
    drop(program);
    let drained = async {
        for reader in readers.into_iter().flatten() {
            let _ = reader.await;
        }
    };
    let _ = tokio::time::timeout(DRAIN_WAIT, drained).await;
    Asked::Yes(exited)
}

/// A line when the stop program ended with an error, or could not be waited
/// for. Nothing when it ended well.
fn report(command: &StopCommand, shared: &Shared, ended: &io::Result<ExitStatus>) {
    let name = command.name;
    let why = match ended {
        Ok(status) if status.success() => return,
        Ok(status) => status.to_string(),
        Err(err) => err.to_string(),
    };
    warn!(service = %shared.id, %why, "{name} failed");
    shared.note(&format!(
        "marsad: {name} failed ({why}). {}",
        command.on_failure
    ));
}

fn command(spec: &ServiceSpec) -> Command {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .envs(&spec.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // ADR 0009: a panic or a return from `main` kills the child too.
        .kill_on_drop(true);
    // Only where asked: a Ctrl+C in `marsad`'s terminal must reach the rest.
    if spec.own_group {
        process::own_group(&mut cmd);
    }
    for name in &spec.env_remove {
        cmd.env_remove(name);
    }
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    process::hide_console_window(&mut cmd);
    cmd
}

/// Reads one pipe until it closes: never stop early, because a process whose
/// pipe is full blocks (ADR 0009).
fn read_lines(
    pipe: impl AsyncRead + Unpin + Send + 'static,
    stream: LogStream,
    shared: &Shared,
) -> JoinHandle<()> {
    let shared = shared.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::new(pipe);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let mut limited = (&mut reader).take(MAX_LINE_BYTES);
            match limited.read_until(b'\n', &mut buf).await {
                Ok(0) => break,
                Ok(_) => {
                    let line = shared.record(stream, &String::from_utf8_lossy(&buf));
                    shared.write_log(&line.text);
                }
                Err(err) => {
                    warn!(service = %shared.id, %err, "cannot read the output");
                    break;
                }
            }
        }
    })
}

/// Opens `logs_dir/<id>.log`, or warns and gives `None`.
fn open_log(id: ServiceId, logs_dir: &Path) -> Option<ServiceLog> {
    let path = logs_dir.join(format!("{id}.log"));
    let file = fs::create_dir_all(logs_dir)
        .and_then(|()| LogFile::open(&path, LOG_FILE_LIMIT))
        .inspect_err(|err| warn!(service = %id, %err, "cannot open the log file"))
        .ok()?;
    Some(ServiceLog {
        file,
        failing: false,
    })
}

/// An append-only log file that keeps one older part. When a write would take
/// it past `limit`, the file moves to `<name>.1` (over the older part) and a
/// new file starts. So a noisy service, or a web page that sends rejected
/// requests to `marsad` without end, fills at most about twice the limit.
///
/// A write that fails leaves the size as it was, so the next write tries
/// again and the file still never grows past the limit.
#[derive(Debug)]
pub struct LogFile {
    path: PathBuf,
    /// `<name>.1`.
    older: PathBuf,
    limit: u64,
    /// `None` after a move, until the next write opens the new file.
    file: Option<File>,
    size: u64,
}

impl LogFile {
    /// Opens `path` to append, and creates it if needed. The parent folder
    /// must exist.
    ///
    /// # Errors
    ///
    /// The file cannot be opened.
    pub fn open(path: &Path, limit: u64) -> io::Result<Self> {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".1");
        let mut log = Self {
            path: path.to_path_buf(),
            older: path.with_file_name(name),
            limit,
            file: None,
            size: 0,
        };
        log.file()?;
        Ok(log)
    }

    /// The open file. Opens it first if a move closed it.
    fn file(&mut self) -> io::Result<&mut File> {
        match &mut self.file {
            Some(file) => Ok(file),
            slot @ None => {
                let file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?;
                self.size = file.metadata()?.len();
                Ok(slot.insert(file))
            }
        }
    }

    /// Moves the file to `<name>.1`, over the older part.
    fn rotate(&mut self) -> io::Result<()> {
        // Closed first: Windows cannot always rename an open file.
        self.file = None;
        fs::rename(&self.path, &self.older)?;
        self.size = 0;
        Ok(())
    }
}

impl Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        // `size > 0`: a write larger than the limit still goes into an empty
        // file, instead of moving files without end.
        if self.size > 0 && self.size.saturating_add(len) > self.limit {
            self.rotate()?;
        }
        let written = self.file()?.write(buf)?;
        self.size += u64::try_from(written).unwrap_or(u64::MAX);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().map_or(Ok(()), Write::flush)
    }
}

/// `4s`, or `500ms` under a second, for the notes in a worker's log.
fn seconds(wait: Duration) -> String {
    if wait >= Duration::from_secs(1) {
        format!("{}s", wait.as_secs())
    } else {
        format!("{}ms", wait.as_millis())
    }
}

fn status(id: ServiceId, state: ServiceState, pid: Option<u32>, restarts: u32) -> ServiceStatus {
    ServiceStatus {
        id,
        state,
        pid,
        restarts,
        since_ms: Some(now_ms()),
    }
}

/// A poisoned lock still holds whole values: every write replaces a field or
/// appends a line.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn now_ms() -> u64 {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn a_stop_may_take_its_waits_and_the_margin_up_to_a_minute() {
        use marsa_core::services::Signal;
        let secs = Duration::from_secs;
        assert_eq!(stop_limit(&StopMethod::term(secs(2))), secs(7));
        // PostgreSQL: SIGINT and SIGQUIT, 10 s each.
        let postgres = StopMethod::signals([(Signal::Int, secs(10)), (Signal::Quit, secs(10))]);
        assert_eq!(stop_limit(&postgres), secs(25));
        // A method that would wait longer is cut at a minute.
        let endless = StopMethod::signals([(Signal::Int, Duration::MAX)]);
        assert_eq!(stop_limit(&endless), STOP_LIMIT_MAX);
        let long = StopMethod::term(secs(90));
        assert_eq!(stop_limit(&long), secs(60));
    }

    #[test]
    fn a_log_file_moves_to_dot_1_past_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caddy.log");
        let older = dir.path().join("caddy.log.1");
        // Each line is 7 bytes; two fit in 20.
        let mut log = LogFile::open(&path, 20).unwrap();
        for n in 1..=5 {
            log.write_all(format!("line {n}\n").as_bytes()).unwrap();
        }
        // 1 and 2 filled the first file, 3 and 4 the second, 5 the third. The
        // second replaced the first as `.1`.
        assert_eq!(read(&older), "line 3\nline 4\n");
        assert_eq!(read(&path), "line 5\n");
    }

    #[test]
    fn a_log_file_counts_what_it_already_holds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marsad.log");
        fs::write(&path, "0123456789abcdef").unwrap();
        let mut log = LogFile::open(&path, 20).unwrap();
        log.write_all(b"new line\n").unwrap();
        assert_eq!(read(&dir.path().join("marsad.log.1")), "0123456789abcdef");
        assert_eq!(read(&path), "new line\n");
    }

    #[test]
    fn a_write_larger_than_the_limit_goes_into_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.log");
        let mut log = LogFile::open(&path, 4).unwrap();
        log.write_all(b"longer than four\n").unwrap();
        assert_eq!(read(&path), "longer than four\n");
        assert!(!dir.path().join("big.log.1").exists());
        log.write_all(b"next\n").unwrap();
        assert_eq!(read(&dir.path().join("big.log.1")), "longer than four\n");
        assert_eq!(read(&path), "next\n");
    }

    #[test]
    fn a_failed_move_writes_nothing_and_is_tried_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.log");
        let older = dir.path().join("svc.log.1");
        let mut log = LogFile::open(&path, 10).unwrap();
        log.write_all(b"12345678\n").unwrap();
        // A folder named `svc.log.1` makes the rename fail on every OS.
        fs::create_dir(&older).unwrap();
        fs::write(older.join("keep"), "x").unwrap();
        assert!(log.write_all(b"second\n").is_err());
        assert_eq!(read(&path), "12345678\n");
        fs::remove_dir_all(&older).unwrap();
        log.write_all(b"second\n").unwrap();
        assert_eq!(read(&older), "12345678\n");
        assert_eq!(read(&path), "second\n");
    }
}
