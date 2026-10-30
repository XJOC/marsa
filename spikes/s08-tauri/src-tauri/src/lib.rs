//! S-08 spike app: a minimal Tauri window that answers the packaging
//! questions in task S-08a. Two commands only: `diagnostics` and
//! `run_sidecar`. Field names stay snake_case; the UI types mirror them.

use serde::Serialize;
use tauri::Manager;

#[derive(Serialize)]
pub struct Diagnostics {
    pub os: &'static str,
    pub arch: &'static str,
    pub webview_version: String,
    pub current_exe: String,
    pub app_data_dir: String,
}

#[tauri::command]
fn diagnostics(app: tauri::AppHandle) -> Result<Diagnostics, String> {
    let webview_version = tauri::webview_version().map_err(|e| e.to_string())?;
    let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // The app data dir is chosen by the OS (`directories` in the product).
    // It only fails when the OS gives us no home directory at all.
    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir is not available: {e}"))?;
    Ok(Diagnostics {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        webview_version,
        current_exe: current_exe.display().to_string(),
        app_data_dir: app_data_dir.display().to_string(),
    })
}

#[derive(Serialize)]
pub struct SidecarOutput {
    pub exit_code: i32,
    pub stdout: String,
}

// `CREATE_NO_WINDOW` on Windows, nothing anywhere else: platform selection
// happens at compile time, unlike PHP's runtime `PHP_OS_FAMILY` checks.
#[cfg(windows)]
fn configure(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure(_command: &mut std::process::Command) {}

#[tauri::command]
fn run_sidecar() -> Result<SidecarOutput, String> {
    let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = current_exe
        .parent()
        .ok_or_else(|| format!("no parent directory for {}", current_exe.display()))?;
    // Absolute path next to the running app: never `PATH`, never a shell
    // string. This is how the product will run `marsad` from `platform`.
    let sidecar = dir.join(if cfg!(windows) {
        "s08-sidecar.exe"
    } else {
        "s08-sidecar"
    });
    let mut command = std::process::Command::new(&sidecar);
    configure(&mut command);
    let output = command
        .output()
        .map_err(|e| format!("failed to run {}: {e}", sidecar.display()))?;
    Ok(SidecarOutput {
        // `code()` is `None` only when the process was killed by a signal.
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
    })
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![diagnostics, run_sidecar])
        .run(tauri::generate_context!())
        .expect("the window comes from tauri.conf.json, so building it cannot fail here")
}
