//! Tabs (components.md, Tabs): a strip of text tabs over a 1 px `line`; the
//! chosen tab is in 500 weight with a 2 px `accent` underline. It is a tab
//! list for a screen reader and one Tab stop (the WAI-ARIA "roving focus"):
//! Tab enters on the chosen tab and the next Tab leaves. The arrow keys move
//! the choice and the focus with it ("automatic activation": choosing a tab
//! only shows another part of the page, it sends nothing).
//!
//! Only the tabs of shipped features are given (rules.md 8): the site page
//! has Overview now, and Mail, Dumps, Logs, and Database join with their
//! features. The optional section tile and count of components.md join with
//! the first tab that has them.
//!
//! The strip draws the tab it is given and returns the tab the user chose,
//! like the other widgets: the screen keeps the choice.

use eframe::egui::{
    Align2, Color32, CornerRadius, InnerResponse, Rect, Response, Role, Sense, Ui, Widget,
    WidgetInfo, pos2, vec2,
};

use super::keys::roving_keys;
use super::{Look, Touch, group_button, layout, paint_focus_ring, quiet_fill, slot, tab_stop};
use crate::theme::{Direction, Face, TextToken, Theme};

/// What a tab is made of in one state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabColors {
    /// `None` at rest: the page shows through.
    pub fill: Option<Color32>,
    pub text: Color32,
    /// The underline of the chosen tab.
    pub bar: Option<Color32>,
    pub face: Face,
}

/// A tab, chosen or not, while `touch` happens to it.
pub fn tab_colors(theme: &Theme, chosen: bool, touch: Touch) -> TabColors {
    let c = &theme.color;
    TabColors {
        fill: quiet_fill(theme, touch),
        text: if chosen { c.ink } else { c.ink2 },
        bar: chosen.then_some(c.accent),
        face: if chosen { Face::Medium } else { Face::Regular },
    }
}

/// A strip of tabs. `tabs` pair a value with its text; `selected` is the
/// value that is chosen now. The strip's `label` is what a screen reader
/// says ("Site").
#[derive(Clone, Copy, Debug)]
#[must_use = "tabs are drawn only when they are added to a Ui"]
pub struct Tabs<'a, T> {
    label: &'a str,
    tabs: &'a [(T, &'a str)],
    selected: T,
    look: Option<Look>,
}

impl<'a, T: Copy + PartialEq> Tabs<'a, T> {
    pub const fn new(label: &'a str, tabs: &'a [(T, &'a str)], selected: T) -> Self {
        Self {
            label,
            tabs,
            selected,
            look: None,
        }
    }

    /// Draws the chosen tab as it looks in `look` (the gallery and the
    /// tests).
    pub const fn looks(mut self, look: Look) -> Self {
        self.look = Some(look);
        self
    }

    /// Draws the strip. `inner` is the tab the user chose this frame, when
    /// it is not the one that was already chosen.
    pub fn show(self, ui: &mut Ui) -> InnerResponse<Option<T>> {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let size = vec2(ui.available_width(), theme.size.tab);
        let (rect, group) = ui.allocate_exact_size(size, Sense::hover());
        let enabled = ui.is_enabled();
        let label = self.label;
        group.widget_info(|| WidgetInfo::labeled(Role::TabList, enabled, label));

        // Sized for the heavier face, so a tab keeps its width when the
        // choice moves to it.
        let token = TextToken {
            size: theme.text.body.size,
            face: Face::Medium,
        };
        let pad = theme.space.s3;
        let widths: Vec<f32> = self
            .tabs
            .iter()
            .map(|(_, text)| {
                layout(ui.painter(), text, token, Color32::PLACEHOLDER)
                    .size()
                    .x
                    + pad
                    + pad
            })
            .collect();
        // The tab stops short of the line under the strip.
        let height = theme.size.tab - theme.size.border;
        let mut offset = 0.0;
        let mut spots = Vec::with_capacity(widths.len());
        for width in &widths {
            let x = slot(direction, rect, offset, *width);
            spots.push(Rect::from_min_size(
                pos2(x, rect.top()),
                vec2(*width, height),
            ));
            offset += width + theme.space.s1;
        }

        let chosen_at = self
            .tabs
            .iter()
            .position(|(value, _)| *value == self.selected);
        let stop = tab_stop(ui, group.id, spots.len(), chosen_at);
        let responses: Vec<Response> = spots
            .iter()
            .enumerate()
            .map(|(index, spot)| group_button(ui, *spot, group.id, index, stop))
            .collect();
        let mut chosen = None;
        for ((value, text), response) in self.tabs.iter().zip(&responses) {
            let is_chosen = *value == self.selected;
            if response.clicked() && !is_chosen {
                chosen = Some(*value);
            }
            let (selected, text) = (is_chosen, *text);
            response.widget_info(|| WidgetInfo::selected(Role::Tab, enabled, selected, text));
        }
        // The keys choose the tab the focus moves to.
        if let Some(target) = roving_keys(ui, direction, &responses)
            && self.tabs[target].0 != self.selected
        {
            chosen = Some(self.tabs[target].0);
        }

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let line = Rect::from_min_max(
                pos2(rect.left(), rect.bottom() - theme.size.border),
                rect.right_bottom(),
            );
            painter.rect_filled(line, CornerRadius::ZERO, theme.color.line);
            let top = CornerRadius {
                nw: theme.radius.md,
                ne: theme.radius.md,
                sw: 0,
                se: 0,
            };
            for (index, ((value, text), response)) in self.tabs.iter().zip(&responses).enumerate() {
                let is_chosen = *value == self.selected;
                let look = match self.look {
                    Some(look) if is_chosen => look,
                    Some(_) => Look::rest(),
                    None if enabled => Look::of(response),
                    None => Look::rest(),
                };
                let paint = tab_colors(theme, is_chosen, look.touch);
                let spot = spots[index];
                if let Some(fill) = paint.fill {
                    painter.rect_filled(spot, top, fill);
                }
                let galley = layout(
                    painter,
                    text,
                    TextToken {
                        size: theme.text.body.size,
                        face: paint.face,
                    },
                    Color32::PLACEHOLDER,
                );
                let at = Align2::CENTER_CENTER.align_size_within_rect(galley.size(), spot);
                painter.galley(at.min, galley, paint.text);
                if let Some(bar) = paint.bar {
                    // The underline covers the line under the strip.
                    let under = Rect::from_min_max(
                        pos2(spot.left(), rect.bottom() - theme.size.tab_bar),
                        pos2(spot.right(), rect.bottom()),
                    );
                    painter.rect_filled(under, CornerRadius::ZERO, bar);
                }
                if look.focused {
                    paint_focus_ring(painter, theme, spot, theme.radius.md);
                }
            }
        }
        InnerResponse::new(chosen, group)
    }
}

impl<T: Copy + PartialEq> Widget for Tabs<'_, T> {
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
    use crate::widgets::harness::{Harness, over};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Tab {
        Overview,
        Logs,
    }

    const TABS: [(Tab, &str); 2] = [(Tab::Overview, "Overview"), (Tab::Logs, "Logs")];

    fn show(ui: &mut Ui, selected: Tab, chosen: &mut Option<Tab>) -> Response {
        let shown = Tabs::new("Site", &TABS, selected).show(ui);
        if shown.inner.is_some() {
            *chosen = shown.inner;
        }
        shown.response
    }

    #[test]
    fn the_chosen_tab_is_ink_in_medium_with_an_accent_bar_and_the_others_ink2() {
        for theme in [&LIGHT, &DARK] {
            let chosen = tab_colors(theme, true, Touch::Rest);
            assert_eq!(chosen.text, theme.color.ink);
            assert_eq!(chosen.bar, Some(theme.color.accent));
            assert_eq!(chosen.face, Face::Medium);
            assert_eq!(chosen.fill, None);
            let other = tab_colors(theme, false, Touch::Rest);
            assert_eq!(other.text, theme.color.ink2);
            assert_eq!((other.bar, other.fill), (None, None));
            assert_eq!(other.face, Face::Regular);
        }
    }

    #[test]
    fn a_tab_under_the_pointer_is_selected_at_half_and_pressed_is_selected() {
        for theme in [&LIGHT, &DARK] {
            let hover = tab_colors(theme, false, Touch::Hover).fill;
            let pressed = tab_colors(theme, false, Touch::Pressed).fill;
            assert_eq!(
                hover,
                Some(theme.color.selected.gamma_multiply(theme.state.row_hover))
            );
            assert_eq!(pressed, Some(theme.color.selected));
        }
    }

    #[test]
    fn every_tab_text_reads_on_what_it_sits_on() {
        for theme in [&LIGHT, &DARK] {
            for chosen in [true, false] {
                for touch in [Touch::Rest, Touch::Hover, Touch::Pressed] {
                    let paint = tab_colors(theme, chosen, touch);
                    let behind = paint
                        .fill
                        .map_or(theme.color.bg, |fill| over(fill, theme.color.bg));
                    assert!(
                        contrast(paint.text, behind) >= TEXT,
                        "{chosen} {touch:?}: {}",
                        contrast(paint.text, behind)
                    );
                }
            }
        }
    }

    #[test]
    fn the_strip_is_38_high_and_as_wide_as_the_space_it_is_given() {
        let harness = Harness::new(egui::Theme::Light);
        let (mut rect, mut width) = (Rect::NOTHING, 0.0);
        harness.run(|ui| {
            width = ui.available_width();
            rect = show(ui, Tab::Overview, &mut None).rect;
        });
        assert_eq!(rect.height(), LIGHT.size.tab);
        assert_eq!(rect.width(), width);
    }

    #[test]
    fn the_chosen_tab_is_underlined_by_2_px_of_accent_and_only_that_one() {
        let harness = Harness::new(egui::Theme::Light);
        let mut strip = Rect::NOTHING;
        let shapes = harness.shapes(|ui| strip = show(ui, Tab::Overview, &mut None).rect);
        let bars: Vec<Rect> = shapes
            .iter()
            .filter_map(|shape| match shape {
                egui::Shape::Rect(rect) if rect.fill == LIGHT.color.accent => Some(rect.rect),
                _ => None,
            })
            .collect();
        assert_eq!(bars.len(), 1, "{bars:?}");
        assert_eq!(bars[0].height(), LIGHT.size.tab_bar);
        assert_eq!(bars[0].bottom(), strip.bottom());
        // The line under the whole strip is 1 px of `line`.
        assert!(shapes.iter().any(|shape| matches!(
            shape,
            egui::Shape::Rect(rect)
                if rect.fill == LIGHT.color.line
                    && rect.rect.height() == LIGHT.size.border
                    && rect.rect.width() == strip.width()
        )));
    }

    #[test]
    fn clicking_another_tab_chooses_it_and_clicking_the_chosen_one_does_not() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            show(ui, Tab::Overview, &mut None);
        });
        let centre = |label: &str| {
            let bounds = tree
                .node_with_label(label)
                .expect(label)
                .bounds()
                .expect("a place");
            pos2(
                ((bounds.x0 + bounds.x1) / 2.0) as f32,
                ((bounds.y0 + bounds.y1) / 2.0) as f32,
            )
        };
        let mut chosen = None;
        harness.click(centre("Logs"), |ui| {
            show(ui, Tab::Overview, &mut chosen);
        });
        assert_eq!(chosen, Some(Tab::Logs));
        let mut chosen = None;
        harness.click(centre("Overview"), |ui| {
            show(ui, Tab::Overview, &mut chosen);
        });
        assert_eq!(chosen, None, "Overview was already chosen");
    }

    #[test]
    fn a_screen_reader_hears_a_tab_list_with_the_chosen_tab() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            show(ui, Tab::Logs, &mut None);
        });
        let list = tree.node_with_label("Site").expect("the list");
        assert_eq!(list.role(), egui::Role::TabList);
        let logs = tree.node_with_label("Logs").expect("chosen");
        assert_eq!(logs.role(), egui::Role::Tab);
        assert_eq!(logs.toggled(), Some(egui::accesskit::Toggled::True));
        let overview = tree.node_with_label("Overview").expect("other");
        assert_eq!(overview.toggled(), Some(egui::accesskit::Toggled::False));
    }

    #[test]
    fn the_strip_is_one_tab_stop_entered_on_the_chosen_tab() {
        let harness = Harness::new(egui::Theme::Light);
        let frame = |harness: &Harness, key: Option<(Key, egui::Modifiers)>, selected: Tab| {
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
            let tab = (0..TABS.len()).find(|index| focused == Some(group.with(*index)));
            (focused == Some(before), tab, focused == Some(after))
        };
        let tab = |harness: &Harness, modifiers, selected| {
            frame(harness, Some((Key::Tab, modifiers)), selected);
            frame(harness, None, selected)
        };
        let none = egui::Modifiers::NONE;
        // Tab enters on the chosen tab (Logs, the second), and the next Tab
        // leaves.
        assert_eq!(tab(&harness, none, Tab::Logs), (true, None, false));
        assert_eq!(tab(&harness, none, Tab::Logs), (false, Some(1), false));
        assert_eq!(tab(&harness, none, Tab::Logs), (false, None, true));
    }

    #[test]
    fn the_arrow_keys_move_the_choice_from_the_keyboard() {
        let harness = Harness::new(egui::Theme::Light);
        let frame = |harness: &Harness, key: Option<Key>, selected: Tab| {
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
        frame(&harness, None, Tab::Overview);
        // Tab reaches the first tab and only shows the ring; one more frame
        // for it to claim the arrows.
        assert_eq!(frame(&harness, Some(Key::Tab), Tab::Overview), None);
        assert_eq!(frame(&harness, None, Tab::Overview), None);
        assert_eq!(
            frame(&harness, Some(Key::ArrowRight), Tab::Overview),
            Some(Tab::Logs)
        );
        frame(&harness, None, Tab::Logs);
        // At the end the arrow does nothing, and the left arrow goes back.
        assert_eq!(frame(&harness, Some(Key::ArrowRight), Tab::Logs), None);
        frame(&harness, None, Tab::Logs);
        assert_eq!(
            frame(&harness, Some(Key::ArrowLeft), Tab::Logs),
            Some(Tab::Overview)
        );
    }

    #[test]
    fn a_focused_tab_shows_the_ring() {
        let harness = Harness::new(egui::Theme::Light);
        let ring = |look: Look| {
            let shapes = harness.shapes(|ui| {
                Tabs::new("Site", &TABS, Tab::Overview).looks(look).show(ui);
            });
            shapes
                .iter()
                .filter(|shape| matches!(shape, egui::Shape::Rect(rect) if rect.stroke.color == LIGHT.color.accent && rect.stroke.width == LIGHT.focus.width))
                .count()
        };
        assert_eq!(ring(Look::focused()), 1);
        assert_eq!(ring(Look::rest()), 0);
    }

    #[test]
    fn home_and_end_and_the_vertical_arrows_choose_like_the_other_groups() {
        let harness = Harness::new(egui::Theme::Light);
        let frame = |harness: &Harness, key: Option<Key>, selected: Tab| {
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
        frame(&harness, None, Tab::Overview);
        frame(&harness, Some(Key::Tab), Tab::Overview);
        frame(&harness, None, Tab::Overview);
        assert_eq!(
            frame(&harness, Some(Key::End), Tab::Overview),
            Some(Tab::Logs)
        );
        frame(&harness, None, Tab::Logs);
        assert_eq!(
            frame(&harness, Some(Key::Home), Tab::Logs),
            Some(Tab::Overview)
        );
        frame(&harness, None, Tab::Overview);
        assert_eq!(
            frame(&harness, Some(Key::ArrowDown), Tab::Overview),
            Some(Tab::Logs)
        );
    }
}
