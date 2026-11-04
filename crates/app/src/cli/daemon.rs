//! `marsa daemon start|stop|status`, and what every command that talks to
//! `marsad` shares: [`connect`] and [`ClientFailure`]. Task M3-09.
//!
//! The calls are in `marsa_core::client`; this file prints their result.
//! Nothing here prints the token: only the port and the PID leave the client.

use std::error::Error as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use marsa_core::caddy::CaddyPaths;
use marsa_core::client::{self, ClientError, DaemonClient};
use marsa_core::platform::daemon_file::daemon_file_path;

use super::strings::{self, Details};

/// The commands of `marsa daemon`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::Subcommand)]
pub enum DaemonCommand {
    /// Start the daemon if it is not running, then show its PID and port.
    Start,
    /// Stop the daemon and the services it runs.
    Stop,
    /// Show whether the daemon runs. Never starts it.
    Status,
}

/// Runs `command`: the result on stdout, an error on stderr. Exit code 0 also
/// when `stop` or `status` finds no daemon.
pub async fn run(command: DaemonCommand, data_dir: &Path) -> ExitCode {
    let result = match command {
        DaemonCommand::Start => start(data_dir).await,
        DaemonCommand::Stop => stop(data_dir).await,
        DaemonCommand::Status => status(data_dir).await,
    };
    match result {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(failure) => failure.report(),
    }
}

async fn start(data_dir: &Path) -> Result<String, ClientFailure> {
    let client = connect(data_dir).await?;
    let health = client
        .health()
        .await
        .map_err(|err| failure(&err, data_dir, None))?;
    Ok(strings::daemon_running(
        &clean(&health.app_version),
        health.pid,
        client.port(),
    ))
}

/// `POST /v1/shutdown`, then waits until the daemon has exited.
async fn stop(data_dir: &Path) -> Result<String, ClientFailure> {
    match client::stop_daemon(data_dir).await {
        Ok(true) => Ok(strings::DAEMON_STOPPED.to_owned()),
        Ok(false) => Ok(strings::DAEMON_NOT_RUNNING.to_owned()),
        Err(err) => Err(failure(&err, data_dir, None)),
    }
}

/// `GET /v1/health` on the daemon that holds the lock, if one does. A daemon
/// that is starting or stopping may show as not running.
async fn status(data_dir: &Path) -> Result<String, ClientFailure> {
    let health = async {
        let client = DaemonClient::connect(data_dir)?;
        let health = client.health().await?;
        Ok::<_, ClientError>((client.port(), health))
    };
    match health.await {
        Ok((port, health)) => Ok(strings::daemon_running(
            &clean(&health.app_version),
            health.pid,
            port,
        )),
        Err(ClientError::NotRunning) => Ok(strings::DAEMON_NOT_RUNNING.to_owned()),
        Err(err) => Err(failure(&err, data_dir, None)),
    }
}

/// The daemon's version, safe to print: without control characters, which
/// could move the cursor, and short. `status` does not check the version, so
/// the daemon may be another build.
fn clean(version: &str) -> String {
    version
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect()
}

/// [`client::ensure_running`]: a client for a daemon of this build, started
/// if needed with `marsad` from the folder of this program.
///
/// # Errors
///
/// The [`ClientFailure`] of the [`ClientError`], ready to print.
pub async fn connect(data_dir: &Path) -> Result<DaemonClient, ClientFailure> {
    client::ensure_running(data_dir)
        .await
        .map_err(|err| failure(&err, data_dir, None))
}

/// Why a command that talks to the daemon failed, ready to print. Like
/// [`super::Failure`], but the code is owned: the daemon may send one this
/// build does not know, and the line still shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientFailure {
    pub code: String,
    pub message: String,
}

impl ClientFailure {
    /// The text from [`strings`] for `code`. `service` is the service id the
    /// command is about, if any.
    pub fn new(code: &str, data_dir: &Path, service: Option<&str>) -> Self {
        let path = path_for(code, data_dir);
        let details = Details {
            service,
            path: path.as_deref(),
            ..Details::default()
        };
        Self {
            code: code.to_owned(),
            message: strings::message(code, &details),
        }
    }

    /// The same form as [`super::Failure::line`]: `marsa: <message> [<code>]`.
    pub fn line(&self) -> String {
        format!("marsa: {} [{}]", self.message, self.code)
    }

    /// Prints [`ClientFailure::line`] to stderr and gives the exit code of a
    /// failed command.
    pub fn report(&self) -> ExitCode {
        eprintln!("{}", self.line());
        ExitCode::FAILURE
    }
}

/// The text and code the user sees for `err`. Its `Display` text, with each
/// cause, goes to the log only (`MARSA_LOG=debug`).
pub fn failure(err: &ClientError, data_dir: &Path, service: Option<&str>) -> ClientFailure {
    let mut log = err.to_string();
    let mut cause = err.source();
    while let Some(current) = cause {
        log.push_str(": ");
        log.push_str(&current.to_string());
        cause = current.source();
    }
    tracing::debug!(code = err.code(), error = %log, "the daemon call failed");
    ClientFailure::new(err.code(), data_dir, service)
}

/// The file a message names, for the codes that name one.
fn path_for(code: &str, data_dir: &Path) -> Option<PathBuf> {
    match code {
        "daemon_start_failed" | "spawn_failed" => Some(data_dir.join("logs").join("marsad.log")),
        "daemon_file_invalid" | "daemon_file_insecure" | "daemon_file_write" => {
            Some(daemon_file_path(&data_dir.join("run")))
        }
        "caddy_not_installed" => Some(CaddyPaths::in_dir(data_dir).program),
        "caddy_config_write" => Some(CaddyPaths::in_dir(data_dir).config_file),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_start_failure_names_the_daemon_log() {
        let data = Path::new("/data");
        let failure = ClientFailure::new("daemon_start_failed", data, None);
        let log = data.join("logs").join("marsad.log");
        assert!(
            failure.message.ends_with(&log.display().to_string()),
            "{}",
            failure.message
        );
    }

    #[test]
    fn an_unknown_daemon_code_keeps_its_code() {
        let failure = ClientFailure::new("newer_daemon_code", Path::new("/data"), None);
        assert_eq!(
            failure.line(),
            "marsa: Something went wrong. Run again with MARSA_LOG=debug for details. \
             [newer_daemon_code]"
        );
    }

    #[test]
    fn the_failure_of_a_client_error_uses_its_code() {
        let err = ClientError::Api {
            status: 404,
            code: "service_not_found".into(),
            message: "no service named \"php\"".into(),
        };
        let failure = failure(&err, Path::new("/data"), Some("php"));
        assert_eq!(
            failure.line(),
            "marsa: There is no service named \"php\". Run \"marsa services\" to see them. \
             [service_not_found]"
        );
    }

    #[test]
    fn a_version_from_the_daemon_is_cleaned() {
        assert_eq!(clean("0.1.0"), "0.1.0");
        assert_eq!(clean("0.1\n\u{1b}[2J.0"), "0.1[2J.0");
        assert_eq!(clean(&"9".repeat(100)).len(), 64);
    }
}
