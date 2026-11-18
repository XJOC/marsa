//! Home: for now the status line of the old one-page window. The shell adds
//! the first-run panel under it. M5-D5 draws the design's Home: the status
//! sentence, the default version, the first sites.

use eframe::egui;

use crate::model::Model;
use crate::strings;

/// The status line.
pub fn show(ui: &mut egui::Ui, model: &Model) {
    ui.label(strings::status_line(&model.connection));
}
