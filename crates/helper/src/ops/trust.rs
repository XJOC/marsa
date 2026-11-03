//! macOS and Linux: trust of Marsa's CA root certificate.
//!
//! Stubs until task M4-09 writes them.

use super::not_implemented;
use crate::response::OpResult;
use crate::system::System;

/// `trust_install`: trust the certificate in `pem`.
pub fn install(_pem: &str, _system: &impl System) -> OpResult {
    not_implemented("trust_install")
}

/// `trust_remove`: remove the trust.
pub fn remove(_system: &impl System) -> OpResult {
    not_implemented("trust_remove")
}
