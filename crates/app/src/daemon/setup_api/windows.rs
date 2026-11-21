//! Windows: runs the helper's ops through the helper service (ADR 0018).
//! The macOS and Linux twins are the `osascript` line of ADR
//! 0012 and [`pkexec`](super::pkexec).
//!
//! On Windows the helper is the LocalSystem service `MarsaHelper`, which
//! serves this user only, on `\\.\pipe\marsa-helper`, and trusts only the
//! CA named at its install. So setup first sends the request to the
//! service. Only when that cannot work does it run `marsa-helper install`
//! behind the UAC prompt, once, and then send the same request again:
//!
//! | The service answers | Then |
//! | --- | --- |
//! | results | done, no prompt |
//! | `helper_not_running` (not installed, or stopped) | install, send again |
//! | `helper_access_denied`, `client_not_allowed` (installed for another user) | install, send again |
//! | `trust_not_allowed` (installed for another CA) | install, send again |
//! | `helper_not_the_service` (another program holds the name) | `setup_helper_not_the_service`: nothing sent, no prompt |
//! | `helper_busy` | `setup_helper_busy` |
//! | `helper_timed_out`, `helper_no_answer` | `setup_helper_no_answer`: the request may have run |
//! | another pipe failure | `setup_helper_pipe_failed`: nothing sent |
//!
//! The UAC prompt gives back no output, so `install`'s exit code is its
//! answer: 0 installed, anything else `setup_helper_install_failed`; a No
//! at the prompt is `setup_cancelled`. An install still running at
//! [`INSTALL_LIMIT`] is `setup_helper_install_running`, and the next setup
//! shows no prompt while it runs.
//!
//! The daemon's stop ends the flow before each step (`setup_interrupted`),
//! and ends the wait for an answer. It cannot end the UAC prompt, which has
//! no time limit, nor the install after the user's Yes: the daemon waits for
//! them before it exits.

use std::path::Path;
use std::time::Duration;

use marsa_core::helper::{OpResult, OpStatus};
use tokio_util::sync::CancellationToken;

use super::{ApiError, Flow, Reason, failure, parse_answer, results_of};

/// The longest one request to the service may take: a `trust_install` whose
/// `certutil` runs to its limit, and the rollback after it, is about eight
/// minutes.
pub const SEND_LIMIT: Duration = Duration::from_secs(600);
/// The longest `marsa-helper install` may run after the prompt: it may wait
/// up to ten minutes for the service to finish a request and stop.
pub const INSTALL_LIMIT: Duration = Duration::from_secs(900);

/// What `marsa-helper install` gave, behind the UAC prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    Exited(u32),
    /// The user said No.
    Cancelled,
    /// An earlier run still runs (it passed [`INSTALL_LIMIT`]): this one
    /// did not start, so no second prompt showed.
    StillRunning,
    /// This run started and still runs at [`INSTALL_LIMIT`].
    TimedOut,
    /// It did not start, and why.
    Failed(String),
}

/// The Windows calls of setup, behind a trait so that the tests run on
/// every system. `RealService` is the daemon's.
pub trait Service: Send + Sync {
    /// The SID of the user `marsad` runs as.
    fn user_sid(&self) -> Result<String, String>;
    /// Sends `request` to the helper service: its JSON answer, or the stable
    /// code of the failure (`helper_*`, from `marsa_core`'s pipe client).
    /// `stop` ends the wait (`helper_interrupted`). Blocking.
    fn send(&self, request: &[u8], stop: &CancellationToken) -> Result<Vec<u8>, &'static str>;
    /// Runs `helper install <sid> <hash>` behind the UAC prompt, unless an
    /// earlier one still runs. Blocking.
    fn install(&self, helper: &Path, sid: &str, hash: &str) -> Install;
    /// Runs `helper uninstall` behind the UAC prompt, unless an install
    /// still runs: the hosts lines, the CA, the service, and its folder go.
    /// Blocking.
    fn uninstall(&self, helper: &Path) -> Install;
}

/// `marsa uninstall --all` on Windows: `marsa-helper uninstall` behind the
/// UAC prompt, which takes out everything `install` and the service put on
/// the machine. The prompt gives back only the exit code: 0
/// is one `uninstall_all` result, `done` (also when there was nothing left
/// to remove: the code cannot say more).
///
/// # Errors
///
/// - `uninstall_cancelled`: a No at the prompt; nothing changed.
/// - `uninstall_interrupted`: the daemon stops; no prompt showed.
/// - `uninstall_helper_partial`: exit 1, some of it may be left (the hosts
///   lines, the CA, the service, or its folder).
/// - `uninstall_helper_running`: this run, or an earlier one, still runs.
/// - `uninstall_helper_failed`: the helper did not run as asked (it did not
///   start, or its arguments were refused); nothing changed.
pub fn uninstall(
    service: &dyn Service,
    helper: &Path,
    stop: &CancellationToken,
) -> Result<OpResult, ApiError> {
    let flow = Flow::Uninstall;
    interrupted(flow, stop)?;
    let failed = |message: String| failure(flow.code(Reason::HelperFailed), message);
    match service.uninstall(helper) {
        Install::Exited(0) => Ok(OpResult {
            op: "uninstall_all".to_owned(),
            status: OpStatus::Done,
            code: None,
            message: None,
        }),
        Install::Cancelled => {
            tracing::info!("the UAC prompt was declined");
            Err(failure(
                flow.code(Reason::Cancelled),
                "the UAC prompt was declined",
            ))
        }
        // 1: a step failed. Any code but 2 (its arguments refused) may come
        // from a run that stopped partway, a crash say.
        Install::Exited(code) if code != 2 => {
            tracing::warn!(code, "marsa-helper uninstall did not remove everything");
            Err(failure(
                flow.code(Reason::HelperPartial),
                "the helper could not remove everything",
            ))
        }
        Install::Exited(code) => {
            tracing::warn!(code, "marsa-helper uninstall did not run as asked");
            Err(failed(format!(
                "the helper did not run as asked (exit code {code})"
            )))
        }
        Install::StillRunning | Install::TimedOut => {
            tracing::warn!("a run of marsa-helper still goes on; no uninstall now");
            Err(failure(
                flow.code(Reason::HelperRunning),
                "a run of the helper still goes on",
            ))
        }
        Install::Failed(reason) => {
            tracing::warn!(%reason, "cannot start marsa-helper uninstall");
            Err(failed(format!(
                "cannot start the helper's uninstall: {reason}"
            )))
        }
    }
}

/// Sends `request` (the JSON of the ops) to the service, and installs the
/// service first when it must (the table above). `ca_sha256` is the CA that
/// the request trusts, if any: the service is installed for it. Only setup
/// installs; another flow gets an error instead.
///
/// # Errors
///
/// The `ApiError` of `flow`, as in the table above.
pub fn launch(
    flow: Flow,
    service: &dyn Service,
    helper: &Path,
    request: &str,
    ca_sha256: Option<&str>,
    stop: &CancellationToken,
) -> Result<Vec<OpResult>, ApiError> {
    let why = match ask(flow, service, request, stop)? {
        Asked::Results(results) => return Ok(results),
        Asked::Install(why) => why,
    };
    if flow != Flow::Setup {
        let message = format!("the helper service cannot take the request: {why}");
        return Err(failure(flow.code(Reason::HelperPipeFailed), message));
    }
    interrupted(flow, stop)?;
    let install_failed = |message: String| failure(flow.code(Reason::HelperInstallFailed), message);
    tracing::info!(why, "installing the helper service, behind the UAC prompt");
    let sid = service.user_sid().map_err(|reason| {
        tracing::warn!(%reason, "cannot read the SID of this user");
        install_failed("cannot read this user's SID".to_owned())
    })?;
    match service.install(helper, &sid, ca_sha256.unwrap_or("-")) {
        Install::Exited(0) => {}
        Install::Cancelled => {
            tracing::info!("the UAC prompt was declined");
            return Err(failure(
                flow.code(Reason::Cancelled),
                "the UAC prompt was declined",
            ));
        }
        Install::Exited(code) => {
            tracing::warn!(
                code,
                "marsa-helper install failed; Windows' service log says why"
            );
            return Err(install_failed(format!(
                "the helper service was not installed (exit code {code})"
            )));
        }
        Install::StillRunning | Install::TimedOut => {
            tracing::warn!("marsa-helper install still runs; no second prompt");
            return Err(failure(
                flow.code(Reason::HelperInstallRunning),
                "the install of the helper service still runs",
            ));
        }
        Install::Failed(reason) => {
            tracing::warn!(%reason, "cannot start marsa-helper install");
            return Err(install_failed(format!(
                "cannot start the install of the helper service: {reason}"
            )));
        }
    }
    interrupted(flow, stop)?;
    match ask(flow, service, request, stop)? {
        Asked::Results(results) => Ok(results),
        Asked::Install(why) => {
            tracing::warn!(
                why,
                "the helper service refuses the request after its install"
            );
            Err(install_failed(format!(
                "the helper service refuses the request after its install: {why}"
            )))
        }
    }
}

/// `setup_interrupted` once the daemon stops.
fn interrupted(flow: Flow, stop: &CancellationToken) -> Result<(), ApiError> {
    if stop.is_cancelled() {
        return Err(failure(
            flow.code(Reason::Interrupted),
            "the daemon is stopping",
        ));
    }
    Ok(())
}

/// What one request to the service gave.
enum Asked {
    Results(Vec<OpResult>),
    /// The service must be installed (again) first, and why.
    Install(&'static str),
}

fn ask(
    flow: Flow,
    service: &dyn Service,
    request: &str,
    stop: &CancellationToken,
) -> Result<Asked, ApiError> {
    interrupted(flow, stop)?;
    let json = match service.send(request.as_bytes(), stop) {
        Ok(json) => json,
        Err("helper_not_running") => return Ok(Asked::Install("the service is not running")),
        Err("helper_access_denied") => {
            return Ok(Asked::Install("the service's pipe refused this user"));
        }
        Err("helper_interrupted") => {
            return Err(failure(
                flow.code(Reason::Interrupted),
                "the daemon is stopping",
            ));
        }
        Err("helper_not_the_service") => {
            tracing::warn!("another program holds the helper service's pipe name");
            let message = "another program holds the name of the helper service's pipe, so \
                           nothing was sent";
            return Err(failure(flow.code(Reason::HelperNotTheService), message));
        }
        Err("helper_busy") => {
            return Err(failure(
                flow.code(Reason::HelperBusy),
                "the helper service stayed busy with other requests",
            ));
        }
        Err(code @ ("helper_timed_out" | "helper_no_answer")) => {
            tracing::warn!(
                code,
                "the helper service gave no answer; the request may have run"
            );
            return Err(failure(
                flow.code(Reason::HelperNoAnswer),
                "the helper service gave no answer, so the result is not known",
            ));
        }
        // `helper_pipe_failed`, `helper_cannot_check`,
        // `helper_request_too_large`: nothing was sent.
        Err(code) => {
            tracing::warn!(code, "the helper service's pipe failed; nothing was sent");
            let message = format!("the helper service's pipe failed: {code}");
            return Err(failure(flow.code(Reason::HelperPipeFailed), message));
        }
    };
    let Some(answer) = parse_answer(json.trim_ascii()) else {
        tracing::warn!(
            bytes = json.len(),
            "the helper service's answer cannot be read"
        );
        return Err(failure(
            flow.code(Reason::HelperFailed),
            "the helper service's answer cannot be read",
        ));
    };
    match answer.error.as_ref().map(|refused| refused.code.as_str()) {
        Some("client_not_allowed") => Ok(Asked::Install("the service serves another user")),
        Some("trust_not_allowed") => Ok(Asked::Install("the service trusts another CA")),
        _ => results_of(flow, answer, None).map(Asked::Results),
    }
}

/// The daemon's [`Service`]: the pipe client and the UAC prompt of
/// `marsa_core::platform::windows`.
// Windows only: its calls are Win32.
#[cfg(windows)]
#[derive(Default)]
pub struct RealService {
    /// An install still running past [`INSTALL_LIMIT`]: the next setup asks
    /// no second prompt while it runs.
    running: std::sync::Mutex<Option<marsa_core::platform::windows::ElevatedProcess>>,
}

// Windows only: its calls are Win32.
#[cfg(windows)]
impl Service for RealService {
    fn user_sid(&self) -> Result<String, String> {
        marsa_core::platform::windows::current_user_sid().map_err(|err| err.to_string())
    }

    fn send(&self, request: &[u8], stop: &CancellationToken) -> Result<Vec<u8>, &'static str> {
        use marsa_core::platform::windows::helper_pipe::{self, PipeError, SERVICE_NAME};
        if stop.is_cancelled() {
            return Err("helper_interrupted");
        }
        // Setup runs on a blocking thread of the daemon's runtime, which can
        // wait for a future.
        let runtime = tokio::runtime::Handle::current();
        let sent = runtime.block_on(async {
            tokio::select! {
                () = stop.cancelled() => None,
                sent = helper_pipe::send(request, SEND_LIMIT) => Some(sent),
            }
        });
        match sent {
            None => Err("helper_interrupted"),
            Some(Ok(answer)) => Ok(answer),
            // A pipe that refuses this user while no service runs is not the
            // service's: no prompt can help.
            Some(Err(PipeError::AccessDenied))
                if matches!(helper_pipe::service_process_id(SERVICE_NAME), Ok(None)) =>
            {
                Err("helper_not_the_service")
            }
            Some(Err(err)) => {
                tracing::debug!(error = %err, "the helper service's pipe");
                Err(err.code())
            }
        }
    }

    fn install(&self, helper: &Path, sid: &str, hash: &str) -> Install {
        use std::ffi::OsStr;

        self.elevated(
            helper,
            &[OsStr::new("install"), OsStr::new(sid), OsStr::new(hash)],
        )
    }

    fn uninstall(&self, helper: &Path) -> Install {
        self.elevated(helper, &[std::ffi::OsStr::new("uninstall")])
    }
}

// Windows only: its calls are Win32.
#[cfg(windows)]
impl RealService {
    /// `helper` with `args` behind the UAC prompt, unless an earlier run
    /// still runs; one that runs past [`INSTALL_LIMIT`] is kept, so the
    /// next call shows no second prompt while it runs.
    fn elevated(&self, helper: &Path, args: &[&std::ffi::OsStr]) -> Install {
        use marsa_core::platform::windows::{Elevated, run_elevated};
        let mut running = self
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(process) = running.as_ref() {
            match process.wait(Duration::ZERO) {
                Ok(None) => return Install::StillRunning,
                // Ended, or no longer known: a new run may start.
                Ok(Some(_)) | Err(_) => *running = None,
            }
        }
        match run_elevated(helper, args, INSTALL_LIMIT) {
            Ok(Elevated::Exited(code)) => Install::Exited(code),
            Ok(Elevated::Cancelled) => Install::Cancelled,
            Ok(Elevated::TimedOut(process)) => {
                *running = Some(process);
                Install::TimedOut
            }
            Err(err) => Install::Failed(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use marsa_core::helper::OpStatus;
    use serde_json::json;

    use super::*;

    const SID: &str = "S-1-5-21-1-2-3-1001";
    const HASH: &str = "29E433200254DFCE602212438E9D64E306B672DB9BC4642A7620CEBCC0FD8057";
    const REQUEST: &str = r#"{"version":1,"ops":[{"op":"hosts_sync","names":[]}]}"#;

    /// A Windows machine for setup: the answers of the pipe in order, and
    /// what `install` gives. It records every call.
    struct Fake {
        sends: Mutex<Vec<Result<Vec<u8>, &'static str>>>,
        install: Install,
        calls: Mutex<Vec<String>>,
    }

    impl Fake {
        fn new(sends: Vec<Result<Vec<u8>, &'static str>>, install: Install) -> Self {
            Self {
                sends: Mutex::new(sends),
                install,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Service for Fake {
        fn user_sid(&self) -> Result<String, String> {
            Ok(SID.to_owned())
        }

        fn send(&self, request: &[u8], _stop: &CancellationToken) -> Result<Vec<u8>, &'static str> {
            assert_eq!(request, REQUEST.as_bytes());
            self.calls.lock().unwrap().push("send".to_owned());
            let mut sends = self.sends.lock().unwrap();
            assert!(!sends.is_empty(), "more sends than expected");
            sends.remove(0)
        }

        fn install(&self, helper: &Path, sid: &str, hash: &str) -> Install {
            assert_eq!(helper, helper_path());
            self.calls
                .lock()
                .unwrap()
                .push(format!("install {sid} {hash}"));
            self.install.clone()
        }

        /// The same answer as `install`, for the uninstall.
        fn uninstall(&self, helper: &Path) -> Install {
            assert_eq!(helper, helper_path());
            self.calls.lock().unwrap().push("uninstall".to_owned());
            self.install.clone()
        }
    }

    fn helper_path() -> PathBuf {
        PathBuf::from(r"C:\Users\me\AppData\Local\Marsa\bin\marsa-helper.exe")
    }

    fn done() -> Result<Vec<u8>, &'static str> {
        let answer = json!({ "version": 1, "results": [{ "op": "hosts_sync", "status": "done" }] });
        Ok(answer.to_string().into_bytes())
    }

    fn refused(code: &str) -> Result<Vec<u8>, &'static str> {
        let answer = json!({ "version": 1, "error": { "code": code, "message": "no" } });
        Ok(answer.to_string().into_bytes())
    }

    fn run_flow(flow: Flow, fake: &Fake, hash: Option<&str>) -> Result<Vec<OpResult>, ApiError> {
        launch(
            flow,
            fake,
            &helper_path(),
            REQUEST,
            hash,
            &CancellationToken::new(),
        )
    }

    fn run(fake: &Fake, hash: Option<&str>) -> Result<Vec<OpResult>, ApiError> {
        run_flow(Flow::Setup, fake, hash)
    }

    fn code(result: Result<Vec<OpResult>, ApiError>) -> String {
        result.unwrap_err().code
    }

    #[test]
    fn an_installed_service_answers_with_no_prompt() {
        let fake = Fake::new(vec![done()], Install::Failed("never".to_owned()));
        let results = run(&fake, Some(HASH)).unwrap();
        assert_eq!(results[0].status, OpStatus::Done);
        assert_eq!(fake.calls(), ["send"]);
    }

    #[test]
    fn a_service_that_does_not_run_is_installed_then_asked_again() {
        let fake = Fake::new(vec![Err("helper_not_running"), done()], Install::Exited(0));
        assert!(run(&fake, Some(HASH)).is_ok());
        assert_eq!(
            fake.calls(),
            ["send", format!("install {SID} {HASH}").as_str(), "send"]
        );
    }

    /// Installed for another user: installed again for this one. Without a
    /// CA yet, the service trusts none (`-`).
    #[test]
    fn another_user_means_an_install_again() {
        for first in [Err("helper_access_denied"), refused("client_not_allowed")] {
            let fake = Fake::new(vec![first, done()], Install::Exited(0));
            assert!(run(&fake, None).is_ok());
            assert_eq!(fake.calls()[1], format!("install {SID} -"));
        }
    }

    /// A request that trusts a CA the service was not installed for: the
    /// install names the new one.
    #[test]
    fn another_ca_means_an_install_for_the_new_one() {
        let fake = Fake::new(
            vec![refused("trust_not_allowed"), done()],
            Install::Exited(0),
        );
        assert!(run(&fake, Some(HASH)).is_ok());
        assert_eq!(fake.calls()[1], format!("install {SID} {HASH}"));
    }

    #[test]
    fn a_declined_prompt_changes_nothing() {
        let fake = Fake::new(vec![Err("helper_not_running")], Install::Cancelled);
        assert_eq!(code(run(&fake, Some(HASH))), "setup_cancelled");
        assert_eq!(fake.calls().len(), 2, "no second send");
    }

    #[test]
    fn a_failed_install_has_its_own_code() {
        for install in [Install::Exited(1), Install::Failed("not found".to_owned())] {
            let fake = Fake::new(vec![Err("helper_not_running")], install);
            assert_eq!(code(run(&fake, Some(HASH))), "setup_helper_install_failed");
        }
        for install in [Install::StillRunning, Install::TimedOut] {
            let fake = Fake::new(vec![Err("helper_not_running")], install);
            assert_eq!(code(run(&fake, Some(HASH))), "setup_helper_install_running");
        }
    }

    /// Another program holds the pipe's name: nothing is sent, and no
    /// prompt shows.
    #[test]
    fn a_squatter_gets_no_request_and_no_prompt() {
        let fake = Fake::new(vec![Err("helper_not_the_service")], Install::Exited(0));
        assert_eq!(code(run(&fake, Some(HASH))), "setup_helper_not_the_service");
        assert_eq!(fake.calls(), ["send"]);
    }

    #[test]
    fn each_pipe_failure_has_its_code_and_no_prompt() {
        let cases = [
            ("helper_busy", "setup_helper_busy"),
            ("helper_timed_out", "setup_helper_no_answer"),
            ("helper_no_answer", "setup_helper_no_answer"),
            ("helper_interrupted", "setup_interrupted"),
            ("helper_pipe_failed", "setup_helper_pipe_failed"),
            ("helper_cannot_check", "setup_helper_pipe_failed"),
            ("helper_request_too_large", "setup_helper_pipe_failed"),
        ];
        for (sent, expected) in cases {
            let fake = Fake::new(vec![Err(sent)], Install::Exited(0));
            assert_eq!(code(run(&fake, Some(HASH))), expected, "{sent}");
            assert_eq!(fake.calls(), ["send"], "{sent}");
        }
    }

    /// Still refused after the install: an error, not a second install.
    #[test]
    fn a_refusal_after_the_install_is_not_a_loop() {
        let fake = Fake::new(
            vec![Err("helper_not_running"), refused("client_not_allowed")],
            Install::Exited(0),
        );
        assert_eq!(code(run(&fake, Some(HASH))), "setup_helper_install_failed");
        assert_eq!(fake.calls().len(), 3);
    }

    #[test]
    fn another_refusal_or_an_unreadable_answer_is_an_error() {
        let fake = Fake::new(vec![refused("request_invalid")], Install::Exited(0));
        assert_eq!(code(run(&fake, None)), "setup_helper_rejected");
        let fake = Fake::new(vec![Ok(b"not json".to_vec())], Install::Exited(0));
        assert_eq!(code(run(&fake, None)), "setup_helper_failed");
    }

    /// A daemon that stops: nothing more is sent, and no prompt shows.
    #[test]
    fn a_stop_ends_the_flow_before_any_step() {
        let fake = Fake::new(Vec::new(), Install::Exited(0));
        let stop = CancellationToken::new();
        stop.cancel();
        let result = launch(Flow::Setup, &fake, &helper_path(), REQUEST, None, &stop);
        assert_eq!(code(result), "setup_interrupted");
        assert!(fake.calls().is_empty());
    }

    /// Only setup installs the service: the uninstall gets an error.
    #[test]
    fn only_setup_installs() {
        let fake = Fake::new(vec![Err("helper_not_running")], Install::Exited(0));
        let result = run_flow(Flow::Uninstall, &fake, None);
        assert_eq!(code(result), "uninstall_helper_pipe_failed");
        assert_eq!(fake.calls(), ["send"]);
    }

    #[test]
    fn the_uninstall_is_one_result_or_an_error() {
        let fake = Fake::new(Vec::new(), Install::Exited(0));
        let stop = CancellationToken::new();
        let result = uninstall(&fake, &helper_path(), &stop).unwrap();
        assert_eq!(
            (result.op.as_str(), result.status),
            ("uninstall_all", OpStatus::Done)
        );
        assert_eq!(fake.calls(), ["uninstall"]);
        let cases = [
            (Install::Cancelled, "uninstall_cancelled"),
            (Install::Exited(1), "uninstall_helper_partial"),
            (Install::Exited(0xC000_0005), "uninstall_helper_partial"),
            (Install::Exited(2), "uninstall_helper_failed"),
            (Install::StillRunning, "uninstall_helper_running"),
            (Install::TimedOut, "uninstall_helper_running"),
            (Install::Failed("x".to_owned()), "uninstall_helper_failed"),
        ];
        for (answer, expected) in cases {
            let fake = Fake::new(Vec::new(), answer);
            let error = uninstall(&fake, &helper_path(), &stop).unwrap_err();
            assert_eq!(error.code, expected);
        }
        // A daemon that stops shows no prompt.
        let fake = Fake::new(Vec::new(), Install::Exited(0));
        stop.cancel();
        let error = uninstall(&fake, &helper_path(), &stop).unwrap_err();
        assert_eq!(error.code, "uninstall_interrupted");
        assert!(fake.calls().is_empty());
    }

    /// The pipe client's codes, which the table above matches as text.
    #[cfg(windows)]
    #[test]
    fn the_pipe_codes_are_the_ones_matched_here() {
        use marsa_core::platform::windows::helper_pipe::PipeError;
        let io = || std::io::Error::other("x");
        let codes = [
            (PipeError::NotRunning.code(), "helper_not_running"),
            (PipeError::AccessDenied.code(), "helper_access_denied"),
            (PipeError::NotTheService.code(), "helper_not_the_service"),
            (PipeError::CannotCheck(io()).code(), "helper_cannot_check"),
            (PipeError::Busy(Duration::ZERO).code(), "helper_busy"),
            (PipeError::TooLarge.code(), "helper_request_too_large"),
            (
                PipeError::TimedOut(Duration::ZERO).code(),
                "helper_timed_out",
            ),
            (PipeError::NoAnswer(io()).code(), "helper_no_answer"),
            (PipeError::Io(io()).code(), "helper_pipe_failed"),
        ];
        for (got, expected) in codes {
            assert_eq!(got, expected);
        }
    }
}
