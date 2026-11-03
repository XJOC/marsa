# Manifest test fixtures

Signed files for the tests in `crates/core/src/installer/manifest.rs` (task M2-02).
The product workspace has no `minisign` signing crate. The signatures are made by
`tools/manifest-fixtures/`, a separate Cargo project that the workspace excludes.

**`test-key.pub` is a test key.** It must never become the production key in
`MANIFEST_PUBLIC_KEY`. A test checks this.

## Regenerate

From the repository root:

```sh
cargo run --manifest-path tools/manifest-fixtures/Cargo.toml
```

Each run makes a new key pair, in memory only. The secret keys are never written
to disk, and they are gone when the program exits. So one run rewrites
`test-key.pub` and every `.minisig` together. The JSON files are written by hand
and stay the same.

Before it writes a file, the generator checks each signature with the `minisign`
crate's own verifier, a different implementation from the `minisign-verify` crate
that Marsa uses.

## Cross-check with the minisign CLI (optional)

```sh
cd crates/core/tests/fixtures/manifest
minisign -V -p test-key.pub -m valid.json -x valid.json.minisig
# Signature and comment signature verified
# Trusted comment: marsa runtimes manifest serial=3
```

These must fail:

- `-x valid.json.other-key.minisig`
- `-x valid.json.other-key-same-id.minisig`

## Files

| File | What it is |
| --- | --- |
| `test-key.pub` | Public key A. Every test verifies with it. |
| `valid.json` + `.minisig` | A good manifest (serial 3), signed by key A (prehashed, the minisign default). |
| `valid.json.legacy.minisig` | A legacy signature of `valid.json` by key A (not prehashed, algorithm `Ed`). `minisign` 0.10 cannot make legacy signatures, so the generator builds it from key A's Ed25519 seed. |
| `valid.json.other-key.minisig` | A signature of `valid.json` by key B. |
| `valid.json.other-key-same-id.minisig` | Key B's signature with key A's key id written in, so it reaches the Ed25519 check. |
| `valid.json.wrong-comment.minisig` | A good signature by key A, but with minisign's default trusted comment instead of `marsa runtimes manifest ...`. |
| `invalid-json.json` + `.minisig` | Broken JSON, signed by key A. |
| `http-url.json`, `sha256-uppercase.json`, `sha256-short.json`, `unknown-platform.json`, `bad-version.json`, `duplicate-version.json`, `schema-2.json`, `serial-0.json` + `.minisig` | Each breaks one rule. Each is signed by key A. |

Every signature by key A, except the wrong-comment one, has the trusted comment
`marsa runtimes manifest serial=N`. N is the serial in the file. This is the form
the owner uses for real manifests.

The sha256 values in the JSON files are fake (`sha256("marsa-fixture-<name>")`).

Never commit a secret key.

`.gitattributes` here turns off line-ending conversion. One changed byte breaks a
signature.
