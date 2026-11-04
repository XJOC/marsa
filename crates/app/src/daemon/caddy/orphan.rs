//! Finds and stops a Caddy that a crashed `marsad` left running (ADR 0009).
//! Task M3-11.
//!
//! Before each start the manager inspects three things: `run/caddy.pid`, the
//! answer of our admin port to `GET /id/marsa-srv`, and the two ports.
//! `marsa_core::orphan::decide` picks the action. A reap is `POST /stop`, then
//! SIGTERM, then SIGKILL; before each signal the pid is checked again, path
//! and start time. Anything that is not ours is never signalled.

use std::fmt;
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant};

use marsa_core::caddy::{CaddyInput, SERVER_ID};
use marsa_core::orphan::{
    self, Action, AdminState, Inspection, Liveness, PidRecord, PidfileState, PortState, Strategy,
};
use marsa_core::platform::process::{self, Signal};
use tracing::{debug, info, warn};

use super::Admin;

/// How often a wait checks the ports again.
const POLL: Duration = Duration::from_millis(50);

/// The largest answer to `GET /id/marsa-srv` that is read. The server's config
/// grows with the sites; a longer answer counts as not ours.
const TAG_BODY_MAX: usize = 16 * 1024 * 1024;

/// A boxed future, so [`Processes`] works as `dyn Processes`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Who runs under a pid, and signals. [`SystemProcesses`] asks the OS; a test
/// records the signals instead.
pub trait Processes: fmt::Debug + Send + Sync {
    fn identity(&self, pid: u32) -> BoxFuture<'_, Liveness>;
    /// Sends `signal` to `pid`. The caller checked the identity right before.
    fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>>;
}

/// The real OS: `marsa_core::platform::process`.
#[derive(Debug)]
pub struct SystemProcesses;

impl Processes for SystemProcesses {
    fn identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
        Box::pin(process::identity(pid))
    }

    fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(process::send_signal(pid, signal))
    }
}

/// How long each step of a reap waits for the orphan to let go of the ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ladder {
    /// After `POST /stop`, counted from the request.
    pub stop: Duration,
    pub term: Duration,
    pub kill: Duration,
}

impl Default for Ladder {
    /// ADR 0009: `/stop`, SIGTERM after 2 s, SIGKILL 5 s later.
    fn default() -> Self {
        Self {
            stop: Duration::from_secs(2),
            term: Duration::from_secs(5),
            kill: Duration::from_secs(2),
        }
    }
}

/// `run/caddy.pid`, and the ways the OS may spell our Caddy's path.
#[derive(Debug)]
pub(super) struct PidFile {
    path: PathBuf,
    accepted: Vec<PathBuf>,
}

impl PidFile {
    pub(super) fn new(data_dir: &Path, program: &Path) -> Self {
        let mut accepted = vec![program.to_path_buf()];
        // Linux reports the path with links resolved. Only compared, never
        // passed to a program (architecture.md section 10).
        if let Ok(real) = std::fs::canonicalize(program)
            && real != program
        {
            accepted.push(real);
        }
        Self {
            path: data_dir.join("run").join("caddy.pid"),
            accepted,
        }
    }

    /// Writes the file for a Caddy just spawned, only if `pid` runs our Caddy.
    pub(super) async fn record(&self, processes: &dyn Processes, pid: u32) {
        if !process::ORPHAN_RECOVERY {
            return;
        }
        match processes.identity(pid).await {
            Liveness::Alive { exe, started } if self.accepted.contains(&exe) => {
                let record = PidRecord { pid, exe, started };
                match process::write_pid_file(&self.path, &record) {
                    Ok(()) => debug!(pid, "caddy.pid written"),
                    Err(err) => warn!(%err, "cannot write caddy.pid"),
                }
            }
            // It exited at once; the supervisor reports why.
            Liveness::Gone => debug!(pid, "caddy exited before caddy.pid was written"),
            other => warn!(
                pid,
                ?other,
                "caddy.pid not written: the pid is not our Caddy"
            ),
        }
    }

    /// After Caddy exited: its pid may belong to anyone from now on.
    pub(super) fn remove(&self) {
        if let Err(err) = process::remove_pid_file(&self.path) {
            warn!(%err, "cannot delete caddy.pid");
        }
    }

    /// The file against what the OS says now.
    async fn check(&self, record: &PidRecord, processes: &dyn Processes) -> PidfileState {
        let now = processes.identity(record.pid).await;
        orphan::classify_pidfile(record, &self.accepted, &now)
    }
}

/// The check before each start. `Ok` once nothing holds our ports: nothing
/// was there, or our orphan was stopped. `Err` gives the reason Caddy must not
/// start. For a conflict, nobody was signalled and nothing was sent to the
/// admin port; after a failed reap, only our verified orphan was.
pub(super) async fn recover(
    pid_file: &PidFile,
    processes: &dyn Processes,
    admin: &Admin,
    input: &CaddyInput,
    ladder: Ladder,
) -> Result<(), String> {
    if !process::ORPHAN_RECOVERY {
        return Ok(());
    }
    let record = process::read_pid_file(&pid_file.path);
    let pidfile = match &record {
        Some(record) => pid_file.check(record, processes).await,
        None => PidfileState::Missing,
    };
    let inspection = Inspection {
        pidfile,
        admin: admin.tag().await,
        app_port: port_state(input.http_port).await,
        admin_port: port_state(input.admin_port).await,
        // Only the pid file names a pid to signal. `lsof` names owners for
        // messages only.
        admin_owner: None,
    };
    let action = orphan::decide(&inspection, Strategy::Reap);
    info!(
        pidfile = %inspection.pidfile,
        admin = %inspection.admin,
        site_port = %inspection.app_port,
        admin_port = %inspection.admin_port,
        %action,
        "checked for an orphan caddy"
    );
    match action {
        Action::StartFresh { remove_pidfile } => {
            if remove_pidfile {
                pid_file.remove();
            }
            Ok(())
        }
        // `decide` gives `Adopt` only for `Strategy::Adopt`; it would be
        // stopped the same way.
        Action::Reap { pid } | Action::Adopt { pid } => {
            // The pid comes from the pid file that matched, and nowhere else.
            let target = record.filter(|record| Some(record.pid) == pid);
            let reaper = Reaper {
                pid_file,
                processes,
                admin,
                ports: [input.http_port, input.admin_port],
                ladder,
            };
            reaper.reap(target.as_ref()).await?;
            pid_file.remove();
            Ok(())
        }
        Action::Conflict { reason } => Err(format!(
            "{reason} (site port {}, admin port {})",
            inspection.app_port, inspection.admin_port
        )),
    }
}

struct Reaper<'a> {
    pid_file: &'a PidFile,
    processes: &'a dyn Processes,
    admin: &'a Admin,
    ports: [u16; 2],
    ladder: Ladder,
}

impl Reaper<'_> {
    /// `POST /stop`; if the ports stay busy, SIGTERM, then SIGKILL, each only
    /// to `target` and only while it still is our Caddy.
    async fn reap(&self, target: Option<&PidRecord>) -> Result<(), String> {
        let started = Instant::now();
        self.admin.stop().await;
        let left = self.ladder.stop.saturating_sub(started.elapsed());
        if self.wait_stopped(target, left).await {
            info!("the orphan caddy stopped through its admin API");
            return Ok(());
        }
        let Some(record) = target else {
            return Err(
                "our old Caddy did not stop through its admin API, and no pid file \
                        names it, so it gets no signal"
                    .to_owned(),
            );
        };
        let pid = record.pid;
        for (signal, wait) in [
            (Signal::Term, self.ladder.term),
            (Signal::Kill, self.ladder.kill),
        ] {
            // Again right before the signal: the pid may be someone else's now.
            let now = self.pid_file.check(record, self.processes).await;
            if !matches!(now, PidfileState::Ours { .. }) {
                warn!(pid, state = %now, ?signal, "no signal: the pid is not our caddy any more");
                return if ports_free(&self.ports) {
                    Ok(())
                } else {
                    Err(format!(
                        "pid {pid} is not our Caddy any more, and the ports are still busy"
                    ))
                };
            }
            warn!(pid, ?signal, "the orphan caddy did not stop; signalling it");
            if let Err(err) = self.processes.signal(pid, signal).await {
                warn!(pid, ?signal, %err, "the signal failed");
            }
            if self.wait_stopped(target, wait).await {
                info!(pid, ?signal, "the orphan caddy stopped");
                return Ok(());
            }
        }
        Err(format!(
            "our old Caddy (pid {pid}) still holds the ports after SIGKILL"
        ))
    }

    /// Polls until both ports are free and `target` is not our Caddy any more.
    async fn wait_stopped(&self, target: Option<&PidRecord>, limit: Duration) -> bool {
        let started = Instant::now();
        loop {
            if ports_free(&self.ports) {
                let Some(record) = target else {
                    return true;
                };
                let now = self.pid_file.check(record, self.processes).await;
                if !matches!(now, PidfileState::Ours { .. }) {
                    return true;
                }
            }
            if started.elapsed() >= limit {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
    }
}

/// Free if it can be bound on `127.0.0.1` now. Else `lsof` names the owner.
async fn port_state(port: u16) -> PortState {
    if ports_free(&[port]) {
        return PortState::Free;
    }
    let owner = process::port_owner(port).await;
    PortState::Busy {
        owner: owner.unwrap_or_else(|| "an unknown program".to_owned()),
    }
}

fn ports_free(ports: &[u16]) -> bool {
    // Each listener closes at once.
    ports
        .iter()
        .all(|port| TcpListener::bind((Ipv4Addr::LOCALHOST, *port)).is_ok())
}

impl Admin {
    /// `GET /id/marsa-srv`: whether the program on our admin port is our
    /// Caddy. Read for at most the client's timeout.
    async fn tag(&self) -> AdminState {
        let Some(http) = &self.http else {
            return AdminState::NoReply;
        };
        let url = format!("{}/id/{SERVER_ID}", self.base);
        let mut response = match http.get(url).send().await {
            Ok(response) => response,
            Err(err) if err.is_timeout() => return AdminState::Timeout,
            Err(_) => return AdminState::NoReply,
        };
        let status = response.status().as_u16();
        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) if body.len() + chunk.len() <= TAG_BODY_MAX => {
                    body.extend_from_slice(&chunk);
                }
                Ok(Some(_)) => return AdminState::Foreign,
                Ok(None) => return orphan::classify_admin_reply(status, &body),
                Err(err) if err.is_timeout() => return AdminState::Timeout,
                Err(_) => return AdminState::Foreign,
            }
        }
    }
}

// Only macOS and Linux run the checks; on Windows `recover` returns at once.
#[cfg(test)]
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    const PROGRAM: &str = "/data/bin/caddy";
    const STARTED: &str = "Wed Nov 4 03:31:27 2026";

    /// Gives the identities in order, then `Unknown`. Records signals and sends
    /// none; a SIGKILL closes `held`, as the death of the holder would.
    #[derive(Debug, Default)]
    struct Fake {
        identities: Mutex<VecDeque<Liveness>>,
        signals: Mutex<Vec<(u32, Signal)>>,
        held: Mutex<Option<TcpListener>>,
    }

    impl Processes for Fake {
        fn identity(&self, _pid: u32) -> BoxFuture<'_, Liveness> {
            let next = self.identities.lock().unwrap().pop_front();
            Box::pin(async move { next.unwrap_or(Liveness::Unknown) })
        }

        fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>> {
            self.signals.lock().unwrap().push((pid, signal));
            if signal == Signal::Kill {
                self.held.lock().unwrap().take();
            }
            Box::pin(async { Ok(()) })
        }
    }

    fn ours() -> Liveness {
        Liveness::Alive {
            exe: PathBuf::from(PROGRAM),
            started: STARTED.into(),
        }
    }

    fn free_port() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    /// A data folder whose pid file names pid 4242, our Caddy; a site port that
    /// a listener holds; and an admin port where nothing listens.
    struct Setup {
        _dir: tempfile::TempDir,
        pid_file: PidFile,
        input: CaddyInput,
        admin: Admin,
        held: TcpListener,
    }

    fn setup() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = PidFile::new(dir.path(), Path::new(PROGRAM));
        let record = PidRecord {
            pid: 4242,
            exe: PathBuf::from(PROGRAM),
            started: STARTED.into(),
        };
        process::write_pid_file(&pid_file.path, &record).unwrap();
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let input = CaddyInput {
            http_port: held.local_addr().unwrap().port(),
            admin_port: free_port(),
            https_port: free_port(),
        };
        Setup {
            admin: Admin::new(input.admin_port),
            _dir: dir,
            pid_file,
            input,
            held,
        }
    }

    const SHORT: Ladder = Ladder {
        stop: Duration::from_millis(100),
        term: Duration::from_millis(100),
        kill: Duration::from_millis(500),
    };

    #[tokio::test]
    async fn a_verified_orphan_gets_sigterm_then_sigkill_and_the_file_goes() {
        let Setup {
            _dir,
            pid_file,
            input,
            admin,
            held,
        } = setup();
        let fake = Fake::default();
        // Inspection, before SIGTERM, before SIGKILL, then gone.
        fake.identities
            .lock()
            .unwrap()
            .extend([ours(), ours(), ours(), Liveness::Gone]);
        *fake.held.lock().unwrap() = Some(held);

        let result = recover(&pid_file, &fake, &admin, &input, SHORT).await;
        assert_eq!(result, Ok(()));
        let signals = fake.signals.lock().unwrap().clone();
        assert_eq!(signals, [(4242, Signal::Term), (4242, Signal::Kill)]);
        assert_eq!(process::read_pid_file(&pid_file.path), None);
    }

    #[tokio::test]
    async fn the_pid_is_checked_again_before_each_signal() {
        let Setup {
            _dir,
            pid_file,
            input,
            admin,
            held: _held,
        } = setup();
        let fake = Fake::default();
        let other = Liveness::Alive {
            exe: PathBuf::from("/bin/sleep"),
            started: STARTED.into(),
        };
        // Ours at the inspection and before SIGTERM; another program by the
        // time SIGKILL would go.
        fake.identities
            .lock()
            .unwrap()
            .extend([ours(), ours(), other]);

        let result = recover(&pid_file, &fake, &admin, &input, SHORT).await;
        assert!(result.is_err(), "{result:?}");
        let signals = fake.signals.lock().unwrap().clone();
        assert_eq!(
            signals,
            [(4242, Signal::Term)],
            "no SIGKILL to the new owner"
        );
    }

    #[tokio::test]
    async fn a_held_site_port_with_a_reused_pid_is_a_conflict_without_signals() {
        // R5d: the file names a live process that started at another time.
        let Setup {
            _dir,
            pid_file,
            input,
            admin,
            held: _held,
        } = setup();
        let fake = Fake::default();
        let later = Liveness::Alive {
            exe: PathBuf::from(PROGRAM),
            started: "Wed Nov 4 03:31:28 2026".into(),
        };
        fake.identities.lock().unwrap().push_back(later);

        let reason = recover(&pid_file, &fake, &admin, &input, SHORT)
            .await
            .unwrap_err();
        assert!(reason.contains("site port is held by"), "{reason}");
        assert!(fake.signals.lock().unwrap().is_empty());
        // Touch nothing: the file stays.
        assert!(process::read_pid_file(&pid_file.path).is_some());
    }
}
