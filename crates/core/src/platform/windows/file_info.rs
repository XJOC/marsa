//! Who a file is on Windows: its volume and file number, and how many names
//! it has. std keeps these behind an unstable feature (`windows_by_handle`).

use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::windows::io::AsRawHandle as _;

use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
};

/// What `GetFileInformationByHandle` says about an open file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileInfo {
    /// The serial number of the volume.
    pub(crate) volume: u64,
    /// The file's number on that volume: the same for every name of it.
    /// On ReFS, whose numbers are 128 bits, two files may share it: it
    /// tells a reader where to go on, never whether a file may be read.
    pub(crate) index: u64,
    /// The names of the file (hard links).
    pub(crate) links: u64,
}

/// [`FileInfo`] of `file`, from its handle, so a name that changes after the
/// open does not matter.
///
/// # Errors
///
/// The Win32 error: for example a file system that does not answer.
pub(crate) fn file_info(file: &File) -> io::Result<FileInfo> {
    let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: the handle is `file`'s, open for the whole call; `info` is a
    // valid out pointer to a struct of the type the call writes.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so it wrote the whole struct.
    let info = unsafe { info.assume_init() };
    Ok(FileInfo {
        volume: u64::from(info.dwVolumeSerialNumber),
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        links: u64::from(info.nNumberOfLinks),
    })
}
