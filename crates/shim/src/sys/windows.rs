use std::env;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

use crate::error::ShimError;
use crate::name::ShimName;

/// The tool, from the program file that runs (`shims\php.exe`): `argv[0]` is
/// whatever the caller wrote (`php`, `PHP`, or another word), and Windows
/// shims are copies, not links. `argv[0]` only when Windows cannot say.
pub fn own_name(argv0: &OsStr) -> Option<ShimName> {
    match env::current_exe() {
        Ok(path) => ShimName::from_exe_path(&path),
        Err(_) => ShimName::from_exe_path(Path::new(argv0)),
    }
}

/// Task M1-07b replaces this: a child process that inherits the console,
/// forwards Ctrl+C, and returns the exit code.
pub fn exec(_command: Command) -> ShimError {
    ShimError::UnsupportedPlatform
}
