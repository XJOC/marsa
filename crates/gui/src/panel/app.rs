//! The panel's window loop (ADR 0022): an eframe app like the window's, with
//! a borderless transparent viewport that sizes itself to what the panel
//! says, closes on focus loss and on Esc, and ends with the exit code that
//! tells the tray what the user asked for.
//!
//! Each frame, `logic` applies the bridge's updates, and `ui` draws
//! [`screens::panel`](crate::screens::panel) and applies what it returns. The
//! panel opens in three steps, so it never shows a small panel that jumps to
//! its size: it waits for the daemon's answer (a short time at most), then
//! asks the system for the window's size, and draws once the window has it.
//! Until then the window is transparent and empty.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{self, UiBuilder, Vec2, vec2};

use super::PanelExit;
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
/// A size that differs by less than this is the same: the system rounds.
const SAME_SIZE: f32 = 1.0;

/// The options of the panel's window: borderless and transparent, the corners
/// are the panel's own; always on top; no entry in the taskbar. The system's
/// shadow follows the shape on macOS.
pub fn options(start: Vec2) -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(strings::PANEL_NAME)
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_has_shadow(true)
            .with_inner_size(start)
            .with_window_level(egui::WindowLevel::AlwaysOnTop)
            .with_taskbar(false)
            .with_active(true),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    }
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
    focus: Focus,
    /// Set until the first frame, to log the time it took.
    started: Option<Instant>,
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
            focus: Focus::new(opened, watching),
            started: Some(started),
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
        if let Some(started) = self.started.take() {
            tracing::debug!(ms = started.elapsed().as_millis(), "panel first frame");
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
        let actions = screen::show(&mut child, &view, theme);
        let content = child.min_rect().size();
        let fits = self.fit(&ctx, vec2(theme.size.panel_width, content.y));
        match self.stage {
            Stage::Fitting(since) if fits || since.elapsed() >= FIT_LIMIT => {
                self.stage = Stage::Shown;
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
        let options = options(start_size());
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
