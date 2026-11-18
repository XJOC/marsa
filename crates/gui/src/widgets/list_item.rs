//! A choice in a master list (layout.md, Master and detail; the runtimes list
//! of the mockup): a tile with letters, a name, and a line under it. The
//! chosen one has a `surface` fill and a 1 px `line` border and its name at
//! 500 like the others; the others have no fill until the pointer is over them. It is a
//! choice, not a page: a screen reader hears an option that is selected or
//! not, with the line under the name as its description.

use eframe::egui::text::{LayoutJob, TextWrapping};
use eframe::egui::{
    Align2, Color32, CornerRadius, Rect, Response, Role, Sense, Stroke, StrokeKind, Ui, Vec2,
    Widget, WidgetInfo, pos2, vec2,
};

use super::tile::{TileSize, paint_monogram};
use super::{Look, paint_focus_ring, quiet_fill, slot};
use crate::theme::{Direction, Face, Section, TextToken, Theme};

/// What a list item is painted with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Paint {
    /// `None`: no fill.
    pub fill: Option<Color32>,
    /// `None`: no border.
    pub border: Option<Color32>,
}

/// The fill and the border of an item in one state.
pub fn paint(theme: &Theme, selected: bool, look: Look) -> Paint {
    if selected {
        Paint {
            fill: Some(theme.color.surface),
            border: Some(theme.color.line),
        }
    } else {
        Paint {
            fill: quiet_fill(theme, look.touch),
            border: None,
        }
    }
}

/// One item of a list the user chooses from. `Response::clicked` tells that
/// the user chose it.
#[derive(Clone, Copy, Debug)]
#[must_use = "a list item is drawn only when it is added to a Ui"]
pub struct ListItem<'a> {
    letters: &'a str,
    section: Section,
    title: &'a str,
    sub: &'a str,
    selected: bool,
    look: Option<Look>,
}

impl<'a> ListItem<'a> {
    /// An item named `title`, with `sub` under it and a tile of `letters`.
    pub const fn new(letters: &'a str, title: &'a str, sub: &'a str) -> Self {
        Self {
            letters,
            section: Section::Runtimes,
            title,
            sub,
            selected: false,
            look: None,
        }
    }

    /// The colors of the tile.
    pub const fn section(mut self, section: Section) -> Self {
        self.section = section;
        self
    }

    /// The item that is chosen now.
    pub const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Draws the item as it looks in `look` (the tests).
    pub const fn looks(mut self, look: Look) -> Self {
        self.look = Some(look);
        self
    }
}

impl Widget for ListItem<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let height = theme.size.row_max;
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
        if ui.is_rect_visible(rect) {
            let look = self.look.unwrap_or_else(|| Look::of(&response));
            let shown = paint(theme, self.selected, look);
            let painter = ui.painter();
            let corner = CornerRadius::same(theme.radius.md);
            if let Some(fill) = shown.fill {
                painter.rect_filled(rect, corner, fill);
            }
            if let Some(border) = shown.border {
                let stroke = Stroke::new(theme.size.border, border);
                painter.rect_stroke(rect, corner, stroke, StrokeKind::Inside);
            }
            let pad = theme.space.s3;
            let side = TileSize::Header.side(theme);
            let tile_x = slot(direction, rect, pad, side);
            let spot = Rect::from_min_size(pos2(tile_x, rect.top()), vec2(side, height));
            let tile = Align2::LEFT_CENTER.align_size_within_rect(Vec2::splat(side), spot);
            paint_monogram(
                painter,
                theme,
                tile,
                self.letters,
                TileSize::Header,
                self.section,
            );
            // The two lines of text, one under the other, centered on the
            // item; a line too long for the space ends in an ellipsis.
            let space = (rect.width() - pad - side - pad - pad).max(0.0);
            let line = |text: &str, token: TextToken| {
                let mut job = LayoutJob::simple_singleline(
                    text.to_owned(),
                    token.font_id(),
                    Color32::PLACEHOLDER,
                );
                job.wrap = TextWrapping::truncate_at_width(space);
                painter.layout_job(job)
            };
            // The name is at 500 whether or not the item is chosen (the
            // mockup's): the fill and the border say which one it is.
            let name = TextToken {
                size: theme.text.body.size,
                face: Face::Medium,
            };
            let first = line(self.title, name);
            let second = line(self.sub, theme.text.caption);
            let block = vec2(
                first.size().x.max(second.size().x),
                first.size().y + second.size().y,
            );
            let text_x = slot(direction, rect, pad + side + pad, block.x);
            let spot = Rect::from_min_size(pos2(text_x, rect.top()), vec2(block.x, height));
            let top = Align2::LEFT_CENTER.align_size_within_rect(block, spot).min;
            let below = top + vec2(0.0, first.size().y);
            painter.galley(top, first, theme.color.ink);
            painter.galley(below, second, theme.color.mute);
            if look.focused {
                paint_focus_ring(painter, theme, rect, theme.radius.md);
            }
        }
        let (title, sub, selected, enabled) =
            (self.title, self.sub, self.selected, ui.is_enabled());
        // An option is selected or not (`aria-selected`), which egui's own
        // "toggled" does not say.
        response.widget_info(|| WidgetInfo::labeled(Role::ListBoxOption, enabled, title));
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_selected(selected);
            node.set_description(sub);
        });
        response
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Shape};

    use super::*;
    use crate::theme::tests::{TEXT, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::Harness;

    fn php() -> ListItem<'static> {
        ListItem::new("PHP", "PHP", "Default 8.4, 4 installed")
    }

    #[test]
    fn the_chosen_item_is_surface_with_a_line_border_and_the_others_have_neither() {
        for theme in [&LIGHT, &DARK] {
            let chosen = paint(theme, true, Look::rest());
            assert_eq!(chosen.fill, Some(theme.color.surface));
            assert_eq!(chosen.border, Some(theme.color.line));
            let rest = paint(theme, false, Look::rest());
            assert_eq!((rest.fill, rest.border), (None, None));
            // Under the pointer: `selected` at 50%, still no border.
            let hover = paint(theme, false, Look::hover());
            assert_eq!(
                hover.fill,
                Some(theme.color.selected.gamma_multiply(theme.state.row_hover))
            );
            assert_eq!(hover.border, None);
        }
    }

    #[test]
    fn the_name_and_the_line_under_it_read_on_the_list_and_on_the_chosen_fill() {
        for theme in [&LIGHT, &DARK] {
            for ground in [theme.color.bg, theme.color.surface, theme.color.selected] {
                assert!(contrast(theme.color.ink, ground) >= TEXT);
                assert!(contrast(theme.color.mute, ground) >= TEXT);
            }
        }
    }

    #[test]
    fn an_item_is_as_wide_as_the_list_and_as_high_as_a_two_line_row() {
        let harness = Harness::new(egui::Theme::Light);
        let mut size = Vec2::ZERO;
        harness.run(|ui| {
            ui.set_width(220.0);
            size = ui.add(php()).rect.size();
        });
        assert_eq!(size, vec2(220.0, LIGHT.size.row_max));
    }

    #[test]
    fn the_chosen_item_is_painted_and_a_focused_one_has_the_ring() {
        let harness = Harness::new(egui::Theme::Light);
        let rects = |item: ListItem<'static>| -> Vec<egui::epaint::RectShape> {
            harness
                .shapes(|ui| {
                    ui.add(item);
                })
                .into_iter()
                .filter_map(|shape| match shape {
                    Shape::Rect(rect) => Some(rect),
                    _ => None,
                })
                .collect()
        };
        let chosen = rects(php().selected(true));
        assert!(chosen.iter().any(|r| r.fill == LIGHT.color.surface));
        assert!(!rects(php()).iter().any(|r| r.fill == LIGHT.color.surface));
        let ring = |look: Look| {
            rects(php().looks(look)).iter().any(|r| {
                r.stroke_kind == StrokeKind::Outside && r.stroke.color == LIGHT.color.accent
            })
        };
        assert!(ring(Look::focused()));
        assert!(!ring(Look::hover()));
    }

    #[test]
    fn a_screen_reader_hears_an_option_that_is_selected_with_the_line_as_its_description() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            ui.add(php().selected(true));
            ui.add(ListItem::new("JS", "Node.js", "None installed"));
        });
        let php = tree.node_with_label("PHP").expect("the PHP option");
        assert_eq!(php.role(), Role::ListBoxOption);
        assert_eq!(php.is_selected(), Some(true));
        assert_eq!(php.description(), Some("Default 8.4, 4 installed"));
        let node = tree.node_with_label("Node.js").expect("the Node.js option");
        assert_eq!(node.is_selected(), Some(false));
    }

    #[test]
    fn a_long_line_is_cut_short_and_the_item_keeps_its_height() {
        let harness = Harness::new(egui::Theme::Light);
        let mut size = Vec2::ZERO;
        harness.run(|ui| {
            ui.set_width(120.0);
            let long = "a line that is much longer than the room the item gives it";
            size = ui.add(ListItem::new("Po", "postgres", long)).rect.size();
        });
        assert_eq!(size, vec2(120.0, LIGHT.size.row_max));
    }
}
