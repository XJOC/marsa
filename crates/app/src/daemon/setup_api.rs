//! `POST /v1/setup`: the first setup on macOS. The user may be asked twice:
//! for an administrator password (the helper), then to approve the trust in
//! Marsa's certificate authority for this user. Task M4-13, with the port
//! check of task M4-14b and the user trust of task M4-26.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `POST /v1/setup` | `SetupReport`, also sent as `setup_finished` on `/v1/events` |
//!
//! The steps, on a blocking thread:
//!
//! 1. **CA:** Caddy's root certificate is checked ([`read_ca`]). Caddy makes
//!    it with the first secure site; before that the report says
//!    `ca_not_ready`. A file the helper or `security` would refuse is skipped
//!    as `ca_unreadable`, so the other steps still go. On macOS the trust is
//!    step 5, by the user; elsewhere the certificate goes into
//!    `trust_install` as text, for the helper (ADR 0011).
//! 2. **DNS:** the setup operation of [`dns::strategy_for`].
//! 3. **Ports** (M4-14b): `/usr/sbin/netstat -anv -p tcp` lists the sockets
//!    that listen, [`ports::holders`] picks those on 80 and 443, and `/bin/ps`
//!    names each process. A holder, or a check that fails, means no
//!    `pf_install`: the redirect would take that program's traffic without a
//!    word (ADR 0008, "Herd conflict"). Otherwise `pf_install` gets Caddy's
//!    ports from the daemon's `CaddyInput`.
//! 4. **Helper:** if any operation is left and the `marsa-helper` file can
//!    be changed only by its owner (this user or root), `/usr/bin/osascript`
//!    runs `marsa-helper apply <request>` with administrator privileges,
//!    through the constant script of ADR 0012. The helper's JSON answer gives
//!    one result per operation, in the order sent. No operation left: no
//!    prompt. A helper inside `~/Desktop`, `~/Documents`, or `~/Downloads`
//!    ends setup before the prompt with `setup_helper_protected_folder`:
//!    macOS keeps root out of those folders, so the boot job's copy of the
//!    helper fails there (task M4-17).
//! 5. **User trust (macOS):** after the helper, and only if it ran as asked
//!    or was not needed, `marsad` itself trusts the CA for the current user
//!    with `security add-trusted-cert` against the login keychain, behind the
//!    dialog macOS shows (ADR 0015, [`user_trust`]). Its result goes into the
//!    report as `trust_install`, after the helper's results.
//!
//! One setup at a time: a second request gets 409 `setup_running`, and one
//! during an uninstall gets 409 `uninstall_running`. Linux and Windows get
//! 501 `setup_not_supported` until tasks M4-18 and M4-19.
//!
//! `POST /v1/uninstall` (task M5-13, `uninstall_api.rs`) shares [`Setup`]:
//! the same runner, the same helper file check and admin prompt ([`Flow`]
//! gives each its own error codes), and the same one-at-a-time flag. The
//! protected-folder check of step 4 is setup's only ([`Setup::launch`]).
//!
//! Every program runs through [`Runner`], so the tests run none: they pass a
//! fake that records each call and answers from fixtures.

pub mod user_trust;

use std::collections::BTreeMap;
use std::env::consts::EXE_SUFFIX;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use marsa_core::caddy::{CaddyInput, CaddyPaths};
use marsa_core::dns;
use marsa_core::helper::{
    HELPER_PROTOCOL_VERSION, HelperOp, HelperRequest, HelperResponse, OpResult,
};
use marsa_core::ports::{self, Listener, ProcessInfo};
use marsa_core::recipes::Platform;
use marsa_core::types::{ApiError, Event, PortHolder, SetupReport, SetupSkip};
use tokio_util::sync::CancellationToken;

use super::api::HttpError;
use super::state::Daemon;

const NETSTAT: &str = "/usr/sbin/netstat";
const PS: &str = "/bin/ps";
const OSASCRIPT: &str = "/usr/bin/osascript";

/// The AppleScript of ADR 0012, one `-e` argument per line. A constant:
/// nothing is ever put into it. The helper path and the request travel as
/// `argv` items 1 and 2, and `quoted form of` hands each to `/bin/sh` as one
/// literal argument. This is the only shell line Marsa runs (AGENTS.md
/// section 6).
const APPLESCRIPT: [&str; 3] = [
    "on run argv",
    "do shell script (quoted form of item 1 of argv) & \" apply \" & (quoted form of item 2 of argv) with administrator privileges",
    "end run",
];

/// The ports the `pf` redirect takes (ADR 0008).
const REDIRECTED: [u16; 2] = [80, 443];
/// How long `netstat` and each `ps` may take.
pub(super) const CHECK_LIMIT: Duration = Duration::from_secs(10);
/// How long `osascript` may take: the user types the password meanwhile.
/// The trust dialog of `security add-trusted-cert` gets the same.
pub(super) const PROMPT_LIMIT: Duration = Duration::from_secs(5 * 60);
/// At most this many `ps` calls; further holders keep netstat's name only.
const MAX_PS: usize = 8;
/// The helper refuses a larger certificate (ADR 0011).
const MAX_CA_BYTES: u64 = 16 * 1024;
/// The most read from each pipe of a program. The rest is read and dropped,
/// so the program never waits on a full pipe.
const MAX_OUTPUT: u64 = 4 * 1024 * 1024;
/// The largest helper answer read. A real one is well under 4 KiB.
const MAX_ANSWER: usize = 64 * 1024;
/// The only `PATH` a program sees. Every program here is an absolute path, so
/// this only keeps the daemon's own `PATH` away from them.
const SAFE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
/// How often [`RealRunner`] checks whether the program has finished.
const POLL: Duration = Duration::from_millis(10);
/// How long the pipes may stay open after the program ended. Its output is
/// read all along, so only the last bytes are left by then.
pub(super) const PIPE_GRACE: Duration = Duration::from_secs(2);
/// The longest a setup takes, when every program runs to its limit:
/// `netstat`, [`MAX_PS`] `ps` calls, and `security verify-cert` at
/// [`CHECK_LIMIT`] each, `osascript` and `security add-trusted-cert` at
/// [`PROMPT_LIMIT`] each, and [`PIPE_GRACE`] after each of these programs.
/// A client has to wait longer: `marsa_core::client::SETUP_TIMEOUT` does,
/// and a test holds the two together.
pub const SLOWEST_SETUP: Duration = Duration::from_secs(
    (2 + MAX_PS as u64) * CHECK_LIMIT.as_secs()
        + 2 * PROMPT_LIMIT.as_secs()
        + (4 + MAX_PS as u64) * PIPE_GRACE.as_secs(),
);

/// The first line the helper's `trust_install` accepts (`ops/trust.rs`).
const PEM_BEGIN: &str = "-----BEGIN CERTIFICATE-----";

// `#[cfg]` outside `platform`: std has no `O_NONBLOCK`, its value differs by
// system, and `libc` is not an approved dependency (AGENTS.md section 8).
// These are the values of `<sys/fcntl.h>` on macOS and of
// `<asm-generic/fcntl.h>` on Linux x86-64 and arm64. Another Unix opens
// without it; Marsa has no build there.
#[cfg(target_os = "macos")]
const O_NONBLOCK: i32 = 0x0004;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
const O_NONBLOCK: i32 = 0o4000;
#[cfg(all(
    unix,
    not(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    ))
))]
const O_NONBLOCK: i32 = 0;

const TRUST_INSTALL: &str = "trust_install";
const PF_INSTALL: &str = "pf_install";
const UNINSTALL_ALL: &str = "uninstall_all";

/// The folders under the home folder that macOS keeps other users, root
/// included, out of until the user allows it (the folder protection of
/// Privacy & Security). Root could not read a helper there to copy it for
/// the boot job: `pf_write` in task M4-17, `pf_helper_unreadable` since
/// task M4-27.
const PROTECTED_FOLDERS: [&str; 3] = ["Desktop", "Documents", "Downloads"];
/// Where an install puts the programs, for the message that says to move
/// Marsa there.
const INSTALL_BIN: &str = "~/Library/Application Support/dev.marsa.Marsa/bin";

/// The routes on `setup`, for `api::router` to merge before its fallbacks.
/// The uninstall route gets the same [`Setup`], so the two never run at once.
pub fn routes(setup: Arc<Setup>) -> Router<Arc<Daemon>> {
    let handler = move |State(daemon): State<Arc<Daemon>>| {
        let setup = Arc::clone(&setup);
        async move { start(daemon, setup).await }
    };
    Router::new().route("/v1/setup", post(handler))
}

/// The routes with the given [`Setup`]: tests pass a fake [`Runner`].
pub fn routes_with(setup: Setup) -> Router<Arc<Daemon>> {
    routes(Arc::new(setup))
}

/// Which request runs the helper: the setup, or the uninstall of task M5-13.
/// Each has its own error codes, `setup_*` and `uninstall_*`, and its
/// messages name its own command. It also names the request that runs now,
/// for the 409 of the other one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Setup,
    Uninstall,
}

impl Flow {
    /// `setup_<what>` or `uninstall_<what>`.
    pub fn code(self, what: &str) -> String {
        let prefix = match self {
            Self::Setup => "setup",
            Self::Uninstall => "uninstall",
        };
        format!("{prefix}_{what}")
    }

    /// The request in an English message: "setup", "the uninstall".
    fn noun(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Uninstall => "the uninstall",
        }
    }

    /// Its value in [`Setup`]'s flag; `IDLE` is none.
    fn flag(self) -> u8 {
        match self {
            Self::Setup => 1,
            Self::Uninstall => 2,
        }
    }

    /// The 409 for a request while this one runs.
    pub fn busy(self) -> HttpError {
        let message = match self {
            Self::Setup => "another setup is running",
            Self::Uninstall => "an uninstall is running",
        };
        // `HttpError` wants a `&'static str` code.
        let code = match self {
            Self::Setup => "setup_running",
            Self::Uninstall => "uninstall_running",
        };
        HttpError::new(StatusCode::CONFLICT, code, message)
    }
}

/// [`Setup`]'s flag when neither request runs.
const IDLE: u8 = 0;

/// What a program printed, and how it ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOutput {
    /// The exit code, or `None` if a signal stopped the program.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl RunOutput {
    fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Runs the programs of setup. The daemon uses [`RealRunner`]; tests use a
/// fake, like `Process::fake()` in Laravel.
pub trait Runner: Send + Sync {
    /// Runs `program` with `args`, never through a shell, and waits at most
    /// `limit`. `&'static str`: a constant path in this file, never one from
    /// a request.
    ///
    /// # Errors
    ///
    /// The error of the start; kind `TimedOut` after `limit`, and kind
    /// `Interrupted` once `stop` is cancelled. In both cases the program is
    /// killed first.
    fn run(
        &self,
        program: &'static str,
        args: &[OsString],
        limit: Duration,
        stop: &CancellationToken,
    ) -> io::Result<RunOutput>;
}

/// `std::process::Command`, with an empty environment except [`SAFE_PATH`],
/// no stdin, and the working folder `/`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealRunner;

impl Runner for RealRunner {
    fn run(
        &self,
        program: &'static str,
        args: &[OsString],
        limit: Duration,
        stop: &CancellationToken,
    ) -> io::Result<RunOutput> {
        if !Path::new(program).is_absolute() {
            let reason = "the program is not an absolute path";
            return Err(io::Error::new(io::ErrorKind::InvalidInput, reason));
        }
        // Checked before the start too, so a stopping daemon never opens the
        // admin prompt, not even for a moment.
        if stop.is_cancelled() {
            let reason = "the daemon is stopping";
            return Err(io::Error::new(io::ErrorKind::Interrupted, reason));
        }
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .env("PATH", SAFE_PATH)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Not the daemon's working folder. `/` is a Unix path, and setup runs
        // on macOS only for now (Linux in task M4-18, Windows in M4-19).
        #[cfg(unix)]
        command.current_dir("/");
        let mut child = command.spawn()?;
        finish(&mut child, limit, stop)
    }
}

/// Waits for `child` at most `limit`, or until `stop`, and collects its
/// output. On any error the child is killed and reaped before this returns.
fn finish(child: &mut Child, limit: Duration, stop: &CancellationToken) -> io::Result<RunOutput> {
    let result = collect(child, Instant::now() + limit, stop);
    if result.is_err() {
        // Best effort: the first error is the one to report.
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

/// Waits for `child` until `deadline` or `stop`, and collects its output.
fn collect(
    child: &mut Child,
    deadline: Instant,
    stop: &CancellationToken,
) -> io::Result<RunOutput> {
    let stdout = read_in_background(child.stdout.take())?;
    let stderr = read_in_background(child.stderr.take())?;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if stop.is_cancelled() {
            let reason = "the daemon is stopping";
            return Err(io::Error::new(io::ErrorKind::Interrupted, reason));
        }
        if Instant::now() >= deadline {
            return Err(timed_out());
        }
        thread::sleep(POLL);
    };
    // From the end of the program, not from `deadline`: a program that ends
    // just before its limit still gives its output. A program it started may
    // hold a pipe open, so the wait is short.
    let pipes = Instant::now() + PIPE_GRACE;
    Ok(RunOutput {
        code: status.code(),
        stdout: receive(&stdout, pipes)?,
        stderr: receive(&stderr, pipes)?,
    })
}

/// Reads a pipe on its own thread, so a program that fills one pipe never
/// blocks while the other is read. Keeps the first [`MAX_OUTPUT`] bytes.
fn read_in_background<R>(pipe: Option<R>) -> io::Result<Receiver<io::Result<Vec<u8>>>>
where
    R: Read + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("setup-pipe".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            let result = match pipe {
                Some(mut pipe) => {
                    let kept = (&mut pipe).take(MAX_OUTPUT).read_to_end(&mut bytes);
                    kept.and_then(|_| io::copy(&mut pipe, &mut io::sink()))
                        .map(|_| bytes)
                }
                None => Ok(bytes),
            };
            // After a timeout nobody waits for this, so a failed send is fine.
            let _ = sender.send(result);
        })?;
    Ok(receiver)
}

/// The output of one pipe, read after the program ended. Not `TimedOut` if
/// the pipe stays open past `deadline`: the program itself finished in time.
fn receive(output: &Receiver<io::Result<Vec<u8>>>, deadline: Instant) -> io::Result<Vec<u8>> {
    let left = deadline.saturating_duration_since(Instant::now());
    match output.recv_timeout(left) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(io::Error::other(
            "the program ended, but its output stayed open",
        )),
        Err(RecvTimeoutError::Disconnected) => Err(io::Error::other("the output reader stopped")),
    }
}

fn timed_out() -> io::Error {
    let reason = "the program did not finish in time";
    io::Error::new(io::ErrorKind::TimedOut, reason)
}

/// The setup side of the daemon: how programs run, which system this is,
/// where the helper is, and whether a setup runs now.
pub struct Setup {
    runner: Arc<dyn Runner>,
    /// `None`: a system Marsa has no build for.
    platform: Option<Platform>,
    /// `marsa-helper`. `None` if the daemon's own path is not known.
    helper: Option<PathBuf>,
    /// The home folder of the user `marsad` runs as: the login keychain and
    /// the protected folders are under it. `None` if the system gives none.
    home: Option<PathBuf>,
    /// [`IDLE`], or the [`Flow::flag`] of the request that runs now.
    running: AtomicU8,
}

impl Setup {
    /// The daemon's: [`RealRunner`], this system, `marsa-helper` next to the
    /// running program, and this user's home folder. The helper is in
    /// `<app data>/bin/` in a real install and `target/<profile>/` in
    /// development (ADR 0012).
    pub fn new() -> Self {
        Self::with(
            Arc::new(RealRunner),
            Platform::current(),
            helper_path(),
            home_dir(),
        )
    }

    /// For tests: any runner, system, helper path, and home folder.
    pub fn with(
        runner: Arc<dyn Runner>,
        platform: Option<Platform>,
        helper: Option<PathBuf>,
        home: Option<PathBuf>,
    ) -> Self {
        Self {
            runner,
            platform,
            helper,
            home,
            running: AtomicU8::new(IDLE),
        }
    }

    /// The program runner, for the uninstall's own steps.
    pub(super) fn runner(&self) -> &dyn Runner {
        self.runner.as_ref()
    }

    /// This system, if Marsa has a build for it.
    pub(super) fn platform(&self) -> Option<Platform> {
        self.platform
    }

    /// The home folder of the user `marsad` runs as, if known.
    pub(super) fn home(&self) -> Option<&Path> {
        self.home.as_deref()
    }

    /// The setup itself. Blocking: it runs programs and reads a file.
    fn run(
        &self,
        data_dir: &Path,
        caddy: CaddyInput,
        platform: Platform,
        stop: &CancellationToken,
    ) -> SetupReport {
        let mut report = SetupReport::default();
        let mut ops = Vec::new();
        let ca_root = CaddyPaths::in_dir(data_dir).ca_root();
        // macOS: the user's own trust, after the helper (ADR 0015). Elsewhere
        // the helper trusts the CA as root (ADR 0011).
        let mut trust_for_user = None;
        match read_ca(&ca_root) {
            Ok(pem) if is_macos(platform) => trust_for_user = Some(pem),
            Ok(pem) => ops.push(HelperOp::TrustInstall { pem }),
            Err(code) => report.skipped.push(skip(TRUST_INSTALL, code)),
        }
        if let Some(dns) = dns::strategy_for(platform) {
            ops.push(dns.setup_op());
        }
        match self.port_holders(stop) {
            Some(holders) if holders.is_empty() => ops.push(HelperOp::PfInstall {
                http_port: caddy.http_port,
                https_port: caddy.https_port,
            }),
            Some(holders) => {
                for holder in &holders {
                    tracing::warn!(
                        port = holder.port,
                        process = ?holder.process,
                        pid = ?holder.pid,
                        app = ?holder.app,
                        "a program listens on a port the pf redirect would take; no pf_install"
                    );
                }
                report.port_holders = holders;
                report.skipped.push(skip(PF_INSTALL, "redirect_port_held"));
            }
            None => report.skipped.push(skip(PF_INSTALL, "ports_not_checked")),
        }
        match self.apply(ops, platform, stop) {
            Ok(results) => report.results = results,
            Err(error) => report.error = Some(error),
        }
        // Only if the helper ran as asked, or was not needed: a cancelled or
        // refused admin prompt ends setup with nothing changed, and no second
        // dialog.
        if let Some(pem) = trust_for_user.filter(|_| report.error.is_none()) {
            let record = user_trust::record_path(data_dir);
            let trust = self.trust_for_user(&ca_root, &pem, &record, stop);
            report.results.push(trust);
        }
        report
    }

    /// The user's trust of the CA root at `ca_root`, whose checked text is
    /// `pem` (ADR 0015), kept at `record` once it is trusted; or
    /// `trust_add_failed` when the home folder, and so the login keychain,
    /// is not known.
    fn trust_for_user(
        &self,
        ca_root: &Path,
        pem: &str,
        record: &Path,
        stop: &CancellationToken,
    ) -> OpResult {
        match &self.home {
            Some(home) => {
                user_trust::install(self.runner.as_ref(), home, ca_root, pem, record, stop)
            }
            None => {
                tracing::warn!(
                    "the home folder is not known, so the login keychain cannot be found"
                );
                let message = "the home folder of the current user is not known, so the \
                               login keychain cannot be found";
                user_trust::failed(TRUST_INSTALL, user_trust::TRUST_ADD_FAILED, message)
            }
        }
    }

    /// Who listens on 80 and 443, or `None` if that cannot be known:
    /// `netstat` failed, printed more than [`MAX_OUTPUT`] (the rest, with a
    /// holder in it maybe, was not read), or listed no listening socket at
    /// all. The daemon's own API socket always listens, so an empty list
    /// means the output was not understood. The redirect never goes in blind.
    fn port_holders(&self, stop: &CancellationToken) -> Option<Vec<PortHolder>> {
        let args = ["-anv", "-p", "tcp"].map(OsString::from);
        let output = match self.runner.run(NETSTAT, &args, CHECK_LIMIT, stop) {
            Ok(output) if output.success() => output,
            Ok(output) => {
                tracing::warn!(exit = ?output.code, "netstat failed; ports 80 and 443 not checked");
                return None;
            }
            Err(err) => {
                tracing::warn!(error = %err, "cannot run netstat; ports 80 and 443 not checked");
                return None;
            }
        };
        // `>=`: an output of exactly the limit may have been cut too.
        if output.stdout.len() as u64 >= MAX_OUTPUT {
            tracing::warn!(
                bytes = output.stdout.len(),
                "netstat printed more than Marsa reads; ports 80 and 443 not checked"
            );
            return None;
        }
        let listeners = ports::parse_netstat(&String::from_utf8_lossy(&output.stdout));
        if listeners.is_empty() {
            tracing::warn!("netstat listed no listening socket; its output was not understood");
            return None;
        }
        let mut processes = BTreeMap::new();
        let holders = REDIRECTED
            .iter()
            .flat_map(|&port| ports::holders(&listeners, port))
            .map(|listener| self.holder(listener, &mut processes, stop))
            .collect();
        Some(holders)
    }

    /// One holder, named by `ps` (once per PID, at most [`MAX_PS`] PIDs) and
    /// by netstat. [`ports::known_app`] tries the `ps` command first, then
    /// the netstat name: an nginx worker shows only its title in `ps`.
    fn holder(
        &self,
        listener: &Listener,
        processes: &mut BTreeMap<u32, Option<ProcessInfo>>,
        stop: &CancellationToken,
    ) -> PortHolder {
        let info = listener.pid.and_then(|pid| {
            if !processes.contains_key(&pid) && processes.len() >= MAX_PS {
                return None;
            }
            let info = processes
                .entry(pid)
                .or_insert_with(|| self.process(pid, stop));
            info.clone()
        });
        let app = info
            .as_ref()
            .and_then(|info| ports::known_app(&info.command))
            .or_else(|| listener.process.as_deref().and_then(ports::known_app));
        PortHolder {
            port: listener.port,
            address: listener.address.clone(),
            process: listener.process.clone(),
            pid: listener.pid,
            user: info.as_ref().map(|info| info.user.clone()),
            command: info.map(|info| info.command),
            app: app.map(str::to_owned),
        }
    }

    /// The user and program of `pid`, or `None` if `ps` cannot tell: the
    /// process may have ended since netstat listed it.
    fn process(&self, pid: u32, stop: &CancellationToken) -> Option<ProcessInfo> {
        let pid_text = pid.to_string();
        let args = ["-p", pid_text.as_str(), "-o", "user=,comm="].map(OsString::from);
        match self.runner.run(PS, &args, CHECK_LIMIT, stop) {
            Ok(output) if output.success() => {
                ports::parse_ps(&String::from_utf8_lossy(&output.stdout))
            }
            Ok(_) => None,
            Err(err) => {
                tracing::debug!(pid, error = %err, "cannot run ps");
                None
            }
        }
    }

    /// [`Setup::launch`] for the setup.
    fn apply(
        &self,
        ops: Vec<HelperOp>,
        platform: Platform,
        stop: &CancellationToken,
    ) -> Result<Vec<OpResult>, ApiError> {
        self.launch(Flow::Setup, ops, platform, stop)
    }

    /// Sends `ops` to the helper behind the admin prompt and reads its
    /// answer, with the error codes of `flow`. With no operation nothing
    /// runs and no prompt shows. The helper is checked first: a file that is
    /// missing or that another user could change means no prompt, and so
    /// does, for setup only, a path in a folder macOS keeps root out of (on
    /// macOS; by the path alone, before any look at the file).
    ///
    /// The uninstall skips the folder check: root runs a helper from those
    /// folders through `osascript` (task M4-17: `resolver_install` worked
    /// from `~/Desktop`), and only the boot job's copy of the binary, which
    /// `pf_install` reads, fails there. `uninstall_all` reads no binary, so
    /// moving Marsa first would only be in the user's way.
    pub(super) fn launch(
        &self,
        flow: Flow,
        ops: Vec<HelperOp>,
        platform: Platform,
        stop: &CancellationToken,
    ) -> Result<Vec<OpResult>, ApiError> {
        if ops.is_empty() {
            return Ok(Vec::new());
        }
        let missing = || {
            tracing::warn!(path = ?self.helper, "marsa-helper is not next to marsad");
            failure(
                &flow.code("helper_missing"),
                "marsa-helper is not next to marsad",
            )
        };
        let helper = self.helper.as_deref().ok_or_else(missing)?;
        // First: it reads nothing, and the advice to move Marsa comes before
        // any word about the file itself.
        let protected = (flow == Flow::Setup && is_macos(platform))
            .then(|| protected_folder(helper, self.home.as_deref()))
            .flatten();
        if let Some(folder) = protected {
            tracing::warn!(
                path = %helper.display(),
                folder = %folder.display(),
                "marsa-helper is in a folder macOS keeps root out of; no admin prompt"
            );
            let message = format!(
                "marsa-helper is inside {}, a folder macOS protects, so root could not read \
                 it to set up the boot job; run \"marsa daemon stop\", move Marsa to \
                 {INSTALL_BIN}, and run \"marsa setup\" from there",
                folder.display()
            );
            return Err(failure("setup_helper_protected_folder", message));
        }
        match check_helper(helper) {
            Ok(()) => {}
            Err(HelperFault::Missing) => return Err(missing()),
            Err(HelperFault::Unsafe(reason)) => {
                tracing::warn!(
                    path = %helper.display(),
                    reason,
                    "marsa-helper could be changed by another user; no admin prompt"
                );
                let message = format!("marsa-helper is not safe to run as root: {reason}");
                return Err(failure(&flow.code("helper_unsafe"), message));
            }
        }
        let cannot_write = |err: serde_json::Error| {
            tracing::error!(error = %err, "cannot write the helper request");
            failure(
                &flow.code("helper_failed"),
                "cannot write the helper request",
            )
        };
        let names = ops
            .iter()
            .map(op_name)
            .collect::<Result<Vec<_>, _>>()
            .map_err(cannot_write)?;
        let request = serde_json::to_string(&HelperRequest::new(ops)).map_err(cannot_write)?;
        tracing::info!(helper = %helper.display(), "asking for admin rights to run marsa-helper");
        let args = osascript_args(helper, &request);
        let answer = self.runner.run(OSASCRIPT, &args, PROMPT_LIMIT, stop);
        let results = read_answer_for(flow, answer)?;
        if !results_match(&names, &results) {
            tracing::warn!(
                sent = ?names,
                got = ?results.iter().map(|result| short_code(&result.op)).collect::<Vec<_>>(),
                "marsa-helper answered for other operations than it was sent"
            );
            let message = "the helper's results do not match the request";
            return Err(failure(&flow.code("helper_failed"), message));
        }
        Ok(results)
    }
}

impl Default for Setup {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Setup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Setup")
            .field("platform", &self.platform)
            .field("helper", &self.helper)
            .field("running", &self.running.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// The right to run the one setup or uninstall. Dropping it lets the next
/// one start, also if the request panics.
pub(super) struct Running(Arc<Setup>);

impl Running {
    /// The right to run `flow`, or the [`Flow`] that runs now. One atomic
    /// swap, no lock.
    pub(super) fn take(setup: &Arc<Setup>, flow: Flow) -> Result<Self, Flow> {
        setup
            .running
            .compare_exchange(IDLE, flow.flag(), Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self(Arc::clone(setup)))
            .map_err(|now| {
                if now == Flow::Uninstall.flag() {
                    Flow::Uninstall
                } else {
                    Flow::Setup
                }
            })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.0.running.store(IDLE, Ordering::Release);
    }
}

async fn start(daemon: Arc<Daemon>, setup: Arc<Setup>) -> Result<Json<SetupReport>, HttpError> {
    let platform = setup
        .platform
        .filter(|platform| is_macos(*platform))
        .ok_or_else(|| {
            let message = "setup runs on macOS only for now";
            HttpError::new(StatusCode::NOT_IMPLEMENTED, "setup_not_supported", message)
        })?;
    let running = Running::take(&setup, Flow::Setup).map_err(Flow::busy)?;
    tracing::info!("setup started");
    let job = tokio::task::spawn_blocking(move || {
        // Moved in, so the next setup waits for this one to end even if the
        // client goes away.
        let _running = running;
        let caddy = daemon.caddy.input;
        let report = setup.run(&daemon.data_dir, caddy, platform, &daemon.shutdown);
        tracing::info!(
            results = report.results.len(),
            skipped = report.skipped.len(),
            error = report.error.as_ref().map(|error| error.code.as_str()),
            "setup finished"
        );
        // Here, not in the handler: the event goes out even if the client
        // went away.
        daemon.publish(Event::SetupFinished(report.clone()));
        report
    });
    job.await.map(Json).map_err(|err| {
        tracing::error!(error = %err, "the setup job failed");
        let message = "the setup job failed";
        HttpError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
    })
}

/// `marsa-helper` next to the running program.
fn helper_path() -> Option<PathBuf> {
    match std::env::current_exe() {
        Ok(exe) => exe
            .parent()
            .map(|dir| dir.join(format!("marsa-helper{EXE_SUFFIX}"))),
        Err(err) => {
            tracing::warn!(error = %err, "cannot find the running program, so not marsa-helper");
            None
        }
    }
}

/// The home folder of the user `marsad` runs as: `$HOME`, else the user
/// database, which is what `directories` reads for the data folder too.
/// Never from a request.
fn home_dir() -> Option<PathBuf> {
    let home = std::env::home_dir().filter(|home| home.is_absolute());
    if home.is_none() {
        tracing::warn!("the home folder of the current user is not known");
    }
    home
}

pub(super) fn is_macos(platform: Platform) -> bool {
    matches!(platform, Platform::DarwinArm64 | Platform::DarwinX64)
}

/// The folder of [`PROTECTED_FOLDERS`] under `home` that holds `helper`, if
/// any, as the helper's path writes it. By component, so `~/Desktop2` is not
/// `~/Desktop`. The folder's name is compared without ASCII case: the usual
/// macOS disk takes `~/desktop` for `~/Desktop`. No `canonicalize` and no
/// I/O: the path is compared as root would open it.
fn protected_folder(helper: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let home = home?;
    let first = helper.strip_prefix(home).ok()?.components().next()?;
    let name = first.as_os_str().to_str()?;
    PROTECTED_FOLDERS
        .iter()
        .any(|protected| name.eq_ignore_ascii_case(protected))
        .then(|| home.join(name))
}

/// The text of Caddy's root certificate at `path`, for `trust_install`.
///
/// # Errors
///
/// `ca_not_ready` if there is no file yet, and `ca_unreadable` if it is not
/// a regular file, is larger than 16 KiB, cannot be read, or would fail the
/// helper's own checks ([`pem_problem`]).
fn read_ca(path: &Path) -> Result<String, &'static str> {
    let unusable = |reason: &dyn fmt::Display| {
        tracing::warn!(path = %path.display(), reason = %reason, "cannot use the CA root certificate");
        "ca_unreadable"
    };
    let file = match open_nonblocking(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Err("ca_not_ready"),
        Err(err) => return Err(unusable(&err)),
    };
    // The metadata of the open file, not of the path: a file swapped after
    // a check by path would slip through.
    let meta = file.metadata().map_err(|err| unusable(&err))?;
    if !meta.is_file() || meta.len() > MAX_CA_BYTES {
        return Err(unusable(&"not a regular file of at most 16 KiB"));
    }
    let mut pem = String::new();
    file.take(MAX_CA_BYTES + 1)
        .read_to_string(&mut pem)
        .map_err(|err| unusable(&err))?;
    if pem.len() as u64 > MAX_CA_BYTES {
        return Err(unusable(&"larger than 16 KiB"));
    }
    match pem_problem(&pem) {
        Some(problem) => Err(unusable(&problem)),
        None => Ok(pem),
    }
}

/// Opens `path` to read. On Unix with `O_NONBLOCK`: a FIFO put in place of
/// the file then opens at once, and fails the regular file check, instead of
/// waiting for a writer and holding up setup. A regular file reads as usual.
fn open_nonblocking(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, O_NONBLOCK);
    options.open(path)
}

/// Why the helper would refuse `pem` in `trust_install` (`request.rs` and
/// `ops/trust.rs` of `marsa-helper`), or `None`. Checked here too, so the
/// password is never asked for a request the helper rejects as a whole.
fn pem_problem(pem: &str) -> Option<&'static str> {
    let start = pem.trim_start_matches([' ', '\t', '\r', '\n']);
    if pem.is_empty() {
        Some("is empty")
    } else if !pem.is_ascii() {
        Some("is not ASCII")
    } else if pem.contains('\0') {
        Some("contains a NUL byte")
    } else if !start.starts_with(PEM_BEGIN) {
        Some("does not start with -----BEGIN CERTIFICATE-----")
    } else {
        None
    }
}

/// Why `marsa-helper` must not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelperFault {
    /// No file at the path, or the path is not absolute.
    Missing,
    /// Another user could change the file or put another in its place.
    Unsafe(&'static str),
}

/// Checks the helper file before it runs as root. It must be a regular file
/// (not a symlink), and on Unix the file and its folder must belong to this
/// user or root and be writable by nobody else. Another user could otherwise
/// replace it before the user approves the prompt.
///
/// What this cannot stop: a program of this same user replacing the file
/// between this check and the prompt. Only a copy owned by root would.
fn check_helper(helper: &Path) -> Result<(), HelperFault> {
    if !helper.is_absolute() {
        return Err(HelperFault::Missing);
    }
    // `symlink_metadata`: a symlink is refused, not followed.
    let meta = match fs::symlink_metadata(helper) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(HelperFault::Missing),
        Err(_) => return Err(HelperFault::Unsafe("the file cannot be checked")),
    };
    if !meta.file_type().is_file() {
        return Err(HelperFault::Unsafe("the file is not a regular file"));
    }
    check_owners(helper, &meta)
}

/// The file and its folder: owned by this user or root, and writable by
/// nobody else. The mode check is the one of `platform::daemon_file`.
// `#[cfg]` outside `platform`: Unix owners and modes, read with
// `MetadataExt`. The Windows twin is below.
#[cfg(unix)]
fn check_owners(helper: &Path, file: &fs::Metadata) -> Result<(), HelperFault> {
    use std::os::unix::fs::MetadataExt;

    let folder = helper
        .parent()
        .and_then(|folder| fs::metadata(folder).ok())
        .ok_or(HelperFault::Unsafe("the folder cannot be checked"))?;
    let me = current_uid().map_err(|_| HelperFault::Unsafe("the current user is not known"))?;
    let checks = [
        (
            file,
            "the file is writable by group or others",
            "the file belongs to another user",
        ),
        (
            &folder,
            "the folder is writable by group or others",
            "the folder belongs to another user",
        ),
    ];
    for (meta, writable, foreign) in checks {
        // `0o022`: write for the group or for others.
        if meta.mode() & 0o022 != 0 {
            return Err(HelperFault::Unsafe(writable));
        }
        if meta.uid() != me && meta.uid() != 0 {
            return Err(HelperFault::Unsafe(foreign));
        }
    }
    Ok(())
}

/// `#[cfg]` outside `platform`: this check reads Unix owners and modes.
/// Windows answers 501 before setup runs; its helper service is protected in
/// task M4-19.
#[cfg(not(unix))]
fn check_owners(_helper: &Path, _file: &fs::Metadata) -> Result<(), HelperFault> {
    Ok(())
}

/// The effective user of this process. Std has no `geteuid` and `libc` is
/// not an approved dependency (AGENTS.md section 8), so this reads the owner
/// of a new pipe: macOS and Linux give a pipe the effective user of the
/// process that asks. A test compares it with a new file's owner.
// `#[cfg]` outside `platform`: only `check_owners` on Unix needs it.
#[cfg(unix)]
fn current_uid() -> io::Result<u32> {
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::MetadataExt;

    let (reader, _writer) = io::pipe()?;
    Ok(File::from(OwnedFd::from(reader)).metadata()?.uid())
}

/// The name of `op` in the helper's results: its `op` tag in JSON.
fn op_name(op: &HelperOp) -> Result<String, serde_json::Error> {
    let value = serde_json::to_value(op)?;
    match value.get("op").and_then(serde_json::Value::as_str) {
        Some(name) => Ok(name.to_owned()),
        None => Err(serde::de::Error::custom("an operation without its name")),
    }
}

/// Whether the helper answered for exactly the operations sent, in order.
/// `uninstall_all` gives one result per removal it runs, so a request with
/// it is not checked; setup never sends it, and the uninstall sends it alone.
fn results_match(names: &[String], results: &[OpResult]) -> bool {
    names.iter().any(|name| name == UNINSTALL_ALL)
        || names
            .iter()
            .map(String::as_str)
            .eq(results.iter().map(|result| result.op.as_str()))
}

/// The `osascript` arguments of ADR 0012: the constant script, then the
/// helper path and the request as `argv` items 1 and 2.
fn osascript_args(helper: &Path, request: &str) -> Vec<OsString> {
    let mut args = Vec::with_capacity(APPLESCRIPT.len() * 2 + 2);
    for line in APPLESCRIPT {
        args.push(OsString::from("-e"));
        args.push(OsString::from(line));
    }
    args.push(helper.as_os_str().to_owned());
    args.push(OsString::from(request));
    args
}

/// [`read_answer_for`] for setup, as the tests call it.
#[cfg(test)]
fn read_answer(output: io::Result<RunOutput>) -> Result<Vec<OpResult>, ApiError> {
    read_answer_for(Flow::Setup, output)
}

/// The helper's results from what `osascript` gave back, with the error
/// codes of `flow`.
///
/// - Exit 0: stdout is the helper's JSON line; `do shell script` returns it.
/// - Another exit: the helper exited with 1 (an operation failed) or 2 (it
///   refused the request), and `do shell script` turned that into an
///   AppleScript error whose text is the helper's stdout. `osascript` prints
///   it on stderr as `<at>: execution error: <JSON> (<exit>)`, so the JSON
///   is read from there.
/// - A cancelled prompt is AppleScript error -128:
///   `execution error: User canceled. (-128)`.
/// - A wrong password, or a prompt that cannot show (over SSH), is error
///   -60005 or -60007 of the Authorization Services.
///
/// When Marsa stops waiting (the limit, or the daemon stopping), `osascript`
/// is killed, but the helper may already run as root. Every helper operation
/// can run again safely, so the message says so.
///
/// A message never quotes what the programs printed: only its size and the
/// exit code go to the log.
fn read_answer_for(flow: Flow, output: io::Result<RunOutput>) -> Result<Vec<OpResult>, ApiError> {
    let noun = flow.noun();
    let output = match output {
        Ok(output) => output,
        Err(err) if err.kind() == io::ErrorKind::TimedOut => {
            tracing::warn!("{noun} took more than 5 minutes; Marsa stopped waiting");
            let message = format!(
                "Marsa stopped waiting for {noun} after 5 minutes; some steps may already have \
                 run as root, and running {noun} again is safe"
            );
            return Err(failure(&flow.code("timed_out"), message));
        }
        Err(err) if err.kind() == io::ErrorKind::Interrupted => {
            tracing::warn!("the daemon is stopping; Marsa stopped waiting for {noun}");
            let message = format!(
                "the daemon stopped, so Marsa stopped waiting for {noun}; some steps may \
                 already have run as root, and running {noun} again is safe"
            );
            return Err(failure(&flow.code("interrupted"), message));
        }
        Err(err) => {
            tracing::warn!(error = %err, "cannot run osascript");
            return Err(failure(&flow.code("helper_failed"), "cannot run osascript"));
        }
    };
    let text = if output.success() {
        &output.stdout
    } else {
        &output.stderr
    };
    let answer = if text.len() > MAX_ANSWER {
        None
    } else if output.success() {
        parse_answer(text.trim_ascii())
    } else {
        braces(text).and_then(parse_answer)
    };
    let Some(answer) = answer else {
        if !output.success() && cancelled(&output.stderr) {
            tracing::info!("the admin prompt was cancelled");
            let message = "the admin prompt was cancelled";
            return Err(failure(&flow.code("cancelled"), message));
        }
        if !output.success() && not_authorized(&output.stderr) {
            tracing::info!("macOS did not give admin rights");
            let message = "macOS did not give admin rights: a wrong password, or no prompt";
            return Err(failure(&flow.code("not_authorized"), message));
        }
        tracing::warn!(
            exit = ?output.code,
            stdout_bytes = output.stdout.len(),
            stderr_bytes = output.stderr.len(),
            "marsa-helper gave no answer Marsa can read"
        );
        return Err(failure(
            &flow.code("helper_failed"),
            "the helper's answer cannot be read",
        ));
    };
    if let Some(refused) = answer.error {
        let code = short_code(&refused.code);
        tracing::warn!(code = %code, "marsa-helper refused the request of {noun}");
        let message = format!("the helper refused the request: {code}");
        return Err(failure(&flow.code("helper_rejected"), message));
    }
    if answer.results.is_empty() {
        tracing::warn!(exit = ?output.code, "marsa-helper answered with no result");
        return Err(failure(
            &flow.code("helper_failed"),
            "the helper gave no result",
        ));
    }
    Ok(answer.results)
}

/// The helper's answer in `json`, if it is one of this protocol version.
fn parse_answer(json: &[u8]) -> Option<HelperResponse> {
    let answer: HelperResponse = serde_json::from_slice(json).ok()?;
    (answer.version == HELPER_PROTOCOL_VERSION).then_some(answer)
}

/// From the first `{` to the last `}`: the helper's JSON inside the error
/// line of `osascript`, whose other parts hold no brace.
fn braces(text: &[u8]) -> Option<&[u8]> {
    let start = text.iter().position(|&byte| byte == b'{')?;
    let end = text.iter().rposition(|&byte| byte == b'}')?;
    text.get(start..=end)
}

/// AppleScript error -128. The number is the same in every language; the
/// English text is checked too, in case the number is cut.
fn cancelled(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr);
    text.contains("(-128)") || text.contains("User canceled")
}

/// `errAuthorizationDenied` (-60005: a wrong password, or not an admin) and
/// `errAuthorizationInteractionNotAllowed` (-60007: the prompt cannot show,
/// as over SSH). Only the numbers: the text depends on the language.
fn not_authorized(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr);
    text.contains("(-60005)") || text.contains("(-60007)")
}

/// A code from the helper, safe for a log line and a message.
fn short_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
        .take(64)
        .collect()
}

fn skip(op: &str, code: &str) -> SetupSkip {
    SetupSkip {
        op: op.to_owned(),
        code: code.to_owned(),
    }
}

fn failure(code: &str, message: impl Into<String>) -> ApiError {
    ApiError {
        code: code.to_owned(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    use axum::body::{Body, to_bytes};
    use axum::extract::Request;
    use axum::middleware::from_fn_with_state;
    use marsa_core::platform::daemon_file::{Token, daemon_authority};
    use marsa_core::types::{ErrorBody, OpStatus};
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tower::ServiceExt as _;

    use super::user_trust::SECURITY;
    use crate::daemon::auth::{AuthConfig, require_auth};

    /// The daemon port in the `Host` header. No socket is bound for it.
    const PORT: u16 = 51237;
    /// Real `netstat -anv -p tcp` output with Herd's nginx on 80 and 443.
    const NETSTAT_HERD: &str = include_str!("../../../../fixtures/ports/netstat-macos.txt");
    /// Real `ps` output for an nginx worker: its title only.
    const PS_WORKER: &str = include_str!("../../../../fixtures/ports/ps-nginx-worker.txt");
    const DONE: &str = include_str!("../../../../fixtures/helper/responses/done.json");
    const FAILED: &str = include_str!("../../../../fixtures/helper/responses/failed.json");
    const REJECTED: &str = include_str!("../../../../fixtures/helper/responses/rejected.json");
    pub(super) const TEST_CA: &str = include_str!("../../../../fixtures/helper/test-ca.pem");
    /// What `security verify-cert` prints for a root nobody trusts.
    const NOT_TRUSTED: &str = "Cert Verify Result: CSSMERR_TP_NOT_TRUSTED\n";
    /// ADR 0012, written out here and not taken from `APPLESCRIPT`, so a
    /// change to the constant fails this test.
    const SCRIPT: &str = "do shell script (quoted form of item 1 of argv) & \" apply \" & \
                          (quoted form of item 2 of argv) with administrator privileges";

    /// One call the fake runner got.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct Call {
        pub(super) program: &'static str,
        pub(super) args: Vec<String>,
        pub(super) limit: Duration,
    }

    type Answer = Box<dyn Fn(&'static str, &[String]) -> io::Result<RunOutput> + Send + Sync>;

    /// Records every call and answers by program and arguments. Runs
    /// nothing.
    pub(super) struct Fake {
        calls: Mutex<Vec<Call>>,
        answer: Answer,
    }

    impl Fake {
        /// Answers by program alone.
        fn new(
            answer: impl Fn(&'static str) -> io::Result<RunOutput> + Send + Sync + 'static,
        ) -> Arc<Self> {
            Self::answering(move |program, _| answer(program))
        }

        /// Answers by program and arguments.
        pub(super) fn answering(
            answer: impl Fn(&'static str, &[String]) -> io::Result<RunOutput> + Send + Sync + 'static,
        ) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::default(),
                answer: Box::new(answer),
            })
        }

        /// `netstat` prints `netstat`, `ps` prints the nginx worker,
        /// `osascript` gives `osascript()`, and `security` is a Mac that does
        /// not trust the CA yet: `verify-cert` exits 1, the add exits 0.
        fn scripted(
            netstat: &'static str,
            osascript: impl Fn() -> io::Result<RunOutput> + Send + Sync + 'static,
        ) -> Arc<Self> {
            Self::answering(move |program, args| match program {
                NETSTAT => ok(netstat),
                PS => ok(PS_WORKER),
                OSASCRIPT => osascript(),
                SECURITY => security_untrusted(args),
                other => panic!("unexpected program {other}"),
            })
        }

        pub(super) fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        /// The `security` calls only.
        fn security_calls(&self) -> Vec<Call> {
            self.calls()
                .into_iter()
                .filter(|call| call.program == SECURITY)
                .collect()
        }

        fn programs(&self) -> Vec<&'static str> {
            self.calls().iter().map(|call| call.program).collect()
        }

        /// The request the helper would have got, from the last `osascript`.
        fn request(&self) -> HelperRequest {
            let calls = self.calls();
            let call = calls.iter().rfind(|call| call.program == OSASCRIPT);
            let request = &call.expect("an osascript call").args[7];
            serde_json::from_str(request).unwrap()
        }
    }

    impl Runner for Fake {
        fn run(
            &self,
            program: &'static str,
            args: &[OsString],
            limit: Duration,
            _stop: &CancellationToken,
        ) -> io::Result<RunOutput> {
            let args: Vec<String> = args
                .iter()
                .map(|arg| arg.to_str().unwrap().to_owned())
                .collect();
            self.calls.lock().unwrap().push(Call {
                program,
                args: args.clone(),
                limit,
            });
            (self.answer)(program, &args)
        }
    }

    /// `security` on a Mac that does not trust the CA yet: `verify-cert`
    /// exits 1, and every other command exits 0.
    fn security_untrusted(args: &[String]) -> io::Result<RunOutput> {
        if args.first().map(String::as_str) == Some("verify-cert") {
            exit(1, NOT_TRUSTED)
        } else {
            ok("")
        }
    }

    pub(super) fn ok(stdout: &str) -> io::Result<RunOutput> {
        Ok(RunOutput {
            code: Some(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }

    /// A program that exited with `code` and printed `stderr`.
    pub(super) fn exit(code: i32, stderr: &str) -> io::Result<RunOutput> {
        Ok(RunOutput {
            code: Some(code),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    /// What `osascript` gives when the script failed: exit 1 and `stderr`.
    fn script_error(stderr: &str) -> io::Result<RunOutput> {
        exit(1, stderr)
    }

    /// The helper's answer as one line, as it prints it.
    fn line(json: &str) -> String {
        serde_json::from_str::<Value>(json).unwrap().to_string()
    }

    /// The real netstat output without Herd's nginx: nobody on 80 or 443.
    fn netstat_free() -> &'static str {
        let text: Vec<&str> = NETSTAT_HERD
            .lines()
            .filter(|line| !line.contains("nginx-arm64"))
            .collect();
        text.join("\n").leak()
    }

    /// One done result per op, as the helper prints it.
    fn all_done(ops: &[&str]) -> String {
        let results: Vec<Value> = ops
            .iter()
            .map(|op| json!({ "op": op, "status": "done" }))
            .collect();
        json!({ "version": 1, "results": results }).to_string()
    }

    fn results(json: &str) -> Vec<OpResult> {
        serde_json::from_str::<HelperResponse>(json)
            .unwrap()
            .results
    }

    /// A data folder, with Caddy's CA root if `ca`, and a `marsa-helper`
    /// file in `bin/`. The user's home folder is `home/` in it: a path only,
    /// never made.
    struct Dir {
        dir: TempDir,
    }

    impl Dir {
        fn new(ca: bool) -> Self {
            let dir = TempDir::new().unwrap();
            if ca {
                let root = CaddyPaths::in_dir(dir.path()).ca_root();
                fs::create_dir_all(root.parent().unwrap()).unwrap();
                fs::write(&root, TEST_CA).unwrap();
            }
            let helper = dir.path().join("bin").join("marsa-helper");
            fs::create_dir_all(helper.parent().unwrap()).unwrap();
            fs::write(&helper, "").unwrap();
            // As cargo and an install leave them, whatever the umask.
            #[cfg(unix)]
            for path in [&helper, &dir.path().join("bin")] {
                set_mode(path, 0o755);
            }
            Self { dir }
        }

        fn helper(&self) -> PathBuf {
            self.dir.path().join("bin").join("marsa-helper")
        }

        fn home(&self) -> PathBuf {
            self.dir.path().join("home")
        }

        fn ca_root(&self) -> PathBuf {
            CaddyPaths::in_dir(self.dir.path()).ca_root()
        }

        fn setup(&self, fake: &Arc<Fake>) -> Setup {
            Setup::with(
                fake.clone(),
                Some(Platform::DarwinArm64),
                Some(self.helper()),
                Some(self.home()),
            )
        }

        /// [`Setup::run`] with the default Caddy ports.
        fn run(&self, fake: &Arc<Fake>) -> SetupReport {
            let caddy = CaddyInput::default();
            let stop = CancellationToken::new();
            self.setup(fake)
                .run(self.dir.path(), caddy, Platform::DarwinArm64, &stop)
        }
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn trust() -> HelperOp {
        HelperOp::TrustInstall {
            pem: TEST_CA.to_owned(),
        }
    }

    fn resolver() -> HelperOp {
        HelperOp::ResolverInstall { port: 18053 }
    }

    fn pf(http_port: u16, https_port: u16) -> HelperOp {
        HelperOp::PfInstall {
            http_port,
            https_port,
        }
    }

    fn error_code(report: &SetupReport) -> Option<&str> {
        report.error.as_ref().map(|error| error.code.as_str())
    }

    fn done(op: &str) -> OpResult {
        OpResult {
            op: op.into(),
            status: OpStatus::Done,
            code: None,
            message: None,
        }
    }

    /// A helper file at `path`, with the modes of an install, and its
    /// folder.
    fn make_helper(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
        #[cfg(unix)]
        for path in [path, path.parent().unwrap()] {
            set_mode(path, 0o755);
        }
    }

    /// The route with `setup` behind the real auth layer, on `daemon`.
    struct App {
        router: Router,
        bearer: String,
    }

    impl App {
        fn new(daemon: Arc<Daemon>, setup: Setup) -> Self {
            let token = Token::generate().unwrap();
            let bearer = token.bearer_header();
            let auth = AuthConfig::new(Arc::new(token), PORT);
            let router = routes_with(setup)
                .with_state(daemon)
                .layer(from_fn_with_state(auth, require_auth));
            Self { router, bearer }
        }

        async fn post(&self) -> (StatusCode, Value) {
            let request = Request::builder()
                .method("POST")
                .uri("/v1/setup")
                .header("host", daemon_authority(PORT))
                .header("authorization", &self.bearer)
                .body(Body::empty())
                .unwrap();
            let response = self.router.clone().oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
            (status, serde_json::from_slice(&bytes).unwrap())
        }

        async fn refused(&self) -> (StatusCode, String) {
            let (status, json) = self.post().await;
            let error: ErrorBody = serde_json::from_value(json).unwrap();
            (status, error.error.code)
        }

        /// `POST /v1/setup` with only `headers`, expecting an error body.
        async fn send(&self, headers: &[(&str, &str)]) -> (StatusCode, String) {
            let mut request = Request::builder().method("POST").uri("/v1/setup");
            for &(name, value) in headers {
                request = request.header(name, value);
            }
            let request = request.body(Body::empty()).unwrap();
            let response = self.router.clone().oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
            let error: ErrorBody = serde_json::from_slice(&bytes).unwrap();
            (status, error.error.code)
        }
    }

    #[test]
    fn the_osascript_arguments_are_pinned() {
        let dir = Dir::new(false);
        let fake = Fake::scripted(netstat_free(), || ok(DONE));
        dir.run(&fake);

        let calls = fake.calls();
        let osascript = calls.last().unwrap();
        assert_eq!(osascript.program, "/usr/bin/osascript");
        let helper = dir.helper().to_str().unwrap().to_owned();
        let request = osascript.args[7].clone();
        assert_eq!(
            osascript.args,
            [
                "-e",
                "on run argv",
                "-e",
                SCRIPT,
                "-e",
                "end run",
                helper.as_str(),
                request.as_str(),
            ]
        );
        assert!(Path::new(&helper).is_absolute());
        assert_eq!(osascript.limit, Duration::from_secs(300));
        assert_eq!(
            serde_json::from_str::<Value>(&request).unwrap(),
            json!({ "version": 1, "ops": [
                { "op": "resolver_install", "port": 18053 },
                { "op": "pf_install", "http_port": 18080, "https_port": 18443 },
            ] })
        );
    }

    #[test]
    fn the_user_trust_commands_are_pinned_and_come_after_the_helper() {
        let dir = Dir::new(true);
        let answer = all_done(&["resolver_install", "pf_install"]);
        let fake = Fake::scripted(netstat_free(), move || ok(&answer));
        let report = dir.run(&fake);

        // The request has no trust_install: on macOS the helper does not
        // trust the CA (ADR 0015), and the user's trust comes after it.
        assert_eq!(fake.request().ops, [resolver(), pf(18080, 18443)]);
        assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT, SECURITY, SECURITY]);
        let ca_root = dir.ca_root();
        let ca_root = ca_root.to_str().unwrap();
        let keychain = dir
            .home()
            .join("Library")
            .join("Keychains")
            .join("login.keychain-db");
        let keychain = keychain.to_str().unwrap();
        let security = fake.security_calls();
        // The absolute program, written out: `RealRunner` refuses a bare
        // name, so a constant that lost its path would fail only on a Mac.
        assert_eq!(security[0].program, "/usr/bin/security");
        assert_eq!(security[1].program, "/usr/bin/security");
        assert_eq!(security[0].args, ["verify-cert", "-c", ca_root, "-L", "-l"]);
        assert_eq!(security[0].limit, Duration::from_secs(10));
        assert_eq!(
            security[1].args,
            [
                "add-trusted-cert",
                "-r",
                "trustRoot",
                "-k",
                keychain,
                ca_root
            ]
        );
        assert_eq!(security[1].limit, Duration::from_secs(300));
        assert!(Path::new(ca_root).is_absolute() && Path::new(keychain).is_absolute());

        // The trust result comes after the helper's, under the helper's op
        // name, so the CLI prints it like the others.
        assert_eq!(
            report.results,
            [
                done("resolver_install"),
                done("pf_install"),
                done("trust_install")
            ]
        );
        assert!(report.skipped.is_empty());
        assert_eq!(report.error, None);
    }

    #[test]
    fn a_ca_the_user_trusts_already_is_unchanged_and_nothing_is_added() {
        let dir = Dir::new(true);
        let answer = all_done(&["resolver_install", "pf_install"]);
        let fake = Fake::answering(move |program, args| match program {
            NETSTAT => ok(netstat_free()),
            OSASCRIPT => ok(&answer),
            SECURITY if args[0] == "verify-cert" => ok("...certificate verification successful.\n"),
            other => panic!("{other} {args:?} ran"),
        });
        let report = dir.run(&fake);

        assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT, SECURITY]);
        let trust = report.results.last().unwrap();
        assert_eq!(trust.op, "trust_install");
        assert_eq!(trust.status, OpStatus::Unchanged);
        assert_eq!(report.results.len(), 3);
        assert_eq!(report.error, None);
    }

    #[test]
    fn a_dialog_refused_or_left_open_is_trust_add_failed_and_the_helper_ops_stay() {
        /// What `add-trusted-cert` prints when the dialog is refused.
        const REFUSED: &str = "SecTrustSettingsSetTrustSettings: The authorization was denied \
                               since no user interaction was possible.\n";
        /// What `add-trusted-cert` gives back.
        type Add = fn() -> io::Result<RunOutput>;
        // The case, the add, and the words the message must hold.
        let cases: [(&str, Add, &str); 4] = [
            ("refused", || exit(1, REFUSED), "exited with code 1"),
            (
                "left open",
                || Err(io::Error::from(io::ErrorKind::TimedOut)),
                "did not finish in 5 minutes",
            ),
            (
                "daemon stopping",
                || Err(io::Error::from(io::ErrorKind::Interrupted)),
                "was stopped with the daemon",
            ),
            (
                "security missing",
                || Err(io::Error::from(io::ErrorKind::NotFound)),
                "could not run",
            ),
        ];
        for (name, add, how) in cases {
            let dir = Dir::new(true);
            let fake = Fake::answering(move |program, args| match program {
                NETSTAT => ok(netstat_free()),
                OSASCRIPT => ok(DONE),
                SECURITY if args[0] == "verify-cert" => exit(1, NOT_TRUSTED),
                SECURITY => add(),
                other => panic!("{other} ran"),
            });
            let report = dir.run(&fake);

            // The helper's results stay, and the setup has no error: the
            // failed step is in the results, like a failed helper op.
            assert_eq!(report.error, None, "{name}");
            assert_eq!(report.results.len(), 3, "{name}");
            assert_eq!(report.results[..2], results(DONE)[..], "{name}");
            let trust = &report.results[2];
            assert_eq!(trust.op, "trust_install", "{name}");
            assert_eq!(trust.status, OpStatus::Failed, "{name}");
            assert_eq!(trust.code.as_deref(), Some("trust_add_failed"), "{name}");
            let message = trust.message.as_deref().unwrap().to_ascii_lowercase();
            // What to do: the dialog shows in the daemon's session, so a
            // daemon started over SSH has to stop first.
            for part in [
                how,
                "in terminal on the mac's desktop",
                "approve the dialog",
                "started over ssh",
                "\"marsa daemon stop\" first",
            ] {
                assert!(message.contains(part), "{name}: {message}");
            }
            // Nothing `security` printed is in the message.
            assert!(!message.contains("authorization"), "{name}: {message}");
            assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT, SECURITY, SECURITY]);
        }
    }

    #[test]
    fn no_home_folder_is_trust_add_failed_after_the_helper() {
        let dir = Dir::new(true);
        let answer = all_done(&["resolver_install", "pf_install"]);
        let fake = Fake::scripted(netstat_free(), move || ok(&answer));
        let setup = Setup::with(
            fake.clone(),
            Some(Platform::DarwinArm64),
            Some(dir.helper()),
            None,
        );
        let stop = CancellationToken::new();
        let report = setup.run(
            dir.dir.path(),
            CaddyInput::default(),
            Platform::DarwinArm64,
            &stop,
        );

        assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT]);
        assert_eq!(report.results.len(), 3);
        let trust = &report.results[2];
        assert_eq!(trust.op, "trust_install");
        assert_eq!(trust.code.as_deref(), Some("trust_add_failed"));
        assert_eq!(report.error, None);
    }

    #[test]
    fn the_client_waits_longer_than_the_slowest_setup() {
        use marsa_core::client::SETUP_TIMEOUT;

        // 11 min 40 s of limits, and 2 s of grace after each of 12 programs.
        assert_eq!(SLOWEST_SETUP, Duration::from_secs(724));
        // With room for the HTTP round trip and for killing a stalled
        // program.
        let margin = Duration::from_secs(30);
        assert!(
            SETUP_TIMEOUT >= SLOWEST_SETUP + margin,
            "the client waits {SETUP_TIMEOUT:?}, and a setup may take {SLOWEST_SETUP:?}"
        );

        // The sum holds every program a setup runs. A full run with nobody
        // on 80 or 443 runs all of them but `ps`, each with its limit, so a
        // new program in the flow fails here until it is in `SLOWEST_SETUP`,
        // which then has to fit the client's wait above.
        let dir = Dir::new(true);
        let answer = all_done(&["resolver_install", "pf_install"]);
        let fake = Fake::scripted(netstat_free(), move || ok(&answer));
        dir.run(&fake);
        let calls = fake.calls();
        assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT, SECURITY, SECURITY]);
        let ps = u32::try_from(MAX_PS).unwrap();
        let programs = u32::try_from(calls.len()).unwrap() + ps;
        let limits: Duration = calls.iter().map(|call| call.limit).sum();
        assert_eq!(
            limits + CHECK_LIMIT * ps + PIPE_GRACE * programs,
            SLOWEST_SETUP
        );
    }

    #[test]
    fn a_helper_in_a_protected_folder_is_refused_before_any_prompt() {
        // `desktop`: the usual macOS disk takes it for `Desktop`.
        for folder in ["Desktop", "Documents", "Downloads", "desktop"] {
            let dir = Dir::new(true);
            let helper = dir
                .home()
                .join(folder)
                .join("marsa")
                .join("bin")
                .join("marsa-helper");
            make_helper(&helper);
            let fake = Fake::scripted(netstat_free(), || panic!("osascript ran"));
            let setup = Setup::with(
                fake.clone(),
                Some(Platform::DarwinArm64),
                Some(helper),
                Some(dir.home()),
            );
            let stop = CancellationToken::new();
            let report = setup.run(
                dir.dir.path(),
                CaddyInput::default(),
                Platform::DarwinArm64,
                &stop,
            );

            let error = report.error.as_ref().expect(folder);
            assert_eq!(error.code, "setup_helper_protected_folder", "{folder}");
            for part in [folder, INSTALL_BIN, "marsa daemon stop", "marsa setup"] {
                assert!(error.message.contains(part), "{folder}: {}", error.message);
            }
            assert!(report.results.is_empty(), "{folder}");
            // No admin prompt, and no trust dialog after it either.
            assert_eq!(fake.programs(), [NETSTAT], "{folder}");
        }

        // By the path alone, before any look at the file: a helper that is
        // not there gets the same answer, not `setup_helper_missing`.
        let dir = Dir::new(true);
        let helper = dir.home().join("Downloads").join("marsa-helper");
        let fake = Fake::scripted(netstat_free(), || panic!("osascript ran"));
        let setup = Setup::with(
            fake.clone(),
            Some(Platform::DarwinArm64),
            Some(helper),
            Some(dir.home()),
        );
        let stop = CancellationToken::new();
        let report = setup.run(
            dir.dir.path(),
            CaddyInput::default(),
            Platform::DarwinArm64,
            &stop,
        );
        assert_eq!(error_code(&report), Some("setup_helper_protected_folder"));
        assert_eq!(fake.programs(), [NETSTAT]);
    }

    #[test]
    fn protected_folders_match_by_component_under_the_home_folder() {
        let home = Path::new("/Users/dev");
        let under = |name: &str| Some(PathBuf::from("/Users/dev").join(name));
        let cases = [
            ("/Users/dev/Desktop/marsa-helper", under("Desktop")),
            (
                "/Users/dev/Desktop/marsa/target/release/marsa-helper",
                under("Desktop"),
            ),
            ("/Users/dev/Documents/x/marsa-helper", under("Documents")),
            ("/Users/dev/Downloads/marsa-helper", under("Downloads")),
            // Without ASCII case, and as the path writes the folder.
            ("/Users/dev/desktop/marsa-helper", under("desktop")),
            ("/Users/dev/DOWNLOADS/x/marsa-helper", under("DOWNLOADS")),
            ("/Users/dev/documents2/marsa-helper", None),
            ("/Users/dev/Desktop2/marsa-helper", None),
            ("/Users/dev/DesktopX/Desktop/marsa-helper", None),
            (
                "/Users/dev/Library/Application Support/dev.marsa.Marsa/bin/marsa-helper",
                None,
            ),
            ("/Users/other/Desktop/marsa-helper", None),
            ("/Desktop/marsa-helper", None),
        ];
        for (helper, want) in cases {
            let helper = Path::new(helper);
            assert_eq!(
                protected_folder(helper, Some(home)),
                want,
                "{}",
                helper.display()
            );
            // Without a home folder nothing can be known, so nothing matches.
            assert_eq!(protected_folder(helper, None), None, "{}", helper.display());
        }
    }

    #[test]
    fn linux_keeps_trust_install_for_the_helper_and_runs_no_security() {
        // ADR 0011 stays off macOS: the request carries the certificate, and
        // neither the user trust nor the protected folder check runs. Setup
        // itself answers 501 there until task M4-18; this pins the request it
        // will send.
        let dir = Dir::new(true);
        let helper = dir.home().join("Desktop").join("marsa-helper");
        make_helper(&helper);
        let answer = all_done(&["trust_install", "resolved_install", "pf_install"]);
        let fake = Fake::scripted(netstat_free(), move || ok(&answer));
        let setup = Setup::with(
            fake.clone(),
            Some(Platform::LinuxX64),
            Some(helper),
            Some(dir.home()),
        );
        let stop = CancellationToken::new();
        let report = setup.run(
            dir.dir.path(),
            CaddyInput::default(),
            Platform::LinuxX64,
            &stop,
        );

        assert_eq!(fake.request().ops[0], trust());
        assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT]);
        assert_eq!(report.results.len(), 3);
        assert_eq!(report.error, None);
    }

    #[test]
    fn herd_on_80_and_443_means_no_pf_install() {
        let dir = Dir::new(true);
        let answer = all_done(&["resolver_install"]);
        let fake = Fake::scripted(NETSTAT_HERD, move || ok(&answer));
        let report = dir.run(&fake);

        let nginx = |port| PortHolder {
            port,
            address: "127.0.0.1".into(),
            process: Some("nginx-arm64".into()),
            pid: Some(15464),
            user: Some("dev".into()),
            // The worker's title gives no hint; netstat's name says Herd.
            command: Some("nginx: worker process".into()),
            app: Some("Herd".into()),
        };
        assert_eq!(report.port_holders, [nginx(80), nginx(443)]);
        assert_eq!(report.skipped, [skip("pf_install", "redirect_port_held")]);
        assert_eq!(fake.request().ops, [resolver()]);
        assert_eq!(
            report.results,
            [done("resolver_install"), done("trust_install")]
        );
        assert_eq!(report.error, None);

        // `ps` once for the one PID that holds both ports.
        let calls = fake.calls();
        assert_eq!(
            fake.programs(),
            [NETSTAT, PS, OSASCRIPT, SECURITY, SECURITY]
        );
        assert_eq!(calls[0].args, ["-anv", "-p", "tcp"]);
        assert_eq!(calls[1].args, ["-p", "15464", "-o", "user=,comm="]);
        assert_eq!(calls[0].limit, Duration::from_secs(10));
        assert_eq!(calls[1].limit, Duration::from_secs(10));
    }

    #[test]
    fn no_ca_means_no_trust_install() {
        let dir = Dir::new(false);
        let fake = Fake::scripted(netstat_free(), || ok(DONE));
        let report = dir.run(&fake);

        assert_eq!(report.skipped, [skip("trust_install", "ca_not_ready")]);
        assert_eq!(fake.request().ops, [resolver(), pf(18080, 18443)]);
        assert_eq!(report.results, results(DONE));
        assert!(report.port_holders.is_empty());
        assert_eq!(report.error, None);
    }

    #[test]
    fn a_ca_that_cannot_be_used_is_ca_unreadable() {
        let dir = Dir::new(false);
        let root = CaddyPaths::in_dir(dir.dir.path()).ca_root();
        fs::create_dir_all(&root).unwrap();
        assert_eq!(read_ca(&root), Err("ca_unreadable"));
        fs::remove_dir(&root).unwrap();
        fs::write(&root, "x".repeat(16 * 1024 + 1)).unwrap();
        assert_eq!(read_ca(&root), Err("ca_unreadable"));
        fs::write(&root, [0xff, 0xfe]).unwrap();
        assert_eq!(read_ca(&root), Err("ca_unreadable"));
        fs::write(&root, TEST_CA).unwrap();
        assert_eq!(read_ca(&root).as_deref(), Ok(TEST_CA));
    }

    #[test]
    fn a_pem_the_helper_would_refuse_is_ca_unreadable_and_the_rest_goes() {
        let refused = [
            ("empty", String::new()),
            ("not ASCII", format!("{TEST_CA}é")),
            ("a NUL byte", format!("{TEST_CA}\0")),
            (
                "another block",
                "-----BEGIN PRIVATE KEY-----\nAAAA\n".to_owned(),
            ),
            ("text first", format!("garbage\n{TEST_CA}")),
        ];
        for (name, pem) in refused {
            let dir = Dir::new(false);
            let root = CaddyPaths::in_dir(dir.dir.path()).ca_root();
            fs::create_dir_all(root.parent().unwrap()).unwrap();
            fs::write(&root, &pem).unwrap();
            assert_eq!(read_ca(&root), Err("ca_unreadable"), "{name}");

            // Only the trust step is skipped: DNS and pf still go.
            let answer = all_done(&["resolver_install", "pf_install"]);
            let fake = Fake::scripted(netstat_free(), move || ok(&answer));
            let report = dir.run(&fake);
            assert_eq!(
                report.skipped,
                [skip("trust_install", "ca_unreadable")],
                "{name}"
            );
            assert_eq!(fake.request().ops, [resolver(), pf(18080, 18443)], "{name}");
            assert_eq!(report.error, None, "{name}");
            // A certificate that cannot be used never reaches `security`:
            // only the helper's two results come back.
            assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT], "{name}");
            assert_eq!(report.results.len(), 2, "{name}");
        }
        // The helper allows whitespace before the BEGIN line.
        assert_eq!(pem_problem(&format!(" \t\r\n{TEST_CA}")), None);
        assert_eq!(pem_problem(TEST_CA), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_in_place_of_the_ca_does_not_hold_up_setup() {
        let dir = Dir::new(false);
        let root = CaddyPaths::in_dir(dir.dir.path()).ca_root();
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        // std cannot make a FIFO, so `mkfifo` does, in the test's own folder.
        let made = Command::new("/usr/bin/mkfifo").arg(&root).status().unwrap();
        assert!(made.success());
        // On a thread: a blocking `open` would wait for a writer forever.
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || sender.send(read_ca(&root)));
        let got = receiver.recv_timeout(Duration::from_secs(5));
        assert_eq!(
            got.expect("read_ca waited on the FIFO"),
            Err("ca_unreadable")
        );
    }

    #[test]
    fn a_cancelled_prompt_is_setup_cancelled() {
        for stderr in [
            "0:180: execution error: User canceled. (-128)\n",
            // Another language: the number is the same.
            "0:180: execution error: Vom Benutzer abgebrochen. (-128)\r",
        ] {
            let dir = Dir::new(true);
            let fake = Fake::scripted(netstat_free(), move || script_error(stderr));
            let report = dir.run(&fake);
            assert_eq!(error_code(&report), Some("setup_cancelled"), "{stderr}");
            assert!(report.results.is_empty());
            assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT]);
        }
    }

    #[test]
    fn an_answer_that_cannot_be_read_is_setup_helper_failed() {
        let cases: Vec<(&str, io::Result<RunOutput>)> = vec![
            ("garbage", ok("garbage from the helper")),
            ("empty", ok("")),
            ("newer protocol", ok(r#"{"version":2,"results":[]}"#)),
            ("no result", ok(r#"{"version":1}"#)),
            (
                "too large",
                ok(&format!("{}{}", " ".repeat(MAX_ANSWER), DONE)),
            ),
            (
                "shell error",
                script_error("0:94: execution error: sh: garbage: Permission denied (126)"),
            ),
            (
                "osascript missing",
                Err(io::Error::from(io::ErrorKind::NotFound)),
            ),
        ];
        for (name, output) in cases {
            let answer = read_answer(output);
            let error = answer.expect_err(name);
            assert_eq!(error.code, "setup_helper_failed", "{name}");
            // Nothing the programs printed is copied into the message.
            assert!(
                !error.message.contains("garbage"),
                "{name}: {}",
                error.message
            );
        }
    }

    #[test]
    fn a_failed_op_comes_back_through_the_script_error() {
        // The helper exits 1 when an op fails, so `do shell script` raises
        // an error whose text is the helper's JSON line.
        let stderr = format!("7:83: execution error: {} (1)\n", line(FAILED));
        assert_eq!(read_answer(script_error(&stderr)), Ok(results(FAILED)));
        let failed = &results(FAILED)[1];
        assert_eq!(failed.status, OpStatus::Failed);
        assert_eq!(failed.code.as_deref(), Some("op_not_implemented"));
    }

    #[test]
    fn a_refused_request_is_setup_helper_rejected() {
        let stderr = format!("7:83: execution error: {} (2)\n", line(REJECTED));
        let error = read_answer(script_error(&stderr)).unwrap_err();
        assert_eq!(error.code, "setup_helper_rejected");
        assert_eq!(
            error.message,
            "the helper refused the request: request_invalid"
        );
    }

    #[test]
    fn a_prompt_left_open_or_a_stopping_daemon_ends_setup() {
        let cases = [
            (io::ErrorKind::TimedOut, "setup_timed_out"),
            (io::ErrorKind::Interrupted, "setup_interrupted"),
        ];
        for (kind, code) in cases {
            let error = read_answer(Err(io::Error::from(kind))).unwrap_err();
            assert_eq!(error.code, code);
            // The helper may have run as root already, and a rerun is safe.
            for part in [
                "stopped waiting",
                "may already have run as root",
                "again is safe",
            ] {
                assert!(error.message.contains(part), "{code}: {}", error.message);
            }
        }
    }

    #[test]
    fn a_wrong_password_or_no_prompt_is_setup_not_authorized() {
        for stderr in [
            "0:180: execution error: The administrator user name or password was incorrect. \
             (-60005)\n",
            "0:180: execution error: The authorization was denied since no user interaction \
             was possible. (-60007)\n",
        ] {
            let error = read_answer(script_error(stderr)).unwrap_err();
            assert_eq!(error.code, "setup_not_authorized", "{stderr}");
            assert!(!error.message.contains("incorrect"), "{}", error.message);
        }
    }

    #[test]
    fn a_message_with_brackets_quotes_and_accents_survives_the_script_error() {
        // What osascript printed on macOS for a shell that printed this JSON
        // line and exited 1 (run by hand without admin rights).
        let stderr = r#"7:88: execution error: {"version":1,"results":[{"op":"resolver_install","status":"failed","code":"resolver_write","message":"cannot write (x) } \"q\" é"}]} (1)"#;
        let results = read_answer(script_error(stderr)).unwrap();
        assert_eq!(
            results,
            [OpResult {
                op: "resolver_install".into(),
                status: OpStatus::Failed,
                code: Some("resolver_write".into()),
                message: Some("cannot write (x) } \"q\" é".into()),
            }]
        );
    }

    #[test]
    fn results_for_other_operations_are_setup_helper_failed() {
        // No CA: the request is resolver_install, then pf_install.
        let answers = [
            all_done(&["resolver_install"]),
            all_done(&["pf_install", "resolver_install"]),
            all_done(&["resolver_install", "pf_install", "trust_install"]),
            all_done(&["resolver_install", "trust_install"]),
        ];
        for answer in answers {
            let dir = Dir::new(false);
            let sent = answer.clone();
            let fake = Fake::scripted(netstat_free(), move || ok(&sent));
            let report = dir.run(&fake);
            assert_eq!(error_code(&report), Some("setup_helper_failed"), "{answer}");
            assert!(report.results.is_empty(), "{answer}");
        }
        // `uninstall_all` gives one result per removal, so it is not checked.
        let names = [UNINSTALL_ALL.to_owned()];
        assert!(results_match(
            &names,
            &results(&all_done(&["pf_remove", "resolver_remove"]))
        ));
        let ops = [trust(), resolver(), pf(1, 2), HelperOp::UninstallAll];
        let names: Vec<String> = ops.iter().map(|op| op_name(op).unwrap()).collect();
        assert_eq!(
            names,
            [
                "trust_install",
                "resolver_install",
                "pf_install",
                "uninstall_all"
            ]
        );
    }

    #[test]
    fn no_op_means_no_prompt() {
        let dir = Dir::new(false);
        let fake = Fake::new(|program| panic!("{program} ran"));
        let setup = dir.setup(&fake);
        let stop = CancellationToken::new();
        assert_eq!(
            setup.apply(Vec::new(), Platform::DarwinArm64, &stop),
            Ok(Vec::new())
        );
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn a_missing_helper_is_setup_helper_missing_and_no_prompt() {
        let dir = Dir::new(true);
        let fake = Fake::scripted(netstat_free(), || panic!("osascript ran"));
        let stop = CancellationToken::new();
        let missing = dir.dir.path().join("nowhere").join("marsa-helper");
        for helper in [None, Some(missing), Some(PathBuf::from("marsa-helper"))] {
            let setup = Setup::with(
                fake.clone(),
                Some(Platform::DarwinArm64),
                helper,
                Some(dir.home()),
            );
            let report = setup.run(
                dir.dir.path(),
                CaddyInput::default(),
                Platform::DarwinArm64,
                &stop,
            );
            assert_eq!(error_code(&report), Some("setup_helper_missing"));
            assert!(report.results.is_empty());
        }
        // No prompt, and no trust dialog after a setup that stopped.
        assert_eq!(fake.programs(), [NETSTAT, NETSTAT, NETSTAT]);
    }

    #[cfg(unix)]
    #[test]
    fn a_helper_others_could_change_is_setup_helper_unsafe_and_no_prompt() {
        let file_writable = "the file is writable by group or others";
        let folder_writable = "the folder is writable by group or others";
        let not_regular = "the file is not a regular file";
        /// What a case does to the helper file at the path.
        type Change = fn(&Path);
        let cases: [(&str, Change, &str); 6] = [
            (
                "group may write the file",
                |h| set_mode(h, 0o775),
                file_writable,
            ),
            (
                "others may write the file",
                |h| set_mode(h, 0o757),
                file_writable,
            ),
            (
                "group may write the folder",
                |h| set_mode(h.parent().unwrap(), 0o775),
                folder_writable,
            ),
            (
                "others may write the folder",
                |h| set_mode(h.parent().unwrap(), 0o777),
                folder_writable,
            ),
            (
                "a symlink",
                |h| {
                    let real = h.parent().unwrap().join("real-helper");
                    fs::rename(h, &real).unwrap();
                    std::os::unix::fs::symlink(&real, h).unwrap();
                },
                not_regular,
            ),
            (
                "a folder",
                |h| {
                    fs::remove_file(h).unwrap();
                    fs::create_dir(h).unwrap();
                },
                not_regular,
            ),
        ];
        for (name, change, reason) in cases {
            let dir = Dir::new(true);
            assert_eq!(check_helper(&dir.helper()), Ok(()), "{name}");
            change(&dir.helper());
            assert_eq!(
                check_helper(&dir.helper()),
                Err(HelperFault::Unsafe(reason)),
                "{name}"
            );

            let fake = Fake::scripted(netstat_free(), || panic!("osascript ran"));
            let report = dir.run(&fake);
            let error = report.error.as_ref().expect(name);
            assert_eq!(error.code, "setup_helper_unsafe", "{name}");
            assert!(error.message.contains(reason), "{name}: {}", error.message);
            assert!(report.results.is_empty(), "{name}");
            assert_eq!(fake.programs(), [NETSTAT], "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_current_user_is_the_owner_of_a_new_file() {
        use std::os::unix::fs::MetadataExt;

        // A file owned by another user needs root to make, so the owner
        // check is tested through the user it compares with.
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("mine");
        fs::write(&file, "").unwrap();
        assert_eq!(current_uid().unwrap(), fs::metadata(&file).unwrap().uid());
    }

    #[test]
    fn netstat_output_cut_at_the_limit_means_no_pf_install() {
        // Real lines with nobody on 80 or 443, repeated to just under the
        // limit: read as usual.
        let free = netstat_free();
        let mut text = String::new();
        while text.len() + free.len() + 1 < MAX_OUTPUT as usize {
            text.push_str(free);
            text.push('\n');
        }
        let run = |text: String, done: &[&str]| {
            let dir = Dir::new(true);
            let answer = all_done(done);
            let fake = Fake::answering(move |program, args| match program {
                NETSTAT => ok(&text),
                OSASCRIPT => ok(&answer),
                SECURITY => security_untrusted(args),
                other => panic!("unexpected program {other}"),
            });
            let report = dir.run(&fake);
            (report, fake.request().ops, fake.programs())
        };
        let all = ["resolver_install", "pf_install"];
        let (report, ops, _) = run(text.clone(), &all);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert_eq!(ops, [resolver(), pf(18080, 18443)]);
        assert_eq!(report.error, None);

        // Filled up to the limit, as `RealRunner` keeps a longer output: a
        // holder could sit in the part that was not read.
        text.push_str(&" ".repeat(MAX_OUTPUT as usize - text.len()));
        let (report, ops, programs) = run(text, &all[..1]);
        assert_eq!(report.skipped, [skip("pf_install", "ports_not_checked")]);
        assert_eq!(ops, [resolver()]);
        assert_eq!(programs, [NETSTAT, OSASCRIPT, SECURITY, SECURITY]);
        assert_eq!(report.error, None);
    }

    #[test]
    fn netstat_that_fails_or_is_not_understood_means_no_pf_install() {
        let outputs: [fn() -> io::Result<RunOutput>; 3] = [
            || Err(io::Error::from(io::ErrorKind::NotFound)),
            || script_error("netstat: sysctl: Operation not permitted"),
            || ok("Active Internet connections\nsomething new\n"),
        ];
        for netstat in outputs {
            let dir = Dir::new(true);
            let answer = all_done(&["resolver_install"]);
            let fake = Fake::answering(move |program, args| match program {
                NETSTAT => netstat(),
                OSASCRIPT => ok(&answer),
                SECURITY => security_untrusted(args),
                other => panic!("unexpected program {other}"),
            });
            let report = dir.run(&fake);
            assert_eq!(report.skipped, [skip("pf_install", "ports_not_checked")]);
            assert_eq!(fake.request().ops, [resolver()]);
            assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT, SECURITY, SECURITY]);
            assert_eq!(report.error, None);
        }
    }

    #[tokio::test]
    async fn the_whole_setup_over_http() {
        let dir = Dir::new(true);
        let answer = all_done(&["resolver_install", "pf_install"]);
        let fake = Fake::scripted(netstat_free(), move || ok(&answer));
        let mut daemon = Daemon::new(dir.dir.path().to_path_buf());
        // The ports come from the daemon's Caddy settings.
        daemon.caddy.input.http_port = 28080;
        daemon.caddy.input.https_port = 28443;
        let daemon = Arc::new(daemon);
        let mut events = daemon.events.subscribe();
        let app = App::new(daemon.clone(), dir.setup(&fake));

        let (status, json) = app.post().await;
        assert_eq!(status, StatusCode::OK, "{json}");
        let report: SetupReport = serde_json::from_value(json).unwrap();
        let want = SetupReport {
            results: vec![
                done("resolver_install"),
                done("pf_install"),
                done("trust_install"),
            ],
            ..SetupReport::default()
        };
        assert_eq!(report, want);
        assert_eq!(
            fake.request(),
            HelperRequest::new(vec![resolver(), pf(28080, 28443)])
        );
        assert_eq!(events.try_recv().unwrap(), Event::SetupFinished(want));
    }

    #[tokio::test]
    async fn a_second_setup_while_one_runs_gets_409() {
        let dir = Dir::new(false);
        let (started, mut prompt_shown) = tokio::sync::mpsc::unbounded_channel();
        let (answer, answers) = mpsc::channel::<()>();
        let answers = Mutex::new(answers);
        let fake = Fake::scripted(netstat_free(), move || {
            started.send(()).unwrap();
            answers.lock().unwrap().recv().unwrap();
            ok(DONE)
        });
        let daemon = Arc::new(Daemon::new(dir.dir.path().to_path_buf()));
        let app = Arc::new(App::new(daemon, dir.setup(&fake)));

        let first = tokio::spawn({
            let app = app.clone();
            async move { app.post().await }
        });
        prompt_shown.recv().await.unwrap();
        let busy = (StatusCode::CONFLICT, "setup_running".to_owned());
        assert_eq!(app.refused().await, busy);
        answer.send(()).unwrap();
        assert_eq!(first.await.unwrap().0, StatusCode::OK);

        // Free again once the first one ended.
        answer.send(()).unwrap();
        assert_eq!(app.post().await.0, StatusCode::OK);
        assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT, NETSTAT, OSASCRIPT]);
    }

    #[tokio::test]
    async fn linux_and_windows_get_setup_not_supported() {
        let dir = Dir::new(true);
        let fake = Fake::new(|program| panic!("{program} ran"));
        for platform in [Some(Platform::LinuxX64), Some(Platform::WindowsX64), None] {
            let daemon = Arc::new(Daemon::new(dir.dir.path().to_path_buf()));
            let setup = Setup::with(fake.clone(), platform, Some(dir.helper()), Some(dir.home()));
            let app = App::new(daemon, setup);
            let refused = (
                StatusCode::NOT_IMPLEMENTED,
                "setup_not_supported".to_owned(),
            );
            assert_eq!(app.refused().await, refused, "{platform:?}");
        }
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn a_request_without_the_token_with_an_origin_or_another_host_runs_nothing() {
        let dir = Dir::new(true);
        let answer = all_done(&["resolver_install", "pf_install"]);
        let fake = Fake::scripted(netstat_free(), move || ok(&answer));
        let daemon = Arc::new(Daemon::new(dir.dir.path().to_path_buf()));
        let app = App::new(daemon, dir.setup(&fake));
        let host = daemon_authority(PORT);
        let other_host = daemon_authority(PORT + 1);
        let localhost = format!("localhost:{PORT}");
        let wrong = Token::generate().unwrap().bearer_header();
        let bearer = app.bearer.clone();
        let unauthorized = (StatusCode::UNAUTHORIZED, "unauthorized".to_owned());
        let cases = [
            (
                "no token",
                vec![("host", host.as_str())],
                unauthorized.clone(),
            ),
            (
                "a wrong token",
                vec![("host", host.as_str()), ("authorization", wrong.as_str())],
                unauthorized,
            ),
            (
                "an Origin header",
                vec![
                    ("host", host.as_str()),
                    ("authorization", bearer.as_str()),
                    ("origin", "http://evil.test"),
                ],
                (StatusCode::FORBIDDEN, "origin_rejected".to_owned()),
            ),
            (
                "another port in Host",
                vec![
                    ("host", other_host.as_str()),
                    ("authorization", bearer.as_str()),
                ],
                (StatusCode::FORBIDDEN, "host_rejected".to_owned()),
            ),
            (
                "localhost in Host",
                vec![
                    ("host", localhost.as_str()),
                    ("authorization", bearer.as_str()),
                ],
                (StatusCode::FORBIDDEN, "host_rejected".to_owned()),
            ),
        ];
        for (name, headers, refused) in cases {
            assert_eq!(app.send(&headers).await, refused, "{name}");
            assert!(fake.calls().is_empty(), "{name}: {:?}", fake.calls());
        }
        // The same route with the right headers runs: the refusals above
        // came from the auth layer.
        assert_eq!(app.post().await.0, StatusCode::OK);
        assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT, SECURITY, SECURITY]);
    }

    #[test]
    fn a_relative_program_is_refused() {
        let stop = CancellationToken::new();
        let err = RealRunner.run("echo", &[], Duration::from_secs(10), &stop);
        assert_eq!(err.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    /// [`RealRunner`] with real programs: only `/bin/sleep`, `/bin/echo`, and
    /// `/usr/bin/false`, which change nothing.
    #[cfg(unix)]
    mod real_runner {
        use super::*;
        use std::os::unix::process::ExitStatusExt;

        fn args(args: &[&str]) -> Vec<OsString> {
            args.iter().map(OsString::from).collect()
        }

        #[test]
        fn a_program_past_its_limit_is_killed_and_reaped() {
            let stop = CancellationToken::new();
            let limit = Duration::from_millis(100);
            let started = Instant::now();
            let err = RealRunner.run("/bin/sleep", &args(&["5"]), limit, &stop);
            assert_eq!(err.unwrap_err().kind(), io::ErrorKind::TimedOut);
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "{:?}",
                started.elapsed()
            );

            // The same wait on a child the test holds, to see it is gone.
            let mut child = Command::new("/bin/sleep")
                .arg("5")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let err = finish(&mut child, limit, &stop).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::TimedOut);
            // Reaped (its status is known) and ended by SIGKILL.
            let status = child.try_wait().unwrap().expect("the child was reaped");
            assert_eq!(status.signal(), Some(9));
        }

        #[test]
        fn a_stop_before_the_call_starts_nothing() {
            let stop = CancellationToken::new();
            stop.cancel();
            let started = Instant::now();
            let limit = Duration::from_secs(10);
            let err = RealRunner.run("/bin/sleep", &args(&["5"]), limit, &stop);
            assert_eq!(err.unwrap_err().kind(), io::ErrorKind::Interrupted);
            assert!(started.elapsed() < Duration::from_secs(1));
            // Not `NotFound`: the stop is seen before any start.
            let err = RealRunner.run("/marsa-test/no-such-program", &[], limit, &stop);
            assert_eq!(err.unwrap_err().kind(), io::ErrorKind::Interrupted);
        }

        #[test]
        fn a_stop_during_the_call_ends_it() {
            let stop = CancellationToken::new();
            let canceller = thread::spawn({
                let stop = stop.clone();
                move || {
                    thread::sleep(Duration::from_millis(100));
                    stop.cancel();
                }
            });
            let started = Instant::now();
            let limit = Duration::from_secs(10);
            let err = RealRunner.run("/bin/sleep", &args(&["5"]), limit, &stop);
            assert_eq!(err.unwrap_err().kind(), io::ErrorKind::Interrupted);
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "{:?}",
                started.elapsed()
            );
            canceller.join().unwrap();
        }

        #[test]
        fn output_and_exit_code_come_back() {
            let stop = CancellationToken::new();
            let limit = Duration::from_secs(10);
            let echo = RealRunner.run("/bin/echo", &args(&["hello", "marsa"]), limit, &stop);
            let want = RunOutput {
                code: Some(0),
                stdout: b"hello marsa\n".to_vec(),
                stderr: Vec::new(),
            };
            assert_eq!(echo.unwrap(), want);
            let failed = RealRunner.run("/usr/bin/false", &[], limit, &stop).unwrap();
            assert_eq!(failed.code, Some(1));
            assert!(!failed.success());
        }
    }
}
