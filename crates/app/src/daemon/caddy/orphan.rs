//! Finds and stops a Caddy that a crashed `marsad` left running (ADR 0009).
//! Task M3-11. macOS only for now: `ORPHAN_RECOVERY` is off elsewhere.
//!
//! The manager inspects three things: `run/caddy.pid`; the admin port's answer
//! to `GET /id/marsa-instance-<id>` (this data folder's tag), then to
//! `GET /id/marsa-srv` (any Marsa); and the ports. `marsa_core::orphan::decide`
//! picks the action. A reap is `POST /stop`, then SIGTERM, then SIGKILL.
//! Before each signal the pid is checked again: start path and start time
//! from `ps`, and the real executable from `lsof`. Anything that is not ours
//! is never signalled, and another Marsa's Caddy does not even get `/stop`.

use std::error::Error as _;
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

/// Another program holds a port Caddy needs.
pub const PORT_IN_USE: &str = "port_in_use";
/// Our old Caddy holds the ports and could not be stopped.
pub const ORPHAN_STUCK: &str = "caddy_orphan_stuck";
/// Another Marsa's Caddy (another data folder or user) holds our admin port.
pub const OTHER_MARSA: &str = "caddy_other_marsa";

/// How often a wait checks the ports again.
const POLL: Duration = Duration::from_millis(50);

/// The largest answer to `GET /id/...` that is read. The server's config grows
/// with the sites; a longer answer counts as not ours.
const TAG_BODY_MAX: usize = 16 * 1024 * 1024;

/// A boxed future, so [`Processes`] works as `dyn Processes`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Who runs under a pid, and signals. [`SystemProcesses`] asks the OS; a test
/// records the signals instead.
pub trait Processes: fmt::Debug + Send + Sync {
    /// The start path and start time.
    fn identity(&self, pid: u32) -> BoxFuture<'_, Liveness>;
    /// The file the process really runs.
    fn executable(&self, pid: u32) -> BoxFuture<'_, Option<PathBuf>>;
    /// Sends `signal` to `pid`. The caller checked the pid right before.
    fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>>;
}

/// The real OS: `marsa_core::platform::process`.
#[derive(Debug)]
pub struct SystemProcesses;

impl Processes for SystemProcesses {
    fn identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
        Box::pin(process::identity(pid))
    }

    fn executable(&self, pid: u32) -> BoxFuture<'_, Option<PathBuf>> {
        Box::pin(process::executable(pid))
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

/// Why Caddy must not start, or why our orphan is still there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Blocked {
    /// [`PORT_IN_USE`], [`ORPHAN_STUCK`], or [`OTHER_MARSA`].
    pub code: &'static str,
    pub reason: String,
}

impl Blocked {
    fn new(code: &'static str, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
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
        // `lsof` reports the path with links resolved. Only compared, never
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

    /// Writes the file for a Caddy just spawned, only if `pid` was started as
    /// our Caddy.
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

    /// The file against what `ps` says now.
    async fn check(&self, record: &PidRecord, processes: &dyn Processes) -> PidfileState {
        let now = processes.identity(record.pid).await;
        orphan::classify_pidfile(record, &self.accepted, &now)
    }
}

/// What the check right before a signal says.
enum Verdict {
    Ours,
    /// Gone, or someone else's now.
    NotOurs,
    /// Cannot be told: no signal, and maybe ours.
    Unsure,
}

/// What [`Orphans::inspect`] found.
#[derive(Debug)]
pub(super) struct Found {
    action: Action,
    inspection: Inspection,
    record: Option<PidRecord>,
}

impl Found {
    /// A verified orphan of this data folder, to stop.
    pub(super) fn is_reap(&self) -> bool {
        matches!(self.action, Action::Reap { .. } | Action::Adopt { .. })
    }
}

/// The check for one start or stop.
pub(super) struct Orphans<'a> {
    pub pid_file: &'a PidFile,
    pub processes: &'a dyn Processes,
    pub admin: &'a Admin,
    pub input: &'a CaddyInput,
    pub ladder: Ladder,
    /// This data folder's tag: `marsa-instance-<id>`.
    pub tag: &'a str,
}

impl Orphans<'_> {
    /// Inspects the pid file, the admin port, and the ports, and decides.
    /// `None` where the OS has no orphan recovery.
    pub(super) async fn inspect(&self) -> Option<Found> {
        if !process::ORPHAN_RECOVERY {
            return None;
        }
        let record = process::read_pid_file(&self.pid_file.path);
        let pidfile = match &record {
            Some(record) => self.pid_file.check(record, self.processes).await,
            None => PidfileState::Missing,
        };
        let inspection = Inspection {
            pidfile,
            admin: self.admin.tag(self.tag).await,
            app_port: port_state(self.input.http_port).await,
            admin_port: port_state(self.input.admin_port).await,
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
        Some(Found {
            action,
            inspection,
            record,
        })
    }

    /// Before a start: `Ok` once nothing holds our ports, else why Caddy must
    /// not start. For a conflict nobody was signalled and nothing was sent
    /// to the admin port; a reap signals only our verified orphan.
    pub(super) async fn clear_for_start(&self, found: Found) -> Result<(), Blocked> {
        match &found.action {
            Action::StartFresh { remove_pidfile } => {
                if *remove_pidfile {
                    self.pid_file.remove();
                }
                Ok(())
            }
            Action::Reap { .. } | Action::Adopt { .. } => self.reap(found).await,
            Action::Conflict { reason } => {
                // A frozen process on our admin port that no pid file names
                // may be our orphan, but it cannot be signalled.
                let code = if found.inspection.admin == AdminState::Timeout {
                    ORPHAN_STUCK
                } else {
                    PORT_IN_USE
                };
                Err(Blocked::new(code, with_owners(reason, &found.inspection)))
            }
            Action::OtherMarsa => {
                let reason = "the Caddy of another Marsa (another data folder or user) \
                              answers on our admin port";
                Err(Blocked::new(
                    OTHER_MARSA,
                    with_owners(reason, &found.inspection),
                ))
            }
        }
    }

    /// Stops the orphan [`Found::is_reap`] found. Does nothing otherwise.
    pub(super) async fn reap(&self, found: Found) -> Result<(), Blocked> {
        let (Action::Reap { pid } | Action::Adopt { pid }) = found.action else {
            return Ok(());
        };
        // The pid comes from the pid file that matched, and nowhere else.
        let target = found.record.filter(|record| Some(record.pid) == pid);
        self.ladder(target.as_ref()).await?;
        self.pid_file.remove();
        Ok(())
    }

    /// `POST /stop`; if the ports stay busy, SIGTERM, then SIGKILL, each only
    /// to `target` and only while it still is our Caddy.
    async fn ladder(&self, target: Option<&PidRecord>) -> Result<(), Blocked> {
        let started = Instant::now();
        self.admin.stop().await;
        let left = self.ladder.stop.saturating_sub(started.elapsed());
        if self.wait_stopped(target, left).await {
            info!("the orphan caddy stopped through its admin API");
            return Ok(());
        }
        let Some(record) = target else {
            return Err(Blocked::new(
                ORPHAN_STUCK,
                "our old Caddy did not stop through its admin API, and no pid file names it, \
                 so it gets no signal",
            ));
        };
        let pid = record.pid;
        for (signal, wait) in [
            (Signal::Term, self.ladder.term),
            (Signal::Kill, self.ladder.kill),
        ] {
            match self.verdict(record).await {
                Verdict::Ours => {}
                verdict => {
                    warn!(
                        pid,
                        ?signal,
                        "no signal: the pid is not verified as our caddy"
                    );
                    if self.ports_free() {
                        return Ok(());
                    }
                    let (code, why) = match verdict {
                        Verdict::NotOurs => (PORT_IN_USE, "is not our Caddy any more"),
                        _ => (ORPHAN_STUCK, "cannot be verified as our Caddy"),
                    };
                    let reason = format!("pid {pid} {why}, and the ports are still busy");
                    return Err(Blocked::new(code, reason));
                }
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
        let reason = format!("our old Caddy (pid {pid}) still holds the ports after SIGKILL");
        Err(Blocked::new(ORPHAN_STUCK, reason))
    }

    /// Right before a signal: the start path and start time from `ps`, then
    /// the file the process really runs, from `lsof`. Both must be ours.
    async fn verdict(&self, record: &PidRecord) -> Verdict {
        match self.pid_file.check(record, self.processes).await {
            PidfileState::Ours { .. } => {}
            PidfileState::Unknown { .. } => return Verdict::Unsure,
            _ => return Verdict::NotOurs,
        }
        match self.processes.executable(record.pid).await {
            Some(exe) if self.pid_file.accepted.contains(&exe) => Verdict::Ours,
            Some(exe) => {
                warn!(pid = record.pid, exe = %exe.display(), "the pid runs another program");
                Verdict::NotOurs
            }
            None => Verdict::Unsure,
        }
    }

    /// Polls until the three ports are free and `target` is not our Caddy any
    /// more. Free ports alone are not enough: a Caddy that has closed its
    /// listeners may still be shutting down, and the next step must not
    /// signal a process that is about to exit by itself, nor start a second
    /// Caddy beside it.
    async fn wait_stopped(&self, target: Option<&PidRecord>, limit: Duration) -> bool {
        let started = Instant::now();
        loop {
            if self.ports_free() {
                let Some(record) = target else {
                    return true;
                };
                match self.pid_file.check(record, self.processes).await {
                    // `Unknown` may be a `ps` that failed once: wait on.
                    PidfileState::Ours { .. } | PidfileState::Unknown { .. } => {}
                    _ => return true,
                }
            }
            if started.elapsed() >= limit {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    fn ports_free(&self) -> bool {
        let input = self.input;
        ports_free(&[input.http_port, input.https_port, input.admin_port])
    }
}

/// `reason`, plus the owner of each busy port it does not name yet.
fn with_owners(reason: &str, inspection: &Inspection) -> String {
    let mut text = reason.to_owned();
    for (name, state) in [
        ("site", &inspection.app_port),
        ("admin", &inspection.admin_port),
    ] {
        if let PortState::Busy { owner } = state
            && !text.contains(&format!("the {name} port"))
        {
            text.push_str(&format!("; the {name} port is held by {owner}"));
        }
    }
    text
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

/// One `GET /id/<tag>`.
enum Probe {
    Tagged,
    /// An HTTP answer without the tag.
    Untagged,
    /// Connection refused: nothing listens.
    Refused,
    Timeout,
    /// Anything else: a reset, or an answer that is not HTTP.
    Broken,
}

impl Admin {
    /// Whether the program on our admin port is this data folder's Caddy
    /// (`ours`), another Marsa's (`marsa-srv` only), or something else.
    async fn tag(&self, ours: &str) -> AdminState {
        match self.probe(ours).await {
            Probe::Tagged => orphan::classify_admin_replies(true, false),
            Probe::Refused => AdminState::NoReply,
            Probe::Timeout => AdminState::Timeout,
            Probe::Broken => AdminState::Foreign,
            // HTTP, but not ours: maybe another Marsa.
            Probe::Untagged => {
                let shared = matches!(self.probe(SERVER_ID).await, Probe::Tagged);
                orphan::classify_admin_replies(false, shared)
            }
        }
    }

    /// `GET /id/<tag>`, read for at most the client's timeout.
    async fn probe(&self, tag: &str) -> Probe {
        let Some(http) = &self.http else {
            return Probe::Broken;
        };
        let url = format!("{}/id/{tag}", self.base);
        let mut response = match http.get(url).send().await {
            Ok(response) => response,
            Err(err) => return failed(&err),
        };
        let status = response.status().as_u16();
        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) if body.len() + chunk.len() <= TAG_BODY_MAX => {
                    body.extend_from_slice(&chunk);
                }
                Ok(Some(_)) => return Probe::Broken,
                Ok(None) if orphan::reply_has_tag(status, &body, tag) => return Probe::Tagged,
                Ok(None) => return Probe::Untagged,
                Err(err) => return failed(&err),
            }
        }
    }
}

/// Only a refused connection means that nothing listens.
fn failed(err: &reqwest::Error) -> Probe {
    if err.is_timeout() {
        return Probe::Timeout;
    }
    let mut source = err.source();
    while let Some(cause) = source {
        if cause
            .downcast_ref::<io::Error>()
            .is_some_and(|io| io.kind() == io::ErrorKind::ConnectionRefused)
        {
            return Probe::Refused;
        }
        source = cause.source();
    }
    Probe::Broken
}

// Only macOS runs the checks; elsewhere `inspect` returns `None` at once.
#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::{Read as _, Write as _};
    use std::sync::Mutex;

    const PROGRAM: &str = "/data/bin/caddy";
    const STARTED: &str = "Wed Nov 4 03:31:27 2026";
    const TAG: &str = "marsa-instance-0123456789abcdef0123456789abcdef";

    /// Gives the identities in order, then `Unknown`, and the executable
    /// `exe`. Records signals and sends none; a SIGKILL closes `held`, as the
    /// death of the holder would.
    #[derive(Debug)]
    struct Fake {
        identities: Mutex<VecDeque<Liveness>>,
        exe: Option<PathBuf>,
        signals: Mutex<Vec<(u32, Signal)>>,
        held: Mutex<Option<TcpListener>>,
    }

    impl Fake {
        fn new(identities: impl IntoIterator<Item = Liveness>) -> Self {
            Self {
                identities: Mutex::new(identities.into_iter().collect()),
                exe: Some(PathBuf::from(PROGRAM)),
                signals: Mutex::default(),
                held: Mutex::default(),
            }
        }

        fn signals(&self) -> Vec<(u32, Signal)> {
            self.signals.lock().unwrap().clone()
        }
    }

    impl Processes for Fake {
        fn identity(&self, _pid: u32) -> BoxFuture<'_, Liveness> {
            let next = self.identities.lock().unwrap().pop_front();
            Box::pin(async move { next.unwrap_or(Liveness::Unknown) })
        }

        fn executable(&self, _pid: u32) -> BoxFuture<'_, Option<PathBuf>> {
            Box::pin(async move { self.exe.clone() })
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

    /// A data folder whose pid file names pid 4242, our Caddy, and an admin
    /// port where nothing listens. The listener that comes with it holds the
    /// site port.
    struct Setup {
        _dir: tempfile::TempDir,
        pid_file: PidFile,
        input: CaddyInput,
        admin: Admin,
    }

    fn setup() -> (Setup, TcpListener) {
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
        let setup = Setup {
            admin: Admin::new(input.admin_port),
            _dir: dir,
            pid_file,
            input,
        };
        (setup, held)
    }

    const SHORT: Ladder = Ladder {
        stop: Duration::from_millis(100),
        term: Duration::from_millis(100),
        kill: Duration::from_millis(500),
    };

    async fn run(setup: &Setup, fake: &Fake) -> Result<(), Blocked> {
        let orphans = Orphans {
            pid_file: &setup.pid_file,
            processes: fake,
            admin: &setup.admin,
            input: &setup.input,
            ladder: SHORT,
            tag: TAG,
        };
        let found = orphans.inspect().await.expect("macOS inspects");
        orphans.clear_for_start(found).await
    }

    #[tokio::test]
    async fn a_verified_orphan_gets_sigterm_then_sigkill_and_the_file_goes() {
        let (setup, held) = setup();
        // Inspection, before SIGTERM, before SIGKILL, then gone.
        let fake = Fake::new([ours(), ours(), ours(), Liveness::Gone]);
        *fake.held.lock().unwrap() = Some(held);

        let result = run(&setup, &fake).await;
        assert_eq!(result, Ok(()));
        let want = [(4242, Signal::Term), (4242, Signal::Kill)];
        assert_eq!(fake.signals(), want);
        assert_eq!(process::read_pid_file(&setup.pid_file.path), None);
    }

    #[tokio::test]
    async fn the_pid_is_checked_again_before_each_signal() {
        let (setup, _held) = setup();
        let other = Liveness::Alive {
            exe: PathBuf::from("/bin/sleep"),
            started: STARTED.into(),
        };
        // Ours at the inspection and before SIGTERM; another program by the
        // time SIGKILL would go.
        let fake = Fake::new([ours(), ours(), other]);

        let result = run(&setup, &fake).await;
        assert_eq!(result.unwrap_err().code, PORT_IN_USE);
        assert_eq!(fake.signals(), [(4242, Signal::Term)], "no SIGKILL");
    }

    #[tokio::test]
    async fn a_pid_whose_real_executable_is_not_our_caddy_gets_no_signal() {
        // `ps` shows our path (argv[0] can say anything), `lsof` does not.
        let (setup, _held) = setup();
        let fake = Fake {
            exe: Some(PathBuf::from("/usr/bin/python3")),
            ..Fake::new([ours(), ours()])
        };
        let result = run(&setup, &fake).await;
        assert!(result.is_err(), "{result:?}");
        assert!(fake.signals().is_empty());

        // An executable that cannot be read is no proof either.
        let fake = Fake {
            exe: None,
            ..Fake::new([ours(), ours()])
        };
        let result = run(&setup, &fake).await;
        assert_eq!(result.unwrap_err().code, ORPHAN_STUCK);
        assert!(fake.signals().is_empty());
    }

    #[tokio::test]
    async fn a_held_site_port_with_a_reused_pid_is_a_conflict_without_signals() {
        // R5d: the file names a live process that started at another time.
        let (setup, _held) = setup();
        let later = Liveness::Alive {
            exe: PathBuf::from(PROGRAM),
            started: "Wed Nov 4 03:31:28 2026".into(),
        };
        let fake = Fake::new([later]);

        let blocked = run(&setup, &fake).await.unwrap_err();
        assert_eq!(blocked.code, PORT_IN_USE);
        assert!(
            blocked.reason.contains("site port is held by"),
            "{blocked:?}"
        );
        // The owner is named once.
        assert_eq!(
            blocked.reason.matches("site port").count(),
            1,
            "{blocked:?}"
        );
        assert!(fake.signals().is_empty());
        // Touch nothing: the file stays.
        assert!(process::read_pid_file(&setup.pid_file.path).is_some());
    }

    #[tokio::test]
    async fn a_failed_ps_keeps_the_pid_file() {
        // Nothing holds a port, so only the pid file is in question.
        let (setup, held) = setup();
        drop(held);
        let fake = Fake::new([Liveness::Unknown]);
        assert_eq!(run(&setup, &fake).await, Ok(()));
        assert!(process::read_pid_file(&setup.pid_file.path).is_some());
        assert!(fake.signals().is_empty());
    }

    /// Accepts one connection on a new port and hands it to `serve`.
    fn one_connection(serve: impl FnOnce(std::net::TcpStream) + Send + 'static) -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                serve(stream);
            }
        });
        port
    }

    #[tokio::test]
    async fn only_a_refused_connection_is_no_reply() {
        assert_eq!(Admin::new(free_port()).tag(TAG).await, AdminState::NoReply);

        // Not HTTP: an answer of garbage, and a connection closed at once.
        let garbage = one_connection(|mut stream| {
            let mut buf = [0; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"SSH-2.0-OpenSSH\r\n\r\n");
        });
        assert_eq!(Admin::new(garbage).tag(TAG).await, AdminState::Foreign);
        let closed = one_connection(drop);
        assert_eq!(Admin::new(closed).tag(TAG).await, AdminState::Foreign);

        // Accepted, and never answered: a frozen process.
        let silent = one_connection(|stream| {
            std::thread::sleep(Duration::from_secs(3));
            drop(stream);
        });
        assert_eq!(Admin::new(silent).tag(TAG).await, AdminState::Timeout);
    }
}
