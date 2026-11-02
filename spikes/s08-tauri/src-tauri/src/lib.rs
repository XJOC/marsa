//! S-08 spike app: a minimal Tauri window that answers the packaging
//! questions in tasks S-08a and S-08b. Three commands: `diagnostics`,
//! `run_sidecar`, and `download_and_run`. Field names stay snake_case; the UI
//! types mirror them.

use std::path::Path;

use serde::Serialize;
use tauri::Manager;

/// The pre-release the app downloads its sidecar from. A constant on purpose:
/// the URL is never received from the UI (task S-08b, section 3).
const RELEASE_BASE: &str = "https://github.com/XJOC/marsa/releases/download/spike-s08";

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

/// Result of downloading, extracting, and running the published sidecar.
/// `quarantine` is `None` everywhere except macOS, and on macOS when the
/// attribute is absent: the answer task S-08b question 2 asks for.
#[derive(Serialize)]
pub struct DownloadRunOutput {
    pub path: String,
    pub exit_code: i32,
    pub stdout: String,
    pub quarantine: Option<String>,
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

// The release publishes one archive per target triple. The triple is picked at
// run time from `std::env::consts`, not with `#[cfg]`: a system with no
// archive must get a clear error instead of failing to compile.
fn target_triple() -> Result<&'static str, String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        (os, arch) => Err(format!("no sidecar archive for {os}/{arch}")),
    }
}

// macOS only: `xattr` and the quarantine attribute exist on no other system,
// so the call is compiled out there and those systems get `None`. The `#[cfg]`
// is here because the program path is macOS-specific, not because the caller
// differs.
#[cfg(target_os = "macos")]
fn read_quarantine(path: &Path) -> Result<Option<String>, String> {
    // `xattr -p` exits non-zero when the attribute is absent. Absence is the
    // answer we want for a file the app downloaded itself, so it is `None`,
    // not an error. Only failing to start `/usr/bin/xattr` is an error.
    let output = std::process::Command::new("/usr/bin/xattr")
        .args(["-p", "com.apple.quarantine"])
        .arg(path)
        .output()
        .map_err(|error| format!("failed to run /usr/bin/xattr: {error}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!value.is_empty()).then_some(value))
}

#[cfg(not(target_os = "macos"))]
fn read_quarantine(_path: &Path) -> Result<Option<String>, String> {
    Ok(None)
}

/// Downloads `s08-sidecar-<triple>.tar.gz` from the pre-release, extracts it
/// inside the app data dir, runs it, and reports the quarantine attribute.
/// The URL is built here from a constant, never received from the UI.
#[tauri::command]
async fn download_and_run(app: tauri::AppHandle) -> Result<DownloadRunOutput, String> {
    let triple = target_triple()?;
    let url = format!("{RELEASE_BASE}/s08-sidecar-{triple}.tar.gz");
    // `reqwest` mirrors Laravel's `Http::get()`, but the wait is explicit:
    // every call is `.await`, and `?` turns a failure into our error string.
    let response = reqwest::get(&url)
        .await
        .map_err(|error| format!("failed to download {url}: {error}"))?
        .error_for_status()
        .map_err(|error| format!("download rejected for {url}: {error}"))?;
    let archive = response
        .bytes()
        .await
        .map_err(|error| format!("failed to read {url}: {error}"))?;

    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("app data dir is not available: {error}"))?;

    // Unpacking and spawning block the thread. They must not run on the async
    // runtime, so they move to the blocking pool and the command awaits it.
    tauri::async_runtime::spawn_blocking(move || extract_and_run(&data_dir, &archive))
        .await
        .map_err(|error| format!("the sidecar task did not finish: {error}"))?
}

fn extract_and_run(data_dir: &Path, archive: &[u8]) -> Result<DownloadRunOutput, String> {
    // Downloads stay inside the app data dir, never the system temp dir:
    // moving across volumes fails on every OS (architecture.md section 10).
    let dir = data_dir.join("downloads");
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create {}: {error}", dir.display()))?;

    // `GzDecoder` wraps the reader and `tar::Archive` wraps the decoder, the
    // way PHP stacks a `compress.zlib://` filter on a stream.
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    tar.unpack(&dir)
        .map_err(|error| format!("failed to unpack the sidecar archive: {error}"))?;

    let binary = dir.join(if cfg!(windows) {
        "s08-sidecar.exe"
    } else {
        "s08-sidecar"
    });
    let mut command = std::process::Command::new(&binary);
    configure(&mut command);
    let output = command
        .output()
        .map_err(|error| format!("failed to run {}: {error}", binary.display()))?;

    Ok(DownloadRunOutput {
        path: binary.display().to_string(),
        // `code()` is `None` only when the process was killed by a signal.
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        quarantine: read_quarantine(&binary)?,
    })
}

// `run` fails for real reasons (no WebView2 on Windows, no display on Linux),
// so the `expect` below is not an invariant. In M3 it becomes a returned error,
// because a panic is invisible on Windows with `windows_subsystem = "windows"`.
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            diagnostics,
            run_sidecar,
            download_and_run
        ])
        .run(tauri::generate_context!())
        .expect("failed to start the Tauri app")
}
