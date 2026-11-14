//! S-11: Windows spikes for the Phase 1 gate. Throwaway code.
//!
//! Small wrappers over `windows-sys`, shared by the spike binaries. Every
//! function reports what Windows answered; nothing here is product code.
#![cfg(windows)]

pub mod jobtree;

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::{ManuallyDrop, size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_CONNECTED, GetHandleInformation, GetLastError, HANDLE,
    HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, LocalFree, SetHandleInformation,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, ImpersonateLoggedOnUser, LOGON32_LOGON_INTERACTIVE,
    LOGON32_PROVIDER_DEFAULT, LogonUserW, PSECURITY_DESCRIPTOR, RevertToSelf, SECURITY_ATTRIBUTES,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Console::{
    GetConsoleWindow, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    GetNamedPipeClientSessionId, GetNamedPipeServerProcessId, GetNamedPipeServerSessionId,
    ImpersonateNamedPipeClient, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DETACHED_PROCESS,
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThread, OpenProcessToken, OpenThreadToken,
    PROCESS_INFORMATION, STARTUPINFOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible;

/// The largest message on the spike pipe, like the helper's request limit.
pub const MAX_MESSAGE: usize = 64 * 1024;

/// A NUL-terminated UTF-16 copy of `text`.
pub fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text).encode_wide().chain(Some(0)).collect()
}

fn last() -> io::Error {
    io::Error::last_os_error()
}

/// Creates a job that kills its processes when its last handle closes, and
/// puts the calling process in it. Every child started later joins the job.
/// The handle is returned so it lives as long as the process.
pub fn join_kill_on_close_job() -> io::Result<HANDLE> {
    // SAFETY: plain Win32 calls with valid pointers to local values; the job
    // handle is checked before use and closed on every error path.
    unsafe {
        let job = CreateJobObjectW(null(), null());
        if job.is_null() {
            return Err(last());
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
        let set = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const info).cast(),
            size,
        );
        if set == 0 || AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
            let err = last();
            CloseHandle(job);
            return Err(err);
        }
        Ok(job)
    }
}

/// Whether the calling process is in any job.
pub fn in_job() -> io::Result<bool> {
    let mut result = 0;
    // SAFETY: a null job handle means "any job"; `result` is a valid BOOL.
    let ok = unsafe { IsProcessInJob(GetCurrentProcess(), null_mut(), &mut result) };
    if ok == 0 {
        return Err(last());
    }
    Ok(result != 0)
}

/// The Terminal Services session of a process. Services run in session 0.
pub fn session_of(pid: u32) -> io::Result<u32> {
    let mut session = 0;
    // SAFETY: `session` is a valid out pointer.
    if unsafe { ProcessIdToSessionId(pid, &mut session) } == 0 {
        return Err(last());
    }
    Ok(session)
}

pub fn current_session() -> io::Result<u32> {
    // SAFETY: no arguments.
    session_of(unsafe { GetCurrentProcessId() })
}

/// `(has a console window, the window is visible)` for the calling process.
pub fn console_window() -> (bool, bool) {
    // SAFETY: no arguments; the window handle is only passed back to Windows.
    unsafe {
        let window = GetConsoleWindow();
        if window.is_null() {
            (false, false)
        } else {
            (true, IsWindowVisible(window) != 0)
        }
    }
}

/// Clears the inherit flag on the three standard handles, so a child started
/// with `bInheritHandles = TRUE` does not get the caller's pipes.
pub fn stop_std_handles_inheritance() -> Vec<io::Result<()>> {
    [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
        .into_iter()
        .map(|id| {
            // SAFETY: `GetStdHandle` returns a handle owned by the process
            // or null/invalid, which is checked before use.
            unsafe {
                let handle = GetStdHandle(id);
                if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                    return Ok(());
                }
                if SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) == 0 {
                    return Err(last());
                }
                Ok(())
            }
        })
        .collect()
}

/// Clears the inherit flag on every handle of the process that has it, by
/// trying each possible handle value. Returns how many it cleared. A shell
/// can pass more inheritable handles than the three standard ones.
pub fn stop_all_handles_inheritance() -> usize {
    let mut cleared = 0;
    // Handle values are multiples of 4. A short-lived CLI has few of them.
    for value in (4..=0x1_0000usize).step_by(4) {
        let handle = value as HANDLE;
        let mut flags = 0;
        // SAFETY: both calls only read or change the flags of a handle value;
        // a value that is not a handle makes them fail, which is ignored.
        unsafe {
            if GetHandleInformation(handle, &mut flags) != 0
                && flags & HANDLE_FLAG_INHERIT != 0
                && SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) != 0
            {
                cleared += 1;
            }
        }
    }
    cleared
}

/// Starts `program` with `args` through `CreateProcessW` with
/// `bInheritHandles = FALSE`: detached, no console, and no handle of the
/// caller at all. Returns the child's pid. The spike's paths have no quotes,
/// so wrapping each argument in quotes is enough here.
pub fn spawn_without_handles(program: &str, args: &[&str]) -> io::Result<u32> {
    let application = wide(program);
    let mut line = format!("\"{program}\"");
    for arg in args {
        line.push_str(&format!(" \"{arg}\""));
    }
    let mut command_line = wide(&line);
    // SAFETY: the strings are NUL-terminated and outlive the call; the
    // command line is a mutable buffer, as `CreateProcessW` requires; both
    // returned handles are closed.
    unsafe {
        let mut startup: STARTUPINFOW = zeroed();
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        let mut info: PROCESS_INFORMATION = zeroed();
        let ok = CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            0,
            DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            null(),
            null(),
            &startup,
            &mut info,
        );
        if ok == 0 {
            return Err(last());
        }
        CloseHandle(info.hThread);
        CloseHandle(info.hProcess);
        Ok(info.dwProcessId)
    }
}

/// `C:\Windows\System32`, asked from Windows, not from the environment.
pub fn system_directory() -> io::Result<String> {
    let mut buffer = [0u16; 260];
    // SAFETY: the buffer and its length match.
    let len = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if len == 0 || len > buffer.len() {
        return Err(last());
    }
    Ok(String::from_utf16_lossy(&buffer[..len]))
}

/// The user SID of a token, as text (`S-1-5-21-...`).
fn token_sid(token: HANDLE) -> io::Result<String> {
    // A TOKEN_USER with its SID fits easily in 256 bytes. `u64` keeps the
    // buffer aligned for the struct.
    let mut buffer = [0u64; 32];
    let mut used = 0;
    // SAFETY: the buffer is large enough and aligned for TOKEN_USER; the SID
    // pointer inside it stays valid while `buffer` lives; the string from
    // `ConvertSidToStringSidW` is copied and then freed with `LocalFree`.
    unsafe {
        let size = size_of::<[u64; 32]>() as u32;
        if GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut used,
        ) == 0
        {
            return Err(last());
        }
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut text: *mut u16 = null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
            return Err(last());
        }
        let mut len = 0;
        while *text.add(len) != 0 {
            len += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        Ok(sid)
    }
}

/// The user SID of the calling process.
pub fn process_sid() -> io::Result<String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: `token` is a valid out pointer; the handle is closed below.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(last());
        }
        let sid = token_sid(token);
        CloseHandle(token);
        sid
    }
}

/// The user SID the calling thread impersonates.
fn thread_sid() -> io::Result<String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: as in `process_sid`. `OpenAsSelf = TRUE`: the access check for
    // opening the token uses the process identity, not the client's.
    unsafe {
        if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) == 0 {
            return Err(last());
        }
        let sid = token_sid(token);
        CloseHandle(token);
        sid
    }
}

/// Logs on as a local user and makes the calling thread impersonate it. Every
/// access check on this thread then runs as that user.
pub fn impersonate_user(user: &str, password: &str) -> io::Result<()> {
    let mut token: HANDLE = null_mut();
    // SAFETY: the strings are NUL-terminated and live through the call.
    unsafe {
        let ok = LogonUserW(
            wide(user).as_ptr(),
            wide(".").as_ptr(),
            wide(password).as_ptr(),
            LOGON32_LOGON_INTERACTIVE,
            LOGON32_PROVIDER_DEFAULT,
            &mut token,
        );
        if ok == 0 {
            return Err(last());
        }
        let ok = ImpersonateLoggedOnUser(token);
        let err = last();
        CloseHandle(token);
        if ok == 0 {
            return Err(err);
        }
    }
    Ok(())
}

/// One instance of a byte-mode named pipe that rejects remote clients.
pub struct PipeServer {
    handle: HANDLE,
}

/// What the server learned about a connected client.
#[derive(Debug)]
pub struct ClientInfo {
    pub pid: io::Result<u32>,
    pub session: io::Result<u32>,
    pub sid: io::Result<String>,
}

impl PipeServer {
    /// Creates `\\.\pipe\<name>` with the DACL in `sddl`. With
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE` this fails if the name exists already.
    pub fn create(name: &str, sddl: Option<&str>, first_instance: bool) -> io::Result<Self> {
        let path = wide(&format!(r"\\.\pipe\{name}"));
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        let mut open_mode = PIPE_ACCESS_DUPLEX;
        if first_instance {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        // SAFETY: every pointer is to a local value that outlives the call;
        // the descriptor is freed with `LocalFree` after the pipe exists.
        unsafe {
            if let Some(sddl) = sddl {
                let ok = ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide(sddl).as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    null_mut(),
                );
                if ok == 0 {
                    return Err(last());
                }
            }
            let attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            let handle = CreateNamedPipeW(
                path.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                MAX_MESSAGE as u32,
                MAX_MESSAGE as u32,
                0,
                if sddl.is_some() { &attributes } else { null() },
            );
            let err = last();
            if !descriptor.is_null() {
                LocalFree(descriptor);
            }
            if handle == INVALID_HANDLE_VALUE {
                return Err(err);
            }
            Ok(Self { handle })
        }
    }

    /// Waits for one client.
    pub fn accept(&self) -> io::Result<()> {
        // SAFETY: the handle is a pipe this struct owns; no OVERLAPPED.
        unsafe {
            if ConnectNamedPipe(self.handle, null_mut()) == 0
                && GetLastError() != ERROR_PIPE_CONNECTED
            {
                return Err(last());
            }
        }
        Ok(())
    }

    /// Who is on the other end. The SID comes from impersonating the client,
    /// which needs the client to allow at least `SECURITY_IDENTIFICATION`, and
    /// works only after the first read from the pipe.
    pub fn client(&self) -> ClientInfo {
        let mut pid = 0;
        let mut session = 0;
        // SAFETY: valid out pointers; the impersonation is reverted before
        // the block ends on every path.
        unsafe {
            let pid = if GetNamedPipeClientProcessId(self.handle, &mut pid) == 0 {
                Err(last())
            } else {
                Ok(pid)
            };
            let session = if GetNamedPipeClientSessionId(self.handle, &mut session) == 0 {
                Err(last())
            } else {
                Ok(session)
            };
            let sid = if ImpersonateNamedPipeClient(self.handle) == 0 {
                Err(last())
            } else {
                let sid = thread_sid();
                RevertToSelf();
                sid
            };
            ClientInfo { pid, session, sid }
        }
    }

    /// The pipe as a `File`, for `read` and `write`. Not closed on drop.
    fn file(&self) -> ManuallyDrop<File> {
        // SAFETY: the handle is open; `ManuallyDrop` keeps `File` from
        // closing a handle that `PipeServer` still owns.
        ManuallyDrop::new(unsafe { File::from_raw_handle(self.handle) })
    }

    pub fn read_message(&self) -> io::Result<Vec<u8>> {
        read_message(&mut *self.file())
    }

    pub fn write_message(&self, bytes: &[u8]) -> io::Result<()> {
        write_message(&mut *self.file(), bytes)?;
        // SAFETY: the handle is open. Waits until the client read the reply.
        unsafe { FlushFileBuffers(self.handle) };
        Ok(())
    }

    /// Drops the client so the next `accept` can run.
    pub fn disconnect(&self) {
        // SAFETY: the handle is open.
        unsafe { DisconnectNamedPipe(self.handle) };
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        // SAFETY: the handle is open and owned by this struct.
        unsafe { CloseHandle(self.handle) };
    }
}

/// A 4-byte little-endian length, then the bytes.
pub fn read_message(pipe: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    pipe.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut bytes = vec![0; len];
    pipe.read_exact(&mut bytes)?;
    Ok(bytes)
}

pub fn write_message(pipe: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    pipe.write_all(&(bytes.len() as u32).to_le_bytes())?;
    pipe.write_all(bytes)?;
    pipe.flush()
}

/// The session and the process of the server behind an open pipe client.
pub fn pipe_server_of(client: &File) -> (io::Result<u32>, io::Result<u32>) {
    use std::os::windows::io::AsRawHandle;
    let handle = client.as_raw_handle();
    let mut session = 0;
    let mut pid = 0;
    // SAFETY: the handle is an open pipe; valid out pointers.
    unsafe {
        let session = if GetNamedPipeServerSessionId(handle, &mut session) == 0 {
            Err(last())
        } else {
            Ok(session)
        };
        let pid = if GetNamedPipeServerProcessId(handle, &mut pid) == 0 {
            Err(last())
        } else {
            Ok(pid)
        };
        (session, pid)
    }
}
