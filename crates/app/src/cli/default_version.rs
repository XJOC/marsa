//! `marsa use <tool> <version>`: makes an installed PHP or Node version the
//! default, through `PUT /v1/runtimes/{tool}/default`. Task M5-01.
//!
//! Every folder with no version of its own runs the default: the shims read
//! `global.toml`, and the daemon moves the sites that follow it, with their
//! Caddy routes and php-fpm pools. The name is Valet's (`valet use php@8.3`).

use std::path::Path;
use std::process::ExitCode;

use marsa_core::client::ClientError;
use marsa_core::config::ConfigPaths;
use marsa_core::runtimes::Runtime;
use marsa_core::types::DefaultVersion;

use super::Tool;
use super::daemon::{ClientFailure, connect, failure};
use super::sites::printable;
use super::strings::{self, Details};

/// The arguments of `marsa use`.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct UseArgs {
    /// php or node. Composer and Caddy have no default: Marsa runs the
    /// newest installed one.
    #[arg(value_enum)]
    pub tool: Tool,

    /// 8.4 or 8.4.23 for PHP, 22 for Node.js: the same rules as a
    /// .php-version file. An installed version must match it.
    pub version: String,
}

/// Starts the daemon if needed and sets the default: one line on stdout, or
/// the failure on stderr with exit code 1.
pub async fn run(args: &UseArgs, data_dir: &Path) -> ExitCode {
    let runtime = Runtime::from(args.tool);
    let client = match connect(data_dir).await {
        Ok(client) => client,
        Err(failure) => return failure.report(),
    };
    match client.set_default(runtime, &args.version).await {
        Ok(answer) => {
            println!("{}", render(runtime, &answer));
            ExitCode::SUCCESS
        }
        Err(err) => use_failure(&err, runtime, &args.version, data_dir).report(),
    }
}

/// The line for the daemon's answer: `PHP 8.3 is now the default (8.3.12).`
pub fn render(runtime: Runtime, answer: &DefaultVersion) -> String {
    strings::default_set(
        runtime,
        &printable(&answer.default),
        &printable(&answer.version),
    )
}

/// The text and code the user sees for `err`. A version code names the
/// program and the version asked for, and a `config_*` code names
/// `global.toml`, the file the daemon could not use.
pub fn use_failure(
    err: &ClientError,
    runtime: Runtime,
    version: &str,
    data_dir: &Path,
) -> ClientFailure {
    let mut failure = failure(err, data_dir, None);
    let global_toml = ConfigPaths::in_dir(data_dir).global;
    let path = match failure.code.as_str() {
        "config_read" | "config_invalid" | "config_write" => Some(global_toml.as_path()),
        "invalid_version"
        | "unsupported_alias"
        | "version_not_installed"
        | "no_default_version" => None,
        _ => return failure,
    };
    let requested = printable(version.trim());
    let details = Details {
        runtime: Some(runtime),
        requested: Some(&requested),
        path,
        ..Details::default()
    };
    failure.message = strings::message(&failure.code, &details);
    failure
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(status: u16, code: &str) -> ClientError {
        ClientError::Api {
            status,
            code: code.into(),
            message: "x".into(),
        }
    }

    #[test]
    fn the_answer_is_one_line() {
        let answer = |runtime, default: &str, version: &str| DefaultVersion {
            runtime,
            default: default.into(),
            version: version.into(),
        };
        let php = answer(Runtime::Php, "8.3", "8.3.12");
        assert_eq!(
            render(Runtime::Php, &php),
            "PHP 8.3 is now the default (8.3.12)."
        );
        let node = answer(Runtime::Node, "22", "22.11.0");
        assert_eq!(
            render(Runtime::Node, &node),
            "Node.js 22 is now the default (22.11.0)."
        );
        // A full version is not repeated, and the daemon's text is cleaned.
        let full = answer(Runtime::Php, "8.3.12", "8.3.12");
        assert_eq!(
            render(Runtime::Php, &full),
            "PHP 8.3.12 is now the default."
        );
        let odd = answer(Runtime::Php, "8.3\u{1b}[2J", "8.3.12");
        assert_eq!(
            render(Runtime::Php, &odd),
            "PHP 8.3[2J is now the default (8.3.12)."
        );
    }

    #[test]
    fn a_refusal_names_the_program_and_the_version() {
        let data = Path::new("/data");
        let line = |code, version| use_failure(&api(409, code), Runtime::Php, version, data).line();
        assert_eq!(
            line("version_not_installed", " 8.9 "),
            "marsa: PHP 8.9 is not installed. Run \"marsa install php 8.9\", then try again. \
             [version_not_installed]"
        );
        assert_eq!(
            line("invalid_version", "abc"),
            "marsa: \"abc\" is not a version. Write a version number, such as 8.4. \
             [invalid_version]"
        );
        let composer = use_failure(
            &api(400, "no_default_version"),
            Runtime::Composer,
            "2",
            data,
        );
        assert_eq!(
            composer.line(),
            "marsa: Composer has no default version: Marsa always runs the newest installed \
             one. [no_default_version]"
        );
    }

    #[test]
    fn a_settings_error_names_global_toml() {
        let data = Path::new("/data");
        let failure = use_failure(&api(500, "config_invalid"), Runtime::Node, "22", data);
        let global = ConfigPaths::in_dir(data).global;
        assert!(
            failure
                .message
                .starts_with(&format!("{} has an error", global.display())),
            "{}",
            failure.message
        );
        // Any other code keeps the general text.
        let other = use_failure(&ClientError::NotRunning, Runtime::Php, "8.3", data);
        assert_eq!(other.code, "daemon_not_running");
    }
}
