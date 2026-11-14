//! Linux: runs `marsa-helper` as root through `pkexec` (ADR 0014, task
//! M4-18e). The macOS twin is the `osascript` line of ADR 0012.
//!
//! `pkexec` asks the desktop's polkit agent for the user's password, then
//! runs the program as root. The program, `apply`, and the request go as an
//! argument list: there is no shell anywhere, so nothing is quoted (spike
//! S-02: a request with quotes, `$`, and backticks, and one of 64 KiB, arrive
//! unchanged).
//!
//! `pkexec` passes the helper's stdout and its exit code through, so the
//! helper's JSON line is read from stdout, whatever the exit code. Without a
//! JSON line, the exit code is `pkexec`'s own (the `pkexec` manual; the
//! helper exits with 0, 1, or 2 only):
//!
//! | Exit | Meaning | Code |
//! | --- | --- | --- |
//! | 126 | the user dismissed the password dialog | `setup_cancelled` |
//! | 127 | not authorized: a wrong password, no polkit agent (over SSH, no desktop), or the policy said no | `setup_not_authorized` |
//! | other | – | `setup_helper_failed` |

use std::ffi::OsString;
use std::io;
use std::path::Path;

use marsa_core::helper::OpResult;
use marsa_core::types::ApiError;

use super::{MAX_ANSWER, RunOutput, failure, not_finished, parse_answer, results_of};

/// `pkexec` itself, from polkit. An absolute path: `marsad` never searches
/// `PATH`.
pub const PKEXEC: &str = "/usr/bin/pkexec";
/// `pkexec`'s exit code when the user dismissed the dialog.
const DISMISSED: i32 = 126;
/// `pkexec`'s exit code when it did not authorize the call, or had no way
/// to ask.
const NOT_AUTHORIZED: i32 = 127;

/// The arguments of `pkexec`: the helper's absolute path, `apply`, and the
/// request, each as one argument.
pub fn args(helper: &Path, request: &str) -> Vec<OsString> {
    vec![
        helper.as_os_str().to_owned(),
        OsString::from("apply"),
        OsString::from(request),
    ]
}

/// The helper's results from what `pkexec` gave back.
///
/// A message never quotes what the programs printed: only its size and the
/// exit code go to the log.
pub fn read_answer(output: io::Result<RunOutput>) -> Result<Vec<OpResult>, ApiError> {
    let output = output.map_err(|err| not_finished(&err, "pkexec"))?;
    let answer = if output.stdout.len() > MAX_ANSWER {
        None
    } else {
        parse_answer(output.stdout.trim_ascii())
    };
    let Some(answer) = answer else {
        return Err(match output.code {
            Some(DISMISSED) => {
                tracing::info!("the password dialog was dismissed");
                failure("setup_cancelled", "the password dialog was dismissed")
            }
            Some(NOT_AUTHORIZED) => {
                tracing::info!("pkexec did not give admin rights");
                failure(
                    "setup_not_authorized",
                    "Linux did not give admin rights: a wrong password, no password dialog \
                     (as over SSH), or a policy that refused",
                )
            }
            code => {
                tracing::warn!(
                    exit = ?code,
                    stdout_bytes = output.stdout.len(),
                    stderr_bytes = output.stderr.len(),
                    "marsa-helper gave no answer Marsa can read"
                );
                failure("setup_helper_failed", "the helper's answer cannot be read")
            }
        });
    };
    results_of(answer, output.code)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DONE: &str = include_str!("../../../../../fixtures/helper/responses/done.json");
    const REJECTED: &str = include_str!("../../../../../fixtures/helper/responses/rejected.json");

    fn exited(code: Option<i32>, stdout: &str, stderr: &str) -> io::Result<RunOutput> {
        Ok(RunOutput {
            code,
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    fn code_of(result: Result<Vec<OpResult>, ApiError>) -> String {
        result.unwrap_err().code
    }

    #[test]
    fn the_arguments_are_the_helper_apply_and_the_request() {
        let request = r#"{"version":1,"ops":[{"op":"ports_install"}]} 'a' $HOME `id`"#;
        let args = args(
            Path::new("/home/u/.local/share/marsa/bin/marsa-helper"),
            request,
        );
        assert_eq!(
            args,
            [
                "/home/u/.local/share/marsa/bin/marsa-helper",
                "apply",
                request
            ]
            .map(OsString::from)
        );
        assert_eq!(PKEXEC, "/usr/bin/pkexec");
    }

    /// The helper's line comes on stdout with each of its exit codes: 0 all
    /// done, 1 an op failed.
    #[test]
    fn the_answer_is_read_from_stdout_with_any_exit_code() {
        let line = serde_json::from_str::<serde_json::Value>(DONE)
            .unwrap()
            .to_string();
        for code in [0, 1] {
            let results = read_answer(exited(Some(code), &line, "")).unwrap();
            assert_eq!(results.len(), 2, "{code}");
        }
    }

    #[test]
    fn a_refused_request_is_setup_helper_rejected() {
        let line = serde_json::from_str::<serde_json::Value>(REJECTED)
            .unwrap()
            .to_string();
        let err = read_answer(exited(Some(2), &line, "")).unwrap_err();
        assert_eq!(err.code, "setup_helper_rejected");
        assert_eq!(
            err.message,
            "the helper refused the request: request_invalid"
        );
    }

    /// What `pkexec` prints is never in the message.
    #[test]
    fn pkexec_exit_codes_without_an_answer() {
        let stderr = "Error executing command as another user: Not authorized\nsecret";
        let cases = [
            (Some(126), "setup_cancelled"),
            (Some(127), "setup_not_authorized"),
            (Some(1), "setup_helper_failed"),
            (None, "setup_helper_failed"),
        ];
        for (code, expected) in cases {
            let err = read_answer(exited(code, "", stderr)).unwrap_err();
            assert_eq!(err.code, expected, "{code:?}");
            assert!(!err.message.contains("secret"), "{}", err.message);
        }
    }

    /// A helper line wins over the exit code, and a broken one does not.
    #[test]
    fn only_a_whole_answer_counts() {
        let half = r#"{"version":1,"results":[{"op":"ports_install""#;
        assert_eq!(
            code_of(read_answer(exited(Some(126), half, ""))),
            "setup_cancelled"
        );
        let other = r#"{"version":2,"results":[{"op":"ports_install","status":"done"}]}"#;
        assert_eq!(
            code_of(read_answer(exited(Some(0), other, ""))),
            "setup_helper_failed"
        );
        let large = format!("{}{}", " ".repeat(MAX_ANSWER), DONE);
        assert_eq!(
            code_of(read_answer(exited(Some(0), &large, ""))),
            "setup_helper_failed"
        );
        let empty = r#"{"version":1,"results":[]}"#;
        assert_eq!(
            code_of(read_answer(exited(Some(0), empty, ""))),
            "setup_helper_failed"
        );
    }

    #[test]
    fn a_timeout_a_stopping_daemon_and_a_missing_pkexec() {
        let timed_out = Err(io::Error::new(io::ErrorKind::TimedOut, "x"));
        assert_eq!(code_of(read_answer(timed_out)), "setup_timed_out");
        let stopping = Err(io::Error::new(io::ErrorKind::Interrupted, "x"));
        assert_eq!(code_of(read_answer(stopping)), "setup_interrupted");
        let missing = Err(io::Error::from(io::ErrorKind::NotFound));
        let err = read_answer(missing).unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.message.as_str()),
            ("setup_helper_failed", "cannot run pkexec")
        );
    }
}
