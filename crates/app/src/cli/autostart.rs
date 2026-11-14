//! `marsa autostart` and `marsa autostart on|off`: start Marsa at login, or
//! not, through `GET` and `PUT /v1/autostart`. Task M5-03.
//!
//! The daemon writes or deletes one file in the user's folders: a
//! LaunchAgent on macOS, an XDG Autostart entry on Linux. It starts the tray
//! icon (`marsa-tray`), which starts `marsad`. The change takes effect at the
//! next login.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use marsa_core::client::ClientError;
use marsa_core::platform::autostart::{AutostartKind, user_dir};
use marsa_core::types::AutostartState;

use super::daemon::{ClientFailure, connect, failure};
use super::strings::{self, Details};

/// The arguments of `marsa autostart`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::Args)]
pub struct AutostartArgs {
    /// on or off. Without it, show whether Marsa starts at login.
    #[arg(value_enum)]
    pub switch: Option<Switch>,
}

/// `on` or `off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Switch {
    On,
    Off,
}

/// Starts the daemon if needed, then reads or changes the setting: one line
/// on stdout, or the failure on stderr with exit code 1.
pub async fn run(args: &AutostartArgs, data_dir: &Path) -> ExitCode {
    let client = match connect(data_dir).await {
        Ok(client) => client,
        Err(failure) => return failure.report(),
    };
    let answer = match args.switch {
        None => client.autostart().await,
        Some(switch) => client.set_autostart(switch == Switch::On).await,
    };
    match answer {
        Ok(state) => {
            println!("{}", render(args.switch, state));
            ExitCode::SUCCESS
        }
        Err(err) => autostart_failure(&err, data_dir, login_file().as_deref()).report(),
    }
}

/// The line for the daemon's answer to `marsa autostart` (`switch` is
/// `None`) or to `marsa autostart on|off`.
pub fn render(switch: Option<Switch>, state: AutostartState) -> &'static str {
    match (switch, state.enabled, state.stale) {
        (Some(Switch::On), true, _) => strings::AUTOSTART_TURNED_ON,
        (Some(Switch::Off), false, _) => strings::AUTOSTART_TURNED_OFF,
        (_, true, _) => strings::AUTOSTART_ON,
        (_, false, true) => strings::AUTOSTART_STALE,
        (_, false, false) => strings::AUTOSTART_OFF,
    }
}

/// The text and code the user sees for `err`. `autostart_read` and
/// `autostart_write` name `login_file`.
pub fn autostart_failure(
    err: &ClientError,
    data_dir: &Path,
    login_file: Option<&Path>,
) -> ClientFailure {
    let mut failure = failure(err, data_dir, None);
    if matches!(failure.code.as_str(), "autostart_read" | "autostart_write") {
        let details = Details {
            path: login_file,
            ..Details::default()
        };
        failure.message = strings::message(&failure.code, &details);
    }
    failure
}

/// The login file the daemon uses, for messages only. The CLI and the daemon
/// run as the same user on the same machine, so they find the same folder.
fn login_file() -> Option<PathBuf> {
    let kind = AutostartKind::current()?;
    user_dir(kind).map(|dir| kind.file_in(&dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(enabled: bool, stale: bool) -> AutostartState {
        AutostartState { enabled, stale }
    }

    #[test]
    fn each_answer_has_its_line() {
        assert_eq!(
            render(None, state(true, false)),
            "Start at login: on. Marsa's tray icon starts when you log in."
        );
        assert_eq!(
            render(None, state(false, false)),
            "Start at login: off. Run \"marsa autostart on\" to turn it on."
        );
        assert!(render(None, state(false, true)).contains("\"marsa autostart on\" to fix it"));
        assert_eq!(
            render(Some(Switch::On), state(true, false)),
            "Start at login: on. From your next login, Marsa's tray icon starts by itself."
        );
        assert_eq!(
            render(Some(Switch::Off), state(false, false)),
            "Start at login: off."
        );
    }

    #[test]
    fn a_file_error_names_the_login_file() {
        let api = |status, code: &str| ClientError::Api {
            status,
            code: code.into(),
            message: "x".into(),
        };
        let data = Path::new("/data");
        let file = Path::new("/home/me/.config/autostart/dev.marsa.Marsa.tray.desktop");
        let failure = autostart_failure(&api(500, "autostart_write"), data, Some(file));
        assert_eq!(
            failure.line(),
            format!(
                "marsa: Cannot change {}. Check the permissions of the file and its folder. \
                 [autostart_write]",
                file.display()
            )
        );
        let failure = autostart_failure(&api(500, "autostart_read"), data, None);
        assert!(
            failure.message.starts_with("Cannot read the login file."),
            "{}",
            failure.message
        );
        let failure = autostart_failure(&api(409, "tray_not_found"), data, Some(file));
        assert_eq!(
            failure.line(),
            "marsa: The marsa-tray program is not next to marsad, so Marsa cannot start at \
             login. Reinstall Marsa, then try again. [tray_not_found]"
        );
    }
}
