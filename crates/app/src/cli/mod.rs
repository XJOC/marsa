//! The commands of the `marsa` binary. `bin/marsa.rs` parses the arguments
//! and calls one `run` function per command.
//!
//! The commands hold no logic of their own: they call the daemon through
//! `marsa_core::client` and print what it reports. Every text the user sees
//! comes from [`strings`] (AGENTS.md section 9).

pub mod autostart;
pub mod daemon;
pub mod default_version;
pub mod install;
pub mod logs;
pub mod progress;
pub mod runtimes;
pub mod services;
pub mod setup;
pub mod sites;
pub mod strings;
pub mod xdebug;

use std::path::PathBuf;
use std::process::ExitCode;

use marsa_core::config;
use marsa_core::runtimes::Runtime;

/// A program `marsa install`, `marsa runtimes`, and `marsa use` know. The
/// command-line form of [`Runtime`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Tool {
    Php,
    Node,
    Composer,
    /// The web server `marsad` runs. Installed like the others, but no shim
    /// runs it.
    Caddy,
}

impl From<Tool> for Runtime {
    fn from(tool: Tool) -> Self {
        match tool {
            Tool::Php => Runtime::Php,
            Tool::Node => Runtime::Node,
            Tool::Composer => Runtime::Composer,
            Tool::Caddy => Runtime::Caddy,
        }
    }
}

/// Why a command failed, ready to print: the text from [`strings`] and the
/// stable code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
}

impl Failure {
    /// The first line on stderr, in the same form as the shim:
    /// `marsa: <message> [<code>]`.
    pub fn line(&self) -> String {
        format!("marsa: {} [{}]", self.message, self.code)
    }

    /// Prints [`Failure::line`] to stderr and gives the exit code of a failed
    /// command.
    pub fn report(&self) -> ExitCode {
        eprintln!("{}", self.line());
        ExitCode::FAILURE
    }
}

/// The app data dir: `data_dir` if the `--data-dir` flag gave one, else the
/// one the OS gives ([`config::app_data_dir`]).
///
/// # Errors
///
/// `config_no_home` if the OS gives no home folder.
pub fn data_dir(data_dir: Option<PathBuf>) -> Result<PathBuf, Failure> {
    match data_dir {
        Some(dir) => Ok(dir),
        None => config::app_data_dir().map_err(|err| {
            tracing::debug!(error = %err, "no app data dir");
            Failure {
                code: err.code(),
                message: strings::message(err.code(), &strings::Details::default()),
            }
        }),
    }
}

/// Logs go to stderr at `warn`. `MARSA_LOG=debug` (any `EnvFilter` value)
/// changes the level.
pub fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("MARSA_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}
