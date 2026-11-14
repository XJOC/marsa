//! Windows: runs the helper's ops through the helper service (ADR 0018,
//! task M4-19g2). The macOS and Linux twins are the `osascript` line of ADR
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
//!
//! The UAC prompt gives back no output, so `install`'s exit code is its
//! answer: 0 installed, anything else `setup_helper_failed`; a No at the
//! prompt is `setup_cancelled`. The prompt has no time limit; the install
//! after it has [`INSTALL_LIMIT`].

use std::path::Path;
use std::time::Duration;

use marsa_core::helper::OpResult;

use super::{ApiError, Flow, failure, parse_answer, results_of};

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
    /// It still ran at [`INSTALL_LIMIT`].
    StillRunning,
    /// It did not start, and why.
    Failed(String),
}

/// The Windows calls of setup, behind a trait so that the tests run on
/// every system. [`RealService`] is the daemon's.
pub trait Service: Send + Sync {
    /// The SID of the user `marsad` runs as.
    fn user_sid(&self) -> Result<String, String>;
    /// Sends `request` to the helper service: its JSON answer, or the stable
    /// code of the failure (`helper_*`, from `marsa_core`'s pipe client).
    /// Blocking.
    fn send(&self, request: &[u8]) -> Result<Vec<u8>, &'static str>;
    /// Runs `helper install <sid> <hash>` behind the UAC prompt. Blocking.
    fn install(&self, helper: &Path, sid: &str, hash: &str) -> Install;
}

/// Sends `request` (the JSON of the ops) to the service, and installs the
/// service first when it must (the table above). `ca_sha256` is the CA that
/// the request trusts, if any: the service is installed for it.
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
) -> Result<Vec<OpResult>, ApiError> {
    let why = match ask(flow, service, request)? {
        Asked::Results(results) => return Ok(results),
        Asked::Install(why) => why,
    };
    tracing::info!(why, "installing the helper service, behind the UAC prompt");
    let sid = service.user_sid().map_err(|reason| {
        tracing::warn!(%reason, "cannot read the SID of this user");
        failure(&flow.code("helper_failed"), "cannot read this user's SID")
    })?;
    match service.install(helper, &sid, ca_sha256.unwrap_or("-")) {
        Install::Exited(0) => {}
        Install::Cancelled => {
            tracing::info!("the UAC prompt was declined");
            return Err(failure(
                &flow.code("cancelled"),
                "the UAC prompt was declined",
            ));
        }
        Install::Exited(code) => {
            tracing::warn!(
                code,
                "marsa-helper install failed; Windows' service log says why"
            );
            let message = format!("the helper service was not installed (exit code {code})");
            return Err(failure(&flow.code("helper_failed"), message));
        }
        Install::StillRunning => {
            tracing::warn!("marsa-helper install still runs after its limit");
            let message = "the install of the helper service did not end in time";
            return Err(failure(&flow.code("helper_failed"), message));
        }
        Install::Failed(reason) => {
            tracing::warn!(%reason, "cannot start marsa-helper install");
            let message = format!("cannot start the install of the helper service: {reason}");
            return Err(failure(&flow.code("helper_failed"), message));
        }
    }
    match ask(flow, service, request)? {
        Asked::Results(results) => Ok(results),
        Asked::Install(why) => {
            tracing::warn!(
                why,
                "the helper service refuses the request after its install"
            );
            let message =
                format!("the helper service refuses the request after its install: {why}");
            Err(failure(&flow.code("helper_failed"), message))
        }
    }
}

/// What one request to the service gave.
enum Asked {
    Results(Vec<OpResult>),
    /// The service must be installed (again) first, and why.
    Install(&'static str),
}

fn ask(flow: Flow, service: &dyn Service, request: &str) -> Result<Asked, ApiError> {
    let json = match service.send(request.as_bytes()) {
        Ok(json) => json,
        Err("helper_not_running") => return Ok(Asked::Install("the service is not running")),
        Err("helper_access_denied") => {
            return Ok(Asked::Install("the service's pipe refused this user"));
        }
        Err("helper_not_the_service") => {
            tracing::warn!("another program holds the helper service's pipe name");
            let message = "another program holds the name of the helper service's pipe, so \
                           nothing was sent";
            return Err(failure(&flow.code("helper_not_the_service"), message));
        }
        Err("helper_busy") => {
            return Err(failure(
                &flow.code("helper_busy"),
                "the helper service stayed busy with other requests",
            ));
        }
        Err(code @ ("helper_timed_out" | "helper_no_answer")) => {
            tracing::warn!(
                code,
                "the helper service gave no answer; the request may have run"
            );
            return Err(failure(
                &flow.code("helper_no_answer"),
                "the helper service gave no answer, so the result is not known",
            ));
        }
        Err(code) => {
            tracing::warn!(code, "the helper service's pipe failed");
            let message = format!("the helper service's pipe failed: {code}");
            return Err(failure(&flow.code("helper_failed"), message));
        }
    };
    let Some(answer) = parse_answer(json.trim_ascii()) else {
        tracing::warn!(
            bytes = json.len(),
            "the helper service's answer cannot be read"
        );
        return Err(failure(
            &flow.code("helper_failed"),
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
#[cfg(windows)]
pub struct RealService;

#[cfg(windows)]
impl Service for RealService {
    fn user_sid(&self) -> Result<String, String> {
        marsa_core::platform::windows::current_user_sid().map_err(|err| err.to_string())
    }

    fn send(&self, request: &[u8]) -> Result<Vec<u8>, &'static str> {
        use marsa_core::platform::windows::helper_pipe;
        // Setup runs on a blocking thread of the daemon's runtime, which can
        // wait for a future.
        let runtime = tokio::runtime::Handle::current();
        runtime
            .block_on(helper_pipe::send(request, SEND_LIMIT))
            .map_err(|err| {
                tracing::debug!(error = %err, "the helper service's pipe");
                err.code()
            })
    }

    fn install(&self, helper: &Path, sid: &str, hash: &str) -> Install {
        use std::ffi::OsStr;

        use marsa_core::platform::windows::{Elevated, run_elevated};
        let args = [OsStr::new("install"), OsStr::new(sid), OsStr::new(hash)];
        match run_elevated(helper, &args, INSTALL_LIMIT) {
            Ok(Elevated::Exited(code)) => Install::Exited(code),
            Ok(Elevated::Cancelled) => Install::Cancelled,
            // Never started again meanwhile: setup ends here.
            Ok(Elevated::TimedOut(_running)) => Install::StillRunning,
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

        fn send(&self, request: &[u8]) -> Result<Vec<u8>, &'static str> {
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

    fn run(fake: &Fake, hash: Option<&str>) -> Result<Vec<OpResult>, ApiError> {
        launch(Flow::Setup, fake, &helper_path(), REQUEST, hash)
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

    /// Installed for another user, or for another CA: installed again for
    /// this one. Without a CA yet, the service trusts none (`-`).
    #[test]
    fn another_user_or_another_ca_means_an_install_again() {
        for first in [
            Err("helper_access_denied"),
            refused("client_not_allowed"),
            refused("trust_not_allowed"),
        ] {
            let fake = Fake::new(vec![first, done()], Install::Exited(0));
            assert!(run(&fake, None).is_ok());
            assert_eq!(fake.calls()[1], format!("install {SID} -"));
        }
    }

    #[test]
    fn a_declined_prompt_changes_nothing() {
        let fake = Fake::new(vec![Err("helper_not_running")], Install::Cancelled);
        assert_eq!(code(run(&fake, Some(HASH))), "setup_cancelled");
        assert_eq!(fake.calls().len(), 2, "no second send");
    }

    #[test]
    fn a_failed_install_is_helper_failed() {
        for install in [
            Install::Exited(1),
            Install::StillRunning,
            Install::Failed("not found".to_owned()),
        ] {
            let fake = Fake::new(vec![Err("helper_not_running")], install);
            assert_eq!(code(run(&fake, Some(HASH))), "setup_helper_failed");
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
    fn busy_or_no_answer_have_their_own_codes() {
        let cases = [
            ("helper_busy", "setup_helper_busy"),
            ("helper_timed_out", "setup_helper_no_answer"),
            ("helper_no_answer", "setup_helper_no_answer"),
            ("helper_pipe_failed", "setup_helper_failed"),
            ("helper_cannot_check", "setup_helper_failed"),
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
        assert_eq!(code(run(&fake, Some(HASH))), "setup_helper_failed");
        assert_eq!(fake.calls().len(), 3);
    }

    #[test]
    fn another_refusal_or_an_unreadable_answer_is_an_error() {
        let fake = Fake::new(vec![refused("request_invalid")], Install::Exited(0));
        assert_eq!(code(run(&fake, None)), "setup_helper_rejected");
        let fake = Fake::new(vec![Ok(b"not json".to_vec())], Install::Exited(0));
        assert_eq!(code(run(&fake, None)), "setup_helper_failed");
    }
}
