//! The helper's Win32 calls (ADR 0018): the system folders, and the server
//! end of the named pipe. The `unsafe` code of the helper on Windows is in
//! this file only; the blocks are short, and every result is checked.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::{ManuallyDrop, size_of};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{
    ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, RevertToSelf, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, ImpersonateNamedPipeClient,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};
use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

/// `MAX_PATH`: the system folder is `C:\Windows\System32` or close to it,
/// far below this.
const MAX_PATH: usize = 260;
/// The pipe's buffer in each direction: one request or answer of 64 KiB.
const BUFFER: u32 = 64 * 1024;

/// The Windows system folder, as Windows itself reports it. Not from the
/// `SystemRoot` variable: whoever starts the helper sets its environment.
///
/// # Errors
///
/// The Win32 error, or `InvalidData` for a path that does not fit (it never
/// should).
pub fn system_dir() -> io::Result<PathBuf> {
    let mut buffer = [0u16; MAX_PATH + 1];
    // SAFETY: `buffer` is a valid, writable array, and the length passed is
    // its own length in UTF-16 units, so the call cannot write past it.
    let len = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    match len {
        0 => Err(io::Error::last_os_error()),
        // On success the length excludes the final NUL, so it is below the
        // buffer's. A larger value is the size a longer buffer would need.
        len if len >= buffer.len() => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the Windows system folder path is too long",
        )),
        len => Ok(PathBuf::from(OsString::from_wide(&buffer[..len]))),
    }
}

/// `Program Files` (64-bit), as Windows reports it. A fixed known folder: no
/// user setting moves it, unlike the `ProgramFiles` variable, which a user
/// can set for their own programs, elevated ones included.
///
/// # Errors
///
/// The `HRESULT` of `SHGetKnownFolderPath`.
pub fn program_files() -> io::Result<PathBuf> {
    let mut path = null_mut();
    // SAFETY: `FOLDERID_ProgramFiles` is a static GUID, the token is null
    // (the current user), and `path` is a valid out pointer. The caller frees
    // `path` with `CoTaskMemFree` whether the call works or not, and
    // `CoTaskMemFree` accepts null. On success `path` is a NUL-terminated
    // string, read up to its NUL and copied before the free.
    let (result, folder) = unsafe {
        let result = SHGetKnownFolderPath(
            &FOLDERID_ProgramFiles,
            KF_FLAG_DEFAULT as u32,
            null_mut(),
            &mut path,
        );
        let folder = (result >= 0 && !path.is_null()).then(|| {
            let mut len = 0;
            while *path.add(len) != 0 {
                len += 1;
            }
            PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(path, len)))
        });
        CoTaskMemFree(path.cast());
        (result, folder)
    };
    match folder {
        Some(folder) => Ok(folder),
        None if result < 0 => Err(io::Error::from_raw_os_error(result)),
        // Success with no string: not documented, and not a folder.
        None => Err(io::Error::other("Windows gave no Program Files folder")),
    }
}

/// The service's end of the named pipe (ADR 0018): one instance, byte mode,
/// local clients only, and a DACL for LocalSystem and one user.
pub struct PipeServer {
    handle: OwnedHandle,
}

impl PipeServer {
    /// Makes the pipe `name` (`\\.\pipe\...`). It fails if the name exists
    /// already (`FILE_FLAG_FIRST_PIPE_INSTANCE`), so a program that took the
    /// name first never receives a client meant for the service; and with one
    /// instance at most, nobody can add another later.
    ///
    /// The DACL: full access for LocalSystem, read and write for `user_sid`,
    /// nothing for anyone else. `P` keeps inherited entries out.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a SID with anything but digits, `S`, and `-` in it
    /// (it goes into the SDDL text). Otherwise the Win32 error.
    pub fn create(name: &str, user_sid: &str) -> io::Result<Self> {
        let plain = user_sid.starts_with("S-")
            && user_sid
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b'S' || b == b'-');
        if !plain {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the SID is not plain",
            ));
        }
        let sddl = wide(&format!("D:P(A;;GA;;;SY)(A;;GRGW;;;{user_sid})"));
        let name = wide(name);
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `sddl` and `name` are NUL-terminated and live through both
        // calls. `descriptor` is a valid out pointer; Windows allocates the
        // descriptor, it is used only during `CreateNamedPipeW` (which copies
        // it into the pipe), and it is freed with `LocalFree` on every path
        // after that. The returned handle is checked before it is owned.
        unsafe {
            let converted = ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            );
            if converted == 0 {
                return Err(io::Error::last_os_error());
            }
            let attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            let handle = CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                BUFFER,
                BUFFER,
                0,
                &attributes,
            );
            let err = io::Error::last_os_error();
            LocalFree(descriptor);
            if handle == INVALID_HANDLE_VALUE {
                return Err(err);
            }
            Ok(Self {
                handle: OwnedHandle::from_raw_handle(handle),
            })
        }
    }

    /// Waits for the next client.
    ///
    /// # Errors
    ///
    /// The Win32 error. A client that connected before the call is fine.
    pub fn accept(&self) -> io::Result<()> {
        // SAFETY: the handle is the open pipe this struct owns; no
        // OVERLAPPED, so the call blocks until a client connects.
        let connected = unsafe { ConnectNamedPipe(self.raw(), null_mut()) };
        // SAFETY: no arguments; it reads this thread's last error.
        if connected == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// One message: a 4-byte little-endian length, then at most `max` bytes.
    ///
    /// # Errors
    ///
    /// `InvalidData` for a longer message, or the read error.
    pub fn read_message(&self, max: usize) -> io::Result<Vec<u8>> {
        let mut pipe = self.file();
        let mut len = [0u8; 4];
        pipe.read_exact(&mut len)?;
        let len = u32::from_le_bytes(len) as usize;
        if len > max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the message is too large",
            ));
        }
        let mut bytes = vec![0; len];
        pipe.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    /// One message, framed like [`PipeServer::read_message`], then waits
    /// until the client has read it.
    ///
    /// # Errors
    ///
    /// The write error.
    pub fn write_message(&self, bytes: &[u8]) -> io::Result<()> {
        let len = u32::try_from(bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too large"))?;
        let mut pipe = self.file();
        pipe.write_all(&len.to_le_bytes())?;
        pipe.write_all(bytes)?;
        pipe.flush()?;
        // SAFETY: the handle is the open pipe this struct owns.
        unsafe { FlushFileBuffers(self.raw()) };
        Ok(())
    }

    /// The user SID of the connected client, as text. Windows reads it from
    /// the client's own token, so the client cannot choose it. Works only
    /// after the first read, and only if the client allowed at least
    /// `SECURITY_IDENTIFICATION`.
    ///
    /// # Errors
    ///
    /// The Win32 error: then the client is treated as unknown.
    pub fn client_sid(&self) -> io::Result<String> {
        // SAFETY: the handle is the open pipe this struct owns. Between the
        // two calls this thread acts as the client, and nothing else runs on
        // it. If `RevertToSelf` failed, the thread would keep the client's
        // rights, so the process ends at once instead (Microsoft's advice).
        unsafe {
            if ImpersonateNamedPipeClient(self.raw()) == 0 {
                return Err(io::Error::last_os_error());
            }
            let sid = thread_user_sid();
            if RevertToSelf() == 0 {
                std::process::abort();
            }
            sid
        }
    }

    /// Drops the client, so the next [`PipeServer::accept`] can run.
    pub fn disconnect(&self) {
        // SAFETY: the handle is the open pipe this struct owns.
        unsafe { DisconnectNamedPipe(self.raw()) };
    }

    fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle()
    }

    /// The pipe as a `File`, for `Read` and `Write`. Never closed by the
    /// `File`: `ManuallyDrop` leaves that to `OwnedHandle`.
    fn file(&self) -> ManuallyDrop<File> {
        // SAFETY: the handle is open and stays open while `self` lives; the
        // `File` is never dropped, so it never closes the handle.
        ManuallyDrop::new(unsafe { File::from_raw_handle(self.raw()) })
    }
}

/// The user SID of the token the current thread acts with, as text.
///
/// # Safety
///
/// The thread must be impersonating a client.
unsafe fn thread_user_sid() -> io::Result<String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: `token` is a valid out pointer. `OpenAsSelf = TRUE`: the
    // access check uses the service's own identity, not the client's.
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `token` was just opened and is closed by `OwnedHandle`.
    token_user_sid(&unsafe { OwnedHandle::from_raw_handle(token) })
}

/// The user SID of the calling process, for the tests: the pipe's DACL
/// names it, and the server must read it back from a client.
#[cfg(test)]
fn process_user_sid() -> io::Result<String> {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token: HANDLE = null_mut();
    // SAFETY: the pseudo handle needs no close; `token` is a valid out
    // pointer, and the opened token is closed by `OwnedHandle`.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        token_user_sid(&OwnedHandle::from_raw_handle(token))
    }
}

/// The user SID of an open token, as text.
fn token_user_sid(token: &OwnedHandle) -> io::Result<String> {
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

/// `text` as NUL-terminated UTF-16.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::thread;

    use super::*;

    /// From the `CreateFile` page: the server may read who the client is,
    /// and act as it for nothing more.
    const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;

    /// A pipe name of this test run only.
    fn test_pipe(test: &str) -> String {
        format!(r"\\.\pipe\marsa-helper-test-{}-{test}", std::process::id())
    }

    fn client(name: &str) -> File {
        OpenOptions::new()
            .read(true)
            .write(true)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(name)
            .unwrap()
    }

    /// A real pipe between two threads of this test: no other process, and
    /// nothing outside it changes.
    #[test]
    fn the_pipe_carries_one_message_each_way_and_names_the_client() {
        let sid = process_user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
        let name = test_pipe("one");
        let server = PipeServer::create(&name, &sid).unwrap();
        // The name is taken: a second server is refused.
        assert!(PipeServer::create(&name, &sid).is_err());

        let path = name.clone();
        let other = thread::spawn(move || {
            let mut pipe = client(&path);
            pipe.write_all(&5u32.to_le_bytes()).unwrap();
            pipe.write_all(b"hello").unwrap();
            let mut len = [0u8; 4];
            pipe.read_exact(&mut len).unwrap();
            let mut reply = vec![0; u32::from_le_bytes(len) as usize];
            pipe.read_exact(&mut reply).unwrap();
            reply
        });
        server.accept().unwrap();
        assert_eq!(server.read_message(64).unwrap(), b"hello");
        assert_eq!(server.client_sid().unwrap(), sid);
        server.write_message(b"world").unwrap();
        assert_eq!(other.join().unwrap(), b"world");
        server.disconnect();
    }

    #[test]
    fn a_message_over_the_limit_is_refused_unread() {
        let sid = process_user_sid().unwrap();
        let name = test_pipe("large");
        let server = PipeServer::create(&name, &sid).unwrap();
        let path = name.clone();
        let other = thread::spawn(move || {
            let mut pipe = client(&path);
            pipe.write_all(&65u32.to_le_bytes()).unwrap();
        });
        server.accept().unwrap();
        let err = server.read_message(64).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        other.join().unwrap();
        server.disconnect();
    }

    /// The SID goes into the SDDL text, so only digits, `S`, and `-` pass.
    #[test]
    fn a_sid_with_other_text_never_reaches_the_sddl() {
        for sid in ["S-1-5-21-1)(A;;GA;;;WD", "WD", "", "S-1-5-21-1 2"] {
            let err = PipeServer::create(&test_pipe("sid"), sid).err().unwrap();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{sid}");
        }
    }

    /// Read only: the folder exists, is absolute, and holds the hosts
    /// folder.
    #[test]
    fn the_system_folder_is_the_one_with_drivers_etc() {
        let dir = system_dir().unwrap();
        assert!(dir.is_absolute(), "{}", dir.display());
        assert!(
            dir.join("drivers").join("etc").is_dir(),
            "{}",
            dir.display()
        );
    }

    /// Read only: `Program Files` exists and is not the system folder.
    #[test]
    fn program_files_is_an_absolute_folder() {
        let dir = program_files().unwrap();
        assert!(dir.is_absolute() && dir.is_dir(), "{}", dir.display());
        assert_ne!(dir, system_dir().unwrap());
    }
}
