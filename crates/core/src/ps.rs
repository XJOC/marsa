//! Reading what `ps` prints, and the start times it gives.
//!
//! Pure text parsing, so this module needs no `full` feature. Every `ps`
//! output of the crate is read here, run with `LC_ALL=C` and `TZ=UTC0`
//! (macOS), so the start times always have the same shape and zone.

use std::cmp::Ordering;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::orphan::{ChildProcess, Liveness};
use crate::ports::ProcessInfo;

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
/// In the order of the days since 1970-01-01, which was a Thursday.
const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];

/// When a process started, as the OS reports it. Two forms exist, one for
/// each OS, and they never compare with each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartTime {
    /// Clock ticks since boot (Linux, `/proc/<pid>/stat`).
    Ticks(u64),
    /// Seconds since 1970-01-01 UTC, from `lstart` text such as
    /// `Wed Nov 4 03:31:27 2026` (macOS).
    Utc(i64),
}

/// The text is neither clock ticks nor `lstart` text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a process start time")]
pub struct BadStartTime;

impl StartTime {
    /// Reads the five words of `lstart` (`Wed Nov 4 03:31:27 2026`). `None`
    /// for any other shape and for a field out of range.
    pub fn from_lstart(words: &[&str]) -> Option<Self> {
        let [_weekday, month, day, clock, year] = words else {
            return None;
        };
        let month = u32::try_from(MONTHS.iter().position(|name| name == month)?).ok()? + 1;
        let mut clock = clock.split(':').map(str::parse::<u32>);
        let (Some(Ok(hour)), Some(Ok(minute)), Some(Ok(second)), None) =
            (clock.next(), clock.next(), clock.next(), clock.next())
        else {
            return None;
        };
        let day: u32 = day.parse().ok()?;
        let year: u32 = year.parse().ok()?;
        // 60: a leap second.
        if !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
            return None;
        }
        let days = days_from_civil(i64::from(year), month, day);
        Some(Self::Utc(
            days * 86_400 + i64::from(hour * 3_600 + minute * 60 + second),
        ))
    }

    /// The seconds since 1970-01-01 UTC, so two start times can be told apart
    /// by a margin. `None` for clock ticks, which are no time of day.
    pub fn utc_seconds(self) -> Option<i64> {
        match self {
            Self::Utc(seconds) => Some(seconds),
            Self::Ticks(_) => None,
        }
    }
}

impl FromStr for StartTime {
    type Err = BadStartTime;

    fn from_str(text: &str) -> Result<Self, BadStartTime> {
        if let Ok(ticks) = text.parse::<u64>() {
            return Ok(Self::Ticks(ticks));
        }
        let words: Vec<&str> = text.split_whitespace().collect();
        Self::from_lstart(&words).ok_or(BadStartTime)
    }
}

/// The text that [`FromStr`] reads back: clock ticks, or `lstart` text with
/// single spaces, which is the form a pid file has always held.
impl fmt::Display for StartTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Ticks(ticks) => write!(f, "{ticks}"),
            Self::Utc(seconds) => {
                let days = seconds.div_euclid(86_400);
                let clock = seconds.rem_euclid(86_400);
                let weekday = WEEKDAYS[usize::try_from(days.rem_euclid(7)).expect("0 to 6")];
                let (year, month, day) = civil_from_days(days);
                write!(
                    f,
                    "{weekday} {} {day} {:02}:{:02}:{:02} {year}",
                    MONTHS[month],
                    clock / 3_600,
                    clock % 3_600 / 60,
                    clock % 60
                )
            }
        }
    }
}

impl Serialize for StartTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for StartTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// Compares two start times. `None` when they are not of one form: then
/// nothing can be said, and the caller fails closed.
pub fn started_order(a: StartTime, b: StartTime) -> Option<Ordering> {
    match (a, b) {
        (StartTime::Ticks(a), StartTime::Ticks(b)) => Some(a.cmp(&b)),
        (StartTime::Utc(a), StartTime::Utc(b)) => Some(a.cmp(&b)),
        _ => None,
    }
}

/// The inverse of [`days_from_civil`], from the same source: the year, the
/// month (0 is January), and the day of the month.
fn civil_from_days(days: i64) -> (i64, usize, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    // Years start in March, as in `days_from_civil`.
    let from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * from_march + 2) / 5 + 1;
    let month = usize::try_from((from_march + 2) % 12).expect("0 to 11");
    let year = year_of_era + era * 400 + i64::from(month < 2);
    (year, month, day)
}

/// Days from 1970-01-01 to `year-month-day` in the Gregorian calendar, by
/// Howard Hinnant's `days_from_civil`. `month` is 1 to 12.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    // Years start in March here, so a leap day is the last day of its year.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_from_march = i64::from((month + 9) % 12);
    let day_of_year = (153 * month_from_march + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    // 719 468 days from 0000-03-01 to 1970-01-01.
    era * 146_097 + day_of_era - 719_468
}

/// Reads one line of `ps -o stat=,lstart=,comm=` run with `LC_ALL=C` and
/// `TZ=UTC0` (macOS): the state, five words of start time, then the start
/// path (`argv[0]`), which may hold spaces.
/// Text in any other shape is `Unknown`.
pub fn parse_ps_line(text: &str) -> Liveness {
    let mut rest = text.trim();
    let mut words = Vec::with_capacity(6);
    for _ in 0..6 {
        let Some(end) = rest.find(char::is_whitespace) else {
            return Liveness::Unknown;
        };
        words.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }
    if words[0].starts_with('Z') {
        return Liveness::Gone;
    }
    // One line only, and an absolute path.
    if !rest.starts_with('/') || rest.contains('\n') {
        return Liveness::Unknown;
    }
    let Some(started) = StartTime::from_lstart(&words[1..]) else {
        return Liveness::Unknown;
    };
    Liveness::Alive {
        exe: PathBuf::from(rest),
        started,
    }
}

/// Reads one line of `ps -o stat=,lstart=` run with `LC_ALL=C` and `TZ=UTC0`
/// (macOS): the state letter and the start time, with single spaces as in
/// [`parse_ps_line`]. For a process whose `argv[0]` is no path, such as
/// php-fpm. Text in any other shape is `None`.
pub fn parse_ps_start(text: &str) -> Option<(char, StartTime)> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let [state, started @ ..] = words.as_slice() else {
        return None;
    };
    Some((state.chars().next()?, StartTime::from_lstart(started)?))
}

/// Reads `ps -axo pid=,ppid=,lstart=` output run with `LC_ALL=C` and
/// `TZ=UTC0` (macOS): the children of `ppid`, each with its start time. A line in another shape is skipped: a process
/// that cannot be read is never signalled.
pub fn parse_ps_children(text: &str, ppid: u32) -> Vec<ChildProcess> {
    text.lines()
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            let [pid, parent, started @ ..] = words.as_slice() else {
                return None;
            };
            if parent.parse::<u32>().ok()? != ppid {
                return None;
            }
            Some(ChildProcess {
                pid: pid.parse().ok()?,
                started: StartTime::from_lstart(started)?,
            })
        })
        .collect()
}

/// Reads the output of `/bin/ps -p <pid> -o user=,comm=`: the user, then the
/// command after the first run of whitespace. The command can contain spaces
/// (`nginx: worker process`). Only the first line that is not empty is read,
/// and whitespace and `\r` around it are cut.
///
/// `None` if the text is empty (the process has ended: `ps` then prints
/// nothing on stdout) or if a line has a user but no command.
pub fn parse_ps(text: &str) -> Option<ProcessInfo> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let (user, command) = line.split_once(char::is_whitespace)?;
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    Some(ProcessInfo {
        user: user.to_string(),
        command: command.to_string(),
    })
}

/// One line of `ps -axo pid=,uid=,lstart=,comm=` run with `LC_ALL=C` and
/// `TZ=UTC0` (macOS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRow {
    pub pid: u32,
    /// The effective user id.
    pub uid: u32,
    pub started: StartTime,
    /// The `comm` column: `argv[0]`, which the process chose. A label, never
    /// proof of the program it runs.
    pub command: PathBuf,
}

/// Reads `ps -axo pid=,uid=,lstart=,comm=` output: the pid, the uid, five
/// words of start time, then `argv[0]`, which may hold spaces. A line in
/// another shape is skipped.
pub fn parse_process_table(text: &str) -> Vec<ProcessRow> {
    text.lines().filter_map(parse_process_row).collect()
}

fn parse_process_row(line: &str) -> Option<ProcessRow> {
    let mut rest = line.trim();
    let mut words = [""; 7];
    for word in &mut words {
        let end = rest.find(char::is_whitespace)?;
        *word = &rest[..end];
        rest = rest[end..].trim_start();
    }
    let [pid, uid, started @ ..] = words;
    Some(ProcessRow {
        pid: pid.parse().ok()?,
        uid: uid.parse().ok()?,
        // A time that cannot be compared is of no use here.
        started: StartTime::from_lstart(&started)?,
        command: PathBuf::from(rest),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alive(exe: &str, started: StartTime) -> Liveness {
        Liveness::Alive {
            exe: PathBuf::from(exe),
            started,
        }
    }

    /// `Wed Nov 4 03:31:27 2026`.
    const STARTED: StartTime = StartTime::Utc(1_793_763_087);

    fn at(text: &str) -> StartTime {
        text.parse().expect("a start time")
    }

    #[test]
    fn ps_lines_give_the_path_with_spaces_and_the_start_time() {
        let line = "SN   Wed Nov  4 03:31:27 2026     /Users/o/Library/Application Support/marsa/bin/caddy\n";
        // The start time comes back with single spaces.
        assert_eq!(
            parse_ps_line(line),
            alive(
                "/Users/o/Library/Application Support/marsa/bin/caddy",
                STARTED
            )
        );
        assert_eq!(
            parse_ps_line("Z    Wed Nov  4 03:31:27 2026 /x\n"),
            Liveness::Gone
        );
        for bad in [
            "",
            "S Wed Nov 4",
            "S Wed Nov  4 03:31:27 2026 caddy",
            "S a b c d e /x\n/y",
        ] {
            assert_eq!(parse_ps_line(bad), Liveness::Unknown, "{bad:?}");
        }
    }

    #[test]
    fn ps_start_lines_give_the_state_and_the_start_time() {
        let started = STARTED;
        let line = "Ss   Wed Nov  4 03:31:27 2026\n";
        assert_eq!(parse_ps_start(line), Some(('S', started)));
        // The same form as `parse_ps_line`, so both compare alike.
        let full = parse_ps_line("Ss   Wed Nov  4 03:31:27 2026 /x\n");
        assert_eq!(full, alive("/x", started));
        assert_eq!(
            parse_ps_start("Z Wed Nov 4 03:31:27 2026"),
            Some(('Z', started))
        );
        for bad in [
            "",
            "S Wed Nov 4",
            "S Wed Nov 4 03:31:27 2026 /x",
            "S a b c d e\nS",
        ] {
            assert_eq!(parse_ps_start(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn ps_child_lines_give_the_children_of_one_parent() {
        let table = "    1     0 Thu Jan  1 00:00:06 1970\n\
                     4242     1 Wed Nov  4 03:31:27 2026\n\
                     4301  4242 Wed Nov  4 03:31:27 2026\n\
                     4302  4242 Wed Nov  4 03:31:28 2026\n\
                     4303  4242 Wed Nov  4 03:31\n\
                     x     4242 Wed Nov  4 03:31:28 2026\n\
                     4304  4300 Wed Nov  4 03:31:28 2026\n";
        let child = |pid, started: &str| ChildProcess {
            pid,
            started: at(started),
        };
        assert_eq!(
            parse_ps_children(table, 4242),
            [
                child(4301, "Wed Nov 4 03:31:27 2026"),
                child(4302, "Wed Nov 4 03:31:28 2026"),
            ]
        );
        assert_eq!(
            parse_ps_children(table, 1),
            [child(4242, "Wed Nov 4 03:31:27 2026")]
        );
        assert!(parse_ps_children(table, 9999).is_empty());
        assert!(parse_ps_children("", 1).is_empty());
    }

    #[test]
    fn start_times_compare_as_times_in_both_forms() {
        use Ordering::{Equal, Greater, Less};
        let order = |a: &str, b: &str| started_order(a.parse().ok()?, b.parse().ok()?);
        // `lstart` text, with single or double spaces.
        let t = "Wed Nov 4 03:31:27 2026";
        assert_eq!(order(t, "Wed Nov  4 03:31:27 2026"), Some(Equal));
        assert_eq!(order(t, "Wed Nov 4 03:31:28 2026"), Some(Less));
        assert_eq!(order(t, "Wed Nov 4 03:30:59 2026"), Some(Greater));
        assert_eq!(order(t, "Thu Nov 5 00:00:00 2026"), Some(Less));
        assert_eq!(order(t, "Sat Oct 31 23:59:59 2026"), Some(Greater));
        assert_eq!(order(t, "Thu Dec 4 03:31:27 2025"), Some(Greater));
        // The weekday is not compared: `ps` derives it from the date.
        assert_eq!(order(t, "Mon Nov 4 03:31:27 2026"), Some(Equal));
        // Clock ticks since boot (Linux).
        assert_eq!(order("987654", "987654"), Some(Equal));
        assert_eq!(order("987654", "987655"), Some(Less));
        assert_eq!(order("1000000", "987655"), Some(Greater));
        // Mixed or broken forms say nothing.
        for (a, b) in [
            (t, "987654"),
            (t, ""),
            (t, "Wed Nov 4 03:31 2026"),
            (t, "Wed Foo 4 03:31:27 2026"),
            (t, "Wed Nov 4 03:31:27:00 2026"),
            ("", ""),
        ] {
            assert_eq!(order(a, b), None, "{a:?} {b:?}");
            assert_eq!(order(b, a), None, "{b:?} {a:?}");
        }
    }

    #[test]
    fn lstart_text_reads_as_seconds_since_1970() {
        let lstart_seconds = |text: &str| text.parse::<StartTime>().ok()?.utc_seconds();
        // The values `date -u -j -f "%Y-%m-%d %H:%M:%S" <time> +%s` gives.
        assert_eq!(lstart_seconds("Thu Jan  1 00:00:00 1970"), Some(0));
        assert_eq!(lstart_seconds("Thu Jan 1 00:00:06 1970"), Some(6));
        assert_eq!(
            lstart_seconds("Wed Nov  4 03:31:27 2026"),
            Some(1_793_763_087)
        );
        assert_eq!(
            lstart_seconds("Sat Nov 14 13:28:04 2026"),
            Some(1_794_662_884)
        );
        // Across a leap day: one second apart.
        assert_eq!(
            lstart_seconds("Tue Feb 29 23:59:59 2028"),
            Some(1_835_481_599)
        );
        assert_eq!(
            lstart_seconds("Wed Mar  1 00:00:00 2028"),
            Some(1_835_481_600)
        );
        // Other forms, and fields out of range, say nothing.
        for bad in [
            "",
            "987654",
            "Wed Nov 4 03:31 2026",
            "Wed Foo 4 03:31:27 2026",
            "Wed Nov 0 03:31:27 2026",
            "Wed Nov 32 03:31:27 2026",
            "Wed Nov 4 24:00:00 2026",
            "Wed Nov 4 03:60:00 2026",
            "Wed Nov 4 03:31:61 2026",
        ] {
            assert_eq!(lstart_seconds(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_start_time_reads_back_as_the_text_that_made_it() {
        // The pid file keeps the text, with single spaces.
        for text in [
            "Wed Nov 4 03:31:27 2026",
            "Thu Jan 1 00:00:00 1970",
            "Tue Feb 29 23:59:59 2028",
        ] {
            assert_eq!(at(text).to_string(), text);
        }
        assert_eq!(
            at("Wed Nov  4 03:31:27 2026").to_string(),
            "Wed Nov 4 03:31:27 2026"
        );
        assert_eq!(StartTime::Ticks(987_654).to_string(), "987654");
        // Steps of a week and an hour from 1970 to 2100, round trip.
        for seconds in (0..4_102_444_800_i64).step_by(86_400 * 7 + 3_661) {
            let time = StartTime::Utc(seconds);
            assert_eq!(time.to_string().parse(), Ok(time), "{seconds}");
        }
        let json = serde_json::to_string(&STARTED).unwrap();
        assert_eq!(json, "\"Wed Nov 4 03:31:27 2026\"");
        assert_eq!(serde_json::from_str::<StartTime>(&json).unwrap(), STARTED);
        assert!(serde_json::from_str::<StartTime>("\"soon\"").is_err());
    }
}
