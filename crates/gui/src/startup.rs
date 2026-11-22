//! What the window and the menu-bar icon do when they start: read
//! `--data-dir`, set up logging, and (the window only) take the lock that
//! keeps a second window from opening.
//!
//! `--data-dir <PATH>` is for tests; the default is the OS app data folder.
//! No environment variable overrides it: the shim never reads one, so the
//! programs and the shim always share one folder.

use marsa_core::data_dir::DataDir;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use marsa_core::config::ConfigError;
use tracing_subscriber::EnvFilter;

use crate::model::Route;
use crate::panel::place::{Anchor, BadAnchor};

/// The lock file of the window, in the daemon's `run/` folder. The window
/// holds it while it is open, so a second `marsa-gui` (for example from the
/// tray's "Open Marsa") finds it taken and exits at once.
pub const WINDOW_LOCK: &str = "gui.lock";

/// The default log filter of both programs: `MARSA_LOG` overrides it. It
/// keeps the graphics and GUI crates quiet.
const DEFAULT_LOG: &str = "warn,marsa_gui=info,marsa_core=info";

/// Why the program cannot start.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("--data-dir needs a path")]
    MissingPath,
    #[error("{0} needs a value")]
    MissingValue(&'static str),
    /// `--open` names a screen the window has no such way to open.
    #[error("--open takes `sites`, not {0}")]
    UnknownScreen(String),
    /// `--icon`, `--monitor`, or `--scale` of the panel.
    #[error(transparent)]
    Anchor(#[from] BadAnchor),
    #[error("unknown argument {0}")]
    UnknownArgument(String),
    /// The OS gives no app data folder.
    #[error(transparent)]
    DataDir(#[from] ConfigError),
    #[error("cannot resolve {}", .path.display())]
    Resolve {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// The data folder from the program's arguments (without the program name):
/// `--data-dir <PATH>` or `--data-dir=<PATH>`, else the OS app data folder.
/// Made absolute, so it does not depend on the working folder.
///
/// # Errors
///
/// An unknown argument, a missing path, or no app data folder.
pub fn data_dir(args: impl Iterator<Item = OsString>) -> Result<PathBuf, StartupError> {
    parse(args, false).map(|launch| launch.data_dir)
}

/// What `marsa-gui` was started to be: the window, or the menu-bar panel
/// (`--panel`), with the data folder either way.
#[derive(Clone, Debug, PartialEq)]
pub struct Launch {
    pub data_dir: PathBuf,
    /// `--panel`: a borderless panel under the menu-bar icon, not the window.
    pub panel: bool,
    /// Where the icon is, for the panel: `--icon`, `--monitor`, and
    /// `--scale`, all three or none. Without them the system places the
    /// panel.
    pub anchor: Option<Anchor>,
    /// `--open sites`: the screen the window opens on, instead of Home (the
    /// tray starts the window so when the panel says "3 more sites in
    /// Marsa"). Only `sites`.
    pub open: Option<Route>,
}

/// [`data_dir`], `--panel`, and the place of the icon: the arguments of
/// `marsa-gui`.
///
/// # Errors
///
/// As [`data_dir`], and an `--icon`, `--monitor`, or `--scale` that is not
/// usable, or not all three, or with no `--panel`.
pub fn launch(args: impl Iterator<Item = OsString>) -> Result<Launch, StartupError> {
    parse(args, true)
}

/// The three flags that say where the icon is.
#[derive(Clone, Copy)]
enum AnchorFlag {
    Icon,
    Monitor,
    Scale,
}

/// `arg` as one of the anchor flags with its value: `--icon value` (the value
/// is the next argument) or `--icon=value`. `None` for any other argument.
fn anchor_flag(
    arg: &OsString,
    args: &mut impl Iterator<Item = OsString>,
) -> Result<Option<(AnchorFlag, OsString)>, StartupError> {
    const FLAGS: [(&str, AnchorFlag); 3] = [
        ("--icon", AnchorFlag::Icon),
        ("--monitor", AnchorFlag::Monitor),
        ("--scale", AnchorFlag::Scale),
    ];
    for (name, flag) in FLAGS {
        if arg == name {
            let value = args.next().ok_or(StartupError::MissingValue(name))?;
            return Ok(Some((flag, value)));
        }
        let with_value = arg
            .to_str()
            .and_then(|a| a.strip_prefix(name)?.strip_prefix('='));
        if let Some(value) = with_value {
            return Ok(Some((flag, value.into())));
        }
    }
    Ok(None)
}

fn parse(
    mut args: impl Iterator<Item = OsString>,
    panel_allowed: bool,
) -> Result<Launch, StartupError> {
    let (mut flag, mut panel) = (None, false);
    let (mut icon, mut monitor, mut scale) = (None, None, None);
    let mut open = None;
    while let Some(arg) = args.next() {
        if arg == "--data-dir" {
            flag = Some(args.next().ok_or(StartupError::MissingPath)?);
        } else if let Some(value) = arg.to_str().and_then(|a| a.strip_prefix("--data-dir=")) {
            flag = Some(value.into());
        } else if panel_allowed && arg == "--panel" {
            panel = true;
        } else if panel_allowed && arg == "--open" {
            open = Some(match args.next() {
                None => return Err(StartupError::MissingValue("--open")),
                Some(screen) if screen == "sites" => Route::Sites,
                Some(screen) => {
                    return Err(StartupError::UnknownScreen(screen.display().to_string()));
                }
            });
        } else if panel_allowed && let Some((which, value)) = anchor_flag(&arg, &mut args)? {
            match which {
                AnchorFlag::Icon => icon = Some(value),
                AnchorFlag::Monitor => monitor = Some(value),
                AnchorFlag::Scale => scale = Some(value),
            }
        } else {
            return Err(StartupError::UnknownArgument(arg.display().to_string()));
        }
    }
    let dir = match flag {
        Some(dir) => PathBuf::from(dir),
        None => marsa_core::config::app_data_dir()?,
    };
    // Not `canonicalize`: that gives `\\?\` paths on Windows, and the path is
    // passed to `marsad`.
    let data_dir =
        std::path::absolute(&dir).map_err(|source| StartupError::Resolve { path: dir, source })?;
    let anchor = anchor(panel, icon, monitor, scale)?;
    Ok(Launch {
        data_dir,
        panel,
        anchor,
        open,
    })
}

/// The anchor of the three flags: all of them, or none.
fn anchor(
    panel: bool,
    icon: Option<OsString>,
    monitor: Option<OsString>,
    scale: Option<OsString>,
) -> Result<Option<Anchor>, BadAnchor> {
    let text = |value: &OsString| {
        value
            .to_str()
            .map(str::to_owned)
            .ok_or(BadAnchor("--icon, --monitor, and --scale are numbers"))
    };
    match (icon, monitor, scale) {
        (None, None, None) => Ok(None),
        _ if !panel => Err(BadAnchor("--icon, --monitor, and --scale go with --panel")),
        (Some(icon), Some(monitor), Some(scale)) => {
            Anchor::parse(&text(&icon)?, &text(&monitor)?, &text(&scale)?).map(Some)
        }
        _ => Err(BadAnchor("--icon, --monitor, and --scale go together")),
    }
}

/// Logs to stderr. `MARSA_LOG` sets the filter, as in `marsad`.
pub fn init_logging() {
    let filter =
        EnvFilter::try_from_env("MARSA_LOG").unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG));
    // Fails only if a logger is already set, which cannot happen here.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Takes the window lock of `data_dir`: `Some` with the open file, which
/// holds the lock until it is dropped, or `None` when another window holds
/// it. The OS frees the lock when the window exits, even after a crash.
///
/// `run/` is created if needed; the daemon closes it to other users when it
/// writes its own file there.
///
/// # Errors
///
/// `run/` or the lock file cannot be created or opened.
pub fn window_lock(data_dir: &Path) -> io::Result<Option<File>> {
    let run = DataDir::new(data_dir).run();
    fs::create_dir_all(&run)?;
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(run.join(WINDOW_LOCK))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> impl Iterator<Item = OsString> {
        list.iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn the_data_dir_flag_in_both_forms_gives_an_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().to_str().unwrap();
        assert_eq!(data_dir(args(&["--data-dir", text])).unwrap(), dir.path());
        let joined = format!("--data-dir={text}");
        assert_eq!(data_dir(args(&[&joined])).unwrap(), dir.path());
        // A relative path is resolved against the working folder.
        assert!(
            data_dir(args(&["--data-dir", "rel"]))
                .unwrap()
                .is_absolute()
        );
    }

    #[test]
    fn the_panel_flag_is_for_the_window_program_only() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().to_str().unwrap();
        let panel = launch(args(&["--panel", "--data-dir", text])).unwrap();
        assert_eq!(
            panel,
            Launch {
                data_dir: dir.path().to_path_buf(),
                panel: true,
                anchor: None,
                open: None,
            }
        );
        let window = launch(args(&["--data-dir", text])).unwrap();
        assert!(!window.panel);
        // The tray takes no panel flag.
        assert!(matches!(
            data_dir(args(&["--panel", "--data-dir", text])),
            Err(StartupError::UnknownArgument(arg)) if arg == "--panel"
        ));
        // The order does not matter, and a path still needs its flag's value.
        assert!(
            launch(args(&["--data-dir", text, "--panel"]))
                .unwrap()
                .panel
        );
        assert!(matches!(
            launch(args(&["--panel", "--data-dir"])),
            Err(StartupError::MissingPath)
        ));
    }

    #[test]
    fn the_icon_the_monitor_and_the_scale_make_the_anchor_in_either_form() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().to_str().unwrap();
        let (icon, monitor) = ("2146,0,76,68", "0,0,3024,1964");
        let spaced = launch(args(&[
            "--panel",
            "--data-dir",
            text,
            "--icon",
            icon,
            "--monitor",
            monitor,
            "--scale",
            "2",
        ]))
        .unwrap();
        let joined = launch(args(&[
            "--panel",
            "--scale=2",
            &format!("--monitor={monitor}"),
            &format!("--icon={icon}"),
            &format!("--data-dir={text}"),
        ]))
        .unwrap();
        assert_eq!(spaced, joined);
        let anchor = spaced.anchor.expect("an anchor");
        assert_eq!(anchor.scale, 2.0);
        assert_eq!(anchor.icon.left(), 2146.0);
        assert_eq!(anchor.monitor.width(), 3024.0);
        // No flags, no anchor: the system places the panel.
        assert_eq!(
            launch(args(&["--panel", "--data-dir", text]))
                .unwrap()
                .anchor,
            None
        );
    }

    #[test]
    fn an_anchor_that_is_partial_bad_or_without_the_panel_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().to_str().unwrap();
        let base = ["--data-dir", text];
        let with = |extra: &[&str]| {
            let all: Vec<&str> = base.iter().chain(extra).copied().collect();
            launch(args(&all))
        };
        // Two of three.
        let partial = with(&["--panel", "--icon", "1,2,3,4", "--scale", "2"]);
        assert!(
            matches!(partial, Err(StartupError::Anchor(_))),
            "{partial:?}"
        );
        // All three, one of them garbage.
        let bad = with(&[
            "--panel",
            "--icon",
            "x",
            "--monitor",
            "0,0,9,9",
            "--scale",
            "2",
        ]);
        assert!(matches!(bad, Err(StartupError::Anchor(_))), "{bad:?}");
        // Not for the window.
        let window = with(&["--icon", "1,2,3,4", "--monitor", "0,0,9,9", "--scale", "2"]);
        assert!(matches!(window, Err(StartupError::Anchor(_))), "{window:?}");
        // A flag at the end with no value.
        assert!(matches!(
            with(&["--panel", "--icon"]),
            Err(StartupError::MissingValue("--icon"))
        ));
        // The tray takes none of them.
        assert!(matches!(
            data_dir(args(&["--icon", "1,2,3,4"])),
            Err(StartupError::UnknownArgument(_))
        ));
    }

    #[test]
    fn the_window_may_be_asked_to_open_on_the_sites_screen_and_on_no_other() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().to_str().unwrap();
        let sites = launch(args(&["--data-dir", text, "--open", "sites"])).unwrap();
        assert_eq!(sites.open, Some(Route::Sites));
        assert_eq!(launch(args(&["--data-dir", text])).unwrap().open, None);
        for refused in ["home", "", "../sites"] {
            let result = launch(args(&["--data-dir", text, "--open", refused]));
            assert!(
                matches!(result, Err(StartupError::UnknownScreen(_))),
                "{refused:?}: {result:?}"
            );
        }
        assert!(matches!(
            launch(args(&["--open"])),
            Err(StartupError::MissingValue("--open"))
        ));
        // The tray takes no such flag.
        assert!(matches!(
            data_dir(args(&["--open", "sites"])),
            Err(StartupError::UnknownArgument(_))
        ));
    }

    #[test]
    fn bad_arguments_are_refused() {
        assert!(matches!(
            data_dir(args(&["--data-dir"])),
            Err(StartupError::MissingPath)
        ));
        assert!(matches!(
            data_dir(args(&["--help"])),
            Err(StartupError::UnknownArgument(arg)) if arg == "--help"
        ));
    }

    #[test]
    fn one_window_holds_the_lock_until_it_closes() {
        let dir = tempfile::tempdir().unwrap();
        let first = window_lock(dir.path()).unwrap();
        assert!(first.is_some(), "the first window takes the lock");
        assert!(DataDir::new(dir.path()).run().join(WINDOW_LOCK).is_file());
        assert!(
            window_lock(dir.path()).unwrap().is_none(),
            "a second window finds it taken"
        );
        drop(first);
        assert!(
            window_lock(dir.path()).unwrap().is_some(),
            "the lock is free once the window is gone"
        );
    }
}
