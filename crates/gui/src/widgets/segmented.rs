//! The segmented control (components.md): two to four options in a track
//! tinted with `selected`; the chosen option is a raised `surface` pill. It
//! is a radio group: one option is chosen, and the arrow keys move the choice
//! and the focus with it. It is one Tab stop (the WAI-ARIA "roving focus"):
//! Tab enters on the chosen option and the next Tab leaves. Space and Enter
//! choose the focused option. Used for Appearance and for the log source.
//!
//! The control draws the option it is given and returns the option the user
//! chose, like the other widgets: the screen turns that into an action.

use eframe::egui::{
    Align2, Color32, CornerRadius, InnerResponse, Rect, Response, Role, Sense, Stroke, StrokeKind,
    Ui, Widget, WidgetInfo, pos2, vec2,
};

use super::keys::roving_keys;
use super::{Look, Touch, group_button, layout, paint_focus_ring, slot, tab_stop, touched};
use crate::theme::{Direction, Face, TextToken, Theme};

/// The most and the fewest options a control has (components.md).
pub const OPTIONS: std::ops::RangeInclusive<usize> = 2..=4;

/// What one option is made of in one state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OptionColors {
    /// `None`: the track shows through.
    pub fill: Option<Color32>,
    /// The raised pill has a 1 px border; the others none.
    pub border: Option<Color32>,
    pub text: Color32,
    pub face: Face,
}

/// The track behind the options.
pub const fn track(theme: &Theme) -> Color32 {
    theme.color.selected
}

/// An option, chosen or not, while `touch` happens to it.
pub fn option_colors(theme: &Theme, chosen: bool, touch: Touch) -> OptionColors {
    let c = &theme.color;
    if chosen {
        OptionColors {
            fill: Some(touched(theme, c.surface, touch)),
            border: Some(c.line),
            text: c.ink,
            face: Face::Medium,
        }
    } else {
        OptionColors {
            fill: match touch {
                Touch::Rest => None,
                touch => Some(touched(theme, track(theme), touch)),
            },
            border: None,
            text: c.ink,
            face: Face::Regular,
        }
    }
}

/// The width of the control for these option widths: the options, and the
/// inset before, between, and after them.
pub fn total_width(theme: &Theme, widths: &[f32]) -> f32 {
    let gaps = widths.len() + 1;
    widths.iter().sum::<f32>() + theme.size.segmented_inset * gaps as f32
}

/// A segmented control. `options` pair a value with its text; `selected` is
/// the value that is chosen now. The group's `label` is what a screen reader
/// says ("Appearance").
#[derive(Clone, Copy, Debug)]
#[must_use = "a segmented control is drawn only when it is added to a Ui"]
pub struct Segmented<'a, T> {
    label: &'a str,
    options: &'a [(T, &'a str)],
    selected: T,
    disabled: Option<&'a str>,
    look: Option<Look>,
}

impl<'a, T: Copy + PartialEq> Segmented<'a, T> {
    pub const fn new(label: &'a str, options: &'a [(T, &'a str)], selected: T) -> Self {
        Self {
            label,
            options,
            selected,
            disabled: None,
            look: None,
        }
    }

    /// Disables the control. Hovering it shows `reason`.
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }

    /// Draws the chosen option as it looks in `look` (the gallery and the
    /// tests).
    pub const fn looks(mut self, look: Look) -> Self {
        self.look = Some(look);
        self
    }

    /// The tooltip: the reason when disabled.
    pub const fn tooltip(&self) -> Option<&'a str> {
        self.disabled
    }

    /// The height of the control: `size.control`.
    pub const fn height(theme: &Theme) -> f32 {
        theme.size.control
    }

    /// The height of an option: the control less the inset on both sides.
    pub fn option_height(theme: &Theme) -> f32 {
        theme.size.control - theme.size.segmented_inset - theme.size.segmented_inset
    }

    /// Draws the control. `inner` is the option the user chose this frame,
    /// when it is not the one that was already chosen.
    pub fn show(self, ui: &mut Ui) -> InnerResponse<Option<T>> {
        if self.disabled.is_some() {
            let reason = self.disabled;
            let shown = ui.add_enabled_ui(false, |ui| self.draw(ui)).inner;
            return InnerResponse::new(
                shown.inner,
                super::with_disabled_reason(shown.response, reason),
            );
        }
        self.draw(ui)
    }

    fn draw(self, ui: &mut Ui) -> InnerResponse<Option<T>> {
        debug_assert!(
            OPTIONS.contains(&self.options.len()),
            "a segmented control has 2 to 4 options (components.md)"
        );
        let theme = Theme::of(ui.ctx());
        let inset = theme.size.segmented_inset;
        let height = theme.size.control;
        let option_height = Self::option_height(theme);
        let pad = theme.space.s3;
        let token = |chosen: bool| TextToken {
            size: theme.text.body_sm.size,
            // The widest face, so the control does not change its width
            // when the choice moves.
            face: if chosen { Face::Medium } else { Face::Regular },
        };
        let galleys: Vec<_> = self
            .options
            .iter()
            .map(|(_, text)| layout(ui.painter(), text, token(true), Color32::PLACEHOLDER))
            .collect();
        let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + pad + pad).collect();
        let total = total_width(theme, &widths);
        let (rect, group) = ui.allocate_exact_size(vec2(total, height), Sense::hover());
        let enabled = ui.is_enabled();
        let label = self.label;
        group.widget_info(|| WidgetInfo::labeled(Role::RadioGroup, enabled, label));

        let mut chosen = None;
        let direction = Direction::CURRENT;
        let mut offset = inset;
        let mut spots = Vec::new();
        for width in &widths {
            let x = slot(direction, rect, offset, *width);
            spots.push(Rect::from_min_size(
                pos2(x, rect.top() + inset),
                vec2(*width, option_height),
            ));
            offset += width + inset;
        }
        // One Tab stop for the whole control: the option that has the focus,
        // or else the chosen one.
        let chosen_at = self
            .options
            .iter()
            .position(|(value, _)| *value == self.selected);
        let stop = tab_stop(ui, group.id, spots.len(), chosen_at);
        let responses: Vec<Response> = spots
            .iter()
            .enumerate()
            .map(|(index, spot)| group_button(ui, *spot, group.id, index, stop))
            .collect();
        for ((value, text), response) in self.options.iter().zip(&responses) {
            let is_chosen = *value == self.selected;
            if response.clicked() && !is_chosen {
                chosen = Some(*value);
            }
            let (selected, label) = (is_chosen, *text);
            response
                .widget_info(|| WidgetInfo::selected(Role::RadioButton, enabled, selected, label));
        }
        // A radio group: the keys choose the option the focus moves to.
        if let Some(target) = roving_keys(ui, direction, &responses)
            && self.options[target].0 != self.selected
        {
            chosen = Some(self.options[target].0);
        }

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let outer = CornerRadius::same(theme.radius.md);
            let inner = CornerRadius::same(theme.radius.sm);
            painter.rect_filled(rect, outer, track(theme));
            for (index, ((value, _), response)) in self.options.iter().zip(&responses).enumerate() {
                let is_chosen = *value == self.selected;
                let look = match self.look {
                    Some(look) if is_chosen => look,
                    Some(_) => Look::rest(),
                    None if enabled => Look::of(response),
                    None => Look::rest(),
                };
                let paint = option_colors(theme, is_chosen, look.touch);
                let spot = spots[index];
                if let Some(fill) = paint.fill {
                    painter.rect_filled(spot, inner, fill);
                }
                if let Some(border) = paint.border {
                    painter.rect_stroke(
                        spot,
                        inner,
                        Stroke::new(theme.size.border, border),
                        StrokeKind::Inside,
                    );
                }
                let galley = layout(
                    painter,
                    self.options[index].1,
                    TextToken {
                        size: theme.text.body_sm.size,
                        face: paint.face,
                    },
                    Color32::PLACEHOLDER,
                );
                let at = Align2::CENTER_CENTER.align_size_within_rect(galley.size(), spot);
                painter.galley(at.min, galley, paint.text);
                if look.focused {
                    paint_focus_ring(painter, theme, spot, theme.radius.sm);
                }
            }
        }
        InnerResponse::new(chosen, group)
    }
}

impl<T: Copy + PartialEq> Widget for Segmented<'_, T> {
    fn ui(self, ui: &mut Ui) -> Response {
        let shown = self.show(ui);
        let mut response = shown.response;
        if shown.inner.is_some() {
            response.mark_changed();
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Key};

    use super::*;
    use crate::theme::tests::{TEXT, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::Harness;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Mode {
        System,
        Light,
        Dark,
    }

    const MODES: [(Mode, &str); 3] = [
        (Mode::System, "System"),
        (Mode::Light, "Light"),
        (Mode::Dark, "Dark"),
    ];

    #[test]
    fn the_track_is_selected_and_the_chosen_option_is_a_raised_surface_pill() {
        for theme in [&LIGHT, &DARK] {
            assert_eq!(track(theme), theme.color.selected);
            let chosen = option_colors(theme, true, Touch::Rest);
            assert_eq!(chosen.fill, Some(theme.color.surface));
            assert_eq!(chosen.border, Some(theme.color.line));
            assert_eq!(chosen.face, Face::Medium);
            let other = option_colors(theme, false, Touch::Rest);
            assert_eq!((other.fill, other.border), (None, None));
            assert_eq!(other.face, Face::Regular);
        }
    }

    #[test]
    fn an_option_under_the_pointer_moves_toward_ink() {
        for theme in [&LIGHT, &DARK] {
            let hover = option_colors(theme, false, Touch::Hover).fill;
            let pressed = option_colors(theme, false, Touch::Pressed).fill;
            assert_eq!(hover, Some(theme.shade(track(theme), 0.06)));
            assert_eq!(pressed, Some(theme.shade(track(theme), 0.12)));
            let chosen = option_colors(theme, true, Touch::Hover).fill;
            assert_eq!(chosen, Some(theme.shade(theme.color.surface, 0.06)));
        }
    }

    #[test]
    fn every_option_text_reads_on_what_it_sits_on() {
        for theme in [&LIGHT, &DARK] {
            for chosen in [true, false] {
                for touch in [Touch::Rest, Touch::Hover, Touch::Pressed] {
                    let paint = option_colors(theme, chosen, touch);
                    let behind = paint.fill.unwrap_or_else(|| track(theme));
                    assert!(contrast(paint.text, behind) >= TEXT);
                }
            }
        }
    }

    #[test]
    fn it_is_as_high_as_a_control_with_28_high_options() {
        assert_eq!(Segmented::<Mode>::height(&LIGHT), 32.0);
        assert_eq!(Segmented::<Mode>::option_height(&LIGHT), 28.0);
        assert_eq!(OPTIONS, 2..=4);
    }

    fn show(ui: &mut Ui, selected: Mode, chosen: &mut Option<Mode>) -> Response {
        let shown = Segmented::new("Appearance", &MODES, selected).show(ui);
        if shown.inner.is_some() {
            *chosen = shown.inner;
        }
        shown.response
    }

    #[test]
    fn the_control_has_the_control_height_and_the_width_of_its_options() {
        let harness = Harness::new(egui::Theme::Light);
        let mut rect = Rect::NOTHING;
        harness.run(|ui| rect = show(ui, Mode::System, &mut None).rect);
        assert_eq!(rect.height(), LIGHT.size.control);
        assert!(rect.width() > total_width(&LIGHT, &[0.0, 0.0, 0.0]));
    }

    #[test]
    fn clicking_another_option_chooses_it_and_clicking_the_chosen_one_does_not() {
        let harness = Harness::new(egui::Theme::Light);
        let mut rect = Rect::NOTHING;
        harness.run(|ui| rect = show(ui, Mode::System, &mut None).rect);
        // The last option is near the end of the track; the first near its start.
        let near_end = pos2(rect.right() - LIGHT.space.s4, rect.center().y);
        let near_start = pos2(rect.left() + LIGHT.space.s4, rect.center().y);
        let mut chosen = None;
        harness.click(near_end, |ui| {
            show(ui, Mode::System, &mut chosen);
        });
        assert_eq!(chosen, Some(Mode::Dark));
        let mut chosen = None;
        harness.click(near_start, |ui| {
            show(ui, Mode::System, &mut chosen);
        });
        assert_eq!(chosen, None, "System was already chosen");
    }

    #[test]
    fn a_screen_reader_hears_a_radio_group_with_the_chosen_option() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            show(ui, Mode::Light, &mut None);
        });
        let group = tree.node_with_label("Appearance").expect("group");
        assert_eq!(group.role(), egui::Role::RadioGroup);
        let light = tree.node_with_label("Light").expect("chosen");
        assert_eq!(light.role(), egui::Role::RadioButton);
        assert_eq!(light.toggled(), Some(egui::accesskit::Toggled::True));
        let dark = tree.node_with_label("Dark").expect("other");
        assert_eq!(dark.toggled(), Some(egui::accesskit::Toggled::False));
    }

    #[test]
    fn a_disabled_control_says_why_and_chooses_nothing() {
        let control = Segmented::new("Appearance", &MODES, Mode::System).disabled("Not yet");
        assert_eq!(control.tooltip(), Some("Not yet"));
        assert_eq!(Segmented::new("A", &MODES, Mode::System).tooltip(), None);
        let harness = Harness::new(egui::Theme::Light);
        let mut rect = Rect::NOTHING;
        let draw = |ui: &mut Ui, chosen: &mut Option<Mode>| {
            let shown = Segmented::new("Appearance", &MODES, Mode::System)
                .disabled("Not yet")
                .show(ui);
            *chosen = shown.inner;
            shown.response
        };
        harness.run(|ui| rect = draw(ui, &mut None).rect);
        let mut chosen = None;
        harness.click(pos2(rect.right() - 8.0, rect.center().y), |ui| {
            draw(ui, &mut chosen);
        });
        assert_eq!(chosen, None);
    }

    #[test]
    fn the_control_is_one_tab_stop_entered_on_the_chosen_option() {
        let harness = Harness::new(egui::Theme::Light);
        // The control between two buttons: the option that has the focus, or
        // `None`, with the id of the button before and the one after.
        let frame = |harness: &Harness, key: Option<(Key, egui::Modifiers)>, selected: Mode| {
            let (mut before, mut after, mut group) =
                (egui::Id::NULL, egui::Id::NULL, egui::Id::NULL);
            let draw = |ui: &mut Ui| {
                before = ui.add(crate::widgets::Button::secondary("Before")).id;
                group = show(ui, selected, &mut None).id;
                after = ui.add(crate::widgets::Button::secondary("After")).id;
            };
            match key {
                Some((key, modifiers)) => harness.key_with(key, modifiers, draw),
                None => harness.run(draw),
            }
            let focused = harness.ctx.memory(|memory| memory.focused());
            let option = (0..MODES.len()).find(|index| focused == Some(group.with(*index)));
            (focused == Some(before), option, focused == Some(after))
        };
        let tab = |harness: &Harness, modifiers, selected| {
            frame(harness, Some((Key::Tab, modifiers)), selected);
            frame(harness, None, selected)
        };
        let none = egui::Modifiers::NONE;
        // Tab enters on the chosen option (Light, the second), not on the first.
        assert_eq!(tab(&harness, none, Mode::Light), (true, None, false));
        assert_eq!(tab(&harness, none, Mode::Light), (false, Some(1), false));
        // The next Tab leaves: it does not visit Dark.
        assert_eq!(tab(&harness, none, Mode::Light), (false, None, true));
        // Shift+Tab comes back in on the chosen option.
        let shift = egui::Modifiers::SHIFT;
        assert_eq!(tab(&harness, shift, Mode::Light), (false, Some(1), false));
        // The choice moved to Dark: Tab from outside now enters on Dark.
        assert_eq!(tab(&harness, none, Mode::Dark), (false, None, true));
        assert_eq!(tab(&harness, shift, Mode::Dark), (false, Some(2), false));
    }

    #[test]
    fn the_arrow_keys_move_the_choice_from_the_keyboard() {
        let harness = Harness::new(egui::Theme::Light);
        let frame = |harness: &Harness, key: Option<Key>, selected: Mode| {
            let mut chosen = None;
            let draw = |ui: &mut Ui| {
                show(ui, selected, &mut chosen);
            };
            match key {
                Some(key) => harness.key(key, draw),
                None => harness.run(draw),
            }
            chosen
        };
        frame(&harness, None, Mode::System);
        // Tab reaches the first option, and only shows the ring.
        assert_eq!(frame(&harness, Some(Key::Tab), Mode::System), None);
        // One frame for the focused option to claim the arrows.
        assert_eq!(frame(&harness, None, Mode::System), None);
        assert_eq!(
            frame(&harness, Some(Key::ArrowRight), Mode::System),
            Some(Mode::Light)
        );
        // The focus moved with the choice: the next arrow moves on.
        frame(&harness, None, Mode::Light);
        assert_eq!(
            frame(&harness, Some(Key::ArrowRight), Mode::Light),
            Some(Mode::Dark)
        );
        frame(&harness, None, Mode::Dark);
        // At the end the arrow does nothing, and the left arrow goes back.
        assert_eq!(frame(&harness, Some(Key::ArrowRight), Mode::Dark), None);
        frame(&harness, None, Mode::Dark);
        assert_eq!(
            frame(&harness, Some(Key::ArrowLeft), Mode::Dark),
            Some(Mode::Light)
        );
    }

    #[test]
    fn home_and_end_and_the_vertical_arrows_choose_like_the_other_groups() {
        let harness = Harness::new(egui::Theme::Light);
        let frame = |harness: &Harness, key: Option<Key>, selected: Mode| {
            let mut chosen = None;
            let draw = |ui: &mut Ui| {
                show(ui, selected, &mut chosen);
            };
            match key {
                Some(key) => harness.key(key, draw),
                None => harness.run(draw),
            }
            chosen
        };
        frame(&harness, None, Mode::System);
        frame(&harness, Some(Key::Tab), Mode::System);
        frame(&harness, None, Mode::System);
        assert_eq!(
            frame(&harness, Some(Key::End), Mode::System),
            Some(Mode::Dark)
        );
        frame(&harness, None, Mode::Dark);
        assert_eq!(
            frame(&harness, Some(Key::Home), Mode::Dark),
            Some(Mode::System)
        );
        frame(&harness, None, Mode::System);
        assert_eq!(
            frame(&harness, Some(Key::ArrowDown), Mode::System),
            Some(Mode::Light)
        );
    }
}
