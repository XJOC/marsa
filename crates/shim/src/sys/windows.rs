use std::process::Command;

use crate::error::ShimError;

/// Task M1-07 replaces this: a child process that inherits the console,
/// forwards Ctrl+C, and returns the exit code.
pub fn exec(_command: Command) -> ShimError {
    ShimError::UnsupportedPlatform
}
