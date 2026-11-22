//! The shims of the database clients in `shims/`: `psql`, `mysqldump`,
//! `redis-cli`, and the rest of `marsa_core::databases::clients::CLIENTS`.
//!
//! `marsad` makes them, not the install scripts: a server's clients appear
//! only once it is installed, so they never hide a `psql` the user already
//! has (Homebrew's, say) before the user asks for Marsa's server. They go
//! with the server's last version.
//!
//! Each one is `bin/marsa-shim` under the client's name, as the install
//! scripts make `php`: a hard link on macOS and Linux, a copy (`psql.exe`)
//! on Windows.
//!
//! # Which files are Marsa's
//!
//! A file in `shims/` with a client's name is Marsa's when it is
//! `bin/marsa-shim` (the same file, or the same bytes), or an older build of
//! the shim: a regular file that holds [`SHIM_MARK`], which every build
//! prints. An update of Marsa replaces `bin/marsa-shim` and links only its
//! own five names again, so the clients keep the old build until the next
//! start of `marsad` makes them again. Any other file, or a link (Marsa
//! makes none), is the user's: it is never replaced or deleted, and the log
//! says that it runs instead of Marsa's client.

use marsa_core::data_dir::DataDir;
use std::fs as stdfs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use marsa_core::databases::ServerKind;
use marsa_core::databases::clients::SHIM_MARK;
use marsa_core::fs;
use marsa_core::platform::probe;
use marsa_core::runtimes::RuntimesDir;
use tracing::{debug, info, warn};

use super::state::Daemon;

/// A bigger file is not a shim (a release build is about 1 MB), so it is
/// never read whole.
const MAX_SHIM_BYTES: u64 = 64 * 1024 * 1024;

/// The client shims of one data folder.
#[derive(Debug)]
pub struct ClientShims {
    /// `shims/`, the folder in `PATH`.
    dir: PathBuf,
    /// `bin/marsa-shim`, the program each client shim is.
    shim: PathBuf,
    runtimes: RuntimesDir,
    /// One sync at a time: an install and an uninstall that end close
    /// together each sync, and the last one must see both.
    lock: Mutex<()>,
}

/// What a sync did with the shim of one client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It is `bin/marsa-shim` already.
    Current,
    /// Made, where there was no file.
    Made,
    /// Made again over an older build of the shim.
    Renewed,
    /// Marsa's, and removed: no version of the server is left.
    Removed,
    /// No file, and none is wanted.
    Absent,
    /// The user's file: left as it is.
    Foreign,
    /// A step failed; the log says which.
    Failed,
}

/// What is at the path of a client shim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Found {
    Missing,
    Current,
    /// An older build of the shim.
    Older,
    Foreign,
}

impl ClientShims {
    /// The shims of `data_dir`: `shims/<client>`, from `bin/marsa-shim`.
    pub fn new(data_dir: &Path) -> Self {
        let program = format!("marsa-shim{}", std::env::consts::EXE_SUFFIX);
        Self {
            dir: DataDir::new(data_dir).shims(),
            shim: DataDir::new(data_dir).bin().join(program),
            runtimes: RuntimesDir::in_dir(data_dir),
            lock: Mutex::default(),
        }
    }

    /// [`ClientShims::sync`] for every server. `marsad` runs it at its start.
    pub fn sync_all(&self) -> Vec<(&'static str, Outcome)> {
        ServerKind::ALL
            .into_iter()
            .flat_map(|kind| self.sync(kind))
            .collect()
    }

    /// Makes each client shim of `kind` that is missing or an older build
    /// when a version of `kind` is installed, and removes Marsa's when none
    /// is. The user's files stay. It reads what is installed now, so it is
    /// right whatever ran before it. Blocking; it logs what it does.
    ///
    /// Nothing is made without `bin/marsa-shim`, as in a development build
    /// that runs from `target/`.
    pub fn sync(&self, kind: ServerKind) -> Vec<(&'static str, Outcome)> {
        // The lock guards no data, so a poisoned one is still good.
        let _one = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let wanted = match self.runtimes.installed(kind.runtime()) {
            Ok(installed) => !installed.is_empty(),
            Err(err) => {
                warn!(code = err.code(), error = %err, server = kind.id(), "client shims left as they are");
                return Vec::new();
            }
        };
        let shim = match Shim::read(&self.shim) {
            Ok(shim) => shim,
            Err(err) => {
                warn!(path = %self.shim.display(), error = %err, "cannot read marsa-shim");
                return Vec::new();
            }
        };
        if wanted && shim.is_none() {
            debug!(path = %self.shim.display(), server = kind.id(), "no marsa-shim, so no client shims");
            return Vec::new();
        }
        if wanted && let Err(err) = stdfs::create_dir_all(&self.dir) {
            warn!(path = %self.dir.display(), error = %err, "cannot make the shims folder");
            return Vec::new();
        }
        kind.clients()
            .map(|client| {
                let path = self.path(client.name);
                let outcome = self.sync_one(&path, wanted, shim.as_ref());
                log(&path, client.name, wanted, outcome);
                (client.name, outcome)
            })
            .collect()
    }

    /// `shims/<name>`, with `.exe` on Windows.
    fn path(&self, name: &str) -> PathBuf {
        let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        self.dir.join(file)
    }

    fn sync_one(&self, path: &Path, wanted: bool, shim: Option<&Shim>) -> Outcome {
        let found = match classify(path, shim) {
            Ok(found) => found,
            Err(err) => {
                warn!(path = %path.display(), error = %err, "cannot read a client shim");
                return Outcome::Failed;
            }
        };
        let done = match (wanted, found) {
            (_, Found::Foreign) => return Outcome::Foreign,
            (true, Found::Current) => return Outcome::Current,
            (false, Found::Missing) => return Outcome::Absent,
            (true, Found::Missing) => self.put(path).map(|()| Outcome::Made),
            (true, Found::Older) => self.put(path).map(|()| Outcome::Renewed),
            (false, Found::Current | Found::Older) => retire(path).map(|()| Outcome::Removed),
        };
        done.unwrap_or_else(|err| {
            warn!(path = %path.display(), error = %err, "cannot change a client shim");
            Outcome::Failed
        })
    }

    /// Puts `bin/marsa-shim` at `path` whole: made under a hidden name in
    /// the same folder, then renamed into place, so `path` never holds half
    /// a program.
    fn put(&self, path: &Path) -> io::Result<()> {
        let temp = beside(path, ".", "tmp");
        link_or_copy(&self.shim, &temp)?;
        let placed = replace(&temp, path);
        if placed.is_err() {
            // Best effort: the move already failed, and that is the error.
            let _ = stdfs::remove_file(&temp);
        }
        placed
    }
}

/// [`ClientShims::sync`] of `kind` on a blocking thread, after an install or
/// an uninstall of it. It never fails: what it could not do is in the log,
/// and the next start of `marsad` tries again.
pub async fn sync(daemon: &Arc<Daemon>, kind: ServerKind) {
    let worker = Arc::clone(daemon);
    let job = tokio::task::spawn_blocking(move || worker.client_shims.sync(kind));
    if let Err(err) = job.await {
        warn!(server = kind.id(), error = %err, "the client shims job failed");
    }
}

/// `bin/marsa-shim` as it is now.
struct Shim {
    id: Option<(u64, u64)>,
    bytes: Vec<u8>,
}

impl Shim {
    /// `None` when there is no such regular file.
    fn read(path: &Path) -> io::Result<Option<Self>> {
        let meta = match stdfs::metadata(path) {
            Ok(meta) if meta.is_file() => meta,
            Ok(_) => return Ok(None),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        Ok(Some(Self {
            id: fs::file_id(&meta),
            bytes: stdfs::read(path)?,
        }))
    }
}

/// What is at `path`: nothing, `shim` itself (a hard link to it, or a copy
/// of its bytes), an older build of the shim, or the user's file. A link is
/// never followed: it is the user's.
fn classify(path: &Path, shim: Option<&Shim>) -> io::Result<Found> {
    let meta = match stdfs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Found::Missing),
        Err(err) => return Err(err),
    };
    if !meta.is_file() {
        return Ok(Found::Foreign);
    }
    let id = fs::file_id(&meta);
    if id.is_some() && shim.is_some_and(|shim| shim.id == id) {
        return Ok(Found::Current);
    }
    if meta.len() > MAX_SHIM_BYTES {
        return Ok(Found::Foreign);
    }
    let bytes = stdfs::read(path)?;
    if shim.is_some_and(|shim| shim.bytes == bytes) {
        Ok(Found::Current)
    } else if has_mark(&bytes) {
        Ok(Found::Older)
    } else {
        Ok(Found::Foreign)
    }
}

/// Whether `bytes` hold [`SHIM_MARK`], as every build of the shim does.
fn has_mark(bytes: &[u8]) -> bool {
    let mark = SHIM_MARK.as_bytes();
    bytes.windows(mark.len()).any(|window| window == mark)
}

/// `dst` as `src`: a hard link on macOS and Linux, as `install.sh` makes the
/// shims, and a copy where a link fails (another volume). A copy on
/// Windows, where a shim is never a link (architecture.md section 7).
fn link_or_copy(src: &Path, dst: &Path) -> io::Result<()> {
    // `cfg!` outside `platform`: the one difference between the systems
    // here, std only, as in the install scripts.
    if !cfg!(windows) && stdfs::hard_link(src, dst).is_ok() {
        return Ok(());
    }
    stdfs::copy(src, dst).map(drop)
}

/// Moves `from` to `to`, over an older shim at `to`. Unix replaces it in one
/// step. Windows cannot write over the file of a running program, but can
/// rename it, so the old one is moved aside first (architecture.md section
/// 10).
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    // `cfg!` outside `platform`: one known Windows behavior, std only.
    if cfg!(windows) && stdfs::symlink_metadata(to).is_ok() {
        retire(to)?;
    }
    fs::rename_retry(from, to)
}

/// Removes `path`, a shim that may be running: renamed aside first, as
/// Windows can rename the file of a running program but not delete it. One
/// that still runs there stays as `<name>.<random>.old`, which the next
/// install of Marsa deletes (`install.ps1` deletes `*.old` in `shims\`).
fn retire(path: &Path) -> io::Result<()> {
    let aside = beside(path, "", "old");
    fs::rename_retry(path, &aside)?;
    if let Err(err) = stdfs::remove_file(&aside) {
        debug!(path = %aside.display(), error = %err, "an old client shim stays until the next install");
    }
    Ok(())
}

/// `<prefix><name>.<random>.<extension>` next to `path`: a name no other
/// file has.
fn beside(path: &Path, prefix: &str, extension: &str) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let random = probe::random();
    path.with_file_name(format!("{prefix}{name}.{random:016x}.{extension}"))
}

fn log(path: &Path, name: &str, wanted: bool, outcome: Outcome) {
    let path = path.display();
    match outcome {
        Outcome::Made | Outcome::Renewed => info!(%path, ?outcome, "client shim made"),
        Outcome::Removed => info!(%path, "client shim removed: its server is uninstalled"),
        Outcome::Foreign if wanted => warn!(
            %path,
            "this {name} is not Marsa's, so it stays, and runs instead of Marsa's {name}"
        ),
        Outcome::Current | Outcome::Absent | Outcome::Foreign | Outcome::Failed => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::databases::clients::CLIENTS;
    use semver::Version;
    use tempfile::TempDir;

    /// A data folder with `bin/marsa-shim`, whose bytes are `shim`.
    struct Data {
        dir: TempDir,
        shims: ClientShims,
    }

    impl Data {
        fn new(shim: Option<&str>) -> Self {
            let dir = TempDir::new().unwrap();
            let shims = ClientShims::new(dir.path());
            let data = Self { dir, shims };
            if let Some(text) = shim {
                data.new_shim(text);
            }
            data
        }

        /// Replaces `bin/marsa-shim` as `install.sh` does: a new file
        /// renamed over the old one.
        fn new_shim(&self, text: &str) {
            let bin = DataDir::new(self.dir.path()).bin();
            stdfs::create_dir_all(&bin).unwrap();
            let new = bin.join("marsa-shim.new");
            stdfs::write(&new, shim_bytes(text)).unwrap();
            stdfs::rename(&new, &self.shims.shim).unwrap();
        }

        fn install(&self, kind: ServerKind, version: &str) {
            let version = Version::parse(version).unwrap();
            let dir = self.shims.runtimes.version_dir(kind.runtime(), &version);
            stdfs::create_dir_all(dir.join("bin")).unwrap();
        }

        fn uninstall(&self, kind: ServerKind, version: &str) {
            let version = Version::parse(version).unwrap();
            let dir = self.shims.runtimes.version_dir(kind.runtime(), &version);
            stdfs::remove_dir_all(dir).unwrap();
        }

        fn file(&self, name: &str) -> PathBuf {
            self.shims.path(name)
        }

        fn read(&self, name: &str) -> Option<Vec<u8>> {
            stdfs::read(self.file(name)).ok()
        }

        /// The names in `shims/`, sorted.
        fn listed(&self) -> Vec<String> {
            let Ok(entries) = stdfs::read_dir(&self.shims.dir) else {
                return Vec::new();
            };
            let mut names: Vec<String> = entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    /// The bytes of a fake build of the shim: like a real one, it holds
    /// the mark.
    fn shim_bytes(text: &str) -> Vec<u8> {
        format!("{text}\n{SHIM_MARK}php, node\n").into_bytes()
    }

    fn file_name(name: &str) -> String {
        format!("{name}{}", std::env::consts::EXE_SUFFIX)
    }

    fn outcomes(names: &[&'static str], outcome: Outcome) -> Vec<(&'static str, Outcome)> {
        names.iter().map(|name| (*name, outcome)).collect()
    }

    const POSTGRES: [&str; 3] = ["psql", "pg_dump", "pg_restore"];

    #[test]
    fn an_install_makes_only_the_clients_of_its_server() {
        let data = Data::new(Some("shim v1"));
        // Nothing is installed: nothing is made, not even `shims/`.
        assert_eq!(
            data.shims.sync(ServerKind::Postgres),
            outcomes(&POSTGRES, Outcome::Absent)
        );
        assert!(data.listed().is_empty());

        data.install(ServerKind::Postgres, "18.7.0");
        let made = data.shims.sync(ServerKind::Postgres);
        assert_eq!(made, outcomes(&POSTGRES, Outcome::Made));
        let mut want = POSTGRES.map(file_name).to_vec();
        want.sort();
        assert_eq!(data.listed(), want);
        for name in POSTGRES {
            assert_eq!(data.read(name), Some(shim_bytes("shim v1")), "{name}");
        }
        // The other servers have no version: none of their clients.
        let mysql = data.shims.sync(ServerKind::Mysql);
        assert!(
            mysql.iter().all(|(_, o)| *o == Outcome::Absent),
            "{mysql:?}"
        );
        assert_eq!(data.listed().len(), 3);
        // A second sync finds them in place.
        let again = data.shims.sync(ServerKind::Postgres);
        assert_eq!(again, outcomes(&POSTGRES, Outcome::Current));
    }

    /// As `install.sh` makes `php`: one file under many names.
    #[cfg(unix)]
    #[test]
    fn a_client_shim_is_a_hard_link_to_marsa_shim() {
        let data = Data::new(Some("shim v1"));
        data.install(ServerKind::Valkey, "8.1.11");
        data.shims.sync(ServerKind::Valkey);
        let id = |path: &Path| fs::file_id(&stdfs::metadata(path).unwrap());
        for name in ["valkey-cli", "redis-cli"] {
            assert_eq!(id(&data.file(name)), id(&data.shims.shim), "{name}");
        }
    }

    #[test]
    fn the_last_uninstall_removes_them_and_nothing_else() {
        let data = Data::new(Some("shim v1"));
        data.install(ServerKind::Mysql, "8.4.12");
        data.install(ServerKind::Mysql, "8.4.13");
        data.install(ServerKind::Valkey, "8.1.11");
        data.shims.sync_all();
        let mysql = ["mysql", "mysqldump", "mysqladmin"];

        data.uninstall(ServerKind::Mysql, "8.4.12");
        let kept = data.shims.sync(ServerKind::Mysql);
        assert_eq!(kept, outcomes(&mysql, Outcome::Current));

        data.uninstall(ServerKind::Mysql, "8.4.13");
        let removed = data.shims.sync(ServerKind::Mysql);
        assert_eq!(removed, outcomes(&mysql, Outcome::Removed));
        // Valkey's stay, and nothing is left aside.
        let left = ["redis-cli", "valkey-cli"].map(file_name).to_vec();
        assert_eq!(data.listed(), left);
    }

    /// A `psql` the user put in `shims/` (Homebrew's, say) stays as it is,
    /// whether PostgreSQL is installed or not.
    #[test]
    fn a_file_that_is_not_marsas_is_never_touched() {
        let data = Data::new(Some("shim v1"));
        stdfs::create_dir_all(&data.shims.dir).unwrap();
        let homebrew = b"#!/bin/sh\nexec /opt/homebrew/bin/psql \"$@\"\n".to_vec();
        stdfs::write(data.file("psql"), &homebrew).unwrap();

        data.install(ServerKind::Postgres, "18.7.0");
        let made = data.shims.sync(ServerKind::Postgres);
        let want = [
            ("psql", Outcome::Foreign),
            ("pg_dump", Outcome::Made),
            ("pg_restore", Outcome::Made),
        ];
        assert_eq!(made, want);
        assert_eq!(data.read("psql"), Some(homebrew.clone()));

        // A newer Marsa does not replace it either.
        data.new_shim("shim v2");
        let renewed = data.shims.sync_all();
        assert!(renewed.contains(&("psql", Outcome::Foreign)), "{renewed:?}");
        assert!(
            renewed.contains(&("pg_dump", Outcome::Renewed)),
            "{renewed:?}"
        );

        data.uninstall(ServerKind::Postgres, "18.7.0");
        let removed = data.shims.sync(ServerKind::Postgres);
        let want = [
            ("psql", Outcome::Foreign),
            ("pg_dump", Outcome::Removed),
            ("pg_restore", Outcome::Removed),
        ];
        assert_eq!(removed, want);
        assert_eq!(data.read("psql"), Some(homebrew));
        assert_eq!(data.listed(), [file_name("psql")]);
    }

    /// A link is never Marsa's, even to `bin/marsa-shim`: Marsa makes none.
    #[cfg(unix)]
    #[test]
    fn a_link_with_a_client_name_is_the_users() {
        let data = Data::new(Some("shim v1"));
        stdfs::create_dir_all(&data.shims.dir).unwrap();
        let link = data.file("redis-cli");
        std::os::unix::fs::symlink(&data.shims.shim, &link).unwrap();
        data.install(ServerKind::Valkey, "8.1.11");
        let synced = data.shims.sync(ServerKind::Valkey);
        assert_eq!(
            synced,
            [
                ("valkey-cli", Outcome::Made),
                ("redis-cli", Outcome::Foreign)
            ]
        );
        data.uninstall(ServerKind::Valkey, "8.1.11");
        data.shims.sync(ServerKind::Valkey);
        assert!(stdfs::symlink_metadata(&link).unwrap().is_symlink());
    }

    /// After an update `bin/marsa-shim` is a new file, and the clients
    /// still hold the old one: the start of `marsad` makes them again.
    #[test]
    fn the_start_renews_an_older_build_of_the_shim() {
        let data = Data::new(Some("shim v1"));
        data.install(ServerKind::Valkey, "8.1.11");
        data.install(ServerKind::Postgres, "18.7.0");
        data.shims.sync_all();

        data.new_shim("shim v2");
        let started = data.shims.sync_all();
        let mut want = outcomes(&POSTGRES, Outcome::Renewed);
        want.extend(outcomes(
            &["mysql", "mysqldump", "mysqladmin"],
            Outcome::Absent,
        ));
        want.extend(outcomes(&["valkey-cli", "redis-cli"], Outcome::Renewed));
        assert_eq!(started, want);
        for name in POSTGRES.into_iter().chain(["valkey-cli", "redis-cli"]) {
            assert_eq!(data.read(name), Some(shim_bytes("shim v2")), "{name}");
            let found = classify(
                &data.file(name),
                Shim::read(&data.shims.shim).unwrap().as_ref(),
            );
            assert_eq!(found.unwrap(), Found::Current, "{name}");
        }
        // Nothing is left aside, and no temp file stays.
        assert_eq!(data.listed().len(), 5, "{:?}", data.listed());
    }

    /// A copy with the same bytes is the current shim too: Windows makes
    /// copies, and so does Unix when a link fails.
    #[test]
    fn a_copy_of_marsa_shim_is_current() {
        let data = Data::new(Some("shim v1"));
        stdfs::create_dir_all(&data.shims.dir).unwrap();
        stdfs::copy(&data.shims.shim, data.file("valkey-cli")).unwrap();
        data.install(ServerKind::Valkey, "8.1.11");
        let synced = data.shims.sync(ServerKind::Valkey);
        assert_eq!(
            synced,
            [
                ("valkey-cli", Outcome::Current),
                ("redis-cli", Outcome::Made)
            ]
        );
    }

    /// A development build runs from `target/`, with no `bin/marsa-shim`:
    /// nothing is made then, and an older shim is still removed with its
    /// server.
    #[test]
    fn without_marsa_shim_nothing_is_made() {
        let data = Data::new(None);
        data.install(ServerKind::Postgres, "18.7.0");
        assert!(data.shims.sync(ServerKind::Postgres).is_empty());
        assert!(data.listed().is_empty());

        stdfs::create_dir_all(&data.shims.dir).unwrap();
        stdfs::write(data.file("psql"), shim_bytes("shim v0")).unwrap();
        data.uninstall(ServerKind::Postgres, "18.7.0");
        let removed = data.shims.sync(ServerKind::Postgres);
        let want = [
            ("psql", Outcome::Removed),
            ("pg_dump", Outcome::Absent),
            ("pg_restore", Outcome::Absent),
        ];
        assert_eq!(removed, want);
        assert!(data.listed().is_empty());
    }

    #[test]
    fn the_mark_is_found_anywhere_in_a_file() {
        assert!(has_mark(&shim_bytes("x")));
        assert!(has_mark(SHIM_MARK.as_bytes()));
        assert!(!has_mark(&SHIM_MARK.as_bytes()[1..]));
        assert!(!has_mark(b""));
        assert!(!has_mark(b"#!/bin/sh\nexec psql\n"));
    }

    #[test]
    fn every_client_has_a_path_in_shims() {
        let data = Data::new(None);
        for client in CLIENTS {
            let path = data.file(client.name);
            assert_eq!(path.parent(), Some(data.shims.dir.as_path()));
            let file = path.file_name().unwrap().to_string_lossy();
            assert_eq!(file, file_name(client.name));
        }
    }
}
