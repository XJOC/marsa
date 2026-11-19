//! The window's own preferences, in `<app data>/config/gui.toml`: the
//! appearance (System, Light, Dark) and the size the window had when it
//! closed.
//!
//! This is the window's state, not Marsa's: no other program reads the file,
//! and only `marsa-gui` writes it (AGENTS.md section 9). The file is read once
//! at start, and written when the appearance changes and when the window
//! closes, never while a frame draws. [Laravel: a small `config/gui.php` that
//! only this one app owns.]
//!
//! ```toml
//! # Written by marsa-gui only: the window's own preferences.
//! appearance = "dark"
//!
//! [window]
//! width = 1120
//! height = 720
//! ```
//!
//! A missing or broken file means the defaults (System, and the default size
//! of `theme::SIZE`): it is logged, and the window opens anyway.
//!
//! The window's position is not kept. Before the window exists there is no
//! list of monitors to check a saved position against, and the first frame
//! gives only the size of the monitor the window is on, not where that
//! monitor sits among the others. A position that a monitor which is gone
//! used to hold would open the window out of sight, so the system places it
//! every time, and a saved size costs nothing to restore.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use eframe::egui::{self, Vec2, vec2};
use serde::{Deserialize, Serialize};

use crate::theme;

/// The file's name, in the `config` folder of the data folder.
pub const FILE: &str = "gui.toml";

/// The first line of the file.
const HEADER: &str = "# Written by marsa-gui only: the window's own preferences.\n";

/// The longest side a saved size may have. A garbage value must not ask for
/// a window (and a graphics surface) larger than any screen.
const MAX_SIDE: f32 = 16_384.0;

/// How the window is colored: the setting of the Appearance control.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Follow the computer's setting.
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    /// What egui is asked to follow.
    pub const fn preference(self) -> egui::ThemePreference {
        match self {
            Self::System => egui::ThemePreference::System,
            Self::Light => egui::ThemePreference::Light,
            Self::Dark => egui::ThemePreference::Dark,
        }
    }
}

/// The size of the inside of the window, in points, as it is written in the
/// file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSize {
    pub width: u32,
    pub height: u32,
}

impl WindowSize {
    /// The size `size` rounds to, kept between the smallest window and
    /// `MAX_SIDE`. Not-a-number gives the smallest.
    pub fn of(size: Vec2) -> Self {
        let min = theme::SIZE.window_min;
        // `NaN.clamp(..)` is still NaN, and `NaN as u32` is 0.
        let side = |value: f32, least: f32| {
            if value.is_nan() {
                least as u32
            } else {
                value.round().clamp(least, MAX_SIDE) as u32
            }
        };
        Self {
            width: side(size.x, min.x),
            height: side(size.y, min.y),
        }
    }

    pub fn points(self) -> Vec2 {
        vec2(self.width as f32, self.height as f32)
    }
}

/// What the file holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub appearance: Appearance,
    /// The size the window had when it closed, if it ever did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowSize>,
}

impl Prefs {
    /// `<data folder>/config/gui.toml`.
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("config").join(FILE)
    }

    /// Reads the file. A missing file is the first run; a file that cannot
    /// be read or understood is logged. Both give the defaults.
    pub fn load(path: &Path) -> Self {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "cannot read gui.toml; using the defaults");
                return Self::default();
            }
        };
        // Old Notepad versions start UTF-8 files with a byte order mark.
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        toml::from_str(text).unwrap_or_else(|err| {
            tracing::warn!(path = %path.display(), error = %err, "gui.toml is not valid; using the defaults");
            Self::default()
        })
    }

    /// Writes the file atomically: a temp file in the same folder, then a
    /// rename over the file. A reader sees the old file or the new one,
    /// never half of one.
    ///
    /// # Errors
    ///
    /// The folder or the file cannot be written.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let body = toml::to_string(self).map_err(io::Error::other)?;
        let text = format!("{HEADER}\n{body}");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // The same folder, not the system's temp folder: `rename` fails
        // across volumes. std's `rename` replaces an existing file on all
        // three systems.
        let tmp = path.with_extension("toml.tmp");
        let result = write_synced(&tmp, &text).and_then(|()| fs::rename(&tmp, path));
        if result.is_err() {
            // Best effort: the write already failed, and that is the error
            // to report.
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    /// The size the window opens at: the saved one, else the default.
    pub fn window_size(&self) -> Vec2 {
        self.window.map_or(theme::SIZE.window, |saved| {
            WindowSize::of(saved.points()).points()
        })
    }
}

/// Writes and flushes to disk, so a power cut cannot leave an empty file
/// after the rename.
fn write_synced(path: &Path, text: &str) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(appearance: Appearance, size: Option<(u32, u32)>) -> Prefs {
        Prefs {
            appearance,
            window: size.map(|(width, height)| WindowSize { width, height }),
        }
    }

    #[test]
    fn what_is_saved_is_read_back() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = Prefs::path(dir.path());
        assert!(path.ends_with(Path::new("config").join(FILE)));
        for saved in [
            prefs(Appearance::Dark, Some((1000, 640))),
            prefs(Appearance::Light, None),
            Prefs::default(),
        ] {
            saved.save(&path).expect("saved");
            assert_eq!(Prefs::load(&path), saved);
        }
    }

    #[test]
    fn the_file_is_plain_toml_with_the_appearance_and_the_size() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = Prefs::path(dir.path());
        prefs(Appearance::Dark, Some((1000, 640)))
            .save(&path)
            .expect("saved");
        let text = fs::read_to_string(&path).expect("read");
        assert!(text.starts_with("# Written by marsa-gui only"), "{text}");
        assert!(text.contains("appearance = \"dark\""), "{text}");
        assert!(text.contains("[window]"), "{text}");
        assert!(text.contains("width = 1000"), "{text}");
        assert!(text.contains("height = 640"), "{text}");
        // No size, no table.
        prefs(Appearance::System, None).save(&path).expect("saved");
        let text = fs::read_to_string(&path).expect("read");
        assert!(!text.contains("[window]"), "{text}");
    }

    #[test]
    fn a_missing_file_means_the_defaults() {
        let dir = tempfile::tempdir().expect("temp dir");
        let loaded = Prefs::load(&Prefs::path(dir.path()));
        assert_eq!(loaded, Prefs::default());
        assert_eq!(loaded.appearance, Appearance::System);
        assert_eq!(loaded.window_size(), theme::SIZE.window);
    }

    #[test]
    fn a_broken_file_means_the_defaults_and_does_not_panic() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = Prefs::path(dir.path());
        fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        let broken: [&[u8]; 6] = [
            b"this is not toml [",
            b"appearance = \"purple\"\n",
            b"appearance = 3\n",
            b"[window]\nwidth = \"wide\"\nheight = 600\n",
            b"[window]\nwidth = -5\nheight = 600\n",
            // Not UTF-8 text.
            b"\xff\xfe\x00appearance",
        ];
        for bytes in broken {
            fs::write(&path, bytes).expect("written");
            assert_eq!(Prefs::load(&path), Prefs::default(), "{bytes:?}");
        }
        // A folder where the file should be cannot be read either.
        fs::remove_file(&path).expect("removed");
        fs::create_dir(&path).expect("folder in place of the file");
        assert_eq!(Prefs::load(&path), Prefs::default());
    }

    #[test]
    fn a_file_with_a_byte_order_mark_and_other_keys_still_reads() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = Prefs::path(dir.path());
        fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        fs::write(
            &path,
            "\u{feff}appearance = \"light\"\nfuture = 1\n[window]\nwidth = 900\nheight = 600\n",
        )
        .expect("written");
        assert_eq!(
            Prefs::load(&path),
            prefs(Appearance::Light, Some((900, 600)))
        );
    }

    #[test]
    fn saving_leaves_no_temp_file_and_replaces_the_old_one() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = Prefs::path(dir.path());
        prefs(Appearance::Light, None).save(&path).expect("first");
        prefs(Appearance::Dark, None).save(&path).expect("second");
        let names: Vec<_> = fs::read_dir(path.parent().expect("parent"))
            .expect("listed")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from(FILE)]);
        assert_eq!(Prefs::load(&path).appearance, Appearance::Dark);
    }

    #[test]
    fn a_save_that_fails_leaves_no_temp_file_and_says_so() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = Prefs::path(dir.path());
        // A folder where the file should be: the rename fails.
        fs::create_dir_all(&path).expect("folder in place of the file");
        assert!(prefs(Appearance::Dark, None).save(&path).is_err());
        let names: Vec<_> = fs::read_dir(path.parent().expect("parent"))
            .expect("listed")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from(FILE)], "no .tmp is left");
    }

    #[test]
    fn a_size_is_kept_between_the_smallest_window_and_a_sane_most() {
        let min = theme::SIZE.window_min;
        let size = |width, height| prefs(Appearance::System, Some((width, height))).window_size();
        assert_eq!(size(1000, 640), vec2(1000.0, 640.0));
        // Under the minimum: the minimum, on each side alone.
        assert_eq!(size(10, 10), min);
        assert_eq!(size(0, 0), min);
        assert_eq!(size(2000, 100), vec2(2000.0, min.y));
        assert_eq!(size(100, 900), vec2(min.x, 900.0));
        // Garbage that asks for a huge window.
        let huge = size(u32::MAX, u32::MAX);
        assert_eq!(huge, vec2(MAX_SIDE, MAX_SIDE));
        // The size of a live window: rounded, and not-a-number is the least.
        assert_eq!(
            WindowSize::of(vec2(1120.4, 719.6)),
            WindowSize {
                width: 1120,
                height: 720
            }
        );
        let odd = WindowSize::of(vec2(f32::NAN, f32::INFINITY));
        assert_eq!(odd.width as f32, min.x);
        assert_eq!(odd.height as f32, MAX_SIDE);
    }

    #[test]
    fn each_appearance_maps_to_the_preference_egui_follows() {
        assert_eq!(
            Appearance::System.preference(),
            egui::ThemePreference::System
        );
        assert_eq!(Appearance::Light.preference(), egui::ThemePreference::Light);
        assert_eq!(Appearance::Dark.preference(), egui::ThemePreference::Dark);
        assert_eq!(Appearance::default(), Appearance::System);
    }
}
