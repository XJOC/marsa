// No console window on Windows: the icon is what starts at login. Debug
// builds keep the console so logs stay visible during development.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! `marsa-tray`: the menu-bar icon (task M5-04). A client of the daemon, like
//! the window; it starts `marsad` and stays after the window closes. It is
//! what starts at login (task M5-03).
//!
//! `marsa-tray [--data-dir <PATH>]`, see `marsa_gui::startup`.

use std::process::ExitCode;

use marsa_gui::{startup, tray};

fn main() -> ExitCode {
    startup::init_logging();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = format!("{err:#}"), "marsa-tray failed");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let data_dir = startup::data_dir(std::env::args_os().skip(1))?;
    tray::run(data_dir)?;
    Ok(())
}
