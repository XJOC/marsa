//! Runs an install: takes the install lock, downloads and extracts into a
//! temp folder under `runtimes/.tmp/`, runs the recipe's check, then moves the
//! version folder into place atomically.
//!
//! Called by [`super::install`]; the order of the steps is documented there.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
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
use crate::runtimes::RuntimesDir;
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
/// The variables that would change how the check program starts.
const CHECK_ENV_REMOVED: [&str; 3] = ["PHPRC", "PHP_INI_SCAN_DIR", "NODE_OPTIONS"];

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

    // Held until this function returns; the OS frees it if the process dies.
    let _lock = lock(&runtimes)?;
    clean_tmp(&runtimes);

    let manifest = verified_manifest(env, &runtimes).await?;
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
        return Ok(InstalledVersion {
            runtime,
            version,
            dir: target,
            already: true,
        });
    }

    let plan = plan::build(runtime, release, programs, platform)?;
    // Deleted with everything in it when this function returns, on success
    // and on every error.
    let work = WorkDir::create(&runtimes.tmp_dir())?;
    for step in &plan.steps {
        download_and_extract(&env.fetch, step, &work, events).await?;
    }
    if let Some(check) = recipe.check() {
        events(InstallEvent::Checking);
        run_check(check, programs, &work.root(), &version).await?;
    }
    move_into_place(&work.root(), &target, &runtimes.tmp_dir()).await?;
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

/// Deletes everything in `runtimes/.tmp/` but the lock: what an install that
/// was cut off left behind. Runs under the lock, so no install is using it.
/// A failure is only logged: on Windows a running program cannot be deleted,
/// and the next install tries again.
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

/// Downloads the manifest and its signature, checks them against the last
/// serial accepted, and saves the new serial. The serial is saved before any
/// download, so a failed install still raises the floor.
async fn verified_manifest<F: Fetch>(
    env: &InstallEnv<F>,
    runtimes: &RuntimesDir,
) -> Result<Manifest, InstallError> {
    let json = fetch::fetch_bytes(&env.fetch, &env.manifest_url, MAX_MANIFEST_SIZE as u64).await?;
    let sig_url = format!("{}.minisig", env.manifest_url);
    let sig = fetch::fetch_bytes(&env.fetch, &sig_url, MAX_SIGNATURE_SIZE).await?;
    let sig = String::from_utf8(sig).map_err(|_| ManifestError::BadSignature)?;

    let serial_path = runtimes.manifest_serial_path();
    let last_seen = read_serial(&serial_path)?;
    let manifest = env.trust.verify(&json, &sig, last_seen)?;
    if last_seen != Some(manifest.serial()) {
        write_serial(&serial_path, &runtimes.tmp_dir(), manifest.serial())?;
    }
    Ok(manifest)
}

/// The saved serial, or `None` if there is none yet. A file that is not a
/// number is an error, not `None`: that would turn the rollback check off.
fn read_serial(path: &Path) -> Result<Option<u64>, InstallError> {
    match fs::read_to_string(path) {
        Ok(text) => text.trim().parse().map(Some).map_err(|_| {
            io_error(path)(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a serial number; delete the file to accept any manifest again",
            ))
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
        .and_then(|()| fs::rename(&temp, path));
    if let Err(err) = written {
        remove_quietly(&temp);
        return Err(io_error(path)(err));
    }
    Ok(())
}

/// `runtimes/.tmp/i-<8 hex>/`, new for each install, with `dl/` for the
/// downloads and `root/` for the version folder. Deleted on drop.
struct WorkDir {
    path: PathBuf,
}

impl WorkDir {
    fn create(tmp: &Path) -> Result<WorkDir, InstallError> {
        let mut tries = 0;
        let work = loop {
            // Short on purpose: Windows tools still trip on paths over 260
            // characters (architecture.md section 10).
            let path = tmp.join(format!("i-{}", random_hex()?));
            match fs::create_dir(&path) {
                Ok(()) => break WorkDir { path },
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
    fn drop(&mut self) {
        remove_tree(&self.path);
    }
}

/// Downloads one file into `dl/`, checks it, and unpacks it into `root/`.
async fn download_and_extract<F: Fetch>(
    fetch: &F,
    step: &ArtifactStep<'_>,
    work: &WorkDir,
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
    let archive = file.path().to_path_buf();
    let artifact = step.artifact.clone();
    let root = work.root();
    let task = tokio::task::spawn_blocking(move || {
        extract::extract(
            &archive,
            &Layout::from(&artifact),
            &root,
            &Limits::default(),
        )
    });
    let report = task.await.map_err(|err| InstallError::Io {
        path: work.root(),
        source: io::Error::other(format!("the extract task stopped: {err}")),
    })??;
    tracing::debug!(
        artifact = name,
        files = report.files,
        bytes = report.bytes,
        "extracted"
    );
    if let Err(err) = file.remove() {
        tracing::warn!(path = %dest.display(), error = %err, "cannot delete a download");
    }
    Ok(())
}

/// Runs the recipe's check program from `root` by its full path: no `PATH`
/// search, no shell. Its output must start with the expected text.
async fn run_check(
    check: &Check,
    programs: &PlatformRecipe,
    root: &Path,
    version: &Version,
) -> Result<(), InstallError> {
    let failed = |output: String, exit_code| InstallError::CheckFailed { output, exit_code };
    // The recipe parser checks that the program is in `programs`.
    let program = programs
        .program_path(root, &check.program)
        .ok_or_else(|| failed(format!("no program {:?}", check.program), None))?;
    let mut command = Command::new(&program);
    command
        .args(&check.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for name in CHECK_ENV_REMOVED {
        command.env_remove(name);
    }

    let output = async {
        let mut child = command.spawn()?;
        let (stdout, stderr) = tokio::join!(
            read_capped(child.stdout.take()),
            read_capped(child.stderr.take())
        );
        let status = child.wait().await?;
        io::Result::Ok((status, stdout?, stderr?))
    };
    // On a timeout the future is dropped, and `kill_on_drop` stops the child.
    let (status, stdout, stderr) = match tokio::time::timeout(CHECK_TIMEOUT, output).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            return Err(failed(
                format!("cannot run {}: {err}", program.display()),
                None,
            ));
        }
        Err(_) => {
            return Err(failed(
                format!("no answer after {} seconds", CHECK_TIMEOUT.as_secs()),
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

/// Reads at most [`CHECK_OUTPUT_MAX`] bytes, then closes the pipe.
async fn read_capped(pipe: Option<impl AsyncRead + Unpin>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if let Some(pipe) = pipe {
        pipe.take(CHECK_OUTPUT_MAX).read_to_end(&mut bytes).await?;
    }
    Ok(bytes)
}

/// Renames `root` to `target`. If `target` exists (`force`), it is first
/// moved into `tmp` and deleted after; if the second rename fails, it is
/// put back.
async fn move_into_place(root: &Path, target: &Path, tmp: &Path) -> Result<(), InstallError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(io_error(parent))?;
    }
    let old = match fs::symlink_metadata(target) {
        Ok(_) => {
            let old = tmp.join(format!("old-{}", random_hex()?));
            rename(target, &old).await?;
            Some(old)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(io_error(target)(err)),
    };
    if let Err(err) = rename(root, target).await {
        if let Some(old) = &old
            && let Err(back) = rename(old, target).await
        {
            tracing::error!(
                from = %old.display(),
                to = %target.display(),
                error = %back,
                "cannot put the old version back"
            );
        }
        return Err(err);
    }
    if let Some(old) = old {
        // On Windows a running program cannot be deleted: then the old folder
        // stays in `.tmp/` until the next install cleans it.
        remove_tree(&old);
    }
    Ok(())
}

/// `fs::rename`, tried again on Windows while the error is "access denied":
/// antivirus programs hold files that were just unpacked for a short time.
/// rustup does the same.
async fn rename(from: &Path, to: &Path) -> Result<(), InstallError> {
    let mut delay = Duration::from_millis(100);
    let mut tries = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            // `cfg!` outside `platform`: one known Windows behavior, std only.
            Err(err)
                if cfg!(windows) && err.kind() == io::ErrorKind::PermissionDenied && tries < 5 =>
            {
                tokio::time::sleep(delay).await;
                delay *= 2;
                tries += 1;
            }
            Err(err) => return Err(io_error(from)(err)),
        }
    }
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
        assert!(!events.contains(&InstallEvent::Checking));
        assert_tmp_is_clean(&data);
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
        let (result, _) = run_install(&again, Runtime::Php, Some("8.4"), false).await;
        let installed = result.expect("already there");
        assert!(installed.already);
        assert_eq!(installed.version, version("8.4.23"));
        assert!(archive_requests(&again.fetch).is_empty());
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
        assert_eq!(result.expect_err("broken").code(), "install_io");
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
}
