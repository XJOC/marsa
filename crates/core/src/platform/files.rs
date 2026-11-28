//! Opening a file inside a folder that someone else may change, such as a
//! project's `storage/logs`, and telling who the open file is.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Who an open file is, from its handle: the same for every name of the
/// file, and how many names it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    /// The device and inode on Unix; the volume serial number and the file
    /// number on Windows.
    pub id: (u64, u64),
    /// Its names (hard links). More than one: the file is also reachable
    /// from another folder.
    pub links: u64,
}

/// [`FileIdentity`] of `file`: `fstat` on Unix, `GetFileInformationByHandle`
/// on Windows. The two kinds of id are never compared with each other, so
/// one look and the next agree only on the same system.
///
/// # Errors
///
/// The system cannot say. A caller that checks containment refuses the
/// file then: it cannot prove the file has one name.
pub fn identity(file: &File) -> io::Result<FileIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let meta = file.metadata()?;
        Ok(FileIdentity {
            id: (meta.dev(), meta.ino()),
            links: meta.nlink(),
        })
    }
    #[cfg(windows)]
    {
        let info = super::windows::file_info(file)?;
        Ok(FileIdentity {
            id: (info.volume, info.index),
            links: info.links,
        })
    }
}

/// Opens `path` to read, without following a link at its last part where the
/// system allows it.
///
/// - Windows: `FILE_FLAG_OPEN_REPARSE_POINT` opens a link or junction itself,
///   so the caller sees `is_symlink()` on the open file and refuses it.
/// - macOS and Linux: a plain open, which follows a link. The caller looks at
///   the path before (`symlink_metadata`) and compares the open file's device
///   and inode with it after (`crate::fs::file_id`), so a link swapped in
///   between is seen. `O_NOFOLLOW` would need the `libc` crate.
///
/// Rust opens files with full sharing on Windows, so a reader never stops
/// Laravel from writing or rotating its log (architecture.md section 9).
///
/// # Errors
///
/// The error of the open.
pub fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    #[test]
    fn a_regular_file_opens_and_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("laravel.log");
        std::fs::write(&path, "line\n").unwrap();
        let mut text = String::new();
        open_no_follow(&path)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "line\n");
    }

    /// Every name of a file gives the same id, and counts the names.
    #[test]
    fn two_names_of_one_file_have_one_id_and_two_links() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.log");
        let second = dir.path().join("second.log");
        let other = dir.path().join("other.log");
        std::fs::write(&first, "a").unwrap();
        std::fs::write(&other, "b").unwrap();
        let alone = identity(&open_no_follow(&first).unwrap()).unwrap();
        assert_eq!(alone.links, 1);
        std::fs::hard_link(&first, &second).unwrap();
        let one = identity(&open_no_follow(&first).unwrap()).unwrap();
        let two = identity(&open_no_follow(&second).unwrap()).unwrap();
        assert_eq!(one, two);
        assert_eq!(one.id, alone.id);
        assert_eq!(one.links, 2);
        let apart = identity(&open_no_follow(&other).unwrap()).unwrap();
        assert_ne!(apart.id, one.id);
    }

    /// Windows only: the open file is the link, not its target.
    #[cfg(windows)]
    #[test]
    fn a_link_opens_as_the_link_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        std::fs::write(&target, "secret").unwrap();
        let link = dir.path().join("laravel.log");
        // Needs Developer Mode or an administrator; CI runners have it. Skip
        // where links cannot be made.
        if std::os::windows::fs::symlink_file(&target, &link).is_err() {
            return;
        }
        let opened = open_no_follow(&link).unwrap().metadata().unwrap();
        assert!(opened.file_type().is_symlink());
    }
}
