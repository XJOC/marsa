//! What `dump()` sends to Marsa: one line of Symfony's dump server protocol,
//! `base64(serialize([$data, $context]))`, read into text (ADR 0027).
//!
//! The functions are pure: bytes in, a value out. They never run PHP, never
//! build objects from the data, and never panic on any input. The listener
//! and the store are the daemon's.

pub mod php_serial;

/// A line over this many bytes is not read. The listener skips it up to the
/// next line break, without keeping it.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Why a line is not a dump.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DumpError {
    #[error("a dump line is over {MAX_LINE_BYTES} bytes")]
    TooLarge,
    /// The reason is for the log, not for the user.
    #[error("not a dump of Symfony's var-dumper: {0}")]
    Invalid(&'static str),
}

impl DumpError {
    /// The stable code the clients keep a text for.
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLarge => crate::codes::DUMP_TOO_LARGE,
            Self::Invalid(_) => crate::codes::DUMP_INVALID,
        }
    }
}
