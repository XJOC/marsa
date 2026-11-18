//! A card of facts (the mockup's site page): rows of a label in `mute` at the
//! start, in a column of the same width on every row, and its value after it.
//! The rows are as high as a list row (`size.row`) and divided by 1 px `line`,
//! like the rows of a table. The value is what the screen adds: text, a path,
//! or a switch.
//!
//! The card never reads the model: a screen adds each row with the widgets it
//! needs.

use eframe::egui::{Frame, Label, Margin, RichText, Stroke, Ui, vec2};

use super::table::card;
use super::whole;
use crate::theme::{Direction, Theme};

/// The rows of a facts card, added one after the other.
pub struct Rows<'a> {
    ui: &'a mut Ui,
    label_width: f32,
    first: bool,
}

/// Draws the card and calls `add` to fill it. `label_width` is the width of
/// the column of labels.
pub fn fact_card(ui: &mut Ui, label_width: f32, add: impl FnOnce(&mut Rows<'_>)) {
    card(ui, |ui| {
        // The rows sit flush against each other: the dividers separate them.
        ui.spacing_mut().item_spacing.y = 0.0;
        add(&mut Rows {
            ui,
            label_width,
            first: true,
        });
    });
}

impl Rows<'_> {
    /// One row: `label` in the first column, then what `add` draws.
    pub fn row(&mut self, label: &str, add: impl FnOnce(&mut Ui)) {
        let theme = Theme::of(self.ui.ctx());
        let ui = &mut *self.ui;
        if !std::mem::replace(&mut self.first, false) {
            let (range, y) = (ui.max_rect().x_range(), ui.cursor().top());
            ui.painter()
                .hline(range, y, Stroke::new(theme.size.border, theme.color.line));
            ui.add_space(theme.size.border);
        }
        let margin = Margin::symmetric(whole(theme.space.s4), whole(theme.space.s2));
        Frame::new().inner_margin(margin).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let line = vec2(ui.available_width(), theme.size.control);
            ui.allocate_ui_with_layout(line, Direction::CURRENT.row(), |ui| {
                ui.spacing_mut().item_spacing.x = theme.space.s3;
                let cell = vec2(self.label_width, theme.size.control);
                ui.allocate_ui_with_layout(cell, Direction::CURRENT.row(), |ui| {
                    // The column is as wide as asked, not as wide as its text.
                    ui.set_min_size(cell);
                    let text = RichText::new(label)
                        .font(theme.text.body.font_id())
                        .color(theme.color.mute);
                    ui.add(Label::new(text).selectable(false));
                });
                add(ui);
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Rect};

    use super::*;
    use crate::theme::LIGHT;
    use crate::widgets::harness::Harness;

    const LABEL: f32 = 130.0;

    fn draw(ui: &mut Ui, labels: &[&str]) {
        fact_card(ui, LABEL, |rows| {
            for label in labels {
                rows.row(label, |ui| {
                    ui.label(format!("value of {label}"));
                });
            }
        });
    }

    fn bounds(tree: &crate::widgets::harness::Tree, text: &str) -> Rect {
        let b = tree
            .node_with_text(text)
            .unwrap_or_else(|| panic!("no {text}"))
            .bounds()
            .expect("a place");
        Rect::from_min_max(
            egui::pos2(b.x0 as f32, b.y0 as f32),
            egui::pos2(b.x1 as f32, b.y1 as f32),
        )
    }

    #[test]
    fn every_value_starts_at_the_same_x_after_the_label_column() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| draw(ui, &["Kind", "Folder"]));
        let kind = bounds(&tree, "value of Kind");
        let folder = bounds(&tree, "value of Folder");
        assert_eq!(kind.left(), folder.left());
        let label = bounds(&tree, "Kind");
        assert_eq!(
            kind.left() - label.left(),
            LABEL + LIGHT.space.s3,
            "the label column, then the gap"
        );
    }

    #[test]
    fn a_row_is_as_high_as_a_list_row_and_a_divider_sits_between_two_rows() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| draw(ui, &["Kind", "Folder"]));
        let step = bounds(&tree, "Folder").center().y - bounds(&tree, "Kind").center().y;
        // The value is centered in a row of `size.row`, plus the divider.
        assert_eq!(step, LIGHT.size.row + LIGHT.size.border);
        let shapes = harness.shapes(|ui| draw(ui, &["Kind", "Folder", "Added by"]));
        let dividers = shapes
            .iter()
            .filter(|shape| {
                matches!(shape, egui::Shape::LineSegment { stroke, .. } if stroke.color == LIGHT.color.line && stroke.width == LIGHT.size.border)
            })
            .count();
        assert_eq!(dividers, 2, "three rows, two dividers");
    }
}
