//! The administrator prompt on macOS: `/usr/bin/osascript` runs the constant
//! script of ADR 0012, and what it prints or fails with is read back into
//! the helper's results or into an error with its own code (cancelled, not
//! authorized, an OS error number).

use super::Flow;
use super::Reason;
use super::failure;
use super::helper::MAX_ANSWER;
use super::helper::braces;
use super::helper::parse_answer;
use super::helper::results_of;
use super::runner::RunOutput;
use marsa_core::helper::OpResult;
use marsa_core::types::ApiError;
use std::ffi::OsString;
use std::io;
use std::path::Path;

pub(super) const OSASCRIPT: &str = "/usr/bin/osascript";

/// The AppleScript of ADR 0012, one `-e` argument per line. A constant:
/// nothing is ever put into it. The helper path and the request travel as
/// `argv` items 1 and 2, and `quoted form of` hands each to `/bin/sh` as one
/// literal argument. This is the only shell line Marsa runs (AGENTS.md
/// section 6).
const APPLESCRIPT: [&str; 3] = [
    "on run argv",
    "do shell script (quoted form of item 1 of argv) & \" apply \" & (quoted form of item 2 of argv) with administrator privileges",
    "end run",
];

/// The `osascript` arguments of ADR 0012: the constant script, then the
/// helper path and the request as `argv` items 1 and 2.
pub(super) fn osascript_args(helper: &Path, request: &str) -> Vec<OsString> {
    let mut args = Vec::with_capacity(APPLESCRIPT.len() * 2 + 2);
    for line in APPLESCRIPT {
        args.push(OsString::from("-e"));
        args.push(OsString::from(line));
    }
    args.push(helper.as_os_str().to_owned());
    args.push(OsString::from(request));
    args
}

/// [`read_answer_for`] for setup, as the tests call it.
#[cfg(test)]
pub(super) fn read_answer(output: io::Result<RunOutput>) -> Result<Vec<OpResult>, ApiError> {
    read_answer_for(Flow::Setup, output)
}

/// The helper's results from what `osascript` gave back, with the error
/// codes of `flow`.
///
/// - Exit 0: stdout is the helper's JSON line; `do shell script` returns it.
/// - Another exit: the helper exited with 1 (an operation failed) or 2 (it
///   refused the request), and `do shell script` turned that into an
///   AppleScript error whose text is the helper's stdout. `osascript` prints
///   it on stderr as `<at>: execution error: <JSON> (<exit>)`, so the JSON
///   is read from there.
/// - A cancelled prompt is AppleScript error -128:
///   `execution error: User canceled. (-128)`.
/// - A wrong password, or a prompt that cannot show (over SSH), is error
///   -60005 or -60007 of the Authorization Services.
/// - Any other negative error number (`-10810`, `-1743`, ...) is
///   `osascript` failing before the helper ran (task M5-20), for example
///   when the daemon runs where macOS's prompt cannot reach it: a sandbox it
///   inherited from the program that started it, or a locked or other
///   user's session. `<flow>_prompt_failed`, with the number.
///
/// When Marsa stops waiting (the limit, or the daemon stopping), `osascript`
/// is killed, but the helper may already run as root. Every helper operation
/// can run again safely, so the message says so.
///
/// A message never quotes what the programs printed. The log gets a short
/// excerpt of `osascript`'s error text ([`excerpt`]), without control
/// characters: the request holds op names, ports, and paths, nothing secret,
/// and the password goes to macOS's prompt, never to this text.
pub(super) fn read_answer_for(
    flow: Flow,
    output: io::Result<RunOutput>,
) -> Result<Vec<OpResult>, ApiError> {
    let output = output.map_err(|err| not_finished(flow, &err, "osascript"))?;
    let text = if output.success() {
        &output.stdout
    } else {
        &output.stderr
    };
    let answer = if text.len() > MAX_ANSWER {
        None
    } else if output.success() {
        parse_answer(text.trim_ascii())
    } else {
        braces(text).and_then(parse_answer)
    };
    let Some(answer) = answer else {
        if !output.success() && cancelled(&output.stderr) {
            tracing::info!("the admin prompt was cancelled");
            let message = "the admin prompt was cancelled";
            return Err(failure(flow.code(Reason::Cancelled), message));
        }
        if !output.success() && not_authorized(&output.stderr) {
            tracing::info!("macOS did not give admin rights");
            let message = "macOS did not give admin rights: a wrong password, or no prompt";
            return Err(failure(flow.code(Reason::NotAuthorized), message));
        }
        if !output.success()
            && let Some(number) = os_error(&output.stderr)
        {
            tracing::warn!(
                error = number,
                stderr = %excerpt(&output.stderr),
                "osascript failed before marsa-helper ran"
            );
            let message = format!(
                "macOS could not run the admin prompt (error {number}), so marsa-helper did not \
                 run; running it again is safe"
            );
            return Err(failure(flow.code(Reason::PromptFailed), &message));
        }
        tracing::warn!(
            exit = ?output.code,
            stdout_bytes = output.stdout.len(),
            stderr_bytes = output.stderr.len(),
            stderr = %excerpt(&output.stderr),
            "marsa-helper gave no answer Marsa can read"
        );
        return Err(failure(
            flow.code(Reason::HelperFailed),
            "the helper's answer cannot be read",
        ));
    };
    results_of(flow, answer, output.code)
}

/// Why `flow` stopped waiting for `program`, the program that asks for the
/// password and runs the helper: a timeout, a stopping daemon, or a program
/// that could not run.
pub(super) fn not_finished(flow: Flow, err: &io::Error, program: &str) -> ApiError {
    let noun = flow.noun();
    match err.kind() {
        io::ErrorKind::TimedOut => {
            tracing::warn!("{noun} took more than 5 minutes; Marsa stopped waiting");
            let message = format!(
                "Marsa stopped waiting for {noun} after 5 minutes; some steps may already have \
                 run as root, and running {noun} again is safe"
            );
            failure(flow.code(Reason::TimedOut), message)
        }
        io::ErrorKind::Interrupted => {
            tracing::warn!("the daemon is stopping; Marsa stopped waiting for {noun}");
            let message = format!(
                "the daemon stopped, so Marsa stopped waiting for {noun}; some steps may \
                 already have run as root, and running {noun} again is safe"
            );
            failure(flow.code(Reason::Interrupted), message)
        }
        _ => {
            tracing::warn!(error = %err, program, "cannot run the program that asks for the password");
            failure(
                flow.code(Reason::HelperFailed),
                format!("cannot run {program}"),
            )
        }
    }
}

/// AppleScript error -128. The number is the same in every language; the
/// English text is checked too, in case the number is cut.
pub(super) fn cancelled(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr);
    text.contains("(-128)") || text.contains("User canceled")
}

/// `errAuthorizationDenied` (-60005: a wrong password, or not an admin) and
/// `errAuthorizationInteractionNotAllowed` (-60007: the prompt cannot show,
/// as over SSH). Only the numbers: the text depends on the language.
pub(super) fn not_authorized(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr);
    text.contains("(-60005)") || text.contains("(-60007)")
}

/// The error number at the end of `osascript`'s error line, `(-10810)`, when
/// it is negative: an AppleScript or system error, so `do shell script`
/// never ran the helper. The helper's own exit codes (1, 2, 126, 127) and a
/// helper killed by a signal (1000 plus its number) are positive and stay
/// `helper_failed`.
fn os_error(stderr: &[u8]) -> Option<i64> {
    let text = String::from_utf8_lossy(stderr);
    let inside = text.trim_end().strip_suffix(')')?;
    let number: i64 = inside.rsplit_once('(')?.1.parse().ok()?;
    (number < 0).then_some(number)
}

/// Up to [`EXCERPT_CHARS`] characters of `text` for the log, with control
/// characters (line breaks, escapes) turned into spaces.
fn excerpt(text: &[u8]) -> String {
    String::from_utf8_lossy(text)
        .trim()
        .chars()
        // Also the Unicode line and paragraph separators: one log line stays
        // one line.
        .map(|c| {
            if c.is_control() || (c.is_whitespace() && c != ' ') {
                ' '
            } else {
                c
            }
        })
        .take(EXCERPT_CHARS)
        .collect()
}

/// How much of `osascript`'s error text goes to the log.
const EXCERPT_CHARS: usize = 300;

#[cfg(test)]
mod tests {
    use super::super::helper::MAX_ANSWER;
    use super::super::helper::op_name;
    use super::super::helper::results_match;
    use super::super::ports::NETSTAT;
    use super::super::runner::RunOutput;

    use super::super::steps::UNINSTALL_ALL;
    use super::super::test_support::*;

    use super::super::*;
    use super::*;

    use marsa_core::helper::HelperOp;
    use marsa_core::helper::OpResult;

    use marsa_core::types::OpStatus;
    use serde_json::{Value, json};

    use std::io;
    use std::path::Path;

    use std::time::Duration;

    /// ADR 0012, written out here and not taken from `APPLESCRIPT`, so a
    /// change to the constant fails this test.
    const SCRIPT: &str = "do shell script (quoted form of item 1 of argv) & \" apply \" & \
                          (quoted form of item 2 of argv) with administrator privileges";

    #[test]
    fn the_osascript_arguments_are_pinned() {
        let dir = Dir::new(false);
        let fake = Fake::scripted(netstat_free(), || ok(DONE));
        dir.run(&fake);

        let calls = fake.calls();
        let osascript = calls.last().unwrap();
        assert_eq!(osascript.program, "/usr/bin/osascript");
        let helper = dir.helper().to_str().unwrap().to_owned();
        let request = osascript.args[7].clone();
        assert_eq!(
            osascript.args,
            [
                "-e",
                "on run argv",
                "-e",
                SCRIPT,
                "-e",
                "end run",
                helper.as_str(),
                request.as_str(),
            ]
        );
        assert!(Path::new(&helper).is_absolute());
        assert_eq!(osascript.limit, Duration::from_secs(300));
        assert_eq!(
            serde_json::from_str::<Value>(&request).unwrap(),
            json!({ "version": 1, "ops": [
                { "op": "resolver_install", "port": 18053 },
                { "op": "pf_install", "http_port": 18080, "https_port": 18443 },
            ] })
        );
    }

    #[test]
    fn a_cancelled_prompt_is_setup_cancelled() {
        for stderr in [
            "0:180: execution error: User canceled. (-128)\n",
            // Another language: the number is the same.
            "0:180: execution error: Vom Benutzer abgebrochen. (-128)\r",
        ] {
            let dir = Dir::new(true);
            let fake = Fake::scripted(netstat_free(), move || script_error(stderr));
            let report = dir.run(&fake);
            assert_eq!(error_code(&report), Some("setup_cancelled"), "{stderr}");
            assert!(report.results.is_empty());
            assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT]);
        }
    }

    #[test]
    fn an_os_error_before_the_helper_is_prompt_failed_with_its_number() {
        // What osascript prints when macOS cannot run the prompt for the
        // daemon (task M5-20), and other negative errors of the same kind.
        let cases = [
            (
                "0:194: execution error: An error of type -10810 has occurred. (-10810)\n",
                -10810,
            ),
            (
                "0:94: execution error: Not authorized to send Apple events. (-1743)",
                -1743,
            ),
            ("12:40: execution error: garbage (-2700)\r\n", -2700),
        ];
        for (stderr, number) in cases {
            let error = read_answer(script_error(stderr)).expect_err(stderr);
            assert_eq!(error.code, "setup_prompt_failed", "{stderr}");
            assert!(
                error.message.contains(&number.to_string()),
                "{}",
                error.message
            );
            assert!(!error.message.contains("garbage"), "{}", error.message);
            let error = read_answer_for(Flow::Uninstall, script_error(stderr)).expect_err(stderr);
            assert_eq!(error.code, "uninstall_prompt_failed", "{stderr}");
        }
        // The known ones keep their own codes, and a positive exit of the
        // shell (the helper or sh itself) stays helper_failed.
        let cancelled = script_error("0:180: execution error: User canceled. (-128)\n");
        assert_eq!(read_answer(cancelled).unwrap_err().code, "setup_cancelled");
        let denied = script_error("0:9: execution error: The password was wrong. (-60005)");
        assert_eq!(
            read_answer(denied).unwrap_err().code,
            "setup_not_authorized"
        );
        let shell = script_error("0:94: execution error: sh: x: Permission denied (126)");
        assert_eq!(read_answer(shell).unwrap_err().code, "setup_helper_failed");
    }

    #[test]
    fn the_log_excerpt_is_short_and_has_no_control_characters() {
        let text = format!(
            "  0:1: execution error:\x1b[31m bad\r\nline {}  ",
            "x".repeat(400)
        );
        let got = excerpt(text.as_bytes());
        assert!(got.starts_with("0:1: execution error:"), "{got}");
        assert!(!got.chars().any(char::is_control), "{got}");
        assert_eq!(got.chars().count(), EXCERPT_CHARS);
        assert_eq!(os_error(b"no number here"), None);
        assert_eq!(os_error(b"execution error: x (1)"), None);
        assert_eq!(os_error(b"execution error: x (-10810)\n"), Some(-10810));
        // A helper killed by a signal (1000 plus its number), text after the
        // number, and a broken number are not an OS error.
        assert_eq!(os_error(b"execution error: due to a signal. (1006)"), None);
        assert_eq!(os_error(b"execution error: x (-10810) and more"), None);
        assert_eq!(os_error(b"execution error: x (--5)"), None);
        assert!(!excerpt("a\u{2028}b\u{2029}c".as_bytes()).contains('\u{2028}'));
    }

    #[test]
    fn an_answer_that_cannot_be_read_is_setup_helper_failed() {
        let cases: Vec<(&str, io::Result<RunOutput>)> = vec![
            ("garbage", ok("garbage from the helper")),
            ("empty", ok("")),
            ("newer protocol", ok(r#"{"version":2,"results":[]}"#)),
            ("no result", ok(r#"{"version":1}"#)),
            (
                "too large",
                ok(&format!("{}{}", " ".repeat(MAX_ANSWER), DONE)),
            ),
            (
                "shell error",
                script_error("0:94: execution error: sh: garbage: Permission denied (126)"),
            ),
            (
                "osascript missing",
                Err(io::Error::from(io::ErrorKind::NotFound)),
            ),
        ];
        for (name, output) in cases {
            let answer = read_answer(output);
            let error = answer.expect_err(name);
            assert_eq!(error.code, "setup_helper_failed", "{name}");
            // Nothing the programs printed is copied into the message.
            assert!(
                !error.message.contains("garbage"),
                "{name}: {}",
                error.message
            );
        }
    }

    #[test]
    fn a_failed_op_comes_back_through_the_script_error() {
        // The helper exits 1 when an op fails, so `do shell script` raises
        // an error whose text is the helper's JSON line.
        let stderr = format!("7:83: execution error: {} (1)\n", line(FAILED));
        assert_eq!(read_answer(script_error(&stderr)), Ok(results(FAILED)));
        let failed = &results(FAILED)[1];
        assert_eq!(failed.status, OpStatus::Failed);
        assert_eq!(failed.code.as_deref(), Some("op_not_implemented"));
    }

    #[test]
    fn a_refused_request_is_setup_helper_rejected() {
        let stderr = format!("7:83: execution error: {} (2)\n", line(REJECTED));
        let error = read_answer(script_error(&stderr)).unwrap_err();
        assert_eq!(error.code, "setup_helper_rejected");
        assert_eq!(
            error.message,
            "the helper refused the request: request_invalid"
        );
    }

    #[test]
    fn a_prompt_left_open_or_a_stopping_daemon_ends_setup() {
        let cases = [
            (io::ErrorKind::TimedOut, "setup_timed_out"),
            (io::ErrorKind::Interrupted, "setup_interrupted"),
        ];
        for (kind, code) in cases {
            let error = read_answer(Err(io::Error::from(kind))).unwrap_err();
            assert_eq!(error.code, code);
            // The helper may have run as root already, and a rerun is safe.
            for part in [
                "stopped waiting",
                "may already have run as root",
                "again is safe",
            ] {
                assert!(error.message.contains(part), "{code}: {}", error.message);
            }
        }
    }

    #[test]
    fn a_wrong_password_or_no_prompt_is_setup_not_authorized() {
        for stderr in [
            "0:180: execution error: The administrator user name or password was incorrect. \
             (-60005)\n",
            "0:180: execution error: The authorization was denied since no user interaction \
             was possible. (-60007)\n",
        ] {
            let error = read_answer(script_error(stderr)).unwrap_err();
            assert_eq!(error.code, "setup_not_authorized", "{stderr}");
            assert!(!error.message.contains("incorrect"), "{}", error.message);
        }
    }

    #[test]
    fn a_message_with_brackets_quotes_and_accents_survives_the_script_error() {
        // What osascript printed on macOS for a shell that printed this JSON
        // line and exited 1 (run by hand without admin rights).
        let stderr = r#"7:88: execution error: {"version":1,"results":[{"op":"resolver_install","status":"failed","code":"resolver_write","message":"cannot write (x) } \"q\" é"}]} (1)"#;
        let results = read_answer(script_error(stderr)).unwrap();
        assert_eq!(
            results,
            [OpResult {
                op: "resolver_install".into(),
                status: OpStatus::Failed,
                code: Some("resolver_write".into()),
                message: Some("cannot write (x) } \"q\" é".into()),
            }]
        );
    }

    #[test]
    fn results_for_other_operations_are_setup_helper_failed() {
        // No CA: the request is resolver_install, then pf_install.
        let answers = [
            all_done(&["resolver_install"]),
            all_done(&["pf_install", "resolver_install"]),
            all_done(&["resolver_install", "pf_install", "trust_install"]),
            all_done(&["resolver_install", "trust_install"]),
        ];
        for answer in answers {
            let dir = Dir::new(false);
            let sent = answer.clone();
            let fake = Fake::scripted(netstat_free(), move || ok(&sent));
            let report = dir.run(&fake);
            assert_eq!(error_code(&report), Some("setup_helper_failed"), "{answer}");
            assert!(report.results.is_empty(), "{answer}");
        }
        // `uninstall_all` gives one result per removal, so it is not checked.
        let names = [UNINSTALL_ALL.to_owned()];
        assert!(results_match(
            &names,
            &results(&all_done(&["pf_remove", "resolver_remove"]))
        ));
        let ops = [trust(), resolver(), pf(1, 2), HelperOp::UninstallAll];
        let names: Vec<String> = ops.iter().map(|op| op_name(op).unwrap()).collect();
        assert_eq!(
            names,
            [
                "trust_install",
                "resolver_install",
                "pf_install",
                "uninstall_all"
            ]
        );
    }
}
