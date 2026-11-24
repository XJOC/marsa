//! Opening a file inside a folder that someone else may change, such as a
//! project's `storage/logs`.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

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
