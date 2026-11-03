// No console window when Windows starts the daemon at login. Debug builds keep
// the console so logs stay visible during development (architecture.md section 9).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! `marsad`: the daemon. It is the only process that changes state; the CLI and
//! the window are its clients.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

use anyhow::Context as _;
use clap::Parser;
use marsa::daemon::server::{self, AlreadyRunning};
use marsa::daemon::supervisor::{LOG_FILE_LIMIT, LogFile};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// The exit code when another daemon already runs on the same data folder.
/// The client reads it from the same constant (task M3-08).
const EXIT_ALREADY_RUNNING: u8 = {
    let code = marsa_core::platform::spawn::EXIT_ALREADY_RUNNING;
    assert!(code > 0 && code < 256, "an exit code fits in a byte");
    code as u8
};

#[derive(Parser)]
#[command(name = "marsad", version, about = "Marsa daemon")]
struct Args {
    /// The data folder, for tests. Default: the OS app data folder. There is
    /// no environment variable for it: the shim never reads one, so `marsad`
    /// and the shim could use different folders.
    #[arg(long, value_name = "PATH")]
    data_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let set_up = data_dir(args.data_dir).and_then(|dir| init_logging(&dir).map(|()| dir));
    let data_dir = match set_up {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("marsad: {err:#}");
            return ExitCode::FAILURE;
        }
    };
    match server::run(data_dir).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => match err.downcast_ref::<AlreadyRunning>() {
            Some(running) => {
                eprintln!("marsad: {}: {running}", running.code());
                ExitCode::from(EXIT_ALREADY_RUNNING)
            }
            None => {
                tracing::error!(error = format!("{err:#}"), "marsad failed");
                eprintln!("marsad: {err:#}");
                ExitCode::FAILURE
            }
        },
    }
}

/// The data folder, made absolute.
fn data_dir(flag: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let dir = match flag {
        Some(dir) => dir,
        None => marsa_core::config::app_data_dir()?,
    };
    // Absolute, so later paths do not depend on the working folder. Not
    // `canonicalize`: that gives `\\?\` paths on Windows.
    std::path::absolute(&dir).with_context(|| format!("cannot resolve {}", dir.display()))
}

/// Logs to `logs/marsad.log`, and to stderr in debug builds. `MARSA_LOG` sets
/// the level (default `info`). Past 10 MiB the file moves to `marsad.log.1`:
/// any web page can make the daemon log rejected requests without end.
fn init_logging(data_dir: &Path) -> anyhow::Result<()> {
    let logs = data_dir.join("logs");
    fs::create_dir_all(&logs).with_context(|| format!("cannot create {}", logs.display()))?;
    let path = logs.join("marsad.log");
    let file = LogFile::open(&path, LOG_FILE_LIMIT)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let filter = EnvFilter::try_from_env("MARSA_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let to_file = tracing_subscriber::fmt::layer()
        .with_writer(Mutex::new(file))
        .with_ansi(false);
    let to_stderr = cfg!(debug_assertions)
        .then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    tracing_subscriber::registry()
        .with(filter)
        .with(to_file)
        .with(to_stderr)
        .try_init()
        .context("cannot set up logging")
}
