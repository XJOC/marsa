//! PostgreSQL as a service of `marsad` (ADR 0023, task M6-03), on macOS and
//! Linux. `marsa_core::databases` decides; this module runs the programs.
//!
//! The first start of a major line makes its cluster in
//! `databases/postgres/<major>/` ([`ensure_cluster`]): `initdb`, then Marsa's
//! `pg_hba.conf`, then the role `root` in single-user mode. All of it happens
//! in a temp folder next to the target, which is renamed into place at the
//! end, so a cluster that is half made is never taken for a ready one.
//!
//! Before each start, [`start_facts`] reads the port and `postmaster.pid`
//! from the system for `decide_start`. It only reads: nothing here signals a
//! process or deletes `postmaster.pid`.
//!
//! [`PostgresManager`] starts and stops the server under the supervisor, one
//! action at a time. A failed start leaves the status `failed`, with the
//! reason in the recent lines. A run that ends by itself before it takes
//! connections is read once: if PostgreSQL refused because a live process
//! holds one of its lock files, the start fails as the check before it
//! would have. Nothing reads the log after the server is ready.
//!
//! [`start_at_launch`] starts it with `marsad` when `[postgres] enabled =
//! true` (ADR 0023: a server that is on starts with the daemon). A failure
//! leaves the daemon running and the status `failed`.

use std::fmt;
use std::io;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use marsa_core::config::{self, ConfigError, ConfigPaths, ServerSettings};
use marsa_core::databases::{
    LockConflict, PG_HBA_CONF, POSTGRES_PORT, PidFileCheck, PostgresDirs, ROOT_ROLE_SQL,
    ServiceBusy, StartDecision, StartFacts, classify_postmaster, decide_start, initdb_args,
    line_version, port_check, postgres_args, postgres_stop, postmaster_pid, single_user_args,
    socket_for, startup_lock_conflict,
};
use marsa_core::orphan::Liveness;
use marsa_core::platform::{probe, process};
use marsa_core::ports::PortLookup;
use marsa_core::recipes::{self, Platform};
use marsa_core::runtimes::{Runtime, RuntimesDir};
use marsa_core::services::{ProgramNotAbsolute, RestartPolicy, ServiceSpec};
use marsa_core::types::{Event, LogLine, LogStream, ServiceAction, ServiceState, ServiceStatus};
use semver::Version;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::caddy::orphan::BoxFuture;
use super::caddy::{Launch, Launched, note, set_failed};
use super::php::orphan::accepted;
use super::state::Daemon;
use super::supervisor::{EXITED, ServiceRuntime, Supervisor, lock};

/// The service id: the key in `Daemon::procs`, and the log file
/// `logs/postgres.log`.
pub const POSTGRES_ID: &str = "postgres";

/// How long a start waits for the server to take connections. S-12
/// measured 50 to 690 ms. The postmaster answers before it recovers its data
/// after a crash, so a long recovery does not count against this.
const READY_LIMIT: Duration = Duration::from_secs(15);
const READY_POLL: Duration = Duration::from_millis(100);
/// For each attempt of the readiness check.
const CONNECT_LIMIT: Duration = Duration::from_millis(500);
/// An `SSLRequest`: its length, 8, then the code 80877103. The first message
/// a client may send, before any user name.
const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f];

/// How long `initdb` may run. About a second on a laptop (S-12); a slow disk
/// or a busy CI machine needs more.
const INITDB_LIMIT: Duration = Duration::from_secs(120);
/// How long the single-user `postgres` that makes `root` may run.
const SINGLE_USER_LIMIT: Duration = Duration::from_secs(60);
/// The last lines of a failed step's output kept in its error.
const ERROR_LINES: usize = 10;

/// The programs of one installed PostgreSQL version. Absolute: `marsad` never
/// searches `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Programs {
    pub postgres: PathBuf,
    pub initdb: PathBuf,
}

/// Where the recipe puts the programs of the installed PostgreSQL `version`:
/// `runtimes/postgres/<version>/bin/`. `None` if the recipe has none for
/// `platform`.
pub fn programs(data_dir: &Path, platform: Platform, version: &Version) -> Option<Programs> {
    // A test parses every built-in recipe, so an error here never ships.
    let recipe = recipes::builtin(Runtime::Postgres)
        .ok()?
        .platform(platform)?;
    let dir = RuntimesDir::in_dir(data_dir).version_dir(Runtime::Postgres, version);
    Some(Programs {
        postgres: recipe.program_path(&dir, "postgres")?,
        initdb: recipe.program_path(&dir, "initdb")?,
    })
}

/// `bin/postgres` of every installed PostgreSQL version, and the resolved
/// form of each: the programs that a `postmaster.pid` of this data folder
/// may name. An older version may still run on the folder. A failure to list
/// the versions gives fewer paths, so fewer pids count as ours.
pub fn installed_servers(data_dir: &Path, platform: Platform) -> Vec<PathBuf> {
    let versions = RuntimesDir::in_dir(data_dir)
        .installed(Runtime::Postgres)
        .unwrap_or_else(|err| {
            warn!(code = err.code(), %err, "cannot list the PostgreSQL versions");
            Vec::new()
        });
    let servers = versions
        .iter()
        .filter_map(|version| programs(data_dir, platform, version))
        .map(|programs| programs.postgres);
    accepted(servers)
}

/// Who runs under a pid, and who listens on a port: the OS in the daemon
/// ([`SystemProbes`]), a fake in tests. Only asks; never signals.
pub trait Probes: fmt::Debug + Send + Sync {
    /// By the file the process really runs:
    /// [`marsa_core::platform::process::real_identity`].
    fn real_identity(&self, pid: u32) -> BoxFuture<'_, Liveness>;
    /// Who listens on the port, on any address:
    /// [`marsa_core::platform::process::port_lookup`]. An owner, or a look
    /// that failed, means busy.
    fn port_lookup(&self, port: u16) -> BoxFuture<'_, PortLookup>;
}

/// The real OS.
#[derive(Debug)]
pub struct SystemProbes;

impl Probes for SystemProbes {
    fn real_identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
        Box::pin(process::real_identity(pid))
    }

    fn port_lookup(&self, port: u16) -> BoxFuture<'_, PortLookup> {
        Box::pin(process::port_lookup(port))
    }
}

/// The facts `decide_start` needs, read from the system now:
///
/// - the port: a bind to `127.0.0.1:<port>`, and who listens on it on any
///   address; a look that fails refuses too ([`port_check`]);
/// - `postmaster.pid` in the cluster `data`: read with a size cap, never
///   through a link ([`process::read_small_file`]), and the program at its
///   pid, by its real executable ([`classify_postmaster`] against
///   `servers`, from [`installed_servers`]).
///
/// The caller first makes sure that our own supervisor does not run
/// PostgreSQL: that one holds the port and the file, and is not busy.
pub async fn start_facts(
    probes: &dyn Probes,
    port: u16,
    data: &Path,
    servers: &[PathBuf],
) -> StartFacts {
    // The listener closes at once.
    let bound = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok();
    let lookup = probes.port_lookup(port).await;
    let text = process::read_small_file(&data.join("postmaster.pid"));
    let pid_file = match text.as_deref().and_then(postmaster_pid) {
        None => PidFileCheck::NoPid,
        Some(pid) => classify_postmaster(pid, &probes.real_identity(pid).await, servers),
    };
    StartFacts {
        port,
        port_check: port_check(bound, lookup),
        pid_file,
    }
}

/// Why the first init of a cluster failed. Nothing is left behind: no
/// cluster, and no temp folder. Except when `in_place`: then the cluster
/// was moved into place, and only the sync of its folder failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitError {
    pub reason: String,
    /// The new cluster is in place, and the next start uses it. Only the
    /// sync of the folder that holds it failed, so a crash before the OS
    /// writes that folder could still undo the rename.
    pub in_place: bool,
}

impl InitError {
    /// Stable error name for the UI.
    pub fn code(&self) -> &'static str {
        "postgres_init_failed"
    }

    fn new(reason: String) -> Self {
        Self {
            reason,
            in_place: false,
        }
    }

    fn io(what: &str, err: &io::Error) -> Self {
        Self::new(format!("{what}: {err}"))
    }
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = &self.reason;
        if self.in_place {
            write!(
                f,
                "the PostgreSQL data folder was made and is in place, but {reason}; \
                 the next start uses it"
            )
        } else {
            write!(f, "the PostgreSQL data folder was not made: {reason}")
        }
    }
}

impl std::error::Error for InitError {}

/// Makes the cluster in `dirs.data` if nothing is there yet, and returns
/// whether it made one. Anything at that path, even an empty folder or a
/// link, counts as there: it is never read, run, or changed here, and
/// PostgreSQL says at its start whether it is a cluster.
///
/// The steps, all in a new temp folder `.<major>.init-<random>` next to the
/// target:
///
/// 1. `initdb` with [`initdb_args`];
/// 2. [`PG_HBA_CONF`] over initdb's `pg_hba.conf`;
/// 3. `postgres --single` with [`ROOT_ROLE_SQL`] on stdin;
/// 4. a rename to `dirs.data`, on the same volume, so the cluster appears
///    whole or not at all, then a sync of the folder that holds it.
///
/// Temp folders that an earlier init left (a crash, a power cut) are deleted
/// first: none of them was ever a cluster in use.
///
/// # Errors
///
/// `postgres_init_failed`, with the step and the end of its output. The temp
/// folder is deleted then.
pub async fn ensure_cluster(programs: &Programs, dirs: &PostgresDirs) -> Result<bool, InitError> {
    match tokio::fs::symlink_metadata(&dirs.data).await {
        Ok(_) => return Ok(false),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(InitError::io("cannot look at the data folder", &err)),
    }
    let (Some(parent), Some(name)) = (dirs.data.parent(), dirs.data.file_name()) else {
        let reason = format!("{} has no parent folder", dirs.data.display());
        return Err(InitError::new(reason));
    };
    let prefix = format!(".{}.init-", name.to_string_lossy());
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|err| InitError::io("cannot create the databases folder", &err))?;
    remove_leftovers(parent, &prefix).await;
    let temp = parent.join(format!("{prefix}{:016x}", probe::random()));
    info!(data = %dirs.data.display(), "making the PostgreSQL data folder");
    let made = init_in(programs, dirs, &temp).await;
    if made.is_err()
        && let Err(err) = remove_dir(&temp).await
    {
        warn!(temp = %temp.display(), %err, "cannot delete the temp folder of a failed init");
    }
    made.map(|()| true)
}

/// Steps 1 to 4 of [`ensure_cluster`] in `temp`.
async fn init_in(programs: &Programs, dirs: &PostgresDirs, temp: &Path) -> Result<(), InitError> {
    let in_temp = PostgresDirs {
        data: temp.to_path_buf(),
        sockets: dirs.sockets.clone(),
    };
    let mut initdb = Command::new(&programs.initdb);
    initdb.args(initdb_args(&in_temp));
    run_step(initdb, None, INITDB_LIMIT, "initdb").await?;

    write_hba(&temp.join("pg_hba.conf"))
        .await
        .map_err(|err| InitError::io("cannot write pg_hba.conf", &err))?;

    let mut single = Command::new(&programs.postgres);
    single.args(single_user_args(temp));
    let sql = Some(ROOT_ROLE_SQL);
    run_step(single, sql, SINGLE_USER_LIMIT, "postgres --single").await?;

    // Checked again: a rename would replace an empty folder made meanwhile.
    if tokio::fs::symlink_metadata(&dirs.data).await.is_ok() {
        let reason = format!("{} appeared during the init", dirs.data.display());
        return Err(InitError::new(reason));
    }
    tokio::fs::rename(temp, &dirs.data)
        .await
        .map_err(|err| InitError::io("cannot move the new cluster into place", &err))?;
    // The rename is on disk only once its folder is: after a crash before
    // that, the cluster could be back under its temp name, which the next
    // init deletes as a leftover.
    let parent = dirs.data.parent().map(Path::to_path_buf);
    tokio::task::spawn_blocking(move || parent.map_or(Ok(()), |parent| sync_dir(&parent)))
        .await
        .map_err(io::Error::other)
        .flatten()
        .map_err(|err| InitError {
            reason: format!("its folder could not be synced to disk: {err}"),
            in_place: true,
        })?;
    info!(data = %dirs.data.display(), "the PostgreSQL data folder is ready");
    Ok(())
}

/// Flushes the entries of `dir` (a rename into it) to disk.
// `#[cfg(unix)]` outside `platform`, as in the installer: a folder's entries
// are flushed by opening the folder, which only Unix allows. NTFS logs them
// itself.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Writes Marsa's `pg_hba.conf` over initdb's, which keeps its mode (`0600`),
/// and syncs it: the cluster is renamed into place next.
async fn write_hba(path: &Path) -> io::Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .await?;
    file.write_all(PG_HBA_CONF.as_bytes()).await?;
    file.sync_all().await
}

/// Runs one step: `input` on stdin, then waits up to `limit`. A step that
/// runs longer is killed (`kill_on_drop`).
///
/// It fails on an exit code other than 0, or on a line with `ERROR:`,
/// `FATAL:`, or `PANIC:`, PostgreSQL's severities: a check on the output too,
/// in case a session went on after an error.
async fn run_step(
    mut cmd: Command,
    input: Option<&str>,
    limit: Duration,
    what: &str,
) -> Result<(), InitError> {
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    process::hide_console_window(&mut cmd);
    let failed = |reason: String| InitError::new(format!("{what} {reason}"));
    let mut child = cmd
        .spawn()
        .map_err(|err| failed(format!("did not start: {err}")))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        // Dropped at the end of the block: the step reads to the end.
        stdin
            .write_all(input.as_bytes())
            .await
            .map_err(|err| failed(format!("did not read its input: {err}")))?;
    }
    let output = tokio::time::timeout(limit, child.wait_with_output())
        .await
        .map_err(|_| failed(format!("took longer than {} s", limit.as_secs())))?
        .map_err(|err| failed(format!("cannot be waited for: {err}")))?;
    let text = [&output.stderr, &output.stdout]
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .join("\n");
    let severe = ["ERROR:", "FATAL:", "PANIC:"];
    let error_line = text
        .lines()
        .any(|line| severe.iter().any(|s| line.contains(s)));
    if output.status.success() && !error_line {
        return Ok(());
    }
    let lines: Vec<_> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let tail = lines[lines.len().saturating_sub(ERROR_LINES)..].join(" | ");
    Err(failed(format!("failed ({}): {tail}", output.status)))
}

/// Deletes the temp folders of earlier inits of this major in `parent`: the
/// folders whose name starts with `prefix`. Never one of another major, and
/// never a link (`file_type` does not follow it).
async fn remove_leftovers(parent: &Path, prefix: &str) {
    let Ok(mut entries) = tokio::fs::read_dir(parent).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let ours = entry.file_name().to_string_lossy().starts_with(prefix);
        if !ours || !entry.file_type().await.is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        match remove_dir(&path).await {
            Ok(()) => info!(temp = %path.display(), "deleted the temp folder of an earlier init"),
            Err(err) => warn!(temp = %path.display(), %err, "cannot delete an old temp folder"),
        }
    }
}

/// Deletes `dir` and what is in it. Nothing there is not an error.
async fn remove_dir(dir: &Path) -> io::Result<()> {
    match tokio::fs::remove_dir_all(dir).await {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// How the supervisor runs PostgreSQL: `postgres` with [`postgres_args`],
/// so on `127.0.0.1:<port>` only and with its socket in `run/`, stopped with
/// SIGINT, then SIGQUIT, never SIGTERM ([`postgres_stop`]). PostgreSQL logs
/// to stderr, which the supervisor keeps in `logs/postgres.log`, as for
/// every service.
///
/// Never restarted after it exits, unlike Caddy. A refusal at its start
/// (another server on the folder, the port, or the socket) would only come
/// again, the status must keep the run's own exit, and after a crash of the
/// postmaster its backends may still hold the shared memory, so a restart
/// fails too. PostgreSQL restarts its own crashed backends itself.
pub fn postgres_spec(program: PathBuf, dirs: &PostgresDirs, port: u16) -> ServiceSpec {
    ServiceSpec {
        id: POSTGRES_ID.to_owned(),
        program,
        args: postgres_args(dirs, port),
        env: std::collections::BTreeMap::new(),
        cwd: None,
        restart: RestartPolicy::Never,
        stop: postgres_stop(),
    }
}

/// Why an action on PostgreSQL failed. A start that fails also sets the
/// status `Failed` with the same code, and says why in its recent lines.
#[derive(Debug)]
pub enum ActionError {
    /// Marsa runs PostgreSQL on macOS and Linux only for now (Windows is
    /// task M6-07).
    Unsupported,
    /// `global.toml` cannot be read.
    Config(ConfigError),
    /// No installed version fits `[postgres] default`.
    NotInstalled(String),
    Init(InitError),
    /// The check before the start refused: another program holds the port,
    /// or a server runs on the data folder. Nothing was started or stopped.
    InUse(ServiceBusy),
    /// Another action on PostgreSQL has not finished.
    Busy,
    ProgramNotAbsolute(ProgramNotAbsolute),
}

impl ActionError {
    /// Stable error name for the UI.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unsupported => "postgres_unsupported_platform",
            Self::Config(err) => err.code(),
            Self::NotInstalled(_) => "postgres_not_installed",
            Self::Init(err) => err.code(),
            Self::InUse(busy) => busy.code(),
            Self::Busy => "service_busy",
            Self::ProgramNotAbsolute(err) => err.code(),
        }
    }
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("Marsa runs PostgreSQL on macOS and Linux only"),
            Self::Config(err) => err.fmt(f),
            Self::NotInstalled(reason) => f.write_str(reason),
            Self::Init(err) => err.fmt(f),
            Self::InUse(busy) => busy.fmt(f),
            Self::Busy => f.write_str("another action on this service is running"),
            Self::ProgramNotAbsolute(err) => err.fmt(f),
        }
    }
}

/// Starts, stops, and watches the one PostgreSQL. It lives in
/// `Daemon::postgres`; its task, while it runs, is in `Daemon::procs`, so a
/// shutdown stops it, with the time its stop needs
/// (`ServiceHandle::stop_limit`).
#[derive(Debug)]
pub struct PostgresManager {
    /// Who runs under a pid and who holds a port. Tests put a fake here.
    pub probes: Arc<dyn Probes>,
    /// How long a start waits for the server to take connections.
    pub ready_limit: Duration,
    /// The system whose programs run: [`Platform::current`].
    pub platform: Option<Platform>,
    /// The program of the last start, so an uninstall never deletes the
    /// version that runs. Use [`PostgresManager::running_program`].
    started: Mutex<Option<PathBuf>>,
    /// The status and recent lines, kept across runs, so `seq` never goes
    /// back and the lines of a failed run stay readable.
    runtime: Arc<Mutex<ServiceRuntime>>,
    /// The start that runs now, so a stop can drop it instead of waiting
    /// for it: a first start may run `initdb` for up to 2 minutes.
    starting: Mutex<Option<CancellationToken>>,
    /// Held for the whole of each action, so two starts never run two
    /// servers. A tokio lock held across `.await` as a serialization token,
    /// not a guard of data (manager decision 2026-11-14). In an `Arc`, so a
    /// task can own its guard.
    busy: Arc<tokio::sync::Mutex<()>>,
}

impl Default for PostgresManager {
    /// The real system. Nothing is read or started.
    fn default() -> Self {
        Self {
            probes: Arc::new(SystemProbes),
            ready_limit: READY_LIMIT,
            platform: Platform::current(),
            started: Mutex::default(),
            starting: Mutex::default(),
            runtime: Arc::new(Mutex::new(ServiceRuntime::new(POSTGRES_ID))),
            busy: Arc::default(),
        }
    }
}

/// What a start runs, after the check before it and the first init.
struct Prepared {
    spec: ServiceSpec,
    /// The cluster, for its `postmaster.pid`.
    data: PathBuf,
    /// The socket, for the name of its lock file: `None` for TCP only.
    socket: Option<PathBuf>,
    port: u16,
}

enum Ready {
    Yes,
    /// The run ended by itself first: the status says why.
    Ended,
    TimedOut,
}

impl PostgresManager {
    pub fn status(&self) -> ServiceStatus {
        lock(&self.runtime).status.clone()
    }

    /// The last lines, oldest first.
    pub fn recent_logs(&self) -> Vec<LogLine> {
        lock(&self.runtime).logs.recent()
    }

    /// Whether `GET /v1/services` lists PostgreSQL: when a version is
    /// installed, or when its status says more than `stopped` (a start
    /// that failed because none is installed).
    pub fn listed(&self, data_dir: &Path) -> bool {
        self.status().state != ServiceState::Stopped || any_installed(data_dir)
    }

    /// The program PostgreSQL runs from now, or `None` while it does not run.
    pub fn running_program(&self, daemon: &Daemon) -> Option<PathBuf> {
        if !daemon.proc_alive(POSTGRES_ID) {
            return None;
        }
        lock(&self.started).clone()
    }

    /// Runs `action` and returns the status after it. An action that finds
    /// another one running waits up to `wait` for it in line, as for Caddy
    /// (`CaddyManager::run_waiting`). `start` on a running PostgreSQL and
    /// `stop` on a stopped one change nothing. Once the daemon begins to
    /// stop, nothing runs: the shutdown stops PostgreSQL itself.
    ///
    /// A `stop` does not wait for a start: it drops it first (the start
    /// with the daemon, of the API, or after an install), then takes its
    /// turn and stops what that start spawned.
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
        if action == ServiceAction::Stop {
            self.drop_start();
        }
        let busy = match Arc::clone(&self.busy).try_lock_owned() {
            Ok(busy) => busy,
            Err(_) => {
                info!(%action, "another action on postgres runs; waiting for it");
                let turn = tokio::time::timeout(wait, Arc::clone(&self.busy).lock_owned());
                tokio::select! {
                    () = daemon.shutdown.cancelled() => return Err(ActionError::Busy),
                    turn = turn => turn.map_err(|_| ActionError::Busy)?,
                }
            }
        };
        // Also when the turn came at the same moment as the shutdown.
        if daemon.shutdown.is_cancelled() {
            return Err(ActionError::Busy);
        }
        self.act(daemon, action, busy).await
    }

    /// Runs `action`. `_busy` proves that no other action runs.
    async fn act(
        &self,
        daemon: &Daemon,
        action: ServiceAction,
        _busy: tokio::sync::OwnedMutexGuard<()>,
    ) -> Result<ServiceStatus, ActionError> {
        match action {
            ServiceAction::Start => self.start_or_drop(daemon).await?,
            ServiceAction::Stop => self.stop(daemon).await,
            ServiceAction::Restart => {
                self.stop(daemon).await;
                self.start_or_drop(daemon).await?;
            }
        }
        Ok(self.status())
    }

    /// [`Self::start`], until a stop drops it ([`Self::drop_start`]). A
    /// dropped start ends at its next `.await`: a step of the first init
    /// is killed (`kill_on_drop`) and leaves a temp folder that the next
    /// init deletes; a server it spawned is in `Daemon::procs` already,
    /// because the spawn and `add_proc` have no `.await` between them, so
    /// the stop that dropped it stops it.
    async fn start_or_drop(&self, daemon: &Daemon) -> Result<(), ActionError> {
        let token = CancellationToken::new();
        *lock(&self.starting) = Some(token.clone());
        let result = tokio::select! {
            biased;
            () = token.cancelled() => {
                info!("a stop dropped the start of postgres");
                Ok(())
            }
            result = self.start(daemon) => result,
        };
        // Only this action runs (`busy`), so the token there is this one,
        // unless a stop took it.
        lock(&self.starting).take();
        result
    }

    /// Drops the start that runs now, if any.
    fn drop_start(&self) {
        if let Some(token) = lock(&self.starting).take() {
            info!("a stop drops the start of postgres that runs");
            token.cancel();
        }
    }

    async fn start(&self, daemon: &Daemon) -> Result<(), ActionError> {
        // Our own supervisor runs it already. It holds the port and
        // `postmaster.pid`, so the check before a start would call it busy.
        if daemon.proc_alive(POSTGRES_ID) {
            return Ok(());
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
            data,
            socket,
            port,
        } = prepared;
        // This run's lines come after the ones there now.
        let first_seq = self.recent_logs().last().map_or(0, |line| line.seq + 1);
        self.spawn(daemon, spec)?;
        match self.wait_ready(daemon, port).await {
            Ready::Yes => info!(port, "postgres takes connections"),
            Ready::Ended => {
                // Only a run that exited by itself: a shutdown that took the
                // handle and stopped it leaves `Stopped`, and its lines are
                // not read.
                let state = self.status().state;
                let exited = ServiceState::Failed {
                    code: EXITED.to_owned(),
                };
                if state == exited {
                    let refused = self.lock_refusal(first_seq, &data, socket.as_deref(), port);
                    if let Some((busy, reason)) = refused.await {
                        self.fail(&daemon.events, busy.code(), &reason);
                        return Err(ActionError::InUse(busy));
                    }
                }
                warn!(?state, "postgres ended before it was ready");
            }
            Ready::TimedOut => {
                warn!(limit = ?self.ready_limit, "postgres takes no connections; stopping it");
                self.stop(daemon).await;
                let reason = format!(
                    "it took no connections on 127.0.0.1:{port} within {} s",
                    self.ready_limit.as_secs()
                );
                self.fail(&daemon.events, "postgres_start_timeout", &reason);
            }
        }
        Ok(())
    }

    /// The version, the check before the start (ADR 0023, section 7), then
    /// the first init when the cluster is new. The check comes first, so a
    /// busy port makes no cluster.
    async fn prepare(&self, daemon: &Daemon) -> Result<Prepared, ActionError> {
        let settings = read_settings(&daemon.data_dir)?;
        let (platform, version, programs) = self.installed(&daemon.data_dir, &settings)?;
        let port = settings.port.unwrap_or(POSTGRES_PORT);
        let dirs = PostgresDirs::new(&daemon.data_dir, version.major);
        let servers = installed_servers(&daemon.data_dir, platform);
        let facts = start_facts(&*self.probes, port, &dirs.data, &servers).await;
        info!(?facts, "checked before the start of postgres");
        if let StartDecision::Refuse(busy) = decide_start(&facts) {
            return Err(ActionError::InUse(busy));
        }
        ensure_cluster(&programs, &dirs)
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
            note(&self.runtime, &daemon.events, &text);
        }
        Ok(Prepared {
            spec: postgres_spec(programs.postgres, &dirs, port),
            socket: socket.ok(),
            data: dirs.data,
            port,
        })
    }

    /// The programs to run for `settings`: the newest installed version of
    /// the `default` line, or of all lines when it is not set.
    fn installed(
        &self,
        data_dir: &Path,
        settings: &ServerSettings,
    ) -> Result<(Platform, Version, Programs), ActionError> {
        let platform = match self.platform {
            Some(platform) if !matches!(platform, Platform::WindowsX64 | Platform::Any) => platform,
            _ => return Err(ActionError::Unsupported),
        };
        let line = settings.default.as_deref();
        let missing = || {
            let what = line.map_or_else(String::new, |line| format!(" {line}"));
            ActionError::NotInstalled(format!("no PostgreSQL{what} is installed"))
        };
        let installed = RuntimesDir::in_dir(data_dir)
            .installed(Runtime::Postgres)
            .map_err(|err| ActionError::NotInstalled(err.to_string()))?;
        let version = line_version(line, &installed).ok_or_else(missing)?;
        let programs = programs(data_dir, platform, &version)
            .filter(|programs| programs.postgres.is_file())
            .ok_or_else(missing)?;
        Ok((platform, version, programs))
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
        // A handle this replaces belongs to a run that has ended.
        daemon.add_proc(POSTGRES_ID, handle);
        Ok(())
    }

    /// Asks `127.0.0.1:<port>` ([`answers`]) until our server answers, its
    /// run ends, or the limit. The check before the start found the port
    /// free, so what answers is our server.
    async fn wait_ready(&self, daemon: &Daemon, port: u16) -> Ready {
        let poll = async {
            loop {
                if !daemon.proc_alive(POSTGRES_ID) {
                    return Ready::Ended;
                }
                if answers(port).await {
                    return Ready::Yes;
                }
                tokio::time::sleep(READY_POLL).await;
            }
        };
        tokio::time::timeout(self.ready_limit, poll)
            .await
            .unwrap_or(Ready::TimedOut)
    }

    /// Why a run that ended by itself before it took connections did not
    /// start, when PostgreSQL refused because a live process holds one of
    /// its lock files ([`startup_lock_conflict`] on this run's stderr, from
    /// `first_seq`), with the program at that pid now:
    ///
    /// - `postmaster.pid`: `service_data_dir_busy`, as the check before the
    ///   start reports it. The pid comes from PostgreSQL's hint, else from
    ///   `postmaster.pid` (read only), and may stay unknown;
    /// - the lock of the socket: `service_port_busy`, the server that uses
    ///   the socket of this port.
    ///
    /// Only read once the run has ended: a line never stops a server. That
    /// process gets no signal, and `postmaster.pid` stays: the pid may be
    /// another program's now (orphans are task M6-04).
    async fn lock_refusal(
        &self,
        first_seq: u64,
        data: &Path,
        socket: Option<&Path>,
        port: u16,
    ) -> Option<(ServiceBusy, String)> {
        let recent = self.recent_logs();
        let stderr: Vec<&str> = recent
            .iter()
            .filter(|line| line.seq >= first_seq && line.stream == LogStream::Stderr)
            .map(|line| line.text.as_str())
            .collect();
        let conflict = startup_lock_conflict(&stderr, socket)?;
        let no_signal = "and Marsa sends it no signal";
        match conflict {
            LockConflict::DataDir { pid } => {
                let pid = pid.or_else(|| {
                    let text = process::read_small_file(&data.join("postmaster.pid"))?;
                    postmaster_pid(&text)
                });
                let busy = ServiceBusy::DataDir { pid };
                let reason = match pid {
                    Some(pid) => {
                        let program = self.program_at(pid).await;
                        format!("{busy}; pid {pid} runs {program} now, {no_signal}")
                    }
                    None => format!("{busy}; PostgreSQL named no pid, {no_signal}"),
                };
                Some((busy, reason))
            }
            LockConflict::Socket { pid } => {
                let owner = match pid {
                    Some(pid) => Some(format!("{} (pid {pid})", self.program_at(pid).await)),
                    None => None,
                };
                let busy = ServiceBusy::Port { port, owner };
                let lock = socket.map(|socket| socket.display().to_string());
                let lock = lock.unwrap_or_default();
                let reason = format!("{busy}: it holds the lock of the socket {lock}, {no_signal}");
                Some((busy, reason))
            }
        }
    }

    /// The program at `pid` now, in words, for a message.
    async fn program_at(&self, pid: u32) -> String {
        match self.probes.real_identity(pid).await {
            Liveness::Alive { exe, .. } => exe.display().to_string(),
            Liveness::Gone => "nothing (it has ended since)".to_owned(),
            Liveness::Unknown => "a program that cannot be told".to_owned(),
        }
    }

    /// Stops the task: SIGINT, then SIGQUIT, then a kill, by the spec. A
    /// PostgreSQL that this daemon did not start is never touched (orphans
    /// are task M6-04).
    async fn stop(&self, daemon: &Daemon) {
        // `None`: nothing runs, or a shutdown took the handle and stops it.
        if let Some(handle) = daemon.take_proc(POSTGRES_ID)
            && let Err(err) = handle.stop().await
        {
            warn!(%err, "the postgres task failed");
        }
    }

    /// Says why in the recent lines, and sets `Failed { code }`. Only while
    /// no task runs: the supervisor owns the status then.
    fn fail(&self, events: &broadcast::Sender<Event>, code: &str, reason: &str) {
        warn!(code, %reason, "postgres was not started");
        let text = format!("marsad: PostgreSQL was not started: {reason}.");
        note(&self.runtime, events, &text);
        set_failed(&self.runtime, events, code);
    }
}

/// Turns PostgreSQL on or off in `global.toml` (`[postgres] enabled`), for
/// the start with the daemon. Writes only a change. The caller holds
/// `Daemon::lock_global`.
///
/// # Errors
///
/// The `config_*` codes when `global.toml` cannot be read or written.
pub fn set_enabled(data_dir: &Path, enabled: bool) -> Result<(), ConfigError> {
    let path = ConfigPaths::in_dir(data_dir).global;
    let mut global = config::load_global(&path)?;
    if global.postgres.enabled == enabled {
        return Ok(());
    }
    global.postgres.enabled = enabled;
    config::save_global(&path, &global)
}

/// After an install of a PostgreSQL of the line `major`: when `[postgres]`
/// has no `default` yet, makes `major` the default and turns it on, and
/// returns `true`, so the caller starts it. Otherwise changes nothing. The
/// caller holds `Daemon::lock_global`.
///
/// # Errors
///
/// The `config_*` codes when `global.toml` cannot be read or written.
pub fn first_install(data_dir: &Path, major: u64) -> Result<bool, ConfigError> {
    let path = ConfigPaths::in_dir(data_dir).global;
    let mut global = config::load_global(&path)?;
    if global.postgres.default.is_some() {
        return Ok(false);
    }
    global.postgres.default = Some(major.to_string());
    global.postgres.enabled = true;
    config::save_global(&path, &global)?;
    Ok(true)
}

/// Starts PostgreSQL in the background after its first install. A stop
/// drops it (`run_waiting`), and a daemon that stops starts nothing. A
/// failure only sets the status: the install has succeeded.
pub fn start_after_install(daemon: &Arc<Daemon>) {
    let daemon = Arc::clone(daemon);
    tokio::spawn(async move {
        let postgres = &daemon.postgres;
        let start = postgres.run_waiting(&daemon, ServiceAction::Start, START_WAIT);
        let result = tokio::select! {
            biased;
            () = daemon.shutdown.cancelled() => return,
            result = start => result,
        };
        match result {
            Ok(status) => info!(state = ?status.state, "postgres after its first install"),
            Err(err) => warn!(code = err.code(), %err, "postgres did not start after its install"),
        }
    });
}

/// How long the start after an install waits for another action on
/// PostgreSQL, such as a start the user asked for meanwhile.
const START_WAIT: Duration = Duration::from_secs(30);

/// `[postgres]` of `global.toml`.
fn read_settings(data_dir: &Path) -> Result<ServerSettings, ActionError> {
    let path = ConfigPaths::in_dir(data_dir).global;
    let global = config::load_global(&path).map_err(ActionError::Config)?;
    Ok(global.postgres)
}

/// Starts PostgreSQL in the background when `[postgres] enabled = true` and
/// a version of its `default` line is installed (ADR 0023, section 5), the
/// way Caddy starts with the daemon (task M5-16): the start of
/// `marsa services start postgres`, with the check before it. Its turn is
/// taken here, before the API serves, so a request waits for it. A failure
/// only sets the status `Failed` with its code: the daemon runs on.
///
/// A shutdown drops the start at its next `.await`. The spawn and
/// `add_proc` have no `.await` between them, so a server it spawned is in
/// `Daemon::procs`, which the shutdown stops. A dropped init kills its
/// program (`kill_on_drop`) and leaves a temp folder, which the next init
/// deletes.
pub fn start_at_launch(daemon: &Arc<Daemon>) -> Launch {
    let postgres = &daemon.postgres;
    let settings = match read_settings(&daemon.data_dir) {
        Ok(settings) => settings,
        // Without PostgreSQL, a broken `global.toml` is none of its business:
        // its status and lines stay as they are.
        Err(err) if !any_installed(&daemon.data_dir) => {
            info!(
                code = err.code(),
                "no postgres is installed; it does not start with the daemon"
            );
            return Launch::ended(POSTGRES_ID, Launched::NotInstalled);
        }
        Err(err) => {
            postgres.fail(&daemon.events, err.code(), &err.to_string());
            return Launch::ended(POSTGRES_ID, Launched::Failed(err.code()));
        }
    };
    if !settings.enabled {
        info!("postgres is off; it does not start with the daemon");
        return Launch::ended(POSTGRES_ID, Launched::Off);
    }
    // First, so a missing version does not mark the status failed.
    if let Err(err) = postgres.installed(&daemon.data_dir, &settings) {
        warn!(code = err.code(), %err, "postgres is on, but does not start with the daemon");
        return Launch::ended(POSTGRES_ID, Launched::NotInstalled);
    }
    let Ok(busy) = Arc::clone(&postgres.busy).try_lock_owned() else {
        info!("another action on postgres runs; it decides instead of the start with the daemon");
        return Launch::ended(POSTGRES_ID, Launched::Busy);
    };
    info!("starting postgres with the daemon");
    let daemon = Arc::clone(daemon);
    let task = tokio::spawn(async move {
        tokio::select! {
            // A daemon that already stops starts nothing.
            biased;
            () = daemon.shutdown.cancelled() => Launched::ShuttingDown,
            launched = launch(&daemon, busy) => launched,
        }
    });
    Launch::running(POSTGRES_ID, task)
}

/// Whether any PostgreSQL version is installed. A folder that cannot be read
/// counts as one: then the error is reported.
pub fn any_installed(data_dir: &Path) -> bool {
    let installed = RuntimesDir::in_dir(data_dir).installed(Runtime::Postgres);
    installed.map_or(true, |versions| !versions.is_empty())
}

async fn launch(daemon: &Daemon, busy: tokio::sync::OwnedMutexGuard<()>) -> Launched {
    match daemon
        .postgres
        .act(daemon, ServiceAction::Start, busy)
        .await
    {
        Ok(status) => {
            match &status.state {
                ServiceState::Running => info!("postgres started with the daemon"),
                ServiceState::Failed { code } => {
                    warn!(%code, "postgres did not start with the daemon");
                }
                state => warn!(?state, "postgres is not running after its start"),
            }
            Launched::Ran(status)
        }
        Err(err) => {
            warn!(code = err.code(), %err, "postgres did not start with the daemon");
            Launched::Failed(err.code())
        }
    }
}

/// Whether PostgreSQL takes connections on `127.0.0.1:<port>`: it answers
/// an [`SSL_REQUEST`] with one byte, `N` or `S`. Only a postmaster whose
/// start is done answers: it has taken its lock files and made all its
/// sockets. A bare TCP connection is not enough, because the TCP listener
/// is made before the socket and its lock file. PostgreSQL logs nothing for
/// this: the connection closes before any startup message.
async fn answers(port: u16) -> bool {
    let ask = async {
        let address = (Ipv4Addr::LOCALHOST, port);
        let mut stream = tokio::net::TcpStream::connect(address).await.ok()?;
        stream.write_all(&SSL_REQUEST).await.ok()?;
        let mut answer = [0_u8; 1];
        stream.read_exact(&mut answer).await.ok()?;
        Some(matches!(&answer, b"N" | b"S"))
    };
    matches!(
        tokio::time::timeout(CONNECT_LIMIT, ask).await,
        Ok(Some(true))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use marsa_core::databases::{PortCheck, ServiceBusy, StartDecision, decide_start};

    /// Answers what it was given, and keeps the pids it was asked about.
    #[derive(Debug)]
    struct FakeProbes {
        /// What runs at every pid.
        now: Mutex<Liveness>,
        lookup: PortLookup,
        asked: Mutex<Vec<u32>>,
    }

    /// `owner` listens on the port, or nobody.
    fn probes(now: Liveness, owner: Option<&str>) -> FakeProbes {
        let lookup = owner.map_or(PortLookup::Nobody, |owner| PortLookup::Owner(owner.into()));
        FakeProbes {
            now: Mutex::new(now),
            lookup,
            asked: Mutex::default(),
        }
    }

    impl Probes for FakeProbes {
        fn real_identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
            self.asked.lock().unwrap().push(pid);
            let now = self.now.lock().unwrap().clone();
            Box::pin(std::future::ready(now))
        }

        fn port_lookup(&self, _port: u16) -> BoxFuture<'_, PortLookup> {
            Box::pin(std::future::ready(self.lookup.clone()))
        }
    }

    fn alive(exe: &Path) -> Liveness {
        Liveness::Alive {
            exe: exe.to_path_buf(),
            started: "Wed Nov 18 10:00:00 2026".into(),
        }
    }

    /// A port nothing listens on now.
    fn free_port() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    const POSTMASTER_PID: &str = "4242\n/m/databases/postgres/18\n1763460000\n5432\n/m/run\n\
                                  127.0.0.1\n  5432001         0\nready   \n";

    #[tokio::test]
    async fn the_port_is_busy_when_the_bind_fails_or_lsof_names_an_owner() {
        let data = tempfile::tempdir().unwrap();
        let facts = async |port, owner| {
            let probes = probes(Liveness::Gone, owner);
            start_facts(&probes, port, data.path(), &[])
                .await
                .port_check
        };
        let port = free_port();
        assert_eq!(facts(port, None).await, PortCheck::Free);
        // Docker on `*:<port>`: our bind works on macOS, but lsof sees it.
        let docker = Some("com.docker.backend(77)");
        let busy = PortCheck::Busy {
            owner: docker.map(str::to_owned),
        };
        assert_eq!(facts(port, docker).await, busy);
        // A listener on `127.0.0.1:<port>`, and no lsof (Linux).
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let busy = PortCheck::Busy { owner: None };
        assert_eq!(facts(port, None).await, busy);
    }

    /// macOS: when lsof fails or takes too long, a holder of `*:<port>`
    /// could be there unseen, so there is no start, and it says why.
    #[tokio::test]
    async fn a_failed_look_at_the_port_refuses_as_unknown() {
        let data = tempfile::tempdir().unwrap();
        let mut failed = probes(Liveness::Gone, None);
        failed.lookup = PortLookup::Failed;
        let port = free_port();
        let facts = start_facts(&failed, port, data.path(), &[]).await;
        assert_eq!(facts.port_check, PortCheck::Unknown);
        let refused = StartDecision::Refuse(ServiceBusy::PortUnknown { port });
        assert_eq!(decide_start(&facts), refused);
    }

    #[tokio::test]
    async fn postmaster_pid_is_read_and_its_pid_classified() {
        let data = tempfile::tempdir().unwrap();
        let servers = [data.path().join("runtimes/postgres/18.7.0/bin/postgres")];
        let check = async |now: Liveness| {
            let probes = probes(now, None);
            let facts = start_facts(&probes, free_port(), data.path(), &servers).await;
            (facts.pid_file, probes.asked.into_inner().unwrap())
        };
        // No file: nobody is asked about any pid.
        let gone = || Liveness::Gone;
        assert_eq!(check(gone()).await, (PidFileCheck::NoPid, vec![]));

        let file = data.path().join("postmaster.pid");
        std::fs::write(&file, POSTMASTER_PID).unwrap();
        let pid = 4242;
        let server = check(alive(&servers[0])).await;
        assert_eq!(server, (PidFileCheck::Server { pid }, vec![pid]));
        let other = check(alive(Path::new("/bin/sleep"))).await.0;
        assert_eq!(other, PidFileCheck::Other { pid });
        assert_eq!(check(gone()).await.0, PidFileCheck::Dead { pid });
        let unknown = check(Liveness::Unknown).await.0;
        assert_eq!(unknown, PidFileCheck::Unknown { pid });

        // Junk, and a file too long to be PostgreSQL's: no pid.
        let long = format!("{POSTMASTER_PID}{}", "x".repeat(5000));
        for text in ["", "abc\n", "0\n", long.as_str()] {
            std::fs::write(&file, text).unwrap();
            assert_eq!(
                check(alive(&servers[0])).await,
                (PidFileCheck::NoPid, vec![])
            );
        }
    }

    /// A link is never followed, even to a real `postmaster.pid`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_linked_postmaster_pid_names_no_pid() {
        let data = tempfile::tempdir().unwrap();
        let real = data.path().join("elsewhere.pid");
        std::fs::write(&real, POSTMASTER_PID).unwrap();
        std::os::unix::fs::symlink(&real, data.path().join("postmaster.pid")).unwrap();
        let probes = probes(Liveness::Gone, None);
        let facts = start_facts(&probes, free_port(), data.path(), &[]).await;
        assert_eq!(facts.pid_file, PidFileCheck::NoPid);
        assert!(probes.asked.into_inner().unwrap().is_empty());
    }

    /// The facts as the system gives them, through `decide_start`.
    #[tokio::test]
    async fn real_facts_lead_to_their_decision() {
        let data = tempfile::tempdir().unwrap();
        let ours = data.path().join("runtimes/postgres/17.12.0/bin/postgres");
        let servers = [ours.clone()];
        std::fs::write(data.path().join("postmaster.pid"), POSTMASTER_PID).unwrap();
        let decide = async |port, now, owner| {
            let probes = probes(now, owner);
            decide_start(&start_facts(&probes, port, data.path(), &servers).await)
        };
        // Our older version still runs on the folder: refused, whatever
        // holds the port.
        let busy = StartDecision::Refuse(ServiceBusy::DataDir { pid: Some(4242) });
        assert_eq!(decide(free_port(), alive(&ours), None).await, busy);
        // A stale file, or a pid that another program has now: PostgreSQL
        // decides at its start.
        let sleep = alive(Path::new("/bin/sleep"));
        assert_eq!(decide(free_port(), sleep, None).await, StartDecision::Start);
        assert_eq!(
            decide(free_port(), Liveness::Gone, None).await,
            StartDecision::Start
        );
        // Homebrew's PostgreSQL holds the port.
        let port = free_port();
        let owner = Some("postgres(901)");
        let refused = StartDecision::Refuse(ServiceBusy::Port {
            port,
            owner: owner.map(str::to_owned),
        });
        assert_eq!(decide(port, Liveness::Gone, owner).await, refused);
    }

    #[test]
    fn installed_servers_are_each_versions_postgres() {
        let dir = tempfile::tempdir().unwrap();
        let platform = Platform::current().unwrap();
        let version = |text: &str| Version::parse(text).unwrap();
        let mut want = Vec::new();
        for text in ["17.12.0", "18.7.0"] {
            let programs = programs(dir.path(), platform, &version(text)).unwrap();
            std::fs::create_dir_all(programs.postgres.parent().unwrap()).unwrap();
            std::fs::write(&programs.postgres, "").unwrap();
            assert!(
                programs
                    .initdb
                    .starts_with(programs.postgres.parent().unwrap())
            );
            want.push(programs.postgres);
        }
        let servers = installed_servers(dir.path(), platform);
        for program in &want {
            assert!(servers.contains(program), "{servers:?}");
        }
        let mut names = servers.iter().filter_map(|path| path.file_stem());
        assert!(names.all(|name| name == "postgres"));
    }

    /// A broken `global.toml` (task M6-03 review): without PostgreSQL it
    /// leaves PostgreSQL's status and lines alone; with one, the status
    /// says why it did not start.
    #[tokio::test]
    async fn a_broken_global_toml_fails_postgres_only_when_one_is_installed() {
        let dir = tempfile::tempdir().unwrap();
        let global = ConfigPaths::in_dir(dir.path()).global;
        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        std::fs::write(&global, "[postgres\n").unwrap();
        let daemon = Arc::new(Daemon::new(dir.path().to_path_buf()));
        let launched = start_at_launch(&daemon).wait(Duration::from_secs(5)).await;
        assert_eq!(launched, Some(Launched::NotInstalled));
        assert_eq!(daemon.postgres.status().state, ServiceState::Stopped);
        assert!(daemon.postgres.recent_logs().is_empty());

        let version =
            RuntimesDir::in_dir(dir.path()).version_dir(Runtime::Postgres, &Version::new(18, 7, 0));
        std::fs::create_dir_all(version).unwrap();
        let launched = start_at_launch(&daemon).wait(Duration::from_secs(5)).await;
        assert_eq!(launched, Some(Launched::Failed("config_invalid")));
        let failed = ServiceState::Failed {
            code: "config_invalid".into(),
        };
        assert_eq!(daemon.postgres.status().state, failed);
        assert_eq!(daemon.postgres.recent_logs().len(), 1);
    }

    #[test]
    fn postgres_runs_on_loopback_and_stops_with_sigint_then_sigquit() {
        let dirs = PostgresDirs::new(Path::new("/m"), 18);
        let program = PathBuf::from("/m/runtimes/postgres/18.7.0/bin/postgres");
        let spec = postgres_spec(program.clone(), &dirs, 5433);
        assert_eq!(spec.id, POSTGRES_ID);
        assert_eq!(spec.program, program);
        assert_eq!(spec.args, postgres_args(&dirs, 5433));
        assert_eq!(spec.stop, postgres_stop());
        assert_eq!(spec.restart, RestartPolicy::Never);
        assert!(spec.env.is_empty() && spec.cwd.is_none());
    }

    /// The test binary stands in for the `postgres` server, run by a script
    /// at `bin/postgres` with a fake `initdb` next to it: macOS and Linux
    /// only, the systems of this task.
    #[cfg(unix)]
    mod server {
        use super::*;
        use std::fs;
        use std::os::unix::fs::PermissionsExt as _;

        use marsa_core::config::GlobalConfig;
        use tempfile::TempDir;

        use crate::daemon::supervisor::STOP_MARGIN;

        const FAKE: &str = "MARSA_FAKE_POSTGRES";
        /// Generous: a test binary can start slowly on a busy CI machine.
        const WAIT: Duration = Duration::from_secs(30);
        /// The program the pid of the lock file runs now.
        const OTHER: &str = "/Applications/Other.app/Contents/MacOS/Other";

        /// Not a check: the fake server's entry point. In a normal test run
        /// the variable is unset and this returns at once.
        ///
        /// - `serve` listens on the `port=` of its arguments, answers an
        ///   `SSLRequest` with `N`, prints each stop signal, ignores
        ///   SIGTERM, and exits at SIGINT, as PostgreSQL does with a client
        ///   connected.
        /// - `noisy` serves too, but prints PostgreSQL's refusal for
        ///   `postmaster.pid` first, and again after its first answer.
        /// - `locked` prints that refusal, held by pid 4242, and exits 1.
        /// - `locked-no-hint` prints the refusal alone, and exits 1.
        /// - `locked-hang` prints the refusal, then waits for a signal: a
        ///   shutdown stops it.
        /// - `socket-locked` does the same as `locked` for the lock of its
        ///   socket.
        #[test]
        fn fake_postgres() {
            let Ok(mode) = std::env::var(FAKE) else {
                return;
            };
            let args = std::env::var("MARSA_FAKE_POSTGRES_ARGS").unwrap();
            let setting = |name: &str| {
                let found = args.split(' ').find_map(|arg| arg.strip_prefix(name));
                found.unwrap().to_owned()
            };
            let port: u16 = setting("port=").parse().unwrap();
            let at = format!("2026-11-18 10:00:00.123 UTC [{}]", std::process::id());
            let data_dir = || {
                eprintln!("{at} FATAL:  lock file \"postmaster.pid\" already exists");
                eprintln!(
                    "{at} HINT:  Is another postmaster (PID 4242) running in data directory \
                     \"/m\"?"
                );
            };
            match mode.as_str() {
                "locked" => {
                    data_dir();
                    std::process::exit(1);
                }
                "locked-no-hint" => {
                    eprintln!("{at} FATAL:  lock file \"postmaster.pid\" already exists");
                    std::process::exit(1);
                }
                "locked-hang" => {
                    data_dir();
                    let hang = async {
                        use tokio::signal::unix::{SignalKind, signal};
                        let mut int = signal(SignalKind::interrupt()).unwrap();
                        int.recv().await;
                        std::process::exit(0);
                    };
                    tokio::runtime::Runtime::new().unwrap().block_on(hang);
                }
                "socket-locked" => {
                    let socket = format!("{}/.s.PGSQL.{port}", setting("unix_socket_directories="));
                    eprintln!("{at} FATAL:  lock file \"{socket}.lock\" already exists");
                    eprintln!(
                        "{at} HINT:  Is another postmaster (PID 4242) using socket file \
                         \"{socket}\"?"
                    );
                    std::process::exit(1);
                }
                "noisy" => data_dir(),
                _ => {}
            }
            let serve = async {
                use tokio::io::AsyncReadExt as _;
                use tokio::signal::unix::{SignalKind, signal};
                let mut term = signal(SignalKind::terminate()).unwrap();
                let mut int = signal(SignalKind::interrupt()).unwrap();
                let address = (Ipv4Addr::LOCALHOST, port);
                let listener = tokio::net::TcpListener::bind(address).await.unwrap();
                eprintln!("fake ready");
                let mut answered = false;
                loop {
                    tokio::select! {
                        _ = term.recv() => eprintln!("fake got SIGTERM"),
                        _ = int.recv() => {
                            eprintln!("fake got SIGINT");
                            std::process::exit(0);
                        }
                        Ok((mut stream, _)) = listener.accept() => {
                            let mut request = [0_u8; 8];
                            if stream.read_exact(&mut request).await.is_ok()
                                && request == SSL_REQUEST
                            {
                                let _ = stream.write_all(b"N").await;
                                if mode == "noisy" && !answered {
                                    data_dir();
                                }
                                answered = true;
                            }
                        }
                    }
                }
            };
            tokio::runtime::Runtime::new().unwrap().block_on(serve);
        }

        /// A daemon whose PostgreSQL 18.7.0 is the fake in `mode`, with
        /// `[postgres]` on, on a free port. The fakes write to `calls.log`.
        struct Pg {
            dir: TempDir,
            daemon: Arc<Daemon>,
            port: u16,
            probes: Arc<FakeProbes>,
            dirs: PostgresDirs,
        }

        fn pg(mode: &str, probes: FakeProbes) -> Pg {
            pg_in("data", mode, probes)
        }

        /// [`pg`] with the daemon's data folder named `name`.
        fn pg_in(name: &str, mode: &str, probes: FakeProbes) -> Pg {
            let dir = tempfile::tempdir().unwrap();
            let data = dir.path().join(name);
            let platform = Platform::current().unwrap();
            let version = Version::new(18, 7, 0);
            let programs = programs(&data, platform, &version).unwrap();
            fs::create_dir_all(programs.postgres.parent().unwrap()).unwrap();
            let calls = dir.path().join("calls.log").display().to_string();
            let initdb = format!(
                "#!/bin/sh\necho initdb >> '{calls}'\nmkdir \"$2\" || exit 3\n\
                 echo 18 > \"$2/PG_VERSION\"\necho x > \"$2/pg_hba.conf\"\n"
            );
            let exe = std::env::current_exe().unwrap().display().to_string();
            let postgres = format!(
                "#!/bin/sh\nif [ \"$1\" = --single ]; then cat > /dev/null; \
                 echo single >> '{calls}'; exit 0; fi\necho \"server $*\" >> '{calls}'\n\
                 {FAKE}='{mode}' MARSA_FAKE_POSTGRES_ARGS=\"$*\" exec '{exe}' \
                 daemon::databases::tests::server::fake_postgres --exact --nocapture\n"
            );
            for (path, script) in [(&programs.initdb, initdb), (&programs.postgres, postgres)] {
                fs::write(path, script).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            let port = free_port();
            set_postgres(&data, Some("18"), true, port);
            let probes = Arc::new(probes);
            let mut daemon = Daemon::new(data.clone());
            daemon.postgres.probes = Arc::clone(&probes) as Arc<dyn Probes>;
            Pg {
                dir,
                daemon: Arc::new(daemon),
                port,
                probes,
                dirs: PostgresDirs::new(&data, 18),
            }
        }

        fn set_postgres(data: &Path, default: Option<&str>, enabled: bool, port: u16) {
            let global = GlobalConfig {
                postgres: ServerSettings {
                    default: default.map(str::to_owned),
                    enabled,
                    port: Some(port),
                },
                ..GlobalConfig::default()
            };
            config::save_global(&ConfigPaths::in_dir(data).global, &global).unwrap();
        }

        impl Pg {
            async fn act(&self, action: ServiceAction) -> Result<ServiceStatus, ActionError> {
                let postgres = &self.daemon.postgres;
                postgres.run_waiting(&self.daemon, action, WAIT).await
            }

            fn calls(&self) -> Vec<String> {
                let text = fs::read_to_string(self.dir.path().join("calls.log"));
                text.unwrap_or_default()
                    .lines()
                    .map(str::to_owned)
                    .collect()
            }

            fn count(&self, call: &str) -> usize {
                let calls = self.calls();
                calls.iter().filter(|line| line.starts_with(call)).count()
            }

            fn texts(&self) -> Vec<String> {
                let lines = self.daemon.postgres.recent_logs();
                lines.into_iter().map(|line| line.text).collect()
            }

            /// Waits until the fake prints `text`.
            async fn wait_for(&self, text: &str) {
                let seen = async {
                    while !self.texts().iter().any(|line| line == text) {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                };
                tokio::time::timeout(WAIT, seen).await.expect(text);
            }
        }

        fn failed(code: &str) -> ServiceState {
            ServiceState::Failed { code: code.into() }
        }

        #[tokio::test]
        async fn a_start_makes_the_cluster_once_and_a_stop_sends_sigint_only() {
            let pg = pg("serve", probes(Liveness::Gone, None));
            let status = pg.act(ServiceAction::Start).await.unwrap();
            assert_eq!(status.state, ServiceState::Running);
            let args = postgres_args(&pg.dirs, pg.port).join(" ");
            assert_eq!(pg.calls(), ["initdb", "single", &format!("server {args}")]);
            assert!(pg.dirs.data.join("PG_VERSION").is_file());
            // It took a connection on the port, and runs the version's
            // program.
            let address = (Ipv4Addr::LOCALHOST, pg.port);
            tokio::net::TcpStream::connect(address).await.unwrap();
            let program = pg.daemon.postgres.running_program(&pg.daemon).unwrap();
            assert!(program.ends_with("18.7.0/bin/postgres"), "{program:?}");
            // A shutdown gives it the time of its whole ladder.
            let limit = pg.daemon.procs.lock().unwrap()[POSTGRES_ID].stop_limit;
            assert_eq!(limit, postgres_stop().total_wait() + STOP_MARGIN);

            // A start while it runs changes nothing; a restart makes no
            // second cluster.
            pg.act(ServiceAction::Start).await.unwrap();
            assert_eq!(pg.count("server"), 1);
            let status = pg.act(ServiceAction::Restart).await.unwrap();
            assert_eq!(status.state, ServiceState::Running);
            assert_eq!((pg.count("initdb"), pg.count("server")), (1, 2));

            pg.wait_for("fake ready").await;
            let status = pg.act(ServiceAction::Stop).await.unwrap();
            assert_eq!(status.state, ServiceState::Stopped);
            assert_eq!(pg.daemon.postgres.running_program(&pg.daemon), None);
            let texts = pg.texts();
            assert_eq!(texts.iter().filter(|t| *t == "fake got SIGINT").count(), 2);
            assert!(!texts.iter().any(|text| text == "fake got SIGTERM"));
            assert!(
                !texts
                    .iter()
                    .any(|text| text.contains("TCP connections only"))
            );
            let log = fs::read_to_string(pg.daemon.data_dir.join("logs/postgres.log")).unwrap();
            assert!(log.contains("fake got SIGINT"), "{log}");
        }

        /// No socket (a path too long, or a comma in it): TCP only, and the
        /// recent lines say why, and that the superuser cannot connect then.
        #[tokio::test]
        async fn a_data_folder_with_no_socket_says_why_and_that_the_superuser_is_out() {
            let deep = "d".repeat(100);
            for (folder, why) in [(deep.as_str(), "bytes long"), ("a,b", "a comma")] {
                let pg = pg_in(folder, "serve", probes(Liveness::Gone, None));
                let status = pg.act(ServiceAction::Start).await.unwrap();
                assert_eq!(status.state, ServiceState::Running);
                let server = pg.calls().pop().unwrap();
                assert!(server.contains(" -c unix_socket_directories= "), "{server}");
                let texts = pg.texts();
                let said = texts.iter().any(|text| {
                    text.contains(why)
                        && text.contains("TCP connections only")
                        && text.contains("superuser postgres")
                });
                assert!(said, "{texts:?}");
                pg.act(ServiceAction::Stop).await.unwrap();
            }
        }

        #[tokio::test]
        async fn a_busy_port_starts_nothing_and_makes_no_cluster() {
            // Homebrew's PostgreSQL on `127.0.0.1:<port>`.
            let pg = pg("serve", probes(Liveness::Gone, None));
            let held = TcpListener::bind((Ipv4Addr::LOCALHOST, pg.port)).unwrap();
            let err = pg.act(ServiceAction::Start).await.unwrap_err();
            assert_eq!(err.code(), "service_port_busy");
            let status = pg.daemon.postgres.status();
            assert_eq!(status.state, failed("service_port_busy"));
            let why = format!("port {} is in use by another program", pg.port);
            assert!(
                pg.texts().last().unwrap().contains(&why),
                "{:?}",
                pg.texts()
            );
            assert_eq!(pg.calls(), Vec::<String>::new());
            assert!(!pg.dirs.data.exists());
            drop(held);

            // Docker on `*:<port>`: the bind works, lsof names it.
            let pg = pg_with_owner();
            let err = pg.act(ServiceAction::Start).await.unwrap_err();
            assert_eq!(err.code(), "service_port_busy");
            assert!(err.to_string().contains("com.docker.backend(77)"), "{err}");
            assert_eq!(pg.calls(), Vec::<String>::new());
        }

        fn pg_with_owner() -> Pg {
            pg(
                "serve",
                probes(Liveness::Gone, Some("com.docker.backend(77)")),
            )
        }

        /// A data folder that `postmaster.pid` says is in use, by pid 4242.
        fn in_use(pg: &Pg) -> PathBuf {
            fs::create_dir_all(&pg.dirs.data).unwrap();
            let file = pg.dirs.data.join("postmaster.pid");
            fs::write(&file, POSTMASTER_PID).unwrap();
            file
        }

        #[tokio::test]
        async fn our_server_on_the_data_folder_refuses_at_once() {
            let pg = pg("serve", probes(Liveness::Gone, None));
            // Our own PostgreSQL 18.7.0 runs at the pid of the file.
            let data = &pg.daemon.data_dir;
            let platform = Platform::current().unwrap();
            let ours = programs(data, platform, &Version::new(18, 7, 0)).unwrap();
            *pg.probes.now.lock().unwrap() = alive(&ours.postgres);
            let file = in_use(&pg);
            let err = pg.act(ServiceAction::Start).await.unwrap_err();
            assert_eq!(err.code(), "service_data_dir_busy");
            assert!(err.to_string().contains("pid 4242"), "{err}");
            let state = pg.daemon.postgres.status().state;
            assert_eq!(state, failed("service_data_dir_busy"));
            assert_eq!(pg.calls(), Vec::<String>::new());
            assert_eq!(fs::read_to_string(file).unwrap(), POSTMASTER_PID);
        }

        /// The pid in `postmaster.pid` now runs another program: the check
        /// lets the start go, and PostgreSQL refuses by itself. The start
        /// reports it as the check before it would have.
        #[tokio::test]
        async fn a_lock_file_refusal_at_the_start_is_data_dir_busy_without_a_restart() {
            let pg = pg("locked", probes(alive(Path::new(OTHER)), None));
            let file = in_use(&pg);
            let err = pg.act(ServiceAction::Start).await.unwrap_err();
            let busy = "service_data_dir_busy";
            assert_eq!(err.code(), busy);
            assert!(err.to_string().contains("pid 4242"), "{err}");
            assert_eq!(pg.daemon.postgres.status().state, failed(busy));
            let last = pg.texts().pop().unwrap();
            assert!(last.contains("pid 4242"), "{last}");
            assert!(last.contains(OTHER), "{last}");
            // Asked about that pid, never signalled it (the fake has no way
            // to), and its file stays.
            assert_eq!(*pg.probes.asked.lock().unwrap(), [4242, 4242]);
            assert_eq!(fs::read_to_string(&file).unwrap(), POSTMASTER_PID);
            // Longer than a restart delay would be: none came.
            tokio::time::sleep(Duration::from_millis(1500)).await;
            assert_eq!(pg.count("server"), 1);
            assert!(!pg.daemon.proc_alive(POSTGRES_ID));
            assert_eq!(pg.daemon.postgres.status().state, failed(busy));
        }

        /// No hint and no `postmaster.pid`: still the data folder, with the
        /// pid unknown.
        #[tokio::test]
        async fn a_lock_file_refusal_without_a_pid_is_still_data_dir_busy() {
            let pg = pg("locked-no-hint", probes(alive(Path::new(OTHER)), None));
            let err = pg.act(ServiceAction::Start).await.unwrap_err();
            assert_eq!(err.code(), "service_data_dir_busy");
            let text = err.to_string();
            assert_eq!(text, "the data folder is in use by a running server");
            let state = pg.daemon.postgres.status().state;
            assert_eq!(state, failed("service_data_dir_busy"));
            let last = pg.texts().pop().unwrap();
            assert!(last.contains("named no pid"), "{last}");
            assert!(pg.probes.asked.lock().unwrap().is_empty());
        }

        /// A shutdown stops the run during its start: its lines are not
        /// read, even with PostgreSQL's refusal among them.
        #[tokio::test]
        async fn a_run_that_a_shutdown_stopped_is_not_read() {
            let pg = pg("locked-hang", probes(alive(Path::new(OTHER)), None));
            let daemon = Arc::clone(&pg.daemon);
            let start = tokio::spawn(async move {
                let postgres = &daemon.postgres;
                postgres
                    .run_waiting(&daemon, ServiceAction::Start, WAIT)
                    .await
            });
            let fatal = "FATAL:  lock file \"postmaster.pid\" already exists";
            let printed = async {
                while !pg.texts().iter().any(|text| text.ends_with(fatal)) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            tokio::time::timeout(WAIT, printed).await.unwrap();
            pg.daemon.stop_procs(WAIT).await;
            let status = start.await.unwrap().unwrap();
            assert_eq!(status.state, ServiceState::Stopped);
            assert!(pg.probes.asked.lock().unwrap().is_empty());
        }

        /// Another server holds the lock of our socket for this port.
        #[tokio::test]
        async fn a_socket_lock_refusal_is_port_busy() {
            let pg = pg("socket-locked", probes(alive(Path::new(OTHER)), None));
            let err = pg.act(ServiceAction::Start).await.unwrap_err();
            assert_eq!(err.code(), "service_port_busy");
            let text = err.to_string();
            assert!(text.contains(&format!("port {}", pg.port)), "{text}");
            assert!(text.contains(&format!("{OTHER} (pid 4242)")), "{text}");
            let state = pg.daemon.postgres.status().state;
            assert_eq!(state, failed("service_port_busy"));
            let last = pg.texts().pop().unwrap();
            assert!(last.contains(".s.PGSQL."), "{last}");
        }

        /// The fake prints PostgreSQL's refusal at its start and again once
        /// it answers, as a client could make it log, but runs on. A line
        /// never stops a server: only a run that ended by itself is read.
        #[tokio::test]
        async fn a_lock_line_from_a_running_server_changes_nothing() {
            let pg = pg("noisy", probes(alive(Path::new(OTHER)), None));
            let status = pg.act(ServiceAction::Start).await.unwrap();
            assert_eq!(status.state, ServiceState::Running);
            let pid = status.pid;
            // The second refusal comes after its first answer, so after
            // the start was ready.
            let fatal = "FATAL:  lock file \"postmaster.pid\" already exists";
            let printed = async {
                let count = || pg.texts().iter().filter(|t| t.ends_with(fatal)).count();
                while count() < 2 {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            tokio::time::timeout(WAIT, printed).await.unwrap();
            tokio::time::sleep(Duration::from_millis(700)).await;
            let status = pg.daemon.postgres.status();
            assert_eq!((status.state, status.pid), (ServiceState::Running, pid));
            assert!(pg.daemon.proc_alive(POSTGRES_ID));
            assert!(pg.probes.asked.lock().unwrap().is_empty());
            let texts = pg.texts();
            let signals = ["fake got SIGINT", "fake got SIGTERM"];
            assert!(!texts.iter().any(|t| signals.contains(&t.as_str())));
            pg.act(ServiceAction::Stop).await.unwrap();
        }

        /// A stop does not wait for a first start: it drops it, `initdb` and
        /// all, and nothing runs after.
        #[tokio::test]
        async fn a_stop_drops_a_start_that_runs() {
            let pg = pg("serve", probes(Liveness::Gone, None));
            let platform = Platform::current().unwrap();
            let programs = programs(&pg.daemon.data_dir, platform, &Version::new(18, 7, 0));
            let calls = pg.dir.path().join("calls.log").display().to_string();
            let slow = format!("#!/bin/sh\necho initdb >> '{calls}'\nexec sleep 60\n");
            fs::write(programs.unwrap().initdb, slow).unwrap();

            let daemon = Arc::clone(&pg.daemon);
            let start = tokio::spawn(async move {
                let postgres = &daemon.postgres;
                postgres
                    .run_waiting(&daemon, ServiceAction::Start, WAIT)
                    .await
            });
            let init = async {
                while pg.count("initdb") == 0 {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            tokio::time::timeout(WAIT, init).await.unwrap();

            let begun = std::time::Instant::now();
            let status = pg.act(ServiceAction::Stop).await.unwrap();
            assert!(
                begun.elapsed() < Duration::from_secs(10),
                "{:?}",
                begun.elapsed()
            );
            assert_eq!(status.state, ServiceState::Stopped);
            // The start ended without an error and started nothing.
            let started = start.await.unwrap().unwrap();
            assert_eq!(started.state, ServiceState::Stopped);
            assert_eq!(pg.count("server"), 0);
            assert!(!pg.dirs.data.exists());
            assert!(!pg.daemon.proc_alive(POSTGRES_ID));
            // Nothing is left to drop, and the next start runs as usual.
            assert!(lock(&pg.daemon.postgres.starting).is_none());
        }

        #[tokio::test]
        async fn postgres_starts_with_the_daemon_only_when_it_is_on() {
            let pg = pg("serve", probes(Liveness::Gone, None));
            let launched = start_at_launch(&pg.daemon).wait(WAIT).await;
            let Some(Launched::Ran(status)) = launched else {
                panic!("{launched:?}");
            };
            assert_eq!(status.state, ServiceState::Running);
            pg.daemon.stop_procs(WAIT).await;

            let data = &pg.daemon.data_dir;
            set_postgres(data, Some("18"), false, pg.port);
            let launched = start_at_launch(&pg.daemon).wait(WAIT).await;
            assert_eq!(launched, Some(Launched::Off));
            // On, but its line is not installed: the status stays as it was.
            set_postgres(data, Some("17"), true, pg.port);
            let launched = start_at_launch(&pg.daemon).wait(WAIT).await;
            assert_eq!(launched, Some(Launched::NotInstalled));
            assert_eq!(pg.daemon.postgres.status().state, ServiceState::Stopped);
            assert_eq!(pg.count("server"), 1);

            // A failed start: the status says why, and the daemon runs on.
            set_postgres(data, Some("18"), true, pg.port);
            let _held = TcpListener::bind((Ipv4Addr::LOCALHOST, pg.port)).unwrap();
            let launched = start_at_launch(&pg.daemon).wait(WAIT).await;
            assert_eq!(launched, Some(Launched::Failed("service_port_busy")));
            let state = pg.daemon.postgres.status().state;
            assert_eq!(state, failed("service_port_busy"));
        }
    }

    /// A failed sync after the rename does not say that nothing was made.
    /// The first install makes its line the default and turns it on; a
    /// later one, of any line, changes nothing.
    #[test]
    fn only_the_first_install_turns_postgres_on() {
        let data = tempfile::tempdir().unwrap();
        let path = ConfigPaths::in_dir(data.path()).global;
        assert!(first_install(data.path(), 18).unwrap());
        let settings = config::load_global(&path).unwrap().postgres;
        assert_eq!(
            (settings.default.as_deref(), settings.enabled),
            (Some("18"), true)
        );

        set_enabled(data.path(), false).unwrap();
        assert!(!first_install(data.path(), 17).unwrap());
        let settings = config::load_global(&path).unwrap().postgres;
        assert_eq!(
            (settings.default.as_deref(), settings.enabled),
            (Some("18"), false)
        );
    }

    #[test]
    fn an_init_error_says_whether_the_cluster_is_in_place() {
        let failed = InitError::new("initdb failed".to_owned());
        let text = failed.to_string();
        assert_eq!(
            text,
            "the PostgreSQL data folder was not made: initdb failed"
        );
        let synced = InitError {
            reason: "its folder could not be synced to disk: I/O error".to_owned(),
            in_place: true,
        };
        let text = synced.to_string();
        assert!(
            text.contains("was made and is in place, but its folder"),
            "{text}"
        );
        assert!(text.ends_with("the next start uses it"), "{text}");
        assert!(!text.contains("not made"), "{text}");
        assert_eq!(synced.code(), failed.code());
    }

    /// Shell scripts stand in for `initdb` and `postgres`: macOS and Linux
    /// only, the systems of this task (Windows is task M6-07).
    #[cfg(unix)]
    mod init {
        use super::*;
        use std::fs;
        use std::os::unix::fs::PermissionsExt as _;

        use tempfile::TempDir;

        /// A data folder and the fakes. The fakes write what they were given
        /// to `calls.log` and `stdin.log` in `dir`.
        struct Fakes {
            dir: TempDir,
            programs: Programs,
            dirs: PostgresDirs,
        }

        /// `initdb` makes `-D <folder>` with a `PG_VERSION` and its own
        /// `pg_hba.conf`. `postgres` keeps the `pg_hba.conf` it found and
        /// its stdin, prints `print`, and exits with `exit`.
        fn fakes(print: &str, exit: u8) -> Fakes {
            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("runtimes/postgres/18.7.0/bin");
            fs::create_dir_all(&bin).unwrap();
            let at = |name: &str| dir.path().join(name).display().to_string();
            let initdb = format!(
                "#!/bin/sh\necho \"initdb $*\" >> '{calls}'\n[ \"$1\" = -D ] || exit 2\n\
                 mkdir \"$2\" || exit 3\necho 18 > \"$2/PG_VERSION\"\n\
                 echo 'host all all 127.0.0.1/32 trust' > \"$2/pg_hba.conf\"\n",
                calls = at("calls.log"),
            );
            let postgres = format!(
                "#!/bin/sh\necho \"postgres $*\" >> '{calls}'\n\
                 cp \"$3/pg_hba.conf\" '{hba}'\ncat > '{stdin}'\necho '{print}' >&2\nexit {exit}\n",
                calls = at("calls.log"),
                hba = at("hba-at-single.conf"),
                stdin = at("stdin.log"),
            );
            for (name, script) in [("initdb", initdb), ("postgres", postgres)] {
                let path = bin.join(name);
                fs::write(&path, script).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            let programs = Programs {
                postgres: bin.join("postgres"),
                initdb: bin.join("initdb"),
            };
            let dirs = PostgresDirs::new(&dir.path().join("app"), 18);
            Fakes {
                dir,
                programs,
                dirs,
            }
        }

        impl Fakes {
            fn read(&self, name: &str) -> String {
                fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
            }

            /// The names in `databases/postgres/`.
            fn listing(&self) -> Vec<String> {
                let parent = self.dirs.data.parent().unwrap();
                let mut names: Vec<_> = fs::read_dir(parent)
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                names
            }
        }

        #[tokio::test]
        async fn the_first_init_runs_initdb_then_makes_root_then_moves_the_folder() {
            let fakes = fakes("LOG:  database system is shut down", 0);
            assert_eq!(ensure_cluster(&fakes.programs, &fakes.dirs).await, Ok(true));

            let calls = fakes.read("calls.log");
            let calls: Vec<_> = calls.lines().collect();
            let [initdb, single] = calls[..] else {
                panic!("{calls:?}");
            };
            // The same temp folder for both steps, next to the target.
            let temp = initdb.split(' ').nth(2).unwrap();
            let parent = fakes.dirs.data.parent().unwrap();
            let prefix = parent.join(".18.init-").display().to_string();
            assert!(temp.starts_with(&prefix), "{temp}");
            let in_temp = PostgresDirs {
                data: temp.into(),
                sockets: fakes.dirs.sockets.clone(),
            };
            assert_eq!(
                initdb,
                format!("initdb {}", initdb_args(&in_temp).join(" "))
            );
            let want = format!("postgres {}", single_user_args(Path::new(temp)).join(" "));
            assert_eq!(single, want);
            // Our pg_hba.conf was in place before root was made.
            assert_eq!(fakes.read("hba-at-single.conf"), PG_HBA_CONF);
            assert_eq!(fakes.read("stdin.log"), ROOT_ROLE_SQL);

            // Moved into place whole, and nothing else is left.
            let hba = fs::read_to_string(fakes.dirs.data.join("pg_hba.conf")).unwrap();
            assert_eq!(hba, PG_HBA_CONF);
            assert!(fakes.dirs.data.join("PG_VERSION").is_file());
            assert_eq!(fakes.listing(), ["18"]);

            // The next start makes nothing.
            assert_eq!(
                ensure_cluster(&fakes.programs, &fakes.dirs).await,
                Ok(false)
            );
            assert_eq!(fakes.read("calls.log").lines().count(), 2);
        }

        #[tokio::test]
        async fn anything_at_the_data_folder_is_left_as_it_is() {
            let fakes = fakes("", 0);
            let parent = fakes.dirs.data.parent().unwrap();
            fs::create_dir_all(parent).unwrap();
            // An empty folder, a file, and a link: none is touched or run on.
            let data = &fakes.dirs.data;
            for kind in ["folder", "file", "link"] {
                match kind {
                    "folder" => fs::create_dir(data).unwrap(),
                    "file" => fs::write(data, "x").unwrap(),
                    _ => std::os::unix::fs::symlink("/nowhere", data).unwrap(),
                }
                let before = fs::symlink_metadata(data).unwrap();
                assert_eq!(
                    ensure_cluster(&fakes.programs, &fakes.dirs).await,
                    Ok(false)
                );
                let after = fs::symlink_metadata(data).unwrap();
                assert_eq!(before.file_type(), after.file_type(), "{kind}");
                assert_eq!(before.modified().unwrap(), after.modified().unwrap());
                if kind == "folder" {
                    fs::remove_dir(data).unwrap();
                } else {
                    fs::remove_file(data).unwrap();
                }
            }
            assert_eq!(fakes.read("calls.log"), "", "no program ran");
            assert_eq!(fakes.listing(), Vec::<String>::new());
        }

        #[tokio::test]
        async fn a_failed_step_leaves_no_cluster_and_no_temp_folder() {
            // An error that the session went on after, and an error exit.
            for (print, exit) in [
                ("ERROR:  role \"root\" already exists", 0),
                ("FATAL:  could not open file", 1),
            ] {
                let fakes = fakes(print, exit);
                let err = ensure_cluster(&fakes.programs, &fakes.dirs)
                    .await
                    .unwrap_err();
                assert_eq!(err.code(), "postgres_init_failed");
                assert!(err.reason.starts_with("postgres --single failed"), "{err}");
                assert!(err.reason.contains(print), "{err}");
                assert!(!fakes.dirs.data.exists());
                assert_eq!(fakes.listing(), Vec::<String>::new());
            }
        }

        #[tokio::test]
        async fn a_failed_initdb_runs_nothing_after_it() {
            let fakes = fakes("", 0);
            let script = "#!/bin/sh\necho 'initdb: error: cannot be run as root' >&2\nexit 1\n";
            fs::write(&fakes.programs.initdb, script).unwrap();
            let err = ensure_cluster(&fakes.programs, &fakes.dirs)
                .await
                .unwrap_err();
            assert!(err.reason.contains("initdb failed"), "{err}");
            assert!(err.reason.contains("cannot be run as root"), "{err}");
            assert_eq!(fakes.read("stdin.log"), "", "no single-user step");
            assert!(!fakes.dirs.data.exists());
        }

        #[tokio::test]
        async fn temp_folders_of_earlier_inits_of_this_major_are_deleted() {
            let fakes = fakes("", 0);
            let parent = fakes.dirs.data.parent().unwrap();
            for name in [".18.init-dead", ".17.init-dead"] {
                fs::create_dir_all(parent.join(name).join("base")).unwrap();
            }
            assert_eq!(ensure_cluster(&fakes.programs, &fakes.dirs).await, Ok(true));
            assert_eq!(fakes.listing(), [".17.init-dead", "18"]);
        }
    }
}
