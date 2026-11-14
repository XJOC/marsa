//! Windows: `marsa-helper install` and `marsa-helper uninstall`, which
//! `marsad` runs once each, elevated through the UAC prompt (ADR 0018). Task
//! M4-19j.
//!
//! ```text
//! marsa-helper install <user SID> <CA SHA-256 | ->
//! marsa-helper uninstall
//! ```
//!
//! `install` copies this program to `<Program Files>\Marsa\marsa-helper.exe`,
//! a folder only administrators can write, and registers that copy as the
//! LocalSystem service `MarsaHelper` with an automatic start. The two
//! arguments become the service's own command line (`service <SID> <hash>`):
//! who may open the pipe, and which CA certificate it may trust. Running it
//! again updates the program and the arguments (a new CA after `marsa setup`
//! again, or a new Marsa version).
//!
//! `uninstall` first takes Marsa's lines out of the hosts file and its CA
//! out of the Root store (the ops of `uninstall_all`), then stops and deletes
//! the service, then deletes its folder. Nothing Marsa installed stays.
//!
//! The UAC prompt gives `marsad` no output of the elevated process, so the
//! exit code is the answer: 0 done, 1 a step failed, 2 bad arguments.

use crate::response::{OpResult, Outcome, Response, code};

/// The two commands.
pub const INSTALL: &str = "install";
pub const UNINSTALL: &str = "uninstall";

/// Off Windows there is no service to install.
#[cfg(not(windows))]
pub fn install(_args: &[std::ffi::OsString]) -> Response {
    not_windows()
}

#[cfg(not(windows))]
pub fn uninstall() -> Response {
    not_windows()
}

#[cfg(not(windows))]
fn not_windows() -> Response {
    let reason = "the helper service runs on Windows only";
    Response::error(code::OP_NOT_SUPPORTED, reason.to_owned())
}

// Off Windows only the tests call it.
#[cfg_attr(not(windows), allow(dead_code))]
fn done(op: &'static str, outcome: Outcome) -> Response {
    Response::results(vec![OpResult { op, outcome }])
}

/// `install <SID> <hash>`.
#[cfg(windows)]
pub fn install(args: &[std::ffi::OsString]) -> Response {
    let [sid, hash] = args else {
        return Response::error(code::USAGE, crate::USAGE.to_owned());
    };
    let allowed = match crate::service::Allowed::parse(sid, hash) {
        Ok(allowed) => allowed,
        Err(reason) => return Response::error(code::USAGE, reason),
    };
    // A group has the shape of a user's SID, so `parse` lets it pass; Windows
    // tells them apart. If Windows cannot tell, the service's exact compare
    // still serves only this SID.
    if let Ok(Some(false)) = crate::windows::is_user_account(&allowed.user_sid) {
        let reason = "the SID is not a user account";
        return Response::error(code::USAGE, reason.to_owned());
    }
    match scm::install(&allowed) {
        Ok(()) => done("service_install", Outcome::Done),
        Err(reason) => {
            let result = OpResult::failed("service_install", code::SERVICE_FAILED, reason);
            Response::results(vec![result])
        }
    }
}

/// `uninstall`: the removals, then the service and its folder.
#[cfg(windows)]
pub fn uninstall() -> Response {
    use crate::os::Os;
    use crate::request::Request;
    use crate::system::RealSystem;

    // Through the parser like every request, so nothing runs unchecked.
    let raw = r#"{"version":1,"ops":[{"op":"uninstall_all"}]}"#;
    let request = Request::from_arg(std::ffi::OsStr::new(raw))
        .expect("a constant request that the parser's own tests accept");
    let mut results = crate::ops::apply(&request, Os::Windows, &RealSystem);
    // The service goes only once its lines and its CA are gone: its folder
    // holds the record of the CA, which `trust_remove` needs.
    let removed = results
        .iter()
        .all(|result| !matches!(result.outcome, Outcome::Failed { .. }));
    let result = if removed {
        match scm::uninstall() {
            Ok(outcome) => OpResult {
                op: "service_uninstall",
                outcome,
            },
            Err(reason) => OpResult::failed("service_uninstall", code::SERVICE_FAILED, reason),
        }
    } else {
        let reason = "the hosts lines or the CA could not be removed, so the service stays";
        OpResult::failed("service_uninstall", code::SERVICE_FAILED, reason.to_owned())
    };
    results.push(result);
    Response::results(results)
}

/// The service manager and the files, through `windows-service`.
#[cfg(windows)]
mod scm {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::thread;
    use std::time::{Duration, Instant};

    use windows_service::service::{
        Service, ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceState,
        ServiceType,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    use crate::response::Outcome;
    use crate::service::{Allowed, COMMAND, SERVICE_NAME};
    use crate::system::{RealSystem, System};

    /// The program's name in its folder.
    const PROGRAM: &str = "marsa-helper.exe";
    /// `ERROR_SERVICE_DOES_NOT_EXIST`.
    const NO_SERVICE: i32 = 1060;
    /// How long a start or a stop of the service may take.
    const WAIT: Duration = Duration::from_secs(30);

    fn text(what: &str, err: impl std::fmt::Display) -> String {
        format!("{what}: {err}")
    }

    pub fn install(allowed: &Allowed) -> Result<(), String> {
        let dir = RealSystem
            .install_dir()
            .map_err(|err| text("cannot find Program Files", err))?;
        fs::create_dir_all(&dir).map_err(|err| text("cannot create Marsa's folder", err))?;
        let access = ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE;
        let manager = ServiceManager::local_computer(None::<&str>, access)
            .map_err(|err| text("cannot open the service manager", err))?;
        let rights = ServiceAccess::QUERY_STATUS
            | ServiceAccess::STOP
            | ServiceAccess::START
            | ServiceAccess::CHANGE_CONFIG;
        let existing = match manager.open_service(SERVICE_NAME, rights) {
            Ok(service) => Some(service),
            Err(windows_service::Error::Winapi(err)) if err.raw_os_error() == Some(NO_SERVICE) => {
                None
            }
            Err(err) => return Err(text("cannot open the service", err)),
        };
        // Windows does not replace a program that runs.
        if let Some(service) = &existing {
            stop(service)?;
        }
        let program = dir.join(PROGRAM);
        copy_self(&program)?;
        let info = ServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from("Marsa helper"),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: program,
            launch_arguments: vec![
                OsString::from(COMMAND),
                OsString::from(&allowed.user_sid),
                OsString::from(allowed.ca_sha256.as_deref().unwrap_or("-")),
            ],
            dependencies: Vec::new(),
            // `None`: the LocalSystem account.
            account_name: None,
            account_password: None,
        };
        let service = match existing {
            Some(service) => {
                service
                    .change_config(&info)
                    .map_err(|err| text("cannot update the service", err))?;
                service
            }
            None => manager
                .create_service(&info, rights)
                .map_err(|err| text("cannot create the service", err))?,
        };
        let about = "Keeps Marsa's .test names in the hosts file and trusts Marsa's local CA.";
        // Only a description: a failure here changes nothing that matters.
        let _ = service.set_description(about);
        service
            .start::<&OsStr>(&[])
            .map_err(|err| text("cannot start the service", err))?;
        wait_for(&service, ServiceState::Running)
    }

    pub fn uninstall() -> Result<Outcome, String> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
            .map_err(|err| text("cannot open the service manager", err))?;
        let rights = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
        let outcome = match manager.open_service(SERVICE_NAME, rights) {
            Ok(service) => {
                stop(&service)?;
                service
                    .delete()
                    .map_err(|err| text("cannot delete the service", err))?;
                Outcome::Done
            }
            Err(windows_service::Error::Winapi(err)) if err.raw_os_error() == Some(NO_SERVICE) => {
                Outcome::Unchanged
            }
            Err(err) => return Err(text("cannot open the service", err)),
        };
        let dir = RealSystem
            .install_dir()
            .map_err(|err| text("cannot find Program Files", err))?;
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(Outcome::Done),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(outcome),
            Err(err) => Err(text("cannot delete Marsa's folder", err)),
        }
    }

    /// Stops the service if it runs, and waits until it has.
    fn stop(service: &Service) -> Result<(), String> {
        let status = service
            .query_status()
            .map_err(|err| text("cannot ask the service's state", err))?;
        if status.current_state == ServiceState::Stopped {
            return Ok(());
        }
        // Already stopping is fine; the wait below covers it.
        let _ = service.stop();
        wait_for(service, ServiceState::Stopped)
    }

    fn wait_for(service: &Service, state: ServiceState) -> Result<(), String> {
        let deadline = Instant::now() + WAIT;
        loop {
            let status = service
                .query_status()
                .map_err(|err| text("cannot ask the service's state", err))?;
            if status.current_state == state {
                return Ok(());
            }
            // A start that ended: the exit code says why, for example 231
            // when another program took the pipe's name first.
            if state == ServiceState::Running && status.current_state == ServiceState::Stopped {
                return Err(format!(
                    "the service stopped at start: {:?}",
                    status.exit_code
                ));
            }
            if Instant::now() >= deadline {
                return Err(format!("the service did not reach {state:?} in time"));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Copies this program to `target`, through a temp file in the same
    /// folder and a rename. Nothing to do when this program is `target`.
    fn copy_self(target: &Path) -> Result<(), String> {
        let me = std::env::current_exe().map_err(|err| text("cannot find this program", err))?;
        if me == target {
            return Ok(());
        }
        let temp = target.with_extension("exe.new");
        let copied = fs::copy(&me, &temp).and_then(|_| fs::rename(&temp, target));
        if let Err(err) = copied {
            // Best effort: the copy already failed, and that is the error.
            let _ = fs::remove_file(&temp);
            return Err(text("cannot copy the helper to Program Files", err));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn there_is_nothing_to_install_off_windows() {
        for response in [install(&[]), uninstall()] {
            let json: serde_json::Value = serde_json::from_str(&response.to_json()).unwrap();
            assert_eq!(json["error"]["code"], code::OP_NOT_SUPPORTED);
            assert_eq!(response.exit_code(), 2);
        }
    }

    #[test]
    fn a_step_is_reported_as_one_result() {
        let response = done("service_install", Outcome::Done);
        assert_eq!(response.exit_code(), 0);
        let json: serde_json::Value = serde_json::from_str(&response.to_json()).unwrap();
        assert_eq!(json["results"][0]["op"], "service_install");
    }
}
