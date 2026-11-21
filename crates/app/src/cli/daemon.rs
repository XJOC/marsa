//! `marsa daemon start|stop|status`, and what every command that talks to
//! `marsad` shares: [`connect`] and [`failure`].
//!
//! The calls are in `marsa_core::client`; this file prints their result.
//! Nothing here prints the token: only the port and the PID leave the client.

use marsa_core::codes;
use std::error::Error as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use marsa_core::caddy::CaddyPaths;
use marsa_core::client::{self, ClientError, DaemonClient};
use marsa_core::platform::daemon_file::daemon_file_path;
use marsa_core::runtimes::RuntimesDir;

use super::Failure;
use super::strings::{self, Details};
use super::text::clipped;

/// The longest daemon version shown. `status` does not check the version, so
/// the daemon may be another build, and its text may be long.
const VERSION_MAX: usize = 64;

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
pub async fn run(command: DaemonCommand, data_dir: &Path) -> Result<ExitCode, Failure> {
    let text = match command {
        DaemonCommand::Start => start(data_dir).await,
        DaemonCommand::Stop => stop(data_dir).await,
        DaemonCommand::Status => status(data_dir).await,
    }?;
    println!("{text}");
    Ok(ExitCode::SUCCESS)
}

async fn start(data_dir: &Path) -> Result<String, Failure> {
    let client = connect(data_dir).await?;
    let health = client
        .health()
        .await
        .map_err(|err| failure(&err, data_dir, None))?;
    Ok(strings::daemon_running(
        &clipped(&health.app_version, VERSION_MAX),
        health.pid,
        client.port(),
    ))
}

/// `POST /v1/shutdown`, then waits until the daemon has exited.
async fn stop(data_dir: &Path) -> Result<String, Failure> {
    match client::stop_daemon(data_dir).await {
        Ok(true) => Ok(strings::DAEMON_STOPPED.to_owned()),
        Ok(false) => Ok(strings::DAEMON_NOT_RUNNING.to_owned()),
        Err(err) => Err(failure(&err, data_dir, None)),
    }
}

/// `GET /v1/health` on the daemon that holds the lock, if one does. A daemon
/// that is starting or stopping may show as not running.
async fn status(data_dir: &Path) -> Result<String, Failure> {
    let health = async {
        let client = DaemonClient::connect(data_dir)?;
        let health = client.health().await?;
        Ok::<_, ClientError>((client.port(), health))
    };
    match health.await {
        Ok((port, health)) => Ok(strings::daemon_running(
            &clipped(&health.app_version, VERSION_MAX),
            health.pid,
            port,
        )),
        Err(ClientError::NotRunning) => Ok(strings::DAEMON_NOT_RUNNING.to_owned()),
        Err(err) => Err(failure(&err, data_dir, None)),
    }
}

/// [`client::ensure_running`]: a client for a daemon of this build, started
/// if needed with `marsad` from the folder of this program.
///
/// # Errors
///
/// The [`Failure`] of the [`ClientError`], ready to print.
pub async fn connect(data_dir: &Path) -> Result<DaemonClient, Failure> {
    client::ensure_running(data_dir)
        .await
        .map_err(|err| failure(&err, data_dir, None))
}

impl Failure {
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
            code: code.to_owned().into(),
            message: strings::message(code, &details),
        }
    }
}

/// The text and code the user sees for `err`. Its `Display` text, with each
/// cause, goes to the log only (`MARSA_LOG=debug`).
pub fn failure(err: &ClientError, data_dir: &Path, service: Option<&str>) -> Failure {
    let mut log = err.to_string();
    let mut cause = err.source();
    while let Some(current) = cause {
        log.push_str(": ");
        log.push_str(&current.to_string());
        cause = current.source();
    }
    tracing::debug!(code = err.code(), error = %log, "the daemon call failed");
    Failure::new(err.code(), data_dir, service)
}

/// [`failure`] for a command whose answers with one of `codes` name `path`:
/// the file the daemon could not use.
pub fn failure_naming(
    err: &ClientError,
    data_dir: &Path,
    codes: &[&str],
    path: Option<&Path>,
) -> Failure {
    let mut failure = failure(err, data_dir, None);
    if codes.contains(&failure.code.as_ref()) {
        let details = Details {
            path,
            ..Details::default()
        };
        failure.message = strings::message(&failure.code, &details);
    }
    failure
}

/// The file a message names, for the codes that name one.
fn path_for(code: &str, data_dir: &Path) -> Option<PathBuf> {
    match code {
        codes::DAEMON_START_FAILED | codes::SPAWN_FAILED => {
            Some(data_dir.join("logs").join("marsad.log"))
        }
        codes::DAEMON_FILE_INVALID | codes::DAEMON_FILE_INSECURE | codes::DAEMON_FILE_WRITE => {
            Some(daemon_file_path(&data_dir.join("run")))
        }
        codes::CADDY_NOT_INSTALLED => Some(CaddyPaths::in_dir(data_dir).runtime_dir()),
        codes::CADDY_CONFIG_WRITE => Some(CaddyPaths::in_dir(data_dir).config_file),
        codes::RUNTIMES_READ => Some(RuntimesDir::in_dir(data_dir).root().to_path_buf()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_start_failure_names_the_daemon_log() {
        let data = Path::new("/data");
        let failure = Failure::new("daemon_start_failed", data, None);
        let log = data.join("logs").join("marsad.log");
        assert!(
            failure.message.ends_with(&log.display().to_string()),
            "{}",
            failure.message
        );
    }

    #[test]
    fn no_caddy_names_its_folder_and_the_install_command() {
        let data = Path::new("/data");
        let failure = Failure::new("caddy_not_installed", data, Some("caddy"));
        let caddy = data.join("runtimes").join("caddy");
        assert_eq!(
            failure.line(),
            format!(
                "marsa: Caddy is not installed in {}. Run \"marsa install caddy\", then try \
                 again. [caddy_not_installed]",
                caddy.display()
            )
        );
    }

    #[test]
    fn an_unknown_daemon_code_keeps_its_code() {
        let failure = Failure::new("newer_daemon_code", Path::new("/data"), None);
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
}
