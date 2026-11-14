//! PHP configuration per version: a php-fpm pool on macOS and Linux, a php-cgi
//! plan on Windows (one php-cgi that keeps its own workers), and `php.ini` on
//! every system.
//!
//! This is Marsa's version of the `valet-fpm.conf` that Laravel Valet writes
//! for each PHP version, but the functions only return text or a plan. The
//! daemon writes the files and starts the processes.
//!
//! The work has two steps, so the output is the same on every OS:
//!
//! 1. [`PhpFiles::new`] builds every path with `Path::join`, from the app data
//!    dir and the installed version (architecture.md section 7):
//!
//!    ```text
//!    <app data dir>/
//!    ├── php/8.4/               php-fpm.conf, php.ini
//!    ├── runtimes/php/8.4.3/    ext/ (xdebug.so, or the Windows DLLs), php-cgi.exe
//!    ├── shims/
//!    ├── logs/                  php-8.4-fpm.log, php-8.4-errors.log
//!    └── run/                   php-8.4.sock
//!    ```
//!
//! 2. [`fpm_conf`], [`php_ini`], and [`cgi_pool`] print the paths they get.
//!    They never join paths, so the snapshot tests pass fixed paths and match
//!    on macOS, Linux, and Windows.
//!
//! Every path is written between double quotes: `Application Support` or
//! `Program Files (x86)` breaks an unquoted INI value. A path that the quotes
//! cannot carry is refused with `php_path_not_allowed` ([`PhpError`]).
//!
//! This module is not behind the `full` feature: it reads and writes nothing.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};

use semver::Version;

use crate::recipes::Platform;
use crate::runtimes::{Runtime, RuntimesDir};

/// The first line of every file this module renders.
const HEADER: &str = "; Managed by Marsa. Changes to this file are overwritten.";

/// The longest socket path [`fpm_listen`] accepts, in bytes. The limit for a
/// Unix socket path is 104 bytes on macOS and 108 on Linux, with the final NUL
/// byte; 100 leaves a margin.
pub const MAX_SOCKET_PATH: usize = 100;

/// How many requests PHP serves at the same moment on Windows: the worker
/// processes one php-cgi keeps (ADR 0016). One worker serves one request at a
/// time, so a page that calls its own site needs a second one. With 4, up to
/// 3 such pages can run at once; other requests wait in line.
pub const CGI_WORKERS: u16 = 4;

/// A php-cgi worker exits after this many requests, and php-cgi starts a new
/// one. The same number as `pm.max_requests` of the php-fpm pool.
pub const CGI_MAX_REQUESTS: u32 = 500;

/// The folders after the shims in the `PATH` of php-fpm workers.
const SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// Extensions that the official Windows build ships but does not load.
/// Laravel needs them.
const WINDOWS_EXTENSIONS: [&str; 8] = [
    "curl",
    "fileinfo",
    "mbstring",
    "openssl",
    "pdo_mysql",
    "pdo_sqlite",
    "sqlite3",
    "zip",
];

/// From PHP 8.5, OPcache is always built in. Before it, the official Windows
/// build ships it as a DLL that `php.ini` must load, or `opcache.enable` does
/// nothing.
const OPCACHE_BUILT_IN: PhpMinor = PhpMinor { major: 8, minor: 5 };

/// A PHP `major.minor` line, like `8.4`. Each line has its own pool and its
/// own config files, so a patch update (8.4.3 to 8.4.4) keeps them.
///
/// `Ord` compares numbers, so `8.10` sorts after `8.9`. The field order is
/// the sort order: `major` first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhpMinor {
    pub major: u64,
    pub minor: u64,
}

impl From<&Version> for PhpMinor {
    fn from(version: &Version) -> Self {
        Self {
            major: version.major,
            minor: version.minor,
        }
    }
}

impl fmt::Display for PhpMinor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Where php-fpm takes requests from Caddy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listen {
    /// A Unix socket with mode `0600`: no other user on the machine can
    /// connect to it.
    Unix(PathBuf),
    /// `127.0.0.1:<port>`. Every local user can connect and run PHP as you,
    /// so this is only the fallback for a socket path that is too long.
    Tcp(u16),
}

/// Every path that the config of one PHP version uses. The paths are final:
/// the render functions print them as they are.
///
/// [`PhpFiles::new`] builds them from the disk layout. Tests build the struct
/// by hand with fixed paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhpFiles {
    /// The version line these files belong to.
    pub minor: PhpMinor,
    /// `php/<minor>/php-fpm.conf`, where the daemon writes [`fpm_conf`]
    /// (macOS, Linux).
    pub fpm_conf_file: PathBuf,
    /// `php/<minor>/php.ini`, where the daemon writes [`php_ini`].
    pub ini_file: PathBuf,
    /// `run/php-<minor>.sock`, the php-fpm socket (macOS, Linux).
    pub socket: PathBuf,
    /// `logs/php-<minor>-fpm.log`, php-fpm's own log (macOS, Linux).
    pub fpm_log: PathBuf,
    /// `logs/php-<minor>-errors.log`, PHP's `error_log`.
    pub error_log: PathBuf,
    /// `shims/`, first in the `PATH` of php-fpm workers.
    pub shims_dir: PathBuf,
    /// A CA bundle that includes Marsa's CA, for `curl.cainfo` and
    /// `openssl.cafile` (architecture.md section 10). [`PhpFiles::new`]
    /// leaves it `None`: the daemon sets it once the bundle exists.
    pub ca_bundle: Option<PathBuf>,
    /// `runtimes/php/<version>/ext`: the extension DLLs (Windows), or
    /// `xdebug.so` (macOS, Linux).
    pub ext_dir: PathBuf,
    /// The Xdebug that `php.ini` loads, or `None` for a PHP without Xdebug
    /// (ADR 0010). [`PhpFiles::new`] leaves it `None`: the daemon sets it to
    /// [`PhpFiles::xdebug_so`] when `[php] xdebug` is on in `global.toml` and
    /// the file is there.
    pub xdebug: Option<PathBuf>,
    /// `runtimes/php/<version>/php-cgi.exe` (Windows).
    pub php_cgi: PathBuf,
}

impl PhpFiles {
    /// The paths for the installed PHP `version`, under `app_data_dir`.
    ///
    /// Pass an absolute `app_data_dir` and a version from
    /// [`RuntimesDir::installed`]: then every path is absolute and stays
    /// inside the app data dir.
    pub fn new(app_data_dir: &Path, version: &Version) -> Self {
        let minor = PhpMinor::from(version);
        let config_dir = app_data_dir.join("php").join(minor.to_string());
        let log_dir = app_data_dir.join("logs");
        let php_dir = RuntimesDir::in_dir(app_data_dir).version_dir(Runtime::Php, version);
        Self {
            minor,
            fpm_conf_file: config_dir.join("php-fpm.conf"),
            ini_file: config_dir.join("php.ini"),
            socket: app_data_dir.join("run").join(format!("php-{minor}.sock")),
            fpm_log: log_dir.join(format!("php-{minor}-fpm.log")),
            error_log: log_dir.join(format!("php-{minor}-errors.log")),
            shims_dir: app_data_dir.join("shims"),
            ca_bundle: None,
            ext_dir: php_dir.join("ext"),
            xdebug: None,
            php_cgi: php_dir.join("php-cgi.exe"),
        }
    }

    /// `runtimes/php/<version>/ext/xdebug.so`, where Marsa's PHP builds for
    /// macOS and Linux ship Xdebug. The official Windows build has none.
    ///
    /// Joined, never canonicalized: `php.ini` gets the path as it is.
    pub fn xdebug_so(&self) -> PathBuf {
        self.ext_dir.join("xdebug.so")
    }
}

/// How php-fpm listens: on `socket`, or on `127.0.0.1:<fallback_port>` when
/// the socket path is longer than [`MAX_SOCKET_PATH`] bytes.
///
/// A long user name with `Library/Application Support/dev.marsa.Marsa/run/`
/// comes close to the macOS limit, and php-fpm cannot bind a longer path.
pub fn fpm_listen(socket: &Path, fallback_port: u16) -> Listen {
    if socket.as_os_str().as_encoded_bytes().len() <= MAX_SOCKET_PATH {
        Listen::Unix(socket.to_path_buf())
    } else {
        Listen::Tcp(fallback_port)
    }
}

/// The text of `php-fpm.conf` for one version: one pool named `marsa`.
///
/// - `daemonize = no`: the daemon owns the process and waits for it.
/// - No `user` or `group`: php-fpm runs as the current user, and ignores them
///   when it does not run as root.
/// - `pm = ondemand`: no idle PHP workers hold memory.
/// - `clear_env = yes` with an explicit `PATH`: the daemon's environment does
///   not leak into PHP, and `exec('php ...')` in Laravel finds PHP and Node
///   through the shims.
///
/// php-fpm runs on macOS and Linux only, so the `PATH` uses `:`.
///
/// # Errors
///
/// `php_path_not_allowed` if `files.fpm_log`, the socket in `listen`, or
/// `files.shims_dir` cannot be written in the file (see
/// [`PhpError::PathNotAllowed`]), or if `files.shims_dir` holds `:`, the
/// `PATH` separator.
pub fn fpm_conf(listen: &Listen, files: &PhpFiles) -> Result<String, PhpError> {
    let fpm_log = ini_path(&files.fpm_log)?;
    let shims_dir = path_entry(&files.shims_dir)?;
    let (listen, is_socket) = match listen {
        Listen::Unix(socket) => (format!("\"{}\"", ini_path(socket)?), true),
        Listen::Tcp(port) => (format!("127.0.0.1:{port}"), false),
    };

    Ok(render(|out| {
        writeln!(out, "{HEADER}")?;
        writeln!(out, "[global]")?;
        writeln!(out, "error_log = \"{fpm_log}\"")?;
        writeln!(out, "daemonize = no")?;
        writeln!(out)?;
        writeln!(out, "[marsa]")?;
        writeln!(out, "listen = {listen}")?;
        if is_socket {
            writeln!(out, "listen.mode = 0600")?;
        }
        writeln!(out, "pm = ondemand")?;
        writeln!(out, "pm.max_children = 10")?;
        writeln!(out, "pm.process_idle_timeout = 30s")?;
        writeln!(out, "pm.max_requests = 500")?;
        writeln!(out, "catch_workers_output = yes")?;
        writeln!(out, "decorate_workers_output = no")?;
        writeln!(out, "clear_env = yes")?;
        writeln!(out, "env[PATH] = \"{shims_dir}:{SYSTEM_PATH}\"")
    }))
}

/// The text of `php.ini` for one version, with fixed values for local
/// development: large limits, every error shown and logged, UTC, and an
/// OPcache that checks for changed files on every request.
///
/// - With `files.ca_bundle`: `curl.cainfo` and `openssl.cafile`, so an HTTP
///   call from PHP to another `.test` site trusts Marsa's CA.
/// - On [`Platform::WindowsX64`]: `extension_dir`, `zend_extension = opcache`
///   before PHP 8.5, and one `extension` line for each extension Laravel
///   needs. Every other platform gets the same text,
///   because static-php-cli builds have their extensions built in.
/// - With `files.xdebug`: `zend_extension` with that path, and Xdebug's debug
///   and develop modes. A debug session starts only when the request asks
///   for one (`XDEBUG_TRIGGER`, or the browser extension), so other requests
///   do not wait for an IDE. PHP still runs slower while Xdebug is loaded
///   (ADR 0010), so it is off unless the user turns it on.
///
/// The Xdebug lines are in this file, not in a file of their own: php-fpm
/// (`-c`) and the shim (`PHPRC`) both read this one file.
///
/// # Errors
///
/// `php_path_not_allowed` if `files.error_log`, `files.ca_bundle`,
/// `files.xdebug`, or (on Windows) `files.ext_dir` cannot be written in the
/// file (see [`PhpError::PathNotAllowed`]).
pub fn php_ini(platform: Platform, files: &PhpFiles) -> Result<String, PhpError> {
    let error_log = ini_path(&files.error_log)?;
    let ca_bundle = files.ca_bundle.as_deref().map(ini_path).transpose()?;
    let ext_dir = match platform {
        Platform::WindowsX64 => Some(ini_path(&files.ext_dir)?),
        _ => None,
    };
    let xdebug = files.xdebug.as_deref().map(ini_path).transpose()?;

    Ok(render(|out| {
        writeln!(out, "{HEADER}")?;
        writeln!(out, "memory_limit = 512M")?;
        writeln!(out, "upload_max_filesize = 128M")?;
        writeln!(out, "post_max_size = 128M")?;
        writeln!(out, "max_execution_time = 60")?;
        writeln!(out)?;
        writeln!(out, "display_errors = On")?;
        writeln!(out, "display_startup_errors = On")?;
        writeln!(out, "error_reporting = E_ALL")?;
        writeln!(out, "log_errors = On")?;
        writeln!(out, "error_log = \"{error_log}\"")?;
        writeln!(out)?;
        writeln!(out, "date.timezone = UTC")?;
        writeln!(out, "realpath_cache_size = 4096K")?;
        writeln!(out)?;
        writeln!(out, "opcache.enable = 1")?;
        writeln!(out, "opcache.validate_timestamps = 1")?;
        writeln!(out, "opcache.revalidate_freq = 0")?;
        if let Some(ca_bundle) = ca_bundle {
            writeln!(out)?;
            writeln!(out, "curl.cainfo = \"{ca_bundle}\"")?;
            writeln!(out, "openssl.cafile = \"{ca_bundle}\"")?;
        }
        if let Some(ext_dir) = ext_dir {
            writeln!(out)?;
            writeln!(out, "extension_dir = \"{ext_dir}\"")?;
            if files.minor < OPCACHE_BUILT_IN {
                writeln!(out, "zend_extension = opcache")?;
            }
            for extension in WINDOWS_EXTENSIONS {
                writeln!(out, "extension = {extension}")?;
            }
        }
        if let Some(xdebug) = xdebug {
            writeln!(out)?;
            writeln!(out, "zend_extension = \"{xdebug}\"")?;
            writeln!(out, "xdebug.mode = debug,develop")?;
            writeln!(out, "xdebug.start_with_request = trigger")?;
        }
        Ok(())
    }))
}

/// How php-cgi runs for one PHP version on Windows: one process on one
/// loopback port. It keeps [`CGI_WORKERS`] worker processes itself and
/// replaces a worker that exits, so Caddy has one address per PHP version,
/// as with php-fpm (ADR 0016).
///
/// This is Marsa's own type. The daemon turns it into a service spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgiPool {
    /// `php-cgi.exe` of the installed version.
    pub program: PathBuf,
    /// `-b 127.0.0.1:<port> -c <php.ini>`, one entry per argument.
    pub args: Vec<String>,
    /// `PHP_FCGI_CHILDREN` and `PHP_FCGI_MAX_REQUESTS`.
    pub env: BTreeMap<String, String>,
}

/// The php-cgi of one PHP version on Windows, listening on
/// `127.0.0.1:<port>`.
///
/// `PHP_FCGI_CHILDREN` makes php-cgi start that many workers and restart
/// each one after `PHP_FCGI_MAX_REQUESTS` requests. No request fails at that
/// moment, unlike with separate php-cgi processes behind Caddy (spike S-11).
///
/// # Errors
///
/// `php_path_not_allowed` if `files.ini_file` is not UTF-8.
pub fn cgi_pool(files: &PhpFiles, port: u16) -> Result<CgiPool, PhpError> {
    let ini = files
        .ini_file
        .to_str()
        .ok_or_else(|| PhpError::PathNotAllowed {
            path: files.ini_file.clone(),
        })?;
    Ok(CgiPool {
        program: files.php_cgi.clone(),
        args: vec![
            "-b".to_string(),
            format!("127.0.0.1:{port}"),
            "-c".to_string(),
            ini.to_string(),
        ],
        env: BTreeMap::from([
            ("PHP_FCGI_CHILDREN".to_string(), CGI_WORKERS.to_string()),
            (
                "PHP_FCGI_MAX_REQUESTS".to_string(),
                CGI_MAX_REQUESTS.to_string(),
            ),
        ]),
    })
}

/// Why a PHP config could not be built. [`PhpError::code`] gives a stable
/// name that the UI maps to a message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PhpError {
    /// The path is not UTF-8, or it holds text that a double-quoted INI value
    /// cannot carry: `"`, `$`, `\\`, a line break, or a NUL byte.
    #[error("{path:?} cannot be written in a PHP config file")]
    PathNotAllowed { path: PathBuf },
}

impl PhpError {
    /// Stable error name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            PhpError::PathNotAllowed { .. } => "php_path_not_allowed",
        }
    }
}

/// `path` as text that is safe between double quotes in php.ini and
/// php-fpm.conf.
///
/// Inside double quotes PHP's INI parser keeps spaces, `;`, brackets, and a
/// single `\` (so `C:\php\ext` is fine). It ends the value at `"` or a line
/// break, reads `${NAME}` as an environment variable, and turns `\\` into `\`,
/// which breaks a UNC path. php-fpm also replaces `$pool` in values, and reads
/// an `env` value that starts with `$` as a variable. All of these are refused.
fn ini_path(path: &Path) -> Result<&str, PhpError> {
    let not_allowed = || PhpError::PathNotAllowed {
        path: path.to_path_buf(),
    };
    let text = path.to_str().ok_or_else(not_allowed)?;
    if text.contains(['"', '$', '\r', '\n', '\0']) || text.contains(r"\\") {
        return Err(not_allowed());
    }
    Ok(text)
}

/// Like [`ini_path`], for a folder that goes into a `PATH` value: a `:` in it
/// would split it into two entries.
fn path_entry(path: &Path) -> Result<&str, PhpError> {
    let text = ini_path(path)?;
    if text.contains(':') {
        return Err(PhpError::PathNotAllowed {
            path: path.to_path_buf(),
        });
    }
    Ok(text)
}

/// Runs `write` on a new `String` and returns the text. Writing to a `String`
/// cannot fail, so this is the only place that has to say so.
fn render(write: impl FnOnce(&mut String) -> fmt::Result) -> String {
    let mut out = String::new();
    write(&mut out).expect("writing to a String never fails");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINOR: PhpMinor = PhpMinor { major: 8, minor: 4 };

    fn v(text: &str) -> Version {
        Version::parse(text).expect("test versions are valid")
    }

    fn byte_len(path: &Path) -> usize {
        path.as_os_str().as_encoded_bytes().len()
    }

    /// Fixed macOS-style paths, the same on every OS that runs the tests.
    fn unix_files() -> PhpFiles {
        PhpFiles {
            minor: MINOR,
            fpm_conf_file: "/marsa/php/8.4/php-fpm.conf".into(),
            ini_file: "/marsa/php/8.4/php.ini".into(),
            socket: "/marsa/run/php-8.4.sock".into(),
            fpm_log: "/marsa/logs/php-8.4-fpm.log".into(),
            error_log: "/marsa/logs/php-8.4-errors.log".into(),
            shims_dir: "/marsa/shims".into(),
            ca_bundle: None,
            ext_dir: "/marsa/runtimes/php/8.4.3/ext".into(),
            xdebug: None,
            php_cgi: "/marsa/runtimes/php/8.4.3/php-cgi.exe".into(),
        }
    }

    /// Fixed Windows paths, with a CA bundle.
    fn windows_files() -> PhpFiles {
        PhpFiles {
            minor: MINOR,
            fpm_conf_file: r"C:\marsa\php\8.4\php-fpm.conf".into(),
            ini_file: r"C:\marsa\php\8.4\php.ini".into(),
            socket: r"C:\marsa\run\php-8.4.sock".into(),
            fpm_log: r"C:\marsa\logs\php-8.4-fpm.log".into(),
            error_log: r"C:\marsa\logs\php-8.4-errors.log".into(),
            shims_dir: r"C:\marsa\shims".into(),
            ca_bundle: Some(r"C:\marsa\caddy\ca-bundle.pem".into()),
            ext_dir: r"C:\marsa\runtimes\php\8.4.3\ext".into(),
            xdebug: None,
            php_cgi: r"C:\marsa\runtimes\php\8.4.3\php-cgi.exe".into(),
        }
    }

    fn assert_not_allowed<T: fmt::Debug>(result: Result<T, PhpError>, path: &str) {
        let err = result.expect_err("the path must be refused");
        assert_eq!(err.code(), "php_path_not_allowed");
        assert_eq!(err, PhpError::PathNotAllowed { path: path.into() });
    }

    #[test]
    fn minor_from_version_drops_the_patch() {
        assert_eq!(PhpMinor::from(&v("8.4.3")), MINOR);
        assert_eq!(
            PhpMinor::from(&v("8.5.0-rc1")),
            PhpMinor { major: 8, minor: 5 }
        );
        assert_eq!(PhpMinor::from(&v("8.4.3")).to_string(), "8.4");
        assert_eq!(
            PhpMinor {
                major: 8,
                minor: 10
            }
            .to_string(),
            "8.10"
        );
    }

    #[test]
    fn minor_sorts_by_number() {
        let m = |major, minor| PhpMinor { major, minor };
        let mut list = vec![m(8, 10), m(9, 0), m(7, 4), m(8, 9)];
        list.sort();
        assert_eq!(list, [m(7, 4), m(8, 9), m(8, 10), m(9, 0)]);
    }

    #[test]
    fn files_follow_the_disk_layout() {
        let data = Path::new("data");
        let files = PhpFiles::new(data, &v("8.4.3"));
        let config = data.join("php").join("8.4");
        let php_dir = data.join("runtimes").join("php").join("8.4.3");
        let expected = PhpFiles {
            minor: MINOR,
            fpm_conf_file: config.join("php-fpm.conf"),
            ini_file: config.join("php.ini"),
            socket: data.join("run").join("php-8.4.sock"),
            fpm_log: data.join("logs").join("php-8.4-fpm.log"),
            error_log: data.join("logs").join("php-8.4-errors.log"),
            shims_dir: data.join("shims"),
            ca_bundle: None,
            ext_dir: php_dir.join("ext"),
            xdebug: None,
            php_cgi: php_dir.join("php-cgi.exe"),
        };
        assert_eq!(files, expected);
        assert_eq!(files.xdebug_so(), php_dir.join("ext").join("xdebug.so"));
    }

    #[test]
    fn two_patches_share_the_config_but_not_the_binaries() {
        let data = Path::new("data");
        let old = PhpFiles::new(data, &v("8.4.3"));
        let new = PhpFiles::new(data, &v("8.4.4"));
        assert_eq!(old.ini_file, new.ini_file);
        assert_eq!(old.socket, new.socket);
        assert_ne!(old.php_cgi, new.php_cgi);
    }

    #[test]
    fn listen_on_a_short_socket() {
        let socket = Path::new("/marsa/run/php-8.4.sock");
        assert_eq!(fpm_listen(socket, 9084), Listen::Unix(socket.into()));
    }

    #[test]
    fn listen_on_a_socket_of_exactly_100_bytes() {
        let socket = PathBuf::from(format!("/{}", "s".repeat(99)));
        assert_eq!(byte_len(&socket), MAX_SOCKET_PATH);
        assert_eq!(fpm_listen(&socket, 9084), Listen::Unix(socket.clone()));
    }

    #[test]
    fn listen_on_tcp_when_the_socket_has_101_bytes() {
        let socket = PathBuf::from(format!("/{}", "s".repeat(100)));
        assert_eq!(byte_len(&socket), MAX_SOCKET_PATH + 1);
        assert_eq!(fpm_listen(&socket, 9084), Listen::Tcp(9084));
    }

    #[test]
    fn listen_counts_bytes_not_characters() {
        // 51 characters but 101 bytes: an Arabic letter is two bytes in UTF-8.
        let socket = PathBuf::from(format!("/{}", "ع".repeat(50)));
        assert_eq!(byte_len(&socket), MAX_SOCKET_PATH + 1);
        assert_eq!(fpm_listen(&socket, 9084), Listen::Tcp(9084));
    }

    #[test]
    fn fpm_conf_with_a_socket() {
        let files = unix_files();
        let text = fpm_conf(&Listen::Unix(files.socket.clone()), &files).expect("render");
        insta::assert_snapshot!(text);
    }

    #[test]
    fn fpm_conf_with_tcp() {
        let text = fpm_conf(&Listen::Tcp(9084), &unix_files()).expect("render");
        assert!(!text.contains("listen.mode"));
        insta::assert_snapshot!(text);
    }

    #[test]
    fn php_ini_on_macos_without_a_ca_bundle() {
        let text = php_ini(Platform::DarwinArm64, &unix_files()).expect("render");
        insta::assert_snapshot!(text);
    }

    #[test]
    fn php_ini_on_windows_with_a_ca_bundle() {
        let text = php_ini(Platform::WindowsX64, &windows_files()).expect("render");
        insta::assert_snapshot!(text);
    }

    /// Both states of `php.ini`: off is the snapshot of
    /// `php_ini_on_macos_without_a_ca_bundle` (the same files), and on is
    /// that text plus the Xdebug block.
    #[test]
    fn php_ini_on_macos_with_xdebug() {
        let mut files = unix_files();
        let off = php_ini(Platform::DarwinArm64, &files).expect("render off");
        assert!(!off.contains("xdebug"));

        // A space in the path, as in `Application Support`.
        files.xdebug = Some("/Users/a b/marsa/runtimes/php/8.4.3/ext/xdebug.so".into());
        let on = php_ini(Platform::DarwinArm64, &files).expect("render on");
        // Off is the same file without the last block.
        assert_eq!(
            on.split_once("\n\nzend_extension").map(|(a, _)| a),
            off.strip_suffix('\n')
        );
        insta::assert_snapshot!(on);
    }

    #[test]
    fn opcache_is_loaded_on_windows_before_php_8_5() {
        let mut files = windows_files();
        let text = php_ini(Platform::WindowsX64, &files).expect("render 8.4");
        assert!(text.contains("zend_extension = opcache\n"));

        files.minor = PhpMinor { major: 8, minor: 5 };
        let text = php_ini(Platform::WindowsX64, &files).expect("render 8.5");
        assert!(!text.contains("zend_extension"));
    }

    #[test]
    fn php_ini_is_the_same_on_every_platform_but_windows() {
        let files = unix_files();
        let macos = php_ini(Platform::DarwinArm64, &files).expect("render");
        for platform in Platform::ALL {
            if platform != Platform::WindowsX64 {
                assert_eq!(php_ini(platform, &files).expect("render"), macos);
            }
        }
        assert!(!macos.contains("extension"));
    }

    #[test]
    fn output_has_lf_only_and_one_final_newline() {
        let (unix, windows) = (unix_files(), windows_files());
        let outputs = [
            fpm_conf(&Listen::Unix(unix.socket.clone()), &unix),
            fpm_conf(&Listen::Tcp(9084), &unix),
            php_ini(Platform::DarwinArm64, &unix),
            php_ini(Platform::WindowsX64, &windows),
        ];
        for text in outputs {
            let text = text.expect("render");
            assert!(text.starts_with(&format!("{HEADER}\n")), "{text}");
            assert!(text.ends_with('\n') && !text.ends_with("\n\n"), "{text}");
            assert!(!text.contains('\r'), "{text}");
        }
    }

    #[test]
    fn paths_with_spaces_and_semicolons_are_quoted() {
        let mut files = unix_files();
        files.error_log = "/Users/a b/Application Support (x86);1/errors.log".into();
        files.ca_bundle = Some("/Users/a b/ca.pem".into());
        let text = php_ini(Platform::DarwinArm64, &files).expect("render");
        assert!(
            text.contains("\nerror_log = \"/Users/a b/Application Support (x86);1/errors.log\"\n")
        );
        assert!(text.contains("\ncurl.cainfo = \"/Users/a b/ca.pem\"\n"));
        assert!(text.contains("\nopenssl.cafile = \"/Users/a b/ca.pem\"\n"));

        files.ext_dir = r"C:\Program Files (x86)\Marsa\ext".into();
        let text = php_ini(Platform::WindowsX64, &files).expect("render");
        assert!(text.contains("\nextension_dir = \"C:\\Program Files (x86)\\Marsa\\ext\"\n"));
    }

    #[test]
    fn path_with_a_newline_is_refused() {
        let mut files = unix_files();
        files.fpm_log = "/marsa/logs\n[evil]".into();
        assert_not_allowed(fpm_conf(&Listen::Tcp(9084), &files), "/marsa/logs\n[evil]");
    }

    #[test]
    fn paths_that_break_quoted_ini_values_are_refused() {
        let bad = [
            "/a\"b",
            "/a\rb",
            "/a\nb",
            "/a\0b",
            "/a/${HOME}",
            "/a/$pool",
            r"\\server\share\marsa",
        ];
        for path in bad {
            let mut files = unix_files();
            files.fpm_log = path.into();
            assert_not_allowed(fpm_conf(&Listen::Tcp(9084), &files), path);

            let socket = Listen::Unix(path.into());
            assert_not_allowed(fpm_conf(&socket, &unix_files()), path);

            let mut files = unix_files();
            files.shims_dir = path.into();
            assert_not_allowed(fpm_conf(&Listen::Tcp(9084), &files), path);

            let mut files = windows_files();
            files.error_log = path.into();
            assert_not_allowed(php_ini(Platform::WindowsX64, &files), path);

            let mut files = windows_files();
            files.ca_bundle = Some(path.into());
            assert_not_allowed(php_ini(Platform::WindowsX64, &files), path);

            let mut files = windows_files();
            files.ext_dir = path.into();
            assert_not_allowed(php_ini(Platform::WindowsX64, &files), path);

            let mut files = unix_files();
            files.xdebug = Some(path.into());
            assert_not_allowed(php_ini(Platform::DarwinArm64, &files), path);
        }
    }

    #[test]
    fn ext_dir_is_only_checked_on_windows() {
        let mut files = unix_files();
        files.ext_dir = "/a\nb".into();
        assert!(php_ini(Platform::LinuxX64, &files).is_ok());
    }

    #[test]
    fn shims_dir_cannot_hold_the_path_separator() {
        let mut files = unix_files();
        files.shims_dir = "/Users/a:b/shims".into();
        assert_not_allowed(fpm_conf(&Listen::Tcp(9084), &files), "/Users/a:b/shims");
    }

    // Only Unix lets a path hold bytes that are not UTF-8.
    #[cfg(unix)]
    #[test]
    fn paths_that_are_not_utf8_are_refused() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let path = PathBuf::from(OsString::from_vec(b"/marsa/\xff.log".to_vec()));
        let not_allowed = PhpError::PathNotAllowed { path: path.clone() };

        let mut files = unix_files();
        files.error_log = path.clone();
        assert_eq!(
            php_ini(Platform::LinuxX64, &files),
            Err(not_allowed.clone())
        );

        let mut files = unix_files();
        files.fpm_log = path.clone();
        assert_eq!(
            fpm_conf(&Listen::Tcp(9084), &files),
            Err(not_allowed.clone())
        );

        let mut files = windows_files();
        files.ini_file = path;
        assert_eq!(cgi_pool(&files, 19084), Err(not_allowed));
    }

    #[test]
    fn cgi_pool_is_one_php_cgi_that_keeps_its_own_workers() {
        let files = windows_files();
        let pool = cgi_pool(&files, 19084).expect("a UTF-8 path");
        let expected = CgiPool {
            program: r"C:\marsa\runtimes\php\8.4.3\php-cgi.exe".into(),
            args: Vec::from(
                ["-b", "127.0.0.1:19084", "-c", r"C:\marsa\php\8.4\php.ini"].map(String::from),
            ),
            env: BTreeMap::from([
                ("PHP_FCGI_CHILDREN".to_string(), "4".to_string()),
                ("PHP_FCGI_MAX_REQUESTS".to_string(), "500".to_string()),
            ]),
        };
        assert_eq!(pool, expected);
        assert_eq!(pool.program, files.php_cgi);
    }

    /// php-cgi listens on loopback only (AGENTS.md section 6), whatever the
    /// port.
    #[test]
    fn cgi_pool_listens_on_loopback_only() {
        for port in [1, 19084, u16::MAX] {
            let pool = cgi_pool(&windows_files(), port).expect("a UTF-8 path");
            assert_eq!(
                pool.args[..2],
                ["-b".to_string(), format!("127.0.0.1:{port}")]
            );
        }
    }

    #[test]
    fn error_messages_name_the_problem() {
        let err = PhpError::PathNotAllowed {
            path: "/a\nb".into(),
        };
        assert_eq!(
            err.to_string(),
            r#""/a\nb" cannot be written in a PHP config file"#
        );
    }
}
