use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use crate::error::ShimError;

/// Replaces this process with `command`, like `pcntl_exec()` in PHP. No child
/// process stays, so signals, the exit code, and stdin, stdout, and stderr
/// pass through untouched.
///
/// It returns only if `exec` failed, so the return type is the error itself.
pub fn exec(mut command: Command) -> ShimError {
    let source = command.exec();
    ShimError::Exec {
        path: PathBuf::from(command.get_program()),
        source,
    }
}
