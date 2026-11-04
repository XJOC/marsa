// No console window behind the window on Windows. Debug builds keep the console
// so logs stay visible during development (architecture.md section 9).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! `marsa-gui`: the window. A client of the daemon, like the CLI; it never
//! changes state by itself (task M3-10).
//!
//! `marsa-gui [--data-dir <PATH>]`. The flag is for tests; the default is
//! the OS app data folder. No environment variable overrides it: the shim
//! never reads one, so the window and the shim always share one folder.

mod app;
mod bridge;
mod model;
mod strings;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context as _, bail};
use eframe::egui;
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    let started = Instant::now();
    init_logging();
    match run(started) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = format!("{err:#}"), "marsa-gui failed");
            ExitCode::FAILURE
        }
    }
}

fn run(started: Instant) -> anyhow::Result<()> {
    let data_dir = data_dir(std::env::args_os().skip(1))?;
    // Connecting starts now, while the window opens.
    let bridge = bridge::start(data_dir).context("cannot start the bridge thread")?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(strings::APP_NAME)
            .with_inner_size(strings::WINDOW_SIZE)
            .with_min_inner_size(strings::WINDOW_MIN_SIZE),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        strings::APP_NAME,
        options,
        Box::new(move |creation| Ok(Box::new(app::App::new(&creation.egui_ctx, bridge, started)))),
    )
    .map_err(|err| anyhow::anyhow!("cannot open the window: {err}"))
}

/// The data folder: `--data-dir` (for tests), else the OS app data folder,
/// made absolute.
fn data_dir(mut args: impl Iterator<Item = OsString>) -> anyhow::Result<PathBuf> {
    let mut flag = None;
    while let Some(arg) = args.next() {
        if arg == "--data-dir" {
            flag = Some(args.next().context("--data-dir needs a path")?);
        } else if let Some(value) = arg.to_str().and_then(|a| a.strip_prefix("--data-dir=")) {
            flag = Some(value.into());
        } else {
            bail!("unknown argument {}", arg.display());
        }
    }
    let dir = match flag {
        Some(dir) => PathBuf::from(dir),
        None => marsa_core::config::app_data_dir()?,
    };
    // Not `canonicalize`: that gives `\\?\` paths on Windows, and the path is
    // passed to `marsad`.
    std::path::absolute(&dir).with_context(|| format!("cannot resolve {}", dir.display()))
}

/// Logs to stderr. `MARSA_LOG` sets the filter, as in `marsad`. The default
/// keeps the graphics crates quiet.
fn init_logging() {
    let filter = EnvFilter::try_from_env("MARSA_LOG")
        .unwrap_or_else(|_| EnvFilter::new("warn,marsa_gui=info,marsa_core=info"));
    // Fails only if a logger is already set, which cannot happen here.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
