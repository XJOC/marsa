//! Home. For now the status line and the last notice, with the Settings
//! section folded under them, as in the old one-page window.

use eframe::egui;

use super::settings;
use crate::bridge::Command;
use crate::model::Model;
use crate::strings;
use crate::theme::page::SETTINGS_HEIGHT;

/// The status line, the last notice, and the Settings section.
pub fn show(ui: &mut egui::Ui, model: &mut Model) -> Option<Command> {
    ui.label(strings::status_line(&model.connection));
    if let Some(code) = &model.notice {
        ui.colored_label(ui.visuals().error_fg_color, strings::error_text(code));
    }
    let mut command = None;
    egui::CollapsingHeader::new(strings::SETTINGS)
        .id_salt("settings")
        .default_open(false)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("settings_scroll")
                .max_height(SETTINGS_HEIGHT)
                .show(ui, |ui| command = settings::show(ui, model));
        });
    command
}
