# Manifest test fixtures

Signed files for the tests in `crates/core/src/installer/manifest.rs` (task M2-02).
The repository has no `minisign` signing crate, so these were signed once and committed.

**`test-key.pub` is a test key.** It must never become the production key in
`MANIFEST_PUBLIC_KEY`. A test checks this.

## Files

| File | What it is |
| --- | --- |
| `test-key.pub` | Public key A (key id `74F999C2E825FB87`). Every test verifies with it. |
| `valid.json` + `.minisig` | A good manifest, signed by key A (prehashed, the minisign default). |
| `valid.json.legacy.minisig` | A legacy (not prehashed, algorithm `Ed`) signature of `valid.json` by key A. |
| `valid.json.other-key.minisig` | A signature of `valid.json` by key B. |
| `valid.json.other-key-same-id.minisig` | Key B's signature with key A's key id written in, so it reaches the Ed25519 check. |
| `invalid-json.json` + `.minisig` | Broken JSON, signed by key A. |
| `http-url.json`, `sha256-uppercase.json`, `sha256-short.json`, `unknown-platform.json`, `bad-version.json`, `duplicate-version.json`, `schema-2.json` + `.minisig` | Each breaks one rule, each signed by key A. |

The sha256 values in the JSON files are fake (`sha256("marsa-fixture-<name>")`).

## How they were made (2026-11-04)

1. A throwaway Cargo project outside the repository, with the `minisign` 0.10,
   `ed25519-dalek` 2, and `base64` crates.
2. It made two key pairs in memory with `KeyPair::generate_unencrypted_keypair()`.
   The secret keys were never written to disk and were gone when the program exited.
3. It wrote `test-key.pub` (key A) and signed each `*.json` here with
   `minisign::sign` (prehashed) and key A.
4. Legacy: `minisign` 0.10 cannot make legacy signatures, so it was built by hand
   from key A's Ed25519 seed: `"Ed" || key id || ed25519(sk, bytes)`, then the
   global signature `ed25519(sk, signature || trusted comment)`.
5. Other key: `minisign::sign` with key B. The same-id variant replaces bytes
   2..10 of the decoded second line with key A's id.
6. Each output was checked with `minisign::verify` (an independent
   implementation) before writing: good signatures pass; the legacy one passes only
   with `allow_legacy = true`; key B's signatures fail with key A.
7. The throwaway project was deleted.

To change a fixture, make all of them again the same way: a new key pair means
every `.minisig` and `test-key.pub` change together. Never commit a secret key.

`.gitattributes` here turns off line-ending conversion: one changed byte breaks a signature.
