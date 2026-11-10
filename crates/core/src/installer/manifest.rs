//! The signed manifest: which versions of each runtime exist, the URLs of
//! their files, and the sha256 and size of each file. This is the root of
//! trust of the installer: a sha256 protects nothing if an attacker can change
//! the manifest that lists it.
//!
//! [`Manifest::verify_embedded`] is the only public way to get a
//! [`Manifest`]. It checks the minisign signature over the exact bytes with
//! the key built into the binary **before** `serde_json` reads them, refuses a
//! manifest older than the last one accepted (a rollback to a version with a
//! known hole), then checks the content. It is pure: bytes in, a value or an
//! error out.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};

use crate::recipes::Platform;
use crate::runtimes::Runtime;

/// The public key that signs the real manifest: the base64 line (the second
/// line) of the owner's `marsa-runtimes.pub`.
///
/// The owner created the key pair on 2026-11-10 (task M2-08). Key id
/// `0136668A6B96BDEE`. If this were `None`, [`Manifest::verify_embedded`]
/// would fail with `manifest_no_trusted_key`, so nothing could be installed.
///
/// Rules: this constant is the only trusted key; no environment variable or
/// config file can replace it, in any build. Never put a test key here (a test
/// checks this). Changing it is a `sensitive` task. When it is set, write the
/// date it was created and its key id in this comment.
pub const MANIFEST_PUBLIC_KEY: Option<&str> =
    Some("RWTuvZZrimY2ASzeaoWovb0LKYQTrVnNIbXoxoTKGJEZmSeLlyomDKxU");

/// Every real manifest is signed with a trusted comment that starts with
/// this, for example `minisign -S ... -t "marsa runtimes manifest serial=7"`.
/// The trusted comment is covered by the signature, so this ties the
/// signature to its purpose: a file the same key signed for something else
/// is refused.
pub const TRUSTED_COMMENT_PREFIX: &str = "marsa runtimes manifest";

/// The lowest serial accepted, even when no serial was saved yet (first run).
/// The floor is `max(last_seen, MIN_SERIAL)`. Raise it in a release when old
/// manifests must never be accepted again, for example after a security fix.
pub const MIN_SERIAL: u64 = 1;

/// Largest manifest we read: 1 MiB. Checked before anything else.
pub const MAX_MANIFEST_SIZE: usize = 1024 * 1024;

/// A file listed in a manifest is at most this size: 2 GiB.
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

/// A manifest whose signature, serial, and content passed every check. The
/// fields are private and the type has no `Deserialize`, so code outside this
/// module can get one only through [`Manifest::verify_embedded`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    serial: u64,
    php: Vec<Release>,
    node: Vec<Release>,
    composer: Vec<Release>,
    caddy: Vec<Release>,
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
    /// Checks `json` with the key built into the binary ([`MANIFEST_PUBLIC_KEY`]).
    ///
    /// `last_seen` is the serial of the last manifest accepted, or `None` if
    /// there is none yet. It is required, so the rollback check cannot be
    /// forgotten. The caller saves [`Manifest::serial`] after success.
    ///
    /// # Errors
    ///
    /// `manifest_no_trusted_key` while [`MANIFEST_PUBLIC_KEY`] is `None`, or
    /// any error of `verify_and_parse`: `manifest_too_large`,
    /// `manifest_bad_signature`, `manifest_schema_unsupported`,
    /// `manifest_rollback`, or `manifest_invalid`.
    pub fn verify_embedded(
        json: &[u8],
        minisig: &str,
        last_seen: Option<u64>,
    ) -> Result<Manifest, ManifestError> {
        Manifest::verify_and_parse(json, minisig, &production_key()?, last_seen)
    }

    /// Checks the signature of `json` with `key`, then parses and checks it.
    /// Not public: outside the crate, only the embedded key is trusted. The
    /// crate's own tests pass a test key here.
    ///
    /// The order is fixed, cheapest first: size, signature (and its trusted
    /// comment), schema, serial, content. `serde_json` never sees bytes whose
    /// signature failed.
    ///
    /// # Errors
    ///
    /// `manifest_too_large`, `manifest_bad_signature` (one error for every
    /// signature failure; the reason goes to `tracing::debug!`),
    /// `manifest_schema_unsupported`, `manifest_rollback`, or
    /// `manifest_invalid`.
    pub(crate) fn verify_and_parse(
        json: &[u8],
        minisig: &str,
        key: &PublicKey,
        last_seen: Option<u64>,
    ) -> Result<Manifest, ManifestError> {
        if json.len() > MAX_MANIFEST_SIZE {
            return Err(ManifestError::TooLarge { size: json.len() });
        }
        verify_signature(json, minisig, key)?;
        parse_verified(json, last_seen)
    }

    /// The serial number. The caller saves it as the next `last_seen`.
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Every release of `runtime`, in the manifest's order.
    pub fn releases(&self, runtime: Runtime) -> &[Release] {
        match runtime {
            Runtime::Php => &self.php,
            Runtime::Node => &self.node,
            Runtime::Composer => &self.composer,
            Runtime::Caddy => &self.caddy,
        }
    }

    /// The release of `runtime` with exactly this version.
    pub fn release(&self, runtime: Runtime, version: &Version) -> Option<&Release> {
        self.releases(runtime)
            .iter()
            .find(|r| &r.version == version)
    }
}

// Compiled only into this crate's own unit tests, never into a build that
// ships.
#[cfg(test)]
impl Manifest {
    /// Reads `json` as if its signature had passed, with the same serial and
    /// content checks. The installer's tests use it for manifests that list
    /// archives the test builds itself: their sha256 is not known when the
    /// signed fixtures are made. The signature path is tested with the signed
    /// fixtures.
    pub(crate) fn unsigned_for_tests(
        json: &[u8],
        last_seen: Option<u64>,
    ) -> Result<Manifest, ManifestError> {
        parse_verified(json, last_seen)
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
    /// Not empty; only `a-z`, `0-9`, `.`, `_`, and `-`.
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

    /// `serial` is lower than `max(last_seen, MIN_SERIAL)`, given as `minimum`.
    #[error(
        "the runtimes manifest (serial {serial}) is older than allowed (serial {minimum} or newer)"
    )]
    Rollback { serial: u64, minimum: u64 },

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

/// The key built into the binary.
fn production_key() -> Result<PublicKey, ManifestError> {
    let line = MANIFEST_PUBLIC_KEY.ok_or(ManifestError::NoTrustedKey)?;
    PublicKey::from_base64(line).map_err(|e| {
        tracing::debug!(reason = %e, "the embedded manifest key is not a valid minisign key");
        ManifestError::NoTrustedKey
    })
}

/// Refuses a serial lower than `max(last_seen, MIN_SERIAL)`. The same serial
/// is fine (the same file again).
fn check_serial(serial: u64, last_seen: Option<u64>) -> Result<(), ManifestError> {
    let minimum = last_seen.map_or(MIN_SERIAL, |last| last.max(MIN_SERIAL));
    if serial < minimum {
        return Err(ManifestError::Rollback { serial, minimum });
    }
    Ok(())
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
    })?;
    // Only now is the trusted comment trusted.
    if !signature
        .trusted_comment()
        .starts_with(TRUSTED_COMMENT_PREFIX)
    {
        tracing::debug!(
            comment = signature.trusted_comment(),
            "manifest signature is not for a runtimes manifest"
        );
        return Err(ManifestError::BadSignature);
    }
    Ok(())
}

/// Reads JSON whose signature already passed. Private: the only caller is
/// [`Manifest::verify_and_parse`] (and tests).
fn parse_verified(json: &[u8], last_seen: Option<u64>) -> Result<Manifest, ManifestError> {
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
    // Then the serial, alone, so an old manifest is refused as a rollback
    // whatever its content.
    let probe: SerialProbe = serde_json::from_slice(json).map_err(bad_json)?;
    check_serial(probe.serial, last_seen)?;

    let raw: RawManifest = serde_json::from_slice(json).map_err(bad_json)?;
    Ok(Manifest {
        serial: probe.serial,
        php: releases("php", raw.runtimes.php)?,
        node: releases("node", raw.runtimes.node)?,
        composer: releases("composer", raw.runtimes.composer)?,
        caddy: releases("caddy", raw.runtimes.caddy)?,
    })
}

// The JSON shape. Unknown fields are ignored on purpose (no
// `deny_unknown_fields`): a newer manifest may add fields that this version
// does not need. A change that breaks the meaning raises `schema` instead.
// A field named twice in one object is an error (serde's derive refuses it).

#[derive(Deserialize)]
struct SchemaProbe {
    schema: u64,
}

#[derive(Deserialize)]
struct SerialProbe {
    serial: u64,
}

#[derive(Deserialize)]
struct RawManifest {
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
    /// Added in M2-10 within schema 1: older builds ignore it, and a manifest
    /// without it has no Caddy.
    #[serde(default)]
    caddy: Vec<RawRelease>,
}

#[derive(Deserialize)]
struct RawRelease {
    version: String,
    /// Kept as a list, not a map: a map would keep only the last of two
    /// entries with the same key, and we want to refuse that.
    #[serde(deserialize_with = "map_entries")]
    platforms: Vec<(String, Vec<RawSource>)>,
}

#[derive(Deserialize)]
struct RawSource {
    name: String,
    urls: Vec<String>,
    sha256: String,
    size: u64,
}

/// Reads a JSON object as its list of entries, duplicates included.
fn map_entries<'de, D>(deserializer: D) -> Result<Vec<(String, Vec<RawSource>)>, D::Error>
where
    D: Deserializer<'de>,
{
    struct Entries;

    impl<'de> Visitor<'de> for Entries {
        type Value = Vec<(String, Vec<RawSource>)>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an object of platforms")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut entries = Vec::new();
            while let Some(entry) = map.next_entry()? {
                entries.push(entry);
            }
            Ok(entries)
        }
    }

    deserializer.deserialize_map(Entries)
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
                let sources = sources_for(&what, sources)?;
                if platforms.insert(platform, sources).is_some() {
                    return Err(invalid(format!("{what}: platform is listed twice")));
                }
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
            if !is_artifact_name(&raw.name) {
                return Err(invalid(format!(
                    "{what}: file name {:?} must be [a-z0-9._-]+",
                    raw.name
                )));
            }
            if !names.insert(raw.name.clone()) {
                return Err(invalid(format!(
                    "{what}: file name {:?} is listed twice",
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

/// `[a-z0-9._-]+`, like the artifact names in the recipes.
fn is_artifact_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
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

    // Made by tools/manifest-fixtures; see tests/fixtures/manifest/README.md.
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

    fn check_with(
        json: &str,
        sig: &str,
        last_seen: Option<u64>,
    ) -> Result<Manifest, ManifestError> {
        Manifest::verify_and_parse(json.as_bytes(), sig, &test_key(), last_seen)
    }

    fn check(json: &str, sig: &str) -> Result<Manifest, ManifestError> {
        check_with(json, sig, None)
    }

    fn code(result: Result<Manifest, ManifestError>) -> &'static str {
        result.expect_err("the manifest must be refused").code()
    }

    /// The `reason` of a `manifest_invalid` error, so a test also pins the cause.
    fn reason(result: Result<Manifest, ManifestError>) -> String {
        match result.expect_err("the manifest must be refused") {
            ManifestError::Invalid { reason } => reason,
            other => panic!("expected manifest_invalid, got {}", other.code()),
        }
    }

    /// The signature is good for these bytes with the test key: whatever
    /// refuses the fixture is not the Ed25519 check.
    fn assert_crypto_ok(json: &str, sig: &str, allow_legacy: bool) {
        let decoded = Signature::decode(sig).expect("the fixture decodes");
        assert!(
            test_key()
                .verify(json.as_bytes(), &decoded, allow_legacy)
                .is_ok()
        );
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
    fn trusted_comment_without_our_prefix_is_bad_signature() {
        // A real signature by the right key, but its trusted comment is the
        // minisign default: it was not made for a runtimes manifest.
        let sig = fixture!("valid.json.wrong-comment.minisig");
        assert_crypto_ok(VALID, sig, false);
        assert_eq!(code(check(VALID, sig)), "manifest_bad_signature");
    }

    #[test]
    fn legacy_signature_is_bad_signature() {
        // A real legacy signature by the right key, with our trusted comment.
        let sig = fixture!("valid.json.legacy.minisig");
        assert_crypto_ok(VALID, sig, true);
        assert_eq!(code(check(VALID, sig)), "manifest_bad_signature");
    }

    #[test]
    fn missing_or_broken_signature_is_bad_signature() {
        let two_lines = VALID_SIG.lines().take(2).collect::<Vec<_>>().join("\n");
        for sig in [
            "",
            "\n",
            &two_lines,
            &VALID_SIG[..VALID_SIG.len() / 2],
            "not a signature at all",
            fixture!("test-key.pub"),
        ] {
            assert_eq!(code(check(VALID, sig)), "manifest_bad_signature", "{sig:?}");
        }
    }

    #[test]
    fn oversized_signature_is_bad_signature() {
        // A good signature with a junk fifth line. `Signature::decode` ignores
        // the fifth line, so only the size limit refuses it.
        let padded = format!("{VALID_SIG}{}\n", "#".repeat(MAX_SIGNATURE_SIZE));
        assert_crypto_ok(VALID, &padded, false);
        assert_eq!(code(check(VALID, &padded)), "manifest_bad_signature");
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
    fn signed_invalid_json_is_invalid() {
        let json = fixture!("invalid-json.json");
        let sig = fixture!("invalid-json.json.minisig");
        // The signature is good: the error comes from the JSON step.
        assert!(verify_signature(json.as_bytes(), sig, &test_key()).is_ok());
        let reason = reason(check(json, sig));
        assert!(reason.contains("bad JSON"), "{reason}");
    }

    #[test]
    fn signed_http_url_is_invalid() {
        let json = fixture!("http-url.json");
        let reason = reason(check(json, fixture!("http-url.json.minisig")));
        assert!(reason.contains("is not an https:// URL"), "{reason}");
    }

    #[test]
    fn signed_bad_sha256_is_invalid() {
        let upper = fixture!("sha256-uppercase.json");
        let upper_sig = fixture!("sha256-uppercase.json.minisig");
        let reason_upper = reason(check(upper, upper_sig));
        assert!(reason_upper.contains("sha256 must be"), "{reason_upper}");
        let short = fixture!("sha256-short.json");
        let short_sig = fixture!("sha256-short.json.minisig");
        let reason_short = reason(check(short, short_sig));
        assert!(reason_short.contains("sha256 must be"), "{reason_short}");
    }

    #[test]
    fn signed_schema_2_is_unsupported() {
        let json = fixture!("schema-2.json");
        let sig = fixture!("schema-2.json.minisig");
        assert_eq!(code(check(json, sig)), "manifest_schema_unsupported");
        // The schema comes before the serial.
        assert_eq!(
            code(check_with(json, sig, Some(100))),
            "manifest_schema_unsupported"
        );
    }

    #[test]
    fn signed_unknown_platform_is_invalid() {
        let json = fixture!("unknown-platform.json");
        let reason = reason(check(json, fixture!("unknown-platform.json.minisig")));
        assert!(reason.contains("unknown platform"), "{reason}");
    }

    #[test]
    fn signed_non_semver_version_is_invalid() {
        let json = fixture!("bad-version.json");
        let reason = reason(check(json, fixture!("bad-version.json.minisig")));
        assert!(reason.contains("not a full semver version"), "{reason}");
    }

    #[test]
    fn signed_duplicate_version_is_invalid() {
        let json = fixture!("duplicate-version.json");
        let reason = reason(check(json, fixture!("duplicate-version.json.minisig")));
        assert!(reason.contains("2.8.12 is listed twice"), "{reason}");
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
    fn rollback_is_refused_by_verify_and_parse() {
        // valid.json has serial 3.
        assert_eq!(
            code(check_with(VALID, VALID_SIG, Some(4))),
            "manifest_rollback"
        );
        for last_seen in [Some(3), Some(2), None] {
            check_with(VALID, VALID_SIG, last_seen).expect("serial 3 is not older");
        }
        // Serial 0 is below MIN_SERIAL even on first run.
        let json = fixture!("serial-0.json");
        let sig = fixture!("serial-0.json.minisig");
        assert_eq!(code(check(json, sig)), "manifest_rollback");
    }

    #[test]
    fn check_serial_uses_the_higher_floor() {
        let err = check_serial(2, Some(3)).expect_err("lower than last seen");
        assert_eq!(err.code(), "manifest_rollback");
        assert_eq!(
            err,
            ManifestError::Rollback {
                serial: 2,
                minimum: 3
            }
        );
        let err = check_serial(MIN_SERIAL - 1, None).expect_err("below MIN_SERIAL");
        assert_eq!(err.code(), "manifest_rollback");
        let err = check_serial(MIN_SERIAL - 1, Some(0)).expect_err("below MIN_SERIAL");
        assert_eq!(err.code(), "manifest_rollback");
        assert!(check_serial(3, Some(3)).is_ok());
        assert!(check_serial(4, Some(3)).is_ok());
        assert!(check_serial(MIN_SERIAL, None).is_ok());
    }

    #[test]
    fn production_key_is_embedded_or_refused() {
        match MANIFEST_PUBLIC_KEY {
            None => {
                let err = Manifest::verify_embedded(VALID.as_bytes(), VALID_SIG, None)
                    .expect_err("nothing verifies without a key");
                assert_eq!(err.code(), "manifest_no_trusted_key");
            }
            Some(line) => {
                // Compare the decoded keys: both from `from_base64`, so the
                // untrusted comment (part of `PartialEq`) is `None` on both.
                let test_line = fixture!("test-key.pub").lines().nth(1).expect("key line");
                let embedded = PublicKey::from_base64(line).expect("the embedded key decodes");
                let test = PublicKey::from_base64(test_line).expect("the test key decodes");
                assert_ne!(
                    embedded, test,
                    "the test key must never be the production key"
                );
                // And by behaviour: a manifest signed by the test key is refused.
                let err = Manifest::verify_embedded(VALID.as_bytes(), VALID_SIG, None)
                    .expect_err("the test key's signature must not verify");
                assert_eq!(err.code(), "manifest_bad_signature");
            }
        }
    }

    #[test]
    fn duplicate_platform_key_is_invalid() {
        let file = r#"{"name": "phar", "urls": ["https://getcomposer.org/composer.phar"],
            "sha256": "2e17f6527a6c92f5713ba16e5dfd1b55869ab5e98de46677e994fed5536316ba",
            "size": 10}"#;
        let json = format!(
            r#"{{"schema": 1, "serial": 1, "runtimes": {{"composer": [{{"version": "2.8.12",
                "platforms": {{"any": [{file}], "any": [{file}]}}}}]}}}}"#
        );
        let reason = reason(parse_verified(json.as_bytes(), None));
        assert!(reason.contains("platform is listed twice"), "{reason}");
    }

    /// Caddy is read with the same rules as the other runtimes, under its own
    /// key, within schema 1.
    #[test]
    fn caddy_releases_are_read_under_their_own_key() {
        let file = |name: &str| {
            format!(
                r#"{{"name": "{name}", "urls": ["https://github.com/caddyserver/caddy/releases/download/v2.11.8/caddy_2.11.8_mac_arm64.tar.gz"],
                "sha256": "2e17f6527a6c92f5713ba16e5dfd1b55869ab5e98de46677e994fed5536316ba",
                "size": 10}}"#
            )
        };
        let doc = |files: &str| {
            format!(
                r#"{{"schema": 1, "serial": 1, "runtimes": {{"caddy": [{{"version": "2.11.8",
                "platforms": {{"darwin-arm64": [{files}]}}}}]}}}}"#
            )
        };
        let manifest =
            parse_verified(doc(&file("archive")).as_bytes(), None).expect("caddy release");
        let [caddy] = manifest.releases(Runtime::Caddy) else {
            panic!("one caddy release expected");
        };
        assert_eq!(caddy.version(), &Version::new(2, 11, 8));
        let mac = caddy.platform(Platform::DarwinArm64).expect("darwin-arm64");
        assert_eq!(mac[0].name(), "archive");
        assert!(manifest.releases(Runtime::Composer).is_empty());

        let reason = reason(parse_verified(doc(&file("A")).as_bytes(), None));
        assert!(reason.starts_with("caddy 2.11.8 darwin-arm64"), "{reason}");
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
        let manifest = parse_verified(doc("1", &good).as_bytes(), None).expect("good content");
        assert!(manifest.releases(Runtime::Php).is_empty());
        assert!(manifest.releases(Runtime::Caddy).is_empty());
        assert_eq!(manifest.releases(Runtime::Composer).len(), 1);
        let max = file("my-file_1.0", &one_url, MAX_ARTIFACT_SIZE);
        parse_verified(doc("1", &max).as_bytes(), None).expect("size at the limit, name rules");

        let refused = [
            doc("0", &good),
            doc("1", ""),
            doc("1", &file("phar", &one_url, 0)),
            doc("1", &file("phar", &one_url, MAX_ARTIFACT_SIZE + 1)),
            doc("1", &file("phar", "[]", 10)),
            doc("1", &file("", &one_url, 10)),
            doc("1", &file("PHAR", &one_url, 10)),
            doc("1", &file("a/b", &one_url, 10)),
            doc("1", &file("a b", &one_url, 10)),
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
            r#"{"schema": 1, "serial": 1, "serial": 2, "runtimes": {}}"#.to_string(),
        ];
        for json in refused {
            let err = parse_verified(json.as_bytes(), None).expect_err(&json);
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
                    minimum: 2,
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
