//! Start the tray icon at login (architecture.md section 5, "Lifecycle").
//! Task M5-03.
//!
//! One file in the user's own folders: no system service, no admin rights,
//! and no `launchctl`. The file takes effect at the next login.
//!
//! | System | File |
//! | --- | --- |
//! | macOS | `~/Library/LaunchAgents/dev.marsa.Marsa.tray.plist`: a LaunchAgent with `RunAtLoad`, for the graphical session only (`LimitLoadToSessionType` `Aqua`), and without `KeepAlive`, so a tray icon the user quit stays quit. |
//! | Linux | `dev.marsa.Marsa.tray.desktop` in `$XDG_CONFIG_HOME/autostart` (usually `~/.config/autostart`), the XDG Autostart folder. The icon needs a graphical session, so `systemd --user` does not fit. |
//! | Windows | Not yet: the `HKCU\...\Run` key needs `windows-sys` and `unsafe` (task M4-19). |
//!
//! What starts is `marsa-tray` (task M5-04), from the folder of `marsad`. The
//! tray starts `marsad`, as the window does.
//!
//! The text of each file comes from pure functions ([`render_plist`] and
//! [`render_desktop`]). [`write`], [`remove`], and [`read`] take the folder
//! as an argument: the daemon passes the user's folder ([`user_dir`]), and
//! tests pass a temp folder. Only `marsad` calls `write` and `remove`: only
//! the daemon changes state (AGENTS.md section 6).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use super::spawn::sibling_program;

/// The LaunchAgent label, and the name of each file without its extension.
pub const LABEL: &str = "dev.marsa.Marsa.tray";
/// The program that starts at login. It sits next to `marsad`.
pub const TRAY_PROGRAM: &str = "marsa-tray";
/// The mode of the file on Unix. launchd refuses a LaunchAgent that other
/// users can write to.
#[cfg(unix)]
const FILE_MODE: u32 = 0o644;
/// The real files are under 1 KiB. A longer one is not ours.
const MAX_LEN: u64 = 64 * 1024;

/// How this system starts a program at login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartKind {
    /// macOS: a user LaunchAgent.
    LaunchAgent,
    /// Linux: an XDG Autostart desktop entry.
    XdgAutostart,
}

impl AutostartKind {
    /// The kind for this system, or `None` where Marsa has none yet
    /// (Windows).
    pub fn current() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(AutostartKind::LaunchAgent)
        } else if cfg!(target_os = "linux") {
            Some(AutostartKind::XdgAutostart)
        } else {
            None
        }
    }

    /// `dev.marsa.Marsa.tray.plist` or `dev.marsa.Marsa.tray.desktop`.
    pub fn file_name(self) -> &'static str {
        match self {
            AutostartKind::LaunchAgent => "dev.marsa.Marsa.tray.plist",
            AutostartKind::XdgAutostart => "dev.marsa.Marsa.tray.desktop",
        }
    }

    /// The folder of the file, from the user's home folder and config folder
    /// (`$XDG_CONFIG_HOME`, usually `~/.config`): `<home>/Library/LaunchAgents`
    /// on macOS, `<config>/autostart` on Linux.
    pub fn dir(self, home: &Path, config: &Path) -> PathBuf {
        match self {
            AutostartKind::LaunchAgent => home.join("Library").join("LaunchAgents"),
            AutostartKind::XdgAutostart => config.join("autostart"),
        }
    }

    /// The file inside `dir`.
    pub fn file_in(self, dir: &Path) -> PathBuf {
        dir.join(self.file_name())
    }

    /// The text of the file that starts `tray`.
    ///
    /// # Errors
    ///
    /// `autostart_path_invalid` if the file cannot hold `tray`.
    pub fn render(self, tray: &Path) -> Result<String, AutostartError> {
        match self {
            AutostartKind::LaunchAgent => render_plist(tray),
            AutostartKind::XdgAutostart => render_desktop(tray),
        }
    }

    /// The temp file of [`write`]: hidden, and without the `.plist` or
    /// `.desktop` extension, so a half-written file is never loaded.
    fn temp_name(self) -> String {
        format!(".{}.tmp", self.file_name())
    }
}

/// What [`read`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartFile {
    /// No file: Marsa does not start at login.
    Absent,
    /// The file [`write`] makes for this `marsa-tray`.
    Current,
    /// A file this Marsa would not write: it starts a `marsa-tray` at another
    /// path (the app moved), or it was changed (by an older Marsa, by the
    /// user, or by the desktop's settings). [`write`] replaces it.
    Stale,
}

/// Why the login file cannot be made, read, or changed.
#[derive(Debug, thiserror::Error)]
pub enum AutostartError {
    /// `reason` is fixed text.
    #[error("{} cannot go into a login file: {reason}", .path.display())]
    InvalidPath { path: PathBuf, reason: &'static str },

    #[error("cannot read {}", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("cannot write {}", .path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl AutostartError {
    /// Stable error name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            AutostartError::InvalidPath { .. } => "autostart_path_invalid",
            AutostartError::Read { .. } => "autostart_read",
            AutostartError::Write { .. } => "autostart_write",
        }
    }
}

/// The folder of the login file for the user who runs this program, or
/// `None` if the system gives no home folder.
pub fn user_dir(kind: AutostartKind) -> Option<PathBuf> {
    let dirs = directories::BaseDirs::new()?;
    Some(kind.dir(dirs.home_dir(), dirs.config_dir()))
}

/// `marsa-tray` in the folder of the running program (`marsad`), with `.exe`
/// on Windows. Never looked up on `PATH`.
///
/// # Errors
///
/// The `io::Error` from `current_exe`.
pub fn tray_path() -> io::Result<PathBuf> {
    sibling_program(TRAY_PROGRAM)
}

/// The LaunchAgent that starts `tray` at login. The path is XML-escaped.
///
/// # Errors
///
/// `autostart_path_invalid` if `tray` is not a full path, is not UTF-8, or
/// has a control character.
pub fn render_plist(tray: &Path) -> Result<String, AutostartError> {
    let tray = checked(tray, AutostartKind::LaunchAgent)?;
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{LABEL}</string>\n\
         \t<key>LimitLoadToSessionType</key>\n\
         \t<string>Aqua</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t<string>{program}</string>\n\
         \t</array>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         </dict>\n\
         </plist>\n",
        program = xml_text(tray),
    ))
}

/// The XDG Autostart entry that starts `tray` at login. `Exec` quotes the
/// path by the Desktop Entry spec.
///
/// # Errors
///
/// `autostart_path_invalid` if `tray` is not a full path, is not UTF-8, or
/// has a control character or a `%`.
pub fn render_desktop(tray: &Path) -> Result<String, AutostartError> {
    let tray = checked(tray, AutostartKind::XdgAutostart)?;
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.5\n\
         Name=Marsa\n\
         Comment=Starts the Marsa tray icon at login\n\
         Exec={exec}\n\
         Terminal=false\n",
        exec = exec_arg(tray),
    ))
}

/// Whether `dir` holds the login file, and whether it is the one [`write`]
/// makes for `tray`. A missing `dir` means no file.
///
/// # Errors
///
/// `autostart_read` if the file is there but cannot be read.
pub fn read(kind: AutostartKind, dir: &Path, tray: &Path) -> Result<AutostartFile, AutostartError> {
    let path = kind.file_in(dir);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(AutostartFile::Absent),
        Err(source) => return Err(AutostartError::Read { path, source }),
    };
    let mut text = Vec::new();
    if let Err(source) = file.take(MAX_LEN + 1).read_to_end(&mut text) {
        return Err(AutostartError::Read { path, source });
    }
    // The exact text, not a parse: a file the desktop changed (KDE adds
    // `Hidden=true` to turn it off, for example) may no longer start Marsa.
    // A `tray` no file can hold makes any file stale.
    let current = kind
        .render(tray)
        .is_ok_and(|wanted| wanted.as_bytes() == text);
    Ok(if current {
        AutostartFile::Current
    } else {
        AutostartFile::Stale
    })
}

/// Writes the login file for `tray` into `dir`, which is made if missing.
/// Atomic: a temp file in `dir` (so the `rename` stays on one volume), mode
/// `0644` on Unix whatever the umask, flushed to disk, then renamed over the
/// old file. Only one writer at a time: the daemon holds a lock around it.
///
/// Returns the path of the file.
///
/// # Errors
///
/// `autostart_path_invalid` if the file cannot hold `tray` (nothing is
/// written), and `autostart_write` if the folder or the file cannot be
/// written.
pub fn write(kind: AutostartKind, dir: &Path, tray: &Path) -> Result<PathBuf, AutostartError> {
    let text = kind.render(tray)?;
    let path = kind.file_in(dir);
    let temp = dir.join(kind.temp_name());
    // `write_new` closes its handle before the `rename`: Windows cannot
    // rename an open file.
    let result = fs::create_dir_all(dir)
        .and_then(|()| write_new(&temp, text.as_bytes()))
        .and_then(|()| fs::rename(&temp, &path));
    if let Err(source) = result {
        // Best effort: the write already failed, and that is the error to report.
        let _ = fs::remove_file(&temp);
        return Err(AutostartError::Write { path, source });
    }
    Ok(path)
}

/// Deletes the login file in `dir`. No file is not an error.
///
/// # Errors
///
/// `autostart_write` if the file is there and cannot be deleted.
pub fn remove(kind: AutostartKind, dir: &Path) -> Result<(), AutostartError> {
    let path = kind.file_in(dir);
    match fs::remove_file(&path) {
        Err(source) if source.kind() != io::ErrorKind::NotFound => {
            Err(AutostartError::Write { path, source })
        }
        _ => Ok(()),
    }
}

/// `tray` as text a login file can hold.
///
/// - A full path. The files are for macOS and Linux, where a path with a
///   root is a full path; `has_root` keeps the tests the same on Windows.
/// - UTF-8: both files are UTF-8 text.
/// - No control character: a line break would end the `.desktop` line, and
///   XML 1.0 forbids most of them even escaped (U+FFFE and U+FFFF too).
/// - No `%` in a `.desktop` file. The spec writes it `%%`, but GLib checks
///   that the program exists before it expands `%%`, so GNOME would skip the
///   entry without a word.
fn checked(tray: &Path, kind: AutostartKind) -> Result<&str, AutostartError> {
    let invalid = |reason| AutostartError::InvalidPath {
        path: tray.to_path_buf(),
        reason,
    };
    if !tray.has_root() {
        return Err(invalid("it is not a full path"));
    }
    let text = tray.to_str().ok_or_else(|| invalid("it is not UTF-8"))?;
    if text
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{FFFE}' | '\u{FFFF}'))
    {
        return Err(invalid("it has a control character"));
    }
    if kind == AutostartKind::XdgAutostart && text.contains('%') {
        return Err(invalid(
            "it has a %, which desktop sessions read as a field code",
        ));
    }
    Ok(text)
}

/// `text` as XML character data: `&`, `<`, and `>` as entities.
fn xml_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// `arg` as one quoted argument of `Exec` (Desktop Entry spec, "The Exec
/// key"): in double quotes, with `"`, `` ` ``, `$`, and `\` escaped by a
/// backslash. The file's string escape rule is applied after that and
/// doubles each backslash, so a literal `$` is written `\\$` and a literal
/// `\` is written `\\\\`, as the spec says. Always quoted: a path with a
/// space, `&`, `<`, or another reserved character must be, and quoting any
/// other path is allowed.
fn exec_arg(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' => {
                out.push_str(r"\\");
                out.push(c);
            }
            '\\' => out.push_str(r"\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Creates the temp file and writes `body` to disk. `create_new` refuses a
/// file or a symlink that is already there.
fn write_new(path: &Path, body: &[u8]) -> io::Result<()> {
    let mut file = match create_new(path) {
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            // Left by a write that stopped half way. The daemon writes one
            // login file at a time.
            fs::remove_file(path)?;
            create_new(path)?
        }
        other => other?,
    };
    file.write_all(body)?;
    set_mode(&file)?;
    file.sync_all()
}

fn create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, FILE_MODE);
    options.open(path)
}

/// [`FILE_MODE`] again: the umask may have taken bits off the mode of
/// `create_new` (a umask of `077` gives `0600`).
#[cfg(unix)]
fn set_mode(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(fs::Permissions::from_mode(FILE_MODE))
}

/// Windows has no mode bits, and no login file yet.
#[cfg(not(unix))]
fn set_mode(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    use AutostartKind::{LaunchAgent, XdgAutostart};

    /// Where a real install puts it on macOS: the app data folder has a space.
    const MAC_TRAY: &str = "/Users/me/Library/Application Support/dev.marsa.Marsa/bin/marsa-tray";
    const LINUX_TRAY: &str = "/home/me/.local/share/marsa/bin/marsa-tray";
    const KINDS: [AutostartKind; 2] = [LaunchAgent, XdgAutostart];

    /// The line of `text` that holds the program.
    fn program_line(kind: AutostartKind, text: &str) -> String {
        let wanted = match kind {
            LaunchAgent => "\t\t<string>",
            XdgAutostart => "Exec=",
        };
        let line = text.lines().find(|line| line.starts_with(wanted));
        line.expect("a program line").to_owned()
    }

    /// What a desktop session makes of an `Exec` value with one quoted
    /// argument, by the spec: first the string escapes (only `\\` and `\s`
    /// are used here; any other escape is refused, as GLib does), then the
    /// quoting.
    fn exec_decode(value: &str) -> String {
        let mut unescaped = String::new();
        let mut chars = value.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('\\') => unescaped.push('\\'),
                    Some('s') => unescaped.push(' '),
                    other => panic!("invalid string escape \\{other:?} in {value}"),
                }
            } else {
                unescaped.push(c);
            }
        }
        let inner = unescaped
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or_else(|| panic!("not one quoted argument: {unescaped}"));
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    let next = chars.next().expect("an escaped character");
                    assert!(matches!(next, '"' | '`' | '$' | '\\'), "\\{next}");
                    out.push(next);
                }
                '"' | '`' | '$' => panic!("{c} not escaped in {value}"),
                _ => out.push(c),
            }
        }
        out
    }

    #[test]
    fn the_plist_starts_the_tray_at_login_in_the_gui_session() {
        let text = render_plist(Path::new(MAC_TRAY)).unwrap();
        insta::assert_snapshot!("launch_agent", text);
    }

    #[test]
    fn the_desktop_entry_starts_the_tray_at_login() {
        let text = render_desktop(Path::new(LINUX_TRAY)).unwrap();
        insta::assert_snapshot!("xdg_autostart", text);
    }

    #[test]
    fn the_plist_escapes_xml() {
        for (path, expected) in [
            ("/Apps/My Marsa/marsa-tray", "/Apps/My Marsa/marsa-tray"),
            ("/Apps/R&D/marsa-tray", "/Apps/R&amp;D/marsa-tray"),
            ("/Apps/<new>/marsa-tray", "/Apps/&lt;new&gt;/marsa-tray"),
            // Fine in XML character data as they are.
            (
                r#"/Apps/"q" 'a' $x `y` \z %p/marsa-tray"#,
                r#"/Apps/"q" 'a' $x `y` \z %p/marsa-tray"#,
            ),
            ("/Apps/مرسى/marsa-tray", "/Apps/مرسى/marsa-tray"),
        ] {
            let text = render_plist(Path::new(path)).unwrap();
            assert_eq!(
                program_line(LaunchAgent, &text),
                format!("\t\t<string>{expected}</string>"),
                "{path}"
            );
        }
    }

    #[test]
    fn the_desktop_entry_quotes_exec() {
        for (path, expected) in [
            ("/opt/My Marsa/marsa-tray", r#""/opt/My Marsa/marsa-tray""#),
            ("/opt/R&D/marsa-tray", r#""/opt/R&D/marsa-tray""#),
            ("/opt/<new>/marsa-tray", r#""/opt/<new>/marsa-tray""#),
            ("/opt/a\"b/marsa-tray", r#""/opt/a\\"b/marsa-tray""#),
            ("/opt/$HOME/marsa-tray", r#""/opt/\\$HOME/marsa-tray""#),
            ("/opt/`id`/marsa-tray", r#""/opt/\\`id\\`/marsa-tray""#),
            (r"/opt/a\b/marsa-tray", r#""/opt/a\\\\b/marsa-tray""#),
            (
                "/opt/~ 'x';|*?#()/marsa-tray",
                r#""/opt/~ 'x';|*?#()/marsa-tray""#,
            ),
        ] {
            let text = render_desktop(Path::new(path)).unwrap();
            let line = program_line(XdgAutostart, &text);
            assert_eq!(line, format!("Exec={expected}"), "{path}");
            assert_eq!(exec_decode(expected), path);
        }
    }

    #[test]
    fn a_path_a_login_file_cannot_hold_is_refused() {
        for kind in KINDS {
            for path in [
                "marsa-tray",
                "bin/marsa-tray",
                "/opt/a\nb/marsa-tray",
                "/opt/a\rb/marsa-tray",
                "/opt/a\tb/marsa-tray",
                "/opt/a\u{7f}b/marsa-tray",
                "/opt/a\u{85}b/marsa-tray",
                "/opt/a\u{FFFE}b/marsa-tray",
            ] {
                let err = kind.render(Path::new(path)).unwrap_err();
                assert_eq!(err.code(), "autostart_path_invalid", "{kind:?} {path:?}");
            }
        }
        // `%` only in a `.desktop` file.
        let percent = Path::new("/opt/100%/marsa-tray");
        assert!(render_plist(percent).is_ok());
        let err = render_desktop(percent).unwrap_err();
        assert_eq!(err.code(), "autostart_path_invalid");
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_is_refused() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let path = Path::new(OsStr::from_bytes(b"/opt/\xff/marsa-tray"));
        for kind in KINDS {
            let err = kind.render(path).unwrap_err();
            assert_eq!(err.code(), "autostart_path_invalid", "{kind:?}");
            assert!(err.to_string().contains("not UTF-8"), "{err}");
        }
    }

    /// `plutil` reads the escaped file back to the same path. It only reads
    /// a file in a temp folder.
    #[cfg(target_os = "macos")]
    #[test]
    fn plutil_reads_the_path_back() {
        use std::process::Command;

        let dir = TempDir::new().unwrap();
        let tray = Path::new("/Apps/R&D <new> \"q\" 'a'/مرسى/marsa-tray");
        let file = write(LaunchAgent, dir.path(), tray).unwrap();
        let plutil = |args: &[&str]| {
            let output = Command::new("/usr/bin/plutil")
                .args(args)
                .arg(&file)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout).unwrap()
        };
        assert!(plutil(&["-lint"]).ends_with("OK\n"));
        let extract = |key: &str| plutil(&["-extract", key, "raw", "-o", "-"]);
        assert_eq!(
            extract("ProgramArguments.0").trim_end_matches('\n'),
            tray.to_str().unwrap()
        );
        assert_eq!(extract("Label").trim_end_matches('\n'), LABEL);
        assert_eq!(extract("RunAtLoad").trim_end_matches('\n'), "true");
        assert_eq!(
            extract("LimitLoadToSessionType").trim_end_matches('\n'),
            "Aqua"
        );
    }

    #[test]
    fn each_system_has_its_folder_and_file() {
        let home = Path::new("/home/me");
        let config = Path::new("/home/me/.config");
        assert_eq!(
            LaunchAgent.file_in(&LaunchAgent.dir(home, config)),
            home.join("Library")
                .join("LaunchAgents")
                .join("dev.marsa.Marsa.tray.plist")
        );
        assert_eq!(
            XdgAutostart.file_in(&XdgAutostart.dir(home, config)),
            config
                .join("autostart")
                .join("dev.marsa.Marsa.tray.desktop")
        );
        for kind in KINDS {
            assert!(kind.file_name().starts_with(LABEL));
            let temp = kind.temp_name();
            assert!(temp.starts_with('.'), "{temp}");
            assert!(!temp.ends_with(".plist") && !temp.ends_with(".desktop"));
        }
        let expected = if cfg!(target_os = "macos") {
            Some(LaunchAgent)
        } else if cfg!(target_os = "linux") {
            Some(XdgAutostart)
        } else {
            None
        };
        assert_eq!(AutostartKind::current(), expected);
        assert_eq!(
            tray_path().unwrap().file_stem().unwrap(),
            std::ffi::OsStr::new(TRAY_PROGRAM)
        );
    }

    #[test]
    fn write_then_read_is_current_and_leaves_no_temp_file() {
        for kind in KINDS {
            let dir = TempDir::new().unwrap();
            // Made if missing, with its parents.
            let target = dir.path().join("Library").join("LaunchAgents");
            let tray = Path::new(LINUX_TRAY);
            assert_eq!(read(kind, &target, tray).unwrap(), AutostartFile::Absent);

            let path = write(kind, &target, tray).unwrap();
            assert_eq!(path, kind.file_in(&target));
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                kind.render(tray).unwrap()
            );
            assert_eq!(read(kind, &target, tray).unwrap(), AutostartFile::Current);
            let names: Vec<_> = fs::read_dir(&target)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(names, [kind.file_name()], "{kind:?}");
        }
    }

    #[test]
    fn a_file_for_another_tray_or_a_changed_file_is_stale() {
        for kind in KINDS {
            let dir = TempDir::new().unwrap();
            let old = Path::new("/old/place/marsa-tray");
            let new = Path::new("/new/place/marsa-tray");
            write(kind, dir.path(), old).unwrap();
            assert_eq!(read(kind, dir.path(), new).unwrap(), AutostartFile::Stale);
            assert_eq!(read(kind, dir.path(), old).unwrap(), AutostartFile::Current);

            // Writing again for the new path replaces the stale file.
            write(kind, dir.path(), new).unwrap();
            assert_eq!(read(kind, dir.path(), new).unwrap(), AutostartFile::Current);

            let path = kind.file_in(dir.path());
            let current = kind.render(new).unwrap();
            for text in [
                String::new(),
                format!("{current}Hidden=true\n"),
                current.replace('\n', "\r\n"),
                format!("{current}{}", " ".repeat(MAX_LEN as usize)),
            ] {
                fs::write(&path, &text).unwrap();
                assert_eq!(read(kind, dir.path(), new).unwrap(), AutostartFile::Stale);
            }
            // A tray path no file can hold: any file is stale.
            let bad = Path::new("relative/marsa-tray");
            assert_eq!(read(kind, dir.path(), bad).unwrap(), AutostartFile::Stale);
        }
    }

    #[test]
    fn remove_deletes_the_file_and_accepts_no_file() {
        for kind in KINDS {
            let dir = TempDir::new().unwrap();
            let tray = Path::new(LINUX_TRAY);
            write(kind, dir.path(), tray).unwrap();
            remove(kind, dir.path()).unwrap();
            assert_eq!(read(kind, dir.path(), tray).unwrap(), AutostartFile::Absent);
            remove(kind, dir.path()).unwrap();
            // No folder at all.
            remove(kind, &dir.path().join("missing")).unwrap();
        }
    }

    #[test]
    fn write_replaces_a_temp_file_left_by_a_crash() {
        let dir = TempDir::new().unwrap();
        let temp = dir.path().join(LaunchAgent.temp_name());
        fs::write(&temp, "left by a crash").unwrap();
        write(LaunchAgent, dir.path(), Path::new(MAC_TRAY)).unwrap();
        assert!(!temp.exists());
        let tray = Path::new(MAC_TRAY);
        assert_eq!(
            read(LaunchAgent, dir.path(), tray).unwrap(),
            AutostartFile::Current
        );
    }

    #[test]
    fn failures_have_their_codes_and_write_nothing() {
        let dir = TempDir::new().unwrap();
        let tray = Path::new(LINUX_TRAY);

        // The folder is a file.
        let file = dir.path().join("file");
        fs::write(&file, "").unwrap();
        let err = write(XdgAutostart, &file, tray).unwrap_err();
        assert_eq!(err.code(), "autostart_write", "{err}");
        let err = remove(XdgAutostart, &file).unwrap_err();
        assert_eq!(err.code(), "autostart_write", "{err}");

        // The login file is a folder: the rename fails, and the temp file goes.
        let target = dir.path().join("autostart");
        fs::create_dir_all(XdgAutostart.file_in(&target)).unwrap();
        let err = write(XdgAutostart, &target, tray).unwrap_err();
        assert_eq!(err.code(), "autostart_write", "{err}");
        assert!(!target.join(XdgAutostart.temp_name()).exists());

        // A path the file cannot hold: nothing is written.
        let empty = dir.path().join("empty");
        let err = write(XdgAutostart, &empty, Path::new("/a%b")).unwrap_err();
        assert_eq!(err.code(), "autostart_path_invalid");
        assert!(!empty.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_login_file_that_cannot_be_read_is_an_error() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(LaunchAgent.file_in(dir.path())).unwrap();
        let err = read(LaunchAgent, dir.path(), Path::new(MAC_TRAY)).unwrap_err();
        assert_eq!(err.code(), "autostart_read", "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn unix_file_is_0644_also_over_an_old_file() {
        use std::os::unix::fs::PermissionsExt;

        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let dir = TempDir::new().unwrap();
        let path = LaunchAgent.file_in(dir.path());
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        let temp = dir.path().join(LaunchAgent.temp_name());
        fs::write(&temp, "left by a crash").unwrap();
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o600)).unwrap();

        write(LaunchAgent, dir.path(), Path::new(MAC_TRAY)).unwrap();
        assert_eq!(mode(&path), 0o644);
    }
}
