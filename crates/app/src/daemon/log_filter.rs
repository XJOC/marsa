//! What `marsad` writes to its log.

use tracing::Metadata;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::{FilterFn, filter_fn};

/// Without `MARSA_LOG`: `info`, and hickory's DNS lines only from `warn`
/// (it logs two `info` lines for each `.test` lookup of a browser).
const DEFAULT: &str = "info,hickory_server=warn";

/// The filter of `marsad`'s log: `MARSA_LOG` (`from_env`) if it parses,
/// else [`DEFAULT`].
pub fn log_filter(from_env: Option<&str>) -> EnvFilter {
    from_env
        .and_then(|text| EnvFilter::try_new(text).ok())
        .unwrap_or_else(|| EnvFilter::new(DEFAULT))
}

/// Drops every event of the database driver, whatever [`log_filter`] lets
/// through: PostgreSQL's notices carry the server's own text, and a
/// statement log carries SQL, and neither may reach the log (ADR 0028,
/// section 2). A filter of its own, not a directive next to `MARSA_LOG`'s,
/// so no more specific directive can turn it back on.
pub fn no_driver_log() -> FilterFn<fn(&Metadata<'_>) -> bool> {
    filter_fn(not_the_driver as fn(&Metadata<'_>) -> bool)
}

fn not_the_driver(meta: &Metadata<'_>) -> bool {
    !meta.target().starts_with("sqlx")
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::layer::SubscriberExt;

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

    /// What `marsad`'s filters let through of a PostgreSQL notice, driver
    /// warnings, and a line of the daemon's own; without [`no_driver_log`]
    /// when `driver_off` is `false`.
    fn written(from_env: Option<&str>, driver_off: bool) -> String {
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::registry()
            .with(log_filter(from_env))
            .with(driver_off.then(no_driver_log))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(move || writer.clone())
                    .with_ansi(false),
            );
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: "sqlx::postgres::notice", "notice_from_the_server");
            tracing::info!(target: "sqlx::postgres::notice", "info_notice_from_the_server");
            tracing::warn!(target: "sqlx::query", "slow_statement_text");
            tracing::warn!(target: "sqlx_core::pool", "core_line");
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
            Some("info,sqlx::postgres::notice=trace,sqlx::query=trace"),
            Some("not a filter =="),
        ] {
            let log = written(from_env, true);
            assert!(log.contains("own_line"), "{from_env:?}: {log}");
            for secret in [
                "notice_from_the_server",
                "info_notice_from_the_server",
                "slow_statement_text",
                "core_line",
            ] {
                assert!(!log.contains(secret), "{from_env:?}: {log}");
            }
        }
    }

    /// The control: without it, a notice is written.
    #[test]
    fn without_it_the_notices_would_be_written() {
        let log = written(None, false);
        assert!(log.contains("notice_from_the_server"), "{log}");
    }
}
