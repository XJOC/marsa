//! One tokio task per managed process (architecture.md section 5).
//!
//! The task owns the `Child`: it starts the process, reads its output, waits
//! for it to exit, restarts it after a failure, and stops it when its
//! `CancellationToken` is cancelled. Like Supervisor running `queue:work` for
//! Laravel, but inside the daemon.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use marsa_core::platform::process;
use marsa_core::services::{LogBuffer, ProgramNotAbsolute, ServiceSpec, restart_delay};
use marsa_core::types::{Event, LogLine, LogStream, ServiceState, ServiceStatus};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
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

/// A run this long counts as healthy: the restart count starts again from 0,
/// so failures days apart do not use up the retries.
const STABLE_RUN: Duration = Duration::from_secs(60);

/// The size at which a log file moves to `<name>.1`: 10 MiB.
pub const LOG_FILE_LIMIT: u64 = 10 * 1024 * 1024;

/// The state and recent output of one service.
#[derive(Debug)]
pub struct ServiceRuntime {
    pub status: ServiceStatus,
    pub logs: LogBuffer,
}

impl ServiceRuntime {
    /// A service that has not run yet: `Stopped`, with no lines.
    pub fn new(id: &str) -> Self {
        Self {
            status: ServiceStatus {
                id: id.to_owned(),
                state: ServiceState::Stopped,
                pid: None,
                restarts: 0,
                since_ms: None,
            },
            logs: LogBuffer::new(id),
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
        let runtime = Arc::new(Mutex::new(ServiceRuntime::new(&spec.id)));
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
        lock(&runtime).status = status(&spec.id, ServiceState::Starting, None, 0);
        let shared = Shared {
            id: spec.id.clone(),
            runtime: Arc::clone(&runtime),
            events,
            log: Arc::new(Mutex::new(open_log(&spec.id, logs_dir))),
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
    Exited,
}

async fn supervise(spec: ServiceSpec, shared: Shared, cancel: CancellationToken) {
    let mut restarts = 0;
    loop {
        shared.set_state(ServiceState::Starting, None, restarts);
        let started = Instant::now();
        let code = match command(&spec).spawn() {
            Ok(child) => match run(child, &spec, &shared, &cancel, restarts).await {
                Exit::Stopped => break,
                Exit::Exited => {
                    if started.elapsed() >= STABLE_RUN {
                        restarts = 0;
                    }
                    "exited"
                }
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
fn open_log(id: &str, logs_dir: &Path) -> Option<ServiceLog> {
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

fn status(id: &str, state: ServiceState, pid: Option<u32>, restarts: u32) -> ServiceStatus {
    ServiceStatus {
        id: id.to_owned(),
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
