// No console window behind the window on Windows. Debug builds keep the console
// so logs stay visible during development (architecture.md section 9).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! `marsa-gui`: the window. A client of the daemon, like the CLI; it never
//! changes state by itself (task M3-10).
//!
//! `marsa-gui [--data-dir <PATH>]`, see [`startup`]. Only one window runs per
//! data folder: a second one exits at once (task M5-04).

use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::Context as _;
use eframe::egui;
use marsa_gui::{app, bridge, startup, strings, theme};

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
    let bridge = start_bridge(&data_dir)?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(strings::APP_NAME)
            .with_inner_size(window_size())
            .with_min_inner_size(theme::SIZE.window_min),
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

/// Starts the bridge to the daemon.
fn start_bridge(data_dir: &Path) -> anyhow::Result<bridge::Handle> {
    // Debug builds only: the design review's demo draws fixed data and
    // starts no daemon (see `demo`).
    #[cfg(debug_assertions)]
    if marsa_gui::demo::requested().is_some() {
        return Ok(bridge::Handle::detached());
    }
    bridge::start(data_dir.to_path_buf()).context("cannot start the bridge thread")
}

/// The size the window opens at.
fn window_size() -> egui::Vec2 {
    // Debug builds only: `MARSA_GUI_SIZE` for the design-review pictures
    // (see `screenshot`).
    #[cfg(debug_assertions)]
    if let Some(size) = marsa_gui::screenshot::size() {
        return size;
    }
    theme::SIZE.window
}
