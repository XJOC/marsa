//! `cargo xtask manifest build`: for every pinned version, platform, and
//! artifact, check that its URLs can give the proof its runtime needs,
//! download the file (or reuse the cache), hash it, compare with the
//! upstream sum (sha256, or sha512 for Caddy), run the PHP checks, then
//! write `manifest.json`. A database server without the proof of its own
//! source stops the build; the older runtimes only get a warning without an
//! upstream sum. An MD5 alone is never accepted.
//!
//! The database servers' archives are large (PostgreSQL is about 60 MB a
//! file, 620 MB a build), so they are hashed on the way and not kept.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use marsa_core::installer::fetch::{ChunkSink, Fetch, FetchError, fetch_bytes};
use marsa_core::installer::manifest::{MAX_ARTIFACT_SIZE, MIN_SERIAL, SCHEMA_VERSION};
use marsa_core::recipes::{self, Artifact, Platform, PlatformRecipe};
use marsa_core::runtimes::Runtime;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};

use super::attest;
use super::pins::{Pins, RUNTIMES};
use super::upstream::{
    Algorithm, Layout, MYSQL_FOLDERS, Source, SumFile, hex, parse_hex, sum_file_for,
};

/// The JSON that `marsa_core::installer::manifest` reads. Field
/// order is the output order, and maps are `BTreeMap`, so the same inputs
/// give the same bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ManifestJson {
    pub schema: u64,
    pub serial: u64,
    pub runtimes: RuntimesJson,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(super) struct RuntimesJson {
    #[serde(default)]
    pub php: Vec<ReleaseJson>,
    #[serde(default)]
    pub node: Vec<ReleaseJson>,
    #[serde(default)]
    pub composer: Vec<ReleaseJson>,
    #[serde(default)]
    pub caddy: Vec<ReleaseJson>,
    #[serde(default)]
    pub postgres: Vec<ReleaseJson>,
    /// Left out while empty, so a manifest without MySQL stays as it was.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mysql: Vec<ReleaseJson>,
    /// Left out while empty, as MySQL.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub valkey: Vec<ReleaseJson>,
}

impl RuntimesJson {
    pub(super) fn get(&self, runtime: Runtime) -> &Vec<ReleaseJson> {
        match runtime {
            Runtime::Php => &self.php,
            Runtime::Node => &self.node,
            Runtime::Composer => &self.composer,
            Runtime::Caddy => &self.caddy,
            Runtime::Postgres => &self.postgres,
            Runtime::Mysql => &self.mysql,
            Runtime::Valkey => &self.valkey,
        }
    }

    fn get_mut(&mut self, runtime: Runtime) -> &mut Vec<ReleaseJson> {
        match runtime {
            Runtime::Php => &mut self.php,
            Runtime::Node => &mut self.node,
            Runtime::Composer => &mut self.composer,
            Runtime::Caddy => &mut self.caddy,
            Runtime::Postgres => &mut self.postgres,
            Runtime::Mysql => &mut self.mysql,
            Runtime::Valkey => &mut self.valkey,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ReleaseJson {
    pub version: String,
    pub platforms: BTreeMap<String, Vec<SourceJson>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct SourceJson {
    pub name: String,
    pub urls: Vec<String>,
    /// 64 lowercase hex characters.
    pub sha256: String,
    pub size: u64,
}

/// The manifest as written to disk: pretty JSON and a final newline.
pub(super) fn to_json(manifest: &ManifestJson) -> Result<String> {
    Ok(serde_json::to_string_pretty(manifest)? + "\n")
}

/// One file being built, for the checks and for messages.
pub(super) struct Target<'a> {
    pub version: &'a Version,
    pub platform: Platform,
    pub recipe: &'a PlatformRecipe,
    pub artifact: &'a Artifact,
}

/// Checks that need real programs. Tests pass a stub.
pub(super) trait Inspect {
    /// Checks one of our static-php-cli PHP archives (macOS or Linux).
    /// Returns warnings.
    fn php_build(&self, target: &Target<'_>, file: &Path) -> Result<Vec<String>>;
    /// `gh attestation verify` on one theseus-rs archive, pinned to its
    /// repository, signer workflow, and tag (`attest`). Returns its JSON.
    fn attestation(&self, file: &Path, version: &Version) -> Result<String>;
}

/// Platforms where PHP is our own static-php-cli build (ADR 0012).
pub(super) fn is_spc_platform(platform: Platform) -> bool {
    matches!(
        platform,
        Platform::DarwinArm64 | Platform::DarwinX64 | Platform::LinuxX64 | Platform::LinuxArm64
    )
}

/// The upstream proof a file needs before its sha256 goes in the manifest.
/// The database servers of M6 each need the proof of their own source (ADR
/// 0023, section 3); without it the build stops. An `Md5` sum is never
/// accepted alone: only [`Proof::OracleSignature`] takes one, together with
/// Oracle's GPG signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Proof {
    /// The upstream sum if the source publishes one `build` knows. Without
    /// one, only a warning: today, Windows PHP in `releases/archives/`
    /// (windows.php.net lists only sha1 there). PHP, Node, Composer, Caddy.
    SumIfPublished,
    /// The theseus-rs `<archive>.sha256` ([`Layout::Single`]) and no other
    /// source, of the recipe's own URL, and GitHub's attestation that the
    /// archive was built by theseus-rs' workflow (`attest`). PostgreSQL.
    TheseusSha256,
    /// Oracle's GPG signature `<archive>.asc` by the pinned key, and its
    /// `<archive>.md5`, both ([`check_oracle`]). The MySQL archive of the
    /// recipe's own URL, or the same file name in Oracle's other folder
    /// (`archives/`, for a version that is not current any more).
    OracleSignature,
    /// The `sha256sums.txt` of one of our own `libaio-<v>-r<n>` releases,
    /// which MySQL's recipe names for `libaio.so.1` and `libnuma.so.1`. No
    /// upstream signs them: the trust is in our build and in the owner's
    /// signature on the manifest.
    OurLibaio,
    /// The `sha256sums.txt` of one of our own `valkey-<v>-r<n>` releases
    /// (ADR 0023, section 3): the archive the Valkey recipe names, and no
    /// other. The build workflow checked the source tarball against
    /// `valkey-io/valkey-hashes`; the trust is in that build and in the
    /// owner's signature on the manifest.
    OurValkey,
}

impl Proof {
    /// An exhaustive `match`, so each new runtime has to choose. MySQL's
    /// `archive` is Oracle's; its other files are our own builds. Valkey is
    /// our own build.
    fn of(runtime: Runtime, artifact: &str) -> Proof {
        match runtime {
            Runtime::Php | Runtime::Node | Runtime::Composer | Runtime::Caddy => {
                Proof::SumIfPublished
            }
            Runtime::Postgres => Proof::TheseusSha256,
            Runtime::Mysql if artifact == "archive" => Proof::OracleSignature,
            Runtime::Mysql => Proof::OurLibaio,
            Runtime::Valkey => Proof::OurValkey,
        }
    }

    /// Whether the file at `url` can give this proof, before anything is
    /// downloaded. `sums` is the sum file [`sum_file_for`] finds for `url`
    /// (`None`: no known one), and `template` the recipe's URL for the file.
    /// The sum is compared after the download.
    fn allows(self, url: &str, template: Option<&str>, sums: Option<&SumFile>) -> Result<()> {
        match (self, sums) {
            (Proof::SumIfPublished | Proof::TheseusSha256, Some(sums))
                if sums.algorithm == Algorithm::Md5 =>
            {
                bail!(
                    "its only upstream sum is the MD5 {}, and an MD5 is never accepted alone",
                    sums.url
                )
            }
            (Proof::SumIfPublished, _) => Ok(()),
            // An override could name another theseus-rs archive, of another
            // version or target, with its own matching `.sha256`.
            (Proof::TheseusSha256, _) if template != Some(url) => bail!(
                "it is not the recipe's URL {}, and only that theseus-rs archive is accepted",
                template.unwrap_or("(none)")
            ),
            (Proof::TheseusSha256, Some(sums))
                if sums.source == Source::Theseus
                    && sums.layout == Layout::Single
                    && sums.algorithm == Algorithm::Sha256 =>
            {
                Ok(())
            }
            (Proof::TheseusSha256, Some(sums)) => bail!(
                "its sum would come from {}, but only the theseus-rs `.sha256` is accepted",
                sums.url
            ),
            (Proof::TheseusSha256, None) => bail!(
                "no upstream sum that `manifest build` knows, and only the theseus-rs \
                 `.sha256` is accepted"
            ),
            // Another signed Oracle archive, such as an older MySQL with
            // known holes, would pass the signature check too.
            (Proof::OracleSignature, _) if !is_recipes_archive(url, template) => bail!(
                "it is not the recipe's URL {}, and only that Oracle archive is accepted",
                template.unwrap_or("(none)")
            ),
            (Proof::OracleSignature, Some(sums))
                if sums.source == Source::Oracle
                    && sums.layout == Layout::Single
                    && sums.algorithm == Algorithm::Md5 =>
            {
                Ok(())
            }
            (Proof::OracleSignature, _) => bail!(
                "only an Oracle MySQL 8.4 archive on cdn.mysql.com, with its `.md5` and its \
                 signature `.asc`, is accepted"
            ),
            // A file of another release, or of another architecture, would
            // carry its own matching sum: so the file is the recipe's.
            (Proof::OurLibaio, _) if template != Some(url) => bail!(
                "it is not the recipe's URL {}, and only that file of our libaio release is \
                 accepted",
                template.unwrap_or("(none)")
            ),
            (Proof::OurLibaio, Some(sums))
                if sums.source == Source::OurLibaio
                    && sums.layout == Layout::List
                    && sums.algorithm == Algorithm::Sha256 =>
            {
                Ok(())
            }
            (Proof::OurLibaio, _) => bail!(
                "only a file of our own libaio release, with its `sha256sums.txt`, is accepted"
            ),
            // An override could name another of our Valkey archives, of
            // another version, revision, or platform, with its own matching
            // sum: so the file is the recipe's. A rebuild changes the recipe.
            (Proof::OurValkey, _) if template != Some(url) => bail!(
                "it is not the recipe's URL {}, and only that archive of our Valkey release is \
                 accepted",
                template.unwrap_or("(none)")
            ),
            (Proof::OurValkey, Some(sums))
                if sums.source == Source::OurValkey
                    && sums.layout == Layout::List
                    && sums.algorithm == Algorithm::Sha256 =>
            {
                Ok(())
            }
            (Proof::OurValkey, _) => bail!(
                "only an archive of our own Valkey release, with its `sha256sums.txt`, is \
                 accepted"
            ),
        }
    }

    /// Whether the file is kept in the cache after the build: only when a
    /// later check reads it (PHP, the sha512 of Caddy). A file this proof
    /// checks by its sha256 alone is hashed on the way and never written,
    /// and a MySQL or PostgreSQL archive is deleted after its checks.
    fn keeps_file(self) -> bool {
        match self {
            Proof::SumIfPublished => true,
            Proof::TheseusSha256 | Proof::OracleSignature | Proof::OurLibaio | Proof::OurValkey => {
                false
            }
        }
    }
}

/// Whether `url` is the recipe's own archive `template`, or the file of the
/// same name in another folder of Oracle's (a version that is not current
/// any more moves from `Downloads/` to `archives/`). The name carries the
/// version and the platform, so this never accepts another archive.
fn is_recipes_archive(url: &str, template: Option<&str>) -> bool {
    let Some(template) = template else {
        return false;
    };
    if url == template {
        return true;
    }
    let (Some((dir, name)), Some((template_dir, template_name))) =
        (url.rsplit_once('/'), template.rsplit_once('/'))
    else {
        return false;
    };
    name == template_name && MYSQL_FOLDERS.contains(&dir) && MYSQL_FOLDERS.contains(&template_dir)
}

/// The programs behind Oracle's proof: `oracle::System` in the build, a
/// stub in tests.
pub(super) trait OracleCheck {
    /// Checks that `signature`, the `.asc`, is Oracle's good signature on
    /// `file`, by the pinned key.
    fn signature(&self, signature: &Path, file: &Path) -> Result<()>;
    /// The MD5 of `file`.
    fn md5(&self, file: &Path) -> Result<[u8; 16]>;
    /// The name of the first entry of the archive `file`, of the `kind` its
    /// name gives, as it is written there (`mysql-8.4.13-.../bin/mysqld`).
    fn first_entry(&self, file: &Path, kind: ArchiveKind) -> Result<String>;
}

/// The two kinds of Oracle's MySQL archives: a tar archive (`.tar.gz`,
/// `.tar.xz`) for macOS and Linux, a `.zip` for Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ArchiveKind {
    Tar,
    Zip,
}

/// The top folder of a MySQL archive called `name`: the name without its
/// `.tar.gz`, `.tar.xz`, or `.zip`, and the kind of archive. Oracle's
/// signature does not bind the archive to its name, so a hijacked CDN could
/// serve an older signed archive under a new name: its top folder would
/// still say the old version.
fn top_folder_of(name: &str) -> Result<(&str, ArchiveKind)> {
    let tar = name
        .strip_suffix(".tar.gz")
        .or_else(|| name.strip_suffix(".tar.xz"));
    if let Some(top) = tar {
        return Ok((top, ArchiveKind::Tar));
    }
    let top = name
        .strip_suffix(".zip")
        .with_context(|| format!("{name} is not a .tar.gz, .tar.xz, or .zip archive"))?;
    Ok((top, ArchiveKind::Zip))
}

/// A file whose proof passed: what the manifest records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Checked {
    pub url: String,
    pub sha256: [u8; 32],
    pub size: u64,
}

/// Oracle's whole proof for the MySQL archive at `url`
/// ([`Proof::OracleSignature`]), with `url` as the recipe's own URL:
///
/// 1. `url` is an Oracle MySQL 8.4 archive, checked before any download;
/// 2. its `.md5` and its signature `.asc` are fetched first, so without
///    either nothing large is downloaded;
/// 3. the archive is downloaded to `work` and hashed on the way;
/// 4. the signature is good and by the pinned key, then its MD5 matches,
///    then its sha256 is still the one hashed on the way, so all three are
///    of the same bytes;
/// 5. the archive's top folder is the one its file name gives, so a signed
///    archive of another version is not accepted under this version's name.
///
/// The archive and its `.asc` are deleted after, whatever the result: they
/// are 80 to 834 MB.
///
/// # Errors
///
/// The first step that fails, with the URL.
pub(super) async fn check_oracle(
    fetch: &impl Fetch,
    url: &str,
    work: &Path,
    check: &impl OracleCheck,
) -> Result<Checked> {
    let sums = sum_file_for(url);
    // Any file of Oracle's two folders passes here: this function does not
    // know the recipe's URL. `build` checks the URL against the recipe's
    // before it calls this; the `check-mysql` command is for trying one file.
    Proof::OracleSignature
        .allows(url, Some(url), sums.as_ref())
        .with_context(|| url.to_owned())?;
    let sums = sums.with_context(|| format!("{url} has no Oracle `.md5`"))?;
    let md5_text = fetch_bytes(fetch, &sums.url, 4096)
        .await
        .with_context(|| format!("{url}: Oracle's md5"))?;
    let md5_text =
        String::from_utf8(md5_text).with_context(|| format!("{} is not text", sums.url))?;
    let expected = sums.find(&md5_text).with_context(|| sums.url.clone())?;
    let signature_url = format!("{url}.asc");
    let signature = fetch_bytes(fetch, &signature_url, 64 * 1024)
        .await
        .with_context(|| {
            format!("{url}: Oracle's signature, without which an MD5 is never accepted")
        })?;

    fs::create_dir_all(work).with_context(|| format!("cannot create {}", work.display()))?;
    // Fixed names, so a run that was killed leaves nothing the next one
    // does not overwrite or delete.
    let path = work.join("archive");
    let asc = work.join("archive.asc");
    let remove = || {
        for leftover in [&path, &asc] {
            if let Err(err) = fs::remove_file(leftover)
                && err.kind() != io::ErrorKind::NotFound
            {
                println!("    warning: cannot delete {}: {err}", leftover.display());
            }
        }
    };
    remove();
    let checked = async {
        fs::write(&asc, &signature).with_context(|| format!("cannot write {}", asc.display()))?;
        let file = temp_download(fetch, &[url.to_owned()], &path).await?;
        check
            .signature(&asc, &path)
            .context("Oracle's signature check failed")?;
        let md5 = check.md5(&path)?;
        ensure!(
            md5.as_slice() == expected.as_slice(),
            "md5 is {}, but {} says {}",
            hex(&md5),
            sums.url,
            hex(&expected)
        );
        let (now, _) = hash_file(&path)?;
        ensure!(
            now == file.sha256,
            "{} changed on disk during its checks",
            path.display()
        );
        let name = url.rsplit_once('/').map_or(url, |(_, name)| name);
        let (want, kind) = top_folder_of(name)?;
        let first = check.first_entry(&path, kind)?;
        let top = first.split('/').next().unwrap_or_default();
        ensure!(
            top == want,
            "the archive's first entry is in {top:?}, not in {want:?}, the folder its name gives"
        );
        Ok(Checked {
            url: file.url,
            sha256: file.sha256,
            size: file.size,
        })
    }
    .await;
    remove();
    checked.with_context(|| url.to_owned())
}

/// GitHub's attestation for the theseus-rs archive at `url`, as `build` runs
/// it for PostgreSQL (`attest`): the archive is downloaded to `work` and
/// hashed on the way, `gh` verifies it, the JSON is read again, and the
/// archive is deleted. The `.sha256` is not compared here: `build` does.
///
/// # Errors
///
/// `url` is not a theseus-rs release archive, or the first step that fails.
pub(super) async fn check_attested(
    fetch: &impl Fetch,
    url: &str,
    work: &Path,
    inspect: &impl Inspect,
) -> Result<Checked> {
    let sums = sum_file_for(url)
        .filter(|sums| sums.source == Source::Theseus)
        .with_context(|| format!("{url} is not a theseus-rs release archive"))?;
    let (dir, name) = url
        .rsplit_once('/')
        .with_context(|| format!("{url} has no file name"))?;
    let version = dir
        .rsplit_once('/')
        .and_then(|(_, tag)| Version::parse(tag).ok())
        .with_context(|| format!("{url} has no version in its folder"))?;
    fs::create_dir_all(work).with_context(|| format!("cannot create {}", work.display()))?;
    let path = work.join("archive");
    let _ = fs::remove_file(&path);
    let _scratch = RemoveOnDrop(path.clone());
    let file = temp_download(fetch, &[url.to_owned()], &path).await?;
    let json = inspect
        .attestation(&path, &version)
        .with_context(|| format!("{}: GitHub attestation", sums.name))?;
    attest::check_result(&json, name, &file.sha256, &version)
        .with_context(|| format!("{}: GitHub attestation", sums.name))?;
    Ok(Checked {
        url: file.url,
        sha256: file.sha256,
        size: file.size,
    })
}

/// The result: the manifest, one line per file for the report, and warnings.
pub(super) struct Built {
    pub manifest: ManifestJson,
    pub rows: Vec<String>,
    pub warnings: Vec<String>,
}

/// The new serial must be greater than the published one.
pub(super) fn check_serial(serial: u64, published: Option<u64>) -> Result<()> {
    match published {
        Some(old) => ensure!(
            serial > old,
            "serial {serial} in pins.toml must be greater than the published serial {old}"
        ),
        None => ensure!(serial >= MIN_SERIAL, "serial must be {MIN_SERIAL} or more"),
    }
    Ok(())
}

/// Builds the manifest. The files that a later check reads are saved under
/// `cache` (see `Proof::keeps_file`); the others are only hashed.
pub(super) async fn build(
    fetch: &impl Fetch,
    pins: &Pins,
    cache: &Path,
    inspect: &(impl Inspect + OracleCheck),
) -> Result<Built> {
    let mut manifest = ManifestJson {
        schema: SCHEMA_VERSION,
        serial: pins.serial,
        runtimes: RuntimesJson::default(),
    };
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut sum_files: BTreeMap<String, String> = BTreeMap::new();

    for runtime in RUNTIMES {
        let recipe = recipes::builtin(runtime)?;
        for version in pins.versions(runtime) {
            let mut platforms = BTreeMap::new();
            for (platform, platform_recipe) in recipe.platforms() {
                if let Some(reason) = pins.skip_reason(runtime, version, platform) {
                    rows.push(format!(
                        "{} {version} {platform}: skipped ({reason})",
                        runtime.dir_name()
                    ));
                    continue;
                }
                let mut sources = Vec::new();
                for artifact in platform_recipe.artifacts() {
                    let target = Target {
                        version,
                        platform,
                        recipe: platform_recipe,
                        artifact,
                    };
                    let what = format!(
                        "{} {version} {platform} {}",
                        runtime.dir_name(),
                        artifact.name
                    );
                    let urls = pins.urls(runtime, version, platform, &artifact.name)?;
                    let template = recipe.artifact_url(platform, &artifact.name, version);
                    let proof = Proof::of(runtime, &artifact.name);
                    for url in &urls {
                        let sums = sum_file_for(url);
                        proof
                            .allows(url, template.as_deref(), sums.as_ref())
                            .with_context(|| format!("{what}: {url}"))?;
                    }
                    let file = if proof == Proof::OracleSignature {
                        // One URL: Oracle's proof has no mirror.
                        let [url] = urls.as_slice() else {
                            bail!("{what}: an Oracle archive has one URL, not {}", urls.len());
                        };
                        let work = cache.join("oracle-work");
                        let checked = check_oracle(fetch, url, &work, inspect)
                            .await
                            .with_context(|| what.clone())?;
                        Ok(Downloaded {
                            path: None,
                            url: checked.url,
                            sha256: checked.sha256,
                            size: checked.size,
                            _scratch: None,
                        })
                    } else if proof == Proof::TheseusSha256 {
                        // `gh attestation verify` reads a file. It is 60 MB:
                        // deleted at the end of this artifact, whatever the
                        // result.
                        let work = cache.join("theseus-work");
                        fs::create_dir_all(&work)
                            .with_context(|| format!("cannot create {}", work.display()))?;
                        let path = work.join("archive");
                        let _ = fs::remove_file(&path);
                        // Dropped with the answer, or with the error.
                        let scratch = RemoveOnDrop(path.clone());
                        temp_download(fetch, &urls, &path)
                            .await
                            .map(|file| Downloaded {
                                _scratch: Some(scratch),
                                ..file
                            })
                    } else if proof.keeps_file() {
                        let dir = cache
                            .join(runtime.dir_name())
                            .join(version.to_string())
                            .join(platform.as_str());
                        cached_download(fetch, &urls, &dir, &artifact.name).await
                    } else {
                        hashed_download(fetch, &urls).await
                    }
                    .with_context(|| what.clone())?;

                    let mut notes = Vec::new();
                    let sums = sum_file_for(&file.url);
                    // Checked above, as one of `urls`; again on the URL that
                    // answered, so this never rests on that alone.
                    proof
                        .allows(&file.url, template.as_deref(), sums.as_ref())
                        .with_context(|| format!("{what}: {}", file.url))?;
                    match sums {
                        // `check_oracle` compared the MD5 and checked the
                        // signature on the bytes it hashed.
                        Some(_) if proof == Proof::OracleSignature => {
                            notes.push("Oracle signature and md5 verified");
                        }
                        Some(sums) => {
                            let sum_url = &sums.url;
                            if !sum_files.contains_key(sum_url) {
                                let bytes = fetch_bytes(fetch, sum_url, 1024 * 1024)
                                    .await
                                    .with_context(|| format!("{what}: upstream sums"))?;
                                let text = String::from_utf8(bytes)
                                    .with_context(|| format!("{sum_url} is not text"))?;
                                sum_files.insert(sum_url.clone(), text);
                            }
                            let text = sum_files.get(sum_url).map_or("", String::as_str);
                            let expected = sums
                                .find(text)
                                .with_context(|| format!("{what}: {sum_url}"))?;
                            // The manifest keeps our sha256; the upstream hash
                            // is only compared.
                            let (actual, note) = match sums.algorithm {
                                Algorithm::Sha256 => {
                                    (file.sha256.to_vec(), "upstream sha256 matches")
                                }
                                Algorithm::Sha512 => {
                                    let sha512 = sha512_file(file.kept()?, &file.sha256)?;
                                    (sha512.to_vec(), "upstream sha512 matches")
                                }
                                // `Proof::allows` refused it before the
                                // download, except for `OracleSignature`,
                                // whose files only `check_oracle` proves.
                                Algorithm::Md5 => {
                                    bail!("{what}: an MD5 is never accepted alone")
                                }
                            };
                            let algorithm = sums.algorithm.name();
                            ensure!(
                                expected == actual,
                                "{what}: {algorithm} is {}, but {sum_url} says {}",
                                hex(&actual),
                                hex(&expected)
                            );
                            notes.push(note);
                            if proof == Proof::TheseusSha256 {
                                let json = inspect
                                    .attestation(file.kept()?, version)
                                    .with_context(|| format!("{what}: GitHub attestation"))?;
                                let name = file.url.rsplit_once('/').map_or(&*file.url, |(_, n)| n);
                                attest::check_result(&json, name, &file.sha256, version)
                                    .with_context(|| format!("{what}: GitHub attestation"))?;
                                notes.push("GitHub attestation verified");
                            }
                        }
                        None => {
                            warnings.push(format!(
                                "{what}: {} publishes no sha256, so only our own sha256 is recorded",
                                file.url
                            ));
                            notes.push("no upstream sum");
                        }
                    }
                    if runtime == Runtime::Php && is_spc_platform(platform) {
                        let found = inspect
                            .php_build(&target, file.kept()?)
                            .with_context(|| format!("{what}: PHP check failed"))?;
                        warnings.extend(found);
                        notes.push("PHP checks passed");
                    }
                    rows.push(format!(
                        "{what}: {} bytes, sha256 {} ({})",
                        file.size,
                        hex(&file.sha256),
                        notes.join("; ")
                    ));
                    sources.push(SourceJson {
                        name: artifact.name.clone(),
                        urls,
                        sha256: hex(&file.sha256),
                        size: file.size,
                    });
                }
                platforms.insert(platform.as_str().to_owned(), sources);
            }
            manifest.runtimes.get_mut(runtime).push(ReleaseJson {
                version: version.to_string(),
                platforms,
            });
        }
    }
    Ok(Built {
        manifest,
        rows,
        warnings,
    })
}

/// Deletes a file when it goes out of scope: the large archives that are only
/// checked, never kept.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.0)
            && err.kind() != io::ErrorKind::NotFound
        {
            println!("    warning: cannot delete {}: {err}", self.0.display());
        }
    }
}

/// A downloaded file, and the URL that served it.
struct Downloaded {
    /// Where it is kept, or `None` when it was only hashed.
    path: Option<PathBuf>,
    url: String,
    sha256: [u8; 32],
    size: u64,
    /// Never read: deletes the file when this goes out of scope.
    _scratch: Option<RemoveOnDrop>,
}

impl Downloaded {
    /// The file on disk, for a check that reads it.
    fn kept(&self) -> Result<&Path> {
        self.path
            .as_deref()
            .with_context(|| format!("{} was hashed on the way and not kept", self.url))
    }
}

/// Saves the first URL that answers to `dir/<name>`, with `dir/<name>.sha256`
/// (`<hex> <url>`) next to it. A cached file is reused only if its sha256
/// still matches the saved one.
async fn cached_download(
    fetch: &impl Fetch,
    urls: &[String],
    dir: &Path,
    name: &str,
) -> Result<Downloaded> {
    ensure!(
        !name.is_empty()
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-')),
        "artifact name {name:?} is not [a-z0-9._-]+"
    );
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join(name);
    let record = dir.join(format!("{name}.sha256"));

    if let (Ok(saved), true) = (fs::read_to_string(&record), path.is_file()) {
        let (saved_hex, url) = saved.trim_end().split_once(' ').unwrap_or_default();
        let (sha256, size) = hash_file(&path)?;
        if parse_hex(saved_hex) == Some(sha256) && urls.iter().any(|u| u == url) {
            return Ok(Downloaded {
                path: Some(path),
                url: url.to_owned(),
                sha256,
                size,
                _scratch: None,
            });
        }
    }

    let part = dir.join(format!("{name}.part"));
    let result = first_answer(fetch, urls, |url| {
        let _ = fs::remove_file(&part);
        let file =
            File::create(&part).with_context(|| format!("cannot create {}", part.display()))?;
        Ok(HashSink::new(url, Some((file, part.clone()))))
    })
    .await;
    let (url, sink) = match result {
        Ok(found) => found,
        Err(err) => {
            let _ = fs::remove_file(&part);
            return Err(err);
        }
    };
    let HashSink {
        file, hasher, size, ..
    } = sink;
    if let Some((mut file, _)) = file {
        file.flush()
            .with_context(|| format!("cannot write {}", part.display()))?;
    }
    let sha256: [u8; 32] = hasher.finalize().into();
    fs::rename(&part, &path).with_context(|| format!("cannot move {}", part.display()))?;
    fs::write(&record, format!("{} {url}\n", hex(&sha256)))?;
    Ok(Downloaded {
        path: Some(path),
        url,
        sha256,
        size,
        _scratch: None,
    })
}

/// Hashes the body of the first URL that answers, and keeps nothing: for
/// large files that only their sha256 proves.
async fn hashed_download(fetch: &impl Fetch, urls: &[String]) -> Result<Downloaded> {
    let (url, sink) = first_answer(fetch, urls, |url| Ok(HashSink::new(url, None))).await?;
    Ok(Downloaded {
        path: None,
        url,
        sha256: sink.hasher.finalize().into(),
        size: sink.size,
        _scratch: None,
    })
}

/// Downloads the first URL that answers to `path`, hashing on the way, for
/// checks that read the file before the caller deletes it. A URL that fails
/// leaves a partial file, which the next one overwrites.
async fn temp_download(fetch: &impl Fetch, urls: &[String], path: &Path) -> Result<Downloaded> {
    let (url, sink) = first_answer(fetch, urls, |url| {
        let file =
            File::create(path).with_context(|| format!("cannot create {}", path.display()))?;
        Ok(HashSink::new(url, Some((file, path.to_path_buf()))))
    })
    .await?;
    let HashSink {
        file, hasher, size, ..
    } = sink;
    if let Some((mut file, _)) = file {
        file.flush()
            .with_context(|| format!("cannot write {}", path.display()))?;
    }
    Ok(Downloaded {
        path: Some(path.to_path_buf()),
        url,
        sha256: hasher.finalize().into(),
        size,
        _scratch: None,
    })
}

/// Tries `urls` in order, each into a new sink from `sink_for`, and returns
/// the first that answers in full, with its sink. An HTTP error status or a
/// network error tries the next URL; any other error stops.
async fn first_answer(
    fetch: &impl Fetch,
    urls: &[String],
    mut sink_for: impl FnMut(&str) -> Result<HashSink>,
) -> Result<(String, HashSink)> {
    let mut last = None;
    for url in urls {
        let mut sink = sink_for(url)?;
        println!("  downloading {url}");
        match fetch.get(url, &mut sink).await {
            Ok(()) => {
                ensure!(sink.size > 0, "{url} is empty");
                return Ok((url.clone(), sink));
            }
            Err(err @ (FetchError::HttpStatus { .. } | FetchError::Network { .. })) => {
                println!("    {err}; trying the next URL");
                last = Some(err);
            }
            Err(err) => return Err(err.into()),
        }
    }
    match last {
        Some(err) => Err(anyhow::Error::from(err).context("no URL worked")),
        None => bail!("no URL"),
    }
}

/// Hashes the body on the way, and writes it to a file when it has one.
struct HashSink {
    /// The file, and its path for errors.
    file: Option<(File, PathBuf)>,
    url: String,
    hasher: Sha256,
    size: u64,
}

impl HashSink {
    fn new(url: &str, file: Option<(File, PathBuf)>) -> Self {
        Self {
            file,
            url: url.to_owned(),
            hasher: Sha256::new(),
            size: 0,
        }
    }
}

impl ChunkSink for HashSink {
    fn accept(&mut self, chunk: &[u8]) -> Result<(), FetchError> {
        self.size = self.size.saturating_add(chunk.len() as u64);
        if self.size > MAX_ARTIFACT_SIZE {
            return Err(FetchError::TooLarge {
                url: self.url.clone(),
                max: MAX_ARTIFACT_SIZE,
            });
        }
        self.hasher.update(chunk);
        match &mut self.file {
            Some((file, path)) => file.write_all(chunk).map_err(|source| FetchError::Io {
                path: path.clone(),
                source,
            }),
            None => Ok(()),
        }
    }
}

/// The sha256 and size of a file.
fn hash_file(path: &Path) -> Result<([u8; 32], u64)> {
    let mut hasher = Sha256::new();
    let size = read_chunks(path, |chunk| hasher.update(chunk))?;
    Ok((hasher.finalize().into(), size))
}

/// The sha512 of a file, for sources that publish only sha512 (Caddy). The
/// same pass checks that the file still has the `sha256` the manifest will
/// record, so both hashes are of the same bytes.
fn sha512_file(path: &Path, sha256: &[u8; 32]) -> Result<[u8; 64]> {
    let mut sha512 = Sha512::new();
    let mut check = Sha256::new();
    read_chunks(path, |chunk| {
        sha512.update(chunk);
        check.update(chunk);
    })?;
    let now: [u8; 32] = check.finalize().into();
    ensure!(
        &now == sha256,
        "{} changed on disk while the manifest was built",
        path.display()
    );
    Ok(sha512.finalize().into())
}

/// Gives each chunk of the file to `each`, and returns the size.
fn read_chunks(path: &Path, mut each: impl FnMut(&[u8])) -> Result<u64> {
    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut buffer = vec![0u8; 1 << 16];
    let mut size = 0u64;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err.into()),
        };
        each(&buffer[..read]);
        size += read as u64;
    }
    Ok(size)
}

#[cfg(test)]
mod tests;
