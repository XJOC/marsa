//! The OS the helper runs on.
//!
//! The helper does not depend on `marsa-core` (architecture.md section 7), so
//! it cannot use `marsa_core::platform`. This is the only place where the
//! helper's logic asks which OS it runs on. The `#[cfg(unix)]` lines in
//! `system` only pick the std API that exists on each OS.

/// One of the three OSes Marsa supports. Ops take it as a value, so tests can
/// run the macOS, Linux, and Windows paths on any machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Macos,
    Linux,
    Windows,
}

impl Os {
    /// The OS this binary was built for. Any other Unix counts as Linux:
    /// Marsa ships for three OSes only.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    /// The name used in messages.
    pub fn name(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Linux => "linux",
            Self::Windows => "windows",
        }
    }
}
