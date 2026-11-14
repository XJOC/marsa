//! The helper's Win32 calls (ADR 0018). The `unsafe` code of the helper on
//! Windows is in this file only, and each block calls one or two functions.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::ptr::null_mut;

use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

/// `MAX_PATH`: the system folder is `C:\Windows\System32` or close to it,
/// far below this.
const MAX_PATH: usize = 260;

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
    // (the current user), and `path` is a valid out pointer. On success the
    // function returns a NUL-terminated string that the caller frees with
    // `CoTaskMemFree`; on failure it sets `path` to null, and freeing null is
    // allowed. The string is read up to its NUL and copied before the free.
    unsafe {
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
        folder.ok_or_else(|| io::Error::from_raw_os_error(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
