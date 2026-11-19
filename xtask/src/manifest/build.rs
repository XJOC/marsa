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
use super::upstream::{Algorithm, Layout, Source, SumFile, hex, parse_hex, sum_file_for};

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
}

impl RuntimesJson {
    pub fn get(&self, runtime: Runtime) -> &Vec<ReleaseJson> {
        match runtime {
            Runtime::Php => &self.php,
            Runtime::Node => &self.node,
            Runtime::Composer => &self.composer,
            Runtime::Caddy => &self.caddy,
            Runtime::Postgres => &self.postgres,
        }
    }

    fn get_mut(&mut self, runtime: Runtime) -> &mut Vec<ReleaseJson> {
        match runtime {
            Runtime::Php => &mut self.php,
            Runtime::Node => &mut self.node,
            Runtime::Composer => &mut self.composer,
            Runtime::Caddy => &mut self.caddy,
            Runtime::Postgres => &mut self.postgres,
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
/// accepted alone: MySQL (M6-02 part B2) gets a variant that needs Oracle's
/// GPG signature as well as the MD5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Proof {
    /// The upstream sum if the source publishes one `build` knows. Without
    /// one, only a warning: today, Windows PHP in `releases/archives/`
    /// (windows.php.net lists only sha1 there). PHP, Node, Composer, Caddy.
    SumIfPublished,
    /// The theseus-rs `<archive>.sha256` ([`Layout::Single`]) and no other
    /// source, of the recipe's own URL. PostgreSQL.
    TheseusSha256,
}

impl Proof {
    /// An exhaustive `match`, so each new runtime has to choose.
    fn of(runtime: Runtime) -> Proof {
        match runtime {
            Runtime::Php | Runtime::Node | Runtime::Composer | Runtime::Caddy => {
                Proof::SumIfPublished
            }
            Runtime::Postgres => Proof::TheseusSha256,
        }
    }

    /// Whether the file at `url` can give this proof, before anything is
    /// downloaded. `sums` is the sum file [`sum_file_for`] finds for `url`
    /// (`None`: no known one), and `template` the recipe's URL for the file.
    /// The sum is compared after the download.
    fn allows(self, url: &str, template: Option<&str>, sums: Option<&SumFile>) -> Result<()> {
        match (self, sums) {
            (_, Some(sums)) if sums.algorithm == Algorithm::Md5 => bail!(
                "its only upstream sum is the MD5 {}, and an MD5 is never accepted alone",
                sums.url
            ),
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
        }
    }

    /// Whether the file is kept in the cache after the build: only when a
    /// later check reads it (PHP, the sha512 of Caddy). A file this proof
    /// checks by its sha256 alone is hashed on the way and never written.
    fn keeps_file(self) -> bool {
        match self {
            Proof::SumIfPublished => true,
            Proof::TheseusSha256 => false,
        }
    }
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

/// Builds the manifest. Every file is saved under `cache`.
pub async fn build(
    fetch: &impl Fetch,
    pins: &Pins,
    cache: &Path,
    inspect: &impl Inspect,
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
                    let proof = Proof::of(runtime);
                    for url in &urls {
                        let sums = sum_file_for(url);
                        proof
                            .allows(url, template.as_deref(), sums.as_ref())
                            .with_context(|| format!("{what}: {url}"))?;
                    }
                    let file = if proof.keeps_file() {
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
                    // One of `urls`, so `proof` allows its sum file.
                    let sums = sum_file_for(&file.url);
                    match sums {
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
                                // download.
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
        file.flush()?;
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
        format!(
            "{PINS}[[override]]\nruntime = \"{runtime}\"\nversion = \"{version}\"\n\
             platform = \"{platform}\"\nartifact = \"archive\"\nurls = [\"{url}\"]\n"
        )
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
}
