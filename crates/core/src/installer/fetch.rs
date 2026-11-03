//! Downloads a file to a temp folder over HTTPS only, with a size limit, and
//! computes its sha256 while writing. The result is a `VerifiedFile`, which
//! exists only when the sha256 matches the signed manifest.
//!
//! Filled in by task M2-04 (`sensitive`).
