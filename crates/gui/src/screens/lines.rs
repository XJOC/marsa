//! The lines of words several screens share: a sentence in a card, a muted
//! note, a status line with its mark, and a folder's path.

use std::path::Path;

use eframe::egui::Ui;

use crate::strings;
use crate::theme::{Section, Theme};
use crate::widgets::table::padded_card;
use crate::widgets::{Glyph, Icon, NoticeKind, Text};

/// A sentence in a card, in place of a table.
pub(crate) fn message(ui: &mut Ui, theme: &Theme, text: &str) {
    padded_card(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add(Text::new(text, theme.text.body, theme.color.ink2));
        });
    });
}

/// A line of `caption` text in `mute`.
pub(crate) fn mute(ui: &mut Ui, theme: &Theme, text: &str) {
    ui.add(Text::caption(theme, text));
}

/// How a line of status says what kind it is, besides its words (R8).
#[derive(Clone, Copy)]
pub(crate) enum Mark {
    Error,
    /// Amber (tokens.md, Color: states).
    Warning,
}

/// An icon and a sentence that wraps, then whatever `add` puts after them.
pub(crate) fn status_line(
    ui: &mut Ui,
    theme: &Theme,
    mark: Mark,
    text: &str,
    add: impl FnOnce(&mut Ui),
) {
    let (icon, color) = match mark {
        Mark::Error => crate::widgets::notice::mark(theme, NoticeKind::Error),
        Mark::Warning => (Icon::Alert, theme.section(Section::Dumps).fg),
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = theme.space.s2;
        ui.add(Glyph::new(icon).size(theme.size.icon_inline).color(color));
        ui.add(Text::new(text, theme.text.body_sm, theme.color.ink));
        add(ui);
    });
}

/// A folder's path in the mono face, as `~/…` under the home folder, cut
/// short at the end of the column. The tooltip has the full path.
pub(crate) fn path(ui: &mut Ui, theme: &Theme, folder: &Path, home: Option<&Path>) {
    let shown = strings::home_path(folder, home);
    let response = ui.add(Text::new(&shown, theme.text.mono_sm, theme.color.ink).truncate());
    // Shortened for the eye, or cut short by the column: the whole path is
    // one hover away.
    response.on_hover_text(folder.display().to_string());
}
