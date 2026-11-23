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
use std::path::Path;

use semver::Version;

pub mod clients;
pub mod mysql;
pub mod postgres;
pub mod start;
pub mod valkey;

pub use postgres::*;
pub use start::*;

use crate::config::{GlobalConfig, ServerSettings};
use crate::runtimes::Runtime;
use crate::services::StopMethod;

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
/// `path` as one argument. `to_string_lossy` as in `caddy_spec`: the app data
/// dir comes from the OS and is valid Unicode in practice.
fn arg_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::orphan::Liveness;
    use crate::php::MAX_SOCKET_PATH;
    use crate::ports::PortLookup;
    use crate::recipes::Os;
    use crate::services::{Signal, StopAction, StopStep};

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
