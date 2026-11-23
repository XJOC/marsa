//! `POST /v1/uninstall`: take Marsa out of the system.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `POST /v1/jobs/uninstall` | `202 JobAccepted`; the end is a `job` event with the `UninstallReport`; then the daemon stops |
//! | `POST /v1/uninstall` | The same job, answered with its `UninstallReport` when it ends (clients before ADR 0026); then the daemon stops |
//!
//! The steps, on a blocking thread, in this order:
//!
//! 1. **Preflight**, as for setup but without its folder check: a
//!    `marsa-helper` that is missing or that another user could change ends
//!    the request before any prompt (`uninstall_helper_missing`,
//!    `uninstall_helper_unsafe`). A helper inside `~/Desktop`, `~/Documents`,
//!    or `~/Downloads` runs: root can run it from there, and `uninstall_all`
//!    does not read the binary, which is what fails there.
//! 2. **Helper:** `uninstall_all` behind the one admin prompt of ADR 0012:
//!    the `pf` redirect and its boot job, `/etc/resolver/test`, a trust
//!    added as root before ADR 0015, then `/Library/Application
//!    Support/Marsa` if that left it empty. A prompt cancelled, refused, or
//!    left open, or an answer that cannot be used, ends the request here:
//!    nothing else changes, and the daemon keeps running.
//! 3. **User trust:** `user_trust::remove`, as the user, from the record of
//!    the trusted certificate (ADR 0015), used only if it is Marsa's
//!    certificate in the login keychain or Caddy's root; otherwise
//!    `trust_record_unusable`. macOS may show its dialog.
//! 4. **Login file:** the file of that starts the tray icon.
//! 5. **Answer, then stop:** once the helper ran, and the job's last event
//!    is out, the daemon cancels its own shutdown token. The server's
//!    graceful stop lets the old route's answer finish, as for `POST
//!    /v1/shutdown`, then stops Caddy, the PHP pools, and the DNS
//!    server. The CLI waits for that.
//!
//! A failed step 3 or 4 is a failed result, not an error: the other steps
//! still ran, and running the uninstall again is safe, since each step is
//! `unchanged` once done. Each step's result goes to the log at `info`, so
//! `logs/marsad.log` holds the whole report even if the CLI stopped waiting
//! (Ctrl+C). The data folder is the CLI's question, never the daemon's.
//!
//! On Windows (ADR 0018) the steps are fewer: the preflight,
//! then the marker of [`hosts_sync`] goes, so that no sites pass writes
//! the hosts block back, then `marsa-helper uninstall` behind the UAC
//! prompt (the hosts lines, the CA in the machine's Root store, the helper
//! service and its folder), then the login file, then the stop. A helper
//! that did not remove everything keeps its service running, so the marker
//! comes back. There is no trust for the user to take away.
//!
//! On Linux (ADR 0014) the helper's `uninstall_all` runs behind the one
//! pkexec prompt: the sysctl of ports 80 and 443, the `marsa0` link, the
//! hosts lines, and the CA in the system store. Then, once the helper ran,
//! the marker of the hosts way goes if `hosts_remove` succeeded, the daemon
//! deletes this data folder's roots from the user's NSS databases
//! (`nss_remove`, [`nss::remove`]), and the login file goes, where Linux
//! has one. Then the stop, as on macOS.
//!
//! One request at a time, shared with setup ([`Setup`]): 409
//! `uninstall_running` or `setup_running`.

use marsa_core::codes;
use marsa_core::types::OpName;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use marsa_core::caddy::CaddyPaths;
use marsa_core::helper::{HelperOp, OpResult};
use marsa_core::recipes::{Os, Platform};
use marsa_core::types::{JobKind, JobOutcome, OpStatus, UninstallReport};
use tokio_util::sync::CancellationToken;

use super::http;
use super::{
    CHECK_LIMIT, Flow, PIPE_GRACE, PROMPT_LIMIT, Running, Setup, linux_dns, nss, user_trust,
};
use crate::daemon::autostart::Autostart;
use crate::daemon::http::routes::HttpError;
use crate::daemon::jobs;
use crate::daemon::sites::hosts_sync;
use crate::daemon::state::Daemon;

/// The longest an uninstall takes, when every program runs to its limit:
/// `osascript`, then `security remove-trusted-cert` and `delete-certificate`
/// at `PROMPT_LIMIT` each (both may show a dialog), `find-certificate` at
/// [`CHECK_LIMIT`], and [`PIPE_GRACE`] after each of these four. Linux, with
/// one `pkexec` prompt and the NSS step, stays under it. A client of the old
/// `POST /v1/uninstall` has to wait longer: it waits 40 minutes, and a test
/// holds the two together.
pub const SLOWEST_UNINSTALL_ALL: Duration = Duration::from_secs(
    3 * PROMPT_LIMIT.as_secs() + CHECK_LIMIT.as_secs() + 4 * PIPE_GRACE.as_secs(),
);

/// The routes on `setup`, the same one the setup routes get, for
/// `http::routes::router` to merge before its fallbacks: `POST /v1/jobs/uninstall`,
/// and its old waiting form `POST /v1/uninstall` (ADR 0026).
pub fn routes(setup: Arc<Setup>) -> Router<Arc<Daemon>> {
    let for_job = Arc::clone(&setup);
    let job = move |State(daemon): State<Arc<Daemon>>| {
        let setup = Arc::clone(&for_job);
        async move { Ok::<_, HttpError>(jobs::accepted(begin(&daemon, setup)?)) }
    };
    let wait = move |State(daemon): State<Arc<Daemon>>| {
        let setup = Arc::clone(&setup);
        async move {
            let (_, ended) = begin(&daemon, setup)?;
            jobs::answer(ended, "uninstall").await
        }
    };
    Router::new()
        .route("/v1/jobs/uninstall", post(job))
        .route("/v1/uninstall", post(wait))
}

/// Starts the uninstall job: checks the system, takes the one-at-a-time
/// flag, and answers the job's id at once ([`jobs::start`]). Once the last
/// event is out, a finished helper stops the daemon.
fn begin(
    daemon: &Arc<Daemon>,
    setup: Arc<Setup>,
) -> Result<(u64, jobs::Ended<UninstallReport>), HttpError> {
    let platform = setup
        .platform()
        .filter(|platform| platform.os() != Os::Any)
        .ok_or_else(|| {
            let message = "the uninstall runs on macOS, Linux, and Windows only";
            HttpError::new(
                StatusCode::NOT_IMPLEMENTED,
                codes::UNINSTALL_NOT_SUPPORTED,
                message,
            )
        })?;
    let running = Running::take(&setup, Flow::Uninstall).map_err(Flow::busy)?;
    tracing::info!("uninstall started");
    let work = work(Arc::clone(daemon), setup, platform, running);
    Ok(jobs::start(
        daemon,
        JobKind::Uninstall,
        "uninstall",
        JobOutcome::Uninstall,
        work,
        |daemon, report| {
            if report.stopping {
                // After the job's last event, so a reader gets `done` and
                // then `shutting_down`. The daemon stops even if the client
                // went away. The server stops its processes first, then lets
                // open requests finish, so the old route's answer still goes
                // out.
                tracing::info!("Marsa was uninstalled; the daemon stops");
                daemon.shutdown().cancel();
            }
        },
    ))
}

async fn work(
    daemon: Arc<Daemon>,
    setup: Arc<Setup>,
    platform: Platform,
    running: Running,
) -> Result<UninstallReport, HttpError> {
    http::blocking("uninstall", move || {
        // Moved in, so no setup starts until this one ended, even if the
        // client went away.
        let _running = running;
        let report = run(
            &setup,
            daemon.data_dir(),
            daemon.autostart(),
            platform,
            daemon.shutdown(),
        );
        // The whole report goes to `logs/marsad.log`: a CLI stopped with
        // Ctrl+C sends the user there.
        for step in &report.results {
            tracing::info!(
                op = %step.op,
                status = ?step.status,
                code = step.code.as_deref(),
                "uninstall step"
            );
        }
        tracing::info!(
            results = report.results.len(),
            error = report.error.as_ref().map(|error| error.code.as_str()),
            stopping = report.stopping,
            "uninstall finished"
        );
        Ok(report)
    })
    .await
}

/// The uninstall itself. Blocking: it runs programs and deletes files. The
/// steps after the helper run only once the helper ran: a prompt cancelled
/// or refused leaves everything as it was.
pub fn run(
    setup: &Setup,
    data_dir: &Path,
    autostart: &Autostart,
    platform: Platform,
    stop: &CancellationToken,
) -> UninstallReport {
    let mut report = UninstallReport::default();
    if platform.os() == Os::Windows {
        return run_windows(setup, data_dir, autostart, stop);
    }
    match setup.launch(
        Flow::Uninstall,
        vec![HelperOp::UninstallAll],
        platform,
        stop,
    ) {
        Ok(results) => report.results = results,
        Err(error) => {
            tracing::warn!(code = %error.code, "the helper did not run; nothing else changes");
            report.error = Some(error);
            return report;
        }
    }
    if platform.os() == Os::Linux {
        after_the_helper_on_linux(&mut report, setup, data_dir, autostart, stop);
    } else {
        report.results.push(untrust_for_user(setup, data_dir, stop));
        report.results.push(remove_login_file(autostart));
    }
    report.stopping = true;
    report
}

/// Linux, once the helper ran (see the module doc).
fn after_the_helper_on_linux(
    report: &mut UninstallReport,
    setup: &Setup,
    data_dir: &Path,
    autostart: &Autostart,
    stop: &CancellationToken,
) {
    let hosts_gone = report.results.iter().any(|result| {
        result.op == OpName::HostsRemove
            && matches!(result.status, OpStatus::Done | OpStatus::Unchanged)
    });
    // The block is gone, so no sites pass may write it back. A failed
    // removal keeps the marker, and with it `marsa doctor`'s hosts check.
    if hosts_gone {
        remove_marker(&linux_dns::marker(data_dir));
    }
    let files = user_trust::TrustFiles::in_dir(data_dir);
    report.results.push(match setup.home() {
        Some(home) => nss::remove(setup.runner(), home, &files, stop),
        None => {
            tracing::warn!("the home folder is not known, so the NSS databases cannot be found");
            let message = "the home folder of the current user is not known";
            user_trust::failed(OpName::NssRemove, codes::NSS_REMOVE_FAILED, message)
        }
    });
    if let Some(result) = remove_login_file_if_any(autostart) {
        report.results.push(result);
    }
}

/// Deletes a marker file; one that is not there is fine.
fn remove_marker(marker: &Path) {
    match std::fs::remove_file(marker) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::warn!(path = %marker.display(), error = %err, "cannot delete the marker");
        }
    }
}

/// The uninstall on Windows (see the module doc).
fn run_windows(
    setup: &Setup,
    data_dir: &Path,
    autostart: &Autostart,
    stop: &CancellationToken,
) -> UninstallReport {
    let mut report = UninstallReport::default();
    let result = setup.uninstall_windows(stop);
    // The marker of `hosts_sync` goes only once the helper removed it all,
    // service included. After any other outcome the service may still be
    // installed (and start again), so the daemon keeps sending the names; a
    // pipe with no service behind it only costs a retry now and then. The
    // helper stops the service before it removes anything, so no pass
    // writes the block back meanwhile.
    if result.is_ok() {
        remove_marker(&data_dir.join(hosts_sync::MARKER));
    }
    match result {
        Ok(result) => report.results.push(result),
        Err(error) => {
            tracing::warn!(code = %error.code, "the helper did not remove everything; nothing else changes");
            report.error = Some(error);
            return report;
        }
    }
    // A system with no login file of Marsa's (Windows, for now) skips the
    // step.
    if let Some(result) = remove_login_file_if_any(autostart) {
        report.results.push(result);
    }
    report.stopping = true;
    report
}

/// The login file, unless this system has none (`autostart_unsupported`).
fn remove_login_file_if_any(autostart: &Autostart) -> Option<OpResult> {
    let result = remove_login_file(autostart);
    (result.code.as_deref() != Some(codes::AUTOSTART_UNSUPPORTED)).then_some(result)
}

/// The current user's trust in Marsa's CA (ADR 0015), from the record in
/// `data_dir`, or `trust_remove_failed` when the home folder, and so the
/// login keychain, is not known.
fn untrust_for_user(setup: &Setup, data_dir: &Path, stop: &CancellationToken) -> OpResult {
    let ca_root = CaddyPaths::in_dir(data_dir).ca_root();
    let files = user_trust::TrustFiles::in_dir(data_dir);
    match setup.home() {
        Some(home) => user_trust::remove(setup.runner(), home, &ca_root, &files, stop),
        None => {
            tracing::warn!("the home folder is not known, so the login keychain cannot be found");
            let message = "the home folder of the current user is not known, so the login \
                           keychain cannot be found";
            user_trust::failed(OpName::UserTrustRemove, codes::TRUST_REMOVE_FAILED, message)
        }
    }
}

/// The login file of: `done` if it was there, `unchanged` if
/// not, or failed with the code of the autostart route.
fn remove_login_file(autostart: &Autostart) -> OpResult {
    let (status, code, message) = match autostart.remove_for_uninstall() {
        Ok(true) => (OpStatus::Done, None, None),
        Ok(false) => (OpStatus::Unchanged, None, None),
        Err(err) => (
            OpStatus::Failed,
            Some(err.code.to_owned()),
            Some(err.message),
        ),
    };
    OpResult {
        op: OpName::AutostartRemove,
        status,
        code,
        message,
    }
}

#[cfg(test)]
mod tests;
