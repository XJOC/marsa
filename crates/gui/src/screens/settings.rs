//! Settings: start at login, the data folder, the versions, and setup.

use eframe::egui;
use marsa_core::recipes::Platform;

use super::report_line;
use crate::bridge::Command;
use crate::model::{self, Autostart, Model, Setup};
use crate::strings;

/// Start at login, the data folder, the versions, and setup.
// TODO(M5-D9): return `Action`s instead of calling the model's methods
// (code.md, State), with the redesign of this screen.
pub fn show(ui: &mut egui::Ui, model: &mut Model) -> Option<Command> {
    ui.strong(strings::SETTINGS);
    let mut command = None;
    ui.horizontal(|ui| {
        let mut on = model.autostart == Autostart::On;
        let usable = matches!(
            model.autostart,
            Autostart::Off | Autostart::On | Autostart::Stale
        );
        let enabled = model.is_connected() && usable;
        let response = ui
            .add_enabled(enabled, egui::Checkbox::new(&mut on, strings::AUTOSTART))
            .on_hover_text(strings::AUTOSTART_HINT);
        if response.changed() {
            command = model.toggle_autostart().map(Command::SetAutostart);
        }
        if let Some(note) = strings::autostart_note(model.autostart) {
            ui.weak(note);
        }
    });
    if let Some(code) = &model.settings_notice {
        ui.colored_label(ui.visuals().error_fg_color, strings::error_text(code));
    }
    egui::Grid::new("settings_grid")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label(strings::DATA_FOLDER)
                .on_hover_text(strings::DATA_FOLDER_HINT);
            let folder = model.data_dir.display().to_string();
            ui.add(egui::Label::new(folder).selectable(true));
            ui.end_row();
            ui.label(strings::APP_VERSION);
            ui.label(model::APP_VERSION);
            ui.end_row();
            ui.label(strings::DAEMON_VERSION);
            ui.label(strings::daemon_version(&model.connection));
            ui.end_row();
        });
    ui.separator();
    if let Some(setup) = setup(ui, model) {
        command = Some(setup);
    }
    command
}

/// What setup does, its button, and its last report. In the first-run panel
/// and in Settings.
pub fn setup(ui: &mut egui::Ui, model: &mut Model) -> Option<Command> {
    let platform = Platform::current();
    ui.label(strings::setup_explanation(platform));
    let running = model.setup == Setup::Running;
    let mut command = None;
    ui.horizontal(|ui| {
        let enabled = model.is_connected() && !running;
        let clicked = ui
            .add_enabled(enabled, egui::Button::new(strings::RUN_SETUP))
            .on_hover_text(strings::RUN_SETUP_HINT)
            .clicked();
        if clicked && model.start_setup() {
            command = Some(Command::Setup);
        }
        if running {
            ui.weak(strings::setup_running(platform));
        }
    });
    match &model.setup {
        Setup::Finished(report) => {
            for line in strings::setup_lines(report) {
                report_line(ui, line);
            }
        }
        Setup::Failed(code) => {
            ui.colored_label(ui.visuals().error_fg_color, strings::error_text(code));
        }
        Setup::Idle | Setup::Running => {}
    }
    command
}
