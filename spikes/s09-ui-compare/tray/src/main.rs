//! S-09 menu-bar icon (`tray-icon` + `tao`). The menu starts each UI program
//! as a separate process; the process ends when its window closes.
//!
//! The UI programs are found next to this executable (`target/release/` after
//! `measure.sh` or the README build commands):
//! - Tauri: `bundle/macos/tauri-app.app/Contents/MacOS/tauri-app` when the
//!   `.app` exists (built by `tauri build`), otherwise `tauri-app`.
//! - egui: `egui-app`, started with `--renderer glow` or `--renderer wgpu`.
//!
//! `tray --bench` prints `PHASE ready`, `idle-start`, `idle`, and `done`, then
//! exits: case A of `measure.sh` (icon only, no window).

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

use s09_common::bench;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

mod strings {
    //! Every user-visible string of the tray, in one place (AGENTS.md section 9).
    pub const TOOLTIP: &str = "S-09 UI compare";
    pub const OPEN_TAURI: &str = "Open Tauri";
    pub const OPEN_EGUI_GLOW: &str = "Open egui (glow)";
    pub const OPEN_EGUI_WGPU: &str = "Open egui (wgpu)";
    pub const QUIT: &str = "Quit";
    pub const USAGE: &str = "usage: tray [--bench]";
}

/// Icon side in pixels. macOS draws a template icon from its alpha channel.
const ICON_SIZE: u32 = 32;

enum UserEvent {
    Menu(MenuEvent),
}

/// Absolute paths of the UI programs. `marsad` will never rely on `PATH`
/// either (AGENTS.md section 6), so the spike does not.
struct Programs {
    tauri: PathBuf,
    egui: PathBuf,
}

impl Programs {
    fn locate() -> Result<Self, String> {
        let exe = std::env::current_exe()
            .map_err(|error| format!("cannot find the tray executable: {error}"))?;
        let dir = exe
            .parent()
            .ok_or_else(|| format!("no parent directory for {}", exe.display()))?;
        let bundled = dir
            .join("bundle")
            .join("macos")
            .join("tauri-app.app")
            .join("Contents")
            .join("MacOS")
            .join("tauri-app");
        let tauri = if bundled.is_file() {
            bundled
        } else {
            dir.join("tauri-app")
        };
        Ok(Self {
            tauri,
            egui: dir.join("egui-app"),
        })
    }
}

/// Starts `program` as a separate process and reaps it on a helper thread
/// when it exits, so no zombie process is left behind.
fn launch(program: &Path, args: &[&str]) {
    match Command::new(program).args(args).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                // The exit status is not used: the window was closed, which is
                // the normal way for a UI program to end.
                let _ = child.wait();
            });
        }
        Err(error) => eprintln!("tray: failed to start {}: {error}", program.display()),
    }
}

/// A ring with a dot, drawn in code so the spike ships no image file.
fn icon() -> Result<Icon, String> {
    let size = ICON_SIZE as usize;
    let center = (ICON_SIZE as f32 - 1.0) / 2.0;
    let mut rgba = vec![0_u8; size * size * 4];
    for (index, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = (index % size) as f32 - center;
        let y = (index / size) as f32 - center;
        let distance = (x * x + y * y).sqrt();
        let inside = (11.0..=14.0).contains(&distance) || distance <= 5.0;
        if inside {
            *pixel = [0, 0, 0, 255];
        }
    }
    Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE).map_err(|error| format!("bad icon: {error}"))
}

struct MenuIds {
    tauri: MenuId,
    glow: MenuId,
    wgpu: MenuId,
    quit: MenuId,
}

fn build_tray() -> Result<(TrayIcon, MenuIds), String> {
    let open_tauri = MenuItem::new(strings::OPEN_TAURI, true, None);
    let open_glow = MenuItem::new(strings::OPEN_EGUI_GLOW, true, None);
    let open_wgpu = MenuItem::new(strings::OPEN_EGUI_WGPU, true, None);
    let quit = MenuItem::new(strings::QUIT, true, None);
    let menu = Menu::new();
    menu.append_items(&[
        &open_tauri,
        &open_glow,
        &open_wgpu,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .map_err(|error| format!("cannot build the menu: {error}"))?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(strings::TOOLTIP)
        // Template image: macOS recolours it for a light or dark menu bar.
        // The method exists only on macOS; this spike is macOS-only.
        .with_icon_templated(icon()?)
        .build()
        .map_err(|error| format!("cannot create the tray icon: {error}"))?;
    let ids = MenuIds {
        tauri: open_tauri.id().clone(),
        glow: open_glow.id().clone(),
        wgpu: open_wgpu.id().clone(),
        quit: quit.id().clone(),
    };
    Ok((tray, ids))
}

/// The `--bench` walk: ready, pause, idle, pause, done.
enum BenchStep {
    AfterReady,
    Idle,
    AfterIdle,
}

fn main() -> ExitCode {
    bench::start_clock();
    let mut bench_mode = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--bench" => bench_mode = true,
            other => {
                eprintln!("tray: unknown argument: {other}\n{}", strings::USAGE);
                return ExitCode::from(2);
            }
        }
    }
    let programs = match Programs::locate() {
        Ok(programs) => programs,
        Err(error) => {
            eprintln!("tray: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    // macOS only: a menu-bar app has no Dock icon and no app menu. This spike
    // is macOS-only (task S-09), and the API exists only there.
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }

    // Menu clicks arrive on another callback; the proxy wakes the event loop.
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        // Fails only when the event loop is gone, that is while quitting.
        let _ = proxy.send_event(UserEvent::Menu(event));
    }));

    // Kept alive here: dropping the `TrayIcon` removes it from the menu bar.
    let mut tray: Option<(TrayIcon, MenuIds)> = None;
    let mut bench_step: Option<(BenchStep, Instant)> = None;

    event_loop.run(move |event, _target, control_flow| {
        match event {
            // tray-icon on macOS: create the icon once the loop is running.
            Event::NewEvents(StartCause::Init) => match build_tray() {
                Ok(built) => {
                    tray = Some(built);
                    if bench_mode {
                        bench::phase("ready", &[("ms", bench::ms(bench::ms_since_start()))]);
                        bench_step =
                            Some((BenchStep::AfterReady, Instant::now() + bench::PHASE_PAUSE));
                    }
                }
                Err(error) => {
                    eprintln!("tray: {error}");
                    *control_flow = ControlFlow::ExitWithCode(1);
                    return;
                }
            },
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                let now = Instant::now();
                bench_step = match bench_step.take() {
                    Some((BenchStep::AfterReady, _)) => {
                        bench::phase("idle-start", &[]);
                        Some((BenchStep::Idle, now + bench::IDLE_DURATION))
                    }
                    Some((BenchStep::Idle, _)) => {
                        bench::phase(
                            "idle",
                            &[("secs", format!("{:.1}", bench::IDLE_DURATION.as_secs_f64()))],
                        );
                        Some((BenchStep::AfterIdle, now + bench::PHASE_PAUSE))
                    }
                    Some((BenchStep::AfterIdle, _)) => {
                        bench::phase("done", &[]);
                        *control_flow = ControlFlow::Exit;
                        return;
                    }
                    None => None,
                };
            }
            Event::UserEvent(UserEvent::Menu(event)) => {
                if let Some((_, ids)) = &tray {
                    if event.id == ids.tauri {
                        launch(&programs.tauri, &[]);
                    } else if event.id == ids.glow {
                        launch(&programs.egui, &["--renderer", "glow"]);
                    } else if event.id == ids.wgpu {
                        launch(&programs.egui, &["--renderer", "wgpu"]);
                    } else if event.id == ids.quit {
                        *control_flow = ControlFlow::Exit;
                        return;
                    }
                }
            }
            _ => {}
        }
        // Sleep until the next event, or until the next bench step is due.
        *control_flow = match &bench_step {
            Some((_, deadline)) => ControlFlow::WaitUntil(*deadline),
            None => ControlFlow::Wait,
        };
    });
}
