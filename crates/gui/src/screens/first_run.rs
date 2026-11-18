//! The first-run panel: one button that installs PHP and Caddy, then
//! setup.

use eframe::egui;

use super::install_progress;
use super::settings::setup;
use crate::bridge::Command;
use crate::model::{FirstRun, Model, Setup};
use crate::strings;

/// The first-run panel: one button that installs PHP and Caddy, then setup.
// TODO(M5-D5): return `Action`s instead of calling the model's methods
// (code.md, State), with the redesign of this screen.
pub fn show(ui: &mut egui::Ui, model: &mut Model) -> Option<Command> {
    ui.strong(strings::FIRST_RUN_TITLE);
    let mut command = None;
    match model.first_run {
        Some(stage @ (FirstRun::Offer | FirstRun::Installing)) => {
            ui.label(strings::FIRST_RUN_TEXT);
            ui.horizontal(|ui| {
                let offer = stage == FirstRun::Offer;
                let enabled = offer && !model.installing();
                let clicked = ui
                    .add_enabled(enabled, egui::Button::new(strings::FIRST_RUN_INSTALL))
                    .on_hover_text(strings::FIRST_RUN_INSTALL_HINT)
                    .clicked();
                if clicked {
                    // The bridge sends the first job at the end of this frame.
                    model.start_first_run();
                }
                if offer && ui.button(strings::NOT_NOW).clicked() {
                    model.close_first_run();
                }
            });
            install_progress(ui, model);
        }
        Some(FirstRun::Setup) => {
            ui.label(strings::FIRST_RUN_READY);
            command = setup(ui, model);
            let done = matches!(model.setup, Setup::Finished(_));
            let close = if done {
                strings::CLOSE
            } else {
                strings::NOT_NOW
            };
            if ui.button(close).clicked() {
                model.close_first_run();
            }
        }
        None => {}
    }
    command
}
