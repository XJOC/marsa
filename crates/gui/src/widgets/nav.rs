//! A sidebar item (components.md, Sidebar): the section tile, the label, and
//! the count. Folded, the tile alone, with the name in a tooltip and a dot on
//! the tile when something is new. Selected: the `selected` fill, the label
//! at 500, and `aria-current` for screen readers. Settings, at the bottom, has
//! a plain icon instead of a tile.

use eframe::egui::{
    Align2, CornerRadius, Rect, Response, Role, Sense, Ui, Vec2, Widget, WidgetInfo, accesskit,
    pos2, vec2,
};

use super::badge::{count_look, count_text};
use super::icons::Icon;
use super::tile::{TileSize, paint_section};
use super::{Look, layout, paint_focus_ring, quiet_fill, slot};
use crate::strings;
use crate::theme::{Direction, Face, Section, TextToken, Theme};

/// What an item shows at its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    /// The section's tile.
    Section(Section),
    /// An icon in `ink2`, with no tile.
    Plain(Icon),
}

/// A sidebar item. `Response::clicked` tells that the user opened it.
#[derive(Clone, Copy, Debug)]
#[must_use = "a sidebar item is drawn only when it is added to a Ui"]
pub struct NavItem<'a> {
    mark: Mark,
    label: &'a str,
    /// The count, and whether something is new.
    count: Option<(u32, bool)>,
    selected: bool,
    folded: bool,
    look: Option<Look>,
}

impl<'a> NavItem<'a> {
    const fn new(mark: Mark, label: &'a str) -> Self {
        Self {
            mark,
            label,
            count: None,
            selected: false,
            folded: false,
            look: None,
        }
    }

    /// The item of a section, named as the section.
    pub const fn section(section: Section) -> Self {
        Self::new(Mark::Section(section), strings::section_name(section))
    }

    /// An item with a plain icon: Settings.
    pub const fn plain(icon: Icon, label: &'a str) -> Self {
        Self::new(Mark::Plain(icon), label)
    }

    /// The count after the label. `fresh`: something is new, so the count
    /// takes the section's color, and a folded item shows a dot.
    pub const fn count(mut self, count: u32, fresh: bool) -> Self {
        self.count = Some((count, fresh));
        self
    }

    /// The item of the screen that shows.
    pub const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// The tile alone; the name in a tooltip.
    pub const fn folded(mut self, folded: bool) -> Self {
        self.folded = folded;
        self
    }

    /// Draws the item as it looks in `look` (the tests).
    pub const fn looks(mut self, look: Look) -> Self {
        self.look = Some(look);
        self
    }
}

impl Widget for NavItem<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let side = theme.size.nav_item;
        let width = ui.available_width().max(side);
        let (rect, response) = ui.allocate_exact_size(vec2(width, side), Sense::click());
        let fresh = self.count.is_some_and(|(_, fresh)| fresh);
        if ui.is_rect_visible(rect) {
            let look = self.look.unwrap_or_else(|| Look::of(&response));
            let painter = ui.painter();
            let corner = theme.radius.md;
            let fill = if self.selected {
                Some(theme.color.selected)
            } else {
                quiet_fill(theme, look.touch)
            };
            if let Some(fill) = fill {
                painter.rect_filled(rect, CornerRadius::same(corner), fill);
            }
            let inset = theme.size.nav_inset();
            let tile_side = Vec2::splat(theme.size.nav_tile);
            let tile = if self.folded {
                Rect::from_center_size(rect.center(), tile_side)
            } else {
                let x = slot(direction, rect, inset, theme.size.nav_tile);
                Rect::from_min_size(pos2(x, rect.top() + inset), tile_side)
            };
            let text_color = match self.mark {
                Mark::Section(section) => {
                    let dot = self.folded && fresh;
                    paint_section(painter, theme, tile, section, TileSize::Nav, dot);
                    theme.color.ink
                }
                Mark::Plain(icon) => {
                    let spot =
                        Rect::from_center_size(tile.center(), Vec2::splat(theme.size.icon_tile));
                    icon.paint(painter, theme, spot, theme.color.ink2);
                    theme.color.ink2
                }
            };
            if !self.folded {
                let face = if self.selected {
                    Face::Medium
                } else {
                    Face::Regular
                };
                let token = TextToken {
                    size: theme.text.body.size,
                    face,
                };
                let galley = layout(painter, self.label, token, text_color);
                let offset = inset + theme.size.nav_tile + theme.space.group_gap_max;
                let x = slot(direction, rect, offset, galley.size().x);
                let spot = Rect::from_min_size(pos2(x, rect.top()), vec2(galley.size().x, side));
                let at = Align2::LEFT_CENTER.align_size_within_rect(galley.size(), spot);
                painter.galley(at.min, galley, text_color);
                if let (Some((count, fresh)), Mark::Section(section)) = (self.count, self.mark) {
                    let shown = count_look(theme, section, fresh);
                    let token = TextToken {
                        size: theme.text.caption.size,
                        face: shown.face,
                    };
                    let galley = layout(painter, &count_text(count), token, shown.color);
                    let width = galley.size().x;
                    let x = slot(direction, rect, rect.width() - inset - width, width);
                    let spot = Rect::from_min_size(pos2(x, rect.top()), vec2(width, side));
                    let at = Align2::LEFT_CENTER.align_size_within_rect(galley.size(), spot);
                    painter.galley(at.min, galley, shown.color);
                }
            }
            if look.focused {
                paint_focus_ring(painter, theme, rect, corner);
            }
        }
        let (label, selected, enabled) = (self.label, self.selected, ui.is_enabled());
        // A link that is current, not a toggle: egui's `selected` would
        // make it a toggle for a screen reader.
        response.widget_info(|| WidgetInfo::labeled(Role::Link, enabled, label));
        let count = match (self.count, self.mark) {
            (Some((count, _)), Mark::Section(section)) => {
                Some(strings::row_badge_label(section, count))
            }
            _ => None,
        };
        ui.ctx().accesskit_node_builder(response.id, |node| {
            if selected {
                node.set_aria_current(accesskit::AriaCurrent::Page);
            }
            if let Some(count) = count {
                node.set_description(count);
            }
        });
        if self.folded {
            return response.on_hover_text(label);
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Shape, StrokeKind};

    use super::*;
    use crate::theme::LIGHT;
    use crate::widgets::harness::Harness;

    fn sites() -> NavItem<'static> {
        NavItem::section(Section::Sites).count(6, false)
    }

    #[test]
    fn an_item_is_38_high_and_fills_the_sidebar_and_folded_is_a_square() {
        let harness = Harness::new(egui::Theme::Light);
        let (mut open, mut folded) = (Rect::NOTHING, Rect::NOTHING);
        harness.run(|ui| {
            ui.set_width(LIGHT.size.sidebar);
            open = ui.add(sites()).rect;
        });
        harness.run(|ui| {
            ui.set_width(LIGHT.size.nav_item);
            folded = ui.add(sites().folded(true)).rect;
        });
        assert_eq!(open.size(), vec2(LIGHT.size.sidebar, LIGHT.size.nav_item));
        assert_eq!(folded.size(), Vec2::splat(LIGHT.size.nav_item));
    }

    #[test]
    fn the_selected_item_is_filled_and_a_screen_reader_hears_it_as_current() {
        let harness = Harness::new(egui::Theme::Light);
        let fills = |item: NavItem<'static>| {
            harness
                .shapes(|ui| {
                    ui.add(item);
                })
                .into_iter()
                .any(|shape| matches!(shape, Shape::Rect(r) if r.fill == LIGHT.color.selected))
        };
        assert!(fills(sites().selected(true)));
        assert!(!fills(sites()));
        let tree = harness.accessibility(|ui| {
            ui.add(sites().selected(true));
            ui.add(NavItem::plain(Icon::Settings, strings::SETTINGS).folded(true));
        });
        let node = tree.node_with_label("Sites").expect("the Sites item");
        assert_eq!(node.role(), egui::Role::Link);
        assert_eq!(node.aria_current(), Some(accesskit::AriaCurrent::Page));
        assert_eq!(node.description(), Some("6 sites"));
        // Folded, the name still reaches a screen reader.
        let settings = tree.node_with_label(strings::SETTINGS).expect("Settings");
        assert_eq!(settings.aria_current(), None);
    }

    #[test]
    fn a_focused_item_shows_the_ring() {
        let harness = Harness::new(egui::Theme::Light);
        let ring = |look: Look| {
            harness
                .shapes(|ui| {
                    ui.add(sites().looks(look));
                })
                .into_iter()
                .any(|shape| matches!(shape, Shape::Rect(r) if r.stroke_kind == StrokeKind::Outside && r.stroke.color == LIGHT.color.accent))
        };
        assert!(ring(Look::focused()));
        assert!(!ring(Look::hover()));
    }
}
