//! Tags (components.md, Tags): a small framed word, such as NULL in a grid
//! cell, "Read-only" in the SQL bar, or a column's key. The frame, not a
//! color alone, tells a tag from text.

use eframe::egui::{
    Align2, CornerRadius, Painter, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget,
    vec2,
};

use crate::theme::Theme;

/// A framed word, `height` high (`size.tag` or `size.tag_in_grid`).
pub struct Tag<'a> {
    word: &'a str,
    height: f32,
}

impl<'a> Tag<'a> {
    /// A tag of the usual height.
    pub fn new(word: &'a str, theme: &Theme) -> Self {
        Self {
            word,
            height: theme.size.tag,
        }
    }

    /// A tag that fits a grid cell.
    pub fn in_grid(word: &'a str, theme: &Theme) -> Self {
        Self {
            word,
            height: theme.size.tag_in_grid,
        }
    }
}

impl Widget for Tag<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let size = vec2(width(ui.painter(), theme, self.word), self.height);
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
        if ui.is_rect_visible(rect) {
            paint(ui.painter(), theme, rect.min, self.word, self.height);
        }
        response.on_hover_text(self.word)
    }
}

fn width(painter: &Painter, theme: &Theme, word: &str) -> f32 {
    let font = theme.text.caption.font_id();
    let galley = painter.layout_no_wrap(word.to_owned(), font, theme.color.ink2);
    galley.size().x + theme.space.s2 + theme.space.s2
}

/// Draws a tag with its top-left at `min`, for a widget that lays out its
/// own cells, such as the grid.
pub(crate) fn paint(painter: &Painter, theme: &Theme, min: Pos2, word: &str, height: f32) {
    let font = theme.text.caption.font_id();
    let galley = painter.layout_no_wrap(word.to_owned(), font, theme.color.ink2);
    let size = vec2(galley.size().x + theme.space.s2 + theme.space.s2, height);
    let rect = Rect::from_min_size(min, size);
    let stroke = Stroke::new(theme.size.border, theme.color.line);
    painter.rect_stroke(
        rect,
        CornerRadius::same(theme.radius.sm),
        stroke,
        StrokeKind::Inside,
    );
    let at = Align2::CENTER_CENTER.anchor_size(rect.center(), galley.size());
    painter.galley(at.min, galley, theme.color.ink2);
}
