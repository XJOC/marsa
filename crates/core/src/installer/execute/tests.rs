// Only the Unix tests run a fake check program.
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use tokio::process::Command;

#[cfg(unix)]
use super::super::check::{CHECK_ENV_REMOVED, CHECK_OUTPUT_SHOWN, clean_check_env, run_capped};
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
// `#[cfg(unix)]`: only the Unix tests run the `#!/bin/sh` fake check, so on
// Windows this helper would be dead code and fail clippy's `-D warnings`.
#[cfg(unix)]
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
        include_str!(concat!("../../../tests/fixtures/manifest/", $name))
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
            let sha = crate::hex::encode(&Sha256::digest(bytes));
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
        os: None,
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

/// A `.tar.gz` shaped like theseus-rs's PostgreSQL archives: every file
/// under `postgresql-<version>-<target>/`. `postgres` prints `prints`;
/// the other programs are placeholders.
// `#[cfg(unix)]`: the archive holds a `#!/bin/sh` fake `postgres`, which
// only Unix can run for the check.
#[cfg(unix)]
fn theseus_archive(top: &str, prints: &str) -> Vec<u8> {
    let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(gzip);
    let script = format!("#!/bin/sh\necho '{prints}'\n");
    let files = [
        ("bin/postgres", script.as_str()),
        ("bin/initdb", "x"),
        ("bin/pg_ctl", "x"),
        ("bin/psql", "x"),
        ("bin/pg_dump", "x"),
        ("bin/pg_restore", "x"),
        ("bin/pg_isready", "x"),
        ("share/postgres.bki", "x"),
        ("LICENSE", "x"),
    ];
    for (path, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, format!("{top}/{path}"), data.as_bytes())
            .expect("append to tar");
    }
    let gzip = builder.into_inner().expect("finish tar");
    gzip.finish().expect("finish gzip")
}

// `#[cfg(unix)]`: it runs the `#!/bin/sh` fake from `theseus_archive`.
#[cfg(unix)]
#[tokio::test]
async fn postgres_installs_from_a_theseus_shaped_archive() {
    let top = "postgresql-17.12.0-x86_64-unknown-linux-gnu";
    let data = TempDir::new().expect("temp dir");
    let served = Served::new("postgres", "linux-x64").release(
        "17.12.0",
        &[(
            "archive",
            theseus_archive(top, "postgres (PostgreSQL) 17.12"),
        )],
    );
    let mut first = env(&data, served.fetch(1), None, Trust::Unsigned);
    // The built-in recipe asks for glibc 2.36.
    first.os = Some(plan::OsVersions {
        macos: None,
        glibc: Some("2.36".to_owned()),
    });
    let (result, _) = run_install(&first, Runtime::Postgres, Some("17"), false).await;

    let installed = result.expect("install succeeds");
    let dir = runtimes(&data).version_dir(Runtime::Postgres, &version("17.12.0"));
    assert_eq!(installed.dir, dir);
    // The top folder is gone: programs sit in `bin/` of the version.
    for program in ["postgres", "initdb", "pg_ctl", "psql", "pg_dump"] {
        assert!(dir.join("bin").join(program).is_file(), "{program}");
    }
    assert!(dir.join("share").join("postgres.bki").is_file());
    assert!(!dir.join(top).exists());
    assert_tmp_is_clean(&data);

    // Another minor version in the output fails the check.
    let served = Served::new("postgres", "linux-x64").release(
        "17.12.0",
        &[(
            "archive",
            theseus_archive(top, "postgres (PostgreSQL) 17.1"),
        )],
    );
    let data = TempDir::new().expect("temp dir");
    let mut env = env(&data, served.fetch(1), None, Trust::Unsigned);
    env.os = Some(plan::OsVersions {
        macos: None,
        glibc: Some("2.36".to_owned()),
    });
    let (result, _) = run_install(&env, Runtime::Postgres, Some("17"), false).await;
    let err = result.expect_err("17.1 is not 17.12");
    assert_eq!(err.code(), "install_check_failed");
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
        let key = minisign_verify::PublicKey::decode(fixture!("test-key.pub")).expect("test key");
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

#[tokio::test]
async fn an_old_system_is_refused_before_any_download() {
    let data = TempDir::new().expect("temp dir");
    let recipe = FAKE_PHP.replace(
        "[platform.linux-x64]\n",
        "[platform.linux-x64]\nmin_glibc = \"2.36\"\n",
    );
    let mut env = env(&data, MemoryFetch::new(), Some(&recipe), Trust::Unsigned);
    env.os = Some(plan::OsVersions {
        macos: None,
        glibc: Some("2.35".to_owned()),
    });
    let (result, _) = run_install(&env, Runtime::Php, None, false).await;
    let err = result.expect_err("glibc 2.35 is too old");
    assert_eq!(err.code(), "install_os_too_old");
    assert!(err.to_string().contains("glibc 2.36"), "{err}");
    assert!(env.fetch.requests().is_empty(), "nothing was downloaded");
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
    assert_eq!(
        target("old-caddy-2.11.8-0a1b2c3d"),
        Some(runtimes.version_dir(Runtime::Caddy, &version("2.11.8")))
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
