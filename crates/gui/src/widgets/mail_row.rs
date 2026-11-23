//! A mail in the Mail list (layout.md, Mail screen): two lines in a row
//! `size.row` 54 high. The sender, and at the end how long ago it came, in
//! `caption`; the subject in `body.sm` and `ink2`, and at the end, while the
//! mail is new, a dot and the word "New" in the mail color (never the dot
//! alone, R8). A new mail's sender is at 500. The rows touch, with 1 px of
//! `line` under each; the chosen one has the `selected` fill, and one under
//! the pointer has it at 50% (components.md, Tables and lists).
//!
//! A row is a choice, like a [`super::ListItem`]: a screen reader hears an
//! option that is selected or not, named by its sender, with "New", the
//! subject, and the age as its description. The focus ring is drawn inside
//! the row, because the list's card cuts what lies outside it.

use eframe::egui::text::{LayoutJob, TextWrapping};
use eframe::egui::{
    Align2, Color32, CornerRadius, Rect, Response, Role, Sense, Ui, Vec2, Widget, WidgetInfo, pos2,
    vec2,
};

use super::{Look, Touch, layout, paint_focus_ring, quiet_fill, slot};
use crate::strings;
use crate::theme::{Direction, Face, Section, TextToken, Theme};

/// The fill of a row: `selected` when chosen, else the quiet fill of a
/// pointer that touches it.
pub fn fill(theme: &Theme, selected: bool, touch: Touch) -> Option<Color32> {
    if selected {
        Some(theme.color.selected)
    } else {
        quiet_fill(theme, touch)
    }
}

/// One mail of the list. `Response::clicked` tells that the user opened it.
#[derive(Clone, Copy, Debug)]
#[must_use = "a mail row is drawn only when it is added to a Ui"]
pub struct MailRow<'a> {
    from: &'a str,
    subject: &'a str,
    age: &'a str,
    new: bool,
    selected: bool,
    look: Option<Look>,
}

impl<'a> MailRow<'a> {
    /// A mail from `from`, about `subject`, that came `age` ago ("4 min
    /// ago").
    pub const fn new(from: &'a str, subject: &'a str, age: &'a str) -> Self {
        Self {
            from,
            subject,
            age,
            new: false,
            selected: false,
            look: None,
        }
    }

    /// The mail is not opened yet.
    pub const fn new_mail(mut self, new: bool) -> Self {
        self.new = new;
        self
    }

    /// The mail the detail shows.
    pub const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Draws the row as it looks in `look` (the tests).
    pub const fn looks(mut self, look: Look) -> Self {
        self.look = Some(look);
        self
    }
}

impl Widget for MailRow<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let height = theme.size.row_max;
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
        if ui.is_rect_visible(rect) {
            let look = self.look.unwrap_or_else(|| Look::of(&response));
            let painter = ui.painter();
            if let Some(fill) = fill(theme, self.selected, look.touch) {
                painter.rect_filled(rect, CornerRadius::ZERO, fill);
            }
            // The line under the row, inside it.
            let under = Rect::from_min_max(
                pos2(rect.left(), rect.bottom() - theme.size.border),
                rect.right_bottom(),
            );
            painter.rect_filled(under, CornerRadius::ZERO, theme.color.line);
            let pad = theme.space.s4;
            let gap = theme.space.s3;
            let mail = theme.section(Section::Mail).fg;
            // The end column first: the age, and "New" with its dot; the
            // sender and the subject take what is left, cut short.
            let age = layout(painter, self.age, theme.text.caption, Color32::PLACEHOLDER);
            let new_token = TextToken {
                size: theme.text.caption.size,
                face: Face::SemiBold,
            };
            let new = self
                .new
                .then(|| layout(painter, strings::MAIL_NEW, new_token, Color32::PLACEHOLDER));
            let dot = theme.size.dot;
            let new_width = new
                .as_ref()
                .map_or(0.0, |word| dot + theme.space.s1 + word.size().x);
            let end_width = age.size().x.max(new_width);
            let room = (rect.width() - pad - gap - end_width - pad).max(0.0);
            let from_token = TextToken {
                size: theme.text.body.size,
                face: if self.new {
                    Face::Medium
                } else {
                    Face::Regular
                },
            };
            let cut = |text: &str, token: TextToken| {
                let mut job = LayoutJob::simple_singleline(
                    text.to_owned(),
                    token.font_id(),
                    Color32::PLACEHOLDER,
                );
                job.wrap = TextWrapping::truncate_at_width(room);
                painter.layout_job(job)
            };
            let from = cut(self.from, from_token);
            let subject = cut(self.subject, theme.text.body_sm);
            // Two lines, centered on the row as one block; each text is
            // centered on its line.
            let first = from.size().y.max(age.size().y);
            let second = subject
                .size()
                .y
                .max(new.as_ref().map_or(0.0, |word| word.size().y));
            let top = f32::midpoint(rect.top(), rect.bottom() - first - second);
            let below = top + first;
            let start = |width: f32| slot(direction, rect, pad, width);
            let end = |width: f32| slot(direction, rect, rect.width() - pad - width, width);
            let on = |line_top: f32, line: f32, size: f32| {
                f32::midpoint(line_top, line_top + line - size)
            };
            let (from_size, age_size) = (from.size(), age.size());
            let from_at = pos2(start(from_size.x), on(top, first, from_size.y));
            painter.galley(from_at, from, theme.color.ink);
            let age_at = pos2(end(age_size.x), on(top, first, age_size.y));
            painter.galley(age_at, age, theme.color.mute);
            let subject_size = subject.size();
            let subject_at = pos2(start(subject_size.x), on(below, second, subject_size.y));
            painter.galley(subject_at, subject, theme.color.ink2);
            if let Some(word) = new {
                // The dot, then the word, in reading order.
                let x = end(new_width);
                let (dot_x, word_x) = if direction.is_rtl() {
                    (x + new_width - dot, x)
                } else {
                    (x, x + dot + theme.space.s1)
                };
                let spot = Rect::from_min_size(pos2(dot_x, below), vec2(dot, second));
                let circle = Align2::CENTER_CENTER.align_size_within_rect(Vec2::splat(dot), spot);
                // The full radius makes the square a circle.
                painter.rect_filled(circle, CornerRadius::same(theme.radius.full), mail);
                let word_at = pos2(word_x, on(below, second, word.size().y));
                painter.galley(word_at, word, mail);
            }
            if look.focused {
                let inset = theme.focus.offset + theme.focus.width;
                paint_focus_ring(painter, theme, rect.shrink(inset), theme.radius.sm);
            }
        }
        let (from, selected, enabled) = (self.from, self.selected, ui.is_enabled());
        let description = strings::mail_row_description(self.subject, self.age, self.new);
        response.widget_info(|| WidgetInfo::labeled(Role::ListBoxOption, enabled, from));
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_selected(selected);
            node.set_description(description);
        });
        response
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Shape, StrokeKind};

    use super::*;
    use crate::theme::tests::{TEXT, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::Harness;

    fn reset() -> MailRow<'static> {
        MailRow::new(
            "Laravel <hello@example.test>",
            "Reset Password Notification",
            "just now",
        )
    }

    /// The words one frame paints, in order.
    fn words(harness: &Harness, row: MailRow<'static>) -> Vec<String> {
        harness
            .shapes(|ui| {
                ui.add(row);
            })
            .into_iter()
            .filter_map(|shape| match shape {
                Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_row_is_as_wide_as_the_list_and_as_high_as_a_two_line_row() {
        let harness = Harness::new(egui::Theme::Light);
        let mut size = egui::Vec2::ZERO;
        harness.run(|ui| {
            ui.set_width(340.0);
            size = ui.add(reset()).rect.size();
        });
        assert_eq!(size, vec2(340.0, LIGHT.size.row_max));
        assert_eq!(LIGHT.size.row_max, 54.0);
    }

    #[test]
    fn a_new_mail_says_new_in_a_word_and_a_read_one_does_not() {
        let harness = Harness::new(egui::Theme::Light);
        let new = words(&harness, reset().new_mail(true));
        assert!(new.iter().any(|word| word == strings::MAIL_NEW), "{new:?}");
        let read = words(&harness, reset());
        assert!(
            !read.iter().any(|word| word == strings::MAIL_NEW),
            "{read:?}"
        );
        // The sender and the age are on both.
        for shown in [&new, &read] {
            assert!(shown.iter().any(|word| word == "just now"));
            assert!(
                shown
                    .iter()
                    .any(|word| word == "Reset Password Notification")
            );
        }
    }

    #[test]
    fn the_mail_color_reads_on_every_fill_a_row_has() {
        for theme in [&LIGHT, &DARK] {
            let mail = theme.section(Section::Mail).fg;
            for ground in [theme.color.surface, theme.color.selected] {
                assert!(contrast(mail, ground) >= TEXT, "{}", theme.dark);
                assert!(contrast(theme.color.ink2, ground) >= TEXT);
                assert!(contrast(theme.color.mute, ground) >= TEXT);
            }
        }
    }

    #[test]
    fn the_chosen_row_is_selected_and_a_focused_one_has_the_ring_inside() {
        for theme in [&LIGHT, &DARK] {
            assert_eq!(fill(theme, true, Touch::Rest), Some(theme.color.selected));
            assert_eq!(fill(theme, false, Touch::Rest), None);
            assert_eq!(
                fill(theme, false, Touch::Hover),
                Some(theme.color.selected.gamma_multiply(theme.state.row_hover))
            );
        }
        let harness = Harness::new(egui::Theme::Light);
        let mut row = egui::Rect::NOTHING;
        let shapes = harness.shapes(|ui| {
            row = ui.add(reset().looks(Look::focused())).rect;
        });
        let ring = shapes.iter().find_map(|shape| match shape {
            Shape::Rect(rect)
                if rect.stroke_kind == StrokeKind::Outside
                    && rect.stroke.color == LIGHT.color.accent =>
            {
                // The stroke lies outside the shape's rect.
                Some(rect.rect.expand(LIGHT.focus.width))
            }
            _ => None,
        });
        let ring = ring.expect("a focus ring");
        assert!(row.contains_rect(ring), "{ring:?} in {row:?}");
    }

    #[test]
    fn a_screen_reader_hears_an_option_named_by_its_sender() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            ui.add(reset().new_mail(true).selected(true));
        });
        let node = tree
            .node_with_label("Laravel <hello@example.test>")
            .expect("the row");
        assert_eq!(node.role(), Role::ListBoxOption);
        assert_eq!(node.is_selected(), Some(true));
        assert_eq!(
            node.description(),
            Some("New, Reset Password Notification, just now")
        );
    }

    #[test]
    fn a_long_sender_is_cut_short_and_the_row_keeps_its_height() {
        let harness = Harness::new(egui::Theme::Light);
        let mut size = egui::Vec2::ZERO;
        harness.run(|ui| {
            ui.set_width(200.0);
            let long = "A sender with a very long name <someone.with.a.long.address@example.test>";
            size = ui
                .add(MailRow::new(long, long, "3 d ago").new_mail(true))
                .rect
                .size();
        });
        assert_eq!(size, vec2(200.0, LIGHT.size.row_max));
    }
}
