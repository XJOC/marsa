//! `cargo xtask manifest`: build `manifest.json` from `recipes/pins.toml`
//! (task M2-07), and verify a signed one.
//!
//! The tool never signs. The owner signs on their own machine with
//! `minisign`, because the secret key never enters the repository or CI.

mod build;
mod php_smoke;
mod pins;
mod upstream;
mod verify;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use marsa_core::installer::fetch::{FetchError, HttpFetch, fetch_bytes};
use marsa_core::installer::manifest::MAX_MANIFEST_SIZE;
use marsa_core::recipes::{self, Platform};
use marsa_core::runtimes::Runtime;
use semver::Version;
use serde::Deserialize;

pub use verify::run as verify;

use build::{Inspect, Target};
use pins::Pins;

/// Where the published manifest lives (M2-02, question 1).
const PUBLISHED_URL: &str =
    "https://github.com/XJOC/marsa/releases/download/runtimes-manifest/manifest.json";

/// Where `build` gets the serial of the manifest published now.
pub enum Previous {
    /// Download [`PUBLISHED_URL`]; HTTP 404 means nothing is published yet.
    Download,
    /// Read this local `manifest.json`.
    File(PathBuf),
    /// This number.
    Serial(u64),
}

pub struct BuildOptions {
    pub pins: PathBuf,
    pub out: PathBuf,
    pub previous: Previous,
}

/// The workspace root: the parent of `xtask/`.
pub fn workspace_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .context("xtask is not inside the workspace")
}

/// Runs the real PHP checks (`php_smoke`) for our macOS and Linux archives.
struct Smoke<'a> {
    spc_line: &'a str,
    work: PathBuf,
}

impl Inspect for Smoke<'_> {
    fn php_build(&self, target: &Target<'_>, file: &Path) -> Result<Vec<String>> {
        let work = self.work.join(format!(
            "php-{}-{}-{}",
            target.version, target.platform, target.artifact.name
        ));
        php_smoke::check_archive(file, target, self.spc_line, &work)
    }
}

/// `cargo xtask manifest check-php`: the checks of `build` on one local PHP
/// archive, without the manifest. The build workflow runs it on each archive
/// it makes, on the runner that made it.
pub fn run_check_php(
    archive: &Path,
    platform: Platform,
    version: &Version,
    pins: &Path,
) -> Result<()> {
    let text =
        fs::read_to_string(pins).with_context(|| format!("cannot read {}", pins.display()))?;
    let pins = Pins::parse(&text).with_context(|| pins.display().to_string())?;
    ensure!(
        build::is_spc_platform(platform),
        "{platform} has no static-php-cli PHP build"
    );
    let recipe = recipes::builtin(Runtime::Php)?
        .platform(platform)
        .with_context(|| format!("the php recipe has no {platform}"))?;
    let [artifact] = recipe.artifacts() else {
        bail!("the php recipe for {platform} must have one archive");
    };
    let target = Target {
        version,
        platform,
        recipe,
        artifact,
    };
    let work = workspace_root()?
        .join("target")
        .join("check-php")
        .join(format!("{version}-{platform}"));
    let warnings = php_smoke::check_archive(archive, &target, &pins.spc_line, &work)?;
    for warning in &warnings {
        println!("warning: {warning}");
    }
    println!("{}: PHP checks passed", archive.display());
    Ok(())
}

pub fn run_build(options: BuildOptions) -> Result<()> {
    let text = fs::read_to_string(&options.pins)
        .with_context(|| format!("cannot read {}", options.pins.display()))?;
    let pins = Pins::parse(&text).with_context(|| options.pins.display().to_string())?;

    // The static-php-cli checks need macOS: `nm` for the Mach-O files (Apple's
    // nm is llvm-nm, which reads the Linux ELF files too), and running the
    // macOS `php`. No way to skip them: a PHP build is never published
    // unchecked.
    let darwin_php = !pins.versions(Runtime::Php).is_empty()
        && recipes::builtin(Runtime::Php)?
            .platforms()
            .any(|(p, _)| matches!(p, Platform::DarwinArm64 | Platform::DarwinX64));
    if darwin_php && std::env::consts::OS != "macos" {
        bail!("pins.toml has PHP for macOS, and its checks run only on macOS; run this on a Mac");
    }

    let runtime = tokio::runtime::Runtime::new().context("cannot start tokio")?;
    let fetch = HttpFetch::new()?;

    let published = match &options.previous {
        Previous::Serial(serial) => Some(*serial),
        Previous::File(path) => {
            Some(serial_of(&fs::read(path).with_context(|| {
                format!("cannot read {}", path.display())
            })?)?)
        }
        Previous::Download => {
            match runtime.block_on(fetch_bytes(&fetch, PUBLISHED_URL, MAX_MANIFEST_SIZE as u64)) {
                Ok(bytes) => Some(serial_of(&bytes)?),
                Err(FetchError::HttpStatus { status: 404, .. }) => {
                    println!("No manifest is published yet ({PUBLISHED_URL} is 404).");
                    None
                }
                Err(err) => {
                    return Err(anyhow::Error::from(err).context(
                        "cannot read the published serial; pass --previous-serial or --previous-manifest",
                    ));
                }
            }
        }
    };
    build::check_serial(pins.serial, published)?;

    let cache = options.out.join("cache");
    let smoke = Smoke {
        spc_line: &pins.spc_line,
        work: options.out.join("work"),
    };
    let built = runtime.block_on(build::build(&fetch, &pins, &cache, &smoke))?;
    let json = build::to_json(&built.manifest)?;
    verify::check_json(json.as_bytes()).context("the built manifest breaks a rule")?;

    let path = options.out.join("manifest.json");
    fs::write(&path, &json).with_context(|| format!("cannot write {}", path.display()))?;

    println!("\nFiles:");
    for row in &built.rows {
        println!("  {row}");
    }
    if !built.warnings.is_empty() {
        println!("\nWarnings:");
        for warning in &built.warnings {
            println!("  {warning}");
        }
    }
    println!(
        "\nWrote {}\n{}",
        path.display(),
        verify::summary(&built.manifest)
    );
    println!("Read it, then sign it:\n");
    println!(
        "  minisign -S -s ~/.minisign/marsa-runtimes.key -m {} -t \"{}\"\n",
        path.display(),
        verify::trusted_comment(pins.serial)
    );
    println!(
        "Then check the signature:\n\n  cargo xtask manifest verify {}",
        path.display()
    );
    Ok(())
}

fn serial_of(bytes: &[u8]) -> Result<u64> {
    #[derive(Deserialize)]
    struct Probe {
        serial: u64,
    }
    let probe: Probe =
        serde_json::from_slice(bytes).context("the published manifest has no serial")?;
    Ok(probe.serial)
}
