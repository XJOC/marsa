//! Where the user's NSS databases are, from the home folder only, and the
//! checks that keep a link from taking the certificate to another place.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

/// The most Firefox profiles that get the certificate.
pub const MAX_PROFILES: usize = 32;
/// Chrome's database, under the home folder.
pub(super) const CHROME: [&str; 2] = [".pki", "nssdb"];
/// The snap of Chromium, under the home folder, and the link to the folder
/// of its installed revision.
const SNAP_CHROMIUM: [&str; 2] = ["snap", "chromium"];
const SNAP_CURRENT: &str = "current";
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
    /// The folder is not there yet: Chrome's or Chromium's, made before
    /// `-A`.
    pub(super) new: bool,
}

/// The databases under `home`, and why each refused one cannot be used.
pub(super) fn databases(home: &Path) -> Found {
    let mut found = Vec::new();
    let mut refused = Vec::new();
    let mut complete = true;
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
    match snap_chromium(home) {
        Some(Ok(database)) => found.push(database),
        Some(Err(why)) => refused.push(why),
        None => {}
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
                complete = false;
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
    Found {
        databases: found,
        refused,
        complete,
    }
}

/// What [`databases`] found.
pub(super) struct Found {
    pub(super) databases: Vec<Database>,
    /// Why each database left alone cannot be used.
    pub(super) refused: Vec<String>,
    /// `false` when some profiles were left out: more than
    /// [`MAX_PROFILES`].
    pub(super) complete: bool,
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

/// The database of the Chromium snap, if the snap is there:
/// `~/snap/chromium/<revision>/.pki/nssdb`, where `<revision>` is what the
/// link `current` names. A link to anything but a folder right next to it
/// is refused.
pub(super) fn snap_chromium(home: &Path) -> Option<Result<Database, String>> {
    match real_folders(home, &SNAP_CHROMIUM) {
        Folders::Real => {}
        Folders::Missing => return None,
        Folders::Refused(why) => return Some(Err(why)),
    }
    let snap = under(home, &SNAP_CHROMIUM);
    let link = snap.join(SNAP_CURRENT);
    let target = match fs::read_link(&link) {
        Ok(target) => target,
        // No revision installed: no `current`. One that is not a link
        // (a real folder) is refused below, like any other surprise.
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            return Some(Err(format!(
                "{} cannot be read: {}",
                link.display(),
                err.kind()
            )));
        }
    };
    let revision = match target.components().collect::<Vec<_>>().as_slice() {
        [Component::Normal(name)] => name.to_str().map(str::to_owned),
        _ => None,
    };
    let Some(revision) = revision else {
        let why = format!(
            "{} names something other than a folder next to it",
            link.display()
        );
        return Some(Err(why));
    };
    let parts = [revision.as_str(), CHROME[0], CHROME[1]];
    let dir = under(&snap, &parts);
    match real_folders(&snap, &parts) {
        Folders::Real => Some(db_files(&dir).map(|_| Database { dir, new: false })),
        // The revision's own folder must be there; `.pki/nssdb` is made.
        Folders::Missing if matches!(real_folders(&snap, &parts[..1]), Folders::Real) => {
            Some(Ok(Database { dir, new: true }))
        }
        Folders::Missing => None,
        Folders::Refused(why) => Some(Err(why)),
    }
}

/// Makes a database folder, and the folders above it if needed, for this
/// user only.
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

// The only test makes symbolic links, which need Unix here.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct Home {
        dir: TempDir,
    }

    impl Home {
        fn home(&self) -> PathBuf {
            self.dir.path().join("home")
        }
    }

    /// The Chromium snap: `current` names the revision folder next to it.
    #[test]
    fn the_chromium_snap_follows_current_only_to_its_revision() {
        use std::os::unix::fs::symlink;

        let home = Home {
            dir: TempDir::new().unwrap(),
        };
        fs::create_dir_all(home.home()).unwrap();
        assert_eq!(snap_chromium(&home.home()), None);
        let snap = under(&home.home(), &SNAP_CHROMIUM);
        fs::create_dir_all(snap.join("2890")).unwrap();
        // No `current` yet: no revision installed.
        assert_eq!(snap_chromium(&home.home()), None);
        symlink("2890", snap.join(SNAP_CURRENT)).unwrap();
        let nssdb = snap.join("2890").join(".pki").join("nssdb");
        assert_eq!(
            snap_chromium(&home.home()),
            Some(Ok(Database {
                dir: nssdb.clone(),
                new: true
            }))
        );
        fs::create_dir_all(&nssdb).unwrap();
        fs::write(nssdb.join(CERT_DB), "").unwrap();
        assert_eq!(
            snap_chromium(&home.home()),
            Some(Ok(Database {
                dir: nssdb,
                new: false
            }))
        );
        for target in ["../2890", "/tmp", "2890/.pki"] {
            fs::remove_file(snap.join(SNAP_CURRENT)).unwrap();
            symlink(target, snap.join(SNAP_CURRENT)).unwrap();
            let refused = snap_chromium(&home.home()).unwrap().unwrap_err();
            assert!(
                refused.contains("names something other than a folder"),
                "{target}: {refused}"
            );
        }
        // A revision folder that is itself a link.
        fs::remove_file(snap.join(SNAP_CURRENT)).unwrap();
        symlink(home.dir.path(), snap.join("2891")).unwrap();
        symlink("2891", snap.join(SNAP_CURRENT)).unwrap();
        let refused = snap_chromium(&home.home()).unwrap().unwrap_err();
        assert!(refused.contains("2891 is not a folder"), "{refused}");
    }
}
