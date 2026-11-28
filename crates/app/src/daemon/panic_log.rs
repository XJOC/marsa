//! What a panic of `marsad` leaves in `marsad.log`.

use std::panic::{self, PanicHookInfo};
use std::thread;

/// A run of hex digits this long is cut from a panic's message. The token is
/// 64 of them, so less than half of it could ever be written.
const HEX_RUN: usize = 32;

/// Logs every panic at `error`, then runs the hook that was there before. A
/// client starts `marsad` with stderr closed, so without this a panic leaves
/// no trace. Call it once, after logging is set up.
///
/// The hook only watches: the panic unwinds as before, so one in a task ends
/// that task and not the daemon.
pub fn log_panics() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        log(info);
        previous(info);
    }));
}

/// The line holds the panic's place, thread, and message only: no environment,
/// nothing of a request, no backtrace. Those can hold the token, which never
/// reaches the log (architecture.md section 6).
fn log(info: &PanicHookInfo<'_>) {
    let (file, line) = info
        .location()
        .map_or(("unknown", 0), |at| (at.file(), at.line()));
    let message = safe_message(file, info.payload_as_str().unwrap_or("not a text"));
    // A `&str` field is written quoted and escaped: one line, whatever the
    // message holds, so `marsa doctor` shows all of it. The place comes first
    // because `doctor` cuts long lines.
    tracing::error!(
        file,
        line,
        thread = thread::current().name().unwrap_or("unnamed"),
        panic = message.as_str(),
        "panicked"
    );
}

/// The message without what must not be logged. A message can quote the value
/// that broke: slicing a string wrongly prints the string.
fn safe_message(file: &str, message: &str) -> String {
    // Database code works on SQL, cells, and the server's messages, and none
    // of those may reach the log (ADR 0028, section 2).
    if file.contains("db_browser") || file.contains("sqlx") {
        return "hidden: a panic in database code".to_owned();
    }
    hide_long_hex(message)
}

/// `text` with every run of [`HEX_RUN`] or more hex digits replaced.
fn hide_long_hex(text: &str) -> String {
    let other = |c: char| !c.is_ascii_hexdigit();
    // Each piece is a run of hex digits, then at most one other character.
    text.split_inclusive(other)
        .map(|piece| {
            let digits = piece.trim_end_matches(other).len();
            if digits >= HEX_RUN {
                format!("<hidden>{}", &piece[digits..])
            } else {
                piece.to_owned()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use marsa_core::platform::daemon_file::Token;

    use super::*;
    use crate::daemon::services::supervisor::{LOG_FILE_LIMIT, LogFile};

    /// The line of the `panic!` in [`boom`].
    const BOOM_LINE: u32 = line!() + 2;
    async fn boom(message: String) {
        panic!("{message}");
    }

    // The hook belongs to the whole process, so one test installs it. The
    // runtime has one thread: the task panics on this one, where the
    // subscriber is.
    #[tokio::test]
    async fn a_panic_in_a_task_is_logged_and_the_runtime_lives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marsad.log");
        let file = LogFile::open(&path, LOG_FILE_LIMIT).unwrap();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(Mutex::new(file))
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let previous_ran = Arc::new(AtomicBool::new(false));
        let ran = Arc::clone(&previous_ran);
        panic::set_hook(Box::new(move |_| ran.store(true, Ordering::SeqCst)));
        log_panics();

        let token = Token::generate().unwrap().bearer_header();
        let message = format!("first line\nsecond line: {token}");
        let ended = tokio::spawn(boom(message)).await;
        let after = tokio::spawn(async { 7 }).await;
        drop(panic::take_hook());

        assert!(ended.unwrap_err().is_panic());
        assert_eq!(after.unwrap(), 7);
        assert!(previous_ran.load(Ordering::SeqCst));
        let log = fs::read_to_string(&path).unwrap();
        assert_eq!(log.lines().count(), 1, "{log}");
        let thread = thread::current().name().unwrap().to_owned();
        for wanted in [
            "ERROR",
            "panicked",
            &format!("file={:?}", file!()),
            &format!("line={BOOM_LINE}"),
            &format!("thread={thread:?}"),
            r#"panic="first line\nsecond line: Bearer <hidden>""#,
        ] {
            assert!(log.contains(wanted), "{wanted} missing from: {log}");
        }
        let secret = token.strip_prefix("Bearer ").unwrap();
        assert!(!log.contains(secret), "the log shows the token: {log}");
    }

    #[test]
    fn a_long_run_of_hex_digits_is_hidden() {
        let short = "a".repeat(HEX_RUN - 1);
        let long = "0123456789ABCDEFabcdef".repeat(3);
        assert_eq!(
            hide_long_hex(&format!("é{short}é {long}é")),
            format!("é{short}é <hidden>é")
        );
        assert_eq!(hide_long_hex(&"f".repeat(HEX_RUN)), "<hidden>");
        assert_eq!(
            hide_long_hex(&format!("{long}\n{long}")),
            "<hidden>\n<hidden>"
        );
        assert_eq!(hide_long_hex(""), "");
    }

    #[test]
    fn a_panic_in_database_code_does_not_quote_its_value() {
        for file in [
            "crates/app/src/daemon/db_browser/rows.rs",
            r"crates\core\src\db_browser\split.rs",
            "/home/ci/.cargo/registry/src/index/sqlx-postgres-0.9.1/src/row.rs",
        ] {
            let message = safe_message(file, "inside `a cell's text`");
            assert!(!message.contains("cell"), "{file}: {message}");
        }
        let other = safe_message("crates/app/src/daemon/state.rs", "a plain message");
        assert_eq!(other, "a plain message");
    }
}
