// No console window when Windows starts the daemon at login. Debug builds keep
// the console so logs stay visible during development (architecture.md section 9).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! `marsad`: the daemon. It is the only process that changes state; the CLI and
//! the window are its clients.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser;
use marsa::daemon::server::{self, AlreadyRunning};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// The exit code when another daemon already runs on the same data folder.
const EXIT_ALREADY_RUNNING: u8 = 3;

#[derive(Parser)]
#[command(name = "marsad", version, about = "Marsa daemon")]
struct Args {
    /// The data folder. Default: `MARSA_DATA_DIR`, then the OS app data folder.
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
    let from_env = || {
        std::env::var_os("MARSA_DATA_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let dir = match flag.or_else(from_env) {
        Some(dir) => dir,
        None => marsa_core::config::app_data_dir()?,
    };
    // Absolute, so later paths do not depend on the working folder. Not
    // `canonicalize`: that gives `\\?\` paths on Windows.
    std::path::absolute(&dir).with_context(|| format!("cannot resolve {}", dir.display()))
}

/// Logs to `logs/marsad.log`, and to stderr in debug builds. `MARSA_LOG` sets
/// the level (default `info`).
fn init_logging(data_dir: &Path) -> anyhow::Result<()> {
    let logs = data_dir.join("logs");
    fs::create_dir_all(&logs).with_context(|| format!("cannot create {}", logs.display()))?;
    let path = logs.join("marsad.log");
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let filter = EnvFilter::try_from_env("MARSA_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let to_file = tracing_subscriber::fmt::layer()
        .with_writer(Arc::new(file))
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
