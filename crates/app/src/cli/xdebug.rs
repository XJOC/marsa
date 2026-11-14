//! `marsa xdebug` and `marsa xdebug on|off`: load Xdebug in every installed
//! PHP, or in none, through `GET` and `PUT /v1/php/xdebug`. Task M5-11.
//!
//! The daemon saves the setting, writes each `php.ini`, and restarts the
//! php-fpm it changed. `php` in the terminal reads the same `php.ini`, so it
//! follows.

use std::path::Path;
use std::process::ExitCode;

use marsa_core::client::ClientError;
use marsa_core::config::ConfigPaths;
use marsa_core::types::XdebugState;

use super::autostart::Switch;
use super::daemon::{ClientFailure, connect, failure};
use super::strings::{self, Details};

/// The arguments of `marsa xdebug`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::Args)]
pub struct XdebugArgs {
    /// on or off. Without it, show whether PHP loads Xdebug.
    #[arg(value_enum)]
    pub switch: Option<Switch>,
}

/// Starts the daemon if needed, then reads or changes the setting: one line
/// on stdout, or the failure on stderr with exit code 1.
pub async fn run(args: &XdebugArgs, data_dir: &Path) -> ExitCode {
    let client = match connect(data_dir).await {
        Ok(client) => client,
        Err(failure) => return failure.report(),
    };
    let answer = match args.switch {
        None => client.xdebug().await,
        Some(switch) => client.set_xdebug(switch == Switch::On).await,
    };
    match answer {
        Ok(state) => {
            println!("{}", render(args.switch, &state));
            ExitCode::SUCCESS
        }
        Err(err) => xdebug_failure(&err, data_dir).report(),
    }
}

/// The line for the daemon's answer to `marsa xdebug` (`switch` is `None`)
/// or to `marsa xdebug on|off`.
pub fn render(switch: Option<Switch>, state: &XdebugState) -> String {
    match (state.enabled, switch) {
        (true, switch) => {
            let turned_on = switch == Some(Switch::On);
            strings::xdebug_on(&state.loaded, &state.missing, turned_on)
        }
        (false, Some(Switch::Off)) => strings::XDEBUG_TURNED_OFF.to_owned(),
        (false, _) => strings::XDEBUG_OFF.to_owned(),
    }
}

/// The text and code the user sees for `err`. A `config_*` code names
/// `global.toml`, the file the daemon could not use.
pub fn xdebug_failure(err: &ClientError, data_dir: &Path) -> ClientFailure {
    let mut failure = failure(err, data_dir, None);
    if matches!(
        failure.code.as_str(),
        "config_read" | "config_invalid" | "config_write"
    ) {
        let global_toml = ConfigPaths::in_dir(data_dir).global;
        let details = Details {
            path: Some(&global_toml),
            ..Details::default()
        };
        failure.message = strings::message(&failure.code, &details);
    }
    failure
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(enabled: bool, loaded: &[&str], missing: &[&str]) -> XdebugState {
        let list = |versions: &[&str]| versions.iter().map(|v| (*v).to_owned()).collect();
        XdebugState {
            enabled,
            loaded: list(loaded),
            missing: list(missing),
        }
    }

    #[test]
    fn each_answer_has_its_line() {
        let off = state(false, &[], &["8.3.1"]);
        assert_eq!(
            render(None, &off),
            "Xdebug: off. Run \"marsa xdebug on\" to turn it on."
        );
        assert_eq!(
            render(Some(Switch::Off), &off),
            "Xdebug: off. PHP runs at full speed again."
        );

        let on = state(true, &["8.4.3", "8.5.0"], &["8.3.1"]);
        let status = "Xdebug: on, in your sites and in the terminal. Loaded by PHP 8.4.3, 8.5.0. \
                      Not loaded by PHP 8.3.1: installed without Xdebug.";
        assert_eq!(render(None, &on), status);
        assert_eq!(
            render(Some(Switch::On), &on),
            format!(
                "{status} PHP can run up to 4 times slower while it is on. Run \"marsa xdebug \
                 off\" when you finish."
            )
        );
        assert_eq!(
            render(None, &state(true, &["8.4.3"], &[])),
            "Xdebug: on, in your sites and in the terminal. Loaded by PHP 8.4.3."
        );
        // Windows, or no PHP yet: on, and nothing loads it.
        for none in [state(true, &[], &["8.4.3"]), state(true, &[], &[])] {
            assert_eq!(
                render(Some(Switch::On), &none),
                "Xdebug: on, but no installed PHP has Xdebug."
            );
        }
    }

    #[test]
    fn a_settings_error_names_global_toml() {
        let data = Path::new("/data");
        let err = ClientError::Api {
            status: 500,
            code: "config_invalid".into(),
            message: "x".into(),
        };
        let global = ConfigPaths::in_dir(data).global;
        let failure = xdebug_failure(&err, data);
        let start = format!("{} has an error", global.display());
        assert!(failure.message.starts_with(&start), "{}", failure.message);
        // Any other code keeps the general text.
        let other = xdebug_failure(&ClientError::NotRunning, data);
        assert_eq!(other.code, "daemon_not_running");
        let stopped = ClientError::Api {
            status: 500,
            code: "xdebug_interrupted".into(),
            message: "x".into(),
        };
        assert_eq!(
            xdebug_failure(&stopped, data).message,
            "The Marsa daemon stopped before the change reached PHP. The setting is saved: \
             Marsa applies it when it starts again."
        );
    }
}
