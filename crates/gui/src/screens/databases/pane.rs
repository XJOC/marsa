//! What the views of Databases share: the value pane under a grid
//! (components.md, "Value pane"), and the banner of a refused read.

use eframe::egui::{
    Align, CornerRadius, Frame, Label, Layout, Margin, RichText, ScrollArea, Stroke, Ui, Vec2, vec2,
};
use marsa_core::types::DbCell;

use crate::strings;
use crate::theme::{Direction, Theme};
use crate::widgets::icons::{Glyph, Icon};
use crate::widgets::table::card;
use crate::widgets::tag::Tag;
use crate::widgets::{Button, Text};

/// What the pane shows.
pub(super) struct Pane<'a> {
    /// The column's name, or "Value".
    pub title: &'a str,
    /// Its kind and the value's size.
    pub caption: Option<String>,
    pub cell: &'a DbCell,
    /// The card's height.
    pub height: f32,
    /// A pane under a grid closes; one that is the whole detail does not.
    pub closes: bool,
}

/// What the user did on the pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneAction {
    Copy,
    Close,
}

/// The card: the title and the caption, Copy and close, then the value in
/// mono, wrapped and selectable. `below` adds the lines under the value.
pub(super) fn show(
    ui: &mut Ui,
    theme: &Theme,
    pane: Pane<'_>,
    below: impl FnOnce(&mut Ui),
) -> Option<PaneAction> {
    let mut action = None;
    let size = vec2(ui.available_width(), pane.height);
    ui.allocate_ui_with_layout(size, Direction::CURRENT.column(), |ui| {
        card(ui, |ui| {
            ui.set_min_height(pane.height);
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let line = vec2(ui.available_width(), theme.size.tag + theme.space.s3);
            ui.allocate_ui_with_layout(line, Direction::CURRENT.row(), |ui| {
                ui.spacing_mut().item_spacing.x = theme.space.s3;
                ui.add_space(theme.space.s3);
                ui.add(Text::new(
                    pane.title,
                    theme.text.body_strong,
                    theme.color.ink,
                ));
                if let Some(caption) = &pane.caption {
                    ui.add(Text::new(caption, theme.text.caption, theme.color.mute));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(theme.space.s2);
                    if pane.closes {
                        let close = Button::icon(Icon::Cross, strings::DB_CLOSE_VALUE);
                        if ui.add(close).clicked() {
                            action = Some(PaneAction::Close);
                        }
                    }
                    let binary = matches!(pane.cell, DbCell::Binary { .. });
                    let copy = Button::secondary(strings::DB_COPY)
                        .in_row()
                        .disabled_if(binary, strings::DB_NO_COPY);
                    if ui.add(copy).clicked() {
                        action = Some(PaneAction::Copy);
                    }
                });
            });
            let rest = vec2(ui.available_width(), ui.available_height());
            ui.allocate_ui_with_layout(rest, Direction::CURRENT.column(), |ui| {
                ui.add_space(theme.space.s2);
                ScrollArea::vertical()
                    .id_salt("db_value")
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.add_space(theme.space.s3);
                            ui.vertical(|ui| {
                                value(ui, theme, pane.cell);
                                below(ui);
                            });
                        });
                    });
            });
        });
    });
    action
}

fn value(ui: &mut Ui, theme: &Theme, cell: &DbCell) {
    match cell {
        DbCell::Null => {
            ui.add(Tag::new(strings::NULL_TAG, theme));
        }
        DbCell::Binary { bytes } => {
            let text = strings::binary_value(*bytes);
            ui.add(Text::new(&text, theme.text.body_sm, theme.color.mute));
        }
        DbCell::Text(text) | DbCell::Cut { text, .. } => {
            // egui keeps the laid-out text while it does not change.
            let text = RichText::new(text)
                .font(theme.text.mono_sm.font_id())
                .color(theme.color.ink);
            ui.add(Label::new(text).wrap().selectable(true));
        }
    }
}

/// A refused read: the cross, one sentence of ours, then the server's own
/// words in mono, selectable. Those words are data: never translated.
pub(super) fn banner(ui: &mut Ui, theme: &Theme, sentence: &str, detail: Option<&str>) {
    let pad = theme.space.s4;
    Frame::new()
        .fill(theme.color.error_bg)
        .stroke(Stroke::new(theme.size.border, theme.color.error_line))
        .corner_radius(CornerRadius::same(theme.radius.lg))
        .inner_margin(Margin::same(whole(pad)))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = theme.space.s2;
            ui.horizontal(|ui| {
                ui.add(Glyph::new(Icon::Cross).color(theme.color.error));
                ui.add(Text::new(sentence, theme.text.body_strong, theme.color.ink));
            });
            if let Some(words) = detail {
                let words = RichText::new(words)
                    .font(theme.text.mono_sm.font_id())
                    .color(theme.color.ink);
                ui.add(Label::new(words).wrap().selectable(true));
            }
        });
}

/// A margin is whole points.
fn whole(points: f32) -> i8 {
    points.round() as i8
}
