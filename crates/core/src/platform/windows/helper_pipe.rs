//! The client end of the Windows helper service (ADR 0018, task M4-19g1).
//!
//! The service `MarsaHelper` runs as LocalSystem and serves one request per
//! connection on `\\.\pipe\marsa-helper`: a 4-byte little-endian length,
//! then the JSON request; the answer comes back framed the same way. Both
//! are at most 64 KiB.
//!
//! Before it sends anything, the client checks that the server is the
//! service: the server runs in session 0, and its process is the one the
//! service manager names for `MarsaHelper`. A program of the user that took
//! the name while the service was down is in the user's session; a service
//! of another account is in session 0 but has another process.
//!
//! The pipe grants the user read, and write without `FILE_APPEND_DATA`
//! (which on a pipe is the right to add an instance), so the client opens it
//! with exactly `FILE_GENERIC_READ | FILE_WRITE_DATA`.

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
use tokio::time::{Instant, sleep, timeout_at};
use windows_sys::Win32::Foundation::{ERROR_PIPE_BUSY, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ, FILE_WRITE_DATA, SECURITY_IDENTIFICATION,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeServerProcessId, GetNamedPipeServerSessionId};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_HANDLE,
    SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_STATUS_PROCESS,
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

/// Why a request got no answer.
#[derive(Debug, thiserror::Error)]
pub enum PipeError {
    /// No pipe: the service is not installed, or not running.
    #[error("the helper service is not running")]
    NotRunning,
    /// Something else serves the pipe's name. Nothing was sent.
    #[error("the program behind the helper's pipe is not the helper service")]
    NotTheService,
    /// The request is over [`MAX_MESSAGE`]. Nothing was sent.
    #[error("the request is larger than 64 KiB")]
    TooLarge,
    /// No answer in time. The service may still run the request.
    #[error("the helper service did not answer in {} seconds", .0.as_secs())]
    TimedOut(Duration),
    /// The service closed the connection before its answer. Whether the ops
    /// ran is unknown: the caller reads the state again.
    #[error("the helper service closed the connection without an answer")]
    NoAnswer,
    /// Any other failure of the pipe.
    #[error("the helper's pipe failed: {0}")]
    Io(#[source] io::Error),
}

impl PipeError {
    /// A stable code, for the UI's strings.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotRunning => "helper_not_running",
            Self::NotTheService => "helper_not_the_service",
            Self::TooLarge => "helper_request_too_large",
            Self::TimedOut(_) => "helper_timed_out",
            Self::NoAnswer => "helper_no_answer",
            Self::Io(_) => "helper_pipe_failed",
        }
    }
}

/// Sends `request` (the JSON of a helper request) to the service and
/// returns its answer, within `limit` in all. The service runs one client
/// at a time, so a busy pipe is tried again until `limit`.
///
/// # Errors
///
/// [`PipeError`]: nothing is sent unless the server is the service.
pub async fn send(request: &[u8], limit: Duration) -> Result<Vec<u8>, PipeError> {
    exchange(PIPE_NAME, request, limit, is_the_service).await
}

/// [`send`] to the pipe `name`, with `is_server` as the check of the server.
async fn exchange(
    name: &str,
    request: &[u8],
    limit: Duration,
    is_server: impl Fn(HANDLE) -> bool,
) -> Result<Vec<u8>, PipeError> {
    let len = u32::try_from(request.len())
        .ok()
        .filter(|_| request.len() <= MAX_MESSAGE)
        .ok_or(PipeError::TooLarge)?;
    let deadline = Instant::now() + limit;
    let file = loop {
        match open(name) {
            Ok(file) => break file,
            Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                if Instant::now() >= deadline {
                    return Err(PipeError::TimedOut(limit));
                }
                sleep(RETRY).await;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(PipeError::NotRunning),
            Err(err) => return Err(PipeError::Io(err)),
        }
    };
    if !is_server(file.as_raw_handle()) {
        return Err(PipeError::NotTheService);
    }
    // SAFETY: the handle is a pipe client opened for overlapped I/O (the
    // flag in `open`); `into_raw_handle` hands it over, and the
    // `NamedPipeClient` owns and closes it from here.
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
        Ok(Err(err))
            if matches!(
                err.kind(),
                io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
            ) =>
        {
            Err(PipeError::NoAnswer)
        }
        Ok(Err(err)) => Err(PipeError::Io(err)),
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

/// Whether the server end of `pipe` is the running `MarsaHelper` service.
fn is_the_service(pipe: HANDLE) -> bool {
    let mut session = u32::MAX;
    let mut process = 0;
    // SAFETY: `pipe` is an open pipe client handle, and both out pointers
    // are valid.
    let known = unsafe {
        GetNamedPipeServerSessionId(pipe, &mut session) != 0
            && GetNamedPipeServerProcessId(pipe, &mut process) != 0
    };
    known && session == 0 && process != 0 && service_process_id(SERVICE_NAME) == Some(process)
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

/// The process of the running service `name`, as the service manager
/// reports it; `None` if it is not installed or not running. Read only.
pub fn service_process_id(name: &str) -> Option<u32> {
    let name = wide(OsStr::new(name)).ok()?;
    // SAFETY: null machine and database names mean this machine's active
    // database. The handle is checked, then owned by `Sc`.
    let manager = unsafe { OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        return None;
    }
    let manager = Sc(manager);
    // SAFETY: `name` is NUL-terminated and lives through the call; the
    // handle is checked, then owned by `Sc`.
    let service = unsafe { OpenServiceW(manager.0, name.as_ptr(), SERVICE_QUERY_STATUS) };
    if service.is_null() {
        return None;
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
    (read != 0 && status.dwProcessId != 0).then_some(status.dwProcessId)
}

#[cfg(test)]
mod tests {
    use tokio::net::windows::named_pipe::ServerOptions;

    use super::*;

    /// A pipe name of this test run only.
    fn test_pipe(test: &str) -> String {
        format!(r"\\.\pipe\marsa-core-test-{}-{test}", std::process::id())
    }

    const LIMIT: Duration = Duration::from_secs(20);

    #[tokio::test]
    async fn without_the_service_nothing_runs() {
        let err = exchange(&test_pipe("none"), b"{}", LIMIT, |_| true)
            .await
            .unwrap_err();
        assert!(matches!(err, PipeError::NotRunning), "{err}");
        assert_eq!(err.code(), "helper_not_running");
    }

    #[tokio::test]
    async fn a_request_over_64_kib_is_refused_before_the_pipe_opens() {
        let large = vec![b' '; MAX_MESSAGE + 1];
        let err = exchange(&test_pipe("none"), &large, LIMIT, |_| true)
            .await
            .unwrap_err();
        assert!(matches!(err, PipeError::TooLarge), "{err}");
    }

    /// A server of this test: not in session 0 under the service's process,
    /// so not the service. It never receives a byte.
    #[tokio::test]
    async fn another_program_behind_the_name_gets_nothing() {
        let name = test_pipe("other");
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let received = tokio::spawn(async move {
            let mut got = Vec::new();
            // The client may connect and go before this waits: then there
            // is nothing to read either.
            if server.connect().await.is_ok() {
                let _ = server.read_to_end(&mut got).await;
            }
            got
        });
        let err = exchange(&name, b"secret", LIMIT, is_the_service)
            .await
            .unwrap_err();
        assert!(matches!(err, PipeError::NotTheService), "{err}");
        assert!(received.await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn one_request_and_one_answer_each_framed() {
        let name = test_pipe("framed");
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let answer = tokio::spawn(async move {
            server.connect().await.unwrap();
            let mut len = [0u8; 4];
            server.read_exact(&mut len).await.unwrap();
            let mut request = vec![0; u32::from_le_bytes(len) as usize];
            server.read_exact(&mut request).await.unwrap();
            server.write_all(&2u32.to_le_bytes()).await.unwrap();
            server.write_all(b"ok").await.unwrap();
            request
        });
        let got = exchange(&name, b"{\"version\":1}", LIMIT, |_| true)
            .await
            .unwrap();
        assert_eq!(got, b"ok");
        assert_eq!(answer.await.unwrap(), b"{\"version\":1}");
    }

    #[tokio::test]
    async fn a_close_before_the_answer_is_no_answer() {
        let name = test_pipe("closed");
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let reader = tokio::spawn(async move {
            server.connect().await.unwrap();
            let mut len = [0u8; 4];
            server.read_exact(&mut len).await.unwrap();
            // Dropped here, without an answer.
        });
        let err = exchange(&name, b"{}", LIMIT, |_| true).await.unwrap_err();
        reader.await.unwrap();
        assert!(matches!(err, PipeError::NoAnswer), "{err}");
    }

    /// One instance with a client: the next client is busy until the
    /// limit.
    #[tokio::test]
    async fn a_busy_pipe_is_tried_until_the_limit() {
        let name = test_pipe("busy");
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .max_instances(1)
            .create(&name)
            .unwrap();
        let _first = open(&name).unwrap();
        server.connect().await.unwrap();
        let short = Duration::from_millis(300);
        let err = exchange(&name, b"{}", short, |_| true).await.unwrap_err();
        assert!(matches!(err, PipeError::TimedOut(_)), "{err}");
    }

    /// Read only: an always-running Windows service has a process, and a
    /// name that is no service has none.
    #[test]
    fn the_service_manager_names_a_running_service_process() {
        assert!(service_process_id("RpcSs").is_some());
        assert_eq!(service_process_id("MarsaNoSuchService"), None);
    }
}
