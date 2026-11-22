//! Who runs the process: the user's SID, the elevation of a token, and a
//! program run through the UAC prompt.

use std::ffi::OsStr;
use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    ERROR_CANCELLED, LocalFree, RPC_E_CHANGED_MODE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenUser,
};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    ShellExecuteExW,
};

use super::detached::{append_arg, check_program};
use super::wide;

/// The SID of the user this process runs as, in the form Windows prints
/// (`S-1-5-21-...`). The helper service serves this user only (ADR 0018).
///
/// # Errors
///
/// The Win32 error.
pub fn current_user_sid() -> io::Result<String> {
    let mut token = null_mut();
    // SAFETY: the pseudo handle of this process needs no close; `token` is a
    // valid out pointer, and the token is checked, then owned.
    let token = unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        OwnedHandle::from_raw_handle(token)
    };
    // A TOKEN_USER and its SID fit in 256 bytes; `u64` keeps the buffer
    // aligned for the struct.
    let mut buffer = [0u64; 32];
    let mut used = 0;
    // SAFETY: the buffer and the size passed match; on success it holds a
    // TOKEN_USER whose SID pointer points into the same buffer.
    let read = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size_of::<[u64; 32]>() as u32,
            &mut used,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut text: *mut u16 = null_mut();
    // SAFETY: `buffer` holds a TOKEN_USER (above) and outlives these lines.
    // The string `ConvertSidToStringSidW` allocates is read up to its NUL,
    // copied, and freed with `LocalFree`.
    unsafe {
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
            return Err(io::Error::last_os_error());
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

/// Whether this process runs with an elevated token: started with "Run as
/// administrator", or by an administrator while UAC is off. `postgres.exe`
/// refuses to run then (ADR 0023, section 7). Read only.
///
/// # Errors
///
/// The Win32 error.
pub fn is_elevated() -> io::Result<bool> {
    // SAFETY: the pseudo handle of this process (the value -1, which
    // `borrow_raw` allows) is valid for as long as the process runs, and
    // needs no close.
    let process = unsafe { BorrowedHandle::borrow_raw(GetCurrentProcess()) };
    token_elevated(process)
}

/// [`is_elevated`] for the process `pid`: `marsad`, for `marsa doctor`.
/// Read only: the process is opened for `PROCESS_QUERY_LIMITED_INFORMATION`
/// alone, and closed after.
///
/// # Errors
///
/// The Win32 error, for example access denied for another user's process.
pub fn process_is_elevated(pid: u32) -> io::Result<bool> {
    // SAFETY: `OpenProcess` takes plain values. The handle is checked for
    // null (its failure value) and owned at once, so it is closed on every
    // path.
    let process = unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        OwnedHandle::from_raw_handle(handle)
    };
    token_elevated(process.as_handle())
}

/// Whether the token of `process` is elevated (`TokenElevation`).
fn token_elevated(process: BorrowedHandle<'_>) -> io::Result<bool> {
    let mut token = null_mut();
    // SAFETY: `process` is an open process handle for the whole call (a
    // `BorrowedHandle` is), with at least the query right that
    // `OpenProcessToken` needs; `token` is a valid out pointer. The token is
    // checked, then owned, so it is closed on every path.
    let token = unsafe {
        if OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        OwnedHandle::from_raw_handle(token)
    };
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut used = 0;
    // SAFETY: `TokenElevation` fills a `TOKEN_ELEVATION`, which `elevation`
    // is, and the size passed is its own; `elevation`, `used`, and the open
    // token live through the call.
    let read = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut used,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(elevation.TokenIsElevated != 0)
}

/// What a program run through the UAC prompt gave.
#[derive(Debug)]
pub enum Elevated {
    /// It ran and ended with this exit code.
    Exited(u32),
    /// The user said no at the prompt.
    Cancelled,
    /// It still runs at the limit. The caller may wait more, and must not
    /// start it again meanwhile.
    TimedOut(ElevatedProcess),
}

/// A program started through the UAC prompt that still runs.
#[derive(Debug)]
pub struct ElevatedProcess(OwnedHandle);

impl ElevatedProcess {
    /// Waits up to `limit` more: `Some(code)` once it has ended, `None` if
    /// it still runs.
    ///
    /// # Errors
    ///
    /// The Win32 error of the wait or of the exit code.
    pub fn wait(&self, limit: Duration) -> io::Result<Option<u32>> {
        // `u32::MAX` is `INFINITE`.
        let millis =
            u32::try_from(limit.as_millis()).map_or(u32::MAX - 1, |ms| ms.min(u32::MAX - 1));
        // SAFETY: the handle is the open process this struct owns.
        match unsafe { WaitForSingleObject(self.0.as_raw_handle(), millis) } {
            WAIT_OBJECT_0 => {
                let mut code = 0;
                // SAFETY: the handle is the open process, and `code` a valid
                // out pointer.
                if unsafe { GetExitCodeProcess(self.0.as_raw_handle(), &mut code) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(code))
            }
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

/// Runs `program` with `args` as an administrator, through the UAC prompt
/// (`ShellExecuteExW` with the verb `runas`), and waits up to `limit` for it
/// to end. For the helper's `install` and `uninstall` (ADR 0018).
///
/// Blocking: `ShellExecuteExW` returns once the user has answered the
/// prompt, which has no limit of its own and which `limit` does not cover,
/// so call it from a blocking thread. The elevated program's output is not
/// available; its exit code is the answer. Its window, if any, is hidden;
/// the prompt itself is drawn by Windows and is not.
///
/// # Errors
///
/// `InvalidInput` for a program that is not an absolute `.exe` path or
/// whose path holds a quote, or for an argument with a NUL; otherwise the
/// Win32 error. An error means the program did not start.
pub fn run_elevated(program: &Path, args: &[&OsStr], limit: Duration) -> io::Result<Elevated> {
    /// `SW_HIDE`.
    const HIDE: i32 = 0;
    check_program(program)?;
    let file = wide(program.as_os_str())?;
    let verb = wide(OsStr::new("runas"))?;
    let mut parameters = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            parameters.push(u16::from(b' '));
        }
        append_arg(&mut parameters, arg)?;
    }
    parameters.push(0);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: parameters.as_ptr(),
        nShow: HIDE,
        ..SHELLEXECUTEINFOW::default()
    };
    // COM for this thread, as `ShellExecuteExW` asks. A failure stops here,
    // before any prompt; `RPC_E_CHANGED_MODE` means the thread has COM
    // already, in another mode, which serves as well.
    let mode = (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).cast_unsigned();
    // SAFETY: the reserved pointer is null, as the function requires.
    let com = unsafe { CoInitializeEx(null(), mode) };
    if com < 0 && com != RPC_E_CHANGED_MODE {
        return Err(io::Error::from_raw_os_error(com));
    }
    // SAFETY: `info` and the three strings it points to live through the
    // call; the strings are NUL-terminated.
    let launched = unsafe { ShellExecuteExW(&mut info) };
    let launch_error = io::Error::last_os_error();
    if com >= 0 {
        // SAFETY: pairs the successful `CoInitializeEx` above.
        unsafe { CoUninitialize() };
    }
    if launched == 0 {
        if launch_error.raw_os_error() == Some(ERROR_CANCELLED as i32) {
            return Ok(Elevated::Cancelled);
        }
        return Err(launch_error);
    }
    if info.hProcess.is_null() {
        return Err(io::Error::other("Windows gave no process for the program"));
    }
    // SAFETY: with `SEE_MASK_NOCLOSEPROCESS` the process handle is the
    // caller's to close; it is owned from here.
    let process = ElevatedProcess(unsafe { OwnedHandle::from_raw_handle(info.hProcess) });
    Ok(match process.wait(limit) {
        Ok(Some(code)) => Elevated::Exited(code),
        // It started, so a failed wait is not "did not start": the caller
        // keeps the process and may wait again.
        Ok(None) | Err(_) => Elevated::TimedOut(process),
    })
}
