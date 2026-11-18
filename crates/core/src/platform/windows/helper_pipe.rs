//! The client end of the Windows helper service (ADR 0018, task M4-19g1).
//!
//! The service `MarsaHelper` runs as LocalSystem and serves one request per
//! connection on `\\.\pipe\marsa-helper`: a 4-byte little-endian length,
//! then the JSON request; the answer comes back framed the same way. Both
//! are at most 64 KiB.
//!
//! Before it sends anything, the client checks that the server is the
//! service: the server runs in session 0, and its process is the one the
//! service manager names for the running `MarsaHelper`. A program of the
//! user that took the name while the service was down is in the user's
//! session; a service of another account is in session 0 but has another
//! process. The check is on the handle the request then goes through, and a
//! pipe instance keeps its server, so nothing can change in between.
//!
//! The pipe grants the user read, and write without `FILE_APPEND_DATA`
//! (which on a pipe is the right to add an instance), so the client opens it
//! with exactly `FILE_GENERIC_READ | FILE_WRITE_DATA`.
//!
//! The errors say whether the request went out: [`PipeError::NoAnswer`] and
//! [`PipeError::TimedOut`] mean it may have run, and the caller reads the
//! state again; every other error means nothing was sent.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::size_of;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, IntoRawHandle};
use std::ptr::null;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::NamedPipeClient;
use tokio::time::{Instant, sleep_until, timeout_at};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, ERROR_SERVICE_DOES_NOT_EXIST, HANDLE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ, FILE_WRITE_DATA, SECURITY_IDENTIFICATION,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeServerProcessId, GetNamedPipeServerSessionId};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_HANDLE,
    SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
    SERVICE_STATUS_PROCESS, SERVICE_STOP_PENDING,
};

use super::wide;

/// The service's pipe, as `marsa-helper` makes it.
pub const PIPE_NAME: &str = r"\\.\pipe\marsa-helper";
/// The service's name in the service manager.
pub const SERVICE_NAME: &str = "MarsaHelper";
/// The largest request or answer.
pub const MAX_MESSAGE: usize = 64 * 1024;
/// How often a busy pipe is tried again: the service serves one client at a
/// time.
const RETRY: Duration = Duration::from_millis(50);
/// The longest limit `send` takes: the slowest request is a few minutes.
const MAX_LIMIT: Duration = Duration::from_secs(3600);

/// Why a request got no answer.
#[derive(Debug, thiserror::Error)]
pub enum PipeError {
    /// No pipe: the service is not installed, or not running. Nothing sent.
    #[error("the helper service is not running")]
    NotRunning,
    /// The pipe refused this user: the service serves another Windows user,
    /// or another program holds the name. Nothing sent.
    #[error("the helper's pipe refused this user")]
    AccessDenied,
    /// Something else serves the pipe's name. Nothing sent.
    #[error("the program behind the helper's pipe is not the helper service")]
    NotTheService,
    /// Windows would not say which process the service runs in, so the
    /// server could not be checked. Nothing sent.
    #[error("cannot ask Windows about the helper service: {0}")]
    CannotCheck(#[source] io::Error),
    /// The service served other clients until the limit. Nothing sent.
    #[error("the helper service was busy for {0:?}")]
    Busy(Duration),
    /// The request is over [`MAX_MESSAGE`]. Nothing sent.
    #[error("the request is larger than 64 KiB")]
    TooLarge,
    /// The request went out, and no answer came in time. The service may
    /// still run it.
    #[error("the helper service did not answer in {0:?}")]
    TimedOut(Duration),
    /// The request went out (or started to), and no whole answer came back:
    /// the connection ended, or the answer was not one message. Whether the
    /// ops ran is unknown: the caller reads the state again.
    #[error("the helper service gave no answer: {0}")]
    NoAnswer(#[source] io::Error),
    /// The pipe failed before anything was sent.
    #[error("the helper's pipe failed: {0}")]
    Io(#[source] io::Error),
}

impl PipeError {
    /// A stable code, for the UI's strings.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotRunning => "helper_not_running",
            Self::AccessDenied => "helper_access_denied",
            Self::NotTheService => "helper_not_the_service",
            Self::CannotCheck(_) => "helper_cannot_check",
            Self::Busy(_) => "helper_busy",
            Self::TooLarge => "helper_request_too_large",
            Self::TimedOut(_) => "helper_timed_out",
            Self::NoAnswer(_) => "helper_no_answer",
            Self::Io(_) => "helper_pipe_failed",
        }
    }
}

/// Sends `request` (the JSON of a helper request) to the service and
/// returns its answer, within `limit` in all (at most an hour). The service
/// runs one client at a time, so a busy pipe is tried again until `limit`.
///
/// The check of the server asks the service manager, a short blocking call
/// outside `limit`.
///
/// # Errors
///
/// [`PipeError`], which says whether the request went out.
pub async fn send(request: &[u8], limit: Duration) -> Result<Vec<u8>, PipeError> {
    exchange(PIPE_NAME, request, limit, |pipe| {
        verdict(server_of(pipe), service_process_id(SERVICE_NAME))
    })
    .await
}

/// What the check of the server found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Service,
    NotTheService,
}

/// The decision of the check: the server is in session 0, and its process
/// is the running service's. `server` is `(session, process)`, `None` if
/// Windows did not say; `service` is the service's process, `Ok(None)` if
/// it is not installed or not running.
fn verdict(
    server: Option<(u32, u32)>,
    service: io::Result<Option<u32>>,
) -> Result<Verdict, PipeError> {
    let service = service.map_err(PipeError::CannotCheck)?;
    Ok(match (server, service) {
        (Some((0, process)), Some(running)) if process != 0 && process == running => {
            Verdict::Service
        }
        _ => Verdict::NotTheService,
    })
}

/// [`send`] to the pipe `name`, with `check` as the check of the server.
async fn exchange(
    name: &str,
    request: &[u8],
    limit: Duration,
    check: impl Fn(HANDLE) -> Result<Verdict, PipeError>,
) -> Result<Vec<u8>, PipeError> {
    let len = u32::try_from(request.len())
        .ok()
        .filter(|_| request.len() <= MAX_MESSAGE)
        .ok_or(PipeError::TooLarge)?;
    let limit = limit.min(MAX_LIMIT);
    let deadline = Instant::now() + limit;
    let file = loop {
        match open(name) {
            Ok(file) => break file,
            Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(PipeError::Busy(limit));
                }
                sleep_until((now + RETRY).min(deadline)).await;
            }
            Err(err) if err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
                return Err(PipeError::AccessDenied);
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(PipeError::NotRunning),
            Err(err) => return Err(PipeError::Io(err)),
        }
    };
    if check(file.as_raw_handle())? != Verdict::Service {
        return Err(PipeError::NotTheService);
    }
    // The open or the check may have taken the rest of the time.
    if Instant::now() >= deadline {
        return Err(PipeError::Busy(limit));
    }
    // SAFETY: the handle is a pipe client opened for overlapped I/O (the
    // flag in `open`); `into_raw_handle` hands it over, and the
    // `NamedPipeClient` owns and closes it from here, on failure too.
    let mut pipe = unsafe { NamedPipeClient::from_raw_handle(file.into_raw_handle()) }
        .map_err(PipeError::Io)?;
    let talk = async {
        pipe.write_all(&len.to_le_bytes()).await?;
        pipe.write_all(request).await?;
        let mut size = [0u8; 4];
        pipe.read_exact(&mut size).await?;
        let size = u32::from_le_bytes(size) as usize;
        if size > MAX_MESSAGE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the answer is larger than 64 KiB",
            ));
        }
        let mut answer = vec![0; size];
        pipe.read_exact(&mut answer).await?;
        Ok(answer)
    };
    match timeout_at(deadline, talk).await {
        Err(_) => Err(PipeError::TimedOut(limit)),
        Ok(Ok(answer)) => Ok(answer),
        // A closed connection reads as an end of file, and a server that
        // disconnected gives `ERROR_PIPE_NOT_CONNECTED`; the request may
        // have run in every case.
        Ok(Err(err)) => Err(PipeError::NoAnswer(err)),
    }
}

/// Opens the pipe as the service allows: read, write without
/// `FILE_APPEND_DATA`, and identification only (the service may read who
/// the client is, and act as it for nothing more).
fn open(name: &str) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(FILE_GENERIC_READ | FILE_WRITE_DATA)
        .security_qos_flags(SECURITY_IDENTIFICATION)
        .custom_flags(FILE_FLAG_OVERLAPPED)
        .open(name)
}

/// The session and the process of the server end of `pipe`, as Windows
/// reports them; `None` if it does not.
fn server_of(pipe: HANDLE) -> Option<(u32, u32)> {
    let mut session = 0;
    let mut process = 0;
    // SAFETY: `pipe` is an open pipe client handle, and both out pointers
    // are valid.
    let known = unsafe {
        GetNamedPipeServerSessionId(pipe, &mut session) != 0
            && GetNamedPipeServerProcessId(pipe, &mut process) != 0
    };
    known.then_some((session, process))
}

/// A service manager handle, closed on drop.
struct Sc(SC_HANDLE);

impl Drop for Sc {
    fn drop(&mut self) {
        // SAFETY: the handle came from `OpenSCManagerW` or `OpenServiceW`,
        // was checked, and is closed once.
        unsafe { CloseServiceHandle(self.0) };
    }
}

/// The process of the service `name` while it is running, as the service
/// manager reports it. Read only.
///
/// # Errors
///
/// The Win32 error when Windows will not say, for example when the
/// service's DACL does not let this user query it. A service that is not
/// installed, or not running, is `Ok(None)`.
pub fn service_process_id(name: &str) -> io::Result<Option<u32>> {
    let name = wide(OsStr::new(name))?;
    // SAFETY: null machine and database names mean this machine's active
    // database. The handle is checked, then owned by `Sc`.
    let manager = unsafe { OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        return Err(io::Error::last_os_error());
    }
    let manager = Sc(manager);
    // SAFETY: `name` is NUL-terminated and lives through the call; the
    // handle is checked, then owned by `Sc`.
    let service = unsafe { OpenServiceW(manager.0, name.as_ptr(), SERVICE_QUERY_STATUS) };
    if service.is_null() {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) {
            return Ok(None);
        }
        return Err(err);
    }
    let service = Sc(service);
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0;
    // SAFETY: the buffer is a `SERVICE_STATUS_PROCESS` and its exact size
    // is passed, as `SC_STATUS_PROCESS_INFO` asks.
    let read = unsafe {
        QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            (&raw mut status).cast(),
            size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut needed,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    // `Running`, or stopping after it ran: the service reports `Running` once
    // its pipe exists, and while it stops it finishes the request that runs.
    // While it starts, the process is known but the pipe may be someone
    // else's.
    let serving = matches!(
        status.dwCurrentState,
        SERVICE_RUNNING | SERVICE_STOP_PENDING
    );
    let running = serving && status.dwProcessId != 0;
    Ok(running.then_some(status.dwProcessId))
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use std::ptr::null_mut;

    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use tokio::time::{sleep, timeout};
    use windows_sys::Win32::Security::{
        GetTokenInformation, RevertToSelf, SECURITY_IMPERSONATION_LEVEL, SecurityIdentification,
        TOKEN_QUERY, TokenImpersonationLevel,
    };
    use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
    use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

    use super::*;

    const LIMIT: Duration = Duration::from_secs(20);

    /// A pipe name of this test run only.
    fn test_pipe(test: &str) -> String {
        format!(r"\\.\pipe\marsa-core-test-{}-{test}", std::process::id())
    }

    fn server(name: &str) -> NamedPipeServer {
        ServerOptions::new()
            .first_pipe_instance(true)
            .max_instances(1)
            .create(name)
            .unwrap()
    }

    fn any_server(_: HANDLE) -> Result<Verdict, PipeError> {
        Ok(Verdict::Service)
    }

    /// Reads one framed request, as the service does.
    async fn read_request(server: &mut NamedPipeServer) -> Vec<u8> {
        let mut len = [0u8; 4];
        server.read_exact(&mut len).await.unwrap();
        let mut request = vec![0; u32::from_le_bytes(len) as usize];
        server.read_exact(&mut request).await.unwrap();
        request
    }

    async fn answer(server: &mut NamedPipeServer, bytes: &[u8]) {
        let len = u32::try_from(bytes.len()).unwrap();
        server.write_all(&len.to_le_bytes()).await.unwrap();
        server.write_all(bytes).await.unwrap();
    }

    /// The server's side of a test, with a limit so that a bug fails the
    /// test instead of hanging it.
    async fn within<T>(task: tokio::task::JoinHandle<T>) -> T {
        timeout(LIMIT, task).await.unwrap().unwrap()
    }

    #[test]
    fn only_session_0_and_the_running_services_process_pass() {
        let service = |pid: Option<u32>| -> io::Result<Option<u32>> { Ok(pid) };
        let pass = |server, pid| verdict(server, service(pid)).unwrap();
        assert_eq!(pass(Some((0, 42)), Some(42)), Verdict::Service);
        // Another process in session 0: a service of another account.
        assert_eq!(pass(Some((0, 41)), Some(42)), Verdict::NotTheService);
        // The service is not running: whatever serves the name is not it.
        assert_eq!(pass(Some((0, 42)), None), Verdict::NotTheService);
        // The user's session, even with the right process number.
        assert_eq!(pass(Some((2, 42)), Some(42)), Verdict::NotTheService);
        // Windows did not say who the server is.
        assert_eq!(pass(None, Some(42)), Verdict::NotTheService);
        assert_eq!(pass(Some((0, 0)), Some(0)), Verdict::NotTheService);
        let denied = verdict(Some((0, 42)), Err(io::ErrorKind::PermissionDenied.into()));
        assert_eq!(denied.unwrap_err().code(), "helper_cannot_check");
    }

    #[tokio::test]
    async fn without_the_service_nothing_runs() {
        let err = exchange(&test_pipe("none"), b"{}", LIMIT, any_server)
            .await
            .unwrap_err();
        assert_eq!(err.code(), "helper_not_running", "{err}");
    }

    #[tokio::test]
    async fn a_request_over_64_kib_is_refused_before_the_pipe_opens() {
        let large = vec![b' '; MAX_MESSAGE + 1];
        let err = exchange(&test_pipe("none"), &large, LIMIT, any_server)
            .await
            .unwrap_err();
        assert_eq!(err.code(), "helper_request_too_large", "{err}");
    }

    /// The check reads the server of the handle it is given: this test's
    /// own process, in its own session.
    #[tokio::test]
    async fn the_check_names_the_servers_process() {
        let name = test_pipe("whose");
        let _server = server(&name);
        let client = open(&name).unwrap();
        let (_, process) = server_of(client.as_raw_handle()).unwrap();
        assert_eq!(process, std::process::id());
    }

    /// A server of this test is not the service, and never receives a
    /// byte.
    #[tokio::test]
    async fn another_program_behind_the_name_gets_nothing() {
        let name = test_pipe("other");
        let mut server = server(&name);
        let received = tokio::spawn(async move {
            let mut got = Vec::new();
            // The client may connect and go before this waits: then there
            // is nothing to read either.
            if server.connect().await.is_ok() {
                let _ = server.read_to_end(&mut got).await;
            }
            got
        });
        let check = |pipe| verdict(server_of(pipe), service_process_id(SERVICE_NAME));
        let err = exchange(&name, b"secret", LIMIT, check).await.unwrap_err();
        assert_eq!(err.code(), "helper_not_the_service", "{err}");
        assert!(within(received).await.is_empty());
    }

    /// The server may read who the client is, and act as it for nothing
    /// more.
    #[tokio::test]
    async fn the_server_sees_identification_only() {
        let name = test_pipe("level");
        let mut server = server(&name);
        let level = tokio::spawn(async move {
            server.connect().await.unwrap();
            read_request(&mut server).await;
            // SAFETY: the handle is the connected server end, after a read
            // as `ImpersonateNamedPipeClient` needs. The thread's token is
            // read, closed by `OwnedHandle`, and the thread reverts before
            // anything else runs on it; a failed revert ends the process.
            let level = unsafe {
                assert_ne!(ImpersonateNamedPipeClient(server.as_raw_handle()), 0);
                let mut token = null_mut();
                let opened = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token);
                let mut level: SECURITY_IMPERSONATION_LEVEL = 0;
                let mut used = 0;
                let read = opened != 0 && {
                    let token = OwnedHandle::from_raw_handle(token);
                    GetTokenInformation(
                        token.as_raw_handle(),
                        TokenImpersonationLevel,
                        (&raw mut level).cast(),
                        size_of::<SECURITY_IMPERSONATION_LEVEL>() as u32,
                        &mut used,
                    ) != 0
                };
                // A thread left acting as the client must not go on.
                if RevertToSelf() == 0 {
                    std::process::abort();
                }
                assert!(read);
                level
            };
            answer(&mut server, b"ok").await;
            level
        });
        exchange(&name, b"{}", LIMIT, any_server).await.unwrap();
        assert_eq!(within(level).await, SecurityIdentification);
    }

    #[tokio::test]
    async fn one_request_and_one_answer_each_framed() {
        let name = test_pipe("framed");
        let mut server = server(&name);
        let request = tokio::spawn(async move {
            server.connect().await.unwrap();
            let request = read_request(&mut server).await;
            answer(&mut server, b"ok").await;
            request
        });
        let got = exchange(&name, b"{\"version\":1}", LIMIT, any_server)
            .await
            .unwrap();
        assert_eq!(got, b"ok");
        assert_eq!(within(request).await, b"{\"version\":1}");
    }

    /// The server closed its end, or disconnected the client as the service
    /// does, before its answer: the request may have run.
    #[tokio::test]
    async fn an_end_before_the_answer_is_no_answer() {
        for disconnect in [false, true] {
            let name = test_pipe(&format!("end-{disconnect}"));
            let mut server = server(&name);
            let reader = tokio::spawn(async move {
                server.connect().await.unwrap();
                read_request(&mut server).await;
                if disconnect {
                    server.disconnect().unwrap();
                }
            });
            let err = exchange(&name, b"{}", LIMIT, any_server).await.unwrap_err();
            assert_eq!(err.code(), "helper_no_answer", "{err}");
            within(reader).await;
        }
    }

    /// Every instance has a client for a while: the request is tried
    /// again, and goes out once an instance is free. The free one is a new
    /// instance: a tokio server end cannot be disconnected and used again
    /// (mio keeps the error of its last read), unlike the service's own
    /// blocking server end.
    #[tokio::test]
    async fn a_busy_pipe_is_tried_again() {
        let name = test_pipe("busy");
        let busy = ServerOptions::new()
            .first_pipe_instance(true)
            .max_instances(2)
            .create(&name)
            .unwrap();
        let first = open(&name).unwrap();
        busy.connect().await.unwrap();
        let served_name = name.clone();
        let served = tokio::spawn(async move {
            sleep(Duration::from_millis(150)).await;
            // Made while the first still exists, so the name never goes.
            let mut free = ServerOptions::new()
                .max_instances(2)
                .create(&served_name)
                .unwrap();
            free.connect().await.unwrap();
            let request = read_request(&mut free).await;
            answer(&mut free, b"next").await;
            request
        });
        let got = exchange(&name, b"second", LIMIT, any_server).await.unwrap();
        assert_eq!(got, b"next");
        assert_eq!(within(served).await, b"second");
        drop((first, busy));
    }

    /// Busy until the limit: nothing went out.
    #[tokio::test]
    async fn a_pipe_busy_until_the_limit_is_busy() {
        let name = test_pipe("full");
        let server = server(&name);
        let _first = open(&name).unwrap();
        server.connect().await.unwrap();
        let short = Duration::from_millis(300);
        let err = exchange(&name, b"{}", short, any_server).await.unwrap_err();
        assert_eq!(err.code(), "helper_busy", "{err}");
    }

    /// Read only: an always-running Windows service has a process, and a
    /// name that is no service has none.
    #[test]
    fn the_service_manager_names_a_running_service_process() {
        assert!(service_process_id("RpcSs").unwrap().is_some());
        assert_eq!(service_process_id("MarsaNoSuchService").unwrap(), None);
    }

    /// `marsad`'s handlers run on tokio's threads, so the future must be
    /// `Send`.
    #[test]
    fn the_request_future_is_send() {
        fn is_send<F: Future + Send>(_: &F) {}
        // Made, never polled: nothing opens.
        is_send(&send(b"{}", LIMIT));
    }
}
