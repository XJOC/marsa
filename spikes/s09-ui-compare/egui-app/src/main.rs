//! S-09 egui window: the Table, SQL, and Logs tabs drawn with eframe.
//!
//! `egui-app --renderer glow|wgpu [--bench]`. With `--bench` the program walks
//! the phases by itself, prints one `PHASE` line per phase, and exits.

mod bench_run;
mod strings;
mod view;

use std::process::ExitCode;

use eframe::egui;
use s09_common::bench;

use crate::bench_run::BenchRun;
use crate::view::View;

/// Command-line options.
struct Args {
    renderer: eframe::Renderer,
    renderer_name: &'static str,
    bench: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut renderer = None;
    let mut bench = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = match arg.as_str() {
            "--bench" => {
                bench = true;
                continue;
            }
            "--renderer" => args
                .next()
                .ok_or_else(|| "--renderer needs a value".to_string())?,
            other => match other.strip_prefix("--renderer=") {
                Some(value) => value.to_string(),
                None => return Err(format!("unknown argument: {other}")),
            },
        };
        renderer = Some(match value.as_str() {
            "glow" => (eframe::Renderer::Glow, "glow"),
            "wgpu" => (eframe::Renderer::Wgpu, "wgpu"),
            other => return Err(format!("unknown renderer: {other}")),
        });
    }
    // No `--renderer`: glow, the lighter of the two.
    let (renderer, renderer_name) = renderer.unwrap_or((eframe::Renderer::Glow, "glow"));
    Ok(Args {
        renderer,
        renderer_name,
        bench,
    })
}

struct App {
    view: View,
    bench: Option<BenchRun>,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // egui may run more than one layout pass per frame (`request_discard`).
        // The bench counts frames, so it only steps on the first pass.
        if let Some(bench) = &mut self.bench
            && ui.ctx().current_pass_index() == 0
        {
            bench.step(ui.ctx(), frame.info().cpu_usage, &mut self.view);
        }
        self.view.ui(ui);
    }
}

fn main() -> ExitCode {
    bench::start_clock();
    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("egui-app: {error}\n{}", strings::USAGE);
            return ExitCode::from(2);
        }
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(format!(
            "{} ({})",
            strings::WINDOW_TITLE,
            args.renderer_name
        ))
        .with_inner_size(strings::WINDOW_SIZE)
        .with_active(true);
    if args.bench {
        // eframe stops painting while another window covers this one, so in
        // `--bench` mode the window floats above all others. Tauri does the
        // same (WebKit pauses `requestAnimationFrame` when covered).
        viewport = viewport.with_always_on_top();
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: args.renderer,
        ..Default::default()
    };
    let renderer_name = args.renderer_name;
    let bench = args.bench.then(BenchRun::new);
    let result = eframe::run_native(
        strings::APP_ID,
        options,
        Box::new(move |_creation| {
            Ok(Box::new(App {
                view: View::new(renderer_name),
                bench,
            }))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("egui-app: {error}");
            ExitCode::FAILURE
        }
    }
}
