//! What `marsad` writes to its log.

use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::Directive;

/// Without `MARSA_LOG`: `info`, and hickory's DNS lines only from `warn`
/// (it logs two `info` lines for each `.test` lookup of a browser).
const DEFAULT: &str = "info,hickory_server=warn";

/// The filter of `marsad`'s log: `MARSA_LOG` (`from_env`) if it parses,
/// else [`DEFAULT`]. The database driver is always off, whatever `MARSA_LOG`
/// says: PostgreSQL's notices carry the server's own text, and a statement
/// log carries SQL, and neither may reach the log (ADR 0028, section 2).
pub fn log_filter(from_env: Option<&str>) -> EnvFilter {
    let filter = from_env
        .and_then(|text| EnvFilter::try_new(text).ok())
        .unwrap_or_else(|| EnvFilter::new(DEFAULT));
    filter.add_directive(driver_off())
}

fn driver_off() -> Directive {
    "sqlx=off".parse().expect("a fixed directive that parses")
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// What `filter` lets through of a PostgreSQL notice, a driver warning,
    /// and a line of the daemon's own.
    fn written(filter: EnvFilter) -> String {
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_env_filter(filter)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: "sqlx::postgres::notice", "notice_from_the_server");
            tracing::info!(target: "sqlx::postgres::notice", "info_notice_from_the_server");
            tracing::warn!(target: "sqlx::query", "slow_statement_text");
            tracing::info!(target: "marsa::daemon", "own_line");
        });
        String::from_utf8(capture.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn the_driver_never_writes_to_the_log() {
        for from_env in [
            None,
            Some("trace"),
            Some("sqlx=trace,info"),
            Some("not a filter =="),
        ] {
            let log = written(log_filter(from_env));
            assert!(log.contains("own_line"), "{from_env:?}: {log}");
            for secret in [
                "notice_from_the_server",
                "info_notice_from_the_server",
                "slow_statement_text",
            ] {
                assert!(!log.contains(secret), "{from_env:?}: {log}");
            }
        }
    }

    /// The control: without the directive, these lines are written.
    #[test]
    fn without_it_the_notices_would_be_written() {
        let log = written(EnvFilter::new(DEFAULT));
        assert!(log.contains("notice_from_the_server"), "{log}");
    }
}
