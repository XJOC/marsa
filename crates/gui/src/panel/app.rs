//! The panel's window loop (ADR 0022): an eframe app like the window's, with
//! a borderless transparent viewport that sizes itself to what the panel
//! says, closes on focus loss and on Esc, and ends with the exit code that
//! tells the tray what the user asked for.
//!
//! Each frame, `logic` applies the bridge's updates, and `ui` draws
//! `screens::panel` and applies what it returns. The
//! panel opens in three steps, so it never shows a small panel that jumps to
//! its size: it waits for the daemon's answer (a short time at most), then
//! asks the system for the window's size, and draws once the window has it.
//! Until then the window is transparent and empty.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{self, Pos2, UiBuilder, Vec2, vec2};

use super::PanelExit;
use super::place::Anchor;
use crate::app::{open_site, take_updates};
use crate::bridge;
use crate::model::{Connection, Model};
use crate::prefs::Prefs;
use crate::screens::panel::{self as screen, PanelAction};
use crate::strings;
use crate::theme::{self, Theme};

/// How long the panel waits for the daemon's lists before it draws what it
/// has: "Connecting…" is better than nothing when the daemon is slow.
const READY_LIMIT: Duration = Duration::from_millis(500);
/// How long the window may take to have the size that was asked for before it
/// is drawn anyway.
const FIT_LIMIT: Duration = Duration::from_millis(400);
/// A window that has not had the focus by now is not going to (the system
/// refused it), and a panel that nothing can close is closed at once.
const FOCUS_LIMIT: Duration = Duration::from_secs(3);
/// A size that differs by less than a point is the same: the system rounds.
const SAME_SIZE: f32 = theme::UNIT;
/// How often the window is asked for the same place before the panel takes
/// it that the system keeps it elsewhere (a window may not cover the menu bar,
/// or the system moves it) and draws where it is.
const MOST_MOVES: u8 = 4;

/// The options of the panel's window: borderless and transparent, the corners
/// are the panel's own; always on top; no entry in the taskbar. The system's
/// shadow follows the shape on macOS. With an `anchor`, the window is created
/// under the icon, in points of the icon's monitor; without, the system
/// places it.
pub fn options(start: Vec2, anchor: Option<&Anchor>) -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(strings::PANEL_NAME)
        .with_decorations(false)
        .with_transparent(true)
        .with_resizable(false)
        .with_has_shadow(true)
        .with_inner_size(start)
        .with_window_level(egui::WindowLevel::AlwaysOnTop)
        .with_taskbar(false)
        .with_active(true);
    if let Some(anchor) = anchor {
        let scale = anchor.scale;
        let at = anchor.place(
            start * scale,
            theme::SIZE.panel_gap * scale,
            theme::SIZE.panel_edge * scale,
        );
        viewport = viewport.with_position((at.to_vec2() / scale).to_pos2());
    }
    // `mut` is used on macOS only (the hook below).
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    // macOS only, so the `cfg` is not in `platform`: the activation policy
    // exists in winit's macOS module alone. The panel is a menu-bar
    // accessory: no Dock icon, and it does not take the menu bar from the
    // app the user is in.
    #[cfg(target_os = "macos")]
    {
        options.event_loop_builder = Some(accessory());
    }
    options
}

/// The hook that gives the event loop the `Accessory` activation policy.
#[cfg(target_os = "macos")]
fn accessory() -> eframe::EventLoopBuilderHook {
    Box::new(|builder| {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS as _};
        builder.with_activation_policy(ActivationPolicy::Accessory);
    })
}

/// The size the window opens at, before the panel knows its content.
pub fn start_size() -> Vec2 {
    vec2(theme::SIZE.panel_width, theme::SIZE.panel_start)
}

/// Whether the daemon has told what the panel draws: a daemon that is gone
/// or stopping has nothing more to say, and a running one has said it once
/// Xdebug is read (the last list of the bridge's first answer that the panel
/// draws).
pub fn is_ready(model: &Model) -> bool {
    match &model.connection {
        Connection::Failed(_) | Connection::ShuttingDown => true,
        Connection::Connecting | Connection::Reconnecting => false,
        Connection::Running(_) => model.xdebug.is_some() || model.xdebug_notice.is_some(),
    }
}

/// Whether two sizes are the same one: the system rounds to whole pixels.
pub fn same_size(a: Vec2, b: Vec2) -> bool {
    (a.x - b.x).abs() < SAME_SIZE && (a.y - b.y).abs() < SAME_SIZE
}

/// Whether two places are the same one, to the pixel.
pub fn same_place(a: Pos2, b: Pos2) -> bool {
    same_size(a.to_vec2(), b.to_vec2())
}

/// Watches the focus: a panel that had it and lost it closes (a click
/// outside is a loss), and one that never had it closes after a while.
#[derive(Debug)]
pub struct Focus {
    seen: bool,
    opened: Instant,
    /// Off for the design review's pictures, which run with no one at the
    /// keyboard.
    watching: bool,
}

impl Focus {
    pub const fn new(opened: Instant, watching: bool) -> Self {
        Self {
            seen: false,
            opened,
            watching,
        }
    }

    /// `focused` is what the window reports now (`None`: it does not know).
    /// Returns why the panel should close, if it should.
    pub fn check(&mut self, focused: Option<bool>, now: Instant) -> Option<PanelExit> {
        if !self.watching {
            return None;
        }
        match focused {
            Some(true) => {
                self.seen = true;
                None
            }
            Some(false) if self.seen => Some(PanelExit::FocusLost),
            _ if !self.seen && now.duration_since(self.opened) >= FOCUS_LIMIT => {
                Some(PanelExit::Closed)
            }
            _ => None,
        }
    }
}

/// Where the opening is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Waiting for the daemon's answer.
    Waiting,
    /// The window was asked for its size; it has not got it yet.
    Fitting(Instant),
    /// Drawn.
    Shown,
}

pub struct PanelApp {
    model: Model,
    bridge: bridge::Handle,
    /// The exit code `main` returns: what the user asked of the tray.
    exit: Arc<AtomicU8>,
    /// Set once the panel is closing, so the first reason stays.
    closing: bool,
    stage: Stage,
    opened: Instant,
    /// The size last asked of the window.
    asked: Option<Vec2>,
    /// Where the icon is, if the tray said.
    anchor: Option<Anchor>,
    /// The place last asked of the window, and how many times.
    moved: Option<(Pos2, u8)>,
    focus: Focus,
    /// When the program started, to log how long the first frame and the
    /// drawn panel took.
    started: Instant,
    first_frame: bool,
    /// The picture that `MARSA_GUI_SCREENSHOT` asks for.
    // Debug builds only: a design-review aid (see `screenshot`).
    #[cfg(debug_assertions)]
    screenshot: Option<crate::screenshot::Screenshot>,
}

impl PanelApp {
    pub fn new(
        ctx: &egui::Context,
        bridge: bridge::Handle,
        data_dir: PathBuf,
        started: Instant,
        exit: Arc<AtomicU8>,
        anchor: Option<Anchor>,
    ) -> Self {
        theme::style::apply(ctx);
        // The same appearance as the window; the panel only reads it.
        let prefs = Prefs::load(&Prefs::path(&data_dir));
        ctx.set_theme(prefs.appearance.preference());
        bridge.attach(ctx);
        let mut model = Model::new(data_dir);
        model.home = std::env::home_dir();
        // Debug builds only: fixed data for the design review (see `demo`).
        #[cfg(debug_assertions)]
        if let Some(demo) = crate::demo::requested() {
            crate::demo::fill(&mut model, demo);
        }
        let opened = Instant::now();
        // Debug builds only: a design-review aid (see `screenshot`).
        #[cfg(debug_assertions)]
        let screenshot = crate::screenshot::Screenshot::start(ctx);
        #[cfg(debug_assertions)]
        let watching = screenshot.is_none();
        #[cfg(not(debug_assertions))]
        let watching = true;
        // The fail-safe of `FOCUS_LIMIT` needs a frame to run in.
        ctx.request_repaint_after(FOCUS_LIMIT);
        Self {
            model,
            bridge,
            exit,
            closing: false,
            stage: Stage::Waiting,
            opened,
            asked: None,
            anchor,
            moved: None,
            focus: Focus::new(opened, watching),
            started,
            first_frame: true,
            #[cfg(debug_assertions)]
            screenshot,
        }
    }

    /// Closes the panel for `reason`. The first reason stays.
    fn close(&mut self, ctx: &egui::Context, reason: PanelExit) {
        if self.closing {
            return;
        }
        self.closing = true;
        self.exit.store(reason.code(), Ordering::Relaxed);
        tracing::info!(?reason, "panel closing");
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Applies what the user did in the panel.
    fn apply(&mut self, ctx: &egui::Context, actions: Vec<PanelAction>) {
        for action in actions {
            tracing::debug!(?action, "panel action");
            match action {
                PanelAction::Exit(reason) => self.close(ctx, reason),
                PanelAction::Do(action) => {
                    if let crate::model::Action::OpenSite(name) = &action {
                        open_site(ctx, &self.model, name);
                        // The browser takes the focus: the panel has done
                        // its work.
                        self.close(ctx, PanelExit::Closed);
                        continue;
                    }
                    if let Some(command) = self.model.apply_action(action) {
                        self.bridge.send(command);
                    }
                }
            }
        }
    }

    /// What the panel may be high, in points: the room under (or over, or
    /// beside) the icon on its monitor. No anchor, no limit.
    fn room(&self, ctx: &egui::Context) -> f32 {
        let scale = ctx.pixels_per_point();
        self.anchor.map_or(f32::INFINITY, |anchor| {
            let (gap, edge) = (theme::SIZE.panel_gap, theme::SIZE.panel_edge);
            anchor.max_height(gap * scale, edge * scale) / scale
        })
    }

    /// Asks the window for its place under the icon, for a panel `size`
    /// big. Returns whether it is there (or the system keeps it elsewhere).
    fn place(&mut self, ctx: &egui::Context, size: Vec2) -> bool {
        let Some(anchor) = self.anchor else {
            return true;
        };
        // The anchor is in physical pixels; the window's commands are in the
        // points of its own monitor, and the system turns them back with the
        // same scale.
        let scale = ctx.pixels_per_point();
        let (gap, edge) = (theme::SIZE.panel_gap, theme::SIZE.panel_edge);
        let at = anchor.place(size * scale, gap * scale, edge * scale) / scale;
        let have = ctx.input(|input| input.viewport().outer_rect.map(|rect| rect.min));
        if have.is_some_and(|have| same_place(have, at)) {
            return true;
        }
        let tries = match self.moved {
            Some((last, tries)) if same_place(last, at) => tries,
            _ => 0,
        };
        if tries >= MOST_MOVES {
            return true;
        }
        self.moved = Some((at, tries + 1));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(at));
        false
    }

    /// Asks the window for `want` if it has another size. Returns whether it
    /// has it now.
    fn fit(&mut self, ctx: &egui::Context, want: Vec2) -> bool {
        let have = ctx.input(|input| input.viewport().inner_rect.map(|rect| rect.size()));
        if have.is_some_and(|have| same_size(have, want)) {
            return true;
        }
        if self.asked != Some(want) {
            self.asked = Some(want);
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
        }
        false
    }
}

impl eframe::App for PanelApp {
    /// The window is transparent: the panel draws its own rounded corners.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    // Debug builds only: the design review's click and key (see
    // `screenshot`).
    #[cfg(debug_assertions)]
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(screenshot) = &mut self.screenshot {
            screenshot.inject(raw_input);
        }
    }

    /// Takes every waiting update before the frame is drawn.
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        take_updates(&self.bridge, &mut self.model);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if std::mem::take(&mut self.first_frame) {
            tracing::debug!(ms = self.started.elapsed().as_millis(), "panel first frame");
        }
        let now = Instant::now();
        if let Some(reason) = self
            .focus
            .check(ctx.input(|input| input.viewport().focused), now)
        {
            self.close(&ctx, reason);
            return;
        }
        if self.stage == Stage::Waiting {
            if !(is_ready(&self.model) || self.opened.elapsed() >= READY_LIMIT) {
                // The bridge wakes the window with its answer; the limit is
                // the other way out.
                ctx.request_repaint_after(READY_LIMIT);
                return;
            }
            self.stage = Stage::Fitting(now);
        }
        let theme = Theme::of(&ctx);
        let view = screen::view(&self.model);
        // Laid out first, drawn only once the window has its size: a layout
        // that is not drawn cannot be clicked either.
        let drawn = self.stage == Stage::Shown;
        let mut builder = UiBuilder::new().max_rect(ui.max_rect());
        if !drawn {
            builder = builder.invisible();
        }
        let mut child = ui.new_child(builder);
        let actions = screen::show_limited(&mut child, &view, theme, self.room(&ctx));
        let size = vec2(theme.size.panel_width, child.min_rect().height());
        // Both, every frame: the place depends on the size, and either may
        // change when the daemon answers.
        let (fits, placed) = (self.fit(&ctx, size), self.place(&ctx, size));
        let fits = fits && placed;
        match self.stage {
            Stage::Fitting(since) if fits || since.elapsed() >= FIT_LIMIT => {
                self.stage = Stage::Shown;
                let (outer, inner, monitor, scale) = ctx.input(|input| {
                    let viewport = input.viewport();
                    (
                        viewport.outer_rect,
                        viewport.inner_rect,
                        viewport.monitor_size,
                        input.pixels_per_point,
                    )
                });
                tracing::debug!(
                    ?outer,
                    ?inner,
                    ?monitor,
                    scale,
                    ms = since.elapsed().as_millis(),
                    since_start_ms = self.started.elapsed().as_millis(),
                    "panel placed"
                );
                ctx.request_repaint();
            }
            Stage::Fitting(_) => ctx.request_repaint_after(Duration::from_millis(16)),
            Stage::Waiting | Stage::Shown => {}
        }
        if drawn {
            self.apply(&ctx, actions);
        }
        // Debug builds only: a design-review aid (see `screenshot`).
        #[cfg(debug_assertions)]
        if let Some(screenshot) = &mut self.screenshot {
            screenshot.frame(&ctx, self.stage == Stage::Shown);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use marsa_core::types::{Health, XdebugState};

    use super::*;
    use crate::bridge::Command;
    use crate::model::Update;

    fn connected() -> Model {
        let mut model = Model::default();
        model.update(Update::Connected(Health {
            app_version: "0.0.0".into(),
            api_version: 1,
            pid: 7,
        }));
        model
    }

    #[test]
    fn the_panel_waits_for_the_daemon_and_is_ready_once_it_has_answered_or_is_gone() {
        let mut model = Model::default();
        assert!(!is_ready(&model), "connecting");
        model.update(Update::Failed("daemon_not_running".into()));
        assert!(is_ready(&model), "no daemon: that is the answer");
        let mut model = connected();
        assert!(!is_ready(&model), "connected, but its lists are on the way");
        model.update(Update::Xdebug(XdebugState {
            enabled: false,
            loaded: Vec::new(),
            missing: Vec::new(),
        }));
        assert!(is_ready(&model));
        // A read that failed is an answer too.
        let mut model = connected();
        model.update(Update::XdebugFailed("daemon_unreachable".into()));
        assert!(is_ready(&model));
        let mut model = connected();
        model.update(Update::Event(marsa_core::types::Event::ShuttingDown));
        assert!(is_ready(&model));
    }

    #[test]
    fn two_sizes_within_a_pixel_are_the_same() {
        assert!(same_size(vec2(360.0, 500.0), vec2(360.4, 499.6)));
        assert!(!same_size(vec2(360.0, 500.0), vec2(360.0, 502.0)));
        assert!(!same_size(vec2(360.0, 500.0), vec2(300.0, 500.0)));
    }

    #[test]
    fn a_panel_that_lost_the_focus_closes_and_one_that_never_had_it_closes_late() {
        let opened = Instant::now();
        let mut focus = Focus::new(opened, true);
        // Nothing known, nothing asked.
        assert_eq!(focus.check(None, opened), None);
        assert_eq!(focus.check(Some(false), opened), None, "not focused yet");
        assert_eq!(focus.check(Some(true), opened), None);
        assert_eq!(focus.check(Some(true), opened), None);
        assert_eq!(focus.check(Some(false), opened), Some(PanelExit::FocusLost));
        // Never focused: it closes after the limit, as `Closed`.
        let mut never = Focus::new(opened, true);
        let early = opened + FOCUS_LIMIT / 2;
        assert_eq!(never.check(Some(false), early), None);
        assert_eq!(
            never.check(Some(false), opened + FOCUS_LIMIT),
            Some(PanelExit::Closed)
        );
        // The picture's run is never closed by the focus.
        let mut watching_off = Focus::new(opened, false);
        assert_eq!(watching_off.check(Some(true), opened), None);
        assert_eq!(watching_off.check(Some(false), opened), None);
        assert_eq!(
            watching_off.check(Some(false), opened + FOCUS_LIMIT * 2),
            None
        );
    }

    #[test]
    fn the_options_ask_for_a_borderless_transparent_window_on_top_with_no_taskbar_entry() {
        let options = options(start_size(), None);
        let viewport = &options.viewport;
        assert_eq!(viewport.decorations, Some(false));
        assert_eq!(viewport.transparent, Some(true));
        assert_eq!(viewport.resizable, Some(false));
        assert_eq!(viewport.taskbar, Some(false));
        assert_eq!(viewport.window_level, Some(egui::WindowLevel::AlwaysOnTop));
        assert_eq!(viewport.inner_size, Some(start_size()));
        assert_eq!(start_size().x, 360.0);
    }

    #[test]
    fn closing_keeps_the_first_reason_and_the_exit_code_says_it() {
        let ctx = egui::Context::default();
        let dir = tempfile::tempdir().expect("temp dir");
        let exit = Arc::new(AtomicU8::new(PanelExit::Closed.code()));
        let mut app = PanelApp::new(
            &ctx,
            bridge::Handle::detached(),
            dir.path().to_path_buf(),
            Instant::now(),
            Arc::clone(&exit),
            None,
        );
        assert_eq!(exit.load(Ordering::Relaxed), PanelExit::Closed.code());
        app.apply(&ctx, vec![PanelAction::Exit(PanelExit::Quit)]);
        assert_eq!(exit.load(Ordering::Relaxed), PanelExit::Quit.code());
        // A second reason does not replace the first.
        app.apply(&ctx, vec![PanelAction::Exit(PanelExit::OpenWindow)]);
        assert_eq!(exit.load(Ordering::Relaxed), PanelExit::Quit.code());
        // The panel reads the appearance and writes nothing.
        assert!(!Prefs::path(dir.path()).exists());
        assert!(Path::new(dir.path()).exists());
    }

    #[test]
    fn a_command_of_the_panel_goes_to_the_bridge_like_the_windows() {
        let ctx = egui::Context::default();
        let dir = tempfile::tempdir().expect("temp dir");
        let exit = Arc::new(AtomicU8::new(PanelExit::Closed.code()));
        let mut app = PanelApp::new(
            &ctx,
            bridge::Handle::detached(),
            dir.path().to_path_buf(),
            Instant::now(),
            exit,
            None,
        );
        app.model.update(Update::Connected(Health {
            app_version: "0.0.0".into(),
            api_version: 1,
            pid: 7,
        }));
        app.model.update(Update::Xdebug(XdebugState {
            enabled: false,
            loaded: Vec::new(),
            missing: Vec::new(),
        }));
        // The model is the window's: Xdebug flips there, and asks for a command.
        let command = app.model.apply_action(crate::model::Action::ToggleXdebug);
        assert!(matches!(command, Some(Command::SetXdebug(true))));
        assert_eq!(app.model.xdebug_on(), Some(true));
    }
}
