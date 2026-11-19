//! Opening a site's folder in the system's file manager ("Open folder" on a
//! site's page, task M5-D7). A local action: `App` does it, never a screen
//! (code.md, State).
//!
//! The program is the one `marsa open` opens a URL with, found by its
//! absolute path (`marsa_core::platform::browser::opener`), and each of them
//! shows a folder in the file manager when it is given one:
//!
//! - macOS: `/usr/bin/open <folder>`.
//! - Windows: `<SystemRoot>\explorer.exe <folder>`.
//! - Linux and the other Unix systems: `xdg-open <folder>`, looked for in the
//!   standard folders and the absolute folders of `PATH`.
//!
//! The folder is the one argument, as the path itself: never a shell string,
//! never joined into a command line, and never shortened (`~` is display
//! only).
//!
//! The menu-bar icon opens a site the same way ([`open_url`], task M5-D11): the
//! window's own button hands the address to egui, which has no window to do it
//! in the icon's program.

use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use marsa_core::platform::browser::{self, Opener};

/// The code of an error the notice at the bottom of the window says in
/// words (`strings::error_text`).
const FAILED: &str = "open_failed";

/// The opener for `folder`, or the code of why there is none:
/// `open_no_program` when the system has no opener, `open_failed` for a
/// folder that is not an absolute path (a name that starts with `-` would
/// pass for an option).
pub fn folder_opener(folder: &Path) -> Result<Opener, &'static str> {
    if !folder.is_absolute() {
        return Err(FAILED);
    }
    // The program is found as for a URL; only its argument differs, and it
    // stays the path as the system wrote it, not text that may have lost
    // characters.
    let mut opener = browser::opener("").map_err(|err| err.code())?;
    opener.args = vec![folder.as_os_str().to_owned()];
    Ok(opener)
}

/// Starts the opener for `folder` and returns at once: the window never
/// waits for a program. A thread collects its exit status, so no zombie
/// stays, and logs a failure.
///
/// # Errors
///
/// The code of why it did not start: see [`folder_opener`], and
/// `open_failed` when the program cannot be run.
pub fn open_folder(folder: &Path) -> Result<(), &'static str> {
    start(folder_opener(folder)?)
}

/// The opener for the address of a site (`https://shop.test`), or the code of
/// why there is none: `open_failed` for anything that is not an `http` or an
/// `https` address with no space in it (an argument that starts with `-` would
/// pass for an option, and `file:` would open a file), `open_no_program` when
/// the system has no opener.
pub fn url_opener(url: &str) -> Result<Opener, &'static str> {
    let web = url.starts_with("https://") || url.starts_with("http://");
    if !web || url.chars().any(char::is_whitespace) {
        return Err(FAILED);
    }
    browser::opener(url).map_err(|err| err.code())
}

/// Opens `url` in the browser: [`url_opener`], started as [`open_folder`]
/// does.
///
/// # Errors
///
/// As [`url_opener`], and `open_failed` when the program cannot be run.
pub fn open_url(url: &str) -> Result<(), &'static str> {
    start(url_opener(url)?)
}

/// Starts `opener` and returns at once.
fn start(opener: Opener) -> Result<(), &'static str> {
    let mut child = Command::new(&opener.program)
        .args(&opener.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| {
            tracing::warn!(error = %err, program = %opener.program.display(), "cannot open it");
            FAILED
        })?;
    let Opener {
        program,
        checks_status,
        ..
    } = opener;
    let reaper = thread::Builder::new()
        .name("opener".to_owned())
        .stack_size(64 * 1024)
        .spawn(move || match child.wait() {
            // `explorer.exe` exits with 1 even when it opened the folder.
            Ok(status) if checks_status && !status.success() => {
                tracing::warn!(%status, program = %program.display(), "the opener failed");
            }
            Ok(_) => {}
            Err(err) => tracing::debug!(error = %err, "cannot wait for the opener"),
        });
    if let Err(err) = reaper {
        // The `Child` is dropped, which does not stop the opener.
        tracing::debug!(error = %err, "cannot start a thread to reap the opener");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn the_opener_is_absolute_and_gets_the_folder_alone_as_it_is() {
        // A folder with spaces and a dash: still one argument, unchanged.
        let folder = std::env::temp_dir().join("My Site").join("-odd");
        // On Linux without `xdg-open` (a bare CI image), there is nothing to
        // check but the code.
        match folder_opener(&folder) {
            Ok(opener) => {
                assert!(opener.program.is_absolute(), "{opener:?}");
                assert_eq!(opener.args, [OsString::from(&folder)]);
                if cfg!(target_os = "macos") {
                    assert_eq!(opener.program, Path::new("/usr/bin/open"));
                }
                if cfg!(windows) {
                    assert!(opener.program.ends_with("explorer.exe"), "{opener:?}");
                }
            }
            Err(code) => assert_eq!(code, "open_no_program"),
        }
    }

    #[test]
    fn a_relative_folder_is_refused_before_anything_starts() {
        for relative in ["", "shop", "-rf", "./shop", "../shop"] {
            assert_eq!(
                folder_opener(&PathBuf::from(relative)).map(|_| ()),
                Err("open_failed"),
                "{relative:?}"
            );
            assert_eq!(open_folder(Path::new(relative)), Err("open_failed"));
        }
    }

    #[test]
    fn only_a_web_address_without_spaces_opens_in_the_browser() {
        for refused in [
            "",
            "shop.test",
            "-n",
            "file:///etc/passwd",
            "ftp://shop.test",
            "https://shop.test and more",
            "https://shop.test\n--args",
            "javascript:alert(1)",
        ] {
            assert_eq!(
                url_opener(refused).map(|_| ()),
                Err("open_failed"),
                "{refused:?}"
            );
            assert_eq!(open_url(refused), Err("open_failed"), "{refused:?}");
        }
        // The address is the one argument, as it is.
        match url_opener("https://shop.test") {
            Ok(opener) => {
                assert!(opener.program.is_absolute(), "{opener:?}");
                assert_eq!(opener.args, [OsString::from("https://shop.test")]);
            }
            // A bare Linux image has no `xdg-open`.
            Err(code) => assert_eq!(code, "open_no_program"),
        }
    }

    #[test]
    fn the_codes_the_opener_gives_have_a_text() {
        for code in ["open_failed", "open_no_program"] {
            assert!(
                !crate::strings::error_text(code).starts_with("Unexpected error"),
                "{code}"
            );
        }
    }
}
