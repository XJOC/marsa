// Prevents an extra console window on Windows in release builds. In debug
// builds the console stays, which makes panic messages visible while developing.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    s08_app::run()
}
