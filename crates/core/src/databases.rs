//! PostgreSQL as a managed service (ADR 0023): its folders, the arguments of
//! `initdb` and `postgres`, how it stops, and the check before a start.
//!
//! Not behind the `full` feature: it only decides, like a Laravel policy. The
//! daemon reads the files, tests the port, and starts the processes (M6-03c).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::php::MAX_SOCKET_PATH;
use crate::services::{Signal, StopMethod};

/// PostgreSQL's port when `[postgres] port` is not set in `global.toml`. The
/// daemon never moves to another port: the project's `.env` names it.
pub const POSTGRES_PORT: u16 = 5432;

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
    pub fn new(app_data_dir: &Path, major: u64) -> Self {
        let databases = app_data_dir.join("databases").join("postgres");
        Self {
            data: databases.join(major.to_string()),
            sockets: app_data_dir.join("run"),
        }
    }
}

/// The arguments of `initdb` for a new cluster in `dirs.data`.
///
/// - `-U postgres -A trust`: the superuser `postgres`, and no password for
///   local clients. The daemon adds the superuser `root` after this (M6-03c):
///   Laravel's default `.env` connects as `root`.
/// - `--locale-provider=builtin --builtin-locale=C.UTF-8` (PostgreSQL 17 and
///   later): sorting and case rules come from PostgreSQL itself, so they are
///   the same on every OS and do not change with an OS update.
/// - `--locale=C` for what still comes from the C library (messages, number
///   and date formats). `--locale=C.UTF-8` would make initdb stop with
///   "invalid locale name" where the C library has no `C.UTF-8`: Windows,
///   and macOS releases that S-12 did not check.
pub fn initdb_args(dirs: &PostgresDirs) -> Vec<String> {
    let user = ["-U", "postgres", "-A", "trust", "-E", "UTF8", "--locale=C"];
    let builtin = ["--locale-provider=builtin", "--builtin-locale=C.UTF-8"];
    let mut args = vec!["-D".to_owned(), arg_path(&dirs.data)];
    args.extend(user.into_iter().chain(builtin).map(str::to_owned));
    args
}

/// The socket PostgreSQL makes on `port`: `<sockets>/.s.PGSQL.<port>`. `None`
/// means TCP only, as php-fpm does in [`crate::php::fpm_listen`], when:
///
/// - the socket path is longer than [`MAX_SOCKET_PATH`] bytes. Above the OS
///   limit (103 bytes on macOS, 107 on Linux) PostgreSQL stops with "could
///   not create any Unix-domain sockets";
/// - the folder cannot be one item of `unix_socket_directories`, a list: not
///   absolute (`@` is an abstract socket on Linux), not UTF-8, a `,` or a
///   `"`, or whitespace at the end.
pub fn postgres_socket(dirs: &PostgresDirs, port: u16) -> Option<PathBuf> {
    let dir = dirs.sockets.to_str()?;
    let plain = dirs.sockets.is_absolute() && !dir.contains([',', '"']) && dir.trim_end() == dir;
    let socket = dirs.sockets.join(format!(".s.PGSQL.{port}"));
    (plain && socket.as_os_str().as_encoded_bytes().len() <= MAX_SOCKET_PATH).then_some(socket)
}

/// The arguments of `postgres`: the cluster in `dirs.data`, on
/// `127.0.0.1:<port>` only, with a socket in `dirs.sockets` when
/// [`postgres_socket`] allows one. No shell reads them, so "no socket" is an
/// empty value, not the `''` of `postgresql.conf`.
pub fn postgres_args(dirs: &PostgresDirs, port: u16) -> Vec<String> {
    let sockets = postgres_socket(dirs, port).map_or_else(String::new, |_| arg_path(&dirs.sockets));
    let settings = [
        "listen_addresses=127.0.0.1".to_owned(),
        format!("port={port}"),
        format!("unix_socket_directories={sockets}"),
    ];
    let mut args = vec!["-D".to_owned(), arg_path(&dirs.data)];
    for setting in settings {
        args.extend(["-c".to_owned(), setting]);
    }
    args
}

/// SIGINT ("fast": ends the sessions), up to 10 s, then SIGQUIT
/// ("immediate"), up to 5 s, then a kill (manager's decision, 2026-11-18).
/// Never SIGTERM: PostgreSQL then waits for every client, and one idle queue
/// worker keeps it running (ADR 0023).
pub fn postgres_stop() -> StopMethod {
    StopMethod::signals([
        (Signal::Int, Duration::from_secs(10)),
        (Signal::Quit, Duration::from_secs(5)),
    ])
}

/// The pid on the first line of `postmaster.pid`, or `None` for junk: `0`, or
/// above `i32::MAX`, the largest pid. A negative pid is a single-user
/// `postgres --single`, which holds the folder too, so its sign is dropped.
pub fn postmaster_pid(text: &str) -> Option<u32> {
    let line = text.lines().next()?.trim();
    let digits = line.strip_prefix('-').unwrap_or(line);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let pid: i32 = digits.parse().ok()?;
    u32::try_from(pid).ok().filter(|&pid| pid != 0)
}

/// Who holds the server's port, from the daemon's bind test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortCheck {
    Free,
    /// `owner` is the program's name, when the OS gives it.
    Busy {
        owner: Option<String>,
    },
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
    /// The OS could not tell.
    Unknown { pid: u32 },
}

/// The facts the daemon gathers before it starts a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartFacts {
    pub port: u16,
    pub port_check: PortCheck,
    pub pid_file: PidFileCheck,
}

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
    /// A running server still uses the data folder.
    #[error("the data folder is in use by a running server (pid {pid})")]
    DataDir { pid: u32 },
}

impl ServiceBusy {
    /// Stable error name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Port { .. } => "service_port_busy",
            Self::DataDir { .. } => "service_data_dir_busy",
        }
    }
}

/// The check before a start (ADR 0023, section 7):
///
/// 1. The pid file names a running server: `service_data_dir_busy`, even if
///    it holds the port too, because that is the exact reason. M6-04 later
///    recovers it when it is our own orphan.
/// 2. Another program holds the port: `service_port_busy`.
/// 3. Otherwise start. PostgreSQL checks its pid file again: it starts over a
///    stale one, but refuses while the pid is alive and runs as the same
///    user, even as another program ([`PidFileCheck::Other`]; M6-04).
pub fn decide_start(facts: &StartFacts) -> StartDecision {
    if let PidFileCheck::Server { pid } = facts.pid_file {
        return StartDecision::Refuse(ServiceBusy::DataDir { pid });
    }
    match &facts.port_check {
        PortCheck::Free => StartDecision::Start,
        PortCheck::Busy { owner } => StartDecision::Refuse(ServiceBusy::Port {
            port: facts.port,
            owner: owner.clone(),
        }),
    }
}

/// `path` as one argument. `to_string_lossy` as in `caddy_spec`: the app data
/// dir comes from the OS and is valid Unicode in practice.
fn arg_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{StopAction, StopStep};

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
        assert_eq!(pg18.sockets, Path::new("/marsa").join("run"));
        assert_ne!(PostgresDirs::new(Path::new("/marsa"), 17).data, pg18.data);
    }

    #[test]
    fn initdb_makes_a_trust_cluster_with_the_builtin_locale() {
        let args = initdb_args(&dirs("/m/databases/postgres/18", "/m/run"));
        let want = "-D /m/databases/postgres/18 -U postgres -A trust -E UTF8 --locale=C \
                    --locale-provider=builtin --builtin-locale=C.UTF-8";
        assert_eq!(args, words(want));
    }

    /// Unix paths: an absolute path on Windows needs a drive, and PostgreSQL
    /// on Windows is task M6-07.
    #[cfg(unix)]
    mod unix_sockets {
        use super::*;

        fn sockets_arg(folder: &str, port: u16) -> String {
            let args = postgres_args(&dirs("/d", folder), port);
            args.last().unwrap().clone()
        }

        #[test]
        fn postgres_listens_on_loopback_and_its_socket() {
            let args = postgres_args(&dirs("/m/databases/postgres/18", "/m/run"), 5433);
            let want = "-D /m/databases/postgres/18 -c listen_addresses=127.0.0.1 \
                        -c port=5433 -c unix_socket_directories=/m/run";
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
            for folder in ["/a,b/run", "/a\"b/run", "/run ", "run", "@/run"] {
                assert_eq!(sockets_arg(folder, 5432), "unix_socket_directories=");
            }
            let folder = "/Users/a/Library/Application Support/dev.marsa.Marsa/run";
            let want = format!("unix_socket_directories={folder}");
            assert_eq!(sockets_arg(folder, 5432), want);
        }
    }

    #[test]
    fn postgres_stops_with_sigint_then_sigquit_and_never_sigterm() {
        let steps = postgres_stop().steps;
        let signal = |step: &StopStep| match step.action {
            StopAction::Signal(signal) => (signal, step.wait.as_secs()),
        };
        let signals: Vec<_> = steps.iter().map(signal).collect();
        assert_eq!(signals, [(Signal::Int, 10), (Signal::Quit, 5)]);
        assert!(signals.iter().all(|(signal, _)| *signal != Signal::Term));
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
        let (port, port_check) = (5432, port_check.clone());
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
            StartDecision::Refuse(ServiceBusy::Port { port: 5432, owner })
        };
        let data_dir_busy = StartDecision::Refuse(ServiceBusy::DataDir { pid: 7 });
        for pid_file in [NoPid, Dead { pid: 7 }, Other { pid: 7 }, Unknown { pid: 7 }] {
            assert_eq!(decide(&PortCheck::Free, pid_file), StartDecision::Start);
            let herd = decide(&busy(Some("Herd")), pid_file);
            assert_eq!(herd, refuse_port(Some("Herd")));
            assert_eq!(decide(&busy(None), pid_file), refuse_port(None));
        }
        for port_check in [PortCheck::Free, busy(Some("postgres")), busy(None)] {
            assert_eq!(decide(&port_check, Server { pid: 7 }), data_dir_busy);
        }
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
        let data = ServiceBusy::DataDir { pid: 7 };
        assert_eq!(data.code(), "service_data_dir_busy");
        assert!(data.to_string().contains("pid 7"), "{data}");
    }
}
