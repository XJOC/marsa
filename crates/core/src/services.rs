//! The generic managed process: what to run, when to restart it, and its
//! recent output.
//!
//! Pure data and functions, so this module needs no `full` feature. The
//! daemon's supervisor (`marsa::daemon::supervisor`) runs the process.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use crate::types::{LogLine, LogStream};

/// Lines kept in memory per service. The full output is in `logs/<id>.log`.
pub const LOG_LINES: usize = 2000;

/// How to run one managed process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    /// For example `caddy`. Also the log file name: `logs/<id>.log`, so the
    /// daemon picks it, never the user.
    pub id: String,
    /// Absolute: `marsad` never searches `PATH` (architecture.md section 10).
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Added to the daemon's own environment.
    pub env: BTreeMap<String, String>,
    /// The working folder. `None` keeps the daemon's.
    pub cwd: Option<PathBuf>,
    pub restart: RestartPolicy,
    /// How long a stop waits after SIGTERM before it kills the process.
    pub stop_grace: Duration,
}

/// A relative `program`: `marsad` never searches `PATH`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("program {} is not an absolute path", .0.display())]
pub struct ProgramNotAbsolute(pub PathBuf);

impl ProgramNotAbsolute {
    /// Stable error name for the UI.
    pub fn code(&self) -> &'static str {
        "program_not_absolute"
    }
}

/// What the supervisor does when the process exits without a stop request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    /// Stay `Failed`.
    Never,
    /// Restart up to `max_retries` times. The wait starts at `initial` and
    /// doubles after each restart, up to `max`.
    OnFailure {
        max_retries: u32,
        initial: Duration,
        max: Duration,
    },
}

/// How long to wait before the next restart, or `None` to stay `Failed`.
///
/// `attempt` counts the restarts already made since the last start asked by
/// the user (`ServiceStatus::restarts`): 0 before the first restart.
pub fn restart_delay(policy: RestartPolicy, attempt: u32) -> Option<Duration> {
    match policy {
        RestartPolicy::Never => None,
        RestartPolicy::OnFailure { max_retries, .. } if attempt >= max_retries => None,
        // `checked_pow` is `None` from 2^32 on: the delay is `max` long before.
        RestartPolicy::OnFailure { initial, max, .. } => Some(
            2_u32
                .checked_pow(attempt)
                .map_or(max, |factor| initial.saturating_mul(factor))
                .min(max),
        ),
    }
}

/// The last [`LOG_LINES`] lines of one service, oldest first.
#[derive(Debug, Clone)]
pub struct LogBuffer {
    service: String,
    lines: VecDeque<LogLine>,
    /// Never reset, not even by a restart, so a client can find gaps.
    next_seq: u64,
}

impl LogBuffer {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            lines: VecDeque::new(),
            next_seq: 0,
        }
    }

    /// Stores one line without its line ending (`\n` or `\r\n`), drops the
    /// oldest line when full, and returns the stored line.
    pub fn push(&mut self, stream: LogStream, text: &str, at_ms: u64) -> LogLine {
        let line = LogLine {
            service: self.service.clone(),
            seq: self.next_seq,
            at_ms,
            stream,
            text: text.trim_end_matches(['\r', '\n']).to_owned(),
        };
        self.next_seq += 1;
        if self.lines.len() == LOG_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line.clone());
        line
    }

    /// A copy of the kept lines, oldest first.
    pub fn recent(&self) -> Vec<LogLine> {
        self.lines.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn on_failure(max_retries: u32) -> RestartPolicy {
        RestartPolicy::OnFailure {
            max_retries,
            initial: 100 * MS,
            max: 1000 * MS,
        }
    }

    #[test]
    fn restart_delay_doubles_up_to_max_then_stops() {
        let delays: Vec<_> = (0..6).map(|n| restart_delay(on_failure(5), n)).collect();
        let ms = |n: u32| Some(n * MS);
        assert_eq!(delays, [ms(100), ms(200), ms(400), ms(800), ms(1000), None]);
        // No overflow, however many restarts.
        assert_eq!(restart_delay(on_failure(u32::MAX), 40), Some(1000 * MS));
    }

    #[test]
    fn restart_delay_never_restarts_with_never_or_zero_retries() {
        assert_eq!(restart_delay(RestartPolicy::Never, 0), None);
        assert_eq!(restart_delay(on_failure(0), 0), None);
    }

    #[test]
    fn log_buffer_keeps_the_last_lines_with_growing_seq() {
        let mut logs = LogBuffer::new("caddy");
        for n in 0..=LOG_LINES {
            logs.push(LogStream::Stdout, &format!("line {n}"), 7);
        }
        let recent = logs.recent();
        assert_eq!(recent.len(), LOG_LINES);
        assert_eq!((recent[0].seq, recent[0].text.as_str()), (1, "line 1"));
        assert!(recent.windows(2).all(|w| w[1].seq == w[0].seq + 1));
        assert_eq!(recent[LOG_LINES - 1].service, "caddy");
    }

    #[test]
    fn log_buffer_strips_the_line_ending() {
        let mut logs = LogBuffer::new("caddy");
        let line = logs.push(LogStream::Stderr, "ready\r\n", 5);
        assert_eq!(
            (line.text.as_str(), line.stream, line.at_ms),
            ("ready", LogStream::Stderr, 5)
        );
        assert_eq!(logs.push(LogStream::Stdout, "a\r", 5).text, "a");
    }
}
