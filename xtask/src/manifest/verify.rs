//! `cargo xtask manifest verify`: check the signature with the key built into
//! Marsa, and check the JSON with the same rules as the installer (M2-02).
//!
//! The signature check goes through `Manifest::verify_embedded` only: no
//! other key can be trusted. While the key slot is empty, that call refuses
//! everything with `manifest_no_trusted_key`; the local JSON check still runs.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use marsa_core::installer::manifest::{
    MAX_ARTIFACT_SIZE, MAX_MANIFEST_SIZE, MIN_SERIAL, Manifest, SCHEMA_VERSION,
    TRUSTED_COMMENT_PREFIX,
};
use marsa_core::recipes::Platform;
use semver::Version;

use super::build::ManifestJson;
use super::pins::RUNTIMES;
use super::upstream::parse_hex;

/// The rules of `marsa_core::installer::manifest`, on JSON whose signature
/// is not checked here. `build` runs it before writing; `verify` always.
pub fn check_json(bytes: &[u8]) -> Result<ManifestJson> {
    ensure!(
        bytes.len() <= MAX_MANIFEST_SIZE,
        "{} bytes, more than the limit of {MAX_MANIFEST_SIZE}",
        bytes.len()
    );
    let manifest: ManifestJson = serde_json::from_slice(bytes).context("bad JSON")?;
    ensure!(
        manifest.schema == SCHEMA_VERSION,
        "schema is {}, expected {SCHEMA_VERSION}",
        manifest.schema
    );
    ensure!(
        manifest.serial >= MIN_SERIAL,
        "serial must be {MIN_SERIAL} or more"
    );
    for runtime in RUNTIMES {
        let mut versions = BTreeSet::new();
        for release in manifest.runtimes.get(runtime) {
            let what = format!("{} {}", runtime.dir_name(), release.version);
            let version = Version::parse(&release.version)
                .with_context(|| format!("{what}: not a full semver version"))?;
            ensure!(versions.insert(version), "{what}: listed twice");
            for (key, sources) in &release.platforms {
                let what = format!("{what} {key}");
                Platform::from_str(key).with_context(|| what.clone())?;
                ensure!(!sources.is_empty(), "{what}: no files");
                let mut names = BTreeSet::new();
                for source in sources {
                    let what = format!("{what} {}", source.name);
                    ensure!(
                        is_artifact_name(&source.name),
                        "{what}: name must be [a-z0-9._-]+, not starting with ."
                    );
                    ensure!(names.insert(&source.name), "{what}: listed twice");
                    ensure!(!source.urls.is_empty(), "{what}: no URL");
                    if let Some(url) = source.urls.iter().find(|u| !is_https_url(u)) {
                        bail!("{what}: {url:?} is not an https:// URL");
                    }
                    ensure!(
                        parse_hex(&source.sha256).is_some(),
                        "{what}: sha256 must be 64 lowercase hex characters"
                    );
                    ensure!(
                        source.size > 0 && source.size <= MAX_ARTIFACT_SIZE,
                        "{what}: size {} is out of range",
                        source.size
                    );
                }
            }
        }
    }
    Ok(manifest)
}

fn is_artifact_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
}

fn is_https_url(url: &str) -> bool {
    url.strip_prefix("https://")
        .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
        && url.bytes().all(|b| b.is_ascii_graphic())
}

/// Serial, then one line per runtime: its versions and their platforms.
pub fn summary(manifest: &ManifestJson) -> String {
    let mut text = format!("serial {}\n", manifest.serial);
    for runtime in RUNTIMES {
        let releases = manifest.runtimes.get(runtime);
        text.push_str(&format!(
            "{}: {} version(s)\n",
            runtime.dir_name(),
            releases.len()
        ));
        for release in releases {
            let platforms: Vec<&str> = release.platforms.keys().map(String::as_str).collect();
            text.push_str(&format!(
                "  {}: {}\n",
                release.version,
                platforms.join(", ")
            ));
        }
    }
    text
}

/// The trusted comment the owner signs with.
pub fn trusted_comment(serial: u64) -> String {
    format!("{TRUSTED_COMMENT_PREFIX} serial={serial}")
}

/// The `trusted comment:` line of a `.minisig` file.
fn trusted_comment_of(minisig: &str) -> Option<&str> {
    minisig
        .lines()
        .find_map(|l| l.trim_end_matches('\r').strip_prefix("trusted comment: "))
}

pub fn run(manifest_path: &Path, sig_path: Option<PathBuf>) -> Result<()> {
    let bytes = fs::read(manifest_path)
        .with_context(|| format!("cannot read {}", manifest_path.display()))?;
    let local = check_json(&bytes).context("the JSON breaks a manifest rule")?;
    println!("Local JSON check: OK\n{}", summary(&local));

    let sig_path = sig_path.unwrap_or_else(|| {
        let mut name = manifest_path.as_os_str().to_owned();
        name.push(".minisig");
        PathBuf::from(name)
    });
    let sig = fs::read_to_string(&sig_path)
        .with_context(|| format!("no signature: cannot read {}", sig_path.display()))?;

    match Manifest::verify_embedded(&bytes, &sig, None) {
        Ok(manifest) => {
            let expected = trusted_comment(manifest.serial());
            ensure!(
                trusted_comment_of(&sig) == Some(expected.as_str()),
                "signature OK, but its trusted comment is not {expected:?}"
            );
            println!(
                "Signature: OK with the key built into marsa (serial {})",
                manifest.serial()
            );
            Ok(())
        }
        Err(err) if err.code() == "manifest_no_trusted_key" => bail!(
            "signature NOT checked ({}): this build of marsa has no trusted key yet \
             (MANIFEST_PUBLIC_KEY is None in crates/core/src/installer/manifest.rs). \
             Only the local JSON check above passed. Do not publish until the key is \
             added and this command passes.",
            err.code()
        ),
        Err(err) => bail!("signature check failed ({}): {err}", err.code()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "2e17f6527a6c92f5713ba16e5dfd1b55869ab5e98de46677e994fed5536316ba";

    fn doc(schema: u64, serial: u64, file: &str) -> String {
        format!(
            r#"{{"schema": {schema}, "serial": {serial}, "runtimes": {{"composer": [
                {{"version": "2.11.1", "platforms": {{"any": [{file}]}}}}]}}}}"#
        )
    }

    fn file(name: &str, url: &str, sha: &str, size: u64) -> String {
        format!(r#"{{"name": "{name}", "urls": ["{url}"], "sha256": "{sha}", "size": {size}}}"#)
    }

    const URL: &str = "https://getcomposer.org/download/2.11.1/composer.phar";

    #[test]
    fn local_rules() {
        let good = doc(1, 1, &file("phar", URL, SHA, 10));
        let manifest = check_json(good.as_bytes()).expect("good");
        assert_eq!(
            summary(&manifest),
            "serial 1\nphp: 0 version(s)\nnode: 0 version(s)\ncomposer: 1 version(s)\n  2.11.1: any\n"
        );
        let refused = [
            doc(2, 1, &file("phar", URL, SHA, 10)),
            doc(1, 0, &file("phar", URL, SHA, 10)),
            doc(1, 1, &file(".phar", URL, SHA, 10)),
            doc(1, 1, &file("Phar", URL, SHA, 10)),
            doc(1, 1, &file("phar", "http://getcomposer.org/x", SHA, 10)),
            doc(1, 1, &file("phar", URL, &SHA.to_uppercase(), 10)),
            doc(1, 1, &file("phar", URL, SHA, 0)),
            doc(1, 1, ""),
            doc(1, 1, &format!("{0}, {0}", file("phar", URL, SHA, 10))),
            good.replace("2.11.1", "2.11"),
            good.replace("\"any\"", "\"windows-arm64\""),
        ];
        for json in refused {
            assert!(check_json(json.as_bytes()).is_err(), "{json}");
        }
    }

    #[test]
    fn empty_key_slot_is_reported_after_the_local_check() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("manifest.json");
        fs::write(&path, doc(1, 1, &file("phar", URL, SHA, 10))).expect("write");
        // No signature file.
        let err = run(&path, None).expect_err("no signature");
        assert!(format!("{err:#}").contains("no signature"), "{err:#}");
        // Any signature: the empty key slot refuses before reading it.
        fs::write(dir.path().join("manifest.json.minisig"), "junk").expect("write");
        let err = run(&path, None).expect_err("not verified");
        if marsa_core::installer::manifest::MANIFEST_PUBLIC_KEY.is_none() {
            assert!(err.to_string().contains("manifest_no_trusted_key"), "{err}");
        } else {
            assert!(err.to_string().contains("manifest_bad_signature"), "{err}");
        }
    }

    #[test]
    fn reads_the_trusted_comment() {
        let sig = "untrusted comment: x\r\nRUQ...\r\ntrusted comment: marsa runtimes manifest serial=3\r\nabc\r\n";
        assert_eq!(trusted_comment_of(sig), Some(trusted_comment(3).as_str()));
    }
}
