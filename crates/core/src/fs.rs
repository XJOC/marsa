//! File helpers shared by every crate: the atomic write, `create_new`, the
//! folder sync, and the rename that tries again on Windows.
//!
//! Plain `std`, so the shim can use it too (no `full` feature needed).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Writes `bytes` to `path` whole: a temp file in the same folder, flushed to
/// disk, then renamed over `path`. A reader sees the old file or the new one,
/// never half of one, and a power cut cannot leave an empty file after the
/// rename.
///
/// The parent folder is made if missing. The temp file is `.<name>.tmp` next
/// to `path`, not in the system temp folder, because `rename` fails across
/// volumes. One that a crash left behind is replaced. Only one writer at a
/// time per `path`: callers hold a lock where that matters.
///
/// `mode` is the Unix mode of the new file, set from the first byte and again
/// after the write (the umask may take bits off). `None` keeps the default
/// mode. Ignored on Windows.
///
/// # Errors
///
/// Any I/O error of the folder, the temp file, or the rename. The temp file
/// is deleted then.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = temp_path(path);
    // The handle is closed before the rename: Windows cannot rename an open file.
    let result = write_new(&temp, bytes, mode).and_then(|()| rename_retry(&temp, path));
    if result.is_err() {
        // Best effort: the write already failed, and that is the error to report.
        let _ = fs::remove_file(&temp);
    }
    result
}

/// `.<name>.tmp` in the folder of `path`. Hidden, and with neither the
/// extension of the target nor a suffix a program would pick up (`.plist`,
/// `.desktop`, `.json`).
fn temp_path(path: &Path) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(".tmp");
    path.with_file_name(name)
}

fn write_new(temp: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let mut file = match create_new(temp, mode) {
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            // Left by a write that stopped half way.
            fs::remove_file(temp)?;
            create_new(temp, mode)?
        }
        other => other?,
    };
    file.write_all(bytes)?;
    // `#[cfg(unix)]` outside `platform`: mode bits exist only on Unix, std only.
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    file.sync_all()
}

/// Creates a new file for writing. It refuses a file or a symlink that is
/// already there. On Unix `mode` applies from the first byte, with no `chmod`
/// after the write; `None` and Windows use the default.
pub fn create_new(path: &Path, mode: Option<u32>) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    // `#[cfg(unix)]` outside `platform`: mode bits exist only on Unix, std only.
    #[cfg(unix)]
    if let Some(mode) = mode {
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    options.open(path)
}

/// The device and inode of a file, so two looks can tell whether they saw
/// the same file, or two names are hard links to one file.
// `#[cfg(unix)]` outside `platform`: std gives a stable file id on Unix only.
#[cfg(unix)]
pub fn file_id(meta: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    Some((meta.dev(), meta.ino()))
}

/// Windows: std gives no stable file id from a path's metadata, so the
/// caller decides otherwise. An open file's id is
/// `platform::files::identity`.
#[cfg(not(unix))]
pub fn file_id(_meta: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// Flushes the entries of `dir` (a rename into it) to disk.
// `#[cfg(unix)]` outside `platform`: a folder's entries are flushed by
// opening the folder, which only Unix allows. NTFS logs them itself.
#[cfg(unix)]
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Windows: nothing to do, see the Unix version.
#[cfg(not(unix))]
pub fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// `fs::rename`, tried again on Windows while another program holds a file:
/// antivirus programs hold files that were just written or unpacked for a
/// short time. rustup does the same. Blocking (it sleeps between tries).
pub fn rename_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut delay = Duration::from_millis(100);
    let mut tries = 0;
    loop {
        match fs::rename(from, to) {
            Err(err) if is_held_on_windows(&err) && tries < 5 => {
                std::thread::sleep(delay);
                delay *= 2;
                tries += 1;
            }
            other => return other,
        }
    }
}

/// `bytes` random bytes from the OS random source, as lowercase hex (two
/// characters each): for the names of temp files and folders.
///
/// # Errors
///
/// The OS random source failed.
// `getrandom` is behind `full`; the shim needs no random names.
#[cfg(feature = "full")]
pub fn random_hex(bytes: usize) -> io::Result<String> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer)
        .map_err(|err| io::Error::other(format!("no random bytes: {err}")))?;
    Ok(crate::hex::encode(&buffer))
}

/// `dst` as `src`: a hard link on macOS and Linux, as `install.sh` makes the
/// shims, and a copy where a link fails (another volume). A copy on
/// Windows, where a program file is never a link (architecture.md section 7).
pub fn link_or_copy(src: &Path, dst: &Path) -> io::Result<()> {
    // `cfg!` outside `platform`: the one difference between the systems
    // here, std only, as in the install scripts.
    if !cfg!(windows) && fs::hard_link(src, dst).is_ok() {
        return Ok(());
    }
    fs::copy(src, dst).map(drop)
}

/// Whether the file at `path` has to be renamed aside before another file
/// takes its name. Windows cannot write over the file of a running program,
/// but can rename it (architecture.md section 10); Unix replaces it in one
/// step.
pub fn needs_rename_aside(path: &Path) -> bool {
    // `cfg!` outside `platform`: one known Windows behavior, std only.
    cfg!(windows) && fs::symlink_metadata(path).is_ok()
}

/// `ERROR_ACCESS_DENIED` (5), `ERROR_SHARING_VIOLATION` (32), or
/// `ERROR_LOCK_VIOLATION` (33).
fn is_held_on_windows(err: &io::Error) -> bool {
    // `cfg!` outside `platform`: one known Windows behavior, std only.
    cfg!(windows)
        && (err.kind() == io::ErrorKind::PermissionDenied
            || matches!(err.raw_os_error(), Some(5 | 32 | 33)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[cfg(feature = "full")]
    #[test]
    fn random_names_are_lowercase_hex_and_differ() {
        let (one, two) = (random_hex(8).unwrap(), random_hex(8).unwrap());
        assert_eq!(one.len(), 16);
        assert!(
            one.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_ne!(one, two);
    }

    #[test]
    fn writes_a_new_file_and_makes_its_folder() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a/b/file.txt");
        write_atomic(&path, b"hello", None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");
        assert!(!temp_path(&path).exists());
    }

    #[test]
    fn replaces_an_existing_file_whole() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(&path, "a much longer old body").unwrap();
        write_atomic(&path, b"new", None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn replaces_a_temp_file_left_by_a_crash() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(temp_path(&path), "left by a crash").unwrap();
        write_atomic(&path, b"new", None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert!(!temp_path(&path).exists());
    }

    #[test]
    fn the_temp_file_is_hidden_and_next_to_the_target() {
        let temp = temp_path(Path::new("/x/y/Marsa.plist"));
        assert_eq!(temp, Path::new("/x/y/.Marsa.plist.tmp"));
    }

    #[cfg(unix)]
    #[test]
    fn sets_the_mode_whatever_the_umask() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        for mode in [0o600, 0o644] {
            let path = dir.path().join(format!("file-{mode:o}"));
            write_atomic(&path, b"x", Some(mode)).unwrap();
            let got = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(got, mode);
        }
    }

    #[test]
    fn a_failed_write_leaves_no_temp_file_and_keeps_the_target() {
        let dir = TempDir::new().unwrap();
        // The target is a folder: the rename fails after the temp file is written.
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();
        write_atomic(&target, b"x", None).unwrap_err();
        assert!(!temp_path(&target).exists());
        assert!(target.is_dir());
    }

    /// Only an existing file on Windows is moved aside; a missing one never
    /// is.
    #[test]
    fn only_windows_renames_an_existing_file_aside() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("shim");
        assert!(!needs_rename_aside(&file));
        fs::write(&file, "x").unwrap();
        assert_eq!(needs_rename_aside(&file), cfg!(windows));
        let copy = dir.path().join("copy");
        link_or_copy(&file, &copy).unwrap();
        assert_eq!(fs::read(&copy).unwrap(), b"x");
    }

    #[cfg(unix)]
    #[test]
    fn a_hard_link_has_the_id_of_its_file_and_a_copy_has_its_own() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("file");
        fs::write(&file, "x").unwrap();
        fs::hard_link(&file, dir.path().join("link")).unwrap();
        fs::copy(&file, dir.path().join("copy")).unwrap();
        let id = |name: &str| file_id(&fs::metadata(dir.path().join(name)).unwrap());
        assert!(id("file").is_some());
        assert_eq!(id("link"), id("file"));
        assert_ne!(id("copy"), id("file"));
    }

    #[cfg(not(unix))]
    #[test]
    fn windows_has_no_file_id() {
        let dir = TempDir::new().unwrap();
        assert_eq!(file_id(&fs::metadata(dir.path()).unwrap()), None);
    }

    #[test]
    fn only_windows_retries_a_held_file() {
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        let sharing = io::Error::from_raw_os_error(32);
        let missing = io::Error::from(io::ErrorKind::NotFound);
        assert_eq!(is_held_on_windows(&denied), cfg!(windows));
        assert!(!is_held_on_windows(&missing));
        if cfg!(windows) {
            assert!(is_held_on_windows(&sharing));
        }
    }
}
