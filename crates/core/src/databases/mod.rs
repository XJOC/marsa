//! The database servers as managed services (ADR 0023): which servers there
//! are ([`ServerKind`]), PostgreSQL's folders, the arguments of `initdb` and
//! `postgres`, how it stops, and the check before a start. MySQL's own
//! part is in [`mysql`], and Valkey's in [`valkey`]. Their client programs,
//! which run through the shim, are in [`clients`].
//!
//! Not behind the `full` feature: it only decides, like a Laravel policy. The
//! daemon reads the files, tests the port, and starts the processes.
//! The shim reads it too.

use crate::data_dir::DataDir;
use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;

pub mod clients;
pub mod mysql;
pub mod valkey;

use crate::config::{GlobalConfig, ServerSettings};
use crate::orphan::Liveness;
use crate::php::MAX_SOCKET_PATH;
use crate::ports::PortLookup;
use crate::recipes::Os;
use crate::runtimes::Runtime;
use crate::services::{Signal, StopAction, StopCommand, StopMethod, StopStep};

/// PostgreSQL's port when `[postgres] port` is not set in `global.toml`. The
/// daemon never moves to another port: the project's `.env` names it.
pub const POSTGRES_PORT: u16 = 5432;

/// A database server that `marsad` runs as a service (ADR 0023). Each has
/// its own runtime, port, table in `global.toml`, and stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ServerKind {
    Postgres,
    Mysql,
    /// The key-value server of Laravel's Redis cache and queue.
    Valkey,
}

impl ServerKind {
    /// Every server, in the order lists show them.
    pub const ALL: [ServerKind; 3] = [ServerKind::Postgres, ServerKind::Mysql, ServerKind::Valkey];

    /// The service id: the name in `marsa services`, the log file
    /// `logs/<id>.log`, the table in `global.toml`, and `run/<id>.pid`.
    pub fn id(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Mysql => "mysql",
            Self::Valkey => "valkey",
        }
    }

    /// The server's name in messages.
    pub fn name(self) -> &'static str {
        match self {
            Self::Postgres => "PostgreSQL",
            Self::Mysql => "MySQL",
            Self::Valkey => "Valkey",
        }
    }

    /// The server with this id.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }

    /// The server whose versions `runtime` installs.
    pub fn of_runtime(runtime: Runtime) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.runtime() == runtime)
    }

    /// Where its versions are installed.
    pub fn runtime(self) -> Runtime {
        match self {
            Self::Postgres => Runtime::Postgres,
            Self::Mysql => Runtime::Mysql,
            Self::Valkey => Runtime::Valkey,
        }
    }

    /// The port when its table in `global.toml` sets none. The daemon never
    /// moves to another port: the project's `.env` names it.
    pub fn default_port(self) -> u16 {
        match self {
            Self::Postgres => POSTGRES_PORT,
            Self::Mysql => mysql::MYSQL_PORT,
            Self::Valkey => valkey::VALKEY_PORT,
        }
    }

    /// The name of the server program in the recipe: the program the
    /// supervisor runs, and the one a pid file must run to be ours.
    pub fn server_program(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Mysql => "mysqld",
            Self::Valkey => "valkey-server",
        }
    }

    /// The signals that stop it, before a final kill: how the supervisor
    /// stops it, and how our own orphan is stopped. Valkey gets
    /// `SHUTDOWN SAVE` on its port first from the supervisor
    /// ([`valkey::valkey_stop`]), but never as an orphan: a port does not
    /// prove which process listens (ADR 0023, section 8).
    pub fn stop(self) -> StopMethod {
        match self {
            Self::Postgres => postgres_stop(),
            Self::Mysql => mysql::mysql_stop(),
            Self::Valkey => valkey::valkey_signals(),
        }
    }

    /// Its table in `global.toml`.
    pub fn settings(self, global: &GlobalConfig) -> &ServerSettings {
        match self {
            Self::Postgres => &global.postgres,
            Self::Mysql => &global.mysql,
            Self::Valkey => &global.valkey,
        }
    }

    /// Its table in `global.toml`, to change.
    pub fn settings_mut(self, global: &mut GlobalConfig) -> &mut ServerSettings {
        match self {
            Self::Postgres => &mut global.postgres,
            Self::Mysql => &mut global.mysql,
            Self::Valkey => &mut global.valkey,
        }
    }

    /// The line `version` belongs to, as `default` names it: the major for
    /// PostgreSQL (`"18"`), the major and minor for MySQL (`"8.4"`, an LTS
    /// line) and Valkey (`"8.1"`). Each MySQL and PostgreSQL line has its own
    /// data folder.
    pub fn line(self, version: &Version) -> String {
        match self {
            Self::Postgres => version.major.to_string(),
            Self::Mysql | Self::Valkey => format!("{}.{}", version.major, version.minor),
        }
    }

    /// Whether `version` is in the line `default` names. `None` when
    /// `default` names no line: for PostgreSQL not a major number, for
    /// MySQL and Valkey not `<major>.<minor>`.
    pub fn in_line(self, default: &str, version: &Version) -> Option<bool> {
        let default = default.trim();
        match self {
            Self::Postgres => {
                let major = default.parse::<u64>().ok()?;
                Some(version.major == major)
            }
            Self::Mysql | Self::Valkey => {
                let (major, minor) = default.split_once('.')?;
                let number = |text: &str| {
                    let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
                    digits.then(|| text.parse::<u64>().ok()).flatten()
                };
                let (major, minor) = (number(major)?, number(minor)?);
                Some(version.major == major && version.minor == minor)
            }
        }
    }

    /// The version to run for `default`: the newest of `installed` in that
    /// line, or the newest of all when it is not set. `None` when no version
    /// fits, or when `default` names no line ([`Self::in_line`]).
    pub fn line_version(self, default: Option<&str>, installed: &[Version]) -> Option<Version> {
        let fits = |version: &&Version| {
            default.is_none_or(|line| self.in_line(line, version) == Some(true))
        };
        installed.iter().filter(fits).max().cloned()
    }
}

/// The folders of one PostgreSQL major line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresDirs {
    /// `databases/postgres/<major>/`. Each major has its own cluster: 18
    /// cannot open the files of 17. Removing a version never deletes it.
    pub data: PathBuf,
    /// `run/`, for the Unix socket. Never `/tmp` (ADR 0023).
    pub sockets: PathBuf,
}

impl PostgresDirs {
    /// The folders of `major` under `app_data_dir`. Pass an absolute
    /// `app_data_dir`, as for [`crate::php::PhpFiles::new`]: then no path
    /// depends on the working folder of `initdb` or `postgres`.
    pub fn new(app_data_dir: &Path, major: u64) -> Self {
        let databases = app_data_dir.join("databases").join("postgres");
        Self {
            data: databases.join(major.to_string()),
            sockets: DataDir::new(app_data_dir).run(),
        }
    }
}

/// The arguments of `initdb` for a new cluster in `dirs.data`.
///
/// - `-U postgres -A trust`: the superuser `postgres`, and no password for
///   local clients. The daemon then writes [`PG_HBA_CONF`] over initdb's
///   `pg_hba.conf` and adds the role `root` ([`ROOT_ROLE_SQL`]), which is not
///   a superuser: Laravel's default `.env` connects as `root`.
/// - `--locale-provider=builtin --builtin-locale=C.UTF-8` (PostgreSQL 17 and
///   later): sorting, `upper` and `lower`, and regular expression classes
///   come from PostgreSQL itself, so they are the same on every OS and do
///   not change with an OS update.
/// - `--locale=C` for what still comes from the C library (messages, number
///   and date formats).
/// - `--lc-ctype=C.UTF-8`, the one C library category that is not `C`. The
///   text search parser and `pg_trgm` ask the C library which characters
///   are letters: under `C` none outside ASCII, so the similarity of two
///   Arabic words was 0, and 0.57 with `C.UTF-8` (the postgres-e2e
///   workflow, on Ubuntu 24.04 and macOS 15 and 26). Those
///   answers come from each OS's tables, so the workflow checks that
///   Arabic words with diacritics split the same on each. A cluster made
///   before this keeps `C`: nothing makes a cluster again.
/// - Windows: no `--lc-ctype`, so `C` there too. Its C library has no
///   `C.UTF-8`, and the one name it has for a UTF-8 category (`.UTF-8`)
///   depends on the user's language; only `--locale=C` was tested there. So
///   `pg_trgm` finds no similarity between two Arabic words on Windows; the
///   builtin provider above still sorts and changes case the same as on the
///   other systems.
pub fn initdb_args(dirs: &PostgresDirs, os: Os) -> Vec<String> {
    let user = ["-U", "postgres", "-A", "trust", "-E", "UTF8", "--locale=C"];
    let ctype: &[&str] = if os == Os::Windows {
        &[]
    } else {
        &["--lc-ctype=C.UTF-8"]
    };
    let builtin = ["--locale-provider=builtin", "--builtin-locale=C.UTF-8"];
    let mut args = vec!["-D".to_owned(), arg_path(&dirs.data)];
    let all = user.into_iter().chain(ctype.iter().copied()).chain(builtin);
    args.extend(all.map(str::to_owned));
    args
}

/// Marsa's `pg_hba.conf`, written over initdb's in a new cluster before the
/// cluster is moved into place (ADR 0023, "Access", option B). A superuser
/// on TCP with trust would let any local user run OS commands as the owner
/// (`COPY ... TO PROGRAM`), so:
///
/// - `postgres`, the superuser, connects only over the Unix socket in `run/`,
///   a folder only the owner can open;
/// - on TCP only `root` connects, from `127.0.0.1`, and it is not a
///   superuser ([`ROOT_ROLE_SQL`]);
/// - every other line refuses: `postgres` on TCP, any other role, and
///   replication, which `all` does not match.
///
/// PostgreSQL uses the first line that matches, and refuses a connection that
/// no line matches. The reject lines say so, so nobody takes a missing line
/// for an oversight. Written once: a later change by the user stays.
pub const PG_HBA_CONF: &str = "\
# Written by Marsa when it made this cluster (ADR 0023, \"Access\").
#
# - postgres, the superuser, connects only over the Unix socket in Marsa's
#   run/ folder, which only its owner can open.
# - On TCP (127.0.0.1) only root connects. root is not a superuser.
# - Everything else is refused, replication too.
#
# TYPE  DATABASE     USER      ADDRESS        METHOD
local   all          postgres                 trust
host    all          root      127.0.0.1/32   trust
local   all          all                      reject
host    all          all       all            reject
local   replication  all                      reject
host    replication  all       all            reject
";

/// The SQL that makes the role `root` and its database `root` in a new
/// cluster, so Laravel's default `.env` (`DB_USERNAME=root`, no password)
/// and `psql -U root` work. `root` may log in and create databases, and
/// nothing more: not a superuser, so no `COPY ... TO PROGRAM`, and no
/// CREATEROLE, REPLICATION, or BYPASSRLS.
///
/// One statement per line: single-user mode ends a command at each newline
/// and runs it as its own transaction, which `CREATE DATABASE` needs. No
/// backslash, which would join two lines.
pub const ROOT_ROLE_SQL: &str = "\
CREATE ROLE root WITH LOGIN CREATEDB NOSUPERUSER NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE DATABASE root OWNER root;
";

/// The arguments of the single-user `postgres` that runs [`ROOT_ROLE_SQL`],
/// read from stdin, on the cluster in `data`: no server, no socket, no port.
///
/// - `--single` must come first, and the database, `postgres`, last.
/// - `exit_on_error=true`, as initdb runs its own single-user steps: an error
///   ends the session with exit code 1. Without it the session goes on with
///   the next line and exits with 0.
pub fn single_user_args(data: &Path) -> Vec<String> {
    let data = arg_path(data);
    let args = [
        "--single",
        "-D",
        &data,
        "-c",
        "exit_on_error=true",
        "postgres",
    ];
    args.map(str::to_owned).to_vec()
}

/// The socket PostgreSQL makes on `port`: `<sockets>/.s.PGSQL.<port>`. `None`
/// means TCP only, as php-fpm does in [`crate::php::fpm_listen`], when:
///
/// - the socket path is longer than [`MAX_SOCKET_PATH`] bytes. Above the OS
///   limit (103 bytes on macOS, 107 on Linux) PostgreSQL stops with "could
///   not create any Unix-domain sockets";
/// - the folder cannot be one item of `unix_socket_directories`, a list: not
///   absolute (`@` is an abstract socket on Linux), not UTF-8, a `,` or a
///   `"`, or whitespace at the end;
/// - the folder has a control character. PostgreSQL writes it as line 5 of
///   `postmaster.pid`, and a line break would move the lines after it.
pub fn postgres_socket(dirs: &PostgresDirs, port: u16) -> Option<PathBuf> {
    socket_for(dirs, port).ok()
}

/// Why [`postgres_socket`] gives no socket. Its text completes "PostgreSQL
/// gets no Unix socket in `<folder>`: ...".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NoSocket {
    #[error("the folder's path is not UTF-8")]
    NotUtf8,
    #[error("the folder's path is not absolute")]
    NotAbsolute,
    #[error("the folder's path has a comma, a quote, or a control character")]
    OddCharacter,
    #[error("the folder's path ends with a space")]
    TrailingSpace,
    #[error("the socket's path would be {bytes} bytes long, more than {MAX_SOCKET_PATH}")]
    TooLong { bytes: usize },
}

/// [`postgres_socket`], or the reason there is none.
///
/// # Errors
///
/// [`NoSocket`]: the first rule of [`postgres_socket`] that the folder
/// breaks.
pub fn socket_for(dirs: &PostgresDirs, port: u16) -> Result<PathBuf, NoSocket> {
    let dir = dirs.sockets.to_str().ok_or(NoSocket::NotUtf8)?;
    if !dirs.sockets.is_absolute() {
        return Err(NoSocket::NotAbsolute);
    }
    if dir.contains(|c: char| c == ',' || c == '"' || c.is_control()) {
        return Err(NoSocket::OddCharacter);
    }
    if dir.trim_end() != dir {
        return Err(NoSocket::TrailingSpace);
    }
    let socket = dirs.sockets.join(format!(".s.PGSQL.{port}"));
    let bytes = socket.as_os_str().as_encoded_bytes().len();
    if bytes > MAX_SOCKET_PATH {
        return Err(NoSocket::TooLong { bytes });
    }
    Ok(socket)
}

/// The arguments of `postgres`: the cluster in `dirs.data`, on
/// `127.0.0.1:<port>` only, with a socket in `dirs.sockets` when
/// [`postgres_socket`] allows one. No shell reads them, so "no socket" is an
/// empty value, not the `''` of `postgresql.conf`.
///
/// `unix_socket_permissions=0700`: the socket is the superuser's only way in
/// ([`PG_HBA_CONF`]), so it is the owner's alone, a second guard behind the
/// owner-only `run/` folder. PostgreSQL's default is `0777`.
///
/// `lc_messages=C`: the log is English whatever `postgresql.conf` says, so
/// [`startup_lock_conflict`] can read it.
pub fn postgres_args(dirs: &PostgresDirs, port: u16) -> Vec<String> {
    let sockets = postgres_socket(dirs, port).map_or_else(String::new, |_| arg_path(&dirs.sockets));
    let settings = [
        "listen_addresses=127.0.0.1".to_owned(),
        format!("port={port}"),
        format!("unix_socket_directories={sockets}"),
        "unix_socket_permissions=0700".to_owned(),
        "lc_messages=C".to_owned(),
    ];
    let mut args = vec!["-D".to_owned(), arg_path(&dirs.data)];
    for setting in settings {
        args.extend(["-c".to_owned(), setting]);
    }
    args
}

/// SIGINT ("fast": ends the sessions), up to 10 s, then SIGQUIT
/// ("immediate"), up to 10 s, then a kill (manager's decisions, 2026-11-18).
///
/// - Never SIGTERM: PostgreSQL then waits for every client, and one idle
///   queue worker keeps it running (ADR 0023).
/// - After SIGQUIT the postmaster kills its own stuck backends after 5 s
///   (`SIGKILL_CHILDREN_AFTER_SECS`). A kill of the postmaster at that same
///   moment could leave them running, still holding the shared memory, so
///   the next start fails. 10 s leaves the postmaster time to do it.
pub fn postgres_stop() -> StopMethod {
    StopMethod::signals([
        (Signal::Int, Duration::from_secs(10)),
        (Signal::Quit, Duration::from_secs(10)),
    ])
}

/// How the supervisor stops PostgreSQL on Windows, which has no signals
/// (ADR 0023, section 6): `pg_ctl stop -m fast` on the cluster in `data`,
/// PostgreSQL's own program, which sends the same fast shutdown as SIGINT
/// through PostgreSQL's signal pipe and waits up to 10 s for it (`-w -t
/// 10`). Then a kill (`TerminateProcess`), after which PostgreSQL recovers
/// its data at its next start (0.36 s, measured). With a client connected the
/// fast shutdown took under a second there.
pub fn postgres_windows_stop(pg_ctl: &Path, data: &Path) -> StopMethod {
    let args = [
        "stop",
        "-D",
        &arg_path(data),
        "-m",
        "fast",
        "-w",
        "-t",
        "10",
    ];
    let command = StopCommand {
        program: pg_ctl.to_path_buf(),
        args: args.map(str::to_owned).to_vec(),
        name: "pg_ctl stop",
        on_failure: "If PostgreSQL still runs 10 s after the stop began, it is ended, and it \
                     recovers its data at its next start.",
    };
    StopMethod {
        steps: vec![StopStep {
            action: StopAction::Run(command),
            wait: Duration::from_secs(10),
        }],
    }
}

/// How the supervisor stops PostgreSQL on `os`: [`postgres_stop`], or on
/// Windows [`postgres_windows_stop`] with `pg_ctl` on `data`.
pub fn postgres_stop_on(os: Os, pg_ctl: &Path, data: &Path) -> StopMethod {
    if os == Os::Windows {
        postgres_windows_stop(pg_ctl, data)
    } else {
        postgres_stop()
    }
}

/// The pid on the first line of `postmaster.pid`, or `None` for junk: `0`, or
/// above `i32::MAX`, the largest pid. A negative pid is a single-user
/// `postgres --single`, which holds the folder too, so its sign is dropped.
///
/// The daemon only reads this file. It never deletes or rewrites it, not even
/// a stale one: PostgreSQL reads its line 7, the shared memory key, to find
/// backends that outlived a dead postmaster.
pub fn postmaster_pid(text: &str) -> Option<u32> {
    let line = text.lines().next()?.trim();
    let digits = line.strip_prefix('-').unwrap_or(line);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let pid: i32 = digits.parse().ok()?;
    u32::try_from(pid).ok().filter(|&pid| pid != 0)
}

/// Which of its lock files PostgreSQL found held by a live process at its
/// start, and that process's pid when its hint gave one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockConflict {
    /// `postmaster.pid`: another server runs on the data folder.
    DataDir { pid: Option<u32> },
    /// The lock of its socket, `<socket>.lock`: another server uses the
    /// socket of this port.
    Socket { pid: Option<u32> },
    /// The shared memory block that `postmaster.pid` names: processes of an
    /// earlier server on the data folder still use it. After a SIGKILL of
    /// the postmaster, its backends end only once they see that it is gone.
    SharedMemory,
}

/// PostgreSQL's own refusal to start because a live process holds one of
/// its lock files, or the shared memory of an earlier server on its data
/// folder, from `lines`: the stderr lines, oldest first, of a run that
/// ended by itself before it took connections.
///
/// The guard against text that a client wrote is that timing, not the
/// match: PostgreSQL takes its lock files before it accepts any connection,
/// so no client can write a line in that window, and the daemon reads no
/// line of a run that got further, or that it stopped itself. A line can
/// end with text a client sent (a `STATEMENT:` line does), so never read
/// these lines from a server that runs.
///
/// Only these lines count, matched to their end, so whatever
/// `log_line_prefix` puts first does not matter:
///
/// - `FATAL:  lock file "postmaster.pid" already exists`;
/// - `FATAL:  lock file "<socket>.lock" already exists`, with `socket` from
///   [`postgres_socket`];
/// - the pid only from the very next line, PostgreSQL's hint for that
///   error: `HINT:  Is another postmaster (PID <n>) running in data
///   directory "..."?` or `... using socket file "..."?`, or `postgres`
///   for a single-user server;
/// - `FATAL:  pre-existing shared memory block (key <n>, ID <n>) is still
///   in use`, both numbers digits only.
///
/// A line that starts with a tab is the rest of the line before it (a
/// value with a line break in it), never one of these.
///
/// The text is English: [`postgres_args`] sets `lc_messages=C`.
pub fn startup_lock_conflict(lines: &[&str], socket: Option<&Path>) -> Option<LockConflict> {
    let refusal = |name: &str| format!("FATAL:  lock file \"{name}\" already exists");
    let data_dir = refusal("postmaster.pid");
    let socket_lock = socket.map(|socket| refusal(&format!("{}.lock", socket.display())));
    for (index, line) in lines.iter().enumerate() {
        if line.starts_with('\t') {
            continue;
        }
        if is_shared_memory_refusal(line) {
            return Some(LockConflict::SharedMemory);
        }
        let next = lines.get(index + 1).copied();
        let hint = next.filter(|next| !next.starts_with('\t'));
        if line.ends_with(&data_dir) {
            let pid = hint.and_then(|hint| hint_pid(hint, "running in data directory \""));
            return Some(LockConflict::DataDir { pid });
        }
        if socket_lock
            .as_deref()
            .is_some_and(|lock| line.ends_with(lock))
        {
            let pid = hint.and_then(|hint| hint_pid(hint, "using socket file \""));
            return Some(LockConflict::Socket { pid });
        }
    }
    None
}

/// Whether `line` ends with PostgreSQL's refusal to start while processes
/// of an earlier server still use the shared memory block of its data
/// folder: `FATAL:  pre-existing shared memory block (key <n>, ID <n>) is
/// still in use`, both numbers digits only.
fn is_shared_memory_refusal(line: &str) -> bool {
    let Some((_, rest)) = line.rsplit_once("FATAL:  pre-existing shared memory block (key ") else {
        return false;
    };
    let Some(numbers) = rest.strip_suffix(") is still in use") else {
        return false;
    };
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    numbers
        .split_once(", ID ")
        .is_some_and(|(key, id)| digits(key) && digits(id))
}

/// The pid in `HINT:  Is another postmaster (PID <n>) <then>...`.
fn hint_pid(line: &str, then: &str) -> Option<u32> {
    let (_, hint) = line.split_once("HINT:  Is another ")?;
    let rest = hint
        .strip_prefix("postmaster (PID ")
        .or_else(|| hint.strip_prefix("postgres (PID "))?;
    let (digits, rest) = rest.split_once(") ")?;
    rest.starts_with(then).then(|| postmaster_pid(digits))?
}

/// Who holds the server's port, from the daemon's bind test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortCheck {
    Free,
    /// `owner` is the program's name, when the OS gives it.
    Busy {
        owner: Option<String>,
    },
    /// The bind worked, but the look for a listener on every address
    /// failed: a program there could be missed, so no start.
    Unknown,
}

/// The pid that the server's own pid file names, and what runs at it now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PidFileCheck {
    /// No file, or junk in it ([`postmaster_pid`] gave `None`).
    NoPid,
    /// Nothing runs at the pid: the file is stale.
    Dead { pid: u32 },
    /// Another program runs at the pid: it was reused, for example after a
    /// reboot.
    Other { pid: u32 },
    /// The server's own program runs at the pid.
    Server { pid: u32 },
    /// The OS could not tell. PostgreSQL may start then (see [`decide_start`]),
    /// but MySQL must refuse it: a second `mysqld` waits about 100 s for the
    /// InnoDB lock before it fails.
    Unknown { pid: u32 },
}

/// The facts the daemon gathers before it starts a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartFacts {
    pub port: u16,
    pub port_check: PortCheck,
    pub pid_file: PidFileCheck,
}

/// What the daemon does before it starts a server: start it, or refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartDecision {
    Start,
    /// Start nothing, stop nothing, and report this.
    Refuse(ServiceBusy),
}

/// Why a server may not start. [`ServiceBusy::code`] gives a stable name
/// that the UI maps to a message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServiceBusy {
    /// Another program (Homebrew, Herd, DBngin, Docker) holds the port.
    #[error("port {port} is in use by {}", .owner.as_deref().unwrap_or("another program"))]
    Port { port: u16, owner: Option<String> },
    /// Who holds the port could not be checked: `lsof` failed or took too
    /// long. Not "in use", which may not be true.
    #[error("could not check who holds port {port}")]
    PortUnknown { port: u16 },
    /// A running server still uses the data folder. `pid` is `None` when
    /// PostgreSQL refused at its start and named no pid.
    #[error(
        "the data folder is in use by a running server{}",
        .pid.map(|pid| format!(" (pid {pid})")).unwrap_or_default()
    )]
    DataDir { pid: Option<u32> },
    /// PostgreSQL: processes of an earlier server on the data folder still
    /// use its shared memory ([`LockConflict::SharedMemory`]). Usually the
    /// backends of a postmaster that SIGKILL ended: they end by themselves
    /// soon, so a start a moment later works.
    #[error("processes of an earlier PostgreSQL still use the shared memory of its data folder")]
    SharedMemory,
}

impl ServiceBusy {
    /// Stable error name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Port { .. } => crate::codes::SERVICE_PORT_BUSY,
            Self::PortUnknown { .. } => crate::codes::SERVICE_PORT_UNKNOWN,
            Self::DataDir { .. } => crate::codes::SERVICE_DATA_DIR_BUSY,
            Self::SharedMemory => crate::codes::POSTGRES_SHARED_MEMORY_BUSY,
        }
    }
}

/// The check before a start (ADR 0023, section 7):
///
/// 1. The pid file names a running server: `service_data_dir_busy`, even if
///    it holds the port too, because that is the exact reason. A later
///    change recovers it when it is our own orphan.
/// 2. Another program holds the port: `service_port_busy`. Or who holds it
///    could not be checked: `service_port_unknown`.
/// 3. Otherwise start. PostgreSQL checks its pid file again: it starts over a
///    stale one, but refuses while the pid is alive and runs as the same
///    user, even as another program ([`PidFileCheck::Other`]).
///
/// [`PidFileCheck::Unknown`] starts too, for PostgreSQL only: a second
/// postmaster cannot run on the folder, because PostgreSQL's
/// `CreateLockFile` refuses while `kill(pid, 0)` finds the pid, and its
/// shared memory check refuses while backends of a dead postmaster still
/// run. MySQL has no such quick refusal and needs its own rule.
pub fn decide_start(facts: &StartFacts) -> StartDecision {
    // Every case is listed, so a new one (such as our own orphan) needs a decision.
    match facts.pid_file {
        PidFileCheck::Server { pid } => {
            let pid = Some(pid);
            return StartDecision::Refuse(ServiceBusy::DataDir { pid });
        }
        PidFileCheck::NoPid
        | PidFileCheck::Dead { .. }
        | PidFileCheck::Other { .. }
        | PidFileCheck::Unknown { .. } => {}
    }
    let port = facts.port;
    match &facts.port_check {
        PortCheck::Free => StartDecision::Start,
        PortCheck::Busy { owner } => StartDecision::Refuse(ServiceBusy::Port {
            port,
            owner: owner.clone(),
        }),
        PortCheck::Unknown => StartDecision::Refuse(ServiceBusy::PortUnknown { port }),
    }
}

/// Whether a start of `kind` is refused, before anything runs, because the
/// daemon runs with an administrator's token (`elevated`, from
/// `platform::is_elevated`, which is `false` off Windows). PostgreSQL only:
/// `postgres.exe` refuses that token, and a start through `pg_ctl start`,
/// which drops the rights, would leave a server that the supervisor does not
/// own (ADR 0023, section 7). MySQL and Valkey run either way.
pub fn refuses_elevation(kind: ServerKind, elevated: bool) -> bool {
    elevated && kind == ServerKind::Postgres
}

/// [`PortCheck`] from the daemon's two looks at the port. Busy if either
/// says so; [`PortCheck::Unknown`], which refuses too, if the bind worked
/// and the look failed:
///
/// - `bound`: a bind to `127.0.0.1:<port>` worked. Alone it misses a program
///   on `*:<port>` on macOS: std's bind sets `SO_REUSEADDR`, and there that
///   lets a bind to one address share the port with a listener on all of
///   them (Docker, for one).
/// - `lookup`: who listens on the port, on any address, as `lsof` names it
///   (macOS). Any owner means busy. A failed look fails closed.
///   Linux does not look, and there the bind already fails next to a
///   listener on all addresses.
pub fn port_check(bound: bool, lookup: PortLookup) -> PortCheck {
    match (bound, lookup) {
        (_, PortLookup::Owner(owner)) => PortCheck::Busy { owner: Some(owner) },
        (true, PortLookup::Nobody | PortLookup::NotChecked) => PortCheck::Free,
        (true, PortLookup::Failed) => PortCheck::Unknown,
        (false, _) => PortCheck::Busy { owner: None },
    }
}

/// [`PidFileCheck`] of `pid`, the pid in `postmaster.pid`, from `now`: what
/// the OS says runs there by the file the process really runs
/// (`process::real_identity`), because postgres rewrites its own argv
/// (`postgres: checkpointer`). `servers` holds `bin/postgres` of every
/// installed PostgreSQL version, and the resolved form of each.
///
/// - Gone: [`PidFileCheck::Dead`], a stale file.
/// - Cannot be told: [`PidFileCheck::Unknown`].
/// - One of `servers`: [`PidFileCheck::Server`].
/// - Any other program: [`PidFileCheck::Other`], a pid reused since.
pub fn classify_postmaster(pid: u32, now: &Liveness, servers: &[PathBuf]) -> PidFileCheck {
    match now {
        Liveness::Gone => PidFileCheck::Dead { pid },
        Liveness::Unknown => PidFileCheck::Unknown { pid },
        Liveness::Alive { exe, .. } if servers.contains(exe) => PidFileCheck::Server { pid },
        Liveness::Alive { .. } => PidFileCheck::Other { pid },
    }
}

/// A path that a server's settings file cannot hold: one with a double quote
/// or a control character, or not UTF-8, and for Valkey on Windows one that
/// is not on a drive. Its own error, since no start can go on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UnwritablePath {
    #[error(
        "{} cannot be written to my.cnf: it has a double quote or a control character",
        .0.display()
    )]
    Mysql(PathBuf),
    #[error(
        "{} cannot be written to valkey.conf: it has a double quote or a control character",
        .0.display()
    )]
    Valkey(PathBuf),
    /// Valkey on Windows reads paths in Cygwin's form, which is made from a
    /// drive letter (`/cygdrive/c/...`).
    #[error(
        "{} cannot be given to Valkey on Windows: it is not a full path on a drive such as C:",
        .0.display()
    )]
    ValkeyNoDrive(PathBuf),
}

impl UnwritablePath {
    /// Stable error name for the UI.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Mysql(_) => crate::codes::MYSQL_PATH_UNSUPPORTED,
            Self::Valkey(_) | Self::ValkeyNoDrive(_) => crate::codes::VALKEY_PATH_UNSUPPORTED,
        }
    }
}

/// Whether `text` has a character that a double-quoted value of `my.cnf` or
/// `valkey.conf` cannot hold: a double quote, or a control character (a line
/// break would start a new setting).
fn odd(text: &str) -> bool {
    text.contains(|c: char| c == '"' || c.is_control())
}

/// `path` as a value of `my.cnf` or `valkey.conf`: in double quotes, so a `#`
/// is not a comment and spaces stay part of it, with `\` written `\\` (both
/// read escapes in a quoted value). `refuse` makes the error of the file's
/// server.
fn quoted_path(
    path: &Path,
    refuse: fn(PathBuf) -> UnwritablePath,
) -> Result<String, UnwritablePath> {
    let text = path
        .to_str()
        .filter(|text| !odd(text))
        .ok_or_else(|| refuse(path.to_path_buf()))?;
    Ok(format!("\"{}\"", text.replace('\\', "\\\\")))
}

/// `path` as one argument. `to_string_lossy` as in `caddy_spec`: the app data
/// dir comes from the OS and is valid Unicode in practice.
fn arg_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(data: &str, sockets: &str) -> PostgresDirs {
        let (data, sockets) = (data.into(), sockets.into());
        PostgresDirs { data, sockets }
    }

    /// `text` split at spaces: the expected arguments.
    fn words(text: &str) -> Vec<&str> {
        text.split(' ').collect()
    }

    #[test]
    fn each_major_has_its_own_data_folder() {
        let pg18 = PostgresDirs::new(Path::new("/marsa"), 18);
        let databases = Path::new("/marsa").join("databases").join("postgres");
        assert_eq!(pg18.data, databases.join("18"));
        assert_eq!(pg18.sockets, DataDir::new("/marsa").run());
        assert_ne!(PostgresDirs::new(Path::new("/marsa"), 17).data, pg18.data);
    }

    #[test]
    fn initdb_makes_a_trust_cluster_with_the_builtin_locale() {
        let dirs = dirs("/m/databases/postgres/18", "/m/run");
        let want = "-D /m/databases/postgres/18 -U postgres -A trust -E UTF8 --locale=C \
                    --lc-ctype=C.UTF-8 --locale-provider=builtin --builtin-locale=C.UTF-8";
        assert_eq!(initdb_args(&dirs, Os::Mac), words(want));
        assert_eq!(initdb_args(&dirs, Os::Linux), words(want));
        // Windows' C library has no `C.UTF-8`: `C` there.
        let windows = "-D /m/databases/postgres/18 -U postgres -A trust -E UTF8 --locale=C \
                       --locale-provider=builtin --builtin-locale=C.UTF-8";
        assert_eq!(initdb_args(&dirs, Os::Windows), words(windows));
    }

    /// Windows: `pg_ctl stop -m fast` on the cluster, by its absolute path
    /// with fixed arguments, then the kill after 10 s.
    #[test]
    fn on_windows_pg_ctl_stops_it_fast_then_a_kill_after_10_s() {
        let pg_ctl = Path::new(r"C:\m\runtimes\postgres\18.7.0\bin\pg_ctl.exe");
        let data = Path::new(r"C:\Users\John Smith\m\databases\postgres\18");
        let stop = postgres_windows_stop(pg_ctl, data);
        assert_eq!(postgres_stop_on(Os::Windows, pg_ctl, data), stop);
        let [step] = stop.steps.as_slice() else {
            panic!("one step: {stop:?}");
        };
        assert_eq!(step.wait, Duration::from_secs(10));
        let StopAction::Run(command) = &step.action else {
            panic!("a stop program: {step:?}");
        };
        assert_eq!(command.program, pg_ctl);
        // The folder with a space is one argument: no shell reads them.
        let args = [
            "stop",
            "-D",
            r"C:\Users\John Smith\m\databases\postgres\18",
            "-m",
            "fast",
            "-w",
            "-t",
            "10",
        ];
        assert_eq!(command.args, args);
        assert_eq!(command.name, "pg_ctl stop");
        // macOS and Linux keep their signals.
        for os in [Os::Mac, Os::Linux] {
            assert_eq!(postgres_stop_on(os, pg_ctl, data), postgres_stop());
        }
    }

    #[test]
    fn pg_hba_conf_as_written() {
        insta::assert_snapshot!(PG_HBA_CONF);
    }

    /// Who gets in, and how: the first line that matches decides, as in
    /// PostgreSQL, and no match is a refusal.
    #[test]
    fn only_postgres_on_the_socket_and_root_on_tcp_get_in() {
        let lines: Vec<Vec<&str>> = PG_HBA_CONF
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .map(|line| line.split_whitespace().collect())
            .collect();
        // `all` matches every database but `replication`.
        let names = |field: &str, name: &str| field == name || field == "all";
        let databases =
            |field: &str, db: &str| field == db || (field == "all" && db != "replication");
        let method = |tcp: bool, db: &str, user: &str, address: &str| {
            let kind = if tcp { "host" } else { "local" };
            let first = lines.iter().find(|line| {
                let from = !tcp || names(line[3], address);
                line[0] == kind && databases(line[1], db) && names(line[2], user) && from
            });
            first.map_or("no line", |line| line[line.len() - 1])
        };
        assert_eq!(method(false, "postgres", "postgres", ""), "trust");
        assert_eq!(method(true, "laravel", "root", "127.0.0.1/32"), "trust");
        for (tcp, user, address) in [
            (true, "postgres", "127.0.0.1/32"),
            (true, "root", "::1/128"),
            (true, "app", "127.0.0.1/32"),
            (false, "root", ""),
            (false, "app", ""),
        ] {
            let got = method(tcp, "postgres", user, address);
            assert_eq!(got, "reject", "{user} {address}");
        }
        for tcp in [false, true] {
            let got = method(tcp, "replication", "postgres", "127.0.0.1/32");
            assert_eq!(got, "reject");
        }
        // Only those two lines let anyone in.
        let trusted = lines.iter().filter(|line| line.last() == Some(&"trust"));
        assert_eq!(trusted.count(), 2);
    }

    #[test]
    fn root_logs_in_and_creates_databases_and_is_no_superuser() {
        let lines: Vec<_> = ROOT_ROLE_SQL.lines().collect();
        let role = "CREATE ROLE root WITH LOGIN CREATEDB NOSUPERUSER NOCREATEROLE \
                    NOREPLICATION NOBYPASSRLS;";
        assert_eq!(lines, [role, "CREATE DATABASE root OWNER root;"]);
        // Each attribute that gives more is there in its `NO` form only.
        let words: Vec<_> = ROOT_ROLE_SQL
            .split(|c: char| !c.is_ascii_alphanumeric())
            .collect();
        for power in ["SUPERUSER", "CREATEROLE", "REPLICATION", "BYPASSRLS"] {
            assert!(!words.contains(&power), "{power}");
            assert!(words.contains(&format!("NO{power}").as_str()), "{power}");
        }
        assert!(!ROOT_ROLE_SQL.contains('\\'));
        assert!(ROOT_ROLE_SQL.ends_with(";\n"));
    }

    #[test]
    fn single_user_mode_stops_at_the_first_error() {
        let args = single_user_args(Path::new("/m/databases/postgres/.18.init-1"));
        let want = "--single -D /m/databases/postgres/.18.init-1 -c exit_on_error=true postgres";
        assert_eq!(args, words(want));
    }

    #[test]
    fn postgres_listens_on_loopback_only_on_every_os() {
        let args = postgres_args(&PostgresDirs::new(Path::new("/m"), 18), 5432);
        let listen: Vec<_> = args
            .windows(2)
            .filter(|pair| pair[1].starts_with("listen_"))
            .collect();
        assert_eq!(listen, [["-c", "listen_addresses=127.0.0.1"]]);
        assert!(args.windows(2).any(|pair| pair == ["-c", "port=5432"]));
        // The superuser's socket is the owner's alone.
        let owner_only = ["-c", "unix_socket_permissions=0700"];
        assert!(args.windows(2).any(|pair| pair == owner_only));
    }

    /// Unix paths: an absolute path on Windows needs a drive.
    #[cfg(unix)]
    mod unix_sockets {
        use super::*;

        fn sockets_arg(folder: &str, port: u16) -> String {
            let args = postgres_args(&dirs("/d", folder), port);
            let setting = args
                .iter()
                .find(|arg| arg.starts_with("unix_socket_directories="));
            setting.unwrap().clone()
        }

        #[test]
        fn postgres_listens_on_loopback_and_its_socket() {
            let args = postgres_args(&dirs("/m/databases/postgres/18", "/m/run"), 5433);
            let want = "-D /m/databases/postgres/18 -c listen_addresses=127.0.0.1 \
                        -c port=5433 -c unix_socket_directories=/m/run \
                        -c unix_socket_permissions=0700 -c lc_messages=C";
            assert_eq!(args, words(want));
        }

        #[test]
        fn a_socket_of_exactly_the_limit_is_kept_and_one_byte_more_is_tcp_only() {
            // A folder of 86 bytes, then `/.s.PGSQL.5432` (14 bytes): 100 bytes.
            let folder = format!("/{}", "s".repeat(MAX_SOCKET_PATH - 15));
            let socket = postgres_socket(&dirs("/d", &folder), 5432).unwrap();
            assert_eq!(socket.as_os_str().len(), MAX_SOCKET_PATH);
            assert_eq!(socket, Path::new(&folder).join(".s.PGSQL.5432"));
            // The port is in the name: 54320 has one more digit.
            assert_eq!(sockets_arg(&folder, 54320), "unix_socket_directories=");
            // 58 characters, but 101 bytes: an Arabic letter is two in UTF-8.
            let arabic = format!("/{}", "ع".repeat(43));
            assert_eq!(sockets_arg(&arabic, 5432), "unix_socket_directories=");
        }

        #[test]
        fn a_folder_that_is_not_a_plain_list_item_is_tcp_only() {
            for folder in [
                "/a,b/run",
                "/a\"b/run",
                "/a\nb/run",
                "/a\tb",
                "/run ",
                "run",
                "@/run",
            ] {
                assert_eq!(sockets_arg(folder, 5432), "unix_socket_directories=");
            }
            let folder = "/Users/a/Library/Application Support/dev.marsa.Marsa/run";
            let want = format!("unix_socket_directories={folder}");
            assert_eq!(sockets_arg(folder, 5432), want);
        }

        #[test]
        fn each_folder_without_a_socket_says_why() {
            let why = |folder: &str| socket_for(&dirs("/d", folder), 5432).unwrap_err();
            assert_eq!(why("run"), NoSocket::NotAbsolute);
            assert_eq!(why("@/run"), NoSocket::NotAbsolute);
            for folder in ["/a,b/run", "/a\"b/run", "/a\nb/run", "/a\tb"] {
                assert_eq!(why(folder), NoSocket::OddCharacter, "{folder:?}");
            }
            assert_eq!(why("/run "), NoSocket::TrailingSpace);
            // 86 bytes, then `/.s.PGSQL.5432`: one byte over.
            let deep = format!("/{}", "s".repeat(MAX_SOCKET_PATH - 14));
            assert_eq!(why(&deep), NoSocket::TooLong { bytes: 101 });
            let text = why(&deep).to_string();
            assert_eq!(
                text,
                "the socket's path would be 101 bytes long, more than 100"
            );
            let text = why("/a,b").to_string();
            assert!(text.contains("comma"), "{text}");
        }

        /// A path that is not UTF-8: Unix only, where any bytes may name a
        /// file.
        #[test]
        fn a_folder_that_is_not_utf8_has_no_socket() {
            use std::os::unix::ffi::OsStrExt as _;
            let folder = std::ffi::OsStr::from_bytes(b"/r\xffn");
            let dirs = PostgresDirs {
                data: "/d".into(),
                sockets: folder.into(),
            };
            assert_eq!(socket_for(&dirs, 5432), Err(NoSocket::NotUtf8));
            assert_eq!(postgres_socket(&dirs, 5432), None);
        }
    }

    #[test]
    fn postgres_stops_with_sigint_then_sigquit_and_never_sigterm() {
        let steps = postgres_stop().steps;
        let signal = |step: &StopStep| match step.action {
            StopAction::Signal(signal) => (signal, step.wait.as_secs()),
            StopAction::Send(_) | StopAction::Run(_) => {
                panic!("on macOS and Linux PostgreSQL is stopped by signals only")
            }
        };
        let signals: Vec<_> = steps.iter().map(signal).collect();
        assert_eq!(signals, [(Signal::Int, 10), (Signal::Quit, 10)]);
        assert!(signals.iter().all(|(signal, _)| *signal != Signal::Term));
        // Longer than the daemon's 10 s for its processes at shutdown: the
        // supervisor's handle carries this, so the shutdown waits for it.
        assert_eq!(postgres_stop().total_wait(), Duration::from_secs(20));
    }

    #[test]
    fn postmaster_pid_reads_the_first_line_and_refuses_junk() {
        let file = "4242\r\n/m/databases/postgres/18\r\n1731900000\r\n5432\n";
        assert_eq!(postmaster_pid(file), Some(4242));
        assert_eq!(postmaster_pid(" 17 \r"), Some(17));
        assert_eq!(postmaster_pid("-17\n"), Some(17));
        assert_eq!(postmaster_pid("2147483647"), Some(2_147_483_647));
        let junk = ["", "\n17", "0", "-0", "-", "+17", "17abc", "1 7", "١٧"];
        for text in junk.into_iter().chain(["2147483648", "-2147483648"]) {
            assert_eq!(postmaster_pid(text), None, "{text:?}");
        }
    }

    fn decide(port_check: &PortCheck, pid_file: PidFileCheck) -> StartDecision {
        let (port, port_check) = (5433, port_check.clone());
        decide_start(&StartFacts {
            port,
            port_check,
            pid_file,
        })
    }

    #[test]
    fn every_fact_gives_its_decision() {
        use PidFileCheck::*;
        let busy = |owner: Option<&str>| PortCheck::Busy {
            owner: owner.map(str::to_owned),
        };
        let refuse_port = |owner: Option<&str>| {
            let owner = owner.map(str::to_owned);
            StartDecision::Refuse(ServiceBusy::Port { port: 5433, owner })
        };
        let data_dir_busy = StartDecision::Refuse(ServiceBusy::DataDir { pid: Some(7) });
        for pid_file in [NoPid, Dead { pid: 7 }, Other { pid: 7 }, Unknown { pid: 7 }] {
            assert_eq!(decide(&PortCheck::Free, pid_file), StartDecision::Start);
            let herd = decide(&busy(Some("Herd")), pid_file);
            assert_eq!(herd, refuse_port(Some("Herd")));
            assert_eq!(decide(&busy(None), pid_file), refuse_port(None));
        }
        let unknown = StartDecision::Refuse(ServiceBusy::PortUnknown { port: 5433 });
        for pid_file in [NoPid, Dead { pid: 7 }, Other { pid: 7 }, Unknown { pid: 7 }] {
            assert_eq!(decide(&PortCheck::Unknown, pid_file), unknown);
        }
        let checks = [
            PortCheck::Free,
            busy(Some("postgres")),
            busy(None),
            PortCheck::Unknown,
        ];
        for port_check in checks {
            assert_eq!(decide(&port_check, Server { pid: 7 }), data_dir_busy);
        }
    }

    #[test]
    fn only_postgres_refuses_an_elevated_daemon() {
        for kind in ServerKind::ALL {
            assert!(!refuses_elevation(kind, false), "{kind:?}");
        }
        assert!(refuses_elevation(ServerKind::Postgres, true));
        assert!(!refuses_elevation(ServerKind::Mysql, true));
        assert!(!refuses_elevation(ServerKind::Valkey, true));
    }

    #[test]
    fn refusals_have_their_codes_and_messages() {
        let port = |owner: Option<&str>| ServiceBusy::Port {
            port: 5432,
            owner: owner.map(str::to_owned),
        };
        assert_eq!(port(None).code(), "service_port_busy");
        let text = port(Some("Docker")).to_string();
        assert_eq!(text, "port 5432 is in use by Docker");
        let text = port(None).to_string();
        assert_eq!(text, "port 5432 is in use by another program");
        let data = ServiceBusy::DataDir { pid: Some(7) };
        assert_eq!(data.code(), "service_data_dir_busy");
        assert!(data.to_string().contains("pid 7"), "{data}");
        let no_pid = ServiceBusy::DataDir { pid: None };
        assert_eq!(no_pid.code(), "service_data_dir_busy");
        let text = no_pid.to_string();
        assert_eq!(text, "the data folder is in use by a running server");
        let unknown = ServiceBusy::PortUnknown { port: 5432 };
        assert_eq!(unknown.code(), "service_port_unknown");
        assert_eq!(unknown.to_string(), "could not check who holds port 5432");
    }

    #[test]
    fn the_default_line_runs_its_newest_version() {
        let versions: Vec<Version> = ["17.12.0", "18.6.0", "18.7.0", "17.9.0"]
            .iter()
            .map(|text| Version::parse(text).unwrap())
            .collect();
        let line_version =
            |default, installed: &[Version]| ServerKind::Postgres.line_version(default, installed);
        let pick = |default| line_version(default, &versions).map(|v| v.to_string());
        assert_eq!(pick(Some("18")).as_deref(), Some("18.7.0"));
        assert_eq!(pick(Some(" 17 ")).as_deref(), Some("17.12.0"));
        assert_eq!(pick(None).as_deref(), Some("18.7.0"));
        for default in ["16", "18.7", "x", ""] {
            assert_eq!(pick(Some(default)), None, "{default}");
        }
        assert_eq!(line_version(None, &[]), None);
    }

    /// Valkey's line is `<major>.<minor>`, as MySQL's, so `default = "8.1"`
    /// stays on 8.1 when 9.1 is installed too.
    #[test]
    fn valkey_is_its_own_server_on_6379_with_a_major_minor_line() {
        let kind = ServerKind::Valkey;
        assert_eq!(ServerKind::from_id("valkey"), Some(kind));
        assert_eq!(ServerKind::of_runtime(Runtime::Valkey), Some(kind));
        assert_eq!((kind.name(), kind.runtime()), ("Valkey", Runtime::Valkey));
        assert_eq!(kind.default_port(), 6379);
        assert_eq!(kind.server_program(), "valkey-server");
        let mut global = GlobalConfig::default();
        kind.settings_mut(&mut global).port = Some(6380);
        assert_eq!(global.valkey.port, Some(6380));
        assert_eq!(kind.settings(&global).port, Some(6380));
        let versions: Vec<Version> = ["8.1.11", "8.1.9", "9.1.0"]
            .iter()
            .map(|text| Version::parse(text).unwrap())
            .collect();
        assert_eq!(kind.line(&versions[0]), "8.1");
        let pick = |default| kind.line_version(default, &versions).map(|v| v.to_string());
        assert_eq!(pick(Some("8.1")).as_deref(), Some("8.1.11"));
        assert_eq!(pick(None).as_deref(), Some("9.1.0"));
        for default in ["8", "8.1.11", "8.x", "", "+8.1"] {
            assert_eq!(pick(Some(default)), None, "{default}");
        }
        // As an orphan or after a refused request: SIGTERM only.
        assert_eq!(kind.stop(), valkey::valkey_signals());
    }

    /// The two lines PostgreSQL 18 prints when another process holds a lock
    /// file, with the default `log_line_prefix`.
    #[test]
    fn a_lock_file_refusal_and_its_holder_are_read_from_the_start() {
        let at = "2026-11-18 10:00:00.123 UTC [5150]";
        let socket = Path::new("/m/run/.s.PGSQL.5432");
        let conflict = |lines: &[&str]| startup_lock_conflict(lines, Some(socket));
        let data_fatal = format!("{at} FATAL:  lock file \"postmaster.pid\" already exists");
        let data_hint = format!(
            "{at} HINT:  Is another postmaster (PID 4242) running in data directory \"/m/d\"?"
        );
        let started = format!("{at} LOG:  starting PostgreSQL 18.1");
        let lines = [started.as_str(), &data_fatal, &data_hint];
        let pid = Some(4242);
        assert_eq!(conflict(&lines), Some(LockConflict::DataDir { pid }));
        // No hint right after it: no pid.
        let lines = [data_fatal.as_str(), &started, &data_hint];
        assert_eq!(conflict(&lines), Some(LockConflict::DataDir { pid: None }));

        // The lock of our socket, held by a single-user server.
        let socket_fatal = "FATAL:  lock file \"/m/run/.s.PGSQL.5432.lock\" already exists";
        let socket_hint =
            "HINT:  Is another postgres (PID 77) using socket file \"/m/run/.s.PGSQL.5432\"?";
        let lines = [socket_fatal, socket_hint];
        let pid = Some(77);
        assert_eq!(conflict(&lines), Some(LockConflict::Socket { pid }));
        // Without a socket, or with another one, that line is no refusal.
        assert_eq!(startup_lock_conflict(&lines, None), None);
        let other = Path::new("/elsewhere/.s.PGSQL.5432");
        assert_eq!(startup_lock_conflict(&lines, Some(other)), None);
        // A hint of the other kind gives no pid.
        let lines = [socket_fatal, &data_hint];
        assert_eq!(conflict(&lines), Some(LockConflict::Socket { pid: None }));
    }

    /// After a SIGKILL of the postmaster, its backends may still use the
    /// shared memory block that `postmaster.pid` names: PostgreSQL 18 then
    /// refuses with this line and its hint. Only the whole line counts.
    #[test]
    fn a_shared_memory_refusal_is_read_from_the_start() {
        let at = "2026-11-18 10:00:00.123 UTC [5150]";
        let fatal = format!(
            "{at} FATAL:  pre-existing shared memory block (key 5432001, ID 65536) is still in use"
        );
        let hint = format!(
            "{at} HINT:  Terminate any old server processes associated with data directory \"/m/d\"."
        );
        let lines = [fatal.as_str(), &hint];
        let found = startup_lock_conflict(&lines, None);
        assert_eq!(found, Some(LockConflict::SharedMemory));
        assert_eq!(
            ServiceBusy::SharedMemory.code(),
            "postgres_shared_memory_busy"
        );
        for other in [
            "FATAL:  pre-existing shared memory block (key 5432001, ID 65536) is still in use!",
            "FATAL:  pre-existing shared memory block (key x, ID 65536) is still in use",
            "FATAL:  pre-existing shared memory block (key 5432001, ID ) is still in use",
            "FATAL:  pre-existing shared memory block (key 5432001) is still in use",
            "ERROR:  pre-existing shared memory block (key 1, ID 2) is still in use",
            "\tFATAL:  pre-existing shared memory block (key 1, ID 2) is still in use",
            &hint,
        ] {
            assert_eq!(startup_lock_conflict(&[other], None), None, "{other}");
        }
    }

    /// Lines like those that a client can make a running server log. The
    /// match at their end is not the guard: a `STATEMENT:` line can end
    /// with any text a client sent. The guard is that only a run that ended
    /// by itself before it took connections is read. These lines only show
    /// that the match does not pick PostgreSQL's usual messages.
    #[test]
    fn a_lock_line_that_a_client_wrote_is_no_refusal() {
        let forged = [
            "FATAL:  pg_hba.conf rejects connection for host \"127.0.0.1\", user \"lock file \
             \"postmaster.pid\" already exists\", database \"x\", no encryption",
            "FATAL:  role \"lock file \"postmaster.pid\" already exists\" does not exist",
            "ERROR:  lock file \"postmaster.pid\" already exists at character 7",
            "LOG:  database system is ready to accept connections",
            // The rest of a value with a line break: a continuation line.
            "\tFATAL:  lock file \"postmaster.pid\" already exists",
        ];
        assert_eq!(startup_lock_conflict(&forged, None), None);
        // A continuation line where the hint goes gives no pid.
        let fatal = "FATAL:  lock file \"postmaster.pid\" already exists";
        let tabbed = "\tHINT:  Is another postmaster (PID 9) running in data directory \"/m\"?";
        let found = startup_lock_conflict(&[fatal, tabbed], None);
        assert_eq!(found, Some(LockConflict::DataDir { pid: None }));
        // Junk where the hint's pid goes.
        let fatal = "FATAL:  lock file \"postmaster.pid\" already exists";
        for hint in [
            "HINT:  Is another postmaster (PID 0) running in data directory \"/m\"?",
            "HINT:  Is another postmaster (PID x1) running in data directory \"/m\"?",
            "HINT:  Is another postmaster (PID 12 running in data directory \"/m\"?",
            "HINT:  Is another pgbouncer (PID 12) running in data directory \"/m\"?",
            "DETAIL:  Is another postmaster (PID 12) running in data directory \"/m\"?",
        ] {
            let found = startup_lock_conflict(&[fatal, hint], None);
            assert_eq!(found, Some(LockConflict::DataDir { pid: None }), "{hint}");
        }
    }

    #[test]
    fn a_port_is_free_only_when_the_bind_works_and_nobody_listens() {
        let docker = || PortLookup::Owner("com.docker.backend(77)".to_owned());
        assert_eq!(port_check(true, PortLookup::Nobody), PortCheck::Free);
        // Linux: no look, and the bind decides.
        assert_eq!(port_check(true, PortLookup::NotChecked), PortCheck::Free);
        // macOS: our bind works next to Docker on `*:5432`, but lsof sees it.
        let owner = Some("com.docker.backend(77)".to_owned());
        let busy_docker = PortCheck::Busy { owner };
        assert_eq!(port_check(true, docker()), busy_docker);
        assert_eq!(port_check(false, docker()), busy_docker);
        // The bind fails: busy, owner unknown.
        let busy = PortCheck::Busy { owner: None };
        for lookup in [
            PortLookup::Nobody,
            PortLookup::NotChecked,
            PortLookup::Failed,
        ] {
            assert_eq!(port_check(false, lookup), busy);
        }
        // The bind works, but lsof cannot tell: not free either.
        assert_eq!(port_check(true, PortLookup::Failed), PortCheck::Unknown);
    }

    #[test]
    fn the_postmaster_pid_is_ours_only_when_it_runs_an_installed_postgres() {
        let servers = [
            PathBuf::from("/m/runtimes/postgres/17.12.0/bin/postgres"),
            PathBuf::from("/m/runtimes/postgres/18.7.0/bin/postgres"),
        ];
        let alive = |exe: &str| Liveness::Alive {
            exe: exe.into(),
            started: "Wed Nov 18 10:00:00 2026".parse().unwrap(),
        };
        let classify = |now: &Liveness| classify_postmaster(4242, now, &servers);
        let pid = 4242;
        // Any installed version: an older one may still run on the folder.
        for exe in &servers {
            let now = alive(&exe.display().to_string());
            assert_eq!(classify(&now), PidFileCheck::Server { pid });
        }
        // Another PostgreSQL (Homebrew), or the pid reused by another program.
        for exe in [
            "/opt/homebrew/opt/postgresql@18/bin/postgres",
            "/Applications/Safari.app/Contents/MacOS/Safari",
            "/m/runtimes/postgres/18.7.0/bin/initdb",
        ] {
            assert_eq!(classify(&alive(exe)), PidFileCheck::Other { pid });
        }
        assert_eq!(classify(&Liveness::Gone), PidFileCheck::Dead { pid });
        assert_eq!(classify(&Liveness::Unknown), PidFileCheck::Unknown { pid });
    }
}
