//! `cargo xtask manifest build`: for every pinned version, platform, and
//! artifact, download the file (or reuse the cache), hash it, compare with
//! the upstream sum, run the PHP checks, then write `manifest.json`.

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
use sha2::{Digest, Sha256};

use super::pins::{Pins, RUNTIMES};
use super::upstream::{find_sha256, hex, parse_hex, sum_file_for};

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
}

impl RuntimesJson {
    pub fn get(&self, runtime: Runtime) -> &Vec<ReleaseJson> {
        match runtime {
            Runtime::Php => &self.php,
            Runtime::Node => &self.node,
            Runtime::Composer => &self.composer,
        }
    }

    fn get_mut(&mut self, runtime: Runtime) -> &mut Vec<ReleaseJson> {
        match runtime {
            Runtime::Php => &mut self.php,
            Runtime::Node => &mut self.node,
            Runtime::Composer => &mut self.composer,
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
                    let dir = cache
                        .join(runtime.dir_name())
                        .join(version.to_string())
                        .join(platform.as_str());
                    let file = cached_download(fetch, &urls, &dir, &artifact.name)
                        .await
                        .with_context(|| what.clone())?;

                    let mut notes = Vec::new();
                    match sum_file_for(&file.url) {
                        Some((sum_url, name)) => {
                            if !sum_files.contains_key(&sum_url) {
                                let bytes = fetch_bytes(fetch, &sum_url, 1024 * 1024)
                                    .await
                                    .with_context(|| format!("{what}: upstream sums"))?;
                                let text = String::from_utf8(bytes)
                                    .with_context(|| format!("{sum_url} is not text"))?;
                                sum_files.insert(sum_url.clone(), text);
                            }
                            let text = sum_files.get(&sum_url).map_or("", String::as_str);
                            let expected = find_sha256(text, &name)
                                .with_context(|| format!("{what}: {sum_url}"))?;
                            ensure!(
                                expected == file.sha256,
                                "{what}: sha256 is {}, but {sum_url} says {}",
                                hex(&file.sha256),
                                hex(&expected)
                            );
                            notes.push("upstream sha256 matches");
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
                            .php_build(&target, &file.path)
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
    path: PathBuf,
    url: String,
    sha256: [u8; 32],
    size: u64,
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
                path,
                url: url.to_owned(),
                sha256,
                size,
            });
        }
    }

    let part = dir.join(format!("{name}.part"));
    let mut last = None;
    for url in urls {
        let _ = fs::remove_file(&part);
        let mut sink = FileSink {
            file: File::create(&part)
                .with_context(|| format!("cannot create {}", part.display()))?,
            path: part.clone(),
            url: url.clone(),
            hasher: Sha256::new(),
            size: 0,
        };
        println!("  downloading {url}");
        match fetch.get(url, &mut sink).await {
            Ok(()) => {
                sink.file.flush()?;
                ensure!(sink.size > 0, "{url} is empty");
                let sha256: [u8; 32] = sink.hasher.finalize().into();
                fs::rename(&part, &path)
                    .with_context(|| format!("cannot move {}", part.display()))?;
                fs::write(&record, format!("{} {url}\n", hex(&sha256)))?;
                return Ok(Downloaded {
                    path,
                    url: url.clone(),
                    sha256,
                    size: sink.size,
                });
            }
            Err(err @ (FetchError::HttpStatus { .. } | FetchError::Network { .. })) => {
                println!("    {err}; trying the next URL");
                last = Some(err);
            }
            Err(err) => {
                let _ = fs::remove_file(&part);
                return Err(err.into());
            }
        }
    }
    let _ = fs::remove_file(&part);
    match last {
        Some(err) => Err(anyhow::Error::from(err).context("no URL worked")),
        None => bail!("no URL"),
    }
}

/// Writes the body to a file and hashes it on the way.
struct FileSink {
    file: File,
    path: PathBuf,
    url: String,
    hasher: Sha256,
    size: u64,
}

impl ChunkSink for FileSink {
    fn accept(&mut self, chunk: &[u8]) -> Result<(), FetchError> {
        self.size = self.size.saturating_add(chunk.len() as u64);
        if self.size > MAX_ARTIFACT_SIZE {
            return Err(FetchError::TooLarge {
                url: self.url.clone(),
                max: MAX_ARTIFACT_SIZE,
            });
        }
        self.hasher.update(chunk);
        self.file.write_all(chunk).map_err(|source| FetchError::Io {
            path: self.path.clone(),
            source,
        })
    }
}

fn hash_file(path: &Path) -> Result<([u8; 32], u64)> {
    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    let mut size = 0u64;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err.into()),
        };
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((hasher.finalize().into(), size))
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
        hex(&Sha256::digest(body).into())
    }

    /// Every URL of the pins, with a fake body (the URL itself) and sum files
    /// that agree, except for the node file named in `bad_node`.
    fn fetcher(pins: &Pins, bad_node: Option<&str>) -> MemoryFetch {
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
                        if let Some((sum_url, name)) = sum_file_for(url) {
                            let hash = if bad_node == Some(name.as_str()) {
                                sha(b"something else")
                            } else {
                                sha(&body)
                            };
                            sums.entry(sum_url)
                                .or_default()
                                .push_str(&format!("{hash}  {name}\r\n"));
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
            new_requests, 3,
            "only the php, node and composer sum files are fetched again"
        );

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
