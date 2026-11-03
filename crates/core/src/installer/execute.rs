//! Runs an install: takes the install lock, downloads and extracts into a
//! temp folder under `runtimes/.tmp/`, runs the recipe's check, then moves the
//! version folder into place atomically.
//!
//! Called by [`super::install`]; the order of the steps is documented there.
//!
//! Cancel safety: `install()` may be dropped at any `.await` (a timeout, or
//! the daemon shutting down). Every disk step that must not stop halfway (the
//! extraction, the two renames, the deletes) runs in `spawn_blocking`, and the
//! blocking task owns the lock and the work folder. So the lock is freed only
//! after the last write, even when the future is gone.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use semver::Version;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use super::extract::{self, Layout, Limits};
use super::fetch::{self, Expected, Fetch};
use super::manifest::{MAX_MANIFEST_SIZE, Manifest, ManifestError};
use super::plan::{self, ArtifactStep};
use super::{InstallEnv, InstallError, InstallRequest, InstalledVersion};
use crate::recipes::{Check, PlatformRecipe};
use crate::runtimes::{Runtime, RuntimesDir};
use crate::types::InstallEvent;

/// A `.minisig` file is about 300 bytes.
const MAX_SIGNATURE_SIZE: u64 = 4096;
/// At most one `Downloading` event per this many bytes.
const PROGRESS_STEP: u64 = 256 * 1024;
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
/// The most of each output stream of the check that is read.
const CHECK_OUTPUT_MAX: u64 = 64 * 1024;
/// The most characters of the check's output put in an error.
const CHECK_OUTPUT_SHOWN: usize = 500;
/// `STATUS_DLL_NOT_FOUND`, the exit code of a Windows program that misses a
/// DLL. For PHP and Node that is the Visual C++ runtime. Never a Unix code.
const STATUS_DLL_NOT_FOUND: i32 = 0xC000_0135_u32.cast_signed();
/// Variables that would change how the check program starts or which
/// libraries it loads. The rest of the environment stays: Windows programs
/// need `SystemRoot`.
const CHECK_ENV_REMOVED: [&str; 7] = [
    "PHPRC",
    "PHP_INI_SCAN_DIR",
    "NODE_OPTIONS",
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "DYLD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
];
/// The three runtimes, to read the names of moved-aside folders.
const RUNTIMES: [Runtime; 3] = [Runtime::Php, Runtime::Node, Runtime::Composer];

pub(super) async fn run<F: Fetch>(
    req: &InstallRequest<'_>,
    env: &InstallEnv<F>,
    events: &mut (dyn FnMut(InstallEvent) + Send),
) -> Result<InstalledVersion, InstallError> {
    let runtime = req.runtime;
    let unsupported = |platform: String| InstallError::UnsupportedPlatform {
        what: runtime.dir_name().to_owned(),
        platform,
    };
    let platform = env
        .platform
        .ok_or_else(|| unsupported("this system".to_owned()))?;
    let recipe = env.recipe(runtime)?;
    let programs = recipe
        .platform(platform)
        .ok_or_else(|| unsupported(platform.to_string()))?;
    let runtimes = RuntimesDir::in_dir(&env.app_data_dir);

    // Shared with every blocking task, so it is freed only after the last
    // one ends. The OS frees it if the process dies.
    let lock = Arc::new(lock(&runtimes)?);
    blocking(&runtimes.tmp_dir(), {
        let (runtimes, lock) = (runtimes.clone(), Arc::clone(&lock));
        move || {
            clean_tmp(&runtimes);
            drop(lock);
            Ok(())
        }
    })
    .await?;

    let manifest = verified_manifest(env, &runtimes, &lock).await?;
    events(InstallEvent::ManifestVerified {
        serial: manifest.serial(),
    });
    let release = plan::select(&manifest, runtime, req.version)?;
    let version = release.version().clone();
    events(InstallEvent::Selected {
        runtime,
        version: version.to_string(),
    });
    let target = runtimes.version_dir(runtime, &version);
    if !req.force && runtimes.installed(runtime)?.contains(&version) {
        events(InstallEvent::Installed {
            dir: target.clone(),
        });
        return Ok(InstalledVersion {
            runtime,
            version,
            dir: target,
            already: true,
        });
    }

    let plan = plan::build(runtime, release, programs, platform)?;
    // Deleted with everything in it when the last owner drops it.
    let work = Arc::new(WorkDir::create(&runtimes.tmp_dir(), Arc::clone(&lock))?);
    drop(lock);
    let built = fill(
        env,
        &plan.steps,
        recipe.check(),
        programs,
        &work,
        &version,
        events,
    )
    .await;

    // One blocking task for the end, success or not: sync, both renames, and
    // the delete of the work folder. Dropping this future cannot stop it
    // between the two renames.
    let old = runtimes.tmp_dir().join(format!(
        "old-{}-{version}-{}",
        runtime.dir_name(),
        random_hex()?
    ));
    let finish_target = target.clone();
    blocking(&target, move || {
        let result = built.and_then(|()| {
            sync_tree(&work.root())?;
            move_into_place(&work.root(), &finish_target, &old)
        });
        drop(work);
        result
    })
    .await?;

    events(InstallEvent::Installed {
        dir: target.clone(),
    });
    Ok(InstalledVersion {
        runtime,
        version,
        dir: target,
        already: false,
    })
}

/// Downloads and unpacks every file into the work folder, then runs the check.
async fn fill<F: Fetch>(
    env: &InstallEnv<F>,
    steps: &[ArtifactStep<'_>],
    check: Option<&Check>,
    programs: &PlatformRecipe,
    work: &Arc<WorkDir>,
    version: &Version,
    events: &mut (dyn FnMut(InstallEvent) + Send),
) -> Result<(), InstallError> {
    for step in steps {
        download_and_extract(&env.fetch, step, work, events).await?;
    }
    if let Some(check) = check {
        events(InstallEvent::Checking);
        run_check(check, programs, &work.root(), version, CHECK_TIMEOUT).await?;
    }
    Ok(())
}

/// Runs `job` on tokio's blocking threads. If the caller's future is dropped,
/// the job still runs to the end. `path` names the error if the task panics.
async fn blocking<T: Send + 'static>(
    path: &Path,
    job: impl FnOnce() -> Result<T, InstallError> + Send + 'static,
) -> Result<T, InstallError> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|err| InstallError::Io {
            path: path.to_path_buf(),
            source: io::Error::other(format!("a disk task stopped: {err}")),
        })?
}

/// Creates `runtimes/.tmp/` (user-only) and takes the install lock in it.
/// The lock is freed when the returned file is dropped.
fn lock(runtimes: &RuntimesDir) -> Result<File, InstallError> {
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

/// Empties `runtimes/.tmp/` but for the lock: what an install that was cut
/// off left behind. Runs under the lock, so no install is using it.
///
/// A moved-aside version (`old-<runtime>-<version>-<hex>`) whose folder is
/// missing was cut off between the two renames of a `force` install: it is
/// put back, not deleted. A failure is only logged: on Windows a running
/// program cannot be deleted, and the next install tries again.
fn clean_tmp(runtimes: &RuntimesDir) {
    let tmp = runtimes.tmp_dir();
    let lock = runtimes.lock_path();
    let entries = match fs::read_dir(&tmp) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(path = %tmp.display(), error = %err, "cannot read the temp folder");
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == lock {
            continue;
        }
        if let Some(target) = moved_aside_target(runtimes, &entry.file_name())
            && matches!(fs::symlink_metadata(&target), Err(e) if e.kind() == io::ErrorKind::NotFound)
        {
            let restored = target
                .parent()
                .map_or(Ok(()), fs::create_dir_all)
                .map_err(io_error(&target))
                .and_then(|()| rename(&path, &target));
            match restored {
                Ok(()) => {
                    tracing::warn!(path = %target.display(), "put back a version moved aside")
                }
                Err(err) => {
                    tracing::warn!(path = %path.display(), error = %err, "cannot put back a version");
                }
            }
            continue;
        }
        // `file_type` does not follow links: a link is removed, not its target.
        let result = match entry.file_type() {
            Ok(kind) if kind.is_dir() => fs::remove_dir_all(&path),
            _ => fs::remove_file(&path),
        };
        if let Err(err) = result {
            tracing::warn!(path = %path.display(), error = %err, "cannot delete an old temp file");
        }
    }
}

/// The version folder that `old-<runtime>-<version>-<8 hex>` came from, or
/// `None` for any other name.
fn moved_aside_target(runtimes: &RuntimesDir, name: &std::ffi::OsStr) -> Option<PathBuf> {
    let rest = name.to_str()?.strip_prefix("old-")?;
    let (runtime, rest) = rest.split_once('-')?;
    let (version, hex) = rest.rsplit_once('-')?;
    let runtime = RUNTIMES.into_iter().find(|r| r.dir_name() == runtime)?;
    let version = Version::parse(version).ok()?;
    let is_hex = hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_hexdigit());
    is_hex.then(|| runtimes.version_dir(runtime, &version))
}

/// Downloads the manifest and its signature, checks them against the last
/// serial accepted, and saves the new serial. The serial is saved before any
/// download, so a failed install still raises the floor.
async fn verified_manifest<F: Fetch>(
    env: &InstallEnv<F>,
    runtimes: &RuntimesDir,
    lock: &Arc<File>,
) -> Result<Manifest, InstallError> {
    let json = fetch::fetch_bytes(&env.fetch, &env.manifest_url, MAX_MANIFEST_SIZE as u64).await?;
    let sig_url = format!("{}.minisig", env.manifest_url);
    let sig = fetch::fetch_bytes(&env.fetch, &sig_url, MAX_SIGNATURE_SIZE).await?;
    let sig = String::from_utf8(sig).map_err(|_| ManifestError::BadSignature)?;

    let serial_path = runtimes.manifest_serial_path();
    let last_seen = read_serial(&serial_path)?;
    let manifest = env.trust.verify(&json, &sig, last_seen)?;
    let serial = manifest.serial();
    if last_seen != Some(serial) {
        let tmp = runtimes.tmp_dir();
        let lock = Arc::clone(lock);
        blocking(&serial_path.clone(), move || {
            let result = write_serial(&serial_path, &tmp, serial);
            drop(lock);
            result
        })
        .await?;
    }
    Ok(manifest)
}

/// The saved serial, or `None` if there is none yet. A file that is not a
/// number is `install_serial_corrupt`, never `None`: that would turn the
/// rollback check off.
fn read_serial(path: &Path) -> Result<Option<u64>, InstallError> {
    match fs::read_to_string(path) {
        Ok(text) => text
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| InstallError::SerialCorrupt {
                path: path.to_path_buf(),
            }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(io_error(path)(err)),
    }
}

/// Writes `serial` to a new file in `tmp`, then renames it over `path`.
fn write_serial(path: &Path, tmp: &Path, serial: u64) -> Result<(), InstallError> {
    let temp = tmp.join(format!("serial-{}", random_hex()?));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .and_then(|mut file| {
            file.write_all(format!("{serial}\n").as_bytes())?;
            file.sync_all()
        })
        .map_err(io_error(path))
        .and_then(|()| rename(&temp, path));
    if written.is_err() {
        remove_quietly(&temp);
    }
    written
}

/// `runtimes/.tmp/i-<8 hex>/`, new for each install, with `dl/` for the
/// downloads and `root/` for the version folder. Deleted on drop, and only
/// then is its share of the install lock freed.
struct WorkDir {
    path: PathBuf,
    _lock: Arc<File>,
}

impl WorkDir {
    fn create(tmp: &Path, lock: Arc<File>) -> Result<WorkDir, InstallError> {
        let mut tries = 0;
        let work = loop {
            // Short on purpose: Windows tools still trip on paths over 260
            // characters (architecture.md section 10).
            let path = tmp.join(format!("i-{}", random_hex()?));
            match fs::create_dir(&path) {
                Ok(()) => {
                    break WorkDir {
                        path,
                        _lock: Arc::clone(&lock),
                    };
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists && tries < 5 => tries += 1,
                Err(err) => return Err(io_error(&path)(err)),
            }
        };
        for dir in [work.dl(), work.root()] {
            fs::create_dir(&dir).map_err(io_error(&dir))?;
        }
        Ok(work)
    }

    fn dl(&self) -> PathBuf {
        self.path.join("dl")
    }

    fn root(&self) -> PathBuf {
        self.path.join("root")
    }
}

impl Drop for WorkDir {
    // Normally the last owner is a blocking task. Only when `install()` is
    // dropped during a download does this run on an async thread.
    fn drop(&mut self) {
        remove_tree(&self.path);
    }
}

/// Downloads one file into `dl/`, checks it, and unpacks it into `root/`.
async fn download_and_extract<F: Fetch>(
    fetch: &F,
    step: &ArtifactStep<'_>,
    work: &Arc<WorkDir>,
    events: &mut (dyn FnMut(InstallEvent) + Send),
) -> Result<(), InstallError> {
    let name = step.source.name();
    let dest = work.dl().join(name);
    let expected = Expected::from_manifest(step.source);
    let mut last_sent: Option<u64> = None;
    let mut progress = |done: u64, total: u64| {
        // A new mirror starts again from 0, so a smaller value is sent too.
        let due = last_sent.is_none_or(|sent| done < sent || done - sent >= PROGRESS_STEP)
            || done == total;
        if due {
            last_sent = Some(done);
            events(InstallEvent::Downloading {
                artifact: name.to_owned(),
                done,
                total,
            });
        }
    };
    let file = fetch::download_verified(fetch, step.source.urls(), &expected, &dest, &mut progress)
        .await?;
    events(InstallEvent::Verified {
        artifact: name.to_owned(),
    });

    events(InstallEvent::Extracting {
        artifact: name.to_owned(),
    });
    let artifact = step.artifact.clone();
    let work = Arc::clone(work);
    let report = blocking(&dest, move || {
        let report = extract::extract(
            file.path(),
            &Layout::from(&artifact),
            &work.root(),
            &Limits::default(),
        )?;
        let path = file.path().to_path_buf();
        if let Err(err) = file.remove() {
            tracing::warn!(path = %path.display(), error = %err, "cannot delete a download");
        }
        drop(work);
        Ok(report)
    })
    .await?;
    tracing::debug!(
        artifact = name,
        files = report.files,
        bytes = report.bytes,
        "extracted"
    );
    Ok(())
}

/// Runs the recipe's check program from `root` by its full path: no `PATH`
/// search, no shell. It must exit with success, and its output must start
/// with the expected text.
async fn run_check(
    check: &Check,
    programs: &PlatformRecipe,
    root: &Path,
    version: &Version,
    timeout: Duration,
) -> Result<(), InstallError> {
    let failed = |output: String, exit_code| InstallError::CheckFailed { output, exit_code };
    // The recipe parser checks that the program is in `programs`.
    let program = programs
        .program_path(root, &check.program)
        .ok_or_else(|| failed(format!("no program {:?}", check.program), None))?;
    // The app data dir is absolute, so `Command` never searches `PATH`.
    debug_assert!(program.is_absolute(), "{}", program.display());
    let mut command = Command::new(&program);
    command.args(&check.args);
    clean_check_env(&mut command);

    let (status, stdout, stderr) = match run_capped(command, timeout).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            return Err(failed(
                format!("cannot run {}: {err}", program.display()),
                None,
            ));
        }
        Err(_) => {
            return Err(failed(
                format!("no answer after {} ms", timeout.as_millis()),
                None,
            ));
        }
    };

    if status.code() == Some(STATUS_DLL_NOT_FOUND) {
        return Err(InstallError::MissingVcRuntime);
    }
    let stdout = String::from_utf8_lossy(&stdout);
    if status.success() && stdout.starts_with(&check.expected_prefix(version)) {
        return Ok(());
    }
    let text = format!("{stdout}{}", String::from_utf8_lossy(&stderr));
    Err(failed(
        text.chars().take(CHECK_OUTPUT_SHOWN).collect(),
        status.code(),
    ))
}

fn clean_check_env(command: &mut Command) {
    for name in CHECK_ENV_REMOVED {
        command.env_remove(name);
    }
}

type CheckOutput = (std::process::ExitStatus, Vec<u8>, Vec<u8>);

/// Runs `command` with no input, reads at most [`CHECK_OUTPUT_MAX`] bytes of
/// each output, and waits at most `timeout`. On a timeout the future is
/// dropped, and `kill_on_drop` stops the child.
async fn run_capped(
    mut command: Command,
    timeout: Duration,
) -> Result<io::Result<CheckOutput>, tokio::time::error::Elapsed> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = async {
        let mut child = command.spawn()?;
        let (stdout, stderr) = tokio::join!(
            read_capped(child.stdout.take()),
            read_capped(child.stderr.take())
        );
        let status = child.wait().await?;
        Ok((status, stdout?, stderr?))
    };
    tokio::time::timeout(timeout, output).await
}

/// Reads at most [`CHECK_OUTPUT_MAX`] bytes, then closes the pipe: a program
/// that keeps writing then gets a broken pipe and stops.
async fn read_capped(pipe: Option<impl AsyncRead + Unpin>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if let Some(pipe) = pipe {
        pipe.take(CHECK_OUTPUT_MAX).read_to_end(&mut bytes).await?;
    }
    Ok(bytes)
}

/// Flushes every file under `root` to disk before the rename makes it
/// visible: a power cut must not leave empty programs in a version folder
/// that looks installed.
fn sync_tree(root: &Path) -> Result<(), InstallError> {
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
        sync_dir(&dir).map_err(io_error(&dir))?;
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

// `#[cfg(unix)]` outside `platform`: a folder's entries are flushed by
// opening the folder, which only Unix allows. NTFS logs them itself.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Renames `root` to `target`. If `target` exists (`force`), it is first
/// moved to `old` and deleted after; if the second rename fails, it is put
/// back. Blocking: called from a blocking task, so it never stops between
/// the two renames.
fn move_into_place(root: &Path, target: &Path, old: &Path) -> Result<(), InstallError> {
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
        sync_dir(parent).map_err(io_error(parent))?;
    }
    if moved {
        // On Windows a running program cannot be deleted: then the old folder
        // stays in `.tmp/` until the next install cleans it.
        remove_tree(old);
    }
    Ok(())
}

/// `fs::rename`, tried again on Windows while another program holds a file:
/// antivirus programs hold files that were just unpacked for a short time.
/// rustup does the same. Blocking (it sleeps between tries).
fn rename(from: &Path, to: &Path) -> Result<(), InstallError> {
    let mut delay = Duration::from_millis(100);
    let mut tries = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(err) if is_held_on_windows(&err) && tries < 5 => {
                std::thread::sleep(delay);
                delay *= 2;
                tries += 1;
            }
            Err(err) => return Err(io_error(from)(err)),
        }
    }
}

/// `ERROR_ACCESS_DENIED` (5), `ERROR_SHARING_VIOLATION` (32), or
/// `ERROR_LOCK_VIOLATION` (33).
fn is_held_on_windows(err: &io::Error) -> bool {
    // `cfg!` outside `platform`: one known Windows behavior, std only.
    cfg!(windows)
        && (err.kind() == io::ErrorKind::PermissionDenied
            || matches!(err.raw_os_error(), Some(5 | 32 | 33)))
}

/// 8 hex characters from the OS random source, for temp names.
fn random_hex() -> Result<String, InstallError> {
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes).map_err(|err| InstallError::Io {
        path: PathBuf::new(),
        source: io::Error::other(format!("no random bytes: {err}")),
    })?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Deletes a folder and all in it. A failure is only logged.
fn remove_tree(path: &Path) {
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "cannot delete a temp folder");
        }
    }
}

fn remove_quietly(path: &Path) {
    if let Err(err) = fs::remove_file(path)
        && err.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %err, "cannot delete a temp file");
    }
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> InstallError + '_ {
    move |source| InstallError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::installer::Trust;
    use crate::installer::fetch::testing::{MemoryFetch, Reply};
    use crate::installer::install;
    use crate::recipes::{self, Platform};
    use crate::runtimes::Runtime;
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    const MANIFEST: &str = "https://test.example/manifest.json";
    const MANIFEST_SIG: &str = "https://test.example/manifest.json.minisig";

    /// A PHP recipe for `linux-x64`: two archives unpacked into `bin/`.
    const FAKE_PHP: &str = r#"
name = "php"

[platform.linux-x64]
programs = { php = "bin/php", php-fpm = "bin/php-fpm" }

[[platform.linux-x64.artifact]]
name = "cli"
url = "https://example.test/cli"
format = "tar.gz"
into = "bin"

[[platform.linux-x64.artifact]]
name = "fpm"
url = "https://example.test/fpm"
format = "tar.gz"
into = "bin"
"#;

    /// [`FAKE_PHP`] with a check, like the real recipe.
    fn fake_php_with_check() -> String {
        FAKE_PHP.replacen(
            "name = \"php\"\n",
            "name = \"php\"\n[check]\nprogram = \"php\"\nargs = [\"-v\"]\n\
             expect_prefix = \"PHP {version} \"\n",
            1,
        )
    }

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!("../../tests/fixtures/manifest/", $name))
        };
    }

    /// A `.tar.gz` with one file.
    fn tar_gz(path: &str, data: &[u8], mode: u32) -> Vec<u8> {
        let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(gzip);
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, path, data)
            .expect("append to tar");
        let gzip = builder.into_inner().expect("finish tar");
        gzip.finish().expect("finish gzip")
    }

    fn php_cli(prints: &str) -> Vec<u8> {
        let script = format!("#!/bin/sh\necho '{prints}'\n");
        tar_gz("php", script.as_bytes(), 0o755)
    }

    fn php_fpm() -> Vec<u8> {
        tar_gz("php-fpm", b"not a real php-fpm", 0o755)
    }

    fn url(runtime: &str, version: &str, name: &str) -> String {
        format!("https://test.example/{runtime}/{version}/{name}")
    }

    /// A manifest and the files it lists, served by a `MemoryFetch`. Every
    /// release has files for one platform key.
    struct Served {
        runtime: &'static str,
        platform: &'static str,
        releases: Vec<String>,
        files: Vec<(String, Vec<u8>)>,
    }

    impl Served {
        fn new(runtime: &'static str, platform: &'static str) -> Self {
            Served {
                runtime,
                platform,
                releases: Vec::new(),
                files: Vec::new(),
            }
        }

        fn release(mut self, version: &str, files: &[(&str, Vec<u8>)]) -> Self {
            let mut sources = Vec::new();
            for (name, bytes) in files {
                let url = url(self.runtime, version, name);
                let sha: String = Sha256::digest(bytes)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                sources.push(format!(
                    r#"{{"name": "{name}", "urls": ["{url}"], "sha256": "{sha}", "size": {}}}"#,
                    bytes.len()
                ));
                self.files.push((url, bytes.clone()));
            }
            self.releases.push(format!(
                r#"{{"version": "{version}", "platforms": {{"{}": [{}]}}}}"#,
                self.platform,
                sources.join(", ")
            ));
            self
        }

        /// Serves the manifest (with `serial`), a dummy signature, and the
        /// files.
        fn fetch(&self, serial: u64) -> MemoryFetch {
            let json = format!(
                r#"{{"schema": 1, "serial": {serial}, "runtimes": {{"{}": [{}]}}}}"#,
                self.runtime,
                self.releases.join(", ")
            );
            let mut fetch = MemoryFetch::new()
                .with(MANIFEST, Reply::Chunks(vec![json.into_bytes()]))
                .with(MANIFEST_SIG, Reply::Chunks(vec![b"unsigned".to_vec()]));
            for (url, bytes) in &self.files {
                fetch = fetch.with(url, Reply::Chunks(vec![bytes.clone()]));
            }
            fetch
        }
    }

    fn env(
        data: &TempDir,
        fetch: MemoryFetch,
        recipe: Option<&str>,
        trust: Trust,
    ) -> InstallEnv<MemoryFetch> {
        InstallEnv {
            app_data_dir: data.path().to_path_buf(),
            fetch,
            manifest_url: MANIFEST.to_owned(),
            platform: Some(Platform::LinuxX64),
            recipe: recipe.map(|text| recipes::parse("test.toml", text).expect("test recipe")),
            trust,
        }
    }

    async fn run_install(
        env: &InstallEnv<MemoryFetch>,
        runtime: Runtime,
        version: Option<&str>,
        force: bool,
    ) -> (Result<InstalledVersion, InstallError>, Vec<InstallEvent>) {
        let mut events = Vec::new();
        let req = InstallRequest {
            runtime,
            version,
            force,
        };
        let result = install(&req, env, &mut |event| events.push(event)).await;
        (result, events)
    }

    fn runtimes(data: &TempDir) -> RuntimesDir {
        RuntimesDir::in_dir(data.path())
    }

    /// Only the lock is left in `runtimes/.tmp/`.
    fn assert_tmp_is_clean(data: &TempDir) {
        let names: Vec<_> = fs::read_dir(runtimes(data).tmp_dir())
            .expect("read .tmp")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, ["install.lock"]);
    }

    /// The artifacts asked for, without the manifest and its signature.
    fn archive_requests(fetch: &MemoryFetch) -> Vec<String> {
        fetch
            .requests()
            .into_iter()
            .filter(|url| url != MANIFEST && url != MANIFEST_SIG)
            .collect()
    }

    fn version(text: &str) -> Version {
        Version::parse(text).expect("test versions are valid")
    }

    fn fake_php(prints: &str) -> Served {
        Served::new("php", "linux-x64")
            .release("8.4.23", &[("cli", php_cli(prints)), ("fpm", php_fpm())])
            .release(
                "8.5.0",
                &[("cli", php_cli("PHP 8.5.0 ")), ("fpm", php_fpm())],
            )
            .release(
                "8.4.24-rc1",
                &[("cli", php_cli("PHP 8.4.24-rc1 ")), ("fpm", php_fpm())],
            )
    }

    // The check runs a `#!/bin/sh` script; Windows cannot run one. The flow
    // without a check is tested on every system below.
    #[cfg(unix)]
    #[tokio::test]
    async fn fake_php_installs_and_passes_its_check() {
        use std::os::unix::fs::PermissionsExt;

        let data = TempDir::new().expect("temp dir");
        let served = fake_php("PHP 8.4.23 (cli) (built: test)");
        let recipe = fake_php_with_check();
        let env = env(&data, served.fetch(5), Some(&recipe), Trust::Unsigned);
        let (result, events) = run_install(&env, Runtime::Php, Some("8.4"), false).await;

        let installed = result.expect("install succeeds");
        let dir = runtimes(&data).version_dir(Runtime::Php, &version("8.4.23"));
        assert_eq!(installed.dir, dir);
        assert!(!installed.already);
        assert!(dir.join("bin").join("php").is_file());
        assert!(dir.join("bin").join("php-fpm").is_file());
        assert_eq!(
            runtimes(&data).installed(Runtime::Php).expect("read"),
            [version("8.4.23")]
        );
        assert_tmp_is_clean(&data);
        let mode = fs::metadata(runtimes(&data).tmp_dir())
            .expect("stat .tmp")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        assert_eq!(
            fs::read_to_string(runtimes(&data).manifest_serial_path()).expect("serial"),
            "5\n"
        );
        assert_eq!(events[0], InstallEvent::ManifestVerified { serial: 5 });
        assert_eq!(
            events[1],
            InstallEvent::Selected {
                runtime: Runtime::Php,
                version: "8.4.23".into()
            }
        );
        assert!(events.contains(&InstallEvent::Checking));
        assert_eq!(events.last(), Some(&InstallEvent::Installed { dir }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_check_that_prints_another_version_fails() {
        let data = TempDir::new().expect("temp dir");
        let served = fake_php("PHP 8.4.22 (cli)");
        let recipe = fake_php_with_check();
        let env = env(&data, served.fetch(1), Some(&recipe), Trust::Unsigned);
        let (result, _) = run_install(&env, Runtime::Php, Some("8.4.23"), false).await;

        let err = result.expect_err("the check fails");
        assert_eq!(err.code(), "install_check_failed");
        assert!(err.to_string().contains("PHP 8.4.22"), "{err}");
        assert!(!runtimes(&data).runtime_dir(Runtime::Php).exists());
        assert_tmp_is_clean(&data);
    }

    #[tokio::test]
    async fn install_without_a_check_works_and_cleans_old_leftovers() {
        let data = TempDir::new().expect("temp dir");
        // What an install that was cut off leaves behind.
        let tmp = runtimes(&data).tmp_dir();
        fs::create_dir_all(tmp.join("i-00000000").join("root")).expect("old work dir");
        fs::create_dir_all(tmp.join("old-00000000")).expect("old version");
        fs::write(tmp.join("serial-00000000"), "9").expect("old serial file");

        let env = env(
            &data,
            fake_php("").fetch(1),
            Some(FAKE_PHP),
            Trust::Unsigned,
        );
        let (result, events) = run_install(&env, Runtime::Php, None, false).await;

        let installed = result.expect("install succeeds");
        assert_eq!(installed.version, version("8.5.0"), "the newest stable");
        assert!(installed.dir.join("bin").join("php").is_file());
        assert_tmp_is_clean(&data);

        // Each file comes in one chunk: progress at 0 and at the end.
        let file_events = |name: &str, size: usize| {
            let size = size as u64;
            let artifact = || name.to_owned();
            [
                InstallEvent::Downloading {
                    artifact: artifact(),
                    done: 0,
                    total: size,
                },
                InstallEvent::Downloading {
                    artifact: artifact(),
                    done: size,
                    total: size,
                },
                InstallEvent::Verified {
                    artifact: artifact(),
                },
                InstallEvent::Extracting {
                    artifact: artifact(),
                },
            ]
        };
        let mut expected = vec![
            InstallEvent::ManifestVerified { serial: 1 },
            InstallEvent::Selected {
                runtime: Runtime::Php,
                version: "8.5.0".into(),
            },
        ];
        expected.extend(file_events("cli", php_cli("PHP 8.5.0 ").len()));
        expected.extend(file_events("fpm", php_fpm().len()));
        expected.push(InstallEvent::Installed { dir: installed.dir });
        assert_eq!(events, expected);
    }

    #[tokio::test]
    async fn composer_is_saved_as_one_raw_file() {
        let data = TempDir::new().expect("temp dir");
        let phar = vec![7u8; 600 * 1024];
        let served = Served::new("composer", "any").release("2.8.12", &[("phar", phar.clone())]);
        // Sent in 16 KiB chunks, to see how often progress is reported.
        let chunks = phar.chunks(16 * 1024).map(<[u8]>::to_vec).collect();
        let fetch = served
            .fetch(1)
            .with(&url("composer", "2.8.12", "phar"), Reply::Chunks(chunks));
        let env = env(&data, fetch, None, Trust::Unsigned);
        let (result, events) = run_install(&env, Runtime::Composer, Some("2"), false).await;

        let installed = result.expect("install succeeds");
        let file = installed.dir.join("composer.phar");
        assert_eq!(fs::read(file).expect("read phar"), phar);
        let done: Vec<u64> = events
            .iter()
            .filter_map(|event| match event {
                InstallEvent::Downloading { done, .. } => Some(*done),
                _ => None,
            })
            .collect();
        assert_eq!(done, [0, 256 * 1024, 512 * 1024, 600 * 1024]);
        assert_tmp_is_clean(&data);
    }

    #[tokio::test]
    async fn an_installed_version_is_not_downloaded_again() {
        let data = TempDir::new().expect("temp dir");
        let served = fake_php("");
        let first = env(&data, served.fetch(1), Some(FAKE_PHP), Trust::Unsigned);
        run_install(&first, Runtime::Php, Some("8.4"), false)
            .await
            .0
            .expect("first install");

        let again = env(&data, served.fetch(1), Some(FAKE_PHP), Trust::Unsigned);
        let (result, events) = run_install(&again, Runtime::Php, Some("8.4"), false).await;
        let installed = result.expect("already there");
        assert!(installed.already);
        assert_eq!(installed.version, version("8.4.23"));
        assert!(archive_requests(&again.fetch).is_empty());
        // Every run ends with a terminal event.
        assert_eq!(
            events.last(),
            Some(&InstallEvent::Installed { dir: installed.dir })
        );
    }

    #[tokio::test]
    async fn force_replaces_the_installed_version() {
        let data = TempDir::new().expect("temp dir");
        let old = Served::new("php", "linux-x64").release(
            "8.4.23",
            &[("cli", tar_gz("php", b"old", 0o755)), ("fpm", php_fpm())],
        );
        let first = env(&data, old.fetch(1), Some(FAKE_PHP), Trust::Unsigned);
        run_install(&first, Runtime::Php, None, false)
            .await
            .0
            .expect("first install");

        let new = Served::new("php", "linux-x64").release(
            "8.4.23",
            &[("cli", tar_gz("php", b"new", 0o755)), ("fpm", php_fpm())],
        );
        let again = env(&data, new.fetch(2), Some(FAKE_PHP), Trust::Unsigned);
        let (result, _) = run_install(&again, Runtime::Php, None, true).await;

        let installed = result.expect("reinstall");
        assert!(!installed.already);
        let php = installed.dir.join("bin").join("php");
        assert_eq!(fs::read(php).expect("read php"), b"new");
        assert_eq!(
            runtimes(&data).installed(Runtime::Php).expect("read"),
            [version("8.4.23")]
        );
        assert_tmp_is_clean(&data);
    }

    #[tokio::test]
    async fn a_bad_checksum_on_the_second_file_leaves_nothing() {
        let data = TempDir::new().expect("temp dir");
        // Same size, one byte changed.
        let mut changed = php_fpm();
        changed[4] ^= 1;
        let fetch = fake_php("")
            .fetch(1)
            .with(&url("php", "8.4.23", "fpm"), Reply::Chunks(vec![changed]));
        let env = env(&data, fetch, Some(FAKE_PHP), Trust::Unsigned);
        let (result, _) = run_install(&env, Runtime::Php, Some("8.4"), false).await;

        let err = result.expect_err("bad fpm file");
        assert_eq!(err.code(), "checksum_mismatch");
        assert!(!runtimes(&data).runtime_dir(Runtime::Php).exists());
        assert_tmp_is_clean(&data);
    }

    #[tokio::test]
    async fn a_recipe_file_missing_from_the_manifest_downloads_nothing() {
        let data = TempDir::new().expect("temp dir");
        let served = Served::new("php", "linux-x64").release("8.4.23", &[("cli", php_cli(""))]);
        let env = env(&data, served.fetch(1), Some(FAKE_PHP), Trust::Unsigned);
        let (result, _) = run_install(&env, Runtime::Php, None, false).await;

        let err = result.expect_err("fpm is missing");
        assert_eq!(err.code(), "install_manifest_mismatch");
        assert!(archive_requests(&env.fetch).is_empty());
    }

    #[tokio::test]
    async fn a_bad_signature_downloads_nothing() {
        let data = TempDir::new().expect("temp dir");
        let key = minisign_verify::PublicKey::decode(fixture!("test-key.pub")).expect("test key");
        let fetch = MemoryFetch::new()
            .with(MANIFEST, Reply::Chunks(vec![fixture!("valid.json").into()]))
            .with(
                MANIFEST_SIG,
                Reply::Chunks(vec![fixture!("valid.json.other-key.minisig").into()]),
            );
        let env = env(&data, fetch, None, Trust::Key(key));
        let (result, _) = run_install(&env, Runtime::Php, Some("8.4.23"), false).await;

        let err = result.expect_err("signed by another key");
        assert_eq!(err.code(), "manifest_bad_signature");
        assert_eq!(env.fetch.requests(), [MANIFEST, MANIFEST_SIG]);
        assert!(!runtimes(&data).runtime_dir(Runtime::Php).exists());
        assert!(!runtimes(&data).manifest_serial_path().exists());
    }

    #[tokio::test]
    async fn the_serial_is_saved_and_an_older_manifest_is_refused() {
        let data = TempDir::new().expect("temp dir");
        let signed = || {
            let key =
                minisign_verify::PublicKey::decode(fixture!("test-key.pub")).expect("test key");
            let fetch = MemoryFetch::new()
                .with(MANIFEST, Reply::Chunks(vec![fixture!("valid.json").into()]))
                .with(
                    MANIFEST_SIG,
                    Reply::Chunks(vec![fixture!("valid.json.minisig").into()]),
                );
            env(&data, fetch, None, Trust::Key(key))
        };
        let serial_path = runtimes(&data).manifest_serial_path();

        // valid.json (serial 3) is accepted and saved, even though it has no
        // linux-x64 build for this install.
        let (result, _) = run_install(&signed(), Runtime::Php, Some("8.4.23"), false).await;
        let err = result.expect_err("no linux-x64 build");
        assert_eq!(err.code(), "install_unsupported_platform");
        assert_eq!(fs::read_to_string(&serial_path).expect("serial"), "3\n");

        fs::write(&serial_path, "4\n").expect("a newer serial was seen");
        let env = signed();
        let (result, _) = run_install(&env, Runtime::Php, Some("8.4.23"), false).await;
        assert_eq!(result.expect_err("older").code(), "manifest_rollback");
        assert_eq!(env.fetch.requests(), [MANIFEST, MANIFEST_SIG]);

        fs::write(&serial_path, "four").expect("a broken serial file");
        let (result, _) = run_install(&signed(), Runtime::Php, Some("8.4.23"), false).await;
        assert_eq!(result.expect_err("broken").code(), "install_serial_corrupt");
    }

    #[tokio::test]
    async fn a_held_lock_is_install_busy() {
        let data = TempDir::new().expect("temp dir");
        let runtimes = runtimes(&data);
        fs::create_dir_all(runtimes.tmp_dir()).expect("create .tmp");
        let other = File::create(runtimes.lock_path()).expect("create lock");
        other.lock().expect("hold the lock");

        let env = env(
            &data,
            fake_php("").fetch(1),
            Some(FAKE_PHP),
            Trust::Unsigned,
        );
        let (result, _) = run_install(&env, Runtime::Php, None, false).await;
        assert_eq!(result.expect_err("busy").code(), "install_busy");
        assert!(env.fetch.requests().is_empty());

        drop(other);
        let (result, _) = run_install(&env, Runtime::Php, None, false).await;
        result.expect("free again");
    }

    #[tokio::test]
    async fn an_unknown_platform_is_refused_first() {
        let data = TempDir::new().expect("temp dir");
        let mut env = env(&data, MemoryFetch::new(), None, Trust::Unsigned);
        env.platform = None;
        let (result, _) = run_install(&env, Runtime::Node, None, false).await;
        assert_eq!(
            result.expect_err("no platform").code(),
            "install_unsupported_platform"
        );
        assert!(env.fetch.requests().is_empty());
    }

    #[test]
    fn the_install_future_is_send() {
        fn assert_send<T: Send>(_: &T) {}
        let data = TempDir::new().expect("temp dir");
        let env = InstallEnv::production(data.path()).expect("build client");
        let req = InstallRequest {
            runtime: Runtime::Php,
            version: None,
            force: false,
        };
        let mut events = |_| {};
        let future = install(&req, &env, &mut events);
        assert_send(&future);
    }

    /// A small recipe whose check runs `bin/php -v`.
    #[cfg(unix)]
    fn check_recipe() -> crate::recipes::Recipe {
        recipes::parse("test.toml", &fake_php_with_check()).expect("test recipe")
    }

    /// Writes `script` as `root/bin/php` and runs the check on it.
    #[cfg(unix)]
    async fn check_script(script: &str, timeout: Duration) -> Result<(), InstallError> {
        use std::os::unix::fs::PermissionsExt;

        let root = TempDir::new().expect("temp dir");
        let bin = root.path().join("bin");
        fs::create_dir(&bin).expect("bin");
        let php = bin.join("php");
        fs::write(&php, format!("#!/bin/sh\n{script}\n")).expect("script");
        fs::set_permissions(&php, fs::Permissions::from_mode(0o755)).expect("chmod");
        let recipe = check_recipe();
        let check = recipe.check().expect("check");
        let programs = recipe.platform(Platform::LinuxX64).expect("linux");
        run_check(check, programs, root.path(), &version("8.4.23"), timeout).await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_check_needs_the_prefix_and_a_success_exit() {
        let ok = check_script("echo 'PHP 8.4.23 (cli)'", CHECK_TIMEOUT).await;
        ok.expect("right version, exit 0");

        let err = check_script("echo 'PHP 8.4.23 (cli)'; exit 3", CHECK_TIMEOUT)
            .await
            .expect_err("right version, but exit 3");
        assert!(
            matches!(
                err,
                InstallError::CheckFailed {
                    exit_code: Some(3),
                    ..
                }
            ),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_check_times_out_and_caps_its_output() {
        let started = std::time::Instant::now();
        let err = check_script("exec sleep 5", Duration::from_millis(200))
            .await
            .expect_err("too slow");
        assert!(err.to_string().contains("no answer after 200 ms"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(4));

        // `yes` never stops on its own. Without the 64 KiB cap this would
        // read until the timeout; with it, `yes` gets a broken pipe and ends.
        let err = check_script("exec yes 'PHP 8.4.23 x'", Duration::from_secs(10))
            .await
            .expect_err("killed by a broken pipe");
        let InstallError::CheckFailed { output, .. } = &err else {
            panic!("expected install_check_failed, got {err}");
        };
        assert!(!output.contains("no answer"), "{output}");
        assert_eq!(output.chars().count(), CHECK_OUTPUT_SHOWN);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_check_does_not_see_the_removed_variables() {
        // The test cannot change its own environment (`set_var` is unsafe),
        // so the variables are set on the command, before the clean.
        let mut command = Command::new("/bin/sh");
        let mut script = String::from("printf \"[");
        for name in CHECK_ENV_REMOVED {
            command.env(name, "leak");
            script.push_str(&format!("${{{name}-}}"));
        }
        script.push_str("]\"");
        command.arg("-c").arg(script);
        clean_check_env(&mut command);
        let (status, stdout, _) = run_capped(command, CHECK_TIMEOUT)
            .await
            .expect("in time")
            .expect("runs");
        assert!(status.success());
        assert_eq!(stdout, b"[]");
    }

    #[test]
    fn the_missing_dll_code_is_the_windows_status() {
        assert_eq!(STATUS_DLL_NOT_FOUND, -1_073_741_515);
    }

    #[test]
    fn a_failed_second_rename_puts_the_old_version_back() {
        let data = TempDir::new().expect("temp dir");
        let tmp = data.path().join("tmp");
        fs::create_dir(&tmp).expect("tmp");
        let target = data.path().join("php").join("8.4.23");
        fs::create_dir_all(&target).expect("target");
        fs::write(target.join("x"), "old").expect("old file");
        let old = tmp.join("old-php-8.4.23-00000000");

        let missing_root = data.path().join("no-root");
        let err = move_into_place(&missing_root, &target, &old).expect_err("no root");
        assert_eq!(err.code(), "install_io");
        assert_eq!(fs::read_to_string(target.join("x")).expect("back"), "old");
        assert_eq!(fs::read_dir(&tmp).expect("read tmp").count(), 0);
    }

    #[test]
    fn a_version_moved_aside_by_a_crash_is_put_back() {
        let data = TempDir::new().expect("temp dir");
        let runtimes = runtimes(&data);
        let tmp = runtimes.tmp_dir();
        // Cut off between the two renames: the version is only in `.tmp/`.
        let aside = tmp.join("old-php-8.4.24-rc1-0a1b2c3d");
        fs::create_dir_all(&aside).expect("aside");
        fs::write(aside.join("x"), "kept").expect("file");
        // Its version folder is back already: the copy aside is deleted.
        let stale = tmp.join("old-node-22.11.0-0a1b2c3d");
        fs::create_dir_all(&stale).expect("stale");
        let node = runtimes.version_dir(Runtime::Node, &version("22.11.0"));
        fs::create_dir_all(&node).expect("node");
        // Not a moved-aside name: deleted.
        fs::create_dir_all(tmp.join("old-ruby-1.0.0-0a1b2c3d")).expect("other");
        fs::write(runtimes.lock_path(), "").expect("lock");

        clean_tmp(&runtimes);

        let php = runtimes.version_dir(Runtime::Php, &version("8.4.24-rc1"));
        assert_eq!(fs::read_to_string(php.join("x")).expect("put back"), "kept");
        assert!(node.is_dir());
        assert_tmp_is_clean(&data);
    }

    #[test]
    fn moved_aside_names_are_read_strictly() {
        let runtimes = RuntimesDir::in_dir(Path::new("data"));
        let target = |name: &str| moved_aside_target(&runtimes, std::ffi::OsStr::new(name));
        assert_eq!(
            target("old-php-8.4.23-deadbeef"),
            Some(runtimes.version_dir(Runtime::Php, &version("8.4.23")))
        );
        for bad in [
            "old-php-8.4.23",
            "old-php-8.4-deadbeef",
            "old-php-8.4.23-xyz12345",
            "old-ruby-1.0.0-deadbeef",
            "i-deadbeef",
            "old-php-../../x-deadbeef",
        ] {
            assert_eq!(target(bad), None, "{bad}");
        }
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
