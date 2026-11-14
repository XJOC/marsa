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
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    ERROR_NO_DATA, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    ConvertStringSidToSidW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, LookupAccountSidW, PSECURITY_DESCRIPTOR, PSID, RevertToSelf,
    SECURITY_ATTRIBUTES, SID_NAME_USE, SidTypeUser, TOKEN_QUERY, TOKEN_USER, TokenUser,
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
/// The user's rights on the pipe: `FILE_GENERIC_READ`, and
/// `FILE_GENERIC_WRITE` without `FILE_APPEND_DATA`, which on a pipe is
/// `FILE_CREATE_PIPE_INSTANCE`. The user reads and writes, and cannot add
/// an instance of their own. A client opens it with `FILE_GENERIC_READ |
/// FILE_WRITE_DATA` (`GENERIC_WRITE` asks for the right left out).
const USER_RIGHTS: &str = "0x12019b";
/// The pipe's integrity label: Medium, with no read up and no write up. A
/// process of the user at Low integrity (a sandbox) cannot open the pipe
/// at all, so it cannot hold the only instance either.
const LABEL: &str = "S:(ML;;NWNR;;;ME)";

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
    /// The DACL: full access for LocalSystem, [`USER_RIGHTS`] for
    /// `user_sid`, nothing for anyone else. `P` keeps inherited entries out.
    /// The label is [`LABEL`].
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a SID with anything but digits, `S`, and `-` in it
    /// (it goes into the SDDL text). Otherwise the Win32 error.
    pub fn create(name: &str, user_sid: &str) -> io::Result<Self> {
        Self::create_with(
            name,
            user_sid,
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
        )
    }

    /// [`PipeServer::create`] with the open mode `mode`. The tests make a
    /// second instance without `FILE_FLAG_FIRST_PIPE_INSTANCE` with it.
    fn create_with(name: &str, user_sid: &str, mode: u32) -> io::Result<Self> {
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
        let sddl = wide(&security_descriptor(user_sid));
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
                mode,
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

    /// Waits for the next client. A client that connected before the call
    /// is fine.
    ///
    /// # Errors
    ///
    /// The Win32 error. `ERROR_NO_DATA` (232): a client opened the pipe and
    /// closed it before the call. Until a disconnect, every later call would
    /// fail the same way and no client could open the pipe; this disconnects
    /// it, so the next call waits for the next client.
    pub fn accept(&self) -> io::Result<()> {
        // SAFETY: the handle is the open pipe this struct owns; no
        // OVERLAPPED, so the call blocks until a client connects.
        if unsafe { ConnectNamedPipe(self.raw(), null_mut()) } != 0 {
            return Ok(());
        }
        // SAFETY: no arguments; it reads this thread's last error.
        let code = unsafe { GetLastError() };
        match code {
            ERROR_PIPE_CONNECTED => Ok(()),
            ERROR_NO_DATA => {
                self.disconnect();
                Err(io::Error::from_raw_os_error(code as i32))
            }
            _ => Err(io::Error::from_raw_os_error(code as i32)),
        }
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
    /// until the client has read it. A client that never reads holds the
    /// service as long as it stays connected (task M4-19f, open question).
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

/// The user SID of the token the current thread acts with, as text. A
/// thread that does not impersonate anyone has no token: `ERROR_NO_TOKEN`.
fn thread_user_sid() -> io::Result<String> {
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

/// Whether `sid` names a user account, as Windows looks it up: `Some(false)`
/// for a group, an alias, or a well-known SID. `None` if Windows cannot
/// tell, for example a domain account with no domain controller to ask.
///
/// # Errors
///
/// The Win32 error of a SID that Windows cannot read.
pub fn is_user_account(sid: &str) -> io::Result<Option<bool>> {
    let text = wide(sid);
    let mut binary: PSID = null_mut();
    // SAFETY: `text` is NUL-terminated and `binary` is a valid out pointer.
    // Windows allocates the SID, which is freed with `LocalFree` below.
    if unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut binary) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut name = [0u16; 256];
    let mut domain = [0u16; 256];
    let mut name_len = name.len() as u32;
    let mut domain_len = domain.len() as u32;
    let mut kind: SID_NAME_USE = 0;
    // SAFETY: each buffer goes with its own length, `kind` is a valid out
    // pointer, and `binary` is the SID made above, freed right after.
    let found = unsafe {
        let found = LookupAccountSidW(
            null(),
            binary,
            name.as_mut_ptr(),
            &mut name_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        );
        LocalFree(binary);
        found
    };
    // A name longer than the buffers fails too: then Windows did not tell.
    Ok((found != 0).then_some(kind == SidTypeUser))
}

/// The pipe's security descriptor in SDDL: the DACL, then the label.
fn security_descriptor(user_sid: &str) -> String {
    format!("D:P(A;;GA;;;SY)(A;;{USER_RIGHTS};;;{user_sid}){LABEL}")
}

/// `text` as NUL-terminated UTF-16.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::thread;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, LABEL_SECURITY_INFORMATION};
    use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_READ, FILE_WRITE_DATA};

    use super::*;

    /// From the `CreateFile` page: the server may read who the client is,
    /// and act as it for nothing more.
    const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;

    /// A pipe name of this test run only.
    fn test_pipe(test: &str) -> String {
        format!(r"\\.\pipe\marsa-helper-test-{}-{test}", std::process::id())
    }

    /// A client as `marsad` opens the pipe: read, and write without
    /// `FILE_APPEND_DATA` (see [`USER_RIGHTS`]).
    fn client(name: &str) -> io::Result<File> {
        OpenOptions::new()
            .access_mode(FILE_GENERIC_READ | FILE_WRITE_DATA)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(name)
    }

    fn send(pipe: &mut File, bytes: &[u8]) {
        let len = u32::try_from(bytes.len()).unwrap();
        pipe.write_all(&len.to_le_bytes()).unwrap();
        pipe.write_all(bytes).unwrap();
    }

    /// Ends the test process if a pipe call blocks for good, so a bug fails
    /// the test instead of hanging CI. Dropping it ends the watch.
    struct Watchdog {
        _stop: mpsc::Sender<()>,
    }

    fn watchdog() -> Watchdog {
        let (sender, receiver) = mpsc::channel::<()>();
        thread::spawn(move || {
            if receiver.recv_timeout(Duration::from_secs(60)) == Err(RecvTimeoutError::Timeout) {
                eprintln!("a pipe call blocked for 60 s");
                std::process::abort();
            }
        });
        Watchdog { _stop: sender }
    }

    /// A real pipe in this test: no other process, and nothing outside it
    /// changes. The client connects before `accept`, which is allowed, so
    /// no step can wait for a thread that failed.
    #[test]
    fn the_pipe_carries_one_message_each_way_and_names_the_client() {
        let _watch = watchdog();
        let sid = process_user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
        let name = test_pipe("one");
        let server = PipeServer::create(&name, &sid).unwrap();
        // The name is taken: a second server is refused.
        assert!(PipeServer::create(&name, &sid).is_err());

        let mut pipe = client(&name).unwrap();
        // One instance: while it has a client, the next one is busy.
        let busy = client(&name).unwrap_err();
        assert_eq!(busy.raw_os_error(), Some(ERROR_PIPE_BUSY as i32));
        send(&mut pipe, b"hello");
        server.accept().unwrap();
        assert_eq!(server.read_message(64).unwrap(), b"hello");
        assert_eq!(server.client_sid().unwrap(), sid);
        // `write_message` waits until the client has read the answer.
        let reader = thread::spawn(move || {
            let mut len = [0u8; 4];
            pipe.read_exact(&mut len).unwrap();
            let mut reply = vec![0; u32::from_le_bytes(len) as usize];
            pipe.read_exact(&mut reply).unwrap();
            reply
        });
        server.write_message(b"world").unwrap();
        assert_eq!(reader.join().unwrap(), b"world");
        server.disconnect();
    }

    #[test]
    fn a_message_over_the_limit_is_refused_unread() {
        let _watch = watchdog();
        let sid = process_user_sid().unwrap();
        let name = test_pipe("large");
        let server = PipeServer::create(&name, &sid).unwrap();
        // Open until the server has read: the length alone, no body.
        let mut pipe = client(&name).unwrap();
        pipe.write_all(&65u32.to_le_bytes()).unwrap();
        server.accept().unwrap();
        let err = server.read_message(64).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        drop(pipe);
        server.disconnect();
    }

    /// A client that opened the pipe and closed it before `accept`: the
    /// pipe is readied, and the next client is served.
    #[test]
    fn a_client_that_came_and_went_does_not_block_the_next() {
        let _watch = watchdog();
        let sid = process_user_sid().unwrap();
        let name = test_pipe("gone");
        let server = PipeServer::create(&name, &sid).unwrap();
        drop(client(&name).unwrap());
        let err = server.accept().unwrap_err();
        assert_eq!(err.raw_os_error(), Some(ERROR_NO_DATA as i32));

        let path = name.clone();
        let next = thread::spawn(move || {
            // Busy until the server waits again, below.
            let mut pipe = loop {
                match client(&path) {
                    Ok(pipe) => break pipe,
                    Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(err) => panic!("{err}"),
                }
            };
            send(&mut pipe, b"next");
            pipe
        });
        server.accept().unwrap();
        assert_eq!(server.read_message(64).unwrap(), b"next");
        drop(next.join().unwrap());
        server.disconnect();
    }

    /// The user cannot add an instance of their own: the pipe allows one,
    /// and the user's rights leave out `FILE_CREATE_PIPE_INSTANCE`.
    #[test]
    fn no_second_instance_can_be_made() {
        let sid = process_user_sid().unwrap();
        let name = test_pipe("second");
        let _server = PipeServer::create(&name, &sid).unwrap();
        let err = PipeServer::create_with(&name, &sid, PIPE_ACCESS_DUPLEX)
            .err()
            .unwrap();
        let refused = [ERROR_PIPE_BUSY as i32, ERROR_ACCESS_DENIED as i32];
        assert!(refused.contains(&err.raw_os_error().unwrap_or(0)), "{err}");
    }

    /// Windows keeps the DACL and the label as written.
    #[test]
    fn the_pipe_has_its_dacl_and_its_label() {
        let sid = process_user_sid().unwrap();
        let name = test_pipe("dacl");
        let server = PipeServer::create(&name, &sid).unwrap();
        let what = DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION;
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        let mut text: *mut u16 = null_mut();
        // SAFETY: the handle is the open pipe; the out pointers are valid.
        // Windows allocates the descriptor and the text, which are read
        // and then freed with `LocalFree`.
        let sddl = unsafe {
            let read = GetSecurityInfo(
                server.raw(),
                SE_KERNEL_OBJECT,
                what,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut descriptor,
            );
            assert_eq!(read, 0);
            let converted = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                what,
                &mut text,
                null_mut(),
            );
            assert_ne!(converted, 0);
            let mut len = 0;
            while *text.add(len) != 0 {
                len += 1;
            }
            let sddl = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            LocalFree(text.cast());
            LocalFree(descriptor);
            sddl
        };
        // Windows writes it back in its own words: on a CI runner
        // `D:P(A;;FA;;;SY)(A;;0x12019b;;;LA)S:AI(ML;;NWNR;;;ME)`. A SID with
        // a short name (`LA`, the built-in Administrator) gets that name, and
        // the label part gets the `AI` flag. The user is checked by name in
        // `the_pipe_carries_one_message_each_way_and_names_the_client`.
        let (dacl, label) = sddl.split_once("S:").expect("a label part");
        let dacl = dacl.strip_prefix("D:P").expect("a protected DACL first");
        let aces: Vec<&str> = dacl
            .trim_end_matches(')')
            .trim_start_matches('(')
            .split(")(")
            .collect();
        assert_eq!(aces.len(), 2, "{sddl}");
        assert!(aces.contains(&"A;;FA;;;SY"), "{sddl}");
        let user = aces.iter().find(|ace| **ace != "A;;FA;;;SY").unwrap();
        assert!(user.starts_with(&format!("A;;{USER_RIGHTS};;;")), "{sddl}");
        assert!(label.ends_with("(ML;;NWNR;;;ME)"), "{sddl}");
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

    /// Read only: the test's own user is a user; a built-in group, an
    /// alias, and LocalSystem are not.
    #[test]
    fn only_a_user_account_is_a_user() {
        let me = process_user_sid().unwrap();
        assert_eq!(is_user_account(&me).unwrap(), Some(true), "{me}");
        for other in ["S-1-1-0", "S-1-5-32-544", "S-1-5-18"] {
            assert_eq!(is_user_account(other).unwrap(), Some(false), "{other}");
        }
        assert!(is_user_account("not a SID").is_err());
    }

    /// Read only: `Program Files` exists and is not the system folder.
    #[test]
    fn program_files_is_an_absolute_folder() {
        let dir = program_files().unwrap();
        assert!(dir.is_absolute() && dir.is_dir(), "{}", dir.display());
        assert_ne!(dir, system_dir().unwrap());
    }
}
