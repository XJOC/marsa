//! Windows: `marsa-helper install` and `marsa-helper uninstall`, which
//! `marsad` runs once each, elevated through the UAC prompt (ADR 0018). Task
//! M4-19j.
//!
//! ```text
//! marsa-helper install <user SID> <CA SHA-256 | ->
//! marsa-helper uninstall
//! ```
//!
//! `install` copies this program to `<Program Files>\Marsa\marsa-helper.exe`
//! and registers that copy as the LocalSystem service `MarsaHelper` with an
//! automatic start. The folder gets its own DACL first: full control for
//! LocalSystem and Administrators, read and run for Users. The two arguments
//! become the service's own command line (`service <SID> <hash>`): who may
//! open the pipe, and which CA certificate it may trust. Running it again
//! updates the program and the arguments (a new CA after `marsa setup`
//! again, or a new Marsa version).
//!
//! Windows does not let a program that runs be replaced, only renamed
//! (AGENTS.md section 6). So an update writes the new program next to the
//! old one, stops the service and waits for its process to end, moves the
//! old program aside, and puts the new one in place. If a step fails, the
//! service is started again, so the old program keeps serving.
//!
//! `uninstall` stops the service first, so no request puts a line or the CA
//! back meanwhile. Then it takes Marsa's lines out of the hosts file and its
//! CA out of the Root store (the ops of `uninstall_all`), then deletes the
//! service and Marsa's own files, and the folder if nothing else is in it.
//! If a removal fails, the service starts again and stays: its folder holds
//! the record of the CA.
//!
//! `marsad` runs the helper of the app's own folder for both commands, not
//! the copy in Program Files, which `uninstall` deletes.
//!
//! The UAC prompt gives `marsad` no output of the elevated process, so the
//! exit code is the answer: 0 done, 1 a step failed, 2 bad arguments.

use crate::request::{Request, ValidRequest};
use crate::response::{OpResult, Outcome, Response, code};

/// The two commands.
pub const INSTALL: &str = "install";
pub const UNINSTALL: &str = "uninstall";

/// The removals of `uninstall`, through the parser like every request, so
/// nothing runs unchecked.
// Off Windows only the tests call it.
#[cfg_attr(not(windows), allow(dead_code))]
fn removals() -> ValidRequest {
    let raw = r#"{"version":1,"ops":[{"op":"uninstall_all"}]}"#;
    Request::from_arg(std::ffi::OsStr::new(raw))
        .expect("a constant request, which the tests parse on every system")
}

/// Off Windows there is no service to install.
// The Windows version is below.
#[cfg(not(windows))]
pub fn install(_args: &[std::ffi::OsString]) -> Response {
    not_windows()
}

// The Windows version is below.
#[cfg(not(windows))]
pub fn uninstall() -> Response {
    not_windows()
}

// Only the versions above call it.
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

/// `uninstall`: the service stops, the removals run, then the service and
/// Marsa's files go.
#[cfg(windows)]
pub fn uninstall() -> Response {
    use crate::os::Os;
    use crate::system::RealSystem;

    const OP: &str = "service_uninstall";
    let service = match scm::stop_installed() {
        Ok(service) => service,
        Err(reason) => {
            let result = OpResult::failed(OP, code::SERVICE_FAILED, reason);
            return Response::results(vec![result]);
        }
    };
    let mut results = crate::ops::apply(&removals(), Os::Windows, &RealSystem);
    let removed = results
        .iter()
        .all(|result| !matches!(result.outcome, Outcome::Failed { .. }));
    let result = if removed {
        match scm::remove(service) {
            Ok(outcome) => OpResult { op: OP, outcome },
            Err(reason) => OpResult::failed(OP, code::SERVICE_FAILED, reason),
        }
    } else {
        // It serves the hosts lines that stay, and its folder holds the
        // record of the CA, which a later `trust_remove` needs.
        if let Some(service) = &service {
            scm::start_again(service);
        }
        let reason = "the hosts lines or the CA could not be removed, so the service stays";
        OpResult::failed(OP, code::SERVICE_FAILED, reason.to_owned())
    };
    results.push(result);
    Response::results(results)
}

/// The service manager and the files, through `windows-service`.
#[cfg(windows)]
mod scm {
    use std::ffi::{OsStr, OsString};
    use std::fs::{self, OpenOptions};
    use std::io::{self, Write};
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
    /// The next program, written while the service still serves.
    const NEW: &str = "marsa-helper.exe.new";
    /// The program before an update, moved aside while it may still run.
    const OLD: &str = "marsa-helper.exe.old";
    /// Marsa's files in its folder (`ca-root.pem` is the record of the CA,
    /// `trust_windows`). `uninstall` deletes these and nothing else.
    const FILES: [&str; 4] = [PROGRAM, NEW, OLD, "ca-root.pem"];
    /// `ERROR_SERVICE_DOES_NOT_EXIST`.
    const NO_SERVICE: i32 = 1060;
    /// `ERROR_SERVICE_MARKED_FOR_DELETE`: deleted, while a program still
    /// holds a handle to it.
    const MARKED_FOR_DELETE: i32 = 1072;
    /// How long a start may take.
    const START_WAIT: Duration = Duration::from_secs(30);
    /// How long a stop may take: the service first finishes the request
    /// that runs, and each command of an op has its own limit (120 s for
    /// `certutil`). The service gives the same hint (`service.rs`).
    const STOP_WAIT: Duration = Duration::from_secs(600);
    /// How long the process may take to end after the service reports
    /// `Stopped`, which it does just before it exits.
    const EXIT_WAIT: Duration = Duration::from_secs(30);

    fn text(what: &str, err: impl std::fmt::Display) -> String {
        format!("{what}: {err}")
    }

    fn manager(access: ServiceManagerAccess) -> Result<ServiceManager, String> {
        ServiceManager::local_computer(None::<&str>, access)
            .map_err(|err| text("cannot open the service manager", err))
    }

    /// The service, or `None` if it is not installed.
    fn open(manager: &ServiceManager, rights: ServiceAccess) -> Result<Option<Service>, String> {
        match manager.open_service(SERVICE_NAME, rights) {
            Ok(service) => Ok(Some(service)),
            Err(windows_service::Error::Winapi(err)) if err.raw_os_error() == Some(NO_SERVICE) => {
                Ok(None)
            }
            Err(err) => Err(text("cannot open the service", err)),
        }
    }

    pub fn install(allowed: &Allowed) -> Result<(), String> {
        let dir = RealSystem
            .install_dir()
            .map_err(|err| text("cannot find Program Files", err))?;
        crate::windows::private_dir(&dir)
            .map_err(|err| text("cannot make Marsa's folder private", err))?;
        // Left aside by an earlier update, once its process has ended.
        let _ = fs::remove_file(dir.join(OLD));
        write_new(&dir.join(NEW))?;
        let manager =
            manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
        let rights = ServiceAccess::QUERY_STATUS
            | ServiceAccess::STOP
            | ServiceAccess::START
            | ServiceAccess::CHANGE_CONFIG;
        let info = ServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from("Marsa helper"),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: dir.join(PROGRAM),
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
        let service = match open(&manager, rights)? {
            Some(service) => {
                let updated = stop(&service).and_then(|()| swap(&dir)).and_then(|()| {
                    service
                        .change_config(&info)
                        .map_err(|err| text("cannot update the service", err))
                });
                if let Err(reason) = updated {
                    // The old program serves again, or the new one with the
                    // old arguments.
                    start_again(&service);
                    let _ = fs::remove_file(dir.join(NEW));
                    return Err(reason);
                }
                service
            }
            None => {
                swap(&dir)?;
                manager
                    .create_service(&info, rights)
                    .map_err(|err| text("cannot create the service", err))?
            }
        };
        let about = "Keeps Marsa's .test names in the hosts file and trusts Marsa's local CA.";
        // Only a description: a failure here changes nothing that matters.
        let _ = service.set_description(about);
        service
            .start::<&OsStr>(&[])
            .map_err(|err| text("cannot start the service", err))?;
        wait_for_start(&service)
    }

    /// Stops the service if it is installed, and returns it for
    /// [`remove`] or [`start_again`].
    pub fn stop_installed() -> Result<Option<Service>, String> {
        let manager = manager(ServiceManagerAccess::CONNECT)?;
        let rights = ServiceAccess::QUERY_STATUS
            | ServiceAccess::STOP
            | ServiceAccess::START
            | ServiceAccess::DELETE;
        let service = open(&manager, rights)?;
        if let Some(service) = &service {
            stop(service)?;
        }
        Ok(service)
    }

    /// Deletes the stopped service, then Marsa's files and its folder.
    pub fn remove(service: Option<Service>) -> Result<Outcome, String> {
        let mut outcome = Outcome::Unchanged;
        if let Some(service) = service {
            match service.delete() {
                Ok(()) => outcome = Outcome::Done,
                // Deleted by an earlier `uninstall`: the files are left.
                Err(windows_service::Error::Winapi(err))
                    if err.raw_os_error() == Some(MARKED_FOR_DELETE) =>
                {
                    outcome = Outcome::Done;
                }
                Err(err) => return Err(text("cannot delete the service", err)),
            }
        }
        let dir = RealSystem
            .install_dir()
            .map_err(|err| text("cannot find Program Files", err))?;
        for name in FILES {
            match fs::remove_file(dir.join(name)) {
                Ok(()) => outcome = Outcome::Done,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(text(&format!("cannot delete {name}"), err)),
            }
        }
        match fs::remove_dir(&dir) {
            Ok(()) => Ok(Outcome::Done),
            // Gone already, or it holds files that Marsa did not write: they
            // stay, with their folder.
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                Ok(outcome)
            }
            Err(err) => Err(text("cannot delete Marsa's folder", err)),
        }
    }

    /// Starts the service again after a failed step. Best effort: the
    /// failed step is the error.
    pub fn start_again(service: &Service) {
        let _ = service.start::<&OsStr>(&[]);
    }

    /// Stops the service if it runs, waits until it has, then waits for its
    /// process to end, so that its program can be moved and deleted.
    fn stop(service: &Service) -> Result<(), String> {
        let deadline = Instant::now() + STOP_WAIT;
        let mut process = None;
        loop {
            let status = service
                .query_status()
                .map_err(|err| text("cannot ask the service's state", err))?;
            process = status.process_id.or(process);
            match status.current_state {
                ServiceState::Stopped => break,
                // A stop sent while it started was refused: send it again.
                ServiceState::Running | ServiceState::Paused => {
                    let _ = service.stop();
                }
                // Starting or stopping: wait.
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the service did not stop in {} s",
                    STOP_WAIT.as_secs()
                ));
            }
            thread::sleep(Duration::from_millis(100));
        }
        if let Some(process) = process {
            crate::windows::wait_for_exit(process, EXIT_WAIT);
        }
        Ok(())
    }

    fn wait_for_start(service: &Service) -> Result<(), String> {
        let deadline = Instant::now() + START_WAIT;
        loop {
            let status = service
                .query_status()
                .map_err(|err| text("cannot ask the service's state", err))?;
            match status.current_state {
                ServiceState::Running => return Ok(()),
                // A start that ended: the exit code says why, for example
                // 231 when another program took the pipe's name first.
                ServiceState::Stopped => {
                    return Err(format!(
                        "the service stopped at start: {:?}",
                        status.exit_code
                    ));
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the service did not start in {} s",
                    START_WAIT.as_secs()
                ));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Writes this program to `new` as a fresh file: no attribute, stream,
    /// or link of the source or of a leftover comes with it.
    fn write_new(new: &Path) -> Result<(), String> {
        let me = std::env::current_exe().map_err(|err| text("cannot find this program", err))?;
        let bytes = fs::read(&me).map_err(|err| text("cannot read this program", err))?;
        match fs::remove_file(new) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(text("cannot delete an old copy", err)),
        }
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(new)
            .and_then(|mut file| file.write_all(&bytes).and_then(|()| file.sync_all()));
        if let Err(err) = written {
            let _ = fs::remove_file(new);
            return Err(text("cannot copy the helper to Program Files", err));
        }
        Ok(())
    }

    /// Puts the new program in place. The current one, which may still be
    /// loaded, is moved aside first: Windows renames a program that runs,
    /// but does not replace it.
    fn swap(dir: &Path) -> Result<(), String> {
        let program = dir.join(PROGRAM);
        let old = dir.join(OLD);
        match fs::rename(&program, &old) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(text("cannot move the old helper aside", err)),
        }
        if let Err(err) = fs::rename(dir.join(NEW), &program) {
            // Back as it was, best effort.
            let _ = fs::rename(&old, &program);
            return Err(text("cannot put the new helper in place", err));
        }
        // It may still be loaded for a moment; the next install or
        // uninstall deletes it.
        let _ = fs::remove_file(&old);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // On Windows these commands change the system, which tests never do.
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

    /// The constant request of `uninstall` parses, on every system.
    #[test]
    fn the_removals_are_one_uninstall_all() {
        let request = removals();
        assert_eq!(request.ops().len(), 1);
        assert!(matches!(
            request.ops()[0],
            crate::request::Op::UninstallAll {}
        ));
    }
}
