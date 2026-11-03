//! Install plans: verify the manifest signature, download to a temp folder,
//! verify the checksum, extract, then move atomically into place.
//!
//! One submodule per step, so the M2 tasks can work on them in parallel. The
//! recipes they read live in [`crate::recipes`].

pub mod execute;
pub mod extract;
pub mod fetch;
pub mod manifest;
pub mod plan;
