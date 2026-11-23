//! `POST /v1/setup`: the first setup on macOS and Linux (Windows has its own
//! helper service, [`windows`], ADR 0018). On macOS the user may be asked
//! twice: for an administrator password (the helper), then to approve the
//! trust in Marsa's certificate authority for this user. On Linux once, for
//! the password (`pkexec`).
//!
//! | Route | Answer |
//! | --- | --- |
//! | `POST /v1/setup` | `SetupReport`, also sent as `setup_finished` on `/v1/events` |
//!
//! One setup at a time: a second request gets 409 `setup_running`, and one
//! during an uninstall gets 409 `uninstall_running`. `POST /v1/uninstall`
//! (`uninstall_api.rs`) shares [`Setup`]: the same runner, the same helper
//! file check and prompt ([`Flow`] gives each its own error codes), and the
//! same flag.
//!
//! Each job has its own file: `ca` (the root certificate), `ports` (who
//! holds 80 and 443), `helper` and `osascript` (the administrator
//! prompt), `steps` (what each system runs), `runner` (every program
//! goes through it).

mod ca;
pub mod doctor;
mod helper;
pub mod hosts;
pub mod linux_dns;
pub mod nss;
pub mod one_at_a_time;
mod osascript;
pub mod pkexec;
#[cfg(test)]
mod ports;
mod steps;
#[cfg(test)]
pub(super) mod test_support;
pub mod uninstall;
pub mod user_trust;
pub mod windows;

use marsa_core::codes;
use std::collections::BTreeSet;
use std::env::consts::EXE_SUFFIX;
use std::fmt;
use std::fs;
use std::num::NonZeroU8;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use marsa_core::recipes::{Os, Platform};
use marsa_core::types::{
    ApiError, Event, JobAccepted, JobEvent, JobKind, JobOutcome, JobStep, JobUpdate, SetupReport,
    Site, SiteList,
};
// The test modules of this folder reach these through `super::*`.
#[cfg(test)]
use marsa_core::types::OpName;
use tokio::runtime::Handle;
use tokio::sync::oneshot;

use self::one_at_a_time::{OneAtATime, Turn};
use super::http;
use super::http::routes::HttpError;
use super::sites::hosts_sync;
use super::state::Daemon;
use marsa_core::setup::skip;

pub use self::ca::{CA_WAIT, CaddyStart, StartCaddy};
use self::ca::{DaemonCaddy, PEM_BEGIN, read_ca};
pub(super) use self::helper::PROMPT_LIMIT;
#[cfg(unix)]
use self::helper::current_uid;
use self::helper::{MAX_ANSWER, parse_answer, results_of};
use self::osascript::not_finished;
use self::steps::low_ports_set;
pub(super) use marsa_core::platform::ports::CHECK_LIMIT;
use marsa_core::platform::ports::MAX_PS;
pub(super) use marsa_core::platform::runner::PIPE_GRACE;
pub use marsa_core::platform::runner::{RealRunner, RunOutput, Runner};

/// The longest a setup takes, when every step runs to its limit: Caddy's
/// start and its CA root at [`CA_WAIT`], `netstat`, [`MAX_PS`] `ps` calls,
/// and `security verify-cert` at [`CHECK_LIMIT`] each, `osascript` and
/// `security add-trusted-cert` at `PROMPT_LIMIT` each, and [`PIPE_GRACE`]
/// after each of these programs. Linux stays under it: up to four
/// `systemctl` checks, two `pkexec` prompts when the helper chooses the
/// hosts file ([`linux_dns`]), and the NSS step at [`nss::NSS_BUDGET`]. A client has to wait longer:
/// `marsa_core::client::SETUP_TIMEOUT` does, and a test holds the two
/// together.
pub const SLOWEST_SETUP: Duration = Duration::from_secs(
    CA_WAIT.as_secs()
        + (2 + MAX_PS as u64) * CHECK_LIMIT.as_secs()
        + 2 * PROMPT_LIMIT.as_secs()
        + (4 + MAX_PS as u64) * PIPE_GRACE.as_secs(),
);

/// The routes on `setup`, for `api::router` to merge before its fallbacks:
/// `POST /v1/jobs/setup`, its old waiting form `POST /v1/setup`, and the
/// hosts routes of [`hosts`].
/// The uninstall route gets the same [`Setup`], so the two never run at once.
pub fn routes(setup: Arc<Setup>) -> Router<Arc<Daemon>> {
    let hosts = hosts::routes(Arc::clone(&setup));
    let for_job = Arc::clone(&setup);
    let job = move |State(daemon): State<Arc<Daemon>>| {
        let setup = Arc::clone(&for_job);
        async move {
            let (id, _) = begin(daemon, setup).await?;
            Ok::<_, HttpError>((StatusCode::ACCEPTED, Json(JobAccepted { id })))
        }
    };
    // The old route (API v1 keeps it, ADR 0026): the same job, and the
    // answer waits for its end inside the daemon.
    let wait = move |State(daemon): State<Arc<Daemon>>| {
        let setup = Arc::clone(&setup);
        async move {
            let (_, ended) = begin(daemon, setup).await?;
            let report = ended.await.map_err(|_| job_gone())??;
            Ok::<_, HttpError>(Json(report))
        }
    };
    Router::new()
        .route("/v1/jobs/setup", post(job))
        .route("/v1/setup", post(wait))
        .merge(hosts)
}

/// The answer when the task of a job ended without a result: it cannot
/// happen unless the daemon's runtime is going away.
fn job_gone() -> HttpError {
    HttpError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        codes::INTERNAL_ERROR,
        "the setup job did not end",
    )
}

/// The routes with the given [`Setup`]: tests pass a fake [`Runner`].
pub fn routes_with(setup: Setup) -> Router<Arc<Daemon>> {
    routes(Arc::new(setup))
}

/// Which request runs the helper: the setup, or the uninstall of.
/// Each has its own error codes, `setup_*` and `uninstall_*`, and its
/// messages name its own command. It also names the request that runs now,
/// for the 409 of the other one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Setup,
    Uninstall,
}

/// Why a helper request ended without its result, for [`Flow::code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Cancelled,
    NotAuthorized,
    PromptFailed,
    TimedOut,
    Interrupted,
    PkexecMissing,
    HelperFailed,
    HelperRejected,
    HelperMissing,
    HelperUnsafe,
    HelperPartial,
    HelperRunning,
    HelperPipeFailed,
    HelperInstallFailed,
    HelperInstallRunning,
    HelperNotTheService,
    HelperBusy,
    HelperNoAnswer,
}

impl Flow {
    /// The code of `reason` in this flow: `setup_*` or `uninstall_*`.
    pub fn code(self, reason: Reason) -> &'static str {
        match (self, reason) {
            (Self::Setup, Reason::Cancelled) => codes::SETUP_CANCELLED,
            (Self::Uninstall, Reason::Cancelled) => codes::UNINSTALL_CANCELLED,
            (Self::Setup, Reason::NotAuthorized) => codes::SETUP_NOT_AUTHORIZED,
            (Self::Uninstall, Reason::NotAuthorized) => codes::UNINSTALL_NOT_AUTHORIZED,
            (Self::Setup, Reason::PromptFailed) => codes::SETUP_PROMPT_FAILED,
            (Self::Uninstall, Reason::PromptFailed) => codes::UNINSTALL_PROMPT_FAILED,
            (Self::Setup, Reason::TimedOut) => codes::SETUP_TIMED_OUT,
            (Self::Uninstall, Reason::TimedOut) => codes::UNINSTALL_TIMED_OUT,
            (Self::Setup, Reason::Interrupted) => codes::SETUP_INTERRUPTED,
            (Self::Uninstall, Reason::Interrupted) => codes::UNINSTALL_INTERRUPTED,
            (Self::Setup, Reason::PkexecMissing) => codes::SETUP_PKEXEC_MISSING,
            (Self::Uninstall, Reason::PkexecMissing) => codes::UNINSTALL_PKEXEC_MISSING,
            (Self::Setup, Reason::HelperFailed) => codes::SETUP_HELPER_FAILED,
            (Self::Uninstall, Reason::HelperFailed) => codes::UNINSTALL_HELPER_FAILED,
            (Self::Setup, Reason::HelperRejected) => codes::SETUP_HELPER_REJECTED,
            (Self::Uninstall, Reason::HelperRejected) => codes::UNINSTALL_HELPER_REJECTED,
            (Self::Setup, Reason::HelperMissing) => codes::SETUP_HELPER_MISSING,
            (Self::Uninstall, Reason::HelperMissing) => codes::UNINSTALL_HELPER_MISSING,
            (Self::Setup, Reason::HelperUnsafe) => codes::SETUP_HELPER_UNSAFE,
            (Self::Uninstall, Reason::HelperUnsafe) => codes::UNINSTALL_HELPER_UNSAFE,
            (Self::Setup, Reason::HelperPartial) => codes::SETUP_HELPER_PARTIAL,
            (Self::Uninstall, Reason::HelperPartial) => codes::UNINSTALL_HELPER_PARTIAL,
            (Self::Setup, Reason::HelperRunning) => codes::SETUP_HELPER_RUNNING,
            (Self::Uninstall, Reason::HelperRunning) => codes::UNINSTALL_HELPER_RUNNING,
            (Self::Setup, Reason::HelperPipeFailed) => codes::SETUP_HELPER_PIPE_FAILED,
            (Self::Uninstall, Reason::HelperPipeFailed) => codes::UNINSTALL_HELPER_PIPE_FAILED,
            (Self::Setup, Reason::HelperInstallFailed) => codes::SETUP_HELPER_INSTALL_FAILED,
            (Self::Uninstall, Reason::HelperInstallFailed) => {
                codes::UNINSTALL_HELPER_INSTALL_FAILED
            }
            (Self::Setup, Reason::HelperInstallRunning) => codes::SETUP_HELPER_INSTALL_RUNNING,
            (Self::Uninstall, Reason::HelperInstallRunning) => {
                codes::UNINSTALL_HELPER_INSTALL_RUNNING
            }
            (Self::Setup, Reason::HelperNotTheService) => codes::SETUP_HELPER_NOT_THE_SERVICE,
            (Self::Uninstall, Reason::HelperNotTheService) => {
                codes::UNINSTALL_HELPER_NOT_THE_SERVICE
            }
            (Self::Setup, Reason::HelperBusy) => codes::SETUP_HELPER_BUSY,
            (Self::Uninstall, Reason::HelperBusy) => codes::UNINSTALL_HELPER_BUSY,
            (Self::Setup, Reason::HelperNoAnswer) => codes::SETUP_HELPER_NO_ANSWER,
            (Self::Uninstall, Reason::HelperNoAnswer) => codes::UNINSTALL_HELPER_NO_ANSWER,
        }
    }

    /// The request in an English message: "setup", "the uninstall".
    fn noun(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Uninstall => "the uninstall",
        }
    }

    /// Its value in [`Setup`]'s guard.
    fn flag(self) -> NonZeroU8 {
        match self {
            Self::Setup => NonZeroU8::MIN,
            Self::Uninstall => NonZeroU8::new(2).expect("2 is not zero"),
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
            Self::Setup => codes::SETUP_RUNNING,
            Self::Uninstall => codes::UNINSTALL_RUNNING,
        };
        HttpError::new(StatusCode::CONFLICT, code, message)
    }
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
    /// How setup starts Caddy for its CA. `None`, the default: the daemon's
    /// own Caddy ([`DaemonCaddy`]). Tests put a fake here.
    caddy: Option<Arc<dyn StartCaddy>>,
    /// The helper service on Windows (ADR 0018): the daemon's own there,
    /// `None` elsewhere. Tests put a fake here.
    windows: Option<Arc<dyn windows::Service>>,
    /// Linux: the hosts file whose Marsa block setup reads, never writes
    /// ([`linux_dns::read_block`]). [`linux_dns::HOSTS_FILE`] for the
    /// daemon; `None`, as if it had no block, until a test sets one.
    hosts_file: Option<PathBuf>,
    /// Linux: whether setup adds the CA root to the user's NSS databases
    /// ([`nss`]). On for the daemon; off until a test turns it on, so the
    /// other tests run no `certutil`.
    nss: bool,
    /// How long setup waits for Caddy and its CA root: [`CA_WAIT`], shorter
    /// in tests.
    ca_wait: Duration,
    /// Free, or held by the [`Flow`] that runs now.
    running: Arc<OneAtATime>,
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
        .with_hosts_file(PathBuf::from(linux_dns::HOSTS_FILE))
        .with_nss()
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
            caddy: None,
            windows: default_windows_service(),
            hosts_file: None,
            nss: false,
            ca_wait: CA_WAIT,
            running: Arc::new(OneAtATime::new()),
        }
    }

    /// Linux: the CA root goes into the user's NSS databases too ([`nss`]).
    #[must_use]
    pub fn with_nss(mut self) -> Self {
        self.nss = true;
        self
    }

    /// For tests: `service` stands for the Windows helper service.
    #[must_use]
    pub fn with_windows(mut self, service: Arc<dyn windows::Service>) -> Self {
        self.windows = Some(service);
        self
    }

    /// The hosts file setup reads on Linux: [`linux_dns::HOSTS_FILE`] for
    /// the daemon, a file of their own for tests.
    #[must_use]
    pub fn with_hosts_file(mut self, path: PathBuf) -> Self {
        self.hosts_file = Some(path);
        self
    }

    /// For tests: `caddy` starts Caddy for the CA step, not the daemon's own.
    #[must_use]
    pub fn with_caddy(mut self, caddy: Arc<dyn StartCaddy>) -> Self {
        self.caddy = Some(caddy);
        self
    }

    /// The program runner, for the uninstall's own steps.
    pub(super) fn runner(&self) -> &dyn Runner {
        self.runner.as_ref()
    }

    /// This system, if Marsa has a build for it.
    pub(super) fn platform(&self) -> Option<Platform> {
        self.platform
    }

    /// The hosts file whose Marsa block is read on Linux, if any.
    pub(super) fn hosts_file(&self) -> Option<&Path> {
        self.hosts_file.as_deref()
    }

    /// The home folder of the user `marsad` runs as, if known.
    pub(super) fn home(&self) -> Option<&Path> {
        self.home.as_deref()
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
            .field("running", &self.running.holder())
            .finish_non_exhaustive()
    }
}

/// The right to run the one setup or uninstall. Dropping it lets the next
/// one start, also if the request panics.
pub(super) struct Running {
    _turn: Turn,
}

impl Running {
    /// The right to run `flow`, or the [`Flow`] that runs now.
    pub(super) fn take(setup: &Arc<Setup>, flow: Flow) -> Result<Self, Flow> {
        OneAtATime::take(&setup.running, flow.flag())
            .map(|turn| Self { _turn: turn })
            .map_err(|now| {
                if now == Flow::Uninstall.flag().get() {
                    Flow::Uninstall
                } else {
                    Flow::Setup
                }
            })
    }
}

/// What the old waiting route answers, sent after the job's last event.
type Ended = oneshot::Receiver<Result<SetupReport, HttpError>>;

/// Starts the setup job: checks the system, takes the one-at-a-time flag,
/// and answers the job's id at once. The work runs in its own task, so a
/// client that goes away loses nothing.
async fn begin(daemon: Arc<Daemon>, setup: Arc<Setup>) -> Result<(u64, Ended), HttpError> {
    let platform = setup
        .platform
        .filter(|platform| platform.os() != Os::Any)
        .ok_or_else(|| {
            let message = "setup runs on macOS, Linux, and Windows only";
            HttpError::new(
                StatusCode::NOT_IMPLEMENTED,
                codes::SETUP_NOT_SUPPORTED,
                message,
            )
        })?;
    let running = Running::take(&setup, Flow::Setup).map_err(Flow::busy)?;
    let id = daemon.jobs().next_id();
    let event = move |update| JobEvent {
        id,
        kind: JobKind::Setup,
        update,
    };
    daemon.publish_job(event(JobUpdate::Running {
        step: JobStep::Started,
    }));
    let (send, ended) = oneshot::channel();
    tokio::spawn(async move {
        let work = tokio::spawn(run(Arc::clone(&daemon), setup, platform, running));
        let result = work
            .await
            .unwrap_or_else(|err| Err(http::job_failed("setup", &err)));
        // The flag is free by now: the work held it. A client that reads the
        // last event can start the next setup at once.
        let update = match &result {
            Ok(report) => JobUpdate::Done(JobOutcome::Setup(report.clone())),
            Err(err) => JobUpdate::Failed(ApiError {
                code: err.code.to_owned(),
                message: err.message.clone(),
            }),
        };
        daemon.publish_job(event(update));
        // Nobody waits when the job came from `POST /v1/jobs/setup`.
        let _ = send.send(result);
    });
    Ok((id, ended))
}

async fn run(
    daemon: Arc<Daemon>,
    setup: Arc<Setup>,
    platform: Platform,
    running: Running,
) -> Result<SetupReport, HttpError> {
    tracing::info!("setup started");
    // The hosts file on Linux and Windows needs them; elsewhere the list is
    // not read.
    let names = if marsa_core::setup::needs(platform.os()).site_names {
        host_names(&daemon).await
    } else {
        Some(Vec::new())
    };
    let dns_down = daemon.dns().not_serving();
    let runtime = Handle::current();
    let report = http::blocking("setup", move || {
        // Moved in, so the next setup waits for this one to end even if the
        // client goes away.
        let _running = running;
        let caddy = daemon.caddy().input;
        let starter: Arc<dyn StartCaddy> = match &setup.caddy {
            Some(fake) => Arc::clone(fake),
            None => Arc::new(DaemonCaddy {
                daemon: Arc::clone(&daemon),
                runtime: runtime.clone(),
            }),
        };
        let mut report = setup.run(
            daemon.data_dir(),
            caddy,
            platform,
            starter.as_ref(),
            names.as_deref(),
            dns_down.as_deref(),
            daemon.shutdown(),
        );
        tracing::info!(
            results = report.results.len(),
            skipped = report.skipped.len(),
            error = report.error.as_ref().map(|error| error.code.as_str()),
            "setup finished"
        );
        // Linux: Caddy may now bind ports 80 and 443 itself. The next config
        // has them (`CaddyManager::ports`), so it goes to Caddy now, before
        // the event and the answer: the sites open on the usual ports as
        // soon as setup ends. Here, not in the handler, so it happens even
        // if the client went away.
        // Not while the daemon stops: the runtime may be going away, and
        // Caddy reads its file when it starts again.
        if platform.os() == Os::Linux && low_ports_set(&report) && !daemon.shutdown().is_cancelled()
        {
            match runtime.block_on(daemon.caddy().reapply(&daemon)) {
                Ok(applied) => {
                    tracing::info!(?applied, "caddy config sent again with ports 80 and 443")
                }
                Err(err) => {
                    tracing::warn!(
                        code = err.code(),
                        error = %err,
                        "caddy did not take the config with ports 80 and 443"
                    );
                    // The system now allows the ports, but Caddy did not
                    // take them: most often another program listens on 80
                    // or 443 (ADR 0014). Setup must not look done.
                    let message = format!("Caddy did not take ports 80 and 443: {}", err.code());
                    report
                        .error
                        .get_or_insert_with(|| failure(codes::SETUP_CADDY_PORTS_FAILED, message));
                }
            }
        }
        // Windows: the service took the request, so this data folder's
        // daemon sends the sites' names after each pass from now on
        // (`hosts_sync`), and a pass now covers a site added during the UAC
        // prompt. Not while the daemon stops.
        if platform.os() == Os::Windows
            && report.error.is_none()
            && !daemon.shutdown().is_cancelled()
        {
            let marker = daemon.data_dir().join(hosts_sync::MARKER);
            match fs::write(&marker, "") {
                Ok(()) => {
                    let daemon = Arc::clone(&daemon);
                    runtime.spawn(async move {
                        // Not run: the sync task stopped, and the next
                        // daemon's first pass sends the names.
                        let _ = daemon.pass_requests().run_pass().await;
                    });
                }
                Err(err) => tracing::warn!(
                    path = %marker.display(),
                    error = %err,
                    "cannot mark the helper service as set up; the hosts file gets no names"
                ),
            }
        }
        // Here, not in the handler: the event goes out even if the client
        // went away.
        daemon.publish(Event::SetupFinished(report.clone()));
        Ok(report)
    })
    .await?;
    Ok(report)
}

/// The host names of the daemon's sites, sorted, each once, from the list
/// `GET /v1/sites` gives: built now if there is none yet. `None` if it
/// cannot be built (`sites.toml` cannot be read, say).
pub(super) async fn host_names(daemon: &Arc<Daemon>) -> Option<Vec<String>> {
    let list = match daemon.sites() {
        Some(list) => list,
        None => match daemon.rebuild_sites().await {
            Ok(list) => list,
            Err(err) => {
                tracing::warn!(
                    code = err.code(),
                    "no site list, so no names for the hosts file"
                );
                return None;
            }
        },
    };
    Some(names_of(&list))
}

/// The host names of the sites of `list`, sorted, each once.
pub(super) fn names_of(list: &SiteList) -> Vec<String> {
    let names: BTreeSet<String> = list.sites.iter().map(Site::host).collect();
    names.into_iter().collect()
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

/// The daemon's helper service on Windows; no other system has one.
fn default_windows_service() -> Option<Arc<dyn windows::Service>> {
    // `RealService` is Windows only: its calls are Win32.
    #[cfg(windows)]
    return Some(Arc::new(windows::RealService::default()));
    #[cfg(not(windows))]
    None
}

/// A code from the helper, safe for a log line and a message.
fn short_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
        .take(64)
        .collect()
}

fn failure(code: &str, message: impl Into<String>) -> ApiError {
    ApiError {
        code: code.to_owned(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::ca::CA_WAIT;
    use super::ca::CaddyStart;
    use super::osascript::OSASCRIPT;
    use super::test_support::*;
    use super::user_trust::SECURITY;
    use super::*;
    use marsa_core::platform::ports::CHECK_LIMIT;
    use marsa_core::platform::ports::MAX_PS;
    use marsa_core::platform::ports::NETSTAT;
    use marsa_core::platform::runner::PIPE_GRACE;

    use crate::daemon::state::Daemon;

    use axum::http::StatusCode;

    use marsa_core::platform::daemon_file::{Token, daemon_authority};

    use marsa_core::recipes::Platform;

    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn the_client_waits_longer_than_the_slowest_setup() {
        use marsa_core::client::SETUP_TIMEOUT;

        // 12 min 10 s of limits, 30 s of them for Caddy and its CA root, and
        // 2 s of grace after each of 12 programs.
        assert_eq!(SLOWEST_SETUP, Duration::from_secs(754));
        // With room for the HTTP round trip and for killing a stalled
        // program.
        let margin = Duration::from_secs(30);
        assert!(
            SETUP_TIMEOUT >= SLOWEST_SETUP + margin,
            "the client waits {SETUP_TIMEOUT:?}, and a setup may take {SLOWEST_SETUP:?}"
        );
        // Windows: the CA, a request, the install after the UAC prompt, and
        // the request again, each to its limit. The prompt itself has none.
        let windows = CA_WAIT + 2 * windows::SEND_LIMIT + windows::INSTALL_LIMIT;
        assert!(
            SETUP_TIMEOUT >= windows + margin,
            "the client waits {SETUP_TIMEOUT:?}, and a Windows setup may take {windows:?}"
        );

        // The sum holds every step a setup waits for. A full run with no CA
        // root yet and nobody on 80 or 443 starts Caddy and runs every
        // program but `ps`, each with its limit, so a new program or wait in
        // the flow fails here until it is in `SLOWEST_SETUP`, which then has
        // to fit the client's wait above.
        let dir = Dir::new(false).with_caddy(CaddyStart::Started, Some(Duration::ZERO));
        let answer = all_done(&[OpName::ResolverInstall, OpName::PfInstall]);
        let fake = Fake::scripted(netstat_free(), move || ok(&answer));
        let report = dir.run(&fake);
        assert_eq!(report.results.len(), 3);
        let calls = fake.calls();
        assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT, SECURITY, SECURITY]);
        let ps = u32::try_from(MAX_PS).unwrap();
        let programs = u32::try_from(calls.len()).unwrap() + ps;
        let limits: Duration = calls.iter().map(|call| call.limit).sum();
        let caddy: Duration = dir.caddy.limits().iter().sum();
        assert_eq!(caddy, CA_WAIT);
        assert_eq!(
            caddy + limits + CHECK_LIMIT * ps + PIPE_GRACE * programs,
            SLOWEST_SETUP
        );
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
    async fn other_systems_get_setup_not_supported() {
        let dir = Dir::new(true);
        let fake = Fake::new(|program| panic!("{program} ran"));
        for platform in [Some(Platform::Any), None] {
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
        let answer = all_done(&[OpName::ResolverInstall, OpName::PfInstall]);
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
}
