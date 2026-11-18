//! The switch (components.md, Switch): a 36 by 22 track with a 16 knob.
//! On: an `accent` track. Off: the `off` track. It is always next to a text
//! label, and the label is part of the click target.
//!
//! `Switch::new(on, "Start at login")` draws the state it is given.
//! `Response::clicked` tells that the user asked to flip it; the screen turns
//! that into an action, and the model's answer is what the next frame draws.

use eframe::egui::{
    Align, Align2, Color32, CornerRadius, Rect, Response, Role, Sense, Ui, Widget, WidgetInfo,
    pos2, vec2,
};

use super::{Look, Touch, layout, paint_focus_ring, slot, touched};
use crate::theme::{Direction, Theme};

/// The track and the knob in one state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    pub track: Color32,
    pub knob: Color32,
}

/// The colors of a switch that is `on` while `touch` happens to it.
pub fn colors(theme: &Theme, on: bool, touch: Touch) -> Colors {
    let c = &theme.color;
    if on {
        Colors {
            track: touched(theme, c.accent, touch),
            knob: c.on_accent,
        }
    } else {
        Colors {
            track: touched(theme, c.off, touch),
            knob: c.knob_off,
        }
    }
}

/// A switch with its label.
#[derive(Clone, Copy, Debug)]
#[must_use = "a switch is drawn only when it is added to a Ui"]
pub struct Switch<'a> {
    on: bool,
    label: &'a str,
    disabled: Option<&'a str>,
    look: Option<Look>,
    bare: bool,
}

impl<'a> Switch<'a> {
    pub const fn new(on: bool, label: &'a str) -> Self {
        Self {
            on,
            label,
            disabled: None,
            look: None,
            bare: false,
        }
    }

    /// The track alone: the label is not drawn, and stays the name a screen
    /// reader hears. For a switch whose own row names it (Settings): the row
    /// is the label, and it makes its text part of the click target.
    pub const fn bare(mut self) -> Self {
        self.bare = true;
        self
    }

    /// Disables the switch. Hovering it shows `reason`.
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }

    /// [`Switch::disabled`] when `condition` holds.
    pub const fn disabled_if(self, condition: bool, reason: &'a str) -> Self {
        if condition {
            self.disabled(reason)
        } else {
            self
        }
    }

    /// Draws the switch as it looks in `look` (the gallery and the tests).
    pub const fn looks(mut self, look: Look) -> Self {
        self.look = Some(look);
        self
    }

    /// The tooltip: the reason when disabled.
    pub const fn tooltip(&self) -> Option<&'a str> {
        self.disabled
    }

    fn show(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let track_size = theme.size.switch;
        let galley = layout(
            ui.painter(),
            self.label,
            theme.text.body,
            Color32::PLACEHOLDER,
        );
        // At least the smallest click target (R9).
        let height = track_size.y.max(galley.size().y).max(theme.size.target_min);
        let width = if self.bare {
            track_size.x
        } else {
            track_size.x + theme.space.s3 + galley.size().x
        };
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
        if ui.is_rect_visible(rect) {
            let look = match self.look {
                Some(look) => look,
                None if ui.is_enabled() => Look::of(&response),
                None => Look::rest(),
            };
            let paint = colors(theme, self.on, look.touch);
            let painter = ui.painter();
            let x = slot(direction, rect, 0.0, track_size.x);
            let track_slot = Rect::from_min_size(pos2(x, rect.top()), vec2(track_size.x, height));
            let track = Align2::LEFT_CENTER.align_size_within_rect(track_size, track_slot);
            painter.rect_filled(track, CornerRadius::same(theme.radius.full), paint.track);
            // The knob sits at the end of the track when on, at the start
            // when off.
            let side = if self.on {
                direction.end()
            } else {
                direction.start()
            };
            let knob = Align2([side, Align::Center]).align_size_within_rect(
                vec2(theme.size.switch_knob, theme.size.switch_knob),
                track.shrink(theme.size.switch_inset),
            );
            painter.rect_filled(knob, CornerRadius::same(theme.radius.full), paint.knob);
            if !self.bare {
                let x = slot(
                    direction,
                    rect,
                    track_size.x + theme.space.s3,
                    galley.size().x,
                );
                let text_slot =
                    Rect::from_min_size(pos2(x, rect.top()), vec2(galley.size().x, height));
                let at = Align2::LEFT_CENTER.align_size_within_rect(galley.size(), text_slot);
                painter.galley(at.min, galley, theme.color.ink);
            }
            if look.focused {
                paint_focus_ring(painter, theme, rect, theme.radius.sm);
            }
        }
        let (enabled, on, label) = (ui.is_enabled(), self.on, self.label);
        response.widget_info(|| WidgetInfo::selected(Role::Switch, enabled, on, label));
        response
    }
}

impl Widget for Switch<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let reason = self.disabled;
        if reason.is_none() {
            return self.show(ui);
        }
        let response = ui.add_enabled_ui(false, |ui| self.show(ui)).inner;
        super::with_disabled_reason(response, reason)
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui;

    use super::*;
    use crate::theme::tests::{UI, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::Harness;

    #[test]
    fn on_is_the_accent_track_and_off_is_the_off_track() {
        for theme in [&LIGHT, &DARK] {
            let on = colors(theme, true, Touch::Rest);
            assert_eq!(on.track, theme.color.accent);
            assert_eq!(on.knob, theme.color.on_accent);
            let off = colors(theme, false, Touch::Rest);
            assert_eq!(off.track, theme.color.off);
            assert_eq!(off.knob, theme.color.knob_off);
        }
    }

    #[test]
    fn hover_and_pressed_move_the_track_toward_ink() {
        for theme in [&LIGHT, &DARK] {
            for on in [true, false] {
                let rest = colors(theme, on, Touch::Rest).track;
                assert_eq!(
                    colors(theme, on, Touch::Hover).track,
                    theme.shade(rest, 0.06)
                );
                assert_eq!(
                    colors(theme, on, Touch::Pressed).track,
                    theme.shade(rest, 0.12)
                );
            }
        }
    }

    #[test]
    fn the_knob_stands_out_from_an_on_track_and_the_two_states_differ() {
        for theme in [&LIGHT, &DARK] {
            for touch in [Touch::Rest, Touch::Hover, Touch::Pressed] {
                let on = colors(theme, true, touch);
                assert!(contrast(on.knob, on.track) >= UI);
                let off = colors(theme, false, touch);
                assert_ne!(on.track, off.track);
                assert_ne!(off.knob, off.track);
            }
        }
    }

    #[test]
    fn the_track_is_36_by_22_with_a_16_knob_and_a_3_inset() {
        let size = &LIGHT.size;
        assert_eq!(size.switch, egui::vec2(36.0, 22.0));
        assert_eq!(size.switch_knob, 16.0);
        // The knob fills the track's height less the inset on both sides.
        assert_eq!(size.switch.y - size.switch_inset - size.switch_inset, 16.0);
    }

    #[test]
    fn the_label_is_part_of_the_click_target_and_the_target_is_big_enough() {
        let harness = Harness::new(egui::Theme::Light);
        let mut rect = egui::Rect::NOTHING;
        harness.run(|ui| rect = ui.add(Switch::new(false, "Start at login")).rect);
        assert!(rect.width() > LIGHT.size.switch.x + LIGHT.space.s3);
        assert!(rect.height() >= LIGHT.size.target_min);
        // A click on the label, far from the track, flips it.
        let label = egui::pos2(rect.right() - 4.0, rect.center().y);
        let mut clicked = false;
        harness.click(label, |ui| {
            clicked |= ui.add(Switch::new(false, "Start at login")).clicked();
        });
        assert!(clicked);
    }

    #[test]
    fn a_bare_switch_is_the_track_alone_and_keeps_its_name_for_a_screen_reader() {
        let harness = Harness::new(egui::Theme::Light);
        let mut bare = egui::Rect::NOTHING;
        let mut full = egui::Rect::NOTHING;
        let tree = harness.accessibility(|ui| {
            bare = ui.add(Switch::new(true, "Start at login").bare()).rect;
            full = ui.add(Switch::new(true, "Start at login")).rect;
        });
        // The track, and nothing after it.
        assert_eq!(bare.width(), LIGHT.size.switch.x);
        assert!(full.width() > bare.width() + LIGHT.space.s3);
        assert!(bare.height() >= LIGHT.size.target_min);
        let named = tree.nodes_with_label("Start at login");
        assert_eq!(named.len(), 2, "both are named, the bare one too");
        assert!(named.iter().all(|node| node.role() == egui::Role::Switch));
        // It still flips on a click.
        let mut clicked = false;
        harness.click(bare.center(), |ui| {
            clicked |= ui.add(Switch::new(true, "Start at login").bare()).clicked();
        });
        assert!(clicked);
    }

    #[test]
    fn a_screen_reader_hears_a_switch_its_name_and_whether_it_is_on() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            ui.add(Switch::new(true, "Start at login"));
            ui.add(Switch::new(false, "Xdebug"));
            ui.add(Switch::new(false, "Autostart").disabled("Needs setup first"));
        });
        let on = tree.node_with_label("Start at login").expect("on");
        assert_eq!(on.role(), egui::Role::Switch);
        assert_eq!(on.toggled(), Some(egui::accesskit::Toggled::True));
        let off = tree.node_with_label("Xdebug").expect("off");
        assert_eq!(off.toggled(), Some(egui::accesskit::Toggled::False));
        assert!(
            tree.node_with_label("Autostart")
                .expect("disabled")
                .is_disabled()
        );
    }

    #[test]
    fn a_disabled_switch_says_why_and_ignores_clicks() {
        let switch = Switch::new(false, "Autostart").disabled("Needs setup first");
        assert_eq!(switch.tooltip(), Some("Needs setup first"));
        assert_eq!(Switch::new(false, "x").tooltip(), None);
        assert_eq!(
            Switch::new(false, "x").disabled_if(false, "r").tooltip(),
            None
        );
        let harness = Harness::new(egui::Theme::Light);
        let mut rect = egui::Rect::NOTHING;
        harness.run(|ui| rect = ui.add(Switch::new(false, "Autostart").disabled("Why")).rect);
        let mut clicked = false;
        harness.click(rect.center(), |ui| {
            clicked |= ui
                .add(Switch::new(false, "Autostart").disabled("Why"))
                .clicked();
        });
        assert!(!clicked);
    }
}
