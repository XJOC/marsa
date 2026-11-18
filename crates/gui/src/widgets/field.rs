//! The text field (components.md, Inputs): 32 high, `surface` fill, a 1 px
//! `control` border, `radius.md`, the placeholder in `mute`. A search field
//! starts with a magnifier and may show a key hint at its end. An error shows
//! under the field in `error`, with an icon, and the border turns `error`.
//!
//! The text is the caller's `&mut String`, as with egui's own `TextEdit`:
//! `Response::changed` tells that it was edited, and `lost_focus` with Enter
//! that it was submitted.

use eframe::egui::{
    Align, Align2, Color32, CornerRadius, Frame, Id, Margin, Rect, Response, RichText, Role, Sense,
    Shape, Stroke, StrokeKind, TextEdit, Ui, Widget, pos2, vec2,
};

use super::icons::{Glyph, Icon};
use super::{Look, layout, paint_focus_ring, slot};
use crate::theme::{Direction, Theme};

/// The border and the fill of a field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    pub fill: Color32,
    pub border: Color32,
    /// The placeholder, the magnifier, and the key hint.
    pub quiet: Color32,
    pub text: Color32,
}

pub const fn colors(theme: &Theme, error: bool) -> Colors {
    let c = &theme.color;
    Colors {
        fill: c.surface,
        border: if error { c.error } else { c.control },
        quiet: c.mute,
        text: c.ink,
    }
}

/// A single-line text field.
#[must_use = "a field is drawn only when it is added to a Ui"]
pub struct Field<'a> {
    text: &'a mut String,
    /// The name for a screen reader: "Find a site".
    label: &'a str,
    hint: &'a str,
    search: bool,
    key_hint: Option<&'a str>,
    error: Option<&'a str>,
    disabled: Option<&'a str>,
    width: Option<f32>,
    look: Option<Look>,
    id: Option<Id>,
}

impl<'a> Field<'a> {
    pub const fn new(text: &'a mut String, label: &'a str) -> Self {
        Self {
            text,
            label,
            hint: "",
            search: false,
            key_hint: None,
            error: None,
            disabled: None,
            width: None,
            look: None,
            id: None,
        }
    }

    /// A search field: it starts with a magnifier.
    pub const fn search(text: &'a mut String, label: &'a str) -> Self {
        let mut field = Self::new(text, label);
        field.search = true;
        field
    }

    /// The placeholder, shown in `mute` while the field is empty.
    pub const fn hint(mut self, hint: &'a str) -> Self {
        self.hint = hint;
        self
    }

    /// The shortcut that focuses the field, shown at its end ("Cmd K").
    pub const fn key_hint(mut self, key_hint: &'a str) -> Self {
        self.key_hint = Some(key_hint);
        self
    }

    /// Shows `error` under the field, and turns the border `error`.
    pub const fn error(mut self, error: &'a str) -> Self {
        self.error = Some(error);
        self
    }

    /// [`Field::error`] when there is one.
    pub const fn error_if(mut self, error: Option<&'a str>) -> Self {
        self.error = error;
        self
    }

    /// Disables the field. Hovering it shows `reason`.
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }

    /// The width, in points; the available width when not set.
    pub const fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Draws the field as it looks in `look` (the gallery and the tests):
    /// the ring shows when `look.focused`.
    pub const fn looks(mut self, look: Look) -> Self {
        self.look = Some(look);
        self
    }

    /// The id of the text edit, so a screen can move the focus to the field
    /// (`Memory::request_focus`), for a shortcut such as Cmd K.
    pub const fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }

    /// The role a screen reader hears.
    pub const fn role(&self) -> Role {
        if self.search {
            Role::SearchInput
        } else {
            Role::TextInput
        }
    }

    /// The tooltip: the reason when disabled.
    pub const fn tooltip(&self) -> Option<&'a str> {
        self.disabled
    }

    fn show(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let paint = colors(theme, self.error.is_some());
        let pad = theme.space.s3;
        let icon = theme.size.icon_inline;
        let gap = theme.space.s2;
        let width = self.width.unwrap_or_else(|| ui.available_width());
        let (rect, _) = ui.allocate_exact_size(vec2(width, theme.size.control), Sense::hover());
        // The field's frame is painted after the text edit knows if it has
        // the focus; this slot keeps it behind the text.
        let behind = ui.painter().add(Shape::Noop);

        let inner = rect.shrink2(vec2(pad, 0.0));
        let key_hint = self
            .key_hint
            .map(|text| layout(ui.painter(), text, theme.text.caption, Color32::PLACEHOLDER));
        let hint_width = key_hint
            .as_ref()
            .map_or(0.0, |galley| galley.size().x + gap);
        let icon_width = if self.search { icon + gap } else { 0.0 };
        let edit_width = (inner.width() - icon_width - hint_width).max(0.0);
        let edit_x = slot(direction, inner, icon_width, edit_width);
        let edit_rect =
            Rect::from_min_size(pos2(edit_x, rect.top()), vec2(edit_width, rect.height()));

        let enabled = ui.is_enabled();
        let mut text_edit = TextEdit::singleline(self.text);
        if let Some(id) = self.id {
            text_edit = text_edit.id(id);
        }
        let edit = ui.put(
            edit_rect,
            text_edit
                .frame(Frame::NONE)
                .margin(Margin::ZERO)
                .font(theme.text.body_sm.font_id())
                .text_color(paint.text)
                .hint_text(RichText::new(self.hint).color(paint.quiet))
                .desired_width(edit_width)
                .min_size(edit_rect.size())
                .align(Align2([Align::Min, Align::Center])),
        );
        let focused = match self.look {
            Some(look) => look.focused,
            None => enabled && edit.has_focus(),
        };

        let corner = theme.radius.md;
        let painter = ui.painter();
        painter.set(
            behind,
            Shape::Vec(vec![
                Shape::rect_filled(rect, CornerRadius::same(corner), paint.fill),
                Shape::rect_stroke(
                    rect,
                    CornerRadius::same(corner),
                    Stroke::new(theme.size.border, paint.border),
                    StrokeKind::Inside,
                ),
            ]),
        );
        if self.search {
            let x = slot(direction, inner, 0.0, icon);
            let spot = Rect::from_min_size(pos2(x, rect.top()), vec2(icon, rect.height()));
            Icon::Search.paint(painter, theme, spot, paint.quiet);
        }
        if let Some(galley) = key_hint {
            let x = slot(
                direction,
                inner,
                inner.width() - galley.size().x,
                galley.size().x,
            );
            let spot =
                Rect::from_min_size(pos2(x, rect.top()), vec2(galley.size().x, rect.height()));
            let at = Align2::LEFT_CENTER.align_size_within_rect(galley.size(), spot);
            painter.galley(at.min, galley, paint.quiet);
        }
        if focused {
            paint_focus_ring(painter, theme, rect, corner);
        }

        // The text edit reports itself as a text input; it gets the field's
        // name, and a search field its own role.
        let (label, role) = (self.label, self.role());
        ui.ctx().accesskit_node_builder(edit.id, |node| {
            node.set_label(label);
            node.set_role(role);
        });

        if let Some(error) = self.error {
            ui.add_space(theme.space.s1);
            ui.horizontal(|ui| {
                ui.add(
                    Glyph::new(Icon::Alert)
                        .size(theme.size.icon_inline)
                        .color(theme.color.error),
                );
                ui.label(
                    RichText::new(error)
                        .font(theme.text.caption.font_id())
                        .color(theme.color.error),
                );
            });
        }
        // The response is the text edit's (`changed`, `lost_focus`), over the
        // whole field.
        edit.with_new_rect(rect)
    }
}

impl Widget for Field<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let reason = self.disabled;
        if reason.is_none() {
            return ui.vertical(|ui| self.show(ui)).inner;
        }
        let response = ui
            .add_enabled_ui(false, |ui| ui.vertical(|ui| self.show(ui)).inner)
            .inner;
        super::with_disabled_reason(response, reason)
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui;

    use super::*;
    use crate::theme::tests::{TEXT, UI, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::Harness;

    #[test]
    fn a_field_is_surface_with_a_control_border_and_an_error_border_when_wrong() {
        for theme in [&LIGHT, &DARK] {
            let ok = colors(theme, false);
            assert_eq!(ok.fill, theme.color.surface);
            assert_eq!(ok.border, theme.color.control);
            assert_eq!(ok.quiet, theme.color.mute);
            assert_eq!(ok.text, theme.color.ink);
            assert_eq!(colors(theme, true).border, theme.color.error);
        }
    }

    #[test]
    fn the_text_the_placeholder_and_the_border_read_on_the_fill_in_both_themes() {
        for theme in [&LIGHT, &DARK] {
            for error in [false, true] {
                let paint = colors(theme, error);
                assert!(contrast(paint.text, paint.fill) >= TEXT);
                assert!(contrast(paint.quiet, paint.fill) >= TEXT);
                assert!(contrast(paint.border, paint.fill) >= UI);
                assert!(contrast(paint.border, theme.color.bg) >= UI);
            }
        }
    }

    #[test]
    fn a_search_field_is_a_search_input_and_the_others_a_text_input() {
        let mut text = String::new();
        assert_eq!(Field::new(&mut text, "Name").role(), Role::TextInput);
        let mut text = String::new();
        assert_eq!(Field::search(&mut text, "Find").role(), Role::SearchInput);
    }

    #[test]
    fn a_field_is_32_high_and_as_wide_as_it_is_told() {
        let harness = Harness::new(egui::Theme::Light);
        let mut text = String::new();
        let mut sizes = Vec::new();
        harness.run(|ui| {
            sizes.push(
                ui.add(Field::new(&mut text, "Name").width(180.0))
                    .rect
                    .size(),
            );
            sizes.push(ui.add(Field::new(&mut text, "Name")).rect.size());
        });
        assert_eq!(sizes[0].x, 180.0);
        assert_eq!(sizes[0].y, LIGHT.size.control);
        assert!(
            sizes[1].x > sizes[0].x,
            "no width takes the available width"
        );
    }

    #[test]
    fn a_field_with_an_id_takes_the_focus_when_a_screen_asks_for_it() {
        let harness = Harness::new(egui::Theme::Light);
        let id = Id::unique("search_test");
        let mut text = String::new();
        let mut draw = |ui: &mut Ui| {
            ui.add(Field::search(&mut text, "Find").id(id));
        };
        harness.run(&mut draw);
        assert!(!harness.ctx.memory(|memory| memory.has_focus(id)));
        harness.ctx.memory_mut(|memory| memory.request_focus(id));
        harness.run(&mut draw);
        assert!(harness.ctx.memory(|memory| memory.has_focus(id)));
    }

    #[test]
    fn an_error_adds_a_line_under_the_field() {
        let harness = Harness::new(egui::Theme::Light);
        let mut text = String::new();
        let mut bottoms = Vec::new();
        for error in [None, Some("Use letters and dashes.")] {
            harness.run(|ui| {
                ui.add(Field::new(&mut text, "Name").width(180.0).error_if(error));
                bottoms.push(ui.min_rect().bottom());
            });
        }
        // The line is a caption high, and a gap under the field.
        assert!(bottoms[1] >= bottoms[0] + LIGHT.text.caption.size + LIGHT.space.s1);
    }

    #[test]
    fn a_screen_reader_hears_the_field_by_its_name_and_role() {
        let harness = Harness::new(egui::Theme::Light);
        let mut name = String::new();
        let mut search = String::from("sho");
        let tree = harness.accessibility(|ui| {
            ui.add(Field::new(&mut name, "Site name").hint("shop"));
            ui.add(Field::search(&mut search, "Find a site"));
        });
        assert_eq!(
            tree.node_with_label("Site name").expect("name").role(),
            Role::TextInput
        );
        assert_eq!(
            tree.node_with_label("Find a site").expect("search").role(),
            Role::SearchInput
        );
    }

    #[test]
    fn typing_edits_the_text_the_caller_owns() {
        let harness = Harness::new(egui::Theme::Light);
        let mut text = String::new();
        let mut rect = Rect::NOTHING;
        harness.run(|ui| rect = ui.add(Field::new(&mut text, "Name").width(180.0)).rect);
        // Click into the field, then type.
        harness.click(rect.center(), |ui| {
            ui.add(Field::new(&mut text, "Name").width(180.0));
        });
        let input = egui::RawInput {
            events: vec![egui::Event::Text("shop".into())],
            ..egui::RawInput::default()
        };
        harness.frame(input, |ui| {
            ui.add(Field::new(&mut text, "Name").width(180.0));
        });
        assert_eq!(text, "shop");
    }

    #[test]
    fn a_disabled_field_says_why() {
        let mut text = String::new();
        assert_eq!(
            Field::new(&mut text, "Name")
                .disabled("Set up first")
                .tooltip(),
            Some("Set up first")
        );
        let mut text = String::new();
        assert_eq!(Field::new(&mut text, "Name").tooltip(), None);
    }
}
