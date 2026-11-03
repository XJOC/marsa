//! Runs an install plan: takes the install lock, downloads and extracts into a
//! temp folder under `runtimes/.tmp/`, runs the recipe's check, then moves the
//! version folder into place atomically.
//!
//! Filled in by task M2-05.
