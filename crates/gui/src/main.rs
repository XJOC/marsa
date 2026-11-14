// No console window behind the window on Windows. Debug builds keep the console
// so logs stay visible during development (architecture.md section 9).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! `marsa-gui`: the window. A client of the daemon, like the CLI; it never
//! changes state by itself (task M3-10).
//!
//! `marsa-gui [--data-dir <PATH>]`, see [`startup`]. Only one window runs per
//! data folder: a second one exits at once (task M5-04).

use std::process::ExitCode;
use std::time::Instant;

use anyhow::Context as _;
use eframe::egui;
use marsa_gui::{app, bridge, startup, strings};

fn main() -> ExitCode {
    let started = Instant::now();
    startup::init_logging();
    match run(started) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = format!("{err:#}"), "marsa-gui failed");
            ExitCode::FAILURE
        }
    }
}

fn run(started: Instant) -> anyhow::Result<()> {
    let data_dir = startup::data_dir(std::env::args_os().skip(1))?;
    // Held until the window closes. The tray's "Open Marsa" starts a second
    // `marsa-gui` while this one runs: it finds the lock taken and exits.
    let Some(_lock) = startup::window_lock(&data_dir).context("cannot take the window lock")?
    else {
        tracing::info!("the window is already open; this one exits");
        return Ok(());
    };
    // Connecting starts now, while the window opens.
    let bridge = bridge::start(data_dir.clone()).context("cannot start the bridge thread")?;
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
        Box::new(move |creation| {
            let app = app::App::new(&creation.egui_ctx, bridge, data_dir, started);
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyhow::anyhow!("cannot open the window: {err}"))
}
