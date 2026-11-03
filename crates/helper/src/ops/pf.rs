//! macOS: the `pf` redirect of ports 80 and 443 to Caddy (ADR 0008).
//!
//! Stubs until task M4-08 writes them.

use super::not_implemented;
use crate::response::OpResult;
use crate::system::System;

/// `pf_install`: redirect port 80 to `http_port` and 443 to `https_port`.
pub fn install(_http_port: u16, _https_port: u16, _system: &impl System) -> OpResult {
    not_implemented("pf_install")
}

/// `pf_remove`: remove the redirect.
pub fn remove(_system: &impl System) -> OpResult {
    not_implemented("pf_remove")
}
