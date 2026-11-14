//! Opening a URL in the user's browser, for `marsa open` (task M5-09).
//!
//! The system's own opener does it, found by its absolute path and run with
//! the URL as its one argument, never through a shell:
//!
//! - macOS: `/usr/bin/open`.
//! - Windows: `explorer.exe` in the Windows folder (`SystemRoot`).
//! - Linux and the other Unix systems: `xdg-open`, looked for in the standard
//!   folders first (`XDG_OPEN_DIRS`), then in the absolute folders of
//!   `PATH`. A relative `PATH` entry (`.`) is never used. This runs in the
//!   `marsa` command, as the user, never in `marsad`.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// How long [`open_url`] waits for the opener to report. `open` and
/// `explorer.exe` hand the URL over and exit at once; an `xdg-open` that
/// runs the browser itself may never exit, and is left to run.
const WAIT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);

/// Where `xdg-open` is looked for first, in this order. The last one is
/// NixOS's.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
pub const XDG_OPEN_DIRS: [&str; 4] = [
    "/usr/bin",
    "/bin",
    "/usr/local/bin",
    "/run/current-system/sw/bin",
];

/// Why a URL could not be opened. [`OpenError::code`] gives a stable name.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("no program to open a URL with: {tried}")]
    NoOpener { tried: String },

    #[error("cannot run {}", .program.display())]
    Spawn {
        program: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("{} failed: {status}", .program.display())]
    Failed {
        program: PathBuf,
        status: ExitStatus,
    },
}

impl OpenError {
    /// Stable error name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            OpenError::NoOpener { .. } => "open_no_program",
            OpenError::Spawn { .. } | OpenError::Failed { .. } => "open_failed",
        }
    }
}

/// The program that opens a URL, and how to read its exit status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opener {
    /// Always absolute.
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Whether a failed exit status means the URL was not opened.
    /// `explorer.exe` exits with 1 even when it opened it.
    pub checks_status: bool,
}

/// Opens `url` in the default browser. Waits up to 5 seconds for the opener
/// to report, then leaves it running.
///
/// # Errors
///
/// `open_no_program` if the system has no opener, and `open_failed` if it
/// cannot be started or reports a failure.
pub fn open_url(url: &str) -> Result<(), OpenError> {
    run(&opener(url)?, WAIT)
}

/// `/usr/bin/open <url>`.
///
/// # Errors
///
/// Never; the `Result` matches the other systems.
#[cfg(target_os = "macos")]
pub fn opener(url: &str) -> Result<Opener, OpenError> {
    Ok(Opener {
        program: PathBuf::from("/usr/bin/open"),
        args: vec![url.into()],
        checks_status: true,
    })
}

/// `<SystemRoot>\explorer.exe <url>`: it hands the URL to the default
/// browser.
///
/// # Errors
///
/// `open_no_program` if `explorer.exe` is not there.
#[cfg(windows)]
pub fn opener(url: &str) -> Result<Opener, OpenError> {
    let program = windows_dir(std::env::var_os("SystemRoot")).join("explorer.exe");
    if !program.is_file() {
        return Err(OpenError::NoOpener {
            tried: program.display().to_string(),
        });
    }
    Ok(Opener {
        program,
        args: vec![url.into()],
        checks_status: false,
    })
}

/// `xdg-open <url>`, from [`search_dirs`].
///
/// # Errors
///
/// `open_no_program` if no folder has `xdg-open`.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn opener(url: &str) -> Result<Opener, OpenError> {
    let dirs = search_dirs(std::env::var_os("PATH").as_deref());
    let program = find_program("xdg-open", &dirs, std::path::Path::is_file).ok_or_else(|| {
        let tried: Vec<String> = dirs.iter().map(|dir| dir.display().to_string()).collect();
        OpenError::NoOpener {
            tried: format!("xdg-open in {}", tried.join(", ")),
        }
    })?;
    Ok(Opener {
        program,
        args: vec![url.into()],
        checks_status: true,
    })
}

/// The Windows folder from `SystemRoot`, if that is an absolute path, else
/// `C:\Windows`.
#[cfg(windows)]
fn windows_dir(system_root: Option<OsString>) -> PathBuf {
    system_root
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
}

/// [`XDG_OPEN_DIRS`], then each absolute folder of `path` (the `PATH`
/// value), each folder once.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
pub fn search_dirs(path: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = XDG_OPEN_DIRS.iter().map(PathBuf::from).collect();
    for dir in path.map(std::env::split_paths).into_iter().flatten() {
        if dir.is_absolute() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// The first `<dir>/<name>` in `dirs` that `exists` accepts.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
pub fn find_program(
    name: &str,
    dirs: &[PathBuf],
    exists: impl Fn(&std::path::Path) -> bool,
) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(name))
        .find(|program| exists(program))
}

/// Starts `opener` with no input or output, then waits up to `wait` for it
/// to exit. A program still running then is left to run.
fn run(opener: &Opener, wait: Duration) -> Result<(), OpenError> {
    let mut child = Command::new(&opener.program)
        .args(&opener.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| OpenError::Spawn {
            program: opener.program.clone(),
            source,
        })?;
    let deadline = Instant::now() + wait;
    loop {
        let status = child.try_wait().map_err(|source| OpenError::Spawn {
            program: opener.program.clone(),
            source,
        })?;
        match status {
            Some(status) if status.success() || !opener.checks_status => return Ok(()),
            Some(status) => {
                return Err(OpenError::Failed {
                    program: opener.program.clone(),
                    status,
                });
            }
            None if Instant::now() >= deadline => {
                tracing::debug!(program = %opener.program.display(), "the opener still runs");
                return Ok(());
            }
            None => std::thread::sleep(POLL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const URL: &str = "https://shop.test";

    #[test]
    fn the_opener_is_absolute_and_gets_the_url_alone() {
        // On Linux without `xdg-open` (a bare CI image), there is nothing to
        // check but the error code.
        match opener(URL) {
            Ok(opener) => {
                assert!(opener.program.is_absolute(), "{opener:?}");
                assert_eq!(opener.args, [OsString::from(URL)]);
                if cfg!(target_os = "macos") {
                    assert_eq!(opener.program, Path::new("/usr/bin/open"));
                }
                if cfg!(windows) {
                    assert!(opener.program.ends_with("explorer.exe"), "{opener:?}");
                    assert!(!opener.checks_status);
                }
            }
            Err(err) => assert_eq!(err.code(), "open_no_program", "{err}"),
        }
    }

    // Unix paths: on Windows `/home/me/bin` has no drive letter, so it is
    // not absolute and is rightly left out. `xdg-open` is a Linux program.
    #[cfg(unix)]
    #[test]
    fn xdg_open_is_looked_for_in_the_standard_folders_first() {
        let path = std::env::join_paths(["/home/me/bin", "relative/bin", "/usr/bin"]).unwrap();
        let dirs = search_dirs(Some(&path));
        let mut want: Vec<PathBuf> = XDG_OPEN_DIRS.iter().map(PathBuf::from).collect();
        want.push(PathBuf::from("/home/me/bin"));
        assert_eq!(dirs, want, "`relative/bin` is never used, `/usr/bin` once");
        assert_eq!(search_dirs(None).len(), XDG_OPEN_DIRS.len());

        let only = |wanted: &'static str| move |path: &Path| path == Path::new(wanted);
        assert_eq!(
            find_program("xdg-open", &dirs, only("/home/me/bin/xdg-open")),
            Some(PathBuf::from("/home/me/bin/xdg-open"))
        );
        let both = |path: &Path| path.starts_with("/bin") || path.starts_with("/home");
        assert_eq!(
            find_program("xdg-open", &dirs, both),
            Some(PathBuf::from("/bin/xdg-open"))
        );
        assert_eq!(find_program("xdg-open", &dirs, |_| false), None);
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_folder_comes_from_system_root() {
        assert_eq!(
            windows_dir(Some(r"D:\Win".into())),
            PathBuf::from(r"D:\Win")
        );
        for bad in [None, Some(OsString::from("Windows")), Some(OsString::new())] {
            assert_eq!(windows_dir(bad), PathBuf::from(r"C:\Windows"));
        }
    }

    // `true`, `false`, and `sleep` stand in for the opener: no browser opens.
    // They are in `/usr/bin` on macOS and Linux only.
    #[cfg(unix)]
    #[test]
    fn the_exit_status_counts_only_when_the_opener_reports_it() {
        let stand_in = |program: &str, checks_status| Opener {
            program: PathBuf::from(program),
            args: vec![URL.into()],
            checks_status,
        };
        let wait = Duration::from_secs(30);
        run(&stand_in("/usr/bin/true", true), wait).unwrap();
        let err = run(&stand_in("/usr/bin/false", true), wait).unwrap_err();
        assert_eq!(err.code(), "open_failed");
        // `explorer.exe` exits with 1 when it worked.
        run(&stand_in("/usr/bin/false", false), wait).unwrap();
        let err = run(&stand_in("/usr/bin/no-such-opener", true), wait).unwrap_err();
        assert_eq!(err.code(), "open_failed");

        // Still running after the wait: left to run, and not a failure.
        let sleep = Opener {
            program: PathBuf::from("/bin/sleep"),
            args: vec!["2".into()],
            checks_status: true,
        };
        let started = Instant::now();
        run(&sleep, Duration::from_millis(100)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
