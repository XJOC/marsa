//! Logs: the Caddy log. M5-D10 adds the other sources of
//! [`crate::model::LogSource`], the filters, and Follow.

use eframe::egui;
use marsa_core::types::LogStream;

use crate::model::Model;
use crate::strings;

/// The log of [`crate::model::LOG_SERVICE`]. `show_rows` lays out the
/// visible lines only. It follows new lines while scrolled to the end, and
/// stays put once the user scrolls up. Caddy writes its whole log to stderr,
/// so stderr keeps the normal text color and stdout is the one drawn weak.
pub fn show(ui: &mut egui::Ui, model: &Model) {
    ui.strong(strings::LOG_TITLE);
    if model.log.is_empty() {
        ui.weak(strings::LOG_EMPTY);
        return;
    }
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    egui::ScrollArea::vertical()
        .id_salt("log")
        .auto_shrink(false)
        .stick_to_bottom(true)
        .show_rows(ui, row_height, model.log.len(), |ui, rows| {
            for row in model.log.range(rows) {
                let mut text = egui::RichText::new(&row.text).monospace();
                if row.stream == LogStream::Stdout {
                    text = text.weak();
                }
                ui.add(egui::Label::new(text).truncate());
            }
        });
}
