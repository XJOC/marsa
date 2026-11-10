//! What to do with a Caddy left running by a `marsad` that crashed (ADR 0009).
//!
//! Pure data and functions, so this module needs no `full` feature. The daemon
//! inspects the machine (the pid file, the admin API, the ports), turns the
//! results into an [`Inspection`], and acts on what [`decide`] returns. Like a
//! Laravel policy: it answers "may I?", and the controller does the work.
//!
//! The rule that matters most: a pid may be signalled only when the pid file
//! names it, the process at that pid was started as our Caddy program, at the
//! time the file says ([`classify_pidfile`]), and, right before the signal,
//! its real executable is our Caddy. A port, or an answer on the admin API,
//! never authorizes a signal.
//!
//! Identity on the admin API is per data folder: the daemon puts
//! `"@id": "marsa-instance-<id>"` on the `admin` object ([`instance_tag`]). A
//! Caddy that answers only to the shared `marsa-srv` tag belongs to another
//! Marsa (another data folder, or another user), and is never touched.

use std::cmp::Ordering;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PidfileState {
    /// No readable file.
    Missing,
    /// The file names a process that is gone.
    Stale { pid: u32 },
    /// The pid is alive but it is not the process we started.
    Mismatch { pid: u32, found: String },
    /// The OS could not describe the pid this time. Never ours, and the file
    /// is kept: it may be ours on the next try.
    Unknown { pid: u32 },
    /// Alive, same start path, same start time.
    Ours { pid: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminState {
    /// `GET /id/marsa-instance-<id>` answered 200 with our data folder's id.
    Ours,
    /// Only `GET /id/marsa-srv` answered: the Caddy of another Marsa.
    OtherMarsa,
    /// Something answered, but without our id, or not as HTTP.
    Foreign,
    /// Connection refused: nothing listens.
    NoReply,
    /// Connected, but no answer in time (a frozen process).
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortState {
    Free,
    /// Someone listens. The owner is `name(pid)` from `lsof`.
    Busy {
        owner: String,
    },
}

#[derive(Debug, Clone)]
pub struct Inspection {
    pub pidfile: PidfileState,
    pub admin: AdminState,
    pub app_port: PortState,
    pub admin_port: PortState,
    /// The pid that `lsof` finds on the admin port, only when its executable
    /// path is our Caddy. A second source of a verified pid, besides the pid
    /// file. The daemon leaves it `None`: only the pid file names a pid.
    pub admin_owner: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    Reap,
    /// Rejected in ADR 0009. Kept so the spike's tests run unchanged.
    Adopt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing of ours is running. Start Caddy. Remove a dead pid file first.
    StartFresh { remove_pidfile: bool },
    /// Stop the orphan (gently first), then start a new Caddy.
    Reap { pid: Option<u32> },
    /// Keep the orphan and send it a new config.
    Adopt { pid: Option<u32> },
    /// Someone else holds our ports. Report it and touch nothing.
    Conflict { reason: String },
    /// Another Marsa's Caddy answers on our admin port. Touch nothing.
    OtherMarsa,
}

impl fmt::Display for PidfileState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(f, "missing"),
            Self::Stale { pid } => write!(f, "stale(pid={pid})"),
            Self::Mismatch { pid, found } => write!(f, "mismatch(pid={pid},found={found})"),
            Self::Unknown { pid } => write!(f, "unknown(pid={pid})"),
            Self::Ours { pid } => write!(f, "ours(pid={pid})"),
        }
    }
}

impl fmt::Display for AdminState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Ours => "ours",
            Self::OtherMarsa => "other-marsa",
            Self::Foreign => "foreign",
            Self::NoReply => "no-reply",
            Self::Timeout => "timeout",
        };
        write!(f, "{text}")
    }
}

impl fmt::Display for PortState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Free => write!(f, "free"),
            Self::Busy { owner } => write!(f, "busy({owner})"),
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StartFresh { remove_pidfile } => {
                write!(f, "start-fresh(remove_pidfile={remove_pidfile})")
            }
            Self::Reap { pid } => write!(f, "reap(pid={pid:?})"),
            Self::Adopt { pid } => write!(f, "adopt(pid={pid:?})"),
            Self::Conflict { reason } => write!(f, "conflict({reason})"),
            Self::OtherMarsa => write!(f, "other-marsa"),
        }
    }
}

/// Turns inspection results into one action. Identity comes from the admin
/// API (our `@id`). The pid file only supplies a pid to signal, and only after
/// its name and start time matched. A port alone never authorizes a kill.
pub fn decide(i: &Inspection, strategy: Strategy) -> Action {
    let from_file = match i.pidfile {
        PidfileState::Ours { pid } => Some(pid),
        _ => None,
    };
    // A pid we may signal: from the pid file, or else the verified owner of
    // our admin port.
    let ours_pid = from_file.or(i.admin_owner);
    match i.admin {
        AdminState::OtherMarsa => Action::OtherMarsa,
        AdminState::Foreign => Action::Conflict {
            reason: "a Caddy that is not ours answers on our admin port".to_string(),
        },
        AdminState::Ours => match strategy {
            Strategy::Reap => Action::Reap { pid: ours_pid },
            Strategy::Adopt => Action::Adopt { pid: ours_pid },
        },
        // A frozen process cannot be adopted, only removed, and removing it
        // needs a verified pid.
        AdminState::Timeout => match ours_pid {
            Some(pid) => Action::Reap { pid: Some(pid) },
            None => Action::Conflict {
                reason: "a process holds our admin port, does not answer, and cannot be verified as our Caddy"
                    .to_string(),
            },
        },
        AdminState::NoReply => {
            if let Some(pid) = from_file {
                return Action::Reap { pid: Some(pid) };
            }
            for (name, state) in [("site", &i.app_port), ("admin", &i.admin_port)] {
                if let PortState::Busy { owner } = state {
                    return Action::Conflict {
                        reason: format!("the {name} port is held by {owner}, which is not our Caddy"),
                    };
                }
            }
            Action::StartFresh {
                remove_pidfile: matches!(
                    i.pidfile,
                    PidfileState::Stale { .. } | PidfileState::Mismatch { .. }
                ),
            }
        }
    }
}

/// `run/caddy.pid`: written right after each spawn of Caddy, removed after it
/// exits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PidRecord {
    pub pid: u32,
    /// The path the process was started with, as the OS reported it right
    /// after the spawn. On macOS `ps` shows `argv[0]`, which a process sets
    /// itself, so the daemon checks the real executable before each signal.
    pub exe: PathBuf,
    /// The start time as the OS reported it, compared as text. Without it a
    /// reused pid that runs the same program would pass (ADR 0009, R5e).
    pub started: String,
}

/// What the OS says about a pid right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Liveness {
    /// No such process, or a zombie.
    Gone,
    /// `exe` is the path it was started with (`argv[0]` on macOS).
    Alive { exe: PathBuf, started: String },
    /// The OS cannot tell: no permission, a failed tool, or an OS where this
    /// is not supported. Never ours.
    Unknown,
}

/// Compares the pid file with what the OS says now. `Ours` only when the
/// process was started as the program the file names, that program is one of
/// `accepted` (every installed Caddy of this data folder, and the resolved
/// form of each), and the start time is
/// the same. The daemon calls this again before each signal.
pub fn classify_pidfile(record: &PidRecord, accepted: &[PathBuf], now: &Liveness) -> PidfileState {
    let pid = record.pid;
    let (exe, started) = match now {
        Liveness::Gone => return PidfileState::Stale { pid },
        Liveness::Unknown => return PidfileState::Unknown { pid },
        Liveness::Alive { exe, started } => (exe, started),
    };
    let found = exe.display().to_string();
    if *exe != record.exe || !accepted.contains(exe) {
        return PidfileState::Mismatch { pid, found };
    }
    if *started != record.started {
        let found = format!("{found} (started {started:?}, not {:?})", record.started);
        return PidfileState::Mismatch { pid, found };
    }
    PidfileState::Ours { pid }
}

/// One child of a process, as the process table lists it: its pid and its
/// start time, in the form [`Liveness::Alive`] gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildProcess {
    pub pid: u32,
    pub started: String,
}

/// Compares two start times in the form [`Liveness::Alive`] gives: `lstart`
/// text (`Wed Nov 4 03:31:27 2026`, macOS, always in UTC) or clock ticks
/// since boot (Linux). `None` when they are not both of one form: then
/// nothing can be said, and the caller fails closed.
pub fn started_order(a: &str, b: &str) -> Option<Ordering> {
    if let (Ok(a), Ok(b)) = (a.parse::<u64>(), b.parse::<u64>()) {
        return Some(a.cmp(&b));
    }
    Some(lstart_key(a)?.cmp(&lstart_key(b)?))
}

/// `Www Mmm d HH:MM:SS YYYY` as `[year, month, day, hour, minute, second]`,
/// which sorts in the order of time.
fn lstart_key(text: &str) -> Option<[u32; 6]> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let words: Vec<&str> = text.split_whitespace().collect();
    let [_weekday, month, day, clock, year] = words.as_slice() else {
        return None;
    };
    let month = MONTHS.iter().position(|name| name == month)?;
    let month = u32::try_from(month).ok()? + 1;
    let mut clock = clock.split(':').map(str::parse::<u32>);
    let (Some(Ok(hour)), Some(Ok(minute)), Some(Ok(second)), None) =
        (clock.next(), clock.next(), clock.next(), clock.next())
    else {
        return None;
    };
    Some([
        year.parse().ok()?,
        month,
        day.parse().ok()?,
        hour,
        minute,
        second,
    ])
}

/// Whether `now`, what the OS says about `pid`, shows a worker of the php-fpm
/// master `master` names: alive, running the master's very executable, which
/// must be one of `accepted`, and started no earlier than the master (a
/// master forks its workers). The master's own pid is never its worker: the
/// pid file's pid is judged by [`classify_pidfile`] alone. Gives the worker's
/// own record, for the checks before each of its signals
/// ([`classify_pidfile`]). A start time that cannot be compared counts as
/// earlier, so nothing is signalled.
pub fn worker_of(
    master: &PidRecord,
    accepted: &[PathBuf],
    pid: u32,
    now: &Liveness,
) -> Option<PidRecord> {
    let Liveness::Alive { exe, started } = now else {
        return None;
    };
    if pid == master.pid || *exe != master.exe || !accepted.contains(exe) {
        return None;
    }
    if started_order(started, &master.started)? == Ordering::Less {
        return None;
    }
    Some(PidRecord {
        pid,
        exe: exe.clone(),
        started: started.clone(),
    })
}

/// The start of the `@id` the daemon puts on Caddy's `admin` object.
pub const INSTANCE_TAG_PREFIX: &str = "marsa-instance-";

/// The `@id` of this data folder's Caddy: `marsa-instance-<id>`, where `<id>`
/// is the folder's [instance id](is_instance_id).
pub fn instance_tag(instance_id: &str) -> String {
    format!("{INSTANCE_TAG_PREFIX}{instance_id}")
}

/// Whether `text` is an instance id: 32 lowercase hex characters.
pub fn is_instance_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether an answer to `GET /id/<tag>` is the object tagged `tag`: a 200
/// whose JSON carries that `@id`. Any other answer is not.
pub fn reply_has_tag(status: u16, body: &[u8], tag: &str) -> bool {
    status == 200
        && serde_json::from_slice::<serde_json::Value>(body)
            .is_ok_and(|value| value.get("@id").and_then(|id| id.as_str()) == Some(tag))
}

/// The admin API state from two answers: to our instance tag, and, only when
/// that one was not ours, to the shared `marsa-srv` tag.
pub fn classify_admin_replies(ours: bool, shared: bool) -> AdminState {
    match (ours, shared) {
        (true, _) => AdminState::Ours,
        (false, true) => AdminState::OtherMarsa,
        (false, false) => AdminState::Foreign,
    }
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
    Liveness::Alive {
        exe: PathBuf::from(rest),
        started: words[1..].join(" "),
    }
}

/// Reads one line of `ps -o stat=,lstart=` run with `LC_ALL=C` and `TZ=UTC0`
/// (macOS): the state letter and the start time, with single spaces as in
/// [`parse_ps_line`]. For a process whose `argv[0]` is no path, such as
/// php-fpm. Text in any other shape is `None`.
pub fn parse_ps_start(text: &str) -> Option<(char, String)> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let [state, started @ ..] = words.as_slice() else {
        return None;
    };
    if started.len() != 5 {
        return None;
    }
    Some((state.chars().next()?, started.join(" ")))
}

/// Reads `ps -axo pid=,ppid=,lstart=` output run with `LC_ALL=C` and
/// `TZ=UTC0` (macOS): the children of `ppid`, each with its start time in the
/// form of [`parse_ps_start`]. A line in another shape is skipped: a process
/// that cannot be read is never signalled.
pub fn parse_ps_children(text: &str, ppid: u32) -> Vec<ChildProcess> {
    text.lines()
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            let [pid, parent, started @ ..] = words.as_slice() else {
                return None;
            };
            if started.len() != 5 || parent.parse::<u32>().ok()? != ppid {
                return None;
            }
            Some(ChildProcess {
                pid: pid.parse().ok()?,
                started: started.join(" "),
            })
        })
        .collect()
}

/// What `/proc/<pid>/stat` says (Linux): the state letter, the parent, and
/// the start time in clock ticks since boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStat {
    pub state: char,
    pub ppid: u32,
    pub started: u64,
}

/// Reads `/proc/<pid>/stat` (Linux). The name in parentheses may hold any
/// character, so the fields are counted after the last `)`.
pub fn parse_proc_stat(text: &str) -> Option<ProcStat> {
    let (_, fields) = text.rsplit_once(')')?;
    let mut fields = fields.split_whitespace();
    // Of `man 5 proc`: field 3 is the state, 4 the parent, 22 the start time.
    let state = fields.next()?.chars().next()?;
    let ppid = fields.next()?.parse().ok()?;
    let started = fields.nth(17)?.parse().ok()?;
    Some(ProcStat {
        state,
        ppid,
        started,
    })
}

/// Reads `lsof -p <pid> -a -d txt -Fn` output (macOS): the first `n<path>`
/// line is the executable the process runs, links resolved; the next ones are
/// `dyld` and the libraries. `lsof` escapes odd characters (a newline becomes
/// `\n`), so such a path never equals ours and the check fails closed.
pub fn parse_lsof_executable(text: &str) -> Option<PathBuf> {
    let path = text
        .lines()
        .find_map(|line| line.trim_end_matches('\r').strip_prefix('n'))?;
    path.starts_with('/').then(|| PathBuf::from(path))
}

/// Reads `lsof -Fpc` output: `p<pid>` and `c<name>` lines. Gives `name(pid)`
/// for the first process, for messages only.
pub fn parse_lsof(text: &str) -> Option<String> {
    let mut pid = None;
    let mut name = None;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix('p') {
            if pid.is_some() {
                break;
            }
            pid = rest.parse::<u32>().ok();
        } else if let Some(rest) = line.strip_prefix('c') {
            name.get_or_insert_with(|| rest.to_owned());
        }
    }
    Some(format!("{}({})", name.unwrap_or_default(), pid?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caddy::SERVER_ID;

    fn busy(owner: &str) -> PortState {
        PortState::Busy {
            owner: owner.to_string(),
        }
    }

    fn inspection(
        pidfile: PidfileState,
        admin: AdminState,
        app: PortState,
        adm: PortState,
    ) -> Inspection {
        Inspection {
            pidfile,
            admin,
            app_port: app,
            admin_port: adm,
            admin_owner: None,
        }
    }

    // The spike's tests (spikes/s06-caddy-orphan/src/recover.rs), unchanged.

    #[test]
    fn r1_orphan_with_pid_file_is_reaped_by_pid() {
        let i = inspection(
            PidfileState::Ours { pid: 7 },
            AdminState::Ours,
            busy("caddy(7)"),
            busy("caddy(7)"),
        );
        assert_eq!(decide(&i, Strategy::Reap), Action::Reap { pid: Some(7) });
        assert_eq!(decide(&i, Strategy::Adopt), Action::Adopt { pid: Some(7) });
    }

    #[test]
    fn r2_orphan_without_pid_file_is_still_ours() {
        let i = inspection(
            PidfileState::Missing,
            AdminState::Ours,
            busy("caddy(7)"),
            busy("caddy(7)"),
        );
        assert_eq!(decide(&i, Strategy::Reap), Action::Reap { pid: None });
    }

    #[test]
    fn r3_stale_pid_file_is_removed_and_caddy_starts() {
        let i = inspection(
            PidfileState::Stale { pid: 7 },
            AdminState::NoReply,
            PortState::Free,
            PortState::Free,
        );
        assert_eq!(
            decide(&i, Strategy::Reap),
            Action::StartFresh {
                remove_pidfile: true
            }
        );
    }

    #[test]
    fn r4_reused_pid_is_never_signalled() {
        let i = inspection(
            PidfileState::Mismatch {
                pid: 7,
                found: "/bin/sleep".to_string(),
            },
            AdminState::NoReply,
            PortState::Free,
            PortState::Free,
        );
        let action = decide(&i, Strategy::Reap);
        assert_eq!(
            action,
            Action::StartFresh {
                remove_pidfile: true
            }
        );
    }

    #[test]
    fn r5_foreign_caddy_is_a_conflict() {
        let i = inspection(
            PidfileState::Missing,
            AdminState::Foreign,
            busy("caddy(9)"),
            busy("caddy(9)"),
        );
        assert!(matches!(
            decide(&i, Strategy::Reap),
            Action::Conflict { .. }
        ));
        assert!(matches!(
            decide(&i, Strategy::Adopt),
            Action::Conflict { .. }
        ));
    }

    #[test]
    fn r5b_foreign_caddy_wins_over_a_matching_pid_file() {
        let i = inspection(
            PidfileState::Ours { pid: 9 },
            AdminState::Foreign,
            busy("caddy(9)"),
            busy("caddy(9)"),
        );
        assert!(matches!(
            decide(&i, Strategy::Reap),
            Action::Conflict { .. }
        ));
    }

    #[test]
    fn r6_another_program_on_the_site_port_is_a_conflict() {
        let i = inspection(
            PidfileState::Missing,
            AdminState::NoReply,
            busy("s06(5)"),
            PortState::Free,
        );
        match decide(&i, Strategy::Reap) {
            Action::Conflict { reason } => assert!(reason.contains("s06(5)")),
            other => panic!("expected a conflict, got {other}"),
        }
    }

    #[test]
    fn r7_frozen_orphan_is_reaped_never_adopted() {
        let i = inspection(
            PidfileState::Ours { pid: 7 },
            AdminState::Timeout,
            busy("caddy(7)"),
            busy("caddy(7)"),
        );
        assert_eq!(decide(&i, Strategy::Adopt), Action::Reap { pid: Some(7) });
    }

    #[test]
    fn frozen_orphan_without_pid_file_is_reaped_when_the_admin_port_owner_is_verified() {
        let mut i = inspection(
            PidfileState::Missing,
            AdminState::Timeout,
            busy("caddy(7)"),
            busy("caddy(7)"),
        );
        i.admin_owner = Some(7);
        assert_eq!(decide(&i, Strategy::Reap), Action::Reap { pid: Some(7) });
    }

    #[test]
    fn frozen_process_without_a_verified_pid_is_a_conflict() {
        let i = inspection(
            PidfileState::Missing,
            AdminState::Timeout,
            busy("caddy(7)"),
            busy("caddy(7)"),
        );
        assert!(matches!(
            decide(&i, Strategy::Reap),
            Action::Conflict { .. }
        ));
    }

    #[test]
    fn nothing_running_starts_fresh_without_touching_files() {
        let i = inspection(
            PidfileState::Missing,
            AdminState::NoReply,
            PortState::Free,
            PortState::Free,
        );
        assert_eq!(
            decide(&i, Strategy::Reap),
            Action::StartFresh {
                remove_pidfile: false
            }
        );
    }

    // The checks that feed `decide`.

    const CADDY: &str = "/data/bin/caddy";
    const STARTED: &str = "Wed Nov  4 03:31:27 2026";

    fn record() -> PidRecord {
        PidRecord {
            pid: 7,
            exe: PathBuf::from(CADDY),
            started: STARTED.to_owned(),
        }
    }

    fn alive(exe: &str, started: &str) -> Liveness {
        Liveness::Alive {
            exe: PathBuf::from(exe),
            started: started.to_owned(),
        }
    }

    fn classify(now: &Liveness) -> PidfileState {
        classify_pidfile(&record(), &[PathBuf::from(CADDY)], now)
    }

    #[test]
    fn a_pid_file_is_ours_only_with_the_same_path_and_start_time() {
        assert_eq!(
            classify(&alive(CADDY, STARTED)),
            PidfileState::Ours { pid: 7 }
        );
    }

    #[test]
    fn a_pid_that_runs_another_program_is_a_mismatch() {
        let state = classify(&alive("/bin/sleep", STARTED));
        let found = "/bin/sleep".to_owned();
        assert_eq!(state, PidfileState::Mismatch { pid: 7, found });
    }

    #[test]
    fn r5e_the_same_program_started_at_another_time_is_a_mismatch() {
        let state = classify(&alive(CADDY, "Wed Nov  4 03:31:28 2026"));
        assert!(
            matches!(state, PidfileState::Mismatch { pid: 7, .. }),
            "{state}"
        );
    }

    #[test]
    fn a_dead_pid_is_stale_and_an_unknown_one_is_kept() {
        assert_eq!(classify(&Liveness::Gone), PidfileState::Stale { pid: 7 });
        let unknown = classify(&Liveness::Unknown);
        assert_eq!(unknown, PidfileState::Unknown { pid: 7 });
        // A failed `ps` neither names a pid to signal nor removes the file.
        let i = inspection(
            unknown,
            AdminState::NoReply,
            PortState::Free,
            PortState::Free,
        );
        let keep = Action::StartFresh {
            remove_pidfile: false,
        };
        assert_eq!(decide(&i, Strategy::Reap), keep);
    }

    #[test]
    fn a_pid_file_that_names_another_program_is_a_mismatch_even_if_it_runs() {
        // The file and the OS agree, but the program is not our Caddy.
        let record = PidRecord {
            exe: PathBuf::from("/bin/sleep"),
            ..record()
        };
        let now = alive("/bin/sleep", STARTED);
        let state = classify_pidfile(&record, &[PathBuf::from(CADDY)], &now);
        assert!(
            matches!(state, PidfileState::Mismatch { pid: 7, .. }),
            "{state}"
        );
    }

    #[test]
    fn only_a_200_with_the_tag_has_the_tag() {
        // What Caddy answers for the server `caddy_config` builds.
        let config = crate::caddy::caddy_config(&crate::caddy::CaddyInput::default());
        let server = config["apps"]["http"]["servers"]["marsa"].to_string();
        assert!(reply_has_tag(200, server.as_bytes(), SERVER_ID));

        let tag = instance_tag("0123456789abcdef0123456789abcdef");
        let admin = format!(r#"{{"@id":"{tag}","listen":"127.0.0.1:1"}}"#);
        assert!(reply_has_tag(200, admin.as_bytes(), &tag));
        assert!(!reply_has_tag(200, admin.as_bytes(), SERVER_ID));

        let unknown = br#"{"error":"unknown object ID 'marsa-srv'"}"#;
        let cases: [(u16, &[u8]); 5] = [
            (404, unknown),
            (200, br#"{"@id":"other"}"#),
            (200, b"<html>marsa-srv</html>"),
            (200, b""),
            (500, server.as_bytes()),
        ];
        for (status, body) in cases {
            let text = String::from_utf8_lossy(body);
            assert!(!reply_has_tag(status, body, SERVER_ID), "{status} {text}");
        }
    }

    #[test]
    fn our_instance_tag_wins_and_the_shared_tag_alone_is_another_marsa() {
        assert_eq!(classify_admin_replies(true, true), AdminState::Ours);
        assert_eq!(classify_admin_replies(true, false), AdminState::Ours);
        assert_eq!(classify_admin_replies(false, true), AdminState::OtherMarsa);
        assert_eq!(classify_admin_replies(false, false), AdminState::Foreign);
        // Another Marsa wins over a pid file that matches, as R5b.
        let i = inspection(
            PidfileState::Ours { pid: 9 },
            AdminState::OtherMarsa,
            busy("caddy(9)"),
            busy("caddy(9)"),
        );
        assert_eq!(decide(&i, Strategy::Reap), Action::OtherMarsa);
    }

    #[test]
    fn instance_ids_are_32_lowercase_hex() {
        assert!(is_instance_id("0123456789abcdef0123456789abcdef"));
        for bad in [
            "",
            "0123456789abcdef0123456789abcde",
            "0123456789ABCDEF0123456789abcdef",
            "0123456789abcdef0123456789abcdeg",
            "0123456789abcdef0123456789abcdef\n",
        ] {
            assert!(!is_instance_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn lsof_txt_output_gives_the_executable() {
        let out = "p812\nftxt\nn/data/a b/bin/caddy\nftxt\nn/usr/lib/dyld\n";
        let exe = parse_lsof_executable(out);
        assert_eq!(exe, Some(PathBuf::from("/data/a b/bin/caddy")));
        assert_eq!(parse_lsof_executable(""), None);
        assert_eq!(parse_lsof_executable("p812\nftxt\nncaddy\n"), None);
    }

    #[test]
    fn ps_lines_give_the_path_with_spaces_and_the_start_time() {
        let line = "SN   Wed Nov  4 03:31:27 2026     /Users/o/Library/Application Support/marsa/bin/caddy\n";
        // The start time comes back with single spaces.
        assert_eq!(
            parse_ps_line(line),
            alive(
                "/Users/o/Library/Application Support/marsa/bin/caddy",
                "Wed Nov 4 03:31:27 2026"
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
        let started = "Wed Nov 4 03:31:27 2026".to_owned();
        let line = "Ss   Wed Nov  4 03:31:27 2026\n";
        assert_eq!(parse_ps_start(line), Some(('S', started.clone())));
        // The same form as `parse_ps_line`, so both compare alike.
        let full = parse_ps_line("Ss   Wed Nov  4 03:31:27 2026 /x\n");
        assert_eq!(full, alive("/x", &started));
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
    fn proc_stat_gives_the_state_parent_and_start_ticks_after_the_name() {
        // A name with spaces and `)` in it, as a program may choose.
        let stat = "4242 (cad dy) x) S 1 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 9 0 \
                    987654 1000000 200 18446744073709551615";
        let want = ProcStat {
            state: 'S',
            ppid: 1,
            started: 987_654,
        };
        assert_eq!(parse_proc_stat(stat), Some(want));
        assert_eq!(parse_proc_stat("4242 (caddy) Z 1 2"), None);
        assert_eq!(parse_proc_stat("no parenthesis"), None);
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
            started: started.to_owned(),
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
        let order = started_order;
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
    fn a_worker_runs_the_masters_executable_and_started_no_earlier() {
        let master = PidRecord {
            pid: 4242,
            exe: PathBuf::from("/data/runtimes/php/8.4.3/bin/php-fpm"),
            started: "Wed Nov 4 03:31:27 2026".to_owned(),
        };
        let accepted = [
            master.exe.clone(),
            PathBuf::from("/data/runtimes/php/8.3.1/bin/php-fpm"),
        ];
        let fpm = "/data/runtimes/php/8.4.3/bin/php-fpm";
        let same = "Wed Nov 4 03:31:27 2026";
        let later = "Wed Nov 4 03:31:28 2026";
        for started in [same, later] {
            let now = alive(fpm, started);
            let want = PidRecord {
                pid: 4301,
                exe: master.exe.clone(),
                started: started.to_owned(),
            };
            assert_eq!(worker_of(&master, &accepted, 4301, &now), Some(want));
        }
        let rejected = [
            ("another program", alive("/bin/sleep", later)),
            (
                "a php-fpm of another data folder",
                alive("/other/runtimes/php/8.4.3/bin/php-fpm", later),
            ),
            (
                "another installed php-fpm, not the master's",
                alive("/data/runtimes/php/8.3.1/bin/php-fpm", later),
            ),
            (
                "earlier than the master",
                alive(fpm, "Wed Nov 4 03:31:26 2026"),
            ),
            ("a start time of another form", alive(fpm, "987654")),
            ("a start time that cannot be read", alive(fpm, "")),
            ("gone", Liveness::Gone),
            ("unknown", Liveness::Unknown),
        ];
        for (what, now) in rejected {
            assert_eq!(worker_of(&master, &accepted, 4301, &now), None, "{what}");
        }
        // The master's own pid, whatever it runs now.
        let now = alive(fpm, later);
        assert_eq!(worker_of(&master, &accepted, 4242, &now), None);
        // The master's executable must still be accepted (installed).
        let removed = [PathBuf::from("/data/runtimes/php/8.3.1/bin/php-fpm")];
        assert_eq!(worker_of(&master, &removed, 4301, &now), None);
    }

    #[test]
    fn lsof_output_names_the_first_listener() {
        assert_eq!(
            parse_lsof("p812\ncnginx\nf6\np900\ncother\n"),
            Some("nginx(812)".to_owned())
        );
        assert_eq!(parse_lsof(""), None);
        assert_eq!(parse_lsof("cnginx\n"), None);
    }
}
