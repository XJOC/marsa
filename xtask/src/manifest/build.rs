//! `cargo xtask manifest build`: for every pinned version, platform, and
//! artifact, check that its URLs can give the proof its runtime needs,
//! download the file (or reuse the cache), hash it, compare with the
//! upstream sum (sha256, or sha512 for Caddy), run the PHP checks, then
//! write `manifest.json`. A database server without the proof of its own
//! source stops the build; the older runtimes only get a warning without an
//! upstream sum. An MD5 alone is never accepted.
//!
//! The database servers' archives are large (PostgreSQL is about 60 MB a
//! file, 620 MB a build), so they are hashed on the way and not kept: the
//! ones that a program must read (Oracle's signature, GitHub's attestation)
//! are deleted as soon as it has.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use marsa_core::installer::fetch::{Fetch, fetch_bytes};
use marsa_core::installer::manifest::{MIN_SERIAL, SCHEMA_VERSION};
use marsa_core::recipes::{self, Artifact, Platform, PlatformRecipe};
use marsa_core::runtimes::Runtime;
use semver::Version;

use super::pins::{Pins, RUNTIMES};
use super::upstream::{Algorithm, hex, sum_file_for};
mod download;
mod json;
mod proof;

use download::*;
pub(in crate::manifest) use json::*;
pub(in crate::manifest) use proof::*;

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
/// `cache` (see `Proof::keeps_file`); the others are only hashed, and the
/// archives of MySQL and PostgreSQL are kept only while they are checked.
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
                        Ok(Downloaded::gone(checked.url, checked.sha256, checked.size))
                    } else if proof == Proof::TheseusSha256 {
                        // `gh attestation verify` reads a file. It is 60 MB:
                        // deleted at the end of this artifact, whatever the
                        // result.
                        scratch_download(fetch, &urls, &cache.join("theseus-work")).await
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
                    // After the `.sha256`, and for every PostgreSQL archive,
                    // whatever the match above did.
                    if proof == Proof::TheseusSha256 {
                        verify_attestation(inspect, &file, version)
                            .with_context(|| what.clone())?;
                        notes.push("GitHub attestation verified");
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

#[cfg(test)]
mod tests;
