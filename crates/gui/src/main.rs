// No console window behind the window on Windows. Debug builds keep the console
// so logs stay visible during development (architecture.md section 9).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! `marsa-gui`: the window. A client of the daemon, like the CLI; it never
//! changes state by itself (task M3-10).
//!
//! `marsa-gui [--data-dir <PATH>] [--panel]`, see [`startup`]. Only one window
//! runs per data folder: a second one exits at once (task M5-04). `--panel` is
//! the menu-bar panel instead (ADR 0022): a borderless window that the tray
//! starts under its icon, and that ends with an exit code that tells the tray
//! what the user asked for (`marsa_gui::panel::PanelExit`).

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

use anyhow::Context as _;
use eframe::egui;
use marsa_gui::panel::{self, PanelExit};
use marsa_gui::prefs::Prefs;
use marsa_gui::{app, bridge, startup, strings, theme};

fn main() -> ExitCode {
    let started = Instant::now();
    startup::init_logging();
    match run(started) {
        Ok(code) => code,
        Err(err) => {
            tracing::error!(error = format!("{err:#}"), "marsa-gui failed");
            ExitCode::FAILURE
        }
    }
}

fn run(started: Instant) -> anyhow::Result<ExitCode> {
    let launch = startup::launch(std::env::args_os().skip(1))?;
    if launch.panel {
        return run_panel(launch.data_dir, launch.anchor, started);
    }
    let data_dir = launch.data_dir;
    // `MARSA_GUI_MEASURE` fills the window with made-up data and starts no
    // daemon. It is read before the lock is taken: it needs a data folder
    // that is missing or empty, so it never touches one Marsa used.
    // Only a build with the `measure` feature reads it: a shipped binary has
    // no such mode.
    #[cfg(feature = "measure")]
    let measure = marsa_gui::measure::requested(&data_dir)?;
    #[cfg(feature = "measure")]
    let measuring = measure.is_some();
    #[cfg(not(feature = "measure"))]
    let measuring = false;
    // Held until the window closes. The tray's "Open Marsa" starts a second
    // `marsa-gui` while this one runs: it finds the lock taken and exits.
    let Some(_lock) = startup::window_lock(&data_dir).context("cannot take the window lock")?
    else {
        tracing::info!("the window is already open; this one exits");
        return Ok(ExitCode::SUCCESS);
    };
    // Connecting starts now, while the window opens.
    let bridge = start_bridge(&data_dir, measuring)?;
    // Read once: the appearance, and the size the window had when it closed.
    let prefs = Prefs::load(&Prefs::path(&data_dir));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(strings::APP_NAME)
            .with_inner_size(window_size(&prefs))
            .with_min_inner_size(theme::SIZE.window_min),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        strings::APP_NAME,
        options,
        Box::new(move |creation| {
            let app = app::App::new(&creation.egui_ctx, bridge, data_dir, prefs, started);
            // Only with the `measure` feature (see above).
            #[cfg(feature = "measure")]
            let app = match measure {
                Some(plan) => app.measuring(plan),
                None => app,
            };
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyhow::anyhow!("cannot open the window: {err}"))?;
    Ok(ExitCode::SUCCESS)
}

/// The menu-bar panel: no window lock (it runs beside the window), a bridge
/// that never starts the daemon, and an exit code that says why it closed.
fn run_panel(
    data_dir: std::path::PathBuf,
    anchor: Option<panel::place::Anchor>,
    started: Instant,
) -> anyhow::Result<ExitCode> {
    // Debug builds only: the design review's demo draws fixed data and
    // starts no daemon (see `demo`).
    #[cfg(debug_assertions)]
    let bridge = if marsa_gui::demo::requested().is_some() {
        bridge::Handle::detached()
    } else {
        bridge::attach(data_dir.clone()).context("cannot start the bridge thread")?
    };
    #[cfg(not(debug_assertions))]
    let bridge = bridge::attach(data_dir.clone()).context("cannot start the bridge thread")?;
    let exit = Arc::new(AtomicU8::new(PanelExit::Closed.code()));
    let report = Arc::clone(&exit);
    eframe::run_native(
        strings::PANEL_NAME,
        panel::app::options(panel::app::start_size(), anchor.as_ref()),
        Box::new(move |creation| {
            Ok(Box::new(panel::app::PanelApp::new(
                &creation.egui_ctx,
                bridge,
                data_dir,
                started,
                report,
                anchor,
            )))
        }),
    )
    .map_err(|err| anyhow::anyhow!("cannot open the panel: {err}"))?;
    Ok(ExitCode::from(exit.load(Ordering::Relaxed)))
}

/// Starts the bridge to the daemon. `measuring`: there is no daemon to
/// reach, and no bridge.
fn start_bridge(data_dir: &Path, measuring: bool) -> anyhow::Result<bridge::Handle> {
    // Only a build with the `measure` feature has a detached bridge to give.
    #[cfg(feature = "measure")]
    if measuring {
        return Ok(bridge::Handle::detached());
    }
    #[cfg(not(feature = "measure"))]
    let _ = measuring;
    // Debug builds only: the design review's demo draws fixed data and
    // starts no daemon (see `demo`).
    #[cfg(debug_assertions)]
    if marsa_gui::demo::requested().is_some() {
        return Ok(bridge::Handle::detached());
    }
    bridge::start(data_dir.to_path_buf()).context("cannot start the bridge thread")
}

/// The size the window opens at: the one it closed with, else the default.
fn window_size(prefs: &Prefs) -> egui::Vec2 {
    // Debug builds only: `MARSA_GUI_SIZE` for the design-review pictures
    // (see `screenshot`).
    #[cfg(debug_assertions)]
    if let Some(size) = marsa_gui::screenshot::size() {
        return size;
    }
    prefs.window_size()
}
