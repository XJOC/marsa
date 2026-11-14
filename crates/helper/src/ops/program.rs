//! What a Linux or Windows op says about a program it ran through
//! [`System::run`] or [`System::run_system_tool`]: whether it succeeded, and
//! how it failed.
//!
//! [`System::run`]: crate::system::System::run
//! [`System::run_system_tool`]: crate::system::System::run_system_tool

use std::io;

use crate::system::RunOutput;

/// The program ran and exited with code 0.
pub fn succeeded(result: &io::Result<RunOutput>) -> bool {
    matches!(result, Ok(output) if output.code == Some(0))
}

/// How the program failed, for a message. Never its output: the exit code
/// is enough for `marsad`, and the owner can run the same command by hand.
/// The same texts as the private `failure_text` of `trust` (macOS). A
/// negative code is a Windows `HRESULT`, in hex as Windows prints it.
pub fn failure(result: &io::Result<RunOutput>) -> String {
    match result {
        Ok(RunOutput {
            code: Some(code), ..
        }) if *code < 0 => format!("exited with code {:#010x}", code.cast_unsigned()),
        Ok(RunOutput {
            code: Some(code), ..
        }) => format!("exited with code {code}"),
        Ok(_) => "was stopped by a signal".to_owned(),
        Err(err) if err.kind() == io::ErrorKind::TimedOut => "timed out".to_owned(),
        Err(err) => format!("could not run: {}", err.kind()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exit(code: Option<i32>) -> io::Result<RunOutput> {
        Ok(RunOutput {
            code,
            stdout: b"secret output".to_vec(),
            stderr: b"secret error".to_vec(),
        })
    }

    #[test]
    fn only_exit_code_0_is_success() {
        assert!(succeeded(&exit(Some(0))));
        assert!(!succeeded(&exit(Some(1))));
        assert!(!succeeded(&exit(None)));
        assert!(!succeeded(&Err(io::ErrorKind::NotFound.into())));
    }

    #[test]
    fn a_failure_names_the_exit_and_never_the_output() {
        assert_eq!(failure(&exit(Some(255))), "exited with code 255");
        let not_found = 0x8009_0011_u32.cast_signed();
        assert_eq!(
            failure(&exit(Some(not_found))),
            "exited with code 0x80090011"
        );
        assert_eq!(failure(&exit(None)), "was stopped by a signal");
        let timeout = Err(io::Error::new(io::ErrorKind::TimedOut, "secret"));
        assert_eq!(failure(&timeout), "timed out");
        let missing = Err(io::Error::new(io::ErrorKind::NotFound, "secret"));
        assert_eq!(failure(&missing), "could not run: entity not found");
    }
}
