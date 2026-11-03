//! Code shared by the S-09 programs.
//!
//! - [`data`]: the deterministic content (table rows, SQL text, log lines).
//!   egui and Tauri show exactly the same values, generated in Rust.
//! - [`bench`]: the `--bench` helpers (start clock, `PHASE` lines, frame
//!   statistics). Both UI programs use the same code, so their numbers mean
//!   the same thing.

pub mod bench;
pub mod data;
