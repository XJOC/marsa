// No console window when Windows starts the daemon at login. Debug builds keep
// the console so logs stay visible during development (architecture.md section 9).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! `marsad`: the daemon. It is the only process that changes state; the CLI and
//! the window are its clients.

use clap::Command;

fn main() {
    Command::new("marsad")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Marsa daemon")
        .get_matches();
}
