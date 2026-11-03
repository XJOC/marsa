//! One tokio task per managed process (architecture.md section 5).
//!
//! The task owns the `Child`: it starts the process, reads its output, waits
//! for it to exit, restarts it after a failure, and stops it when its
//! `CancellationToken` is cancelled. Like Supervisor running `queue:work` for
//! Laravel, but inside the daemon.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use marsa_core::platform::process;
use marsa_core::services::{LogBuffer, ProgramNotAbsolute, ServiceSpec, restart_delay};
use marsa_core::types::{Event, LogLine, LogStream, ServiceState, ServiceStatus};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::broadcast;
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// A longer line is split, so a line without `\n` cannot grow without limit.
const MAX_LINE_BYTES: u64 = 8 * 1024;

/// How long to wait for the last output after an exit. A grandchild that keeps
/// a pipe open must not block a restart: its reader keeps draining anyway.
const DRAIN_WAIT: Duration = Duration::from_secs(1);

/// The state and recent output of one service.
#[derive(Debug)]
pub struct ServiceRuntime {
    pub status: ServiceStatus,
    pub logs: LogBuffer,
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
}

impl ServiceHandle {
    pub fn status(&self) -> ServiceStatus {
        lock(&self.runtime).status.clone()
    }

    pub fn recent_logs(&self) -> Vec<LogLine> {
        lock(&self.runtime).logs.recent()
    }

    /// Stops the process (SIGTERM, then a kill after `stop_grace`) and waits
    /// for its task.
    ///
    /// # Errors
    ///
    /// The task panicked. The process is killed anyway (`kill_on_drop`).
    pub async fn stop(self) -> Result<(), JoinError> {
        self.cancel.cancel();
        self.task.await
    }
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
        if !spec.program.is_absolute() {
            return Err(ProgramNotAbsolute(spec.program));
        }
        let runtime = Arc::new(Mutex::new(ServiceRuntime {
            status: status(&spec.id, ServiceState::Starting, None, 0),
            logs: LogBuffer::new(spec.id.clone()),
        }));
        let shared = Shared {
            id: spec.id.clone(),
            runtime: Arc::clone(&runtime),
            events,
            log_path: logs_dir.join(format!("{}.log", spec.id)),
        };
        let cancel = CancellationToken::new();
        let task = tokio::spawn(supervise(spec, shared, cancel.clone()));
        Ok(ServiceHandle {
            cancel,
            task,
            runtime,
        })
    }
}

/// What the task and its two pipe readers share.
#[derive(Clone)]
struct Shared {
    id: String,
    runtime: Arc<Mutex<ServiceRuntime>>,
    events: broadcast::Sender<Event>,
    log_path: PathBuf,
}

impl Shared {
    fn set_state(&self, state: ServiceState, pid: Option<u32>, restarts: u32) {
        let mut runtime = lock(&self.runtime);
        runtime.status = status(&self.id, state, pid, restarts);
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
}

/// How one run of the process ended.
enum Exit {
    Stopped,
    Exited,
}

async fn supervise(spec: ServiceSpec, shared: Shared, cancel: CancellationToken) {
    if let Some(dir) = shared.log_path.parent()
        && let Err(err) = tokio::fs::create_dir_all(dir).await
    {
        warn!(service = %spec.id, %err, "cannot create the logs folder");
    }
    let mut restarts = 0;
    loop {
        shared.set_state(ServiceState::Starting, None, restarts);
        let code = match command(&spec).spawn() {
            Ok(child) => match run(child, &spec, &shared, &cancel, restarts).await {
                Exit::Stopped => break,
                Exit::Exited => "exited",
            },
            Err(err) => {
                warn!(service = %spec.id, %err, "cannot start");
                "spawn_failed"
            }
        };
        let failed = ServiceState::Failed { code: code.into() };
        shared.set_state(failed, None, restarts);
        let Some(delay) = restart_delay(spec.restart, restarts) else {
            return;
        };
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
    shared.set_state(ServiceState::Running, pid, restarts);
    let exit = tokio::select! {
        result = child.wait() => {
            match result {
                Ok(status) => warn!(service = %spec.id, %status, "exited"),
                Err(err) => warn!(service = %spec.id, %err, "cannot wait for the process"),
            }
            Exit::Exited
        }
        () = cancel.cancelled() => {
            shared.set_state(ServiceState::Stopping, pid, restarts);
            stop(&mut child, spec).await;
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

/// SIGTERM, up to `stop_grace` to exit, then a kill. On Windows: a kill.
async fn stop(child: &mut Child, spec: &ServiceSpec) {
    if let Some(pid) = child.id() {
        match process::terminate(pid).await {
            Ok(true) => {
                if tokio::time::timeout(spec.stop_grace, child.wait())
                    .await
                    .is_ok()
                {
                    return;
                }
            }
            Ok(false) => {}
            Err(err) => warn!(service = %spec.id, %err, "SIGTERM failed"),
        }
    }
    if let Err(err) = child.kill().await {
        warn!(service = %spec.id, %err, "cannot kill the process");
    }
}

fn command(spec: &ServiceSpec) -> Command {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .envs(&spec.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // ADR 0009: a panic or a return from `main` kills the child too. No
        // `process_group(0)`: a Ctrl+C in the terminal must reach the child.
        .kill_on_drop(true);
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
        // Without the file, lines still reach the buffer and the events.
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&shared.log_path)
            .await
            .inspect_err(|err| warn!(service = %shared.id, %err, "cannot open the log file"))
            .ok();
        let mut reader = BufReader::new(pipe);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let mut limited = (&mut reader).take(MAX_LINE_BYTES);
            match limited.read_until(b'\n', &mut buf).await {
                Ok(0) => break,
                Ok(_) => {
                    let line = shared.record(stream, &String::from_utf8_lossy(&buf));
                    if let Some(out) = &mut file
                        && let Err(err) = out.write_all(format!("{}\n", line.text).as_bytes()).await
                    {
                        warn!(service = %shared.id, %err, "cannot write the log file");
                        file = None;
                    }
                }
                Err(err) => {
                    warn!(service = %shared.id, %err, "cannot read the output");
                    break;
                }
            }
        }
        if let Some(mut out) = file {
            let _ = out.flush().await;
        }
    })
}

fn status(id: &str, state: ServiceState, pid: Option<u32>, restarts: u32) -> ServiceStatus {
    ServiceStatus {
        id: id.to_owned(),
        state,
        pid,
        restarts,
        since_ms: Some(now_ms()),
    }
}

/// A poisoned lock still holds whole values: every write replaces a field.
fn lock(runtime: &Mutex<ServiceRuntime>) -> MutexGuard<'_, ServiceRuntime> {
    runtime.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> u64 {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}
