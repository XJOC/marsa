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

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use semver::Version;

use super::check::{CHECK_TIMEOUT, run_check};
use super::extract::{self, Layout, Limits};
use super::fetch::{self, Expected, Fetch};
use super::fs::{io_error, lock, move_into_place, random_hex, remove_tree, rename, sync_tree};
use super::manifest::{MAX_MANIFEST_SIZE, MAX_SIGNATURE_SIZE, Manifest, ManifestError};
use super::plan::{self, ArtifactStep};
use super::{InstallEnv, InstallError, InstallRequest, InstalledVersion};
use crate::recipes::{Check, PlatformRecipe};
use crate::runtimes::{Runtime, RuntimesDir};
use crate::types::InstallEvent;

/// At most one `Downloading` event per this many bytes.
const PROGRESS_STEP: u64 = 256 * 1024;
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
    // Before any download: an old system is refused at once.
    let found = match (&env.os, programs.min_macos().or(programs.min_glibc())) {
        (Some(os), _) => os.clone(),
        (None, Some(_)) => plan::OsVersions::probe().await,
        (None, None) => plan::OsVersions::default(),
    };
    plan::check_minimum(runtime, programs, &found)?;
    plan::check_xz(runtime, programs, extract::xz_available())?;
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
    let release = plan::select(&manifest, runtime, req.version, platform)?;
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

/// The signature size limit, as the fetch wants it.
fn signature_limit() -> u64 {
    u64::try_from(MAX_SIGNATURE_SIZE).expect("4096 fits in u64")
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
    let runtime = Runtime::ALL.into_iter().find(|r| r.dir_name() == runtime)?;
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
    let sig = fetch::fetch_bytes(&env.fetch, &sig_url, signature_limit()).await?;
    let sig = String::from_utf8(sig).map_err(|_| ManifestError::BadSignature)?;

    let serial_path = runtimes.manifest_serial_path();
    let last_seen = read_serial(&serial_path)?;
    let manifest = env.trust.verify(&json, &sig, last_seen)?;
    let serial = manifest.serial();
    if last_seen != Some(serial) {
        let lock = Arc::clone(lock);
        blocking(&serial_path.clone(), move || {
            let result = write_serial(&serial_path, serial);
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

/// Writes `serial` over `path`, whole.
fn write_serial(path: &Path, serial: u64) -> Result<(), InstallError> {
    crate::fs::write_atomic(path, format!("{serial}\n").as_bytes(), None).map_err(io_error(path))
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

#[cfg(test)]
mod tests;
