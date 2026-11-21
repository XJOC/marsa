//! Valkey 8.1 as a managed service (ADR 0023): its folders, its config
//! file, how it stops, and how it says it is ready.
//!
//! Valkey speaks RESP over TCP: a command is an array of byte strings, each
//! with its length first. Marsa sends two fixed commands, `PING` and
//! `SHUTDOWN SAVE` ([`command`]), so it needs no client library.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{UnwritablePath, quoted_path};
use crate::services::{Signal, StopAction, StopMethod, StopStep, TcpRequest};

/// Valkey's port when `[valkey] port` is not set in `global.toml`: the one
/// Laravel's `.env` names (`REDIS_PORT=6379`).
pub const VALKEY_PORT: u16 = 6379;

/// How long Valkey has to exit after `SHUTDOWN SAVE`, and again after
/// SIGTERM (ADR 0023, section 6). A shutdown took 32 to 252 ms in the
/// measures of ADR 0023; a large `dump.rdb` takes longer to write.
const STOP_WAIT: Duration = Duration::from_secs(10);

/// For the connect, and again for the write, of `SHUTDOWN SAVE`.
const REQUEST_LIMIT: Duration = Duration::from_secs(2);

/// Valkey's folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValkeyDirs {
    /// `databases/valkey/`: `dump.rdb`, which `SHUTDOWN SAVE` writes. One
    /// folder for every version: the file format is read by newer versions.
    /// Removing a version never deletes it.
    pub data: PathBuf,
    /// `run/`, owner-only: `valkey.conf`.
    pub run: PathBuf,
}

impl ValkeyDirs {
    /// The folders under `app_data_dir`. Pass an absolute `app_data_dir`, so
    /// no path depends on the working folder of `valkey-server`.
    pub fn new(app_data_dir: &Path) -> Self {
        Self {
            data: app_data_dir.join("databases").join("valkey"),
            run: app_data_dir.join("run"),
        }
    }

    /// `run/valkey.conf`, written by the daemon before each start.
    pub fn config(&self) -> PathBuf {
        self.run.join("valkey.conf")
    }
}

/// Marsa's `valkey.conf`, written before each start, so a new port takes
/// effect and a change by hand does not last.
///
/// - `bind 127.0.0.1` only, with `protected-mode` left on: Valkey then takes
///   no client from another address even if `bind` changed.
/// - `save ""` and `appendonly no`: no snapshot in the background, so no
///   `fork` (ADR 0023). The data is written by `SHUTDOWN SAVE` at each stop
///   ([`valkey_stop`]), and by SIGTERM and SIGINT too
///   (`shutdown-on-sigterm save`): the last step of a stop, the stop of our
///   own orphan, and a Ctrl+C in the terminal of `marsad` then keep the data
///   as well. A crash loses what changed since the last stop.
/// - `enable-protected-configs no`, `enable-debug-command no`, and
///   `enable-module-command no`, written out though they are the defaults:
///   any local user can connect, and `CONFIG SET dir` with `dbfilename`
///   would let them write a file as the owner (ADR 0023, "Access").
/// - `daemonize no` and `logfile ""`: the supervisor owns the process and
///   keeps its output in `logs/valkey.log`. No `pidfile`: the daemon writes
///   its own record of the pid.
///
/// # Errors
///
/// [`UnwritablePath::Valkey`] when the data folder's path has a double
/// quote or a control character, or is not UTF-8.
pub fn valkey_conf(dirs: &ValkeyDirs, port: u16) -> Result<String, UnwritablePath> {
    Ok(format!(
        "\
# Written by Marsa before each start of Valkey (ADR 0023). A change here is
# lost at the next start.
bind 127.0.0.1
protected-mode yes
port {port}
dir {data}
save \"\"
appendonly no
shutdown-on-sigterm save
shutdown-on-sigint save
daemonize no
logfile \"\"
enable-protected-configs no
enable-debug-command no
enable-module-command no
",
        data = quoted_path(&dirs.data, UnwritablePath::Valkey)?,
    ))
}

/// The arguments of `valkey-server`: only its config file, which has it
/// all.
pub fn valkey_args(config: &Path) -> Vec<String> {
    vec![config.display().to_string()]
}

/// `words` as one RESP command: an array (`*<count>`) of bulk strings
/// (`$<bytes>`), each line ending in `\r\n`. The way every Valkey client
/// sends a command.
pub fn command(words: &[&str]) -> Vec<u8> {
    let mut bytes = format!("*{}\r\n", words.len()).into_bytes();
    for word in words {
        bytes.extend_from_slice(format!("${}\r\n", word.len()).as_bytes());
        bytes.extend_from_slice(word.as_bytes());
        bytes.extend_from_slice(b"\r\n");
    }
    bytes
}

/// Whether `reply`, the start of Valkey's answer to `PING`, is `+PONG`.
/// Only then does it take commands: while it loads `dump.rdb` it answers
/// `-LOADING`, and a server with a password `-NOAUTH`.
pub fn is_pong(reply: &[u8]) -> bool {
    reply.starts_with(b"+PONG\r\n")
}

/// The signals that stop Valkey: SIGTERM, then a kill after 10 s. With
/// `shutdown-on-sigterm save` ([`valkey_conf`]) SIGTERM writes `dump.rdb`
/// too. Alone, it is the stop of our own orphan: a request to the port
/// could reach another program (ADR 0023, section 8).
pub fn valkey_signals() -> StopMethod {
    StopMethod::signals([(Signal::Term, STOP_WAIT)])
}

/// How the supervisor stops Valkey on `port` (ADR 0023, section 6):
/// `SHUTDOWN SAVE`, sent by `marsad` itself, up to 10 s for the exit, then
/// [`valkey_signals`]. A refusal, or nothing on the port, goes on to
/// SIGTERM at once.
pub fn valkey_stop(port: u16) -> StopMethod {
    let request = TcpRequest {
        port,
        bytes: command(&["SHUTDOWN", "SAVE"]),
        name: "SHUTDOWN SAVE",
        limit: REQUEST_LIMIT,
    };
    let mut stop = valkey_signals();
    let shutdown = StopStep {
        action: StopAction::Send(request),
        wait: STOP_WAIT,
    };
    stop.steps.insert(0, shutdown);
    stop
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_folder_is_shared_by_versions_and_the_config_is_in_run() {
        let dirs = ValkeyDirs::new(Path::new("/m"));
        assert_eq!(dirs.data, Path::new("/m").join("databases").join("valkey"));
        assert_eq!(
            dirs.config(),
            Path::new("/m").join("run").join("valkey.conf")
        );
    }

    #[cfg(unix)]
    #[test]
    fn valkey_conf_as_written() {
        let dirs = ValkeyDirs::new(Path::new("/m"));
        insta::assert_snapshot!(valkey_conf(&dirs, 6379).unwrap());
    }

    /// Each line is one setting; these are the ones that keep the server on
    /// loopback, off the disk between stops, and closed to file writes.
    #[cfg(unix)]
    #[test]
    fn valkey_conf_listens_on_loopback_and_locks_its_files() {
        let text = valkey_conf(&ValkeyDirs::new(Path::new("/m")), 6380).unwrap();
        let settings: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
        for line in [
            "bind 127.0.0.1",
            "protected-mode yes",
            "port 6380",
            "save \"\"",
            "appendonly no",
            "daemonize no",
            "logfile \"\"",
            "enable-protected-configs no",
            "enable-debug-command no",
            "enable-module-command no",
            "shutdown-on-sigterm save",
            "shutdown-on-sigint save",
        ] {
            assert!(settings.contains(&line), "{line}: {text}");
        }
        let names: Vec<&str> = settings
            .iter()
            .filter_map(|line| line.split_whitespace().next())
            .collect();
        // No pid file of its own, no socket, no module.
        for absent in ["pidfile", "unixsocket", "loadmodule", "include"] {
            assert!(!names.contains(&absent), "{absent}");
        }
        // One of each: nothing later overrides them.
        for name in ["bind", "protected-mode", "dir", "port", "save"] {
            let count = names.iter().filter(|n| **n == name).count();
            assert_eq!(count, 1, "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_data_folder_is_quoted_and_a_path_it_cannot_hold_is_refused() {
        let conf = |root: &str| valkey_conf(&ValkeyDirs::new(Path::new(root)), 6379);
        let text = conf("/Users/a/Library/Application Support/x #1\\y").unwrap();
        let want = "\ndir \"/Users/a/Library/Application Support/x #1\\\\y/databases/valkey\"\n";
        assert!(text.contains(want), "{text}");
        assert!(conf("/Users/o'brien").is_ok());
        for root in ["/a\"b", "/a\nb", "/a\rb", "/a\tb", "/a\u{0}b"] {
            let err = conf(root).unwrap_err();
            assert_eq!(
                err.code(),
                crate::codes::VALKEY_PATH_UNSUPPORTED,
                "{root:?}"
            );
            assert!(err.to_string().contains("valkey.conf"), "{err}");
        }
    }

    /// A path that is not UTF-8: Unix only, where any bytes may name a
    /// file.
    #[cfg(unix)]
    #[test]
    fn a_data_folder_that_is_not_utf8_is_refused() {
        use std::os::unix::ffi::OsStrExt as _;
        let root = std::ffi::OsStr::from_bytes(b"/m\xff");
        let err = valkey_conf(&ValkeyDirs::new(Path::new(root)), 6379).unwrap_err();
        assert_eq!(err.code(), "valkey_path_unsupported");
    }

    #[test]
    fn valkey_server_gets_its_config_file_only() {
        let config = Path::new("/m/run/valkey.conf");
        assert_eq!(valkey_args(config), ["/m/run/valkey.conf"]);
    }

    #[test]
    fn commands_are_resp_arrays_of_bulk_strings() {
        let shutdown = command(&["SHUTDOWN", "SAVE"]);
        assert_eq!(shutdown, b"*2\r\n$8\r\nSHUTDOWN\r\n$4\r\nSAVE\r\n");
        assert_eq!(command(&["PING"]), b"*1\r\n$4\r\nPING\r\n");
        // The length is in bytes, not characters.
        assert_eq!(command(&["é"]), "*1\r\n$2\r\né\r\n".as_bytes());
        assert_eq!(command(&[]), b"*0\r\n");
    }

    #[test]
    fn only_pong_is_ready() {
        assert!(is_pong(b"+PONG\r\n"));
        for reply in [
            &b"-LOADING Valkey is loading the dataset in memory\r\n"[..],
            b"-NOAUTH Authentication required.\r\n",
            b"-ERR unknown command\r\n",
            b"$4\r\nPONG\r\n",
            b"+PON",
            b"+PONG",
            b"+OK\r\n",
            b"",
        ] {
            assert!(!is_pong(reply), "{:?}", String::from_utf8_lossy(reply));
        }
    }

    #[test]
    fn valkey_stops_with_shutdown_save_then_sigterm_then_a_kill() {
        let stop = valkey_stop(6380);
        let [request, term] = stop.steps.as_slice() else {
            panic!("two steps: {stop:?}");
        };
        let StopAction::Send(sent) = &request.action else {
            panic!("the request comes first: {request:?}");
        };
        assert_eq!(sent.port, 6380);
        assert_eq!(sent.bytes, b"*2\r\n$8\r\nSHUTDOWN\r\n$4\r\nSAVE\r\n");
        assert_eq!(sent.name, "SHUTDOWN SAVE");
        assert_eq!(sent.limit, Duration::from_secs(2));
        assert_eq!(request.wait, Duration::from_secs(10));
        assert_eq!(term.action, StopAction::Signal(Signal::Term));
        assert_eq!(term.wait, Duration::from_secs(10));
        // 10 s, the connect and the write (2 s each), then 10 s.
        assert_eq!(stop.total_wait(), Duration::from_secs(24));
        // An orphan gets the signals only.
        assert_eq!(valkey_signals().steps, std::slice::from_ref(term));
    }
}
