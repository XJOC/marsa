//! Where the user's NSS databases are, from the home folder only, and the
//! checks that keep a link from taking the certificate to another place.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The most Firefox profiles that get the certificate.
pub const MAX_PROFILES: usize = 32;
/// Chrome's database, under the home folder.
pub(super) const CHROME: [&str; 2] = [".pki", "nssdb"];
/// The folders that hold Firefox profiles, under the home folder.
const FIREFOX: [&[&str]; 3] = [
    &[".mozilla", "firefox"],
    // Firefox 147 and later, for a new profile with no `~/.mozilla`.
    &[".config", "mozilla", "firefox"],
    &["snap", "firefox", "common", ".mozilla", "firefox"],
];
/// The files of a database. None may be a link: `certutil` opens them all.
const DB_FILES: [&str; 3] = [CERT_DB, "key4.db", "pkcs11.txt"];
/// The file of a database in the SQL format, the only one used here.
pub(super) const CERT_DB: &str = "cert9.db";

/// One NSS database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Database {
    pub(super) dir: PathBuf,
    /// The folder is not there yet: Chrome's, made before `-A`.
    pub(super) new: bool,
}

/// The databases under `home`, and why each refused one cannot be used.
pub(super) fn databases(home: &Path) -> (Vec<Database>, Vec<String>) {
    let mut found = Vec::new();
    let mut refused = Vec::new();
    let chrome = under(home, &CHROME);
    match real_folders(home, &CHROME) {
        Folders::Real => match db_files(&chrome) {
            Ok(_) => found.push(Database {
                dir: chrome,
                new: false,
            }),
            Err(why) => refused.push(why),
        },
        Folders::Missing => found.push(Database {
            dir: chrome,
            new: true,
        }),
        Folders::Refused(why) => refused.push(why),
    }
    let mut profiles = 0;
    for parts in FIREFOX {
        match real_folders(home, parts) {
            Folders::Real => {}
            Folders::Missing => continue,
            Folders::Refused(why) => {
                refused.push(why);
                continue;
            }
        }
        for dir in profile_dirs(&under(home, parts)) {
            if profiles == MAX_PROFILES {
                tracing::warn!(dir = %dir.display(), "more than {MAX_PROFILES} Firefox profiles; this one is left out");
                continue;
            }
            match profile(&dir) {
                Ok(true) => {
                    profiles += 1;
                    found.push(Database { dir, new: false });
                }
                // A folder that is not a profile, such as `Crash Reports`.
                Ok(false) => {}
                Err(why) => refused.push(why),
            }
        }
    }
    (found, refused)
}

/// The real folders in `dir`, sorted. A link is not looked into.
fn profile_dirs(dir: &Path) -> Vec<PathBuf> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(dir = %dir.display(), error = %err, "cannot list the Firefox profiles");
            return Vec::new();
        }
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            // Does not follow a link.
            entry.file_type().is_ok_and(|kind| kind.is_dir())
        })
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    dirs
}

/// `home` joined with `parts`.
pub(super) fn under(home: &Path, parts: &[&str]) -> PathBuf {
    parts
        .iter()
        .fold(home.to_path_buf(), |path, part| path.join(part))
}

/// What [`real_folders`] found.
#[derive(Debug, PartialEq, Eq)]
enum Folders {
    /// Each one is a real folder of this user's.
    Real,
    /// One is not there; each one before it is real.
    Missing,
    /// One is a link, a file, or another user's.
    Refused(String),
}

/// Checks each folder from `home` down through `parts`. `home` itself is
/// not checked: the system says where it is.
fn real_folders(home: &Path, parts: &[&str]) -> Folders {
    let mut path = home.to_path_buf();
    for part in parts {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Folders::Missing,
            Err(err) => {
                return Folders::Refused(format!(
                    "{} cannot be checked: {}",
                    path.display(),
                    err.kind()
                ));
            }
            Ok(meta) if !meta.file_type().is_dir() => {
                return Folders::Refused(format!(
                    "{} is not a folder (a link, or a file)",
                    path.display()
                ));
            }
            Ok(meta) if !owned(&meta) => {
                return Folders::Refused(format!("{} belongs to another user", path.display()));
            }
            Ok(_) => {}
        }
    }
    Folders::Real
}

/// Whether `dir` holds a database: `true` when `cert9.db` is there. Each
/// of [`DB_FILES`] that is there must be a real file of this user's.
///
/// # Errors
///
/// Why one of them cannot be used.
fn db_files(dir: &Path) -> Result<bool, String> {
    let mut database = false;
    for name in DB_FILES {
        let there = file_kind(&dir.join(name))?;
        database |= there && name == CERT_DB;
    }
    Ok(database)
}

/// Whether `dir` is a Firefox profile: it holds a database. A folder with
/// none is skipped before its owner counts: a root-owned `Crash Reports`
/// after `sudo firefox` is no profile.
///
/// # Errors
///
/// A profile, or a file in it, that cannot be used.
fn profile(dir: &Path) -> Result<bool, String> {
    if !db_files(dir)? {
        return Ok(false);
    }
    owned_dir(dir)?;
    Ok(true)
}

/// A profile folder of this user's; one of another user's is refused.
fn owned_dir(dir: &Path) -> Result<(), String> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if owned(&meta) => Ok(()),
        Ok(_) => Err(format!("{} belongs to another user", dir.display())),
        Err(err) => Err(format!(
            "{} cannot be checked: {}",
            dir.display(),
            err.kind()
        )),
    }
}

/// `true` for a real file of this user's, `false` if there is none.
///
/// # Errors
///
/// A link, a folder, another user's file, or one that cannot be checked.
fn file_kind(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!(
            "{} cannot be checked: {}",
            path.display(),
            err.kind()
        )),
        Ok(meta) if !meta.file_type().is_file() => Err(format!(
            "{} is not a file (a link, or a folder)",
            path.display()
        )),
        Ok(meta) if !owned(&meta) => Err(format!("{} belongs to another user", path.display())),
        Ok(_) => Ok(true),
    }
}

/// Whether the file belongs to the user `marsad` runs as.
// `#[cfg]` outside `platform`: Unix owners, as in `user_trust`. The step
// runs on Linux only; elsewhere this compiles for the tests.
#[cfg(unix)]
fn owned(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    super::super::current_uid().is_ok_and(|me| meta.uid() == me)
}

// No owners to compare on Windows, where the step never runs.
#[cfg(not(unix))]
fn owned(_meta: &fs::Metadata) -> bool {
    true
}

/// Makes Chrome's folder, and `~/.pki` if needed, for this user only.
pub(super) fn make_dir(dir: &Path) -> Result<(), String> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    // `#[cfg]` outside `platform`: the Unix mode of the new folders.
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(dir)
        .map_err(|err| format!("cannot make {}: {}", dir.display(), err.kind()))
}
