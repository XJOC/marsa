//! File work shared by the install and uninstall steps: the install lock,
//! private folders, renames, flushing to disk, and the I/O error type.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use super::InstallError;
use crate::runtimes::RuntimesDir;

/// Creates `runtimes/.tmp/` (user-only) and takes the install lock in it.
/// The lock is freed when the returned file is dropped.
pub(super) fn lock(runtimes: &RuntimesDir) -> Result<File, InstallError> {
    make_private_dir(runtimes.root(), &runtimes.tmp_dir())?;
    let path = runtimes.lock_path();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(io_error(&path))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(InstallError::Busy),
        Err(TryLockError::Error(err)) => Err(io_error(&path)(err)),
    }
}

/// `tmp` holds files that were checked but not yet unpacked, so only the user
/// may write in it. A link or a file in its place is refused.
fn make_private_dir(root: &Path, tmp: &Path) -> Result<(), InstallError> {
    fs::create_dir_all(root).map_err(io_error(root))?;
    let mut builder = fs::DirBuilder::new();
    private_mode(&mut builder);
    match builder.create(tmp) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(io_error(tmp)(err)),
    }
    let meta = fs::symlink_metadata(tmp).map_err(io_error(tmp))?;
    if !meta.is_dir() {
        return Err(io_error(tmp)(io::Error::other("not a folder")));
    }
    make_private(tmp, &meta).map_err(io_error(tmp))
}

// `#[cfg(unix)]` outside `platform`: one std permission call each, like the
// file modes in `extract`. On Windows the app data dir is already private to
// the user.
#[cfg(unix)]
fn private_mode(builder: &mut fs::DirBuilder) {
    use std::os::unix::fs::DirBuilderExt;
    builder.mode(0o700);
}

#[cfg(not(unix))]
fn private_mode(_builder: &mut fs::DirBuilder) {}

/// A folder made earlier, or by another program, may be open to others.
#[cfg(unix)]
fn make_private(path: &Path, meta: &fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if meta.permissions().mode() & 0o777 == 0o700 {
        return Ok(());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn make_private(_path: &Path, _meta: &fs::Metadata) -> io::Result<()> {
    Ok(())
}

/// Flushes every file under `root` to disk before the rename makes it
/// visible: a power cut must not leave empty programs in a version folder
/// that looks installed.
pub(super) fn sync_tree(root: &Path) -> Result<(), InstallError> {
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).map_err(io_error(&dir))? {
            let entry = entry.map_err(io_error(&dir))?;
            let path = entry.path();
            let kind = entry.file_type().map_err(io_error(&path))?;
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file() {
                sync_path(&path).map_err(io_error(&path))?;
            }
        }
        crate::fs::sync_dir(&dir).map_err(io_error(&dir))?;
    }
    Ok(())
}

fn sync_path(path: &Path) -> io::Result<()> {
    // Windows flushes only through a handle with write access.
    OpenOptions::new()
        .read(true)
        .write(cfg!(windows))
        .open(path)?
        .sync_all()
}

/// Renames `root` to `target`. If `target` exists (`force`), it is first
/// moved to `old` and deleted after; if the second rename fails, it is put
/// back. Blocking: called from a blocking task, so it never stops between
/// the two renames.
pub(super) fn move_into_place(root: &Path, target: &Path, old: &Path) -> Result<(), InstallError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(io_error(parent))?;
    }
    let moved = match fs::symlink_metadata(target) {
        Ok(_) => {
            rename(target, old)?;
            true
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
        Err(err) => return Err(io_error(target)(err)),
    };
    if let Err(err) = rename(root, target) {
        if moved && let Err(back) = rename(old, target) {
            // `clean_tmp` tries again at the next install.
            tracing::error!(
                from = %old.display(),
                to = %target.display(),
                error = %back,
                "cannot put the old version back"
            );
        }
        return Err(err);
    }
    if let Some(parent) = target.parent() {
        crate::fs::sync_dir(parent).map_err(io_error(parent))?;
    }
    if moved {
        // On Windows a running program cannot be deleted: then the old folder
        // stays in `.tmp/` until the next install cleans it.
        remove_tree(old);
    }
    Ok(())
}

/// [`crate::fs::rename_retry`] with the error as an [`InstallError`].
pub(super) fn rename(from: &Path, to: &Path) -> Result<(), InstallError> {
    crate::fs::rename_retry(from, to).map_err(io_error(from))
}

/// 8 hex characters from the OS random source, for temp names.
pub(super) fn random_hex() -> Result<String, InstallError> {
    crate::fs::random_hex(4).map_err(|source| InstallError::Io {
        path: PathBuf::new(),
        source,
    })
}

/// Deletes a folder and all in it. A failure is only logged.
pub(super) fn remove_tree(path: &Path) {
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "cannot delete a temp folder");
        }
    }
}

pub(super) fn io_error(path: &Path) -> impl FnOnce(io::Error) -> InstallError + '_ {
    move |source| InstallError::Io {
        path: path.to_path_buf(),
        source,
    }
}
