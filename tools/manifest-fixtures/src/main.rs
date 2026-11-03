//! Makes the signed test fixtures for `marsa_core::installer::manifest`.
//!
//! Every run makes a NEW test key pair, in memory only: the secret keys are
//! never written to disk and are gone when the program exits. So every run
//! rewrites `test-key.pub` and every `.minisig` together.
//!
//! Usage: `cargo run --manifest-path tools/manifest-fixtures/Cargo.toml [dir]`
//! (the default dir is `crates/core/tests/fixtures/manifest`).

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signer, SigningKey};
use minisign::{KeyPair, PublicKey, SignatureBox};

/// Every JSON fixture. Each is signed by key A with the trusted comment
/// `marsa runtimes manifest serial=N`, the form the owner uses.
const SIGNED: [&str; 10] = [
    "valid",
    "invalid-json",
    "http-url",
    "sha256-uppercase",
    "sha256-short",
    "schema-2",
    "unknown-platform",
    "bad-version",
    "duplicate-version",
    "serial-0",
];

fn main() {
    let dir = std::env::args().nth(1).map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/core/tests/fixtures/manifest"),
        PathBuf::from,
    );
    let a = KeyPair::generate_unencrypted_keypair().expect("make key A");
    let b = KeyPair::generate_unencrypted_keypair().expect("make key B");

    let id = key_id(&a.pk);
    let public = format!(
        "untrusted comment: marsa TEST key {id}, never trusted in production\n{}\n",
        a.pk.to_base64()
    );
    write(&dir, "test-key.pub", &public);

    for name in SIGNED {
        let file = format!("{name}.json");
        let data = fs::read(dir.join(&file)).expect("read a JSON fixture");
        let signature = sign(&a, &data, &comment(&data));
        assert!(
            verify(&a.pk, &signature, &data, false),
            "{file} must verify"
        );
        write(&dir, &format!("{file}.minisig"), &signature);
    }

    let valid = fs::read(dir.join("valid.json")).expect("read valid.json");
    let good_comment = comment(&valid);

    // Legacy: the right key, but the raw bytes are signed, not their hash.
    let legacy = sign_legacy(&a, &valid, &good_comment);
    assert!(
        verify(&a.pk, &legacy, &valid, true),
        "legacy verifies when allowed"
    );
    assert!(
        !verify(&a.pk, &legacy, &valid, false),
        "legacy fails when refused"
    );
    write(&dir, "valid.json.legacy.minisig", &legacy);

    // Another key: a good signature, but by key B.
    let other = sign(&b, &valid, &good_comment);
    assert!(
        !verify(&a.pk, &other, &valid, false),
        "key B must fail with key A"
    );
    write(&dir, "valid.json.other-key.minisig", &other);

    // Key B's signature with key A's id written in, so a verifier gets past
    // the key id check and must refuse it at the Ed25519 check.
    let forged = forge_key_id(&other, a.pk.keynum());
    assert!(
        !verify(&a.pk, &forged, &valid, false),
        "forged id must fail"
    );
    write(&dir, "valid.json.other-key-same-id.minisig", &forged);

    // A good signature by key A, with the trusted comment the minisign CLI
    // writes by default instead of ours.
    let wrong = sign(&a, &valid, "timestamp:1762257600\tfile:valid.json\thashed");
    assert!(
        verify(&a.pk, &wrong, &valid, false),
        "wrong comment still verifies"
    );
    write(&dir, "valid.json.wrong-comment.minisig", &wrong);

    println!(
        "signed the fixtures in {} with test key {id}",
        dir.display()
    );
}

/// `marsa runtimes manifest serial=N`, with N read from `"serial": N` in the
/// text (also for the fixture that is not valid JSON).
fn comment(data: &[u8]) -> String {
    let text = std::str::from_utf8(data).expect("fixtures are UTF-8");
    let (_, rest) = text
        .split_once("\"serial\":")
        .expect("each fixture has a serial");
    let serial: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    assert!(!serial.is_empty(), "serial is a number");
    format!("marsa runtimes manifest serial={serial}")
}

/// The key id as minisign prints it: the 8 id bytes, last byte first.
fn key_id(pk: &PublicKey) -> String {
    pk.keynum()
        .iter()
        .rev()
        .map(|b| format!("{b:02X}"))
        .collect()
}

fn sign(kp: &KeyPair, data: &[u8], trusted_comment: &str) -> String {
    minisign::sign(
        Some(&kp.pk),
        &kp.sk,
        Cursor::new(data),
        Some(trusted_comment),
        Some("marsa test signature"),
    )
    .expect("sign")
    .into_string()
}

/// minisign's own verifier, a different implementation from the
/// `minisign-verify` crate that Marsa uses.
fn verify(pk: &PublicKey, signature: &str, data: &[u8], allow_legacy: bool) -> bool {
    let signature = SignatureBox::from_string(signature).expect("parse the signature");
    minisign::verify(pk, &signature, Cursor::new(data), true, false, allow_legacy).is_ok()
}

/// A legacy signature (algorithm `Ed`): Ed25519 over the raw bytes, then the
/// global signature over `signature || trusted comment`.
fn sign_legacy(kp: &KeyPair, data: &[u8], trusted_comment: &str) -> String {
    // `SecretKey::to_bytes` layout: sig_alg (2), kdf_alg (2), chk_alg (2),
    // salt (32), opslimit (8), memlimit (8), key id (8), seed (32), public (32),
    // checksum (32). The key is unencrypted, so the seed is in clear.
    let bytes = kp.sk.to_bytes();
    let key_id = &bytes[54..62];
    let seed: [u8; 32] = bytes[62..94].try_into().expect("32-byte seed");
    let signer = SigningKey::from_bytes(&seed);
    assert_eq!(
        signer.verifying_key().to_bytes()[..],
        bytes[94..126],
        "layout"
    );

    let signature = signer.sign(data).to_bytes();
    let mut line = b"Ed".to_vec();
    line.extend_from_slice(key_id);
    line.extend_from_slice(&signature);
    let mut global = signature.to_vec();
    global.extend_from_slice(trusted_comment.as_bytes());
    let global = signer.sign(&global).to_bytes();
    format!(
        "untrusted comment: marsa test signature, legacy\n{}\ntrusted comment: {trusted_comment}\n{}\n",
        BASE64.encode(line),
        BASE64.encode(global)
    )
}

/// Writes `key_id` into bytes 2..10 of the signature line. The global
/// signature does not cover the key id, so nothing else changes.
fn forge_key_id(signature: &str, key_id: &[u8]) -> String {
    let mut lines: Vec<String> = signature.lines().map(str::to_string).collect();
    let mut line = BASE64.decode(&lines[1]).expect("base64 signature line");
    line[2..10].copy_from_slice(key_id);
    lines[1] = BASE64.encode(line);
    lines.join("\n") + "\n"
}

fn write(dir: &Path, name: &str, text: &str) {
    fs::write(dir.join(name), text).expect("write a fixture");
}
