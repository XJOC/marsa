//! The signed manifest: which versions of each runtime exist, the URLs of
//! their files, and the sha256 and size of each file. Checks the minisign
//! signature with the public key built into the binary, then parses the JSON,
//! and refuses an older manifest than the last one seen.
//!
//! Filled in by task M2-02 (`sensitive`).
