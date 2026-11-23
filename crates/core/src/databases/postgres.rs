//! PostgreSQL's folders, the arguments of `initdb` and `postgres`, its socket, and
//! how it stops.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{DataDir, arg_path};
use crate::php::MAX_SOCKET_PATH;
use crate::recipes::Os;
use crate::services::{Signal, StopAction, StopCommand, StopMethod, StopStep};

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
