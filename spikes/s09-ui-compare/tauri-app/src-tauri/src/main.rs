use std::process::ExitCode;

fn main() -> ExitCode {
    // First line of `main()`: `PHASE first-frame` counts from here.
    s09_common::bench::start_clock();
    match tauri_app::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tauri-app: {error}");
            ExitCode::FAILURE
        }
    }
}
