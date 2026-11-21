//! MySQL 8.4 as a managed service (ADR 0023): its folders, its `my.cnf`,
//! the first init, how it stops, and its own rule for the check before a
//! start.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{
    NoSocket, PidFileCheck, ServiceBusy, StartDecision, StartFacts, UnwritablePath, decide_start,
    odd, quoted_path,
};
use crate::php::MAX_SOCKET_PATH;
use crate::recipes::Os;
use crate::services::{Signal, StopAction, StopCommand, StopMethod, StopStep};

/// MySQL's port when `[mysql] port` is not set in `global.toml`.
pub const MYSQL_PORT: u16 = 3306;

/// How long MySQL has to exit after it was asked to stop, before a kill
/// (ADR 0023, section 6).
const STOP_WAIT: Duration = Duration::from_secs(30);

/// Windows: without it `mysqld` writes its log to `<host>.err` in the data
/// folder; with it, to stderr, which the supervisor keeps in
/// `logs/mysql.log`, as on macOS and Linux.
const CONSOLE: &str = "--console";

/// Windows: without it the `mysqld.exe` that the supervisor starts starts a
/// second one as the server and stays as its monitor (for `RESTART`), as
/// the windows-databases-e2e workflow saw. Then the process the supervisor
/// owns is not the server: a kill after the stop's wait would leave the
/// server running, and `mysqld.pid` would name another pid. With it, a
/// `RESTART` only stops the server, as `SHUTDOWN` does.
const NO_MONITOR: &str = "--no-monitor";

/// The files of one MySQL line (`8.4`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MysqlDirs {
    /// `databases/mysql/<line>/`. Removing a version never deletes it.
    pub data: PathBuf,
    /// `databases/mysql/files/`: `secure_file_priv`, the only folder that
    /// `LOAD DATA INFILE` and `SELECT ... INTO OUTFILE` may use, and only
    /// over the socket: `root` on TCP has no FILE privilege.
    pub files: PathBuf,
    /// `run/`, owner-only: the socket, `mysqld.pid`, and `mysql.cnf`.
    pub run: PathBuf,
}

impl MysqlDirs {
    /// The files of `line` under `app_data_dir`. Pass an absolute
    /// `app_data_dir`, so no path depends on the working folder of `mysqld`.
    pub fn new(app_data_dir: &Path, line: &str) -> Self {
        let mysql = app_data_dir.join("databases").join("mysql");
        Self {
            data: mysql.join(line),
            files: mysql.join("files"),
            run: app_data_dir.join("run"),
        }
    }

    /// `run/mysql.cnf`, written by the daemon before each start.
    pub fn config(&self) -> PathBuf {
        self.run.join("mysql.cnf")
    }

    /// `run/mysqld.pid`, written by `mysqld` itself.
    pub fn pid_file(&self) -> PathBuf {
        self.run.join("mysqld.pid")
    }

    /// `run/mysql-init.sql`, read by the first init only.
    pub fn init_sql(&self) -> PathBuf {
        self.run.join("mysql-init.sql")
    }

    /// The socket `run/mysql.sock`, or why there is none: then MySQL takes
    /// TCP connections only, as php-fpm does in [`crate::php::fpm_listen`].
    /// Above the OS limit (103 bytes on macOS) `mysqld` stops with "The
    /// socket file path is too long".
    ///
    /// # Errors
    ///
    /// [`NoSocket`]: the path is not UTF-8, not absolute, has a double quote
    /// or a control character, or is longer than [`MAX_SOCKET_PATH`] bytes.
    pub fn socket(&self) -> Result<PathBuf, NoSocket> {
        let socket = self.run.join("mysql.sock");
        let text = socket.to_str().ok_or(NoSocket::NotUtf8)?;
        if !socket.is_absolute() {
            return Err(NoSocket::NotAbsolute);
        }
        if odd(text) {
            return Err(NoSocket::OddCharacter);
        }
        let bytes = text.len();
        if bytes > MAX_SOCKET_PATH {
            return Err(NoSocket::TooLong { bytes });
        }
        Ok(socket)
    }
}

/// `path` as a value of `my.cnf` ([`quoted_path`]).
fn cnf_path(path: &Path) -> Result<String, UnwritablePath> {
    quoted_path(path, UnwritablePath::Mysql)
}

/// Marsa's `my.cnf` for `mysqld --defaults-file`, written before each
/// start, so no other `my.cnf` (Homebrew's `/opt/homebrew/etc/my.cnf`, the
/// user's `~/.my.cnf`) is read. `basedir` is the installed version's folder.
/// `socket`: `None` for TCP only (an empty `socket=`).
///
/// - `bind-address=127.0.0.1`, and `mysqlx=OFF`: no X protocol port.
/// - `skip-name-resolve`: a TCP client from `127.0.0.1` is
///   `root@127.0.0.1`, never `root@localhost`, which keeps every privilege
///   and is for the socket only (ADR 0023, "Access").
/// - `host_cache_size=0`: no host is ever blocked after many interrupted
///   connections. The readiness check is one, and a test suite makes many.
/// - `partial_revokes=ON`: `root@127.0.0.1` has its privileges on every
///   schema but `mysql` and `sys` ([`ROOT_TCP_SQL`]). Needed from the first
///   init on.
/// - `performance_schema=OFF`, `innodb_buffer_pool_size=64M`, and a small
///   log buffer and few InnoDB threads: under 250 MB idle on a CI runner,
///   which does not take pages back as macOS does on a busy Mac, instead
///   of about 450 MB by default (S-12).
/// - `disable_log_bin`: no binary log, so the disk does not fill in
///   development.
/// - No `log-error`: the log goes to stderr, which the supervisor keeps in
///   `logs/mysql.log`, so `marsa logs mysql` shows it, as for every service.
///
/// # Errors
///
/// [`UnwritablePath`] when a path has a quote or a control character.
pub fn my_cnf(
    dirs: &MysqlDirs,
    basedir: &Path,
    port: u16,
    socket: Option<&Path>,
) -> Result<String, UnwritablePath> {
    let socket = socket.map_or_else(|| Ok(String::new()), cnf_path)?;
    Ok(format!(
        "\
# Written by Marsa before each start of MySQL (ADR 0023). A change here is
# lost at the next start.
[mysqld]
basedir={basedir}
datadir={data}
port={port}
bind-address=127.0.0.1
mysqlx=OFF
socket={socket}
pid-file={pid}
secure_file_priv={files}
skip-name-resolve=ON
host_cache_size=0
partial_revokes=ON
performance_schema=OFF
innodb_buffer_pool_size=64M
innodb_log_buffer_size=4M
innodb_buffer_pool_instances=1
innodb_page_cleaners=1
innodb_read_io_threads=2
innodb_write_io_threads=2
innodb_parallel_read_threads=1
disable_log_bin
lc_messages=en_US
",
        basedir = cnf_path(basedir)?,
        data = cnf_path(&dirs.data)?,
        pid = cnf_path(&dirs.pid_file())?,
        files = cnf_path(&dirs.files)?,
    ))
}

/// The SQL that the first init runs (`--init-file`): `root@127.0.0.1`, the
/// account of Laravel's default `.env` over TCP, with no password and every
/// privilege on every database that an app needs, but none of the
/// administrative ones: no FILE (no `INTO OUTFILE`), SUPER, SYSTEM_USER,
/// SHUTDOWN, PROCESS, RELOAD, CREATE USER, GRANT OPTION, or replication
/// (ADR 0023, "Access").
///
/// Then none of them on the `mysql` and `sys` schemas (`partial_revokes`
/// in [`my_cnf`]): INSERT or UPDATE on `mysql.user` would create users, or
/// give FILE and SUPER to `root@127.0.0.1` from the next start on.
///
/// `root@localhost`, which `--initialize-insecure` makes, keeps every
/// privilege, and only the socket in the owner-only `run/` reaches it.
pub const ROOT_TCP_SQL: &str = "\
CREATE USER 'root'@'127.0.0.1' IDENTIFIED BY '';
GRANT SELECT, INSERT, UPDATE, DELETE, CREATE, DROP, REFERENCES, INDEX, ALTER, \
SHOW DATABASES, CREATE TEMPORARY TABLES, LOCK TABLES, EXECUTE, CREATE VIEW, SHOW VIEW, \
CREATE ROUTINE, ALTER ROUTINE, EVENT, TRIGGER ON *.* TO 'root'@'127.0.0.1';
REVOKE SELECT, INSERT, UPDATE, DELETE, CREATE, DROP, REFERENCES, INDEX, ALTER, \
CREATE TEMPORARY TABLES, LOCK TABLES, EXECUTE, CREATE VIEW, SHOW VIEW, CREATE ROUTINE, \
ALTER ROUTINE, EVENT, TRIGGER ON mysql.* FROM 'root'@'127.0.0.1';
REVOKE SELECT, INSERT, UPDATE, DELETE, CREATE, DROP, REFERENCES, INDEX, ALTER, \
CREATE TEMPORARY TABLES, LOCK TABLES, EXECUTE, CREATE VIEW, SHOW VIEW, CREATE ROUTINE, \
ALTER ROUTINE, EVENT, TRIGGER ON sys.* FROM 'root'@'127.0.0.1';
";

/// Windows only, after [`ROOT_TCP_SQL`] at the first init: SHUTDOWN for
/// `root@127.0.0.1`, so that `mysqladmin shutdown` over TCP stops MySQL
/// cleanly ([`mysql_windows_stop`]). Windows has no signals, and there no
/// socket reaches `root@localhost`: Marsa opens no named pipe or shared
/// memory, which any local user could open. SHUTDOWN only stops the server:
/// no file, no command, no account, so a local user still cannot run
/// anything as the owner (ADR 0023, "Access").
pub const ROOT_TCP_WINDOWS_SQL: &str = "GRANT SHUTDOWN ON *.* TO 'root'@'127.0.0.1';\n";

/// The SQL of the first init on `os`: [`ROOT_TCP_SQL`], and on Windows
/// [`ROOT_TCP_WINDOWS_SQL`] after it.
pub fn root_tcp_sql(os: Os) -> String {
    let mut sql = ROOT_TCP_SQL.to_owned();
    if os == Os::Windows {
        sql.push_str(ROOT_TCP_WINDOWS_SQL);
    }
    sql
}

/// The arguments of the first init, in the empty folder `data`:
/// `--defaults-file` first, as `mysqld` requires, then
/// `--initialize-insecure` (`root@localhost` with no password), then
/// [`root_tcp_sql`] from `init_sql`, then the temp folder over the
/// `datadir` of `my.cnf`. On Windows `--console` last, so the init's errors
/// reach its output.
pub fn mysql_init_args(config: &Path, init_sql: &Path, data: &Path, os: Os) -> Vec<String> {
    let mut args = vec![
        format!("--defaults-file={}", config.display()),
        "--initialize-insecure".to_owned(),
        format!("--init-file={}", init_sql.display()),
        format!("--datadir={}", data.display()),
    ];
    if os == Os::Windows {
        args.push(CONSOLE.to_owned());
    }
    args
}

/// The arguments of the server: `--defaults-file`, which has it all, and on
/// Windows `--console`, so the log reaches the supervisor, and
/// `--no-monitor`, so the process it starts is the server.
pub fn mysqld_args(config: &Path, os: Os) -> Vec<String> {
    let mut args = vec![format!("--defaults-file={}", config.display())];
    if os == Os::Windows {
        args.extend([CONSOLE, NO_MONITOR].map(str::to_owned));
    }
    args
}

/// SIGTERM, then a kill after 30 s (ADR 0023). SIGTERM ends the sessions
/// and exits in 1 to 3.4 s with a client connected (S-12).
pub fn mysql_stop() -> StopMethod {
    StopMethod::signals([(Signal::Term, STOP_WAIT)])
}

/// How the supervisor stops MySQL on Windows, which has no signals (ADR
/// 0023, section 6): `mysqladmin shutdown` as `root@127.0.0.1` on `port`,
/// up to 30 s for the exit, counted from its start, then a kill
/// (`TerminateProcess`), after which MySQL recovers its data at its next
/// start (1.2 to 1.7 s in S-12).
///
/// - `--no-defaults` first, as `mysqladmin` requires: no option file of the
///   user's (`C:\my.ini`, `.mylogin.cnf`) changes where or how it connects.
/// - `--protocol=TCP --host=127.0.0.1`: the only way in on Windows, where it
///   reaches `root@127.0.0.1` ([`ROOT_TCP_WINDOWS_SQL`] gives it SHUTDOWN).
///   Over TCP `mysqladmin` does not wait for the server's pid file to go,
///   so the supervisor waits for the exit itself.
pub fn mysql_windows_stop(mysqladmin: &Path, port: u16) -> StopMethod {
    let args = [
        "--no-defaults".to_owned(),
        "--protocol=TCP".to_owned(),
        "--host=127.0.0.1".to_owned(),
        format!("--port={port}"),
        "--user=root".to_owned(),
        "shutdown".to_owned(),
    ];
    let command = StopCommand {
        program: mysqladmin.to_path_buf(),
        args: args.to_vec(),
        name: "mysqladmin shutdown",
        on_failure: "mysqladmin logs in as root@127.0.0.1 with no password, so if root has a \
                     password now, it cannot. Then MySQL is ended after 30 s, and it recovers \
                     its data at its next start.",
    };
    StopMethod {
        steps: vec![StopStep {
            action: StopAction::Run(command),
            wait: STOP_WAIT,
        }],
    }
}

/// The pid in `mysqld.pid`: the whole file, digits only, or `None` for junk.
pub fn mysqld_pid(text: &str) -> Option<u32> {
    let digits = text.trim();
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let pid: i32 = digits.parse().ok()?;
    u32::try_from(pid).ok().filter(|&pid| pid != 0)
}

/// The check before a start of MySQL on `os`: [`decide_start`], and on
/// macOS and Linux a pid that the OS cannot tell refuses too. A second
/// `mysqld` on the same data folder waits about 100 s for the InnoDB lock
/// before it fails (S-12), so only a pid known to be dead or another
/// program's lets it start.
///
/// Windows cannot tell any pid to Marsa yet (`process::real_identity`), and
/// there a `mysqld.pid` that names a dead pid is the usual case: the Job
/// Object ends `mysqld` with a crashed `marsad`, before it deletes its file
/// (ADR 0017). So there an unknown pid starts, as for PostgreSQL. A
/// `mysqld` started by hand on the same folder then makes the new one fail
/// at the InnoDB lock (about 14 s on Windows, S-12) instead of at once.
pub fn decide_mysql_start(facts: &StartFacts, os: Os) -> StartDecision {
    match facts.pid_file {
        PidFileCheck::Unknown { pid } if os != Os::Windows => {
            StartDecision::Refuse(ServiceBusy::DataDir { pid: Some(pid) })
        }
        _ => decide_start(facts),
    }
}

/// Whether `bytes`, the first bytes a server sent on a new connection, are
/// MySQL's handshake: a packet whose payload starts with the protocol
/// version 10. `mysqld` opens its TCP port only after InnoDB and the data
/// dictionary are ready, so a handshake means it takes connections. An
/// error packet (`0xff`) is no handshake.
pub fn is_handshake(bytes: &[u8]) -> bool {
    matches!(bytes, [_, _, _, 0, 10, ..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::databases::PortCheck;

    fn dirs() -> MysqlDirs {
        MysqlDirs::new(Path::new("/m"), "8.4")
    }

    #[test]
    fn each_line_has_its_data_folder_and_the_run_files_are_in_run() {
        let dirs = dirs();
        let mysql = Path::new("/m").join("databases").join("mysql");
        assert_eq!(dirs.data, mysql.join("8.4"));
        assert_eq!(dirs.files, mysql.join("files"));
        assert_eq!(dirs.config(), Path::new("/m/run/mysql.cnf"));
        assert_eq!(dirs.pid_file(), Path::new("/m/run/mysqld.pid"));
        assert_eq!(dirs.init_sql(), Path::new("/m/run/mysql-init.sql"));
    }

    #[cfg(unix)]
    #[test]
    fn my_cnf_as_written() {
        let dirs = dirs();
        let socket = dirs.socket().unwrap();
        let basedir = Path::new("/m/runtimes/mysql/8.4.13");
        let text = my_cnf(&dirs, basedir, 3306, Some(&socket)).unwrap();
        insta::assert_snapshot!(text);
    }

    #[cfg(unix)]
    #[test]
    fn my_cnf_quotes_its_paths_and_refuses_what_it_cannot_hold() {
        let base = Path::new("/m/runtimes/mysql/8.4.13");
        let at = |root: &str| MysqlDirs::new(Path::new(root), "8.4");
        let text = my_cnf(&at("/a b/#c\\d"), base, 3307, None).unwrap();
        assert!(
            text.contains("datadir=\"/a b/#c\\\\d/databases/mysql/8.4\"\n"),
            "{text}"
        );
        assert!(text.contains("\nsocket=\npid-file="), "{text}");
        assert!(text.contains("\nport=3307\n"), "{text}");
        assert!(my_cnf(&at("/Users/o'brien"), base, 3306, None).is_ok());
        for root in ["/a\"b", "/a\nb", "/a\tb"] {
            let err = my_cnf(&at(root), base, 3306, None).unwrap_err();
            assert_eq!(err.code(), crate::codes::MYSQL_PATH_UNSUPPORTED);
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_socket_is_tcp_only_when_too_long_or_odd() {
        // 85 bytes, then `/run/mysql.sock` (15 bytes): 100, the limit.
        let fits = MysqlDirs::new(Path::new(&format!("/{}", "s".repeat(84))), "8.4");
        assert_eq!(fits.socket().unwrap().as_os_str().len(), 100);
        let long = MysqlDirs::new(Path::new(&format!("/{}", "s".repeat(85))), "8.4");
        assert_eq!(long.socket(), Err(NoSocket::TooLong { bytes: 101 }));
        let odd = MysqlDirs::new(Path::new("/a\nb"), "8.4");
        assert_eq!(odd.socket(), Err(NoSocket::OddCharacter));
    }

    #[test]
    fn root_on_tcp_gets_no_administrative_privilege() {
        let grants: Vec<&str> = ROOT_TCP_SQL.split([',', '\n']).map(str::trim).collect();
        for power in [
            "FILE",
            "SUPER",
            "SYSTEM_USER",
            "SHUTDOWN",
            "PROCESS",
            "RELOAD",
            "CREATE USER",
            "GRANT OPTION",
            "ALL",
            "ALL PRIVILEGES",
        ] {
            assert!(!grants.contains(&power), "{power}");
        }
        assert!(!ROOT_TCP_SQL.contains("WITH GRANT"));
        assert!(!ROOT_TCP_SQL.contains("REPLICATION"));
        assert!(ROOT_TCP_SQL.contains("TO 'root'@'127.0.0.1';"));
        // One statement per line.
        assert_eq!(ROOT_TCP_SQL.lines().count(), 4);
        // What is granted on *.* is revoked on the two system schemas.
        let granted = |statement: &str, on: &str| {
            let line = ROOT_TCP_SQL
                .lines()
                .find(|line| line.starts_with(statement) && line.contains(on));
            let line = line.unwrap_or_else(|| panic!("{statement} {on}"));
            let list = line
                .split_once(' ')
                .unwrap()
                .1
                .split(" ON ")
                .next()
                .unwrap();
            list.split(", ")
                .filter(|p| *p != "SHOW DATABASES")
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let all = granted("GRANT", "*.*");
        assert_eq!(granted("REVOKE", "mysql.*"), all);
        assert_eq!(granted("REVOKE", "sys.*"), all);
    }

    #[test]
    fn the_defaults_file_comes_first() {
        let config = Path::new("/m/run/mysql.cnf");
        let init = |os| {
            mysql_init_args(
                config,
                Path::new("/m/run/mysql-init.sql"),
                Path::new("/m/databases/mysql/.8.4.init-1"),
                os,
            )
        };
        let unix = [
            "--defaults-file=/m/run/mysql.cnf",
            "--initialize-insecure",
            "--init-file=/m/run/mysql-init.sql",
            "--datadir=/m/databases/mysql/.8.4.init-1",
        ];
        assert_eq!(init(Os::Mac), unix);
        assert_eq!(init(Os::Linux), unix);
        let mut windows = unix.to_vec();
        windows.push("--console");
        assert_eq!(init(Os::Windows), windows);
        let server = ["--defaults-file=/m/run/mysql.cnf"];
        assert_eq!(mysqld_args(config, Os::Mac), server);
        assert_eq!(mysqld_args(config, Os::Linux), server);
        let windows = [
            "--defaults-file=/m/run/mysql.cnf",
            "--console",
            "--no-monitor",
        ];
        assert_eq!(mysqld_args(config, Os::Windows), windows);
    }

    #[test]
    fn mysql_stops_with_sigterm_then_a_kill_after_30_s() {
        let stop = mysql_stop();
        assert_eq!(stop.total_wait(), Duration::from_secs(30));
        assert_eq!(
            stop,
            StopMethod::signals([(Signal::Term, Duration::from_secs(30))])
        );
    }

    /// Windows: `mysqladmin shutdown` by its absolute path, with fixed
    /// arguments and no option file, then the kill after 30 s.
    #[test]
    fn on_windows_mysqladmin_shuts_it_down_over_tcp_then_a_kill_after_30_s() {
        let mysqladmin = Path::new(r"C:\m\runtimes\mysql\8.4.13\bin\mysqladmin.exe");
        let stop = mysql_windows_stop(mysqladmin, 3307);
        let [step] = stop.steps.as_slice() else {
            panic!("one step: {stop:?}");
        };
        assert_eq!(step.wait, Duration::from_secs(30));
        let StopAction::Run(command) = &step.action else {
            panic!("a stop program: {step:?}");
        };
        assert_eq!(command.program, mysqladmin);
        let args = "--no-defaults --protocol=TCP --host=127.0.0.1 --port=3307 --user=root shutdown";
        assert_eq!(command.args, args.split(' ').collect::<Vec<_>>());
        assert_eq!(command.name, "mysqladmin shutdown");
        // The log line says why a password stops it, and what follows.
        assert!(command.on_failure.contains("password"), "{command:?}");
        assert!(command.on_failure.contains("30 s"), "{command:?}");
    }

    /// Only Windows gives root over TCP SHUTDOWN, its one way to a clean
    /// stop; the rest of the access rules stay as they are.
    #[test]
    fn only_windows_gives_root_on_tcp_shutdown() {
        assert_eq!(root_tcp_sql(Os::Mac), ROOT_TCP_SQL);
        assert_eq!(root_tcp_sql(Os::Linux), ROOT_TCP_SQL);
        let windows = root_tcp_sql(Os::Windows);
        let added = windows.strip_prefix(ROOT_TCP_SQL).unwrap();
        assert_eq!(added, "GRANT SHUTDOWN ON *.* TO 'root'@'127.0.0.1';\n");
        assert!(!windows.contains("FILE"));
        assert!(!windows.contains("SUPER"));
    }

    #[test]
    fn mysqld_pid_is_the_whole_file() {
        assert_eq!(mysqld_pid("4242\n"), Some(4242));
        assert_eq!(mysqld_pid(" 17 \r\n"), Some(17));
        for junk in ["", "0", "-17", "17 18", "4242\nx", "2147483648", "x"] {
            assert_eq!(mysqld_pid(junk), None, "{junk:?}");
        }
    }

    #[test]
    fn a_pid_that_cannot_be_told_refuses_mysql() {
        let facts = |pid_file| StartFacts {
            port: 3306,
            port_check: PortCheck::Free,
            pid_file,
        };
        let busy = StartDecision::Refuse(ServiceBusy::DataDir { pid: Some(7) });
        for os in [Os::Mac, Os::Linux, Os::Windows] {
            let server = facts(PidFileCheck::Server { pid: 7 });
            assert_eq!(decide_mysql_start(&server, os), busy);
            for free in [
                PidFileCheck::NoPid,
                PidFileCheck::Dead { pid: 7 },
                PidFileCheck::Other { pid: 7 },
            ] {
                assert_eq!(decide_mysql_start(&facts(free), os), StartDecision::Start);
            }
        }
        let unknown = facts(PidFileCheck::Unknown { pid: 7 });
        assert_eq!(decide_mysql_start(&unknown, Os::Mac), busy);
        assert_eq!(decide_mysql_start(&unknown, Os::Linux), busy);
        // Windows tells no pid, and the file of a run the Job Object ended
        // must not keep MySQL from its next start.
        let start = decide_mysql_start(&unknown, Os::Windows);
        assert_eq!(start, StartDecision::Start);
    }

    #[test]
    fn only_a_protocol_10_greeting_is_a_handshake() {
        assert!(is_handshake(&[0x4a, 0, 0, 0, 10, b'8']));
        // An error packet: "Host is not allowed", for one.
        assert!(!is_handshake(&[0x44, 0, 0, 0, 0xff, 0x6a]));
        assert!(!is_handshake(&[0x4a, 0, 0, 1, 10]));
        assert!(!is_handshake(&[0x4a, 0, 0]));
        assert!(!is_handshake(b"N"));
    }
}
