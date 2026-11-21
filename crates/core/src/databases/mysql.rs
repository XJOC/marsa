//! MySQL 8.4 as a managed service (ADR 0023): its folders, its `my.cnf`,
//! the first init, and its own rule for the check before a start.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{NoSocket, PidFileCheck, ServiceBusy, StartDecision, StartFacts, decide_start};
use crate::php::MAX_SOCKET_PATH;
use crate::services::{Signal, StopMethod};

/// MySQL's port when `[mysql] port` is not set in `global.toml`.
pub const MYSQL_PORT: u16 = 3306;

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

/// A path that `my.cnf` cannot hold: one with a double quote or a control
/// character, or not UTF-8. Its own error, since no start can go on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{} cannot be written to my.cnf: it has a double quote or a control character",
    .0.display()
)]
pub struct UnwritablePath(pub PathBuf);

impl UnwritablePath {
    /// Stable error name for the UI.
    pub fn code(&self) -> &'static str {
        crate::codes::MYSQL_PATH_UNSUPPORTED
    }
}

/// Whether `text` has a character that a double-quoted value of `my.cnf`
/// cannot hold: a double quote, or a control character (a line break would
/// start a new setting).
fn odd(text: &str) -> bool {
    text.contains(|c: char| c == '"' || c.is_control())
}

/// `path` as a value of `my.cnf`: in double quotes, so a `#` is not a
/// comment and spaces at its ends stay, with `\` written `\\` (MySQL reads
/// escapes in option values).
fn cnf_path(path: &Path) -> Result<String, UnwritablePath> {
    let text = path
        .to_str()
        .filter(|text| !odd(text))
        .ok_or_else(|| UnwritablePath(path.to_path_buf()))?;
    Ok(format!("\"{}\"", text.replace('\\', "\\\\")))
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
/// - `performance_schema=OFF` and `innodb_buffer_pool_size=64M`: about
///   180 MB idle instead of about 450 MB (S-12).
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

/// The arguments of the first init, in the empty folder `data`:
/// `--defaults-file` first, as `mysqld` requires, then
/// `--initialize-insecure` (`root@localhost` with no password), then
/// [`ROOT_TCP_SQL`] from `init_sql`, then the temp folder over the
/// `datadir` of `my.cnf`.
pub fn mysql_init_args(config: &Path, init_sql: &Path, data: &Path) -> Vec<String> {
    vec![
        format!("--defaults-file={}", config.display()),
        "--initialize-insecure".to_owned(),
        format!("--init-file={}", init_sql.display()),
        format!("--datadir={}", data.display()),
    ]
}

/// The arguments of the server: only `--defaults-file`, which has it all.
pub fn mysqld_args(config: &Path) -> Vec<String> {
    vec![format!("--defaults-file={}", config.display())]
}

/// SIGTERM, then a kill after 30 s (ADR 0023). SIGTERM ends the sessions
/// and exits in 1 to 3.4 s with a client connected (S-12).
pub fn mysql_stop() -> StopMethod {
    StopMethod::signals([(Signal::Term, Duration::from_secs(30))])
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

/// The check before a start of MySQL: [`decide_start`], and a pid that the
/// OS cannot tell refuses too. A second `mysqld` on the same data folder
/// waits about 100 s for the InnoDB lock before it fails (S-12), so only a
/// pid known to be dead or another program's lets it start.
pub fn decide_mysql_start(facts: &StartFacts) -> StartDecision {
    match facts.pid_file {
        PidFileCheck::Unknown { pid } => {
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
        let init = mysql_init_args(
            config,
            Path::new("/m/run/mysql-init.sql"),
            Path::new("/m/databases/mysql/.8.4.init-1"),
        );
        assert_eq!(
            init,
            [
                "--defaults-file=/m/run/mysql.cnf",
                "--initialize-insecure",
                "--init-file=/m/run/mysql-init.sql",
                "--datadir=/m/databases/mysql/.8.4.init-1",
            ]
        );
        assert_eq!(mysqld_args(config), ["--defaults-file=/m/run/mysql.cnf"]);
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
        assert_eq!(
            decide_mysql_start(&facts(PidFileCheck::Unknown { pid: 7 })),
            busy
        );
        assert_eq!(
            decide_mysql_start(&facts(PidFileCheck::Server { pid: 7 })),
            busy
        );
        for free in [
            PidFileCheck::NoPid,
            PidFileCheck::Dead { pid: 7 },
            PidFileCheck::Other { pid: 7 },
        ] {
            assert_eq!(decide_mysql_start(&facts(free)), StartDecision::Start);
        }
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
