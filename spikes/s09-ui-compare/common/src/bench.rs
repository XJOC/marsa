//! `--bench` helpers shared by `tray`, `egui-app`, and `tauri-app`.
//!
//! Every program prints one stdout line per phase, `PHASE <name> key=value ...`,
//! and flushes it at once: `measure.sh` reads the line and samples memory at
//! that moment.

use std::io::Write;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Pause after each `PHASE` line, before the next phase starts. Two jobs:
/// - `measure.sh` samples memory (about 0.3 s for app + WebKit processes);
/// - the previous phase settles. A fast scroll makes WebKit allocate tile
///   surfaces (over 1 GiB, much of it volatile) and release them about 3 s
///   after scrolling stops. With 2 s, the `sql-loaded` sample still caught
///   part of the table scroll; 5 s keeps the phases apart. egui gets the
///   same pause.
pub const PHASE_PAUSE: Duration = Duration::from_secs(5);

/// How long the table and the logs take to scroll from top to bottom.
pub const SCROLL_DURATION: Duration = Duration::from_secs(5);

/// The idle phase: no input, no animation, no work.
pub const IDLE_DURATION: Duration = Duration::from_secs(10);

/// After `--with-tray` destroys its window: time for WebKit to settle before
/// `PHASE window-closed` is printed.
pub const SETTLE_AFTER_CLOSE: Duration = Duration::from_secs(5);

/// The `sql-typing` phase (S-09b) types these 50 ASCII characters, one per
/// frame, into the SQL editor. Both programs type the same text.
pub const TYPING_TEXT: &str = "-- typed by the S-09b bench, one char per frame --";

/// Typing starts at the beginning of this line (1-based): the middle of the
/// SQL text, so the editor has 2,500 lines above and below the cursor.
pub const TYPING_LINE: usize = crate::data::SQL_LINES / 2;

static START: OnceLock<Instant> = OnceLock::new();

/// Records the start of `main()`. Call it on the first line of `main()`.
/// Later calls keep the first value.
pub fn start_clock() -> Instant {
    *START.get_or_init(Instant::now)
}

/// Milliseconds since [`start_clock`]. If the clock was never started, it
/// starts now and returns about 0, which makes the mistake visible.
pub fn ms_since_start() -> f64 {
    start_clock().elapsed().as_secs_f64() * 1000.0
}

/// Prints `PHASE <name> key=value ...` on stdout and flushes it.
///
/// `println!` is allowed here: this line is the program's requested output in
/// `--bench` mode (AGENTS.md section 6).
pub fn phase(name: &str, fields: &[(&str, String)]) {
    let mut line = format!("PHASE {name}");
    for (key, value) in fields {
        line.push(' ');
        line.push_str(key);
        line.push('=');
        line.push_str(value);
    }
    let mut stdout = std::io::stdout().lock();
    // A closed stdout (the script died) must not crash the program: the
    // bench line is lost either way, so the write error is ignored.
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

/// Formats milliseconds with one decimal, the unit of every `PHASE` value.
pub fn ms(value: f64) -> String {
    format!("{value:.1}")
}

/// Frame-interval statistics, all in milliseconds.
///
/// The input is the wall time between consecutive presented frames, measured
/// the same way in both programs (see the README).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameStats {
    pub frames: usize,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
}

impl FrameStats {
    /// `None` when there is no interval, or when an interval is not a finite
    /// number (bad data from the UI must not produce a fake result).
    pub fn from_intervals(intervals_ms: &[f64]) -> Option<Self> {
        if intervals_ms.is_empty() || intervals_ms.iter().any(|value| !value.is_finite()) {
            return None;
        }
        let mut sorted = intervals_ms.to_vec();
        sorted.sort_by(f64::total_cmp);
        Some(Self {
            frames: sorted.len(),
            p50: nearest_rank(&sorted, 50.0),
            p95: nearest_rank(&sorted, 95.0),
            max: sorted[sorted.len() - 1],
        })
    }

    /// The `key=value` fields of a `PHASE` line for frame intervals.
    pub fn fields(&self) -> [(&'static str, String); 4] {
        [
            ("frames", self.frames.to_string()),
            ("p50", ms(self.p50)),
            ("p95", ms(self.p95)),
            ("max", ms(self.max)),
        ]
    }

    /// The same statistics used for CPU time per frame (egui only).
    pub fn cpu_fields(&self) -> [(&'static str, String); 3] {
        [
            ("cpu_p50", ms(self.p50)),
            ("cpu_p95", ms(self.p95)),
            ("cpu_max", ms(self.max)),
        ]
    }
}

/// Nearest-rank percentile on sorted data: the smallest value with at least
/// `percent`% of the values at or below it. No interpolation, so the result is
/// always a measured interval.
fn nearest_rank(sorted: &[f64], percent: f64) -> f64 {
    let rank = ((percent / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_use_nearest_rank() {
        let intervals: Vec<f64> = (1..=100).map(f64::from).collect();
        let stats = FrameStats::from_intervals(&intervals).expect("non-empty input");
        assert_eq!(stats.frames, 100);
        assert_eq!(stats.p50, 50.0);
        assert_eq!(stats.p95, 95.0);
        assert_eq!(stats.max, 100.0);
    }

    #[test]
    fn stats_ignore_input_order() {
        let stats = FrameStats::from_intervals(&[33.0, 16.0, 17.0]).expect("non-empty input");
        assert_eq!(stats.p50, 17.0);
        assert_eq!(stats.p95, 33.0);
        assert_eq!(stats.max, 33.0);
    }

    #[test]
    fn stats_reject_empty_and_non_finite_input() {
        assert_eq!(FrameStats::from_intervals(&[]), None);
        assert_eq!(FrameStats::from_intervals(&[16.0, f64::NAN]), None);
        assert_eq!(FrameStats::from_intervals(&[f64::INFINITY]), None);
    }

    #[test]
    fn typing_constants_match_the_plan() {
        assert_eq!(TYPING_TEXT.len(), 50);
        assert!(TYPING_TEXT.is_ascii());
        assert_eq!(TYPING_LINE, 2_500);
    }

    #[test]
    fn fields_use_one_decimal() {
        let stats = FrameStats::from_intervals(&[16.66, 8.0]).expect("non-empty input");
        let [frames, p50, p95, max] = stats.fields();
        assert_eq!(frames, ("frames", "2".to_string()));
        assert_eq!(p50, ("p50", "8.0".to_string()));
        assert_eq!(p95, ("p95", "16.7".to_string()));
        assert_eq!(max, ("max", "16.7".to_string()));
        assert_eq!(stats.cpu_fields()[0].0, "cpu_p50");
    }
}
