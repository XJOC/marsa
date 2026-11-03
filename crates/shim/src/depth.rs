//! The recursion guard. Each `exec` adds one to `MARSA_SHIM_DEPTH`, so a link
//! back to the shim inside `runtimes/` stops with an error instead of looping.

use std::ffi::OsStr;

use crate::error::ShimError;

/// The environment variable that counts the shims in the current chain.
pub const VAR: &str = "MARSA_SHIM_DEPTH";

/// Real chains are short: `npm run dev` → vite → `php artisan` → `composer` →
/// `php` is about five steps.
pub const LIMIT: u32 = 16;

/// Reads the variable. Missing or not a number counts as 0.
pub fn parse(value: Option<&OsStr>) -> u32 {
    value
        .and_then(OsStr::to_str)
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

/// # Errors
///
/// `shim_loop` once `depth` reaches [`LIMIT`].
pub fn check(depth: u32) -> Result<(), ShimError> {
    if depth >= LIMIT {
        Err(ShimError::Loop { depth })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_values() {
        assert_eq!(parse(None), 0);
        assert_eq!(parse(Some(OsStr::new("3"))), 3);
        assert_eq!(parse(Some(OsStr::new("abc"))), 0);
        assert_eq!(parse(Some(OsStr::new("-1"))), 0);
        assert_eq!(parse(Some(OsStr::new(""))), 0);
    }

    #[test]
    fn the_limit_stops_the_chain() {
        assert!(check(0).is_ok());
        assert!(check(LIMIT - 1).is_ok());
        let err = check(LIMIT).expect_err("the limit is reached");
        assert_eq!(err.code(), "shim_loop");
    }
}
