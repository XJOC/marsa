//! S-09 Tauri window: the Table, SQL, and Logs tabs in React.
//!
//! `tauri-app [--bench] [--with-tray]`.
//! - The data stays in Rust. The UI asks for the rows it shows, in batches of
//!   500 (`table_rows`, `log_lines`), like a real database browser.
//! - `--bench`: the UI walks the phases (`src/bench.ts`); Rust prints the
//!   `PHASE` lines with the shared code in `s09-common`.
//! - `--with-tray`: a tray icon lives in this process. Closing the window
//!   destroys it (the Tauri default), and the process stays alive with the
//!   tray, so case E measures what remains in memory.

use s09_common::bench::{self, FrameStats};
use s09_common::data;
use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder};

mod strings {
    //! Every user-visible string on the Rust side, in one place (AGENTS.md
    //! section 9). The UI strings are in `src/strings.ts`.
    pub const WINDOW_TITLE: &str = "S-09 Tauri";
    pub const TRAY_TOOLTIP: &str = "S-09 Tauri (with tray)";
    pub const TRAY_TITLE: &str = "S-09";
    pub const MENU_OPEN: &str = "Open window";
    pub const MENU_QUIT: &str = "Quit";
}

/// Label of the only window. `capabilities/default.json` uses it too.
const WINDOW_LABEL: &str = "main";
/// Same logical size as the egui window.
const WINDOW_SIZE: (f64, f64) = (1100.0, 760.0);
const MENU_OPEN_ID: &str = "open";
const MENU_QUIT_ID: &str = "quit";

/// Command-line options. Unknown arguments are ignored: macOS may add its
/// own when the app starts from Finder.
#[derive(Clone, Copy)]
struct Options {
    bench: bool,
    with_tray: bool,
}

impl Options {
    fn from_args() -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        Self {
            bench: args.iter().any(|arg| arg == "--bench"),
            with_tray: args.iter().any(|arg| arg == "--with-tray"),
        }
    }
}

/// What the UI needs to know. The bench timings come from `s09-common`, so
/// egui and Tauri use the same numbers.
#[derive(Serialize)]
struct AppConfig {
    bench: bool,
    with_tray: bool,
    pause_ms: u128,
    scroll_ms: u128,
    table_rows: usize,
    table_columns: [&'static str; 8],
    log_lines: usize,
    batch_size: usize,
}

#[tauri::command]
fn app_config(options: State<'_, Options>) -> AppConfig {
    AppConfig {
        bench: options.bench,
        with_tray: options.with_tray,
        pause_ms: bench::PHASE_PAUSE.as_millis(),
        scroll_ms: bench::SCROLL_DURATION.as_millis(),
        table_rows: data::TABLE_ROWS,
        table_columns: data::TABLE_COLUMNS,
        log_lines: data::LOG_LINES,
        batch_size: data::BATCH_SIZE,
    }
}

// The data commands are `async` so they run on the async runtime, not on the
// main thread that also drives the window (Tauri runs sync commands there).

/// Up to `BATCH_SIZE` table rows from `start`.
#[tauri::command]
async fn table_rows(start: usize, count: usize) -> Vec<[String; 8]> {
    data::table_rows(start, count.min(data::BATCH_SIZE))
}

/// Up to `BATCH_SIZE` log lines from `start`.
#[tauri::command]
async fn log_lines(start: usize, count: usize) -> Vec<String> {
    data::log_lines(start, count.min(data::BATCH_SIZE))
}

/// The 5,000 lines of the SQL tab.
#[tauri::command]
async fn sql_text() -> String {
    data::sql_text()
}

/// Errors from the UI go to stderr: a release webview has no visible console.
#[tauri::command]
fn ui_error(message: String) {
    eprintln!("tauri-app UI: {message}");
}

/// Prints one `PHASE` line for the UI. Only the names below are accepted.
/// - `first-frame`: milliseconds from the start of `main()` to now, the
///   moment the UI reports that the first rows were painted.
/// - `table-scrolled`, `logs-scrolled`: `intervals` are `requestAnimationFrame`
///   deltas in ms; `placeholder_frames` counts frames that showed rows still
///   waiting for their batch.
/// - `sql-loaded`: `ms` from opening the tab to the editor being painted.
#[tauri::command]
fn bench_phase(
    name: String,
    intervals: Option<Vec<f64>>,
    ms: Option<f64>,
    placeholder_frames: Option<u32>,
) -> Result<(), String> {
    match name.as_str() {
        "first-frame" => bench::phase(&name, &[("ms", bench::ms(bench::ms_since_start()))]),
        "table-scrolled" | "logs-scrolled" => {
            let intervals = intervals.unwrap_or_default();
            let mut fields: Vec<(&str, String)> = Vec::with_capacity(5);
            match FrameStats::from_intervals(&intervals) {
                Some(stats) => fields.extend(stats.fields()),
                None => fields.push(("frames", "0".to_string())),
            }
            fields.push((
                "placeholder_frames",
                placeholder_frames.unwrap_or(0).to_string(),
            ));
            bench::phase(&name, &fields);
        }
        "sql-loaded" => bench::phase(
            &name,
            &[
                ("ms", bench::ms(ms.unwrap_or(f64::NAN))),
                ("lines", data::SQL_LINES.to_string()),
            ],
        ),
        other => return Err(format!("unknown bench phase: {other}")),
    }
    Ok(())
}

/// The idle phase. The timer runs in Rust (a JS timer may be throttled), on
/// a blocking-pool thread so no runtime worker sleeps.
#[tauri::command]
async fn bench_idle() -> Result<(), String> {
    bench::phase("idle-start", &[]);
    tauri::async_runtime::spawn_blocking(|| std::thread::sleep(bench::IDLE_DURATION))
        .await
        .map_err(|error| format!("idle timer failed: {error}"))?;
    bench::phase(
        "idle",
        &[("secs", format!("{:.1}", bench::IDLE_DURATION.as_secs_f64()))],
    );
    Ok(())
}

/// Ends the bench. With `--with-tray`: destroy the window, let WebKit settle,
/// print `PHASE window-closed`, wait for the sample, then exit. The work runs
/// on its own thread so this command can return to the window first.
#[tauri::command]
fn bench_finish(app: AppHandle, options: State<'_, Options>) {
    let with_tray = options.with_tray;
    std::thread::spawn(move || {
        if with_tray {
            if let Some(window) = app.get_webview_window(WINDOW_LABEL)
                && let Err(error) = window.destroy()
            {
                eprintln!("tauri-app: cannot destroy the window: {error}");
            }
            std::thread::sleep(bench::SETTLE_AFTER_CLOSE);
            bench::phase(
                "window-closed",
                &[(
                    "settle_secs",
                    format!("{:.1}", bench::SETTLE_AFTER_CLOSE.as_secs_f64()),
                )],
            );
            std::thread::sleep(bench::PHASE_PAUSE);
        }
        bench::phase("done", &[]);
        app.exit(0);
    });
}

/// Opens the window, or focuses it when it is already open.
///
/// In `--bench` mode the window floats above all others: WebKit pauses
/// `requestAnimationFrame` while another window covers it, and a script
/// cannot always bring an app to the front on macOS. egui does the same.
fn open_window(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        return window.set_focus();
    }
    let bench = app.state::<Options>().bench;
    WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::App("index.html".into()))
        .title(strings::WINDOW_TITLE)
        .inner_size(WINDOW_SIZE.0, WINDOW_SIZE.1)
        .always_on_top(bench)
        .focused(true)
        .build()?;
    Ok(())
}

/// The `--with-tray` icon: reopen the window, or quit.
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, MENU_OPEN_ID, strings::MENU_OPEN, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT_ID, strings::MENU_QUIT, true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .tooltip(strings::TRAY_TOOLTIP)
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_OPEN_ID => {
                if let Err(error) = open_window(app) {
                    eprintln!("tauri-app: cannot open the window: {error}");
                }
            }
            MENU_QUIT_ID => app.exit(0),
            _ => {}
        });
    builder = match app.default_window_icon() {
        Some(icon) => builder.icon(icon.clone()),
        None => builder.title(strings::TRAY_TITLE),
    };
    builder.build(app)?;
    Ok(())
}

pub fn run() -> tauri::Result<()> {
    let options = Options::from_args();
    let app = tauri::Builder::default()
        .manage(options)
        .invoke_handler(tauri::generate_handler![
            app_config,
            table_rows,
            log_lines,
            sql_text,
            ui_error,
            bench_phase,
            bench_idle,
            bench_finish
        ])
        .setup(move |app| {
            if options.with_tray {
                build_tray(app.handle())?;
            }
            open_window(app.handle())?;
            Ok(())
        })
        .build(tauri::generate_context!())?;
    app.run(move |_app, event| {
        // `code: None` means "the last window closed". With the tray the
        // process stays; `app.exit(0)` (Quit, bench end) has a code and exits.
        if let RunEvent::ExitRequested {
            code: None, api, ..
        } = event
            && options.with_tray
        {
            api.prevent_exit();
        }
    });
    Ok(())
}
