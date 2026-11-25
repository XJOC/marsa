//! The review aids of the window, in one place: the demo data and the
//! screenshot (debug builds only), the widget gallery (debug builds only),
//! and the measurement mode (the `measure` feature). A shipped binary has
//! none of them (rules.md 15), so every `cfg` for them is here and the
//! window's loop calls these methods without one.

use eframe::egui;

use crate::model::{Model, Route};

/// What the review aids hold. With none of them built, it is empty and every
/// method does nothing.
#[derive(Default)]
pub(crate) struct ReviewAids {
    /// The measurement `MARSA_GUI_MEASURE` asks for (see `measure`).
    #[cfg(feature = "measure")]
    measure: Option<crate::measure::Run>,
    /// The picture of the window that `MARSA_GUI_SCREENSHOT` asks for (see
    /// `screenshot`).
    #[cfg(debug_assertions)]
    screenshot: Option<crate::screenshot::Screenshot>,
    /// `MARSA_GUI_GALLERY=1`: the window shows the widget gallery instead of
    /// the screens (see `widgets::gallery`).
    #[cfg(debug_assertions)]
    gallery: bool,
}

impl ReviewAids {
    /// The aids the environment asks for.
    pub(crate) fn new(ctx: &egui::Context) -> Self {
        #[cfg(debug_assertions)]
        {
            Self {
                #[cfg(feature = "measure")]
                measure: None,
                screenshot: crate::screenshot::Screenshot::start(ctx),
                gallery: crate::widgets::gallery::requested(),
            }
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = ctx;
            Self::default()
        }
    }

    /// Fixed fake data for the design review, if `MARSA_GUI_DEMO` asks.
    pub(crate) fn fill_demo(model: &mut Model) {
        #[cfg(debug_assertions)]
        if let Some(demo) = crate::demo::requested() {
            crate::demo::fill(model, demo);
        }
        #[cfg(not(debug_assertions))]
        let _ = model;
    }

    /// The screen `MARSA_GUI_ROUTE` asks for.
    pub(crate) fn route() -> Option<Route> {
        #[cfg(debug_assertions)]
        {
            crate::screenshot::route()
        }
        #[cfg(not(debug_assertions))]
        {
            None
        }
    }

    /// The demo's sites are made up: none of them is a page to open.
    pub(crate) fn is_demo() -> bool {
        #[cfg(debug_assertions)]
        {
            crate::demo::requested().is_some()
        }
        #[cfg(not(debug_assertions))]
        {
            false
        }
    }

    /// The window in measurement mode: made-up data, and its frames counted.
    #[cfg(feature = "measure")]
    pub(crate) fn measuring(&mut self, model: &mut Model, plan: crate::measure::Plan) {
        crate::measure::fill(model, plan);
        self.measure = Some(crate::measure::Run::new(plan));
    }

    #[cfg(all(test, feature = "measure"))]
    pub(crate) fn measure(&self) -> Option<&crate::measure::Run> {
        self.measure.as_ref()
    }

    /// The click or the wheel a review asks for goes into the frame's input.
    pub(crate) fn inject(&mut self, raw_input: &mut egui::RawInput) {
        #[cfg(debug_assertions)]
        if let Some(screenshot) = &mut self.screenshot {
            screenshot.inject(raw_input);
        }
        #[cfg(feature = "measure")]
        if let Some(run) = &mut self.measure {
            run.inject(raw_input);
        }
        #[cfg(not(any(debug_assertions, feature = "measure")))]
        let _ = raw_input;
    }

    /// The first frame was drawn `ms` after the start.
    pub(crate) fn first_frame(&mut self, ms: u128) {
        #[cfg(feature = "measure")]
        if let Some(run) = &mut self.measure {
            run.first_frame(ms);
        }
        #[cfg(not(feature = "measure"))]
        let _ = ms;
    }

    /// Draws the widget gallery in place of the screens, when it is asked
    /// for. `true` when it did, and the frame is done.
    pub(crate) fn show_gallery(&mut self, ui: &mut egui::Ui) -> bool {
        #[cfg(debug_assertions)]
        if self.gallery {
            crate::widgets::gallery::show(ui);
            if let Some(screenshot) = &mut self.screenshot {
                // The gallery needs no daemon: it is ready at once.
                screenshot.frame(ui.ctx(), true);
            }
            return true;
        }
        #[cfg(not(debug_assertions))]
        let _ = ui;
        false
    }

    /// Draws the SQL editor in place of the screens when the measurement
    /// types into it. `true` when it did.
    pub(crate) fn show_editor(&mut self, ui: &mut egui::Ui) -> bool {
        #[cfg(feature = "measure")]
        if let Some(buffer) = self.measure.as_mut().and_then(crate::measure::Run::editor) {
            let theme = crate::theme::Theme::of(ui.ctx());
            let lines = (ui.available_height() / theme.size.editor_line) as usize;
            let engine = marsa_core::db_browser::Engine::Mysql;
            crate::widgets::editor::Editor::new("measure_editor", "SQL", buffer, engine, lines)
                .show(ui);
            return true;
        }
        #[cfg(not(feature = "measure"))]
        let _ = ui;
        false
    }

    /// The screens were drawn: `built` is what building them took, `cpu` the
    /// last frame's cost, and `connected` whether the picture may be taken.
    pub(crate) fn frame_done(
        &mut self,
        ctx: &egui::Context,
        connected: bool,
        built: std::time::Duration,
        cpu: Option<f32>,
    ) {
        #[cfg(feature = "measure")]
        if let Some(run) = &mut self.measure {
            run.frame(ctx, built, cpu);
        }
        #[cfg(not(feature = "measure"))]
        let _ = (built, cpu);
        #[cfg(debug_assertions)]
        if let Some(screenshot) = &mut self.screenshot {
            screenshot.frame(ctx, connected);
        }
        #[cfg(not(debug_assertions))]
        let _ = (ctx, connected);
    }
}
