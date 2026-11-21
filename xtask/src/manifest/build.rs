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

use super::pins::{Pins, RUNTIMES};
use super::upstream::{
    Algorithm, Layout, MYSQL_FOLDERS, Source, SumFile, hex, parse_hex, sum_file_for,
};

/// The JSON that `marsa_core::installer::manifest` reads (M2-02). Field
/// order is the output order, and maps are `BTreeMap`, so the same inputs
/// give the same bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestJson {
    pub schema: u64,
    pub serial: u64,
    pub runtimes: RuntimesJson,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RuntimesJson {
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
}

impl RuntimesJson {
    pub fn get(&self, runtime: Runtime) -> &Vec<ReleaseJson> {
        match runtime {
            Runtime::Php => &self.php,
            Runtime::Node => &self.node,
            Runtime::Composer => &self.composer,
            Runtime::Caddy => &self.caddy,
            Runtime::Postgres => &self.postgres,
            Runtime::Mysql => &self.mysql,
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseJson {
    pub version: String,
    pub platforms: BTreeMap<String, Vec<SourceJson>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceJson {
    pub name: String,
    pub urls: Vec<String>,
    /// 64 lowercase hex characters.
    pub sha256: String,
    pub size: u64,
}

/// The manifest as written to disk: pretty JSON and a final newline.
pub fn to_json(manifest: &ManifestJson) -> Result<String> {
    Ok(serde_json::to_string_pretty(manifest)? + "\n")
}

/// One file being built, for the checks and for messages.
pub struct Target<'a> {
    pub version: &'a Version,
    pub platform: Platform,
    pub recipe: &'a PlatformRecipe,
    pub artifact: &'a Artifact,
}

/// Checks that need real programs. Tests pass a stub.
pub trait Inspect {
    /// Checks one of our static-php-cli PHP archives (macOS or Linux).
    /// Returns warnings.
    fn php_build(&self, target: &Target<'_>, file: &Path) -> Result<Vec<String>>;
}

/// Platforms where PHP is our own static-php-cli build (ADR 0012).
pub fn is_spc_platform(platform: Platform) -> bool {
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
    /// source, of the recipe's own URL. PostgreSQL.
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
}

impl Proof {
    /// An exhaustive `match`, so each new runtime has to choose. MySQL's
    /// `archive` is Oracle's; its other files are our own builds.
    fn of(runtime: Runtime, artifact: &str) -> Proof {
        match runtime {
            Runtime::Php | Runtime::Node | Runtime::Composer | Runtime::Caddy => {
                Proof::SumIfPublished
            }
            Runtime::Postgres => Proof::TheseusSha256,
            Runtime::Mysql if artifact == "archive" => Proof::OracleSignature,
            Runtime::Mysql => Proof::OurLibaio,
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
        }
    }

    /// Whether the file is kept in the cache after the build: only when a
    /// later check reads it (PHP, the sha512 of Caddy). A file this proof
    /// checks by its sha256 alone is hashed on the way and never written,
    /// and a MySQL archive is deleted after its checks.
    fn keeps_file(self) -> bool {
        match self {
            Proof::SumIfPublished => true,
            Proof::TheseusSha256 | Proof::OracleSignature | Proof::OurLibaio => false,
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
pub trait OracleCheck {
    /// Checks that `signature`, the `.asc`, is Oracle's good signature on
    /// `file`, by the pinned key.
    fn signature(&self, signature: &Path, file: &Path) -> Result<()>;
    /// The MD5 of `file`.
    fn md5(&self, file: &Path) -> Result<[u8; 16]>;
    /// The name of the first entry of the tar archive `file`, as it is
    /// written there (`mysql-8.4.13-.../bin/mysqld`).
    fn first_entry(&self, file: &Path) -> Result<String>;
}

/// The top folder of a MySQL archive called `name`: the name without its
/// `.tar.gz` or `.tar.xz`. Oracle's signature does not bind the archive to
/// its name, so a hijacked CDN could serve an older signed archive under a
/// new name: its top folder would still say the old version.
fn top_folder_of(name: &str) -> Result<&str> {
    name.strip_suffix(".tar.gz")
        .or_else(|| name.strip_suffix(".tar.xz"))
        .with_context(|| format!("{name} is not a .tar.gz or .tar.xz archive"))
}

/// A file whose proof passed: what the manifest records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
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
pub async fn check_oracle(
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
        let want = top_folder_of(name)?;
        let first = check.first_entry(&path)?;
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

/// The result: the manifest, one line per file for the report, and warnings.
pub struct Built {
    pub manifest: ManifestJson,
    pub rows: Vec<String>,
    pub warnings: Vec<String>,
}

/// The new serial must be greater than the published one.
pub fn check_serial(serial: u64, published: Option<u64>) -> Result<()> {
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
pub async fn build(
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

/// A downloaded file, and the URL that served it.
struct Downloaded {
    /// Where it is kept, or `None` when it was only hashed.
    path: Option<PathBuf>,
    url: String,
    sha256: [u8; 32],
    size: u64,
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
mod tests {
    use marsa_core::installer::fetch::testing::{MemoryFetch, Reply};

    use super::super::verify::check_json;
    use super::*;

    const PINS: &str = r#"
serial = 2
spc_line = "2.8"
[php]
versions = ["8.4.23"]
[node]
versions = ["22.23.4"]
[composer]
versions = ["2.11.1"]
[caddy]
versions = ["2.11.8"]
[postgres]
versions = ["18.7.0"]
[[override]]
runtime = "php"
version = "8.4.23"
platform = "windows-x64"
artifact = "zip"
urls = ["https://windows.php.net/downloads/releases/archives/php-8.4.23-nts-Win32-vs17-x64.zip"]
"#;

    /// Counts the PHP checks it was asked to run.
    #[derive(Default)]
    struct Stub(std::sync::Mutex<Vec<String>>);

    /// No Oracle archive is in these pins: any call is a bug of the test.
    impl OracleCheck for Stub {
        fn signature(&self, _signature: &Path, _file: &Path) -> Result<()> {
            bail!("an Oracle check was not expected")
        }

        fn md5(&self, _file: &Path) -> Result<[u8; 16]> {
            bail!("an Oracle check was not expected")
        }

        fn first_entry(&self, _file: &Path) -> Result<String> {
            bail!("an Oracle check was not expected")
        }
    }

    impl Inspect for Stub {
        fn php_build(&self, target: &Target<'_>, _file: &Path) -> Result<Vec<String>> {
            let name = format!("{} {}", target.platform, target.artifact.name);
            if let Ok(mut seen) = self.0.lock() {
                seen.push(name);
            }
            Ok(vec![])
        }
    }

    fn sha(body: &[u8]) -> String {
        hex(Sha256::digest(body).as_slice())
    }

    /// The hash a sum file of `algorithm` lists for `body`.
    fn upstream(algorithm: Algorithm, body: &[u8]) -> String {
        match algorithm {
            Algorithm::Sha256 => sha(body),
            Algorithm::Sha512 => hex(Sha512::digest(body).as_slice()),
            // Never read: the build refuses an MD5 before the download.
            Algorithm::Md5 => "0".repeat(32),
        }
    }

    /// Every URL of the pins, with a fake body (the URL itself) and sum files
    /// that agree, except for the file named in `bad`.
    fn fetcher(pins: &Pins, bad: Option<&str>) -> MemoryFetch {
        let mut fetch = MemoryFetch::new();
        let mut sums: BTreeMap<String, String> = BTreeMap::new();
        for runtime in RUNTIMES {
            let recipe = recipes::builtin(runtime).expect("recipe");
            for version in pins.versions(runtime) {
                for (platform, platform_recipe) in recipe.platforms() {
                    for artifact in platform_recipe.artifacts() {
                        let urls = pins
                            .urls(runtime, version, platform, &artifact.name)
                            .expect("urls");
                        let url = &urls[0];
                        let body = url.as_bytes().to_vec();
                        if let Some(sum) = sum_file_for(url) {
                            let hash = if bad == Some(sum.name.as_str()) {
                                upstream(sum.algorithm, b"something else")
                            } else {
                                upstream(sum.algorithm, &body)
                            };
                            sums.entry(sum.url)
                                .or_default()
                                .push_str(&format!("{hash}  {}\r\n", sum.name));
                        }
                        fetch = fetch.with(url, Reply::Chunks(vec![body]));
                    }
                }
            }
        }
        for (url, text) in sums {
            fetch = fetch.with(&url, Reply::Chunks(vec![text.into_bytes()]));
        }
        fetch
    }

    /// The pins of [`PINS`] plus MySQL: 8.4.13 where Oracle has it, and
    /// 8.4.11 for the Intel Mac only, from Oracle's `archives/` folder.
    fn mysql_pins() -> String {
        let skip = |version: &str, platform: &str| {
            format!(
                "[[skip]]\nruntime = \"mysql\"\nversion = \"{version}\"\nplatform = \"{platform}\"\n\
                 reason = \"no build\"\n"
            )
        };
        format!(
            "{PINS}[mysql]\nversions = [\"8.4.13\", \"8.4.11\"]\n{}{}{}{}{}{}\
             [[override]]\nruntime = \"mysql\"\nversion = \"8.4.11\"\nplatform = \"darwin-x64\"\n\
             artifact = \"archive\"\nurls = [\"https://cdn.mysql.com/archives/mysql-8.4/\
             mysql-8.4.11-macos15-x86_64.tar.gz\"]\n",
            skip("8.4.13", "darwin-x64"),
            skip("8.4.13", "windows-x64"),
            skip("8.4.11", "darwin-arm64"),
            skip("8.4.11", "linux-x64"),
            skip("8.4.11", "linux-arm64"),
            skip("8.4.11", "windows-x64"),
        )
    }

    /// Oracle's checks for the fake archives of [`fetcher`], whose body is
    /// their own URL; the first entry is the top folder their name gives.
    struct OracleOk;

    impl OracleCheck for OracleOk {
        fn signature(&self, _signature: &Path, _file: &Path) -> Result<()> {
            Ok(())
        }

        fn md5(&self, _file: &Path) -> Result<[u8; 16]> {
            // The `.md5` that `fetcher` writes for an MD5 sum file.
            Ok([0; 16])
        }

        fn first_entry(&self, file: &Path) -> Result<String> {
            let url = fs::read_to_string(file)?;
            let name = url.rsplit_once('/').map_or(url.as_str(), |(_, name)| name);
            Ok(format!("{}/bin/mysqld", top_folder_of(name)?))
        }
    }

    struct MysqlStub;

    impl Inspect for MysqlStub {
        fn php_build(&self, _target: &Target<'_>, _file: &Path) -> Result<Vec<String>> {
            Ok(vec![])
        }
    }

    impl OracleCheck for MysqlStub {
        fn signature(&self, signature: &Path, file: &Path) -> Result<()> {
            OracleOk.signature(signature, file)
        }

        fn md5(&self, file: &Path) -> Result<[u8; 16]> {
            OracleOk.md5(file)
        }

        fn first_entry(&self, file: &Path) -> Result<String> {
            OracleOk.first_entry(file)
        }
    }

    /// `fetcher`, and the `.asc` of every Oracle archive.
    fn mysql_fetcher(pins: &Pins) -> MemoryFetch {
        let mut fetch = fetcher(pins, None);
        for version in pins.versions(Runtime::Mysql) {
            let recipe = recipes::builtin(Runtime::Mysql).expect("recipe");
            for (platform, _) in recipe.platforms() {
                let urls = pins
                    .urls(Runtime::Mysql, version, platform, "archive")
                    .expect("urls");
                fetch = fetch.with(
                    &format!("{}.asc", urls[0]),
                    Reply::Chunks(vec![b"signature".to_vec()]),
                );
            }
        }
        fetch
    }

    #[tokio::test]
    async fn mysql_is_built_with_oracles_proof_and_our_own_libraries() {
        let pins = Pins::parse(&mysql_pins()).expect("pins");
        let fetch = mysql_fetcher(&pins);
        let cache = tempfile::tempdir().expect("tempdir");
        let built = build(&fetch, &pins, cache.path(), &MysqlStub)
            .await
            .expect("build");
        let value = serde_json::to_value(&built.manifest).expect("json");
        let releases = value["runtimes"]["mysql"].as_array().expect("mysql");
        let platforms = |version: &str| -> Vec<(String, Vec<String>)> {
            let release = releases
                .iter()
                .find(|release| release["version"] == version)
                .expect("release");
            release["platforms"]
                .as_object()
                .expect("platforms")
                .iter()
                .map(|(platform, files)| {
                    let names = files
                        .as_array()
                        .expect("files")
                        .iter()
                        .map(|file| file["name"].as_str().expect("name").to_owned())
                        .collect();
                    (platform.clone(), names)
                })
                .collect()
        };
        let linux = vec!["archive".to_owned(), "libaio".into(), "libnuma".into()];
        assert_eq!(
            platforms("8.4.13"),
            [
                ("darwin-arm64".to_owned(), vec!["archive".to_owned()]),
                ("linux-arm64".to_owned(), linux.clone()),
                ("linux-x64".to_owned(), linux),
            ]
        );
        assert_eq!(
            platforms("8.4.11"),
            [("darwin-x64".to_owned(), vec!["archive".to_owned()])]
        );
        let intel = &releases[1]["platforms"]["darwin-x64"][0];
        assert_eq!(
            intel["urls"][0],
            "https://cdn.mysql.com/archives/mysql-8.4/mysql-8.4.11-macos15-x86_64.tar.gz"
        );
        let rows = built.rows.join("\n");
        assert!(rows.contains("Oracle signature and md5 verified"), "{rows}");
        assert!(rows.contains("upstream sha256 matches"), "{rows}");
        // Nothing of MySQL stays on disk: archives are hashed or deleted.
        assert!(!cache.path().join("mysql").exists());
        assert!(empty(&cache.path().join("oracle-work")));
        check_json(to_json(&built.manifest).expect("json").as_bytes()).expect("valid manifest");
    }

    #[tokio::test]
    async fn a_bad_oracle_check_or_library_sum_stops_the_mysql_build() {
        let pins = Pins::parse(&mysql_pins()).expect("pins");
        // A wrong top folder.
        struct WrongFolder;
        impl Inspect for WrongFolder {
            fn php_build(&self, _t: &Target<'_>, _f: &Path) -> Result<Vec<String>> {
                Ok(vec![])
            }
        }
        impl OracleCheck for WrongFolder {
            fn signature(&self, _s: &Path, _f: &Path) -> Result<()> {
                Ok(())
            }
            fn md5(&self, _f: &Path) -> Result<[u8; 16]> {
                Ok([0; 16])
            }
            fn first_entry(&self, _f: &Path) -> Result<String> {
                Ok("mysql-8.4.9-other/bin/mysqld".to_owned())
            }
        }
        let cache = tempfile::tempdir().expect("tempdir");
        let err = build(&mysql_fetcher(&pins), &pins, cache.path(), &WrongFolder)
            .await
            .err()
            .expect("wrong folder");
        assert!(format!("{err:#}").contains("first entry"), "{err:#}");

        // A libaio file that its release's sha256sums.txt does not list as
        // it is.
        let fetch = {
            let mut fetch = fetcher(&pins, Some("libaio-0.3.113-linux-x64.so.1"));
            for version in pins.versions(Runtime::Mysql) {
                let recipe = recipes::builtin(Runtime::Mysql).expect("recipe");
                for (platform, _) in recipe.platforms() {
                    let urls = pins
                        .urls(Runtime::Mysql, version, platform, "archive")
                        .expect("urls");
                    fetch = fetch.with(
                        &format!("{}.asc", urls[0]),
                        Reply::Chunks(vec![b"signature".to_vec()]),
                    );
                }
            }
            fetch
        };
        let cache = tempfile::tempdir().expect("tempdir");
        let err = build(&fetch, &pins, cache.path(), &MysqlStub)
            .await
            .err()
            .expect("bad libaio sum");
        assert!(format!("{err:#}").contains("libaio"), "{err:#}");
    }

    #[test]
    fn each_mysql_file_has_its_own_proof() {
        assert_eq!(Proof::of(Runtime::Mysql, "archive"), Proof::OracleSignature);
        for name in ["libaio", "libnuma"] {
            assert_eq!(Proof::of(Runtime::Mysql, name), Proof::OurLibaio);
        }
        let release = "https://github.com/XJOC/marsa/releases/download/libaio-0.3.113-r1/\
                       libaio-0.3.113-linux-x64.so.1";
        let sums = sum_file_for(release);
        Proof::OurLibaio
            .allows(release, Some(release), sums.as_ref())
            .expect("our release");
        // Not our release, an Oracle archive, or no sum file.
        for url in [
            "https://github.com/someone/else/releases/download/libaio-0.3.113-r1/libaio.so.1",
            MYSQL,
            "https://example.org/libaio.so.1",
        ] {
            let sums = sum_file_for(url);
            assert!(
                Proof::OurLibaio
                    .allows(url, Some(url), sums.as_ref())
                    .is_err(),
                "{url}"
            );
        }
        // Another file of our own releases: the other architecture, the other
        // library, or an older release.
        for other in [
            "https://github.com/XJOC/marsa/releases/download/libaio-0.3.113-r1/libaio-0.3.113-linux-arm64.so.1",
            "https://github.com/XJOC/marsa/releases/download/libaio-0.3.113-r1/libnuma-2.0.19-linux-x64.so.1",
            "https://github.com/XJOC/marsa/releases/download/libaio-0.3.100-r1/libaio-0.3.100-linux-x64.so.1",
        ] {
            let sums = sum_file_for(other);
            let err = Proof::OurLibaio
                .allows(other, Some(release), sums.as_ref())
                .expect_err("not the recipe's file");
            assert!(err.to_string().contains("only that file"), "{err}");
        }
        assert!(!Proof::OurLibaio.keeps_file());
    }

    /// An Oracle file has one URL: a second one is a mirror that its
    /// proof does not cover.
    #[tokio::test]
    async fn an_oracle_file_with_a_mirror_stops_the_build() {
        let text = format!(
            "{}\n[[override]]\nruntime = \"mysql\"\nversion = \"8.4.13\"\nplatform = \"darwin-arm64\"\n\
             artifact = \"archive\"\nurls = [\"https://cdn.mysql.com/Downloads/MySQL-8.4/\
             mysql-8.4.13-macos15-arm64.tar.gz\", \"https://cdn.mysql.com/archives/mysql-8.4/\
             mysql-8.4.13-macos15-arm64.tar.gz\"]\n",
            mysql_pins()
        );
        let pins = Pins::parse(&text).expect("pins");
        let cache = tempfile::tempdir().expect("tempdir");
        let err = build(&mysql_fetcher(&pins), &pins, cache.path(), &MysqlStub)
            .await
            .err()
            .expect("a mirror");
        assert!(format!("{err:#}").contains("has one URL, not 2"), "{err:#}");
    }

    #[test]
    fn an_oracle_archive_may_come_from_the_archives_folder_by_its_own_name() {
        let template =
            "https://cdn.mysql.com/Downloads/MySQL-8.4/mysql-8.4.11-macos15-x86_64.tar.gz";
        let archived =
            "https://cdn.mysql.com/archives/mysql-8.4/mysql-8.4.11-macos15-x86_64.tar.gz";
        assert!(is_recipes_archive(template, Some(template)));
        assert!(is_recipes_archive(archived, Some(template)));
        for (url, template) in [
            // Another file name: another version or platform.
            (
                "https://cdn.mysql.com/archives/mysql-8.4/mysql-8.4.9-macos15-x86_64.tar.gz",
                Some(template),
            ),
            // Another host or folder with the same name.
            (
                "https://example.org/archives/mysql-8.4/mysql-8.4.11-macos15-x86_64.tar.gz",
                Some(template),
            ),
            (
                "https://cdn.mysql.com/archives/mysql-8.0/mysql-8.4.11-macos15-x86_64.tar.gz",
                Some(template),
            ),
            (archived, None),
        ] {
            assert!(!is_recipes_archive(url, template), "{url}");
        }
    }

    #[tokio::test]
    async fn builds_the_expected_json_twice_the_same() {
        let pins = Pins::parse(PINS).expect("pins");
        let fetch = fetcher(&pins, None);
        let stub = Stub::default();

        let cache = tempfile::tempdir().expect("tempdir");
        let first = build(&fetch, &pins, cache.path(), &stub)
            .await
            .expect("build");
        let json = to_json(&first.manifest).expect("json");

        // Second run, fresh cache: the same bytes.
        let cache2 = tempfile::tempdir().expect("tempdir");
        let second = build(&fetch, &pins, cache2.path(), &stub)
            .await
            .expect("build");
        assert_eq!(json, to_json(&second.manifest).expect("json"));
        // Third run, same cache: reused, still the same bytes.
        let requests = fetch.requests().len();
        let third = build(&fetch, &pins, cache.path(), &stub)
            .await
            .expect("build");
        assert_eq!(json, to_json(&third.manifest).expect("json"));
        let new_requests = fetch.requests().len() - requests;
        assert_eq!(
            new_requests, 14,
            "only the php, node, composer and caddy sum files, and the five \
             postgres archives and their .sha256 files, are fetched again"
        );
        // The PostgreSQL archives were hashed on the way, never written.
        assert!(cache.path().join("php").is_dir());
        assert!(!cache.path().join("postgres").exists());

        // The local checks (the same rules as M2-02) accept it.
        let checked = check_json(json.as_bytes()).expect("valid manifest");
        assert_eq!(checked, first.manifest);

        let value: serde_json::Value = serde_json::from_str(&json).expect("json");
        assert_eq!(value["schema"], 1);
        assert_eq!(value["serial"], 2);
        let php = &value["runtimes"]["php"][0];
        assert_eq!(php["version"], "8.4.23");
        let keys: Vec<&String> = php["platforms"]
            .as_object()
            .expect("object")
            .keys()
            .collect();
        assert_eq!(
            keys,
            [
                "darwin-arm64",
                "darwin-x64",
                "linux-arm64",
                "linux-x64",
                "windows-x64"
            ]
        );
        let mac = &php["platforms"]["darwin-arm64"];
        assert_eq!(mac.as_array().map(Vec::len), Some(1), "one archive");
        let url = "https://github.com/XJOC/marsa/releases/download/php-8.4.23-r1/php-8.4.23-darwin-arm64.tar.gz";
        assert_eq!(mac[0]["name"], "php");
        assert_eq!(mac[0]["urls"][0], url);
        assert_eq!(mac[0]["sha256"], sha(url.as_bytes()));
        assert_eq!(mac[0]["size"], url.len());
        let windows = &php["platforms"]["windows-x64"][0];
        assert_eq!(windows["urls"].as_array().map(Vec::len), Some(1));
        let node = &value["runtimes"]["node"][0]["platforms"];
        assert_eq!(node.as_object().map(|o| o.len()), Some(5));
        let composer = &value["runtimes"]["composer"][0]["platforms"]["any"][0];
        assert_eq!(composer["name"], "phar");
        let caddy = &value["runtimes"]["caddy"][0];
        assert_eq!(caddy["version"], "2.11.8");
        assert_eq!(caddy["platforms"].as_object().map(|o| o.len()), Some(5));
        let caddy_mac = &caddy["platforms"]["darwin-arm64"][0];
        let url = "https://github.com/caddyserver/caddy/releases/download/v2.11.8/caddy_2.11.8_mac_arm64.tar.gz";
        assert_eq!(caddy_mac["name"], "archive");
        assert_eq!(caddy_mac["urls"][0], url);
        // The manifest stores our sha256, not the upstream sha512.
        assert_eq!(caddy_mac["sha256"], sha(url.as_bytes()));
        let caddy_row = first
            .rows
            .iter()
            .find(|row| row.starts_with("caddy 2.11.8 darwin-arm64 archive:"))
            .expect("a caddy row");
        assert!(
            caddy_row.ends_with("(upstream sha512 matches)"),
            "{caddy_row}"
        );
        // PostgreSQL: the theseus archive, our sha256, checked with its .sha256.
        let postgres = &value["runtimes"]["postgres"][0];
        assert_eq!(postgres["version"], "18.7.0");
        assert_eq!(postgres["platforms"].as_object().map(|o| o.len()), Some(5));
        let url = "https://github.com/theseus-rs/postgresql-binaries/releases/download/18.7.0/postgresql-18.7.0-x86_64-pc-windows-msvc.zip";
        let postgres_windows = &postgres["platforms"]["windows-x64"][0];
        assert_eq!(postgres_windows["urls"][0], url);
        assert_eq!(postgres_windows["sha256"], sha(url.as_bytes()));
        let postgres_rows: Vec<&String> = first
            .rows
            .iter()
            .filter(|row| row.starts_with("postgres 18.7.0 "))
            .collect();
        assert_eq!(postgres_rows.len(), 5);
        for row in postgres_rows {
            assert!(row.ends_with("(upstream sha256 matches)"), "{row}");
        }

        // The PHP checks ran for each macOS and Linux archive, every run.
        let seen = stub.0.lock().expect("lock").clone();
        assert_eq!(seen.len(), 12);
        assert_eq!(
            &seen[..4],
            [
                "darwin-arm64 php",
                "darwin-x64 php",
                "linux-x64 php",
                "linux-arm64 php"
            ]
        );
        // Our PHP archives: their release's sha256sums.txt, and the checks.
        assert!(
            first.rows[0].ends_with("(upstream sha256 matches; PHP checks passed)"),
            "{}",
            first.rows[0]
        );
        // Only archived Windows PHP has no upstream sum.
        assert_eq!(first.warnings.len(), 1, "{:?}", first.warnings);
        assert!(
            first.warnings[0].starts_with("php 8.4.23 windows-x64 zip: "),
            "{:?}",
            first.warnings
        );
    }

    /// One `[[override]]` that sends a file to a host without a known sum.
    fn moved(runtime: &str, version: &str, platform: &str, url: &str) -> String {
        mirrored(runtime, version, platform, &[url])
    }

    /// The same, with mirrors after the first URL.
    fn mirrored(runtime: &str, version: &str, platform: &str, urls: &[&str]) -> String {
        let urls = urls
            .iter()
            .map(|url| format!("\"{url}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{PINS}[[override]]\nruntime = \"{runtime}\"\nversion = \"{version}\"\n\
             platform = \"{platform}\"\nartifact = \"archive\"\nurls = [{urls}]\n"
        )
    }

    /// Every URL of a file is checked, mirrors too, before the first one is
    /// downloaded: a mirror lands in the signed manifest's `urls`.
    #[tokio::test]
    async fn every_mirror_is_checked_before_the_download() {
        let recipe_url = "https://github.com/theseus-rs/postgresql-binaries/releases/download/18.7.0/postgresql-18.7.0-x86_64-unknown-linux-gnu.tar.gz";
        for mirror in [
            "https://example.org/postgresql-18.7.0-x86_64-unknown-linux-gnu.tar.gz",
            "https://github.com/theseus-rs/postgresql-binaries/releases/download/17.12.0/postgresql-17.12.0-x86_64-unknown-linux-gnu.tar.gz",
        ] {
            let text = mirrored("postgres", "18.7.0", "linux-x64", &[recipe_url, mirror]);
            let pins = Pins::parse(&text).expect("pins");
            let fetch = fetcher(&pins, None);
            let cache = tempfile::tempdir().expect("tempdir");
            let err = build(&fetch, &pins, cache.path(), &Stub::default())
                .await
                .err()
                .expect("refused");
            let message = format!("{err:#}");
            assert!(
                message.starts_with(&format!("postgres 18.7.0 linux-x64 archive: {mirror}: ")),
                "{message}"
            );
            assert!(message.contains("not the recipe's URL"), "{message}");
            let asked = fetch.requests();
            assert!(
                !asked.iter().any(|u| u.starts_with(recipe_url)),
                "{asked:?}"
            );
        }
        // An MD5 mirror behind a good first URL, for any runtime.
        let node = "https://nodejs.org/dist/v22.23.4/node-v22.23.4-linux-x64.tar.gz";
        let md5 = "https://cdn.mysql.com/Downloads/MySQL-8.4/node-v22.23.4-linux-x64.tar.gz";
        let pins =
            Pins::parse(&mirrored("node", "22.23.4", "linux-x64", &[node, md5])).expect("pins");
        let fetch = fetcher(&pins, None);
        let cache = tempfile::tempdir().expect("tempdir");
        let err = build(&fetch, &pins, cache.path(), &Stub::default())
            .await
            .err()
            .expect("refused");
        assert!(
            format!("{err:#}").contains("never accepted alone"),
            "{err:#}"
        );
        assert!(!fetch.requests().iter().any(|u| u == node));
    }

    #[tokio::test]
    async fn postgres_needs_the_theseus_sum() {
        // The older runtimes keep the warning: Node from another host.
        let url = "https://example.org/node-v22.23.4-linux-x64.tar.gz";
        let pins = Pins::parse(&moved("node", "22.23.4", "linux-x64", url)).expect("pins");
        let cache = tempfile::tempdir().expect("tempdir");
        let built = build(&fetcher(&pins, None), &pins, cache.path(), &Stub::default())
            .await
            .expect("node without a sum is a warning");
        assert_eq!(built.warnings.len(), 2, "{:?}", built.warnings);
        assert!(
            built.warnings[1].contains(&format!("{url} publishes no sha256")),
            "{:?}",
            built.warnings
        );

        // PostgreSQL from any URL but the recipe's: refused before any
        // download. A host without a sum, a known source that is not
        // theseus-rs, and another theseus-rs archive, whose own `.sha256`
        // the fetcher makes match.
        let recipe_url = "https://github.com/theseus-rs/postgresql-binaries/releases/download/18.7.0/postgresql-18.7.0-x86_64-unknown-linux-gnu.tar.gz";
        for url in [
            "https://example.org/postgresql-18.7.0-x86_64-unknown-linux-gnu.tar.gz",
            "https://nodejs.org/dist/v18.7.0/postgresql-18.7.0.tar.gz",
            "https://windows.php.net/downloads/releases/postgresql-18.7.0.zip",
            "https://getcomposer.org/download/18.7.0/postgresql-18.7.0.tar.gz",
            "https://github.com/caddyserver/caddy/releases/download/v18.7.0/postgresql-18.7.0.tar.gz",
            "https://github.com/XJOC/marsa/releases/download/php-18.7.0-r1/postgresql-18.7.0.tar.gz",
            "https://cdn.mysql.com/Downloads/MySQL-8.4/postgresql-18.7.0.tar.gz",
            "https://github.com/theseus-rs/postgresql-binaries/releases/download/17.12.0/postgresql-17.12.0-x86_64-unknown-linux-gnu.tar.gz",
            "https://github.com/theseus-rs/postgresql-binaries/releases/download/18.7.0/postgresql-18.7.0-aarch64-unknown-linux-gnu.tar.gz",
        ] {
            let pins = Pins::parse(&moved("postgres", "18.7.0", "linux-x64", url)).expect("pins");
            let fetch = fetcher(&pins, None);
            let cache = tempfile::tempdir().expect("tempdir");
            let err = build(&fetch, &pins, cache.path(), &Stub::default())
                .await
                .err()
                .expect("refused");
            let message = format!("{err:#}");
            assert!(
                message.starts_with(&format!("postgres 18.7.0 linux-x64 archive: {url}: ")),
                "{message}"
            );
            let refusal = if url.starts_with("https://cdn.mysql.com/") {
                "an MD5 is never accepted alone".to_owned()
            } else {
                format!("not the recipe's URL {recipe_url}, and only that theseus-rs archive")
            };
            assert!(message.contains(&refusal), "{message}");
            let asked = fetch.requests();
            assert!(!asked.iter().any(|u| u.starts_with(url)), "{asked:?}");
        }
    }

    /// An MD5 is never enough for any runtime, and the build asks for
    /// nothing from such a URL.
    #[tokio::test]
    async fn an_md5_alone_is_refused_before_the_download() {
        let url = "https://cdn.mysql.com/Downloads/MySQL-8.4/node-v22.23.4-linux-x64.tar.gz";
        let pins = Pins::parse(&moved("node", "22.23.4", "linux-x64", url)).expect("pins");
        let fetch = fetcher(&pins, None);
        let cache = tempfile::tempdir().expect("tempdir");
        let err = build(&fetch, &pins, cache.path(), &Stub::default())
            .await
            .err()
            .expect("refused");
        let message = format!("{err:#}");
        assert!(
            message.starts_with(&format!("node 22.23.4 linux-x64 archive: {url}: ")),
            "{message}"
        );
        assert!(
            message.ends_with(&format!(
                "its only upstream sum is the MD5 {url}.md5, and an MD5 is never accepted alone"
            )),
            "{message}"
        );
        assert!(!fetch.requests().iter().any(|u| u.starts_with(url)));
    }

    /// Every way `TheseusSha256` refuses, also for a recipe URL that would
    /// not be theseus-rs.
    #[test]
    fn theseus_proof_needs_the_recipe_url_and_its_sum() {
        let url = format!(
            "{}18.7.0/postgresql-18.7.0.zip",
            super::super::upstream::THESEUS_RELEASES
        );
        let proof = Proof::TheseusSha256;
        let theseus = sum_file_for(&url);
        proof
            .allows(&url, Some(&url), theseus.as_ref())
            .expect("the recipe's theseus-rs URL");
        let refused = |url: &str, template: Option<&str>, sums: Option<&SumFile>| {
            format!("{:#}", proof.allows(url, template, sums).expect_err(url))
        };
        assert!(refused(&url, None, theseus.as_ref()).contains("not the recipe's URL (none)"));
        let other = "https://example.org/postgresql-18.7.0.zip";
        assert!(refused(other, Some(other), None).contains("no upstream sum"));
        let caddy = "https://github.com/caddyserver/caddy/releases/download/v2.11.8/caddy.zip";
        let sums = sum_file_for(caddy);
        assert!(
            refused(caddy, Some(caddy), sums.as_ref())
                .contains("only the theseus-rs `.sha256` is accepted")
        );
        let mysql = "https://cdn.mysql.com/Downloads/MySQL-8.4/postgresql.zip";
        let sums = sum_file_for(mysql);
        assert!(refused(mysql, Some(mysql), sums.as_ref()).contains("never accepted alone"));
        assert!(!proof.keeps_file());
        assert!(Proof::SumIfPublished.keeps_file());
    }

    #[tokio::test]
    async fn bad_theseus_sum_file_stops_the_build() {
        let pins = Pins::parse(PINS).expect("pins");
        let archive = "postgresql-18.7.0-aarch64-apple-darwin.tar.gz";
        let sum_url = format!(
            "https://github.com/theseus-rs/postgresql-binaries/releases/download/18.7.0/{archive}.sha256"
        );
        let hash = sha(sum_url.trim_end_matches(".sha256").as_bytes());
        let cases = [
            (Reply::Status(404), "answered with HTTP status 404"),
            (Reply::Chunks(vec![]), "is empty"),
            (
                Reply::Chunks(vec![format!("{hash}  {archive}\n{hash}  x\n").into_bytes()]),
                "is not one line",
            ),
            (
                Reply::Chunks(vec![format!("{hash}  other.tar.gz\n").into_bytes()]),
                "not for",
            ),
            (Reply::Chunks(vec![vec![0xff; 4]]), "is not text"),
        ];
        for (reply, expected) in cases {
            let fetch = fetcher(&pins, None).with(&sum_url, reply);
            let cache = tempfile::tempdir().expect("tempdir");
            let err = build(&fetch, &pins, cache.path(), &Stub::default())
                .await
                .err()
                .expect("refused");
            let message = format!("{err:#}");
            assert!(message.contains(archive), "names the file: {message}");
            assert!(message.contains(expected), "{expected:?}: {message}");
        }
        // The sum is right, the archive is not.
        let fetch = fetcher(&pins, Some(archive));
        let cache = tempfile::tempdir().expect("tempdir");
        let err = build(&fetch, &pins, cache.path(), &Stub::default())
            .await
            .err()
            .expect("refused");
        let message = format!("{err:#}");
        assert!(
            message.contains("postgres 18.7.0 darwin-arm64 archive: sha256 is")
                && message.contains(&format!("but {sum_url} says")),
            "{message}"
        );
    }

    #[tokio::test]
    async fn upstream_mismatch_stops_the_build() {
        let pins = Pins::parse(PINS).expect("pins");
        let fetch = fetcher(&pins, Some("node-v22.23.4-linux-x64.tar.gz"));
        let cache = tempfile::tempdir().expect("tempdir");
        let err = build(&fetch, &pins, cache.path(), &Stub::default())
            .await
            .err()
            .expect("refused");
        let message = format!("{err:#}");
        assert!(
            message.contains("node 22.23.4 linux-x64 archive: sha256 is"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn caddy_sha512_mismatch_stops_the_build() {
        let pins = Pins::parse(PINS).expect("pins");
        let fetch = fetcher(&pins, Some("caddy_2.11.8_windows_amd64.zip"));
        let cache = tempfile::tempdir().expect("tempdir");
        let err = build(&fetch, &pins, cache.path(), &Stub::default())
            .await
            .err()
            .expect("refused");
        let message = format!("{err:#}");
        assert!(
            message.contains("caddy 2.11.8 windows-x64 archive: sha512 is"),
            "{message}"
        );
        assert!(
            message.contains("but https://github.com/caddyserver/caddy/releases/download/v2.11.8/caddy_2.11.8_checksums.txt says"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn missing_upstream_entry_stops_the_build() {
        let pins = Pins::parse(&PINS.replace("releases/archives/", "releases/")).expect("pins");
        let mut fetch = fetcher(&pins, None);
        // A sum file without our name, as when a release moved to archives/.
        fetch = fetch.with(
            "https://windows.php.net/downloads/releases/sha256sum.txt",
            Reply::Chunks(vec![b"00  php-8.4.27-nts-Win32-vs17-x64.zip\r\n".to_vec()]),
        );
        let cache = tempfile::tempdir().expect("tempdir");
        let err = build(&fetch, &pins, cache.path(), &Stub::default())
            .await
            .err()
            .expect("refused");
        assert!(
            format!("{err:#}").contains("is not in the upstream sum file"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn falls_back_to_the_mirror() {
        let urls = vec![
            "https://example.org/a/composer.phar".to_owned(),
            "https://example.org/b/composer.phar".to_owned(),
        ];
        let fetch = MemoryFetch::new()
            .with(&urls[0], Reply::Status(404))
            .with(&urls[1], Reply::Chunks(vec![b"phar".to_vec()]));
        let dir = tempfile::tempdir().expect("tempdir");
        let file = cached_download(&fetch, &urls, dir.path(), "phar")
            .await
            .expect("mirror works");
        assert_eq!(file.url, urls[1]);
        assert_eq!(file.size, 4);
        assert_eq!(fs::read(dir.path().join("phar")).expect("saved"), b"phar");
        assert!(!dir.path().join("phar.part").exists());

        let dir = tempfile::tempdir().expect("tempdir");
        let err = cached_download(&fetch, &urls, dir.path(), ".hidden")
            .await
            .err()
            .expect("bad name");
        assert!(err.to_string().contains("is not [a-z0-9._-]+"));

        // Hashed only: the same answer, and nothing on disk.
        let file = hashed_download(&fetch, &urls).await.expect("mirror works");
        assert_eq!(file.url, urls[1]);
        assert_eq!(
            (file.size, file.sha256),
            (4, Sha256::digest(b"phar").into())
        );
        assert!(file.path.is_none());
        assert!(format!("{:#}", file.kept().expect_err("not kept")).contains("not kept"));
    }

    /// A body that breaks midway is not part of the hash of the mirror that
    /// answers next, kept or not.
    #[tokio::test]
    async fn a_broken_body_then_a_mirror_hashes_the_mirror_only() {
        let urls = vec![
            "https://example.org/a/phar".to_owned(),
            "https://example.org/b/phar".to_owned(),
        ];
        let fetch = MemoryFetch::new()
            .with(&urls[0], Reply::ChunksThenError(vec![b"ph".to_vec()]))
            .with(&urls[1], Reply::Chunks(vec![b"phar".to_vec()]));
        let want: [u8; 32] = Sha256::digest(b"phar").into();

        let file = hashed_download(&fetch, &urls).await.expect("mirror works");
        assert_eq!(
            (file.url.as_str(), file.size, file.sha256),
            (urls[1].as_str(), 4, want)
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let file = cached_download(&fetch, &urls, dir.path(), "phar")
            .await
            .expect("mirror works");
        assert_eq!(
            (file.url.as_str(), file.size, file.sha256),
            (urls[1].as_str(), 4, want)
        );
        assert_eq!(fs::read(dir.path().join("phar")).expect("saved"), b"phar");
        assert!(!dir.path().join("phar.part").exists());
    }

    #[tokio::test]
    async fn an_empty_or_missing_file_is_an_error_when_only_hashed() {
        let urls = vec!["https://example.org/a.tar.gz".to_owned()];
        for (reply, expected) in [
            (Reply::Chunks(vec![]), "is empty"),
            (Reply::Status(404), "no URL worked"),
        ] {
            let fetch = MemoryFetch::new().with(&urls[0], reply);
            let err = hashed_download(&fetch, &urls).await.err().expect("refused");
            assert!(format!("{err:#}").contains(expected), "{err:#}");
        }
    }

    #[test]
    fn sha512_is_of_the_bytes_the_manifest_records() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("archive");
        fs::write(&path, b"caddy").expect("write");
        let sha256: [u8; 32] = Sha256::digest(b"caddy").into();
        let sha512 = sha512_file(&path, &sha256).expect("same bytes");
        assert_eq!(sha512.as_slice(), Sha512::digest(b"caddy").as_slice());

        fs::write(&path, b"other").expect("write");
        let err = sha512_file(&path, &sha256).expect_err("the file changed");
        assert!(err.to_string().contains("changed on disk"), "{err}");
    }

    #[test]
    fn serial_must_grow() {
        check_serial(2, Some(1)).expect("greater");
        check_serial(1, None).expect("first publish");
        for (serial, published) in [(1, Some(1)), (1, Some(2))] {
            let err = check_serial(serial, published).expect_err("not greater");
            assert!(err.to_string().contains("must be greater"));
        }
        assert!(check_serial(0, None).is_err());
    }

    const MYSQL: &str = "https://cdn.mysql.com/Downloads/MySQL-8.4/mysql-8.4.13-linux-glibc2.28-x86_64-minimal.tar.xz";
    const MYSQL_NAME: &str = "mysql-8.4.13-linux-glibc2.28-x86_64-minimal.tar.xz";
    const BODY: &[u8] = b"the mysql archive";
    const MD5: [u8; 16] = [7; 16];

    /// Oracle's checks, answering what it was given, and what it was asked.
    struct StubOracle {
        signature_ok: bool,
        md5: [u8; 16],
        /// What `tar -tf` would list first.
        first_entry: String,
        /// The `.asc` and archive bytes it was asked about.
        seen: std::sync::Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
    }

    impl StubOracle {
        fn new(signature_ok: bool, md5: [u8; 16]) -> Self {
            Self {
                signature_ok,
                md5,
                first_entry: format!("{}/bin/mysqld", MYSQL_NAME.trim_end_matches(".tar.xz")),
                seen: std::sync::Mutex::default(),
            }
        }
    }

    impl OracleCheck for StubOracle {
        fn signature(&self, signature: &Path, file: &Path) -> Result<()> {
            let read = (fs::read(signature)?, fs::read(file)?);
            self.seen.lock().expect("lock").push(read);
            ensure!(self.signature_ok, "BADSIG");
            Ok(())
        }

        fn md5(&self, _file: &Path) -> Result<[u8; 16]> {
            Ok(self.md5)
        }

        fn first_entry(&self, _file: &Path) -> Result<String> {
            Ok(self.first_entry.clone())
        }
    }

    /// The archive, its `.md5` (of [`MD5`]), and its `.asc`.
    fn oracle_fetch() -> MemoryFetch {
        let md5 = format!("{}  {MYSQL_NAME}\n", hex(&MD5));
        MemoryFetch::new()
            .with(
                MYSQL,
                Reply::Chunks(vec![BODY[..4].to_vec(), BODY[4..].to_vec()]),
            )
            .with(
                &format!("{MYSQL}.md5"),
                Reply::Chunks(vec![md5.into_bytes()]),
            )
            .with(
                &format!("{MYSQL}.asc"),
                Reply::Chunks(vec![b"signature".to_vec()]),
            )
    }

    /// The work folder holds nothing after a check.
    fn empty(dir: &Path) -> bool {
        fs::read_dir(dir).map_or(true, |mut entries| entries.next().is_none())
    }

    #[tokio::test]
    async fn oracle_archive_with_a_good_signature_and_md5_is_checked() {
        let work = tempfile::tempdir().expect("tempdir");
        // What a killed run left.
        fs::write(work.path().join("archive"), b"stale").expect("write");
        let oracle = StubOracle::new(true, MD5);
        let checked = check_oracle(&oracle_fetch(), MYSQL, work.path(), &oracle)
            .await
            .expect("both proofs pass");
        assert_eq!(
            checked,
            Checked {
                url: MYSQL.to_owned(),
                sha256: Sha256::digest(BODY).into(),
                size: BODY.len() as u64,
            }
        );
        let seen = oracle.seen.lock().expect("lock").clone();
        assert_eq!(seen, [(b"signature".to_vec(), BODY.to_vec())]);
        assert!(empty(work.path()), "the archive and its .asc are deleted");
    }

    #[tokio::test]
    async fn oracle_archive_needs_both_proofs() {
        // A bad signature, then a good signature with another MD5.
        for (oracle, expected) in [
            (
                StubOracle::new(false, MD5),
                "Oracle's signature check failed",
            ),
            (StubOracle::new(true, [8; 16]), "md5 is 0808"),
        ] {
            let work = tempfile::tempdir().expect("tempdir");
            let err = check_oracle(&oracle_fetch(), MYSQL, work.path(), &oracle)
                .await
                .expect_err("refused");
            assert!(format!("{err:#}").contains(expected), "{err:#}");
            assert!(empty(work.path()), "deleted after a refusal too");
        }
        // Without the `.asc` or the `.md5`: refused, and the archive is
        // never downloaded.
        for (missing, expected) in [("asc", "Oracle's signature"), ("md5", "Oracle's md5")] {
            let fetch = oracle_fetch().with(&format!("{MYSQL}.{missing}"), Reply::Status(404));
            let work = tempfile::tempdir().expect("tempdir");
            let oracle = StubOracle::new(true, MD5);
            let err = check_oracle(&fetch, MYSQL, work.path(), &oracle)
                .await
                .expect_err("refused");
            assert!(format!("{err:#}").contains(expected), "{err:#}");
            assert!(!fetch.requests().iter().any(|url| url == MYSQL));
            assert!(oracle.seen.lock().expect("lock").is_empty());
        }
    }

    #[tokio::test]
    async fn oracle_archive_of_another_version_under_this_name_is_refused() {
        for entry in [
            "mysql-8.4.11-linux-glibc2.28-x86_64-minimal/bin/mysqld",
            "mysql-8.4.13-linux-glibc2.28-x86_64-minimal-extra/",
            "/bin/mysqld",
            "",
        ] {
            let mut oracle = StubOracle::new(true, MD5);
            oracle.first_entry = entry.to_owned();
            let work = tempfile::tempdir().expect("tempdir");
            let err = check_oracle(&oracle_fetch(), MYSQL, work.path(), &oracle)
                .await
                .expect_err("another folder");
            assert!(
                format!("{err:#}").contains("the archive's first entry"),
                "{entry}: {err:#}"
            );
            assert!(empty(work.path()), "deleted after a refusal too");
        }
        // The top folder alone, as a `tar -tf` lists a folder first.
        let mut oracle = StubOracle::new(true, MD5);
        oracle.first_entry = format!("{}/", MYSQL_NAME.trim_end_matches(".tar.xz"));
        let work = tempfile::tempdir().expect("tempdir");
        check_oracle(&oracle_fetch(), MYSQL, work.path(), &oracle)
            .await
            .expect("the folder itself");
    }

    #[test]
    fn the_top_folder_comes_from_a_tar_name_only() {
        assert_eq!(
            top_folder_of("mysql-8.4.13-macos15-arm64.tar.gz").unwrap(),
            "mysql-8.4.13-macos15-arm64"
        );
        assert_eq!(
            top_folder_of("mysql-8.4.13-x.tar.xz").unwrap(),
            "mysql-8.4.13-x"
        );
        for name in ["mysql-8.4.13-winx64.zip", "mysql.tar", ".tar.xz.exe"] {
            assert!(top_folder_of(name).is_err(), "{name}");
        }
    }

    /// Only an Oracle MySQL 8.4 archive, before any request.
    #[tokio::test]
    async fn oracle_proof_takes_oracle_archives_only() {
        for url in [
            "https://cdn.mysql.com/Downloads/MySQL-9.7/mysql-9.7.4-linux-x64.tar.xz",
            "https://example.org/mysql-8.4.13.tar.xz",
            "https://github.com/theseus-rs/postgresql-binaries/releases/download/18.7.0/postgresql-18.7.0-x86_64-pc-windows-msvc.zip",
            "https://github.com/XJOC/marsa/releases/download/valkey-8.1.11-r1/valkey.tar.gz",
        ] {
            let fetch = oracle_fetch();
            let work = tempfile::tempdir().expect("tempdir");
            let err = check_oracle(&fetch, url, work.path(), &StubOracle::new(true, MD5))
                .await
                .expect_err("refused");
            assert!(
                format!("{err:#}").contains("only an Oracle MySQL 8.4 archive"),
                "{err:#}"
            );
            assert!(fetch.requests().is_empty());
        }
        // In the build, a URL that is not the recipe's.
        let sums = sum_file_for(MYSQL);
        let err = Proof::OracleSignature
            .allows(
                MYSQL,
                Some("https://cdn.mysql.com/Downloads/MySQL-8.4/x.tar.xz"),
                sums.as_ref(),
            )
            .expect_err("another archive");
        assert!(
            err.to_string().contains("only that Oracle archive"),
            "{err}"
        );
        assert!(!Proof::OracleSignature.keeps_file());
    }
}
