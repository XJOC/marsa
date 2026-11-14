//! The helper's Win32 calls (ADR 0018). The `unsafe` code of the helper on
//! Windows is in this file only, and each block calls one function.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

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
}
