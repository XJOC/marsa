//! The rows of a settings card (the mockup's Settings): in each row the name
//! of the setting with a hint under it in `caption`, at the start, and the
//! control or the value at the end. The rows sit in a card and are divided by
//! 1 px `line`, like the rows of a table.
//!
//! The control is drawn first, at the end of the row, and keeps the room it
//! needs (a long value is cut short at the end of what is left after the
//! least the text gets); the text takes the rest, and wraps there. A row is
//! `size.row_max` high at least and grows with a hint that wraps.
//!
//! The card never reads the model: a screen adds each row with the widgets it
//! needs. [`SettingRows::row`] returns the response of the text, so a screen
//! can make the name of a switch part of its click target.

use eframe::egui::{
    Align2, Label, Pos2, Rect, Response, RichText, Sense, Stroke, Ui, UiBuilder, pos2, vec2,
};

use super::table::card;
use super::{layout, slot};
use crate::theme::page::SETTINGS_TEXT_MIN;
use crate::theme::{Direction, Theme};

/// The rows of a settings card, added one after the other.
pub struct SettingRows<'a> {
    ui: &'a mut Ui,
    first: bool,
}

/// Draws the card and calls `add` to fill it.
pub fn setting_card(ui: &mut Ui, add: impl FnOnce(&mut SettingRows<'_>)) {
    card(ui, |ui| {
        // The rows sit flush against each other: the dividers separate them.
        ui.spacing_mut().item_spacing.y = 0.0;
        add(&mut SettingRows { ui, first: true });
    });
}

impl SettingRows<'_> {
    /// One row: `label` and its `hint` at the start, and what `control` draws
    /// at the end. The response is the text's: a click on it is `clicked`.
    pub fn row(
        &mut self,
        label: &str,
        hint: Option<&str>,
        control: impl FnOnce(&mut Ui),
    ) -> Response {
        let theme = Theme::of(self.ui.ctx());
        let direction = Direction::CURRENT;
        let ui = &mut *self.ui;
        if !std::mem::replace(&mut self.first, false) {
            let (range, y) = (ui.max_rect().x_range(), ui.cursor().top());
            ui.painter()
                .hline(range, y, Stroke::new(theme.size.border, theme.color.line));
            ui.add_space(theme.size.border);
        }
        let (side, gap, least) = (theme.space.s4, theme.space.s3, theme.size.row_max);
        let width = ui.available_width();
        let inner = (width - side - side).max(0.0);
        let top = ui.cursor().top();
        let row = Rect::from_min_size(pos2(ui.cursor().left() + side, top), vec2(inner, least));

        // The control, at the end of the row. It may take what the text
        // leaves: at least `SETTINGS_TEXT_MIN`.
        let room = (inner - SETTINGS_TEXT_MIN - gap).max(0.0);
        let at = pos2(slot(direction, row, inner - room, room), top);
        let mut end = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_size(at, vec2(room, least)))
                .layout(direction.row_from_end()),
        );
        end.spacing_mut().item_spacing.x = theme.space.s2;
        control(&mut end);
        let used = end.min_rect().width();

        // The text takes the rest of the row, and its two lines are centered
        // in the least height: a hint that wraps grows the row.
        let text_width = (inner - used - gap).max(0.0);
        let heights =
            |text: &str, token| layout(ui.painter(), text, token, theme.color.ink).size().y;
        let stack = heights(label, theme.text.body)
            + hint.map_or(0.0, |hint| {
                theme.space.s1 + heights(hint, theme.text.caption)
            });
        let pad = pad_for(stack, least).max(theme.space.s2);
        let at = pos2(slot(direction, row, 0.0, text_width), top + pad);
        let mut text = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_size(at, vec2(text_width, f32::INFINITY)))
                .layout(direction.column()),
        );
        text.spacing_mut().item_spacing.y = theme.space.s1;
        let name = RichText::new(label)
            .font(theme.text.body.font_id())
            .color(theme.color.ink);
        text.add(Label::new(name).selectable(false).wrap());
        if let Some(hint) = hint {
            let hint = RichText::new(hint)
                .font(theme.text.caption.font_id())
                .color(theme.color.mute);
            text.add(Label::new(hint).selectable(false).wrap());
        }
        let words = text.min_rect();
        let height = (pad + words.height() + pad).max(least);
        ui.allocate_space(vec2(width, height));
        // Over the text only, so it never covers the control.
        ui.interact(
            words,
            ui.scope_id().with(("setting_row", label)),
            Sense::CLICK,
        )
    }
}

/// The space above and below `stack` when it is centered in `least`.
fn pad_for(stack: f32, least: f32) -> f32 {
    let line = Rect::from_min_size(Pos2::ZERO, vec2(0.0, least));
    Align2::LEFT_CENTER
        .align_size_within_rect(vec2(0.0, stack), line)
        .top()
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Rect};

    use super::*;
    use crate::theme::LIGHT;
    use crate::widgets::harness::{Harness, Tree};
    use crate::widgets::{Button, Switch};

    const HINT: &str = "The hint under the name";

    fn bounds(tree: &Tree, text: &str) -> Rect {
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

    fn draw(ui: &mut Ui) {
        setting_card(ui, |rows| {
            rows.row("Name", Some(HINT), |ui| {
                ui.label("value of Name");
            });
            rows.row("Other", None, |ui| {
                ui.label("value of Other");
            });
        });
    }

    #[test]
    fn the_name_and_its_hint_are_at_the_start_and_the_value_is_at_the_end() {
        let harness = Harness::new(egui::Theme::Light);
        let mut card = Rect::NOTHING;
        let tree = harness.accessibility(|ui| {
            let start = ui.cursor().min;
            draw(ui);
            card = Rect::from_min_max(start, egui::pos2(ui.min_rect().right(), ui.cursor().top()));
        });
        let name = bounds(&tree, "Name");
        let hint = bounds(&tree, HINT);
        let value = bounds(&tree, "value of Name");
        // The hint is under the name, both start at the same x.
        assert!(hint.top() >= name.bottom() - 0.5, "{name:?} {hint:?}");
        assert_eq!(hint.left(), name.left());
        // The value ends where the row's padding begins, at the end.
        assert!(
            (card.right() - LIGHT.space.s4 - value.right()).abs() < 1.5,
            "{card:?} {value:?}"
        );
        assert!(value.left() > hint.right());
        // The value is level with the middle of the row.
        assert!((value.center().y - (name.top() + hint.bottom()) / 2.0).abs() < 4.0);
    }

    #[test]
    fn a_row_is_the_least_height_and_a_divider_sits_between_two_rows() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(draw);
        let step =
            bounds(&tree, "value of Other").center().y - bounds(&tree, "value of Name").center().y;
        // Two rows of `size.row_max` and the divider between them.
        assert_eq!(step, LIGHT.size.row_max + LIGHT.size.border);
        assert_eq!(LIGHT.size.row_max, 54.0);
        let shapes = harness.shapes(draw);
        let dividers = shapes
            .iter()
            .filter(|shape| {
                matches!(shape, egui::Shape::LineSegment { stroke, .. } if stroke.color == LIGHT.color.line && stroke.width == LIGHT.size.border)
            })
            .count();
        assert_eq!(dividers, 1, "two rows, one divider");
    }

    #[test]
    fn a_hint_that_wraps_grows_the_row_and_the_value_keeps_its_room() {
        let long = "A hint that is much too long for the room the text has next to the value, \
                    so it wraps onto a second line and then a third one as well.";
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            // A narrow card: 420 wide.
            ui.set_max_width(420.0);
            setting_card(ui, |rows| {
                rows.row("Name", Some(long), |ui| {
                    ui.label("value");
                });
                rows.row("Next", None, |ui| {
                    ui.label("next value");
                });
            });
        });
        let hint = bounds(&tree, long);
        let value = bounds(&tree, "value");
        let next = bounds(&tree, "next value");
        // Wrapped: more than one line high, and not under the value.
        assert!(hint.height() > 2.0 * LIGHT.text.caption.size);
        assert!(hint.right() <= value.left(), "{hint:?} {value:?}");
        // The next row is below the whole hint.
        assert!(next.center().y > hint.bottom());
    }

    #[test]
    fn a_click_on_the_name_is_the_rows_and_a_click_on_the_control_is_not() {
        let harness = Harness::new(egui::Theme::Light);
        let mut on = false;
        let draw = |ui: &mut Ui, on: &mut bool| -> (bool, bool) {
            let mut text = false;
            let mut control = false;
            setting_card(ui, |rows| {
                let row = rows.row("Start at login", Some(HINT), |ui| {
                    control = ui.add(Switch::new(*on, "Start at login").bare()).clicked();
                });
                text = row.clicked();
            });
            (text, control)
        };
        let tree = harness.accessibility(|ui| {
            draw(ui, &mut on);
        });
        let name = bounds(&tree, "Start at login").center();
        let mut got = (false, false);
        harness.click(name, |ui| got = draw(ui, &mut on));
        assert_eq!(got, (true, false));
        let switch = tree
            .node_with_label("Start at login")
            .and_then(|node| node.bounds());
        assert!(switch.is_some());
    }

    #[test]
    fn a_button_at_the_end_still_works_and_nothing_overlaps() {
        let harness = Harness::new(egui::Theme::Light);
        let draw = |ui: &mut Ui| {
            let mut pressed = false;
            setting_card(ui, |rows| {
                rows.row("Setup", Some(HINT), |ui| {
                    pressed = ui.add(Button::secondary("Run setup")).clicked();
                });
            });
            pressed
        };
        let tree = harness.accessibility(|ui| {
            draw(ui);
        });
        let button = bounds(&tree, "Run setup");
        let hint = bounds(&tree, HINT);
        assert!(hint.right() <= button.left());
        let mut pressed = false;
        harness.click(button.center(), |ui| pressed = draw(ui));
        assert!(pressed);
    }
}
