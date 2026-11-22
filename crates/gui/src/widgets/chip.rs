//! Chips (components.md, Chips): a button that is on or off. The filters of
//! Logs are chips with a small square of the level's color before the word,
//! and Follow is a chip with a check before the word while it is on. On is
//! the `selected` fill with an `ink` word; off is the `surface` fill with a
//! `mute` word. The state is never the fill alone (R8): the square is filled
//! or hollow, the check is there or not, and the word is `ink` or `mute`.
//! There is no accent chip: a view has at most one primary button.
//!
//! [`Chip`] draws the state it is given and reports the click; the screen
//! turns the click into an action that flips the state. A screen reader
//! hears a button that is toggled or not.

use eframe::egui::{
    Align2, Color32, CornerRadius, Rect, Response, Role, Sense, Stroke, StrokeKind, Ui, Widget,
    WidgetInfo, vec2,
};

use super::icons::Icon;
use super::{Look, Touch, layout, paint_focus_ring, slot, spot, touched};
use crate::theme::{Direction, Theme};

/// What a chip is made of in one state, before it is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    pub fill: Color32,
    pub border: Color32,
    pub text: Color32,
}

/// The colors of a chip while `touch` happens to it.
pub fn colors(theme: &Theme, on: bool, touch: Touch) -> Colors {
    let c = &theme.color;
    if on {
        Colors {
            fill: touched(theme, c.selected, touch),
            border: c.control,
            text: c.ink,
        }
    } else {
        Colors {
            fill: touched(theme, c.surface, touch),
            border: c.control,
            text: c.mute,
        }
    }
}

/// What stands before the word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Nothing,
    /// A small square of this color: filled while the chip is on, a hollow
    /// outline while it is off.
    Square(Color32),
    /// A check while the chip is on, and its room while it is off, so the
    /// chip keeps its width.
    Check,
}

/// A chip. Build it with [`Chip::new`], then add it:
/// `ui.add(Chip::new("Error", true).square(color))`. `Response::clicked`
/// tells that the user pressed it.
#[derive(Clone, Copy, Debug)]
#[must_use = "a chip is drawn only when it is added to a Ui"]
pub struct Chip<'a> {
    label: &'a str,
    on: bool,
    mark: Mark,
    hint: Option<&'a str>,
    look: Option<Look>,
}

impl<'a> Chip<'a> {
    pub const fn new(label: &'a str, on: bool) -> Self {
        Self {
            label,
            on,
            mark: Mark::Nothing,
            hint: None,
            look: None,
        }
    }

    /// A small square of `color` before the word.
    pub const fn square(mut self, color: Color32) -> Self {
        self.mark = Mark::Square(color);
        self
    }

    /// A check before the word while the chip is on.
    pub const fn check(mut self) -> Self {
        self.mark = Mark::Check;
        self
    }

    /// Shown while the pointer rests on the chip.
    pub const fn hint(mut self, hint: &'a str) -> Self {
        self.hint = Some(hint);
        self
    }

    /// Draws the chip as it looks in `look`, whatever the pointer and the
    /// keyboard do: for the design gallery and the tests.
    pub const fn looks(mut self, look: Look) -> Self {
        self.look = Some(look);
        self
    }

    /// 32 high (`size.control`), like a button.
    pub const fn height(theme: &Theme) -> f32 {
        theme.size.control
    }
}

impl Widget for Chip<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let height = Self::height(theme);
        let galley = layout(
            ui.painter(),
            self.label,
            theme.text.body_sm,
            Color32::PLACEHOLDER,
        );
        // The room of the mark, and the gap after it.
        let mark_width = match self.mark {
            Mark::Nothing => 0.0,
            Mark::Square(_) => theme.size.dot,
            Mark::Check => theme.size.icon_inline,
        };
        let before = if mark_width > 0.0 {
            mark_width + theme.space.s2
        } else {
            0.0
        };
        let pad = theme.space.s3;
        let content = before + galley.size().x;
        let (rect, response) =
            ui.allocate_exact_size(vec2(content + pad + pad, height), Sense::click());
        if ui.is_rect_visible(rect) {
            let look = self.look.unwrap_or_else(|| Look::of(&response));
            let paint = colors(theme, self.on, look.touch);
            let painter = ui.painter();
            let corner = CornerRadius::same(theme.radius.md);
            painter.rect_filled(rect, corner, paint.fill);
            let stroke = Stroke::new(theme.size.border, paint.border);
            painter.rect_stroke(rect, corner, stroke, StrokeKind::Inside);
            let inside = Rect::from_center_size(rect.center(), vec2(content, height));
            let mark_spot = |width: f32| {
                let x = slot(direction, inside, 0.0, width);
                spot(x, rect.top(), width, height)
            };
            match self.mark {
                Mark::Nothing => {}
                Mark::Square(color) => {
                    let size = vec2(theme.size.dot, theme.size.dot);
                    let square =
                        Align2::CENTER_CENTER.align_size_within_rect(size, mark_spot(size.x));
                    let corner = CornerRadius::same(theme.radius.chip_dot);
                    if self.on {
                        painter.rect_filled(square, corner, color);
                    } else {
                        let stroke = Stroke::new(theme.size.state_ring, theme.color.mute);
                        painter.rect_stroke(square, corner, stroke, StrokeKind::Inside);
                    }
                }
                Mark::Check => {
                    if self.on {
                        Icon::Check.paint(painter, theme, mark_spot(mark_width), paint.text);
                    }
                }
            }
            let x = slot(direction, inside, before, galley.size().x);
            let text_spot = spot(x, rect.top(), galley.size().x, height);
            let at = Align2::LEFT_CENTER.align_size_within_rect(galley.size(), text_spot);
            painter.galley(at.min, galley, paint.text);
            if look.focused {
                paint_focus_ring(painter, theme, rect, theme.radius.md);
            }
        }
        let (enabled, on, label) = (ui.is_enabled(), self.on, self.label);
        // A button with a toggled state is a toggle button for a screen reader.
        response.widget_info(|| WidgetInfo::selected(Role::Button, enabled, on, label));
        match self.hint {
            Some(hint) => response.on_hover_text(hint),
            None => response,
        }
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Shape};

    use super::*;
    use crate::logline::LogLevel;
    use crate::theme::tests::{TEXT, UI, contrast};
    use crate::theme::{DARK, LIGHT, Section};
    use crate::widgets::harness::Harness;
    use crate::widgets::log_entry::level_color;

    const TOUCHES: [Touch; 3] = [Touch::Rest, Touch::Hover, Touch::Pressed];
    const THEMES: [(&str, &Theme); 2] = [("light", &LIGHT), ("dark", &DARK)];

    #[test]
    fn on_is_the_selected_fill_with_ink_and_off_is_the_surface_fill_with_mute() {
        for (name, theme) in THEMES {
            let c = &theme.color;
            let on = colors(theme, true, Touch::Rest);
            assert_eq!(
                (on.fill, on.border, on.text),
                (c.selected, c.control, c.ink),
                "{name}"
            );
            let off = colors(theme, false, Touch::Rest);
            assert_eq!(
                (off.fill, off.border, off.text),
                (c.surface, c.control, c.mute),
                "{name}"
            );
            // The two fills are not the same, so the chip says its state at a glance.
            assert_ne!(on.fill, off.fill, "{name}");
        }
    }

    #[test]
    fn the_word_the_border_and_the_marks_read_in_both_states_in_both_themes() {
        for (name, theme) in THEMES {
            let c = &theme.color;
            for on in [true, false] {
                for touch in TOUCHES {
                    let paint = colors(theme, on, touch);
                    let ratio = contrast(paint.text, paint.fill);
                    assert!(ratio >= TEXT, "{name} on={on} {touch:?}: word {ratio:.2}:1");
                    assert!(
                        contrast(paint.border, c.bg) >= UI,
                        "{name} on={on} {touch:?}: border"
                    );
                }
                // The hollow square of an off chip, on its surface.
                if !on {
                    assert!(contrast(c.mute, c.surface) >= UI, "{name} hollow square");
                }
            }
            // The check (in the word's color) and the filled squares stand out
            // from the selected fill they are drawn on.
            assert!(contrast(c.ink, c.selected) >= UI, "{name} check");
            for level in LogLevel::ALL {
                let ratio = contrast(level_color(theme, level), c.selected);
                assert!(ratio >= UI, "{name} {level:?} square {ratio:.2}:1");
            }
        }
    }

    #[test]
    fn a_chip_is_as_high_as_a_control_and_as_wide_as_its_word_and_its_mark() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let mut sizes = Vec::new();
        harness.run(|ui| {
            sizes.push(ui.add(Chip::new("Error", true)).rect.size());
            sizes.push(
                ui.add(Chip::new("Error", true).square(theme.color.error))
                    .rect
                    .size(),
            );
            sizes.push(ui.add(Chip::new("Follow", true).check()).rect.size());
            sizes.push(ui.add(Chip::new("Follow", false).check()).rect.size());
        });
        assert!(sizes.iter().all(|size| size.y == theme.size.control));
        // The square and the gap after it.
        let extra = sizes[1].x - sizes[0].x;
        assert!(
            (extra - (theme.size.dot + theme.space.s2)).abs() < 0.5,
            "{extra}"
        );
        // The check has its room while off: the chip does not change its width.
        assert_eq!(sizes[2], sizes[3]);
    }

    #[test]
    fn the_square_is_filled_while_on_and_hollow_while_off() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let amber = theme.section(Section::Dumps).fg;
        let squares = |on: bool| {
            harness
                .shapes(|ui| {
                    ui.add(Chip::new("Warning", on).square(amber));
                })
                .into_iter()
                .filter_map(|shape| match shape {
                    Shape::Rect(rect) if rect.rect.width() == theme.size.dot => Some(rect),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let on = squares(true);
        assert_eq!(on.len(), 1);
        assert_eq!(on[0].fill, amber);
        let off = squares(false);
        assert_eq!(off.len(), 1);
        // Hollow: no fill, an outline in `mute`.
        assert_eq!(off[0].fill, Color32::TRANSPARENT);
        assert_eq!(off[0].stroke.color, theme.color.mute);
        assert_eq!(
            on[0].corner_radius,
            CornerRadius::same(theme.radius.chip_dot)
        );
    }

    #[test]
    fn the_check_is_drawn_while_on_and_not_while_off_and_the_fill_follows_the_state() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let shapes = |on: bool| {
            harness.shapes(|ui| {
                ui.add(Chip::new("Follow", on).check());
            })
        };
        // The check is a path in the word's color; the label is a text.
        let strokes = |shapes: &[Shape]| {
            shapes
                .iter()
                .filter(|shape| matches!(shape, Shape::Path(path) if path.stroke.color == egui::epaint::ColorMode::Solid(theme.color.ink)))
                .count()
        };
        assert!(strokes(&shapes(true)) >= 1, "a check while on");
        assert_eq!(strokes(&shapes(false)), 0, "none while off");
        let fill = |on: bool| {
            shapes(on)
                .into_iter()
                .find_map(|shape| match shape {
                    Shape::Rect(rect) if rect.fill != Color32::TRANSPARENT => Some(rect.fill),
                    _ => None,
                })
                .expect("the chip's fill")
        };
        assert_eq!(fill(true), theme.color.selected);
        assert_eq!(fill(false), theme.color.surface);
    }

    #[test]
    fn a_screen_reader_hears_a_button_that_is_toggled_or_not() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            ui.add(Chip::new("Error", true));
            ui.add(Chip::new("Info", false));
            ui.add(Chip::new("Follow", true).check());
        });
        let error = tree.node_with_label("Error").expect("a chip");
        let info = tree.node_with_label("Info").expect("a chip");
        assert_eq!(error.role(), egui::Role::Button);
        assert_eq!(error.toggled(), Some(egui::accesskit::Toggled::True));
        assert_eq!(info.toggled(), Some(egui::accesskit::Toggled::False));
        assert_eq!(
            tree.node_with_label("Follow").expect("a chip").toggled(),
            Some(egui::accesskit::Toggled::True)
        );
    }

    #[test]
    fn a_click_is_reported_and_the_focus_ring_shows_when_focused() {
        let harness = Harness::new(egui::Theme::Light);
        let mut rect = egui::Rect::NOTHING;
        harness.run(|ui| rect = ui.add(Chip::new("Follow", true).check()).rect);
        let mut clicked = false;
        harness.click(rect.center(), |ui| {
            clicked |= ui.add(Chip::new("Follow", true).check()).clicked();
        });
        assert!(clicked);
        let ring = |look: Look| {
            harness
                .shapes(|ui| {
                    ui.add(Chip::new("Follow", true).looks(look));
                })
                .into_iter()
                .any(|shape| matches!(shape, Shape::Rect(r) if r.stroke_kind == StrokeKind::Outside && r.stroke.color == LIGHT.color.accent))
        };
        assert!(ring(Look::focused()));
        for look in [Look::rest(), Look::hover(), Look::pressed()] {
            assert!(!ring(look), "{look:?}");
        }
    }
}
