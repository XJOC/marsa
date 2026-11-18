//! Chips (components.md, Chips): a button that is on or off. The filters of
//! Logs are chips with a small square of the level's color before the word,
//! and Follow is a chip that turns `accent` while it is on. A chip that is
//! off shows it three ways besides its state for a screen reader (R8): the
//! square is hollow, the word is `mute`, and the fill is gone.
//!
//! [`Chip`] draws the state it is given and reports the click; the screen
//! turns the click into an action that flips the state.

use eframe::egui::{
    Align2, Color32, CornerRadius, Rect, Response, Role, Sense, Stroke, StrokeKind, Ui, Widget,
    WidgetInfo, pos2, vec2,
};

use super::{Look, Touch, layout, paint_focus_ring, quiet_fill, slot, touched};
use crate::theme::{Direction, Theme};

/// What a chip is made of in one state, before it is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    /// `None`: no fill.
    pub fill: Option<Color32>,
    pub border: Option<Color32>,
    pub text: Color32,
}

/// The colors of a chip while `touch` happens to it. `accent`: the chip is
/// the one main toggle of a row (Follow), and turns `accent` while it is on.
pub fn colors(theme: &Theme, on: bool, accent: bool, touch: Touch) -> Colors {
    let c = &theme.color;
    match (accent, on) {
        (true, true) => Colors {
            fill: Some(touched(theme, c.accent, touch)),
            border: None,
            text: c.on_accent,
        },
        // A chip that is on, and an accent chip that is off, look like a
        // secondary button.
        (_, true) | (true, false) => Colors {
            fill: Some(touched(theme, c.surface, touch)),
            border: Some(c.control),
            text: c.ink,
        },
        (false, false) => Colors {
            fill: quiet_fill(theme, touch),
            border: Some(c.control),
            text: c.mute,
        },
    }
}

/// A chip. Build it with [`Chip::new`], then add it:
/// `ui.add(Chip::new("Error", true).dot(color))`. `Response::clicked` tells
/// that the user pressed it.
#[derive(Clone, Copy, Debug)]
#[must_use = "a chip is drawn only when it is added to a Ui"]
pub struct Chip<'a> {
    label: &'a str,
    on: bool,
    dot: Option<Color32>,
    accent: bool,
    hint: Option<&'a str>,
    look: Option<Look>,
}

impl<'a> Chip<'a> {
    pub const fn new(label: &'a str, on: bool) -> Self {
        Self {
            label,
            on,
            dot: None,
            accent: false,
            hint: None,
            look: None,
        }
    }

    /// A small square of `color` before the word: filled while the chip is
    /// on, a hollow outline while it is off.
    pub const fn dot(mut self, color: Color32) -> Self {
        self.dot = Some(color);
        self
    }

    /// The chip turns `accent` while it is on.
    pub const fn accent(mut self) -> Self {
        self.accent = true;
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
        let dot = self.dot.map(|color| (color, theme.size.dot));
        let dot_width = dot.map_or(0.0, |(_, size)| size + theme.space.s2);
        let pad = theme.space.s3;
        let content = dot_width + galley.size().x;
        let (rect, response) =
            ui.allocate_exact_size(vec2(content + pad + pad, height), Sense::click());
        if ui.is_rect_visible(rect) {
            let look = self.look.unwrap_or_else(|| Look::of(&response));
            let paint = colors(theme, self.on, self.accent, look.touch);
            let painter = ui.painter();
            let corner = CornerRadius::same(theme.radius.md);
            if let Some(fill) = paint.fill {
                painter.rect_filled(rect, corner, fill);
            }
            if let Some(border) = paint.border {
                let stroke = Stroke::new(theme.size.border, border);
                painter.rect_stroke(rect, corner, stroke, StrokeKind::Inside);
            }
            let inside = Rect::from_center_size(rect.center(), vec2(content, height));
            if let Some((color, size)) = dot {
                let x = slot(direction, inside, 0.0, size);
                let spot = Rect::from_min_size(pos2(x, rect.top()), vec2(size, height));
                let square = Align2::CENTER_CENTER.align_size_within_rect(vec2(size, size), spot);
                let corner = CornerRadius::same(theme.radius.chip_dot);
                if self.on {
                    painter.rect_filled(square, corner, color);
                } else {
                    let stroke = Stroke::new(theme.size.state_ring, theme.color.mute);
                    painter.rect_stroke(square, corner, stroke, StrokeKind::Inside);
                }
            }
            let x = slot(direction, inside, dot_width, galley.size().x);
            let spot = Rect::from_min_size(pos2(x, rect.top()), vec2(galley.size().x, height));
            let at = Align2::LEFT_CENTER.align_size_within_rect(galley.size(), spot);
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
    use crate::theme::tests::{TEXT, UI, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::{Harness, over};

    const TOUCHES: [Touch; 3] = [Touch::Rest, Touch::Hover, Touch::Pressed];

    #[test]
    fn a_chip_that_is_on_is_a_surface_with_a_control_border_and_an_accent_one_is_accent() {
        for theme in [&LIGHT, &DARK] {
            let c = &theme.color;
            let on = colors(theme, true, false, Touch::Rest);
            assert_eq!(on.fill, Some(c.surface));
            assert_eq!((on.border, on.text), (Some(c.control), c.ink));
            let accent = colors(theme, true, true, Touch::Rest);
            assert_eq!(accent.fill, Some(c.accent));
            assert_eq!((accent.border, accent.text), (None, c.on_accent));
            // Off: no fill, the border stays, the word is quiet.
            let off = colors(theme, false, false, Touch::Rest);
            assert_eq!(off.fill, None);
            assert_eq!((off.border, off.text), (Some(c.control), c.mute));
            // An accent chip that is off is a secondary button.
            assert_eq!(colors(theme, false, true, Touch::Rest), on);
        }
    }

    #[test]
    fn every_state_of_every_chip_reads_in_both_themes() {
        for (name, theme) in [("light", &LIGHT), ("dark", &DARK)] {
            let c = &theme.color;
            for (on, accent) in [(true, false), (false, false), (true, true), (false, true)] {
                for touch in TOUCHES {
                    let paint = colors(theme, on, accent, touch);
                    let fill = paint.fill.map_or(c.bg, |fill| over(fill, c.bg));
                    let ratio = contrast(paint.text, fill);
                    assert!(
                        ratio >= TEXT,
                        "{name} on={on} accent={accent} {touch:?}: {ratio:.2}"
                    );
                    if let Some(border) = paint.border {
                        assert!(contrast(border, c.bg) >= UI, "{name} {touch:?} border");
                    }
                }
            }
        }
    }

    #[test]
    fn a_chip_is_as_high_as_a_control_and_as_wide_as_its_word_and_its_square() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let mut sizes = Vec::new();
        harness.run(|ui| {
            sizes.push(ui.add(Chip::new("Error", true)).rect.size());
            sizes.push(
                ui.add(Chip::new("Error", true).dot(theme.color.error))
                    .rect
                    .size(),
            );
        });
        assert_eq!(sizes[0].y, theme.size.control);
        assert_eq!(sizes[1].y, theme.size.control);
        // The square and the gap after it.
        let extra = sizes[1].x - sizes[0].x;
        assert!(
            (extra - (theme.size.dot + theme.space.s2)).abs() < 0.5,
            "{extra}"
        );
    }

    #[test]
    fn the_square_is_filled_while_on_and_hollow_while_off() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let squares = |on: bool| {
            harness
                .shapes(|ui| {
                    ui.add(
                        Chip::new("Warning", on)
                            .dot(theme.section(crate::theme::Section::Dumps).fg),
                    );
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
        assert_eq!(on[0].fill, theme.section(crate::theme::Section::Dumps).fg);
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
    fn a_screen_reader_hears_a_button_that_is_toggled_or_not() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            ui.add(Chip::new("Error", true));
            ui.add(Chip::new("Info", false));
        });
        let error = tree.node_with_label("Error").expect("a chip");
        let info = tree.node_with_label("Info").expect("a chip");
        assert_eq!(error.role(), egui::Role::Button);
        assert_eq!(error.toggled(), Some(egui::accesskit::Toggled::True));
        assert_eq!(info.toggled(), Some(egui::accesskit::Toggled::False));
    }

    #[test]
    fn a_click_is_reported_and_the_focus_ring_shows_when_focused() {
        let harness = Harness::new(egui::Theme::Light);
        let mut rect = egui::Rect::NOTHING;
        harness.run(|ui| rect = ui.add(Chip::new("Follow", true).accent()).rect);
        let mut clicked = false;
        harness.click(rect.center(), |ui| {
            clicked |= ui.add(Chip::new("Follow", true).accent()).clicked();
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
