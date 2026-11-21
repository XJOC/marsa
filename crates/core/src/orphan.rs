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
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::caddy::Dial;
use crate::ps::{StartTime, started_order};

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
    /// The start time as the OS reported it. Without it a reused pid that
    /// runs the same program would pass (ADR 0009, R5e).
    pub started: StartTime,
}

/// What the OS says about a pid right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Liveness {
    /// No such process, or a zombie.
    Gone,
    /// `exe` is the path it was started with (`argv[0]` on macOS).
    Alive { exe: PathBuf, started: StartTime },
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
        let found = format!(
            "{found} (started {:?}, not {:?})",
            started.to_string(),
            record.started.to_string()
        );
        return PidfileState::Mismatch { pid, found };
    }
    PidfileState::Ours { pid }
}

/// One child of a process, as the process table lists it: its pid and its
/// start time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildProcess {
    pub pid: u32,
    pub started: StartTime,
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
    if started_order(*started, master.started)? == Ordering::Less {
        return None;
    }
    Some(PidRecord {
        pid,
        exe: exe.clone(),
        started: *started,
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

/// Reads `lsof -FptPnT` output (macOS) for the processes that hold the
/// listener at `listen` open: a Unix socket bound to that very path, or a
/// TCP socket in the LISTEN state on `127.0.0.1:<port>`. A child that
/// inherited the listener holds it too. Sorted, each pid once.
///
/// `control` is a pid whose files the same `lsof` run was asked for (the
/// daemon itself). If the output does not describe it, `lsof` did not do its
/// work, so its answer proves nothing: `None`, which is not "nobody". `lsof`
/// escapes odd characters in a name, so such a path never matches, and the
/// check fails closed.
pub fn parse_lsof_holders(text: &str, listen: &Dial, control: u32) -> Option<Vec<u32>> {
    /// The fields of one open file.
    #[derive(Default)]
    struct OpenFile<'t> {
        kind: &'t str,
        protocol: &'t str,
        name: &'t str,
        state: &'t str,
    }
    let address = match listen {
        Dial::Tcp(port) => format!("127.0.0.1:{port}"),
        Dial::Unix(_) => String::new(),
    };
    let holds = |file: &OpenFile<'_>| match listen {
        Dial::Unix(path) => file.kind == "unix" && Path::new(file.name) == path.as_path(),
        Dial::Tcp(_) => {
            file.kind == "IPv4"
                && file.protocol == "TCP"
                && file.state == "LISTEN"
                && file.name == address
        }
    };
    let mut holders = Vec::new();
    let mut described = false;
    let mut pid = None;
    let mut file = OpenFile::default();
    // A `p` line starts a process and an `f` line a file: the file before
    // either is complete. The `p` added at the end closes the last file.
    let lines = text.lines().map(|line| line.trim_end_matches('\r'));
    for line in lines.chain(["p"]) {
        if line.starts_with(['p', 'f']) {
            if let Some(pid) = pid
                && holds(&file)
                && !holders.contains(&pid)
            {
                holders.push(pid);
            }
            file = OpenFile::default();
        }
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse::<u32>().ok();
            described |= pid == Some(control);
        } else if let Some(value) = line.strip_prefix('t') {
            file.kind = value;
        } else if let Some(value) = line.strip_prefix('P') {
            file.protocol = value;
        } else if let Some(value) = line.strip_prefix('n') {
            file.name = value;
        } else if let Some(value) = line.strip_prefix("TST=") {
            file.state = value;
        }
    }
    holders.sort_unstable();
    described.then_some(holders)
}

/// The `__SO_ACCEPTCON` flag of `/proc/net/unix`: the socket listens.
const UNIX_LISTENING: u32 = 0x0001_0000;
/// `SOCK_STREAM` in the `Type` column of `/proc/net/unix`.
const UNIX_STREAM: u32 = 1;
/// `TCP_LISTEN` in the `st` column of `/proc/net/tcp`.
const TCP_LISTEN: &str = "0A";

/// Reads `/proc/net/unix` (Linux) for the inodes of the stream sockets
/// that listen at `path`, its bytes compared exactly with the path as it
/// was bound. The table is bytes: the kernel prints each path as it is, so
/// it may not be UTF-8, and a path with a newline reads as more than one
/// line. A real socket has one line, so an inode on more than one line is
/// dropped: a forged line cannot name the socket of a real one. If one of
/// those lines was at `path`, the answer is `None`: the table cannot say who
/// listens there, and a forged copy must not hide our own listener. The
/// path is the last column and may hold spaces; a line in another shape, or
/// an abstract socket (`@...`), never matches.
pub fn parse_proc_net_unix(table: &[u8], path: &[u8]) -> Option<Vec<u64>> {
    use std::collections::{BTreeMap, BTreeSet};
    // Each inode, and whether a line of it listens at `path`.
    let mut lines: BTreeMap<u64, bool> = BTreeMap::new();
    let mut repeated = BTreeSet::new();
    // The first line is the header.
    for line in table.split(|&byte| byte == b'\n').skip(1) {
        // `Num RefCount Protocol Flags Type St Inode Path`.
        let mut rest = line.trim_ascii_start();
        let mut fields: [&[u8]; 7] = [b""; 7];
        let mut complete = true;
        for field in &mut fields {
            let Some(end) = rest.iter().position(|&byte| byte == b' ') else {
                *field = rest;
                rest = b"";
                complete = false;
                break;
            };
            *field = &rest[..end];
            rest = trim_spaces(&rest[end..]);
        }
        let text = |bytes: &[u8]| std::str::from_utf8(bytes).ok().map(str::to_owned);
        let Some(inode) = text(fields[6]).and_then(|inode| inode.parse::<u64>().ok()) else {
            continue;
        };
        let hex = |bytes: &[u8]| text(bytes).and_then(|t| u32::from_str_radix(&t, 16).ok());
        let listens = hex(fields[3]).is_some_and(|flags| flags & UNIX_LISTENING != 0);
        let at_path = complete && listens && hex(fields[4]) == Some(UNIX_STREAM) && rest == path;
        if let Some(before) = lines.insert(inode, at_path) {
            repeated.insert(inode);
            lines.insert(inode, before || at_path);
        }
    }
    let mut ours = Vec::new();
    for (inode, at_path) in lines {
        match (at_path, repeated.contains(&inode)) {
            (true, true) => return None,
            (true, false) => ours.push(inode),
            (false, _) => {}
        }
    }
    Some(ours)
}

/// `bytes` without its leading spaces.
fn trim_spaces(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|&byte| byte != b' ')
        .unwrap_or(bytes.len());
    &bytes[start..]
}

/// Reads `/proc/net/tcp` (Linux) for the inodes of the TCP sockets that
/// listen on `127.0.0.1:<port>`. The address is the kernel's `u32` in host
/// byte order (`0100007F` on a little-endian machine); the port is plain hex.
pub fn parse_proc_net_tcp(text: &str, port: u16) -> Vec<u64> {
    let loopback = if cfg!(target_endian = "little") {
        "0100007F"
    } else {
        "7F000001"
    };
    let ours = [format!("{loopback}:{port:04X}")];
    text.lines()
        .skip(1)
        .filter_map(|line| {
            // `sl local_address rem_address st ... uid timeout inode`.
            let fields: Vec<&str> = line.split_whitespace().collect();
            let (local, state, inode) = (fields.get(1)?, fields.get(3)?, fields.get(9)?);
            let listens = *state == TCP_LISTEN && ours.iter().any(|ours| ours == local);
            listens.then(|| inode.parse().ok()).flatten()
        })
        .collect()
}

/// The inode of a socket from the target of a `/proc/<pid>/fd/<n>` link:
/// `socket:[<inode>]`. `None` for any other file.
pub fn socket_inode(target: &Path) -> Option<u64> {
    target
        .to_str()?
        .strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
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

    /// Lines of a real `/proc/net/unix` (Ubuntu 24.04), plus the
    /// cases around ours.
    #[test]
    fn proc_net_unix_gives_the_listeners_at_the_exact_path() {
        let path = b"/home/u/.local/share/Marsa Data/run/php-fpm-8.4.sock";
        let text = "\
Num       RefCount Protocol Flags    Type St Inode Path
0000000000000000: 00000002 00000000 00010000 0001 01 23456 /home/u/.local/share/Marsa Data/run/php-fpm-8.4.sock
0000000000000000: 00000003 00000000 00000000 0001 03 23457 /home/u/.local/share/Marsa Data/run/php-fpm-8.4.sock
0000000000000000: 00000002 00000000 00010000 0002 01 23458 /home/u/.local/share/Marsa Data/run/php-fpm-8.4.sock
0000000000000000: 00000002 00000000 00010000 0001 01 23459 /home/u/.local/share/Marsa Data/run/php-fpm-8.4.sock2
0000000000000000: 00000002 00000000 00010000 0001 01 23460 @/home/u/.local/share/Marsa Data/run/php-fpm-8.4.sock
0000000000000000: 00000002 00000000 00010000 0001 01 23461
0000000000000000: 00000002 00000000 00010000 0001 01 23462 /home/u/.local/share/Marsa Data/run/php-fpm-8.4.sock
garbage
";
        // Listening stream sockets only: not a connected one (no
        // `__SO_ACCEPTCON`), not a datagram one, not a longer path, not an
        // abstract socket, not one with no path.
        let found = parse_proc_net_unix(text.as_bytes(), path);
        assert_eq!(found, Some(vec![23456, 23462]));
        assert_eq!(
            parse_proc_net_unix(text.as_bytes(), b"/home/u"),
            Some(vec![])
        );
        // The header alone, and nothing.
        let header = text.lines().next().unwrap().as_bytes();
        assert_eq!(parse_proc_net_unix(header, path), Some(vec![]));
        assert_eq!(parse_proc_net_unix(b"", path), Some(vec![]));
    }

    /// A table of listening stream sockets: `(inode, path)` each.
    fn unix_table(sockets: &[(u64, &[u8])]) -> Vec<u8> {
        let mut table = b"Num       RefCount Protocol Flags    Type St Inode Path\n".to_vec();
        for (inode, path) in sockets {
            let start = format!("0000000000000000: 00000002 00000000 00010000 0001 01 {inode} ");
            table.extend_from_slice(start.as_bytes());
            table.extend_from_slice(path);
            table.push(b'\n');
        }
        table
    }

    /// Another user's socket paths are bytes as they are: one that is not
    /// UTF-8 spoils nothing, and one with a newline that forges our line
    /// for another socket's inode does not make it ours. A forged copy of
    /// our own listener's line makes the table say nothing about our path.
    #[test]
    fn proc_net_unix_survives_odd_paths_of_others() {
        let path: &[u8] = b"/run/marsa/php-fpm-8.4.sock";
        let other_text: &[u8] = b"/tmp/\xff\xfe";
        // 103: its real line, then a line forged by its path; 104 the other
        // way round.
        let table = unix_table(&[
            (101, other_text),
            (102, path),
            (103, b"/tmp/x"),
            (103, path),
            (104, path),
            (104, b"/tmp/y"),
        ]);
        assert_eq!(parse_proc_net_unix(&table, path), None);
        // Repeated lines elsewhere: only the real one at our path.
        let table = unix_table(&[
            (101, other_text),
            (102, path),
            (105, b"/tmp/z"),
            (105, b"/tmp/w"),
        ]);
        assert_eq!(parse_proc_net_unix(&table, path), Some(vec![102]));
    }

    /// Lines of a real `/proc/net/tcp` (x86-64).
    #[test]
    fn proc_net_tcp_gives_the_listeners_on_127_0_0_1() {
        let text = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:4E9C 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 31001 1 0000000000000000 100 0 0 10 0
   1: 00000000:4E9C 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 31002 1 0000000000000000 100 0 0 10 0
   2: 0100007F:4E9C 0100007F:D4A2 01 00000000:00000000 00:00000000 00000000  1000        0 31003 1 0000000000000000 20 4 30 10 -1
   3: 0100007F:4E9D 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 31004 1 0000000000000000 100 0 0 10 0
   4: 7F000001:4E9C 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 31005 1 0000000000000000 100 0 0 10 0
   5: 0100007F:4E9C
";
        // 20124 is 0x4E9C. Not `0.0.0.0`, not an established connection,
        // not another port; the other byte order is another address.
        let want = if cfg!(target_endian = "little") {
            31001
        } else {
            31005
        };
        assert_eq!(parse_proc_net_tcp(text, 20124), [want]);
        assert!(parse_proc_net_tcp(text, 80).is_empty());
        assert!(parse_proc_net_tcp("", 20124).is_empty());
    }

    #[test]
    fn a_socket_link_names_its_inode() {
        assert_eq!(socket_inode(Path::new("socket:[23456]")), Some(23456));
        for other in [
            "pipe:[23456]",
            "/dev/null",
            "socket:[]",
            "socket:[12a]",
            "socket:[1",
        ] {
            assert_eq!(socket_inode(Path::new(other)), None, "{other}");
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
    /// `Wed Nov 4 03:31:27 2026`.
    const STARTED: StartTime = StartTime::Utc(1_793_763_087);

    fn at(text: &str) -> StartTime {
        text.parse().expect("a start time")
    }

    fn record() -> PidRecord {
        PidRecord {
            pid: 7,
            exe: PathBuf::from(CADDY),
            started: STARTED,
        }
    }

    fn alive(exe: &str, started: StartTime) -> Liveness {
        Liveness::Alive {
            exe: PathBuf::from(exe),
            started,
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
        let state = classify(&alive(CADDY, at("Wed Nov  4 03:31:28 2026")));
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
    fn a_worker_runs_the_masters_executable_and_started_no_earlier() {
        let master = PidRecord {
            pid: 4242,
            exe: PathBuf::from("/data/runtimes/php/8.4.3/bin/php-fpm"),
            started: at("Wed Nov 4 03:31:27 2026"),
        };
        let accepted = [
            master.exe.clone(),
            PathBuf::from("/data/runtimes/php/8.3.1/bin/php-fpm"),
        ];
        let fpm = "/data/runtimes/php/8.4.3/bin/php-fpm";
        let same = at("Wed Nov 4 03:31:27 2026");
        let later = at("Wed Nov 4 03:31:28 2026");
        for started in [same, later] {
            let now = alive(fpm, started);
            let want = PidRecord {
                pid: 4301,
                exe: master.exe.clone(),
                started,
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
                alive(fpm, at("Wed Nov 4 03:31:26 2026")),
            ),
            (
                "a start time of another form",
                alive(fpm, StartTime::Ticks(987_654)),
            ),
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

    #[test]
    fn lsof_holders_of_a_socket_are_the_pids_with_that_very_socket_open() {
        let socket = Dial::Unix(PathBuf::from("/data/a b/run/php-8.4.sock"));
        // Two workers inherited the socket. Pid 500 holds another pool's
        // socket, pid 600 a plain file of that name, pid 601 a connection,
        // and pid 7 is the daemon that asked.
        let out = "p4302\nf7\ntunix\nn/data/a b/run/php-8.4.sock\n\
                   f8\ntunix\nn/data/a b/run/php-8.4.sock\n\
                   p4301\nf0\ntunix\nn->0x66963a7f3f40cd83\n\
                   f3\ntunix\nn/data/a b/run/php-8.4.sock\n\
                   p500\nf3\ntunix\nn/data/a b/run/php-8.3.sock\n\
                   p600\nf4\ntREG\nn/data/a b/run/php-8.4.sock\n\
                   p601\nf5\ntunix\nn->0x66963a7f3f40cd84\n\
                   p7\nfcwd\ntDIR\nn/\n";
        let holders = parse_lsof_holders(out, &socket, 7);
        assert_eq!(holders, Some(vec![4301, 4302]));
        // Nobody holds it: an empty list, since `lsof` described the daemon.
        let other = Dial::Unix(PathBuf::from("/data/a b/run/php-8.2.sock"));
        assert_eq!(parse_lsof_holders(out, &other, 7), Some(Vec::new()));
        // The last file of the output counts too.
        let last = "p7\nfcwd\ntDIR\nn/\np4301\nf3\ntunix\nn/data/a b/run/php-8.4.sock";
        assert_eq!(parse_lsof_holders(last, &socket, 7), Some(vec![4301]));
        // Without the daemon in it, the output proves nothing.
        assert_eq!(parse_lsof_holders(out, &socket, 8), None);
        assert_eq!(parse_lsof_holders("", &socket, 7), None);
    }

    #[test]
    fn lsof_holders_of_a_port_are_the_tcp_listeners_on_localhost() {
        let port = Dial::Tcp(9084);
        let listener = |pid: u32| {
            format!("p{pid}\nf4\ntIPv4\nPTCP\nn127.0.0.1:9084\nTST=LISTEN\nTQR=0\nTQS=0\n")
        };
        // After the two listeners: UDP on the same address, a connection to
        // the port, another port, IPv6, every address, and a closed socket.
        let others = "p500\nf4\ntIPv4\nPUDP\nn127.0.0.1:9084\n\
                      p501\nf5\ntIPv4\nPTCP\nn127.0.0.1:50000->127.0.0.1:9084\nTST=ESTABLISHED\n\
                      p502\nf6\ntIPv4\nPTCP\nn127.0.0.1:9085\nTST=LISTEN\n\
                      p503\nf7\ntIPv6\nPTCP\nn[::1]:9084\nTST=LISTEN\n\
                      p504\nf8\ntIPv4\nPTCP\nn*:9084\nTST=LISTEN\n\
                      p505\nf9\ntIPv4\nPTCP\nn127.0.0.1:9084\nTST=CLOSED\n\
                      p7\nfcwd\ntDIR\nn/\n";
        let out = format!("{}{}{others}", listener(4302), listener(4301));
        assert_eq!(parse_lsof_holders(&out, &port, 7), Some(vec![4301, 4302]));
        let free = Dial::Tcp(9086);
        assert_eq!(parse_lsof_holders(&out, &free, 7), Some(Vec::new()));
        assert_eq!(parse_lsof_holders(&out, &port, 8), None);
        // The fields of one file never carry over to the next.
        let split = "p7\nf4\ntIPv4\nPTCP\nTST=LISTEN\nf5\ntIPv4\nPTCP\nn127.0.0.1:9084\n";
        assert_eq!(parse_lsof_holders(split, &port, 7), Some(Vec::new()));
        // A socket path is no port, and a port no socket path.
        let socket = Dial::Unix(PathBuf::from("127.0.0.1:9084"));
        assert_eq!(parse_lsof_holders(&out, &socket, 7), Some(Vec::new()));
    }
}
