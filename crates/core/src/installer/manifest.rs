//! The signed manifest: which versions of each runtime exist, the URLs of
//! their files, and the sha256 and size of each file. This is the root of
//! trust of the installer: a sha256 protects nothing if an attacker can change
//! the manifest that lists it.
//!
//! [`Manifest::verify_and_parse`] is the only way to get a [`Manifest`]. It
//! checks the minisign signature over the exact bytes **before** `serde_json`
//! reads them, then checks the content. [`check_serial`] refuses a manifest
//! older than the last one accepted (a rollback to a version with a known
//! hole). Both functions are pure: bytes in, a value or an error out.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use minisign_verify::Signature;
use semver::Version;
use serde::Deserialize;

use crate::recipes::Platform;
use crate::runtimes::Runtime;

/// Re-exported so callers need no direct dependency on `minisign-verify`.
pub use minisign_verify::PublicKey;

/// The public key that signs the real manifest: the base64 line (the second
/// line) of the owner's `marsa-runtimes.pub`.
///
/// `None` until the owner creates the key pair (task M2-02, "owner steps").
/// While it is `None`, [`production_key`] and [`Manifest::verify_embedded`]
/// fail with `manifest_no_trusted_key`, so nothing can be installed.
///
/// Rules: this constant is the only trusted key; no environment variable or
/// config file can replace it, in any build. Never put a test key here (a test
/// checks this). Changing it is a `sensitive` task. When it is set, write the
/// date it was created and its key id in this comment.
pub const MANIFEST_PUBLIC_KEY: Option<&str> = None;

/// Largest manifest we read: 1 MiB. Checked before anything else.
pub const MAX_MANIFEST_SIZE: usize = 1024 * 1024;

/// Largest file a manifest may list: 2 GiB.
pub const MAX_ARTIFACT_SIZE: u64 = 2 * 1024 * 1024 * 1024;

/// The only schema this version reads. A change that breaks the meaning of
/// the JSON raises it; new fields do not.
pub const SCHEMA_VERSION: u64 = 1;

/// A `.minisig` file is about 300 bytes. A larger one is refused unread.
const MAX_SIGNATURE_SIZE: usize = 4096;

/// Legacy signatures sign the raw bytes instead of their BLAKE2b hash. The
/// `minisign` tool has not made them by default for years, so a legacy
/// signature on our manifest means someone other than us made it.
const ALLOW_LEGACY: bool = false;

/// A manifest whose signature and content passed every check. The fields are
/// private and the type has no `Deserialize`, so code outside this module can
/// get one only through [`Manifest::verify_and_parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    serial: u64,
    php: Vec<Release>,
    node: Vec<Release>,
    composer: Vec<Release>,
}

/// One version of a runtime and its files for each platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    version: Version,
    platforms: BTreeMap<Platform, Vec<ArtifactSource>>,
}

/// One file to download. `name` matches an artifact `name` in the recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSource {
    name: String,
    urls: Vec<String>,
    sha256: [u8; 32],
    size: u64,
}

impl Manifest {
    /// Checks the signature of `json` with `key`, then parses and checks it.
    ///
    /// The order is fixed, cheapest first: size, signature, JSON, schema,
    /// content. `serde_json` never sees bytes whose signature failed.
    ///
    /// # Errors
    ///
    /// `manifest_too_large`, `manifest_bad_signature` (one error for every
    /// signature failure; the reason goes to `tracing::debug!`),
    /// `manifest_invalid`, or `manifest_schema_unsupported`.
    pub fn verify_and_parse(
        json: &[u8],
        minisig: &str,
        key: &PublicKey,
    ) -> Result<Manifest, ManifestError> {
        if json.len() > MAX_MANIFEST_SIZE {
            return Err(ManifestError::TooLarge { size: json.len() });
        }
        verify_signature(json, minisig, key)?;
        parse_verified(json)
    }

    /// [`Manifest::verify_and_parse`] with the key built into the binary.
    ///
    /// # Errors
    ///
    /// `manifest_no_trusted_key` while [`MANIFEST_PUBLIC_KEY`] is `None`, or
    /// any error of [`Manifest::verify_and_parse`].
    pub fn verify_embedded(json: &[u8], minisig: &str) -> Result<Manifest, ManifestError> {
        Manifest::verify_and_parse(json, minisig, &production_key()?)
    }

    /// The serial number. The caller passes it to [`check_serial`].
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Every release of `runtime`, in the manifest's order.
    pub fn releases(&self, runtime: Runtime) -> &[Release] {
        match runtime {
            Runtime::Php => &self.php,
            Runtime::Node => &self.node,
            Runtime::Composer => &self.composer,
        }
    }

    /// The release of `runtime` with exactly this version.
    pub fn release(&self, runtime: Runtime, version: &Version) -> Option<&Release> {
        self.releases(runtime)
            .iter()
            .find(|r| &r.version == version)
    }
}

impl Release {
    pub fn version(&self) -> &Version {
        &self.version
    }

    /// The files for `platform`, or `None` if this version has no build for it.
    pub fn platform(&self, platform: Platform) -> Option<&[ArtifactSource]> {
        self.platforms.get(&platform).map(Vec::as_slice)
    }
}

impl ArtifactSource {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Never empty. The first URL is the main one; the others are mirrors of
    /// the same bytes. Every URL starts with `https://`.
    pub fn urls(&self) -> &[String] {
        &self.urls
    }

    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    /// More than zero and at most [`MAX_ARTIFACT_SIZE`].
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// The key built into the binary.
///
/// # Errors
///
/// `manifest_no_trusted_key` if [`MANIFEST_PUBLIC_KEY`] is `None` or is not a
/// valid minisign key.
pub fn production_key() -> Result<PublicKey, ManifestError> {
    let line = MANIFEST_PUBLIC_KEY.ok_or(ManifestError::NoTrustedKey)?;
    PublicKey::from_base64(line).map_err(|e| {
        tracing::debug!(reason = %e, "the embedded manifest key is not a valid minisign key");
        ManifestError::NoTrustedKey
    })
}

/// Refuses a manifest older than the last one accepted. The same serial is
/// fine (the same file again).
///
/// # Errors
///
/// `manifest_rollback` if `new` is lower than `last_seen`.
pub fn check_serial(new: u64, last_seen: Option<u64>) -> Result<(), ManifestError> {
    match last_seen {
        Some(last_seen) if new < last_seen => Err(ManifestError::Rollback {
            serial: new,
            last_seen,
        }),
        _ => Ok(()),
    }
}

/// Why a manifest was refused. [`ManifestError::code`] gives a stable name
/// that the UI maps to a message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("the runtimes manifest is {size} bytes, more than the limit of {MAX_MANIFEST_SIZE}")]
    TooLarge { size: usize },

    /// Every signature failure, on purpose: the message does not tell an
    /// attacker which check failed.
    #[error("the runtimes manifest signature is not valid")]
    BadSignature,

    /// Bad JSON, or content that breaks a rule (in `reason`). Only reached
    /// after the signature passed.
    #[error("the runtimes manifest is not valid: {reason}")]
    Invalid { reason: String },

    #[error(
        "the runtimes manifest uses schema {schema}, which this version cannot read; update Marsa"
    )]
    SchemaUnsupported { schema: u64 },

    #[error(
        "the runtimes manifest (serial {serial}) is older than one already used (serial {last_seen})"
    )]
    Rollback { serial: u64, last_seen: u64 },

    #[error("this build of Marsa has no trusted key for the runtimes manifest")]
    NoTrustedKey,
}

impl ManifestError {
    /// Stable error name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            ManifestError::TooLarge { .. } => "manifest_too_large",
            ManifestError::BadSignature => "manifest_bad_signature",
            ManifestError::Invalid { .. } => "manifest_invalid",
            ManifestError::SchemaUnsupported { .. } => "manifest_schema_unsupported",
            ManifestError::Rollback { .. } => "manifest_rollback",
            ManifestError::NoTrustedKey => "manifest_no_trusted_key",
        }
    }
}

fn verify_signature(json: &[u8], minisig: &str, key: &PublicKey) -> Result<(), ManifestError> {
    if minisig.len() > MAX_SIGNATURE_SIZE {
        tracing::debug!(size = minisig.len(), "manifest signature is too large");
        return Err(ManifestError::BadSignature);
    }
    let signature = Signature::decode(minisig).map_err(|e| {
        tracing::debug!(reason = %e, "manifest signature could not be decoded");
        ManifestError::BadSignature
    })?;
    // Checks the key id, refuses legacy signatures, then checks the signature
    // of the bytes and the signature of the trusted comment.
    key.verify(json, &signature, ALLOW_LEGACY).map_err(|e| {
        tracing::debug!(reason = %e, "manifest signature check failed");
        ManifestError::BadSignature
    })
}

/// Reads JSON whose signature already passed. Private: the only caller is
/// [`Manifest::verify_and_parse`] (and tests).
fn parse_verified(json: &[u8]) -> Result<Manifest, ManifestError> {
    // The schema first, alone, so a newer manifest with a new shape gives
    // "update Marsa" and not a parse error.
    let probe: SchemaProbe = serde_json::from_slice(json).map_err(bad_json)?;
    if probe.schema > SCHEMA_VERSION {
        return Err(ManifestError::SchemaUnsupported {
            schema: probe.schema,
        });
    }
    if probe.schema != SCHEMA_VERSION {
        return Err(invalid(format!("schema {} does not exist", probe.schema)));
    }
    let raw: RawManifest = serde_json::from_slice(json).map_err(bad_json)?;
    Ok(Manifest {
        serial: raw.serial,
        php: releases("php", raw.runtimes.php)?,
        node: releases("node", raw.runtimes.node)?,
        composer: releases("composer", raw.runtimes.composer)?,
    })
}

// The JSON shape. Unknown fields are ignored on purpose (no
// `deny_unknown_fields`): a newer manifest may add fields that this version
// does not need. A change that breaks the meaning raises `schema` instead.

#[derive(Deserialize)]
struct SchemaProbe {
    schema: u64,
}

#[derive(Deserialize)]
struct RawManifest {
    serial: u64,
    runtimes: RawRuntimes,
}

#[derive(Deserialize)]
struct RawRuntimes {
    #[serde(default)]
    php: Vec<RawRelease>,
    #[serde(default)]
    node: Vec<RawRelease>,
    #[serde(default)]
    composer: Vec<RawRelease>,
}

#[derive(Deserialize)]
struct RawRelease {
    version: String,
    platforms: BTreeMap<String, Vec<RawSource>>,
}

#[derive(Deserialize)]
struct RawSource {
    name: String,
    urls: Vec<String>,
    sha256: String,
    size: u64,
}

fn releases(runtime: &str, raw: Vec<RawRelease>) -> Result<Vec<Release>, ManifestError> {
    let mut seen = BTreeSet::new();
    raw.into_iter()
        .map(|raw| {
            let version = Version::parse(&raw.version).map_err(|_| {
                invalid(format!(
                    "{runtime} version {:?} is not a full semver version",
                    raw.version
                ))
            })?;
            if !seen.insert(version.clone()) {
                return Err(invalid(format!("{runtime} {version} is listed twice")));
            }
            let mut platforms = BTreeMap::new();
            for (key, sources) in raw.platforms {
                let platform = Platform::from_str(&key).map_err(|_| {
                    invalid(format!("{runtime} {version}: unknown platform {key:?}"))
                })?;
                let what = format!("{runtime} {version} {key}");
                platforms.insert(platform, sources_for(&what, sources)?);
            }
            Ok(Release { version, platforms })
        })
        .collect()
}

fn sources_for(what: &str, raw: Vec<RawSource>) -> Result<Vec<ArtifactSource>, ManifestError> {
    if raw.is_empty() {
        return Err(invalid(format!("{what} has no files")));
    }
    let mut names = BTreeSet::new();
    raw.into_iter()
        .map(|raw| {
            if raw.name.is_empty() || !names.insert(raw.name.clone()) {
                return Err(invalid(format!(
                    "{what}: file name {:?} is empty or listed twice",
                    raw.name
                )));
            }
            let what = format!("{what} {}", raw.name);
            if raw.urls.is_empty() {
                return Err(invalid(format!("{what} has no URL")));
            }
            if let Some(url) = raw.urls.iter().find(|url| !is_https_url(url)) {
                return Err(invalid(format!(
                    "{what}: URL {url:?} is not an https:// URL"
                )));
            }
            let sha256 = parse_sha256(&raw.sha256).ok_or_else(|| {
                invalid(format!(
                    "{what}: sha256 must be 64 lowercase hex characters"
                ))
            })?;
            if raw.size == 0 || raw.size > MAX_ARTIFACT_SIZE {
                return Err(invalid(format!(
                    "{what}: size {} is out of range",
                    raw.size
                )));
            }
            Ok(ArtifactSource {
                name: raw.name,
                urls: raw.urls,
                sha256,
                size: raw.size,
            })
        })
        .collect()
}

/// `https://`, a host, and only printable ASCII (URLs in the manifest are
/// already percent-encoded). `http`, `file`, and anything else are refused.
fn is_https_url(url: &str) -> bool {
    url.strip_prefix("https://")
        .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
        && url.bytes().all(|b| b.is_ascii_graphic())
}

/// 64 lowercase hex characters to 32 bytes. Uppercase is refused, so each
/// hash has exactly one spelling.
fn parse_sha256(text: &str) -> Option<[u8; 32]> {
    fn digit(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        }
    }
    if text.len() != 64 {
        return None;
    }
    // The length is 64, so there are exactly 32 pairs and no remainder.
    let (pairs, _) = text.as_bytes().as_chunks::<2>();
    let mut bytes = [0u8; 32];
    for (byte, &[high, low]) in bytes.iter_mut().zip(pairs) {
        *byte = (digit(high)? << 4) | digit(low)?;
    }
    Some(bytes)
}

fn invalid(reason: String) -> ManifestError {
    ManifestError::Invalid { reason }
}

fn bad_json(e: serde_json::Error) -> ManifestError {
    invalid(format!("bad JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Signed once with a throwaway key; see tests/fixtures/manifest/README.md.
    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!("../../tests/fixtures/manifest/", $name))
        };
    }

    const VALID: &str = fixture!("valid.json");
    const VALID_SIG: &str = fixture!("valid.json.minisig");

    fn test_key() -> PublicKey {
        PublicKey::decode(fixture!("test-key.pub")).expect("the test key fixture is valid")
    }

    fn check(json: &str, sig: &str) -> Result<Manifest, ManifestError> {
        Manifest::verify_and_parse(json.as_bytes(), sig, &test_key())
    }

    fn code(result: Result<Manifest, ManifestError>) -> &'static str {
        result.expect_err("the manifest must be refused").code()
    }

    #[test]
    fn valid_signature_gives_manifest() {
        let manifest = check(VALID, VALID_SIG).expect("valid fixture");
        assert_eq!(manifest.serial(), 3);

        let php = manifest
            .release(Runtime::Php, &Version::new(8, 4, 23))
            .expect("php 8.4.23");
        let mac = php.platform(Platform::DarwinArm64).expect("darwin-arm64");
        assert_eq!(mac.len(), 2);
        assert_eq!(mac[0].name(), "cli");
        assert_eq!(mac[0].size(), 36_179_860);
        assert_eq!(&mac[0].sha256()[..3], &[0x7f, 0x6b, 0xed]);
        let windows = php.platform(Platform::WindowsX64).expect("windows-x64");
        assert_eq!(windows[0].urls().len(), 2);
        assert!(php.platform(Platform::LinuxX64).is_none());

        assert_eq!(manifest.releases(Runtime::Node).len(), 1);
        let composer = &manifest.releases(Runtime::Composer)[0];
        assert_eq!(composer.version(), &Version::new(2, 8, 12));
        let any = composer.platform(Platform::Any).expect("any");
        assert_eq!(any[0].name(), "phar");
    }

    #[test]
    fn one_changed_byte_is_bad_signature() {
        // Still valid JSON, so only the signature can catch it.
        let changed = VALID.replacen("\"serial\": 3", "\"serial\": 9", 1);
        assert_ne!(changed, VALID);
        assert_eq!(code(check(&changed, VALID_SIG)), "manifest_bad_signature");
        let extra_newline = format!("{VALID}\n");
        assert_eq!(
            code(check(&extra_newline, VALID_SIG)),
            "manifest_bad_signature"
        );
    }

    #[test]
    fn signature_by_another_key_is_bad_signature() {
        let sig = fixture!("valid.json.other-key.minisig");
        assert_eq!(code(check(VALID, sig)), "manifest_bad_signature");
    }

    #[test]
    fn another_key_with_our_key_id_is_bad_signature() {
        // Key B's signature with key A's id written in: it passes the key id
        // check, so the Ed25519 check itself must refuse it.
        let sig = fixture!("valid.json.other-key-same-id.minisig");
        assert_eq!(code(check(VALID, sig)), "manifest_bad_signature");
    }

    #[test]
    fn changed_trusted_comment_is_bad_signature() {
        let sig = VALID_SIG.replacen("serial=3", "serial=4", 1);
        assert_ne!(sig, VALID_SIG);
        assert_eq!(code(check(VALID, &sig)), "manifest_bad_signature");
    }

    #[test]
    fn legacy_signature_is_bad_signature() {
        let sig = fixture!("valid.json.legacy.minisig");
        // The fixture is a real legacy signature by the right key...
        let decoded = Signature::decode(sig).expect("legacy fixture decodes");
        assert!(test_key().verify(VALID.as_bytes(), &decoded, true).is_ok());
        // ...and we still refuse it.
        assert_eq!(code(check(VALID, sig)), "manifest_bad_signature");
    }

    #[test]
    fn missing_or_broken_signature_is_bad_signature() {
        let two_lines = VALID_SIG.lines().take(2).collect::<Vec<_>>().join("\n");
        let huge = "a".repeat(MAX_SIGNATURE_SIZE + 1);
        for sig in [
            "",
            "\n",
            &two_lines,
            &VALID_SIG[..VALID_SIG.len() / 2],
            "not a signature at all",
            fixture!("test-key.pub"),
            &huge,
        ] {
            assert_eq!(code(check(VALID, sig)), "manifest_bad_signature", "{sig:?}");
        }
    }

    #[test]
    fn signed_invalid_json_is_invalid() {
        let json = fixture!("invalid-json.json");
        let sig = fixture!("invalid-json.json.minisig");
        // The signature is good: the error comes from the JSON step.
        assert!(verify_signature(json.as_bytes(), sig, &test_key()).is_ok());
        assert_eq!(code(check(json, sig)), "manifest_invalid");
    }

    #[test]
    fn unsigned_content_is_bad_signature_before_any_parse() {
        // If JSON were read first, these would give `manifest_invalid` or
        // `manifest_schema_unsupported`.
        for json in [
            "{not json",
            fixture!("schema-2.json"),
            fixture!("http-url.json"),
        ] {
            assert_eq!(
                code(check(json, VALID_SIG)),
                "manifest_bad_signature",
                "{json}"
            );
        }
    }

    #[test]
    fn signed_http_url_is_invalid() {
        let json = fixture!("http-url.json");
        let sig = fixture!("http-url.json.minisig");
        assert_eq!(code(check(json, sig)), "manifest_invalid");
    }

    #[test]
    fn signed_bad_sha256_is_invalid() {
        let upper = fixture!("sha256-uppercase.json");
        let upper_sig = fixture!("sha256-uppercase.json.minisig");
        assert_eq!(code(check(upper, upper_sig)), "manifest_invalid");
        let short = fixture!("sha256-short.json");
        let short_sig = fixture!("sha256-short.json.minisig");
        assert_eq!(code(check(short, short_sig)), "manifest_invalid");
    }

    #[test]
    fn signed_schema_2_is_unsupported() {
        let json = fixture!("schema-2.json");
        let sig = fixture!("schema-2.json.minisig");
        assert_eq!(code(check(json, sig)), "manifest_schema_unsupported");
    }

    #[test]
    fn signed_unknown_platform_is_invalid() {
        let json = fixture!("unknown-platform.json");
        let sig = fixture!("unknown-platform.json.minisig");
        assert_eq!(code(check(json, sig)), "manifest_invalid");
    }

    #[test]
    fn signed_non_semver_version_is_invalid() {
        let json = fixture!("bad-version.json");
        let sig = fixture!("bad-version.json.minisig");
        assert_eq!(code(check(json, sig)), "manifest_invalid");
    }

    #[test]
    fn signed_duplicate_version_is_invalid() {
        let json = fixture!("duplicate-version.json");
        let sig = fixture!("duplicate-version.json.minisig");
        assert_eq!(code(check(json, sig)), "manifest_invalid");
    }

    #[test]
    fn too_large_is_refused_before_the_signature() {
        // A good signature would not help: the size check comes first.
        let big = " ".repeat(MAX_MANIFEST_SIZE + 1);
        assert_eq!(code(check(&big, VALID_SIG)), "manifest_too_large");
        // Exactly the limit is read, and then fails the signature.
        let limit = " ".repeat(MAX_MANIFEST_SIZE);
        assert_eq!(code(check(&limit, VALID_SIG)), "manifest_bad_signature");
    }

    #[test]
    fn check_serial_blocks_rollback() {
        let err = check_serial(2, Some(3)).expect_err("lower serial");
        assert_eq!(err.code(), "manifest_rollback");
        assert!(check_serial(3, Some(3)).is_ok());
        assert!(check_serial(4, Some(3)).is_ok());
        assert!(check_serial(0, None).is_ok());

        let manifest = check(VALID, VALID_SIG).expect("valid fixture");
        assert!(check_serial(manifest.serial(), Some(4)).is_err());
    }

    #[test]
    fn production_key_is_embedded_or_refused() {
        match MANIFEST_PUBLIC_KEY {
            None => {
                let err = production_key().expect_err("no key yet");
                assert_eq!(err.code(), "manifest_no_trusted_key");
                let err = Manifest::verify_embedded(VALID.as_bytes(), VALID_SIG)
                    .expect_err("nothing verifies without a key");
                assert_eq!(err.code(), "manifest_no_trusted_key");
            }
            Some(_) => {
                let key = production_key().expect("the embedded key decodes");
                assert_ne!(
                    key,
                    test_key(),
                    "the test key must never be the production key"
                );
            }
        }
    }

    /// Content rules on JSON whose signature is taken as checked, one composer
    /// file at a time. The signed fixtures above cover the same path end to end.
    #[test]
    fn content_rules() {
        const SHA: &str = "2e17f6527a6c92f5713ba16e5dfd1b55869ab5e98de46677e994fed5536316ba";
        const URL: &str = "https://getcomposer.org/composer.phar";
        let doc = |schema: &str, files: &str| {
            format!(
                r#"{{"schema": {schema}, "serial": 1, "extra": true, "runtimes": {{"composer": [
                    {{"version": "2.8.12", "platforms": {{"any": [{files}]}}}}]}}}}"#
            )
        };
        let file = |name: &str, urls: &str, size: u64| {
            format!(
                r#"{{"name": "{name}", "urls": {urls}, "sha256": "{SHA}", "size": {size}, "new": 1}}"#
            )
        };
        let one_url = format!(r#"["{URL}"]"#);
        let good = file("phar", &one_url, 10);

        // Unknown fields are ignored, and runtimes that are not listed are empty.
        let manifest = parse_verified(doc("1", &good).as_bytes()).expect("good content");
        assert!(manifest.releases(Runtime::Php).is_empty());
        assert_eq!(manifest.releases(Runtime::Composer).len(), 1);

        let refused = [
            doc("0", &good),
            doc("1", ""),
            doc("1", &file("phar", &one_url, 0)),
            doc("1", &file("phar", &one_url, MAX_ARTIFACT_SIZE + 1)),
            doc("1", &file("phar", "[]", 10)),
            doc("1", &file("", &one_url, 10)),
            doc("1", &format!("{good}, {good}")),
            doc("1", &file("phar", r#"["file:///etc/passwd"]"#, 10)),
            doc("1", &file("phar", r#"["https://"]"#, 10)),
            doc("1", &file("phar", r#"["https:///path"]"#, 10)),
            doc("1", &file("phar", r#"["https://a b/c"]"#, 10)),
            doc(
                "1",
                &file("phar", &format!(r#"["{URL}", "http://x/y"]"#), 10),
            ),
            r#"{"schema": 1, "runtimes": {}}"#.to_string(),
        ];
        for json in refused {
            let err = parse_verified(json.as_bytes()).expect_err(&json);
            assert_eq!(err.code(), "manifest_invalid", "{json}");
        }
    }

    #[test]
    fn error_codes_are_stable() {
        let all = [
            (ManifestError::TooLarge { size: 0 }, "manifest_too_large"),
            (ManifestError::BadSignature, "manifest_bad_signature"),
            (invalid(String::new()), "manifest_invalid"),
            (
                ManifestError::SchemaUnsupported { schema: 2 },
                "manifest_schema_unsupported",
            ),
            (
                ManifestError::Rollback {
                    serial: 1,
                    last_seen: 2,
                },
                "manifest_rollback",
            ),
            (ManifestError::NoTrustedKey, "manifest_no_trusted_key"),
        ];
        for (err, code) in all {
            assert_eq!(err.code(), code);
        }
    }
}
