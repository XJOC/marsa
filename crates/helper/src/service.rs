//! Windows: the helper as a LocalSystem service behind a named pipe (ADR
//! 0018). Task M4-19f.
//!
//! ```text
//! marsa-helper service <user SID> <CA SHA-256 | ->    started by the SCM
//! ```
//!
//! The service makes `\\.\pipe\marsa-helper`, which only LocalSystem and the
//! user named at install can open, and serves one client at a time:
//!
//! 1. The client sends a 4-byte little-endian length, then the same JSON
//!    request as `marsa-helper apply`, at most 64 KiB.
//! 2. The service checks who is on the other end: the user's SID, read from
//!    the connection itself. Anyone else gets `client_not_allowed`.
//! 3. It parses the request with the same strict parser as `apply`. A
//!    `trust_install` whose certificate is not the one named at install
//!    rejects the whole request with `trust_not_allowed`.
//! 4. It runs the ops for Windows and answers with a 4-byte length and the
//!    JSON response, then drops the client.
//!
//! The SID and the hash come from the service's own command line, which only
//! an administrator can set (`install`, task M4-19j). Steps 2 to 4 are
//! [`answer`], which runs on every system, so the tests do not need Windows.

// The service runs on Windows only, but its checks are built everywhere so
// that their tests run on every system. Off Windows, nothing calls them.
#![cfg_attr(not(windows), allow(dead_code))]

use std::ffi::OsStr;

use crate::ops;
use crate::os::Os;
use crate::request::{Op, Request};
use crate::response::{Response, code};
use crate::system::System;

/// The pipe's name. The client in `marsa_core` uses the same.
pub const PIPE_NAME: &str = r"\\.\pipe\marsa-helper";
/// The service's name in the SCM.
pub const SERVICE_NAME: &str = "MarsaHelper";
/// The service's first argument.
pub const COMMAND: &str = "service";

/// Who may use the service, and which CA certificate it may trust: the two
/// values the administrator approved at install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowed {
    /// The installing user's SID (`S-1-5-21-...`, or `S-1-12-1-...` for a
    /// Microsoft Entra ID account), in the form Windows prints.
    pub user_sid: String,
    /// The SHA-256 of the CA certificate, upper-case hex. `None` (`-` on the
    /// command line): no certificate may be trusted.
    pub ca_sha256: Option<String>,
}

impl Allowed {
    /// Reads the two arguments after `service` (and after `install`).
    ///
    /// # Errors
    ///
    /// Why an argument is refused. Strict, so nothing can be smuggled into
    /// the service's command line or the pipe's security descriptor.
    pub fn parse(user_sid: &OsStr, ca_sha256: &OsStr) -> Result<Self, String> {
        let user_sid = user_sid
            .to_str()
            .filter(|sid| is_user_sid(sid))
            .ok_or("the user SID is not S-1-5-21-<numbers> or S-1-12-1-<numbers>")?;
        let ca_sha256 = match ca_sha256.to_str() {
            Some("-") => None,
            Some(hash) if hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => {
                Some(hash.to_ascii_uppercase())
            }
            _ => return Err("the CA hash is not 64 hex digits or -".to_owned()),
        };
        Ok(Self {
            user_sid: user_sid.to_owned(),
            ca_sha256,
        })
    }
}

/// The shape of a user account's SID, in the form Windows prints it: a
/// local or domain account (`S-1-5-21-` and four to six numbers), or a
/// Microsoft Entra ID account (`S-1-12-1-` and four numbers). Each number
/// fits a `u32` and has no leading zero, so the text can equal what Windows
/// reads from a client. LocalSystem (`S-1-5-18`), built-in groups
/// (`S-1-5-32-...`), and other well-known SIDs are refused.
///
/// A local or domain group has the same shape as a user and passes here.
/// The exact compare in [`answer`] still serves only that SID, which no
/// client's user SID equals; `install` (task M4-19j) also checks with
/// Windows that the SID names a user.
fn is_user_sid(sid: &str) -> bool {
    let (rest, parts) = if let Some(rest) = sid.strip_prefix("S-1-5-21-") {
        (rest, 4..=6)
    } else if let Some(rest) = sid.strip_prefix("S-1-12-1-") {
        (rest, 4..=4)
    } else {
        return false;
    };
    let numbers: Vec<&str> = rest.split('-').collect();
    parts.contains(&numbers.len()) && numbers.iter().all(|number| is_u32_text(number))
}

/// A `u32` as Windows prints it: digits only, no leading zero.
fn is_u32_text(text: &str) -> bool {
    text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'))
        && text.parse::<u32>().is_ok()
}

/// The answer to one message from the pipe. `client_sid` is who sent it, as
/// Windows reports it; `None` if Windows could not tell.
pub fn answer(
    message: &[u8],
    client_sid: Option<&str>,
    allowed: &Allowed,
    system: &impl System,
) -> Response {
    if client_sid != Some(allowed.user_sid.as_str()) {
        let reason = "only the user who installed the helper may use it";
        return Response::error(code::CLIENT_NOT_ALLOWED, reason.to_owned());
    }
    let Ok(text) = std::str::from_utf8(message) else {
        let reason = "the request is not UTF-8";
        return Response::error(code::REQUEST_INVALID, reason.to_owned());
    };
    let request = match Request::from_arg(OsStr::new(text)) {
        Ok(request) => request,
        Err(err) => return Response::error(err.code(), err.to_string()),
    };
    for op in request.ops() {
        if let Op::TrustInstall { pem } = op {
            let hash = ops::certificate_sha256(pem);
            if hash.is_none() || hash != allowed.ca_sha256 {
                let reason = "the certificate is not the one approved at install";
                return Response::error(code::TRUST_NOT_ALLOWED, reason.to_owned());
            }
        }
    }
    Response::results(ops::apply(&request, Os::Windows, system))
}

/// `marsa-helper service ...` off Windows: a usage error.
#[cfg(not(windows))]
pub fn run(_args: &[std::ffi::OsString]) -> Response {
    let reason = "the helper service runs on Windows only";
    Response::error(code::OP_NOT_SUPPORTED, reason.to_owned())
}

/// `marsa-helper service <SID> <hash>`: hands the process to the SCM, which
/// calls `scm::service_main`. Returns only when the service has stopped, or
/// at once if the SCM did not start this process.
#[cfg(windows)]
pub fn run(args: &[std::ffi::OsString]) -> Response {
    match args {
        [sid, hash] => match Allowed::parse(sid, hash) {
            Ok(_) => match scm::start() {
                Ok(()) => Response::results(Vec::new()),
                Err(reason) => Response::error(code::SERVICE_FAILED, reason),
            },
            Err(reason) => Response::error(code::USAGE, reason),
        },
        _ => Response::error(code::USAGE, crate::USAGE.to_owned()),
    }
}

/// The SCM side, through the `windows-service` crate. Windows only.
#[cfg(windows)]
mod scm {
    use std::env;
    use std::ffi::OsString;
    use std::sync::{Arc, Mutex, PoisonError, mpsc};
    use std::thread;
    use std::time::Duration;

    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{
        self, ServiceControlHandlerResult, ServiceStatusHandle,
    };
    use windows_service::{define_windows_service, service_dispatcher};

    use super::{Allowed, PIPE_NAME, SERVICE_NAME, answer};
    use crate::request::MAX_REQUEST_BYTES;
    use crate::system::RealSystem;
    use crate::windows::PipeServer;

    define_windows_service!(ffi_service_main, service_main);

    /// `ERROR_INVALID_PARAMETER`: the service's own arguments are wrong.
    const BAD_ARGUMENTS: u32 = 87;
    /// How long a stop may wait for the request that runs: four `certutil`
    /// calls at their limit of 120 s each, and some room.
    const STOP_WAIT: Duration = Duration::from_secs(600);

    /// Connects this process to the SCM. Fails at once if the SCM did not
    /// start it, for example when a user runs `marsa-helper service`.
    pub fn start() -> Result<(), String> {
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)
            .map_err(|err| format!("not started by the service manager: {err}"))
    }

    /// Tells the SCM the service's state. A failure changes nothing the
    /// service could act on.
    fn report(handle: ServiceStatusHandle, state: ServiceState, exit: u32, wait_hint: Duration) {
        let accept = if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        };
        let _ = handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(exit),
            checkpoint: 0,
            wait_hint,
            process_id: None,
        });
    }

    fn stopped(handle: ServiceStatusHandle, exit: u32) {
        report(handle, ServiceState::Stopped, exit, Duration::default());
    }

    fn service_main(_start_arguments: Vec<OsString>) {
        let (stop_sender, stop_receiver) = mpsc::channel();
        let handler = move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = stop_sender.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        };
        // First, so that every way out below can report `Stopped`. Without
        // it nothing can be reported, and the SCM ends the start at its limit.
        let Ok(handle) = service_control_handler::register(SERVICE_NAME, handler) else {
            return;
        };
        // The arguments of the service's own command line: `service <SID>
        // <hash>`, set at install. `start` checked them before.
        let args: Vec<OsString> = env::args_os().skip(2).collect();
        let Some(Ok(allowed)) = (args.len() == 2).then(|| Allowed::parse(&args[0], &args[1]))
        else {
            stopped(handle, BAD_ARGUMENTS);
            return;
        };
        let server = match PipeServer::create(PIPE_NAME, &allowed.user_sid) {
            Ok(server) => server,
            Err(err) => {
                // Another program has the pipe's name: 231 (`ERROR_PIPE_BUSY`)
                // if it allows one instance, 5 (`ERROR_ACCESS_DENIED`) if its
                // DACL refuses LocalSystem. The SCM shows the code.
                stopped(handle, err.raw_os_error().map_or(1, |code| code as u32));
                return;
            }
        };
        report(handle, ServiceState::Running, 0, Duration::default());
        // A stop waits for the request that runs, if any: ending in the middle
        // of its ops could leave a temp file next to the hosts file, or a
        // `certutil` rollback half done. Between requests the loop holds
        // nothing (it waits in `ConnectNamedPipe`, or for a client's bytes),
        // so a stop then ends the process at once.
        let busy = Arc::new(Mutex::new(()));
        let waiting = Arc::clone(&busy);
        thread::spawn(move || {
            let _ = stop_receiver.recv();
            report(handle, ServiceState::StopPending, 0, STOP_WAIT);
            let _idle = waiting.lock().unwrap_or_else(PoisonError::into_inner);
            stopped(handle, 0);
            std::process::exit(0);
        });
        loop {
            serve_one(&server, &allowed, &busy);
        }
    }

    /// One client: read, check, run, answer, disconnect. Any failure of the
    /// pipe only drops this client.
    fn serve_one(server: &PipeServer, allowed: &Allowed, busy: &Mutex<()>) {
        if server.accept().is_err() {
            // `accept` readies the pipe after a client that came and went.
            // For any other error a disconnect is harmless, and the pause
            // keeps a broken pipe from spinning.
            server.disconnect();
            thread::sleep(Duration::from_millis(100));
            return;
        }
        if let Ok(message) = server.read_message(MAX_REQUEST_BYTES) {
            // After the first read, as `ImpersonateNamedPipeClient` needs.
            let client = server.client_sid().ok();
            let response = {
                // Held while the ops run, so that a stop waits for them.
                let _running = busy.lock().unwrap_or_else(PoisonError::into_inner);
                answer(&message, client.as_deref(), allowed, &RealSystem)
            };
            let _ = server.write_message(response.to_json().as_bytes());
        }
        server.disconnect();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::*;
    use crate::system::FakeSystem;

    const USER: &str = "S-1-5-21-3289802278-1561746829-1498425070-1001";

    fn pem() -> String {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        std::fs::read_to_string(root.join("fixtures").join("helper").join("test-ca.pem")).unwrap()
    }

    fn allowed(hash: Option<String>) -> Allowed {
        Allowed {
            user_sid: USER.to_owned(),
            ca_sha256: hash,
        }
    }

    fn windows() -> FakeSystem {
        FakeSystem {
            system_dir: Some(PathBuf::from(r"C:\Windows\System32")),
            ..FakeSystem::default()
        }
    }

    fn send(request: Value, client: Option<&str>, allowed: &Allowed) -> Value {
        let raw = request.to_string();
        let response = answer(raw.as_bytes(), client, allowed, &windows());
        serde_json::from_str(&response.to_json()).unwrap()
    }

    fn error_code(response: &Value) -> &str {
        response["error"]["code"].as_str().unwrap_or("")
    }

    fn hosts() -> Value {
        json!({ "version": 1, "ops": [{ "op": "hosts_sync", "names": ["shop.test"] }] })
    }

    fn trust(pem: &str) -> Value {
        json!({ "version": 1, "ops": [{ "op": "trust_install", "pem": pem }] })
    }

    #[test]
    fn the_installing_user_gets_the_results() {
        let response = send(hosts(), Some(USER), &allowed(None));
        assert_eq!(response["results"][0]["op"], "hosts_sync");
        assert_eq!(response["results"][0]["status"], "done");
    }

    #[test]
    fn anyone_else_gets_nothing_run() {
        let other = "S-1-5-21-3289802278-1561746829-1498425070-1002";
        for client in [Some(other), Some("S-1-5-18"), Some(""), None] {
            let fake = windows();
            let raw = hosts().to_string();
            let response = answer(raw.as_bytes(), client, &allowed(None), &fake);
            let response: Value = serde_json::from_str(&response.to_json()).unwrap();
            assert_eq!(
                error_code(&response),
                code::CLIENT_NOT_ALLOWED,
                "{client:?}"
            );
            assert!(fake.files.borrow().is_empty() && fake.log.borrow().is_empty());
        }
    }

    #[test]
    fn a_bad_request_is_refused_by_the_same_parser_as_apply() {
        let bad = json!({ "version": 1, "ops": [{ "op": "run", "cmd": "calc.exe" }] });
        assert_eq!(
            error_code(&send(bad, Some(USER), &allowed(None))),
            code::REQUEST_INVALID
        );
        let response = answer(b"\xff\xfe", Some(USER), &allowed(None), &windows());
        let response: Value = serde_json::from_str(&response.to_json()).unwrap();
        assert_eq!(error_code(&response), code::REQUEST_INVALID);
    }

    #[test]
    fn only_the_certificate_approved_at_install_may_be_trusted() {
        let hash = ops::certificate_sha256(&pem()).unwrap();
        // No certificate approved, or another one: refused as a whole, even
        // with a hosts op before it.
        let mut both = trust(&pem());
        both["ops"]
            .as_array_mut()
            .unwrap()
            .insert(0, json!({ "op": "hosts_sync", "names": [] }));
        let other = "0".repeat(64);
        for approved in [None, Some(other)] {
            let fake = windows();
            let raw = both.to_string();
            let response = answer(raw.as_bytes(), Some(USER), &allowed(approved), &fake);
            let response: Value = serde_json::from_str(&response.to_json()).unwrap();
            assert_eq!(error_code(&response), code::TRUST_NOT_ALLOWED);
            assert!(fake.files.borrow().is_empty() && fake.log.borrow().is_empty());
        }
        // The approved one runs (and fails here: the fake has no folder).
        let response = send(trust(&pem()), Some(USER), &allowed(Some(hash.clone())));
        assert_eq!(response["results"][0]["op"], "trust_install");
        assert_eq!(error_code(&response), "");
    }

    /// A PEM that is not one certificate has no hash, so it is never the
    /// approved one: `trust_not_allowed`, where `apply` would answer
    /// `trust_cert_invalid`. Nothing runs.
    #[test]
    fn a_bad_pem_is_not_the_approved_certificate() {
        let hash = ops::certificate_sha256(&pem()).unwrap();
        let fake = windows();
        let raw = trust("-----BEGIN CERTIFICATE-----\nnot base64\n").to_string();
        let response = answer(raw.as_bytes(), Some(USER), &allowed(Some(hash)), &fake);
        let response: Value = serde_json::from_str(&response.to_json()).unwrap();
        assert_eq!(error_code(&response), code::TRUST_NOT_ALLOWED);
        assert!(fake.files.borrow().is_empty() && fake.log.borrow().is_empty());
    }

    #[test]
    fn the_install_arguments_are_strict() {
        let parse = |sid: &str, hash: &str| Allowed::parse(OsStr::new(sid), OsStr::new(hash));
        let hash = "ab".repeat(32);
        let ok = parse(USER, &hash).unwrap();
        assert_eq!(ok.user_sid, USER);
        assert_eq!(ok.ca_sha256, Some(hash.to_ascii_uppercase()));
        assert_eq!(parse(USER, "-").unwrap().ca_sha256, None);
        let entra = "S-1-12-1-1234567890-1234567890-1234567890-4294967295";
        assert_eq!(parse(entra, "-").unwrap().user_sid, entra);
        assert!(parse("S-1-5-21-0-1-2-1001", "-").is_ok());

        let bad_sids = [
            "S-1-5-18",
            "S-1-5-32-544",
            "S-1-1-0",
            "S-1-5-21-1-2-3",
            "S-1-5-21-1-2-3-4-5-6-7",
            "S-1-5-21-1-2-3-x",
            "S-1-5-21-1-2-3-12345678901",
            "S-1-5-21-1-2-3-4294967296",
            "S-1-5-21-01-2-3-4",
            "S-1-5-21-1-2-3-+4",
            "S-1-5-21-1-2--4",
            "S-1-12-1-1-2-3",
            "S-1-12-1-1-2-3-4-5",
            "S-1-12-2-1-2-3-4",
            "s-1-5-21-1-2-3-4",
            "S-1-5-21-1-2-3-4)(A;;GA;;;WD",
            "S-1-5-21-1-2-3-4 ",
            "",
        ];
        for sid in bad_sids {
            assert!(parse(sid, "-").is_err(), "{sid}");
        }
        for bad in ["", "--", &"a".repeat(63), &"a".repeat(65), &"g".repeat(64)] {
            assert!(parse(USER, bad).is_err(), "{bad}");
        }
        // Only Unix builds an `OsStr` that is not UTF-8 from plain bytes.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let not_utf8 = OsStr::from_bytes(b"S-1-5-21-1-2-3-\xff");
            assert!(Allowed::parse(not_utf8, OsStr::new("-")).is_err());
        }
    }

    // On Windows `run` hands the process to the SCM.
    #[cfg(not(windows))]
    #[test]
    fn there_is_no_service_off_windows() {
        use std::ffi::OsString;
        let response = run(&[OsString::from(USER), OsString::from("-")]);
        let response: Value = serde_json::from_str(&response.to_json()).unwrap();
        assert_eq!(error_code(&response), code::OP_NOT_SUPPORTED);
    }
}
