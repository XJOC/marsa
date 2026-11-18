//! Badges (components.md, Badges): the count after a sidebar item, the dot
//! on a folded sidebar item, and the pill on a site's row.
//!
//! The count is the number in the section color, 600 weight, when something
//! is new, and `mute` at 400 otherwise. The row badge is a 26 high pill in
//! the section tint with the section icon and the count; a click opens that
//! tab of the site.

use eframe::egui::{
    Color32, CornerRadius, Painter, Pos2, Rect, Response, Role, Sense, Stroke, StrokeKind, Ui,
    Vec2, Widget, WidgetInfo, pos2, vec2,
};

use super::icons::Icon;
use super::{Look, Touch, layout, paint_focus_ring, slot};
use crate::strings;
use crate::theme::{Direction, Face, Section, TextToken, Theme};

/// The most a badge counts to: more shows as "99+", so a pill stays small.
pub const MOST: u32 = 99;

/// The text of a count: the number, or "99+".
pub fn count_text(count: u32) -> String {
    if count > MOST {
        format!("{MOST}+")
    } else {
        count.to_string()
    }
}

/// Paints the dot of a folded sidebar item: `color`, with a ring in the
/// color of what is behind it, so it stands off the tile.
pub fn paint_dot(painter: &Painter, theme: &Theme, center: Pos2, color: Color32, ring: Color32) {
    // A rounded rect with the full radius is a circle: egui caps the radius
    // at half the shape.
    let circle = |size: f32, fill: Color32| {
        let rect = Rect::from_center_size(center, Vec2::splat(size));
        painter.rect_filled(rect, CornerRadius::same(theme.radius.full), fill);
    };
    circle(
        theme.size.dot + theme.size.dot_ring + theme.size.dot_ring,
        ring,
    );
    circle(theme.size.dot, color);
}

/// How the count after a sidebar item looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CountLook {
    pub color: Color32,
    pub face: Face,
}

/// `fresh`: something is new in the section since the user looked.
pub const fn count_look(theme: &Theme, section: Section, fresh: bool) -> CountLook {
    if fresh {
        CountLook {
            color: theme.section(section).fg,
            face: Face::SemiBold,
        }
    } else {
        CountLook {
            color: theme.color.mute,
            face: Face::Regular,
        }
    }
}

/// The count after a sidebar item: `3`, `12`, `99+`.
#[derive(Clone, Copy, Debug)]
#[must_use = "a count is drawn only when it is added to a Ui"]
pub struct CountBadge {
    section: Section,
    count: u32,
    fresh: bool,
}

impl CountBadge {
    pub const fn new(section: Section, count: u32) -> Self {
        Self {
            section,
            count,
            fresh: false,
        }
    }

    /// Something is new: the number takes the section's color.
    pub const fn fresh(mut self, fresh: bool) -> Self {
        self.fresh = fresh;
        self
    }
}

impl Widget for CountBadge {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let look = count_look(theme, self.section, self.fresh);
        let token = TextToken {
            size: theme.text.caption.size,
            face: look.face,
        };
        let text = count_text(self.count);
        let galley = layout(ui.painter(), &text, token, look.color);
        let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
        if ui.is_rect_visible(rect) {
            ui.painter().galley(rect.min, galley, look.color);
        }
        let label = strings::row_badge_label(self.section, self.count);
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &label));
        response
    }
}

/// A row badge in one state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowPaint {
    pub fill: Color32,
    /// The icon and the count.
    pub text: Color32,
    pub border: Option<Stroke>,
}

/// The paint of a row badge: the section's tint behind, its color on top.
/// Under the pointer the fill stays, because a tint darkened 6% or 12% no
/// longer holds 4.5:1 for the section color, and the section color outlines
/// the pill instead: 1 px on hover, 2 px when pressed.
pub fn row_paint(theme: &Theme, section: Section, touch: Touch) -> RowPaint {
    let colors = theme.section(section);
    let border = match touch {
        Touch::Rest => None,
        Touch::Hover => Some(Stroke::new(theme.size.border, colors.fg)),
        Touch::Pressed => Some(Stroke::new(theme.focus.width, colors.fg)),
    };
    RowPaint {
        fill: colors.tint,
        text: colors.fg,
        border,
    }
}

/// The pill on a site's row: the section icon and a count, such as the mail
/// a site got. A click opens that tab of the site: `Response::clicked`.
#[derive(Clone, Copy, Debug)]
#[must_use = "a badge is drawn only when it is added to a Ui"]
pub struct RowBadge<'a> {
    section: Section,
    count: u32,
    /// What a screen reader says: "3 new mails". From
    /// [`strings::row_badge_label`] when not set.
    label: Option<&'a str>,
    look: Option<Look>,
}

impl<'a> RowBadge<'a> {
    pub const fn new(section: Section, count: u32) -> Self {
        Self {
            section,
            count,
            label: None,
            look: None,
        }
    }

    pub const fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    /// Draws the badge as it looks in `look` (the gallery and the tests).
    pub const fn looks(mut self, look: Look) -> Self {
        self.look = Some(look);
        self
    }
}

impl Widget for RowBadge<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let token = TextToken {
            size: theme.text.caption.size,
            face: Face::Medium,
        };
        let text = count_text(self.count);
        let galley = layout(ui.painter(), &text, token, Color32::PLACEHOLDER);
        let icon = theme.size.icon_badge;
        let gap = theme.space.s1;
        let pad = theme.space.s2;
        let content = icon + gap + galley.size().x;
        let size = vec2(content + pad + pad, theme.size.badge);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        if ui.is_rect_visible(rect) {
            let look = self.look.unwrap_or_else(|| Look::of(&response));
            let paint = row_paint(theme, self.section, look.touch);
            let color = paint.text;
            let painter = ui.painter();
            let corner = theme.radius.full;
            painter.rect_filled(rect, CornerRadius::same(corner), paint.fill);
            if let Some(border) = paint.border {
                painter.rect_stroke(rect, CornerRadius::same(corner), border, StrokeKind::Inside);
            }
            let inner = rect.shrink2(vec2(pad, 0.0));
            let x = slot(direction, inner, 0.0, icon);
            let icon_spot = Rect::from_min_size(pos2(x, rect.top()), vec2(icon, size.y));
            Icon::of(self.section).paint(painter, theme, icon_spot, color);
            let x = slot(direction, inner, icon + gap, galley.size().x);
            let spot = Rect::from_min_size(pos2(x, rect.top()), vec2(galley.size().x, size.y));
            let at = eframe::egui::Align2::LEFT_CENTER.align_size_within_rect(galley.size(), spot);
            painter.galley(at.min, galley, color);
            if look.focused {
                paint_focus_ring(painter, theme, rect, corner);
            }
        }
        let label = self.label.map_or_else(
            || strings::row_badge_label(self.section, self.count),
            str::to_owned,
        );
        response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
        response
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Rect};

    use super::*;
    use crate::theme::tests::{TEXT, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::Harness;

    #[test]
    fn a_count_shows_the_number_up_to_99_and_then_99_plus() {
        assert_eq!(count_text(0), "0");
        assert_eq!(count_text(3), "3");
        assert_eq!(count_text(99), "99");
        assert_eq!(count_text(100), "99+");
        assert_eq!(count_text(u32::MAX), "99+");
    }

    #[test]
    fn something_new_is_the_section_color_at_600_and_the_rest_is_mute_at_400() {
        for theme in [&LIGHT, &DARK] {
            for section in Section::ALL {
                let fresh = count_look(theme, section, true);
                assert_eq!(fresh.color, theme.section(section).fg);
                assert_eq!(fresh.face, Face::SemiBold);
                let seen = count_look(theme, section, false);
                assert_eq!(seen.color, theme.color.mute);
                assert_eq!(seen.face, Face::Regular);
            }
        }
    }

    #[test]
    fn a_count_reads_on_the_sidebar_and_on_its_selected_item() {
        for theme in [&LIGHT, &DARK] {
            for section in Section::ALL {
                for fresh in [true, false] {
                    let color = count_look(theme, section, fresh).color;
                    let ratio = contrast(color, theme.color.side);
                    assert!(
                        ratio >= TEXT,
                        "{section:?} fresh {fresh} on side: {ratio:.2}:1"
                    );
                    let ratio = contrast(color, theme.color.selected);
                    assert!(
                        ratio >= TEXT,
                        "{section:?} fresh {fresh} on selected: {ratio:.2}:1"
                    );
                }
            }
        }
    }

    #[test]
    fn a_row_badge_is_the_section_tint_under_the_section_color_and_an_outline_when_touched() {
        for theme in [&LIGHT, &DARK] {
            for section in Section::ALL {
                let colors = theme.section(section);
                for touch in [Touch::Rest, Touch::Hover, Touch::Pressed] {
                    let paint = row_paint(theme, section, touch);
                    assert_eq!(paint.fill, colors.tint);
                    assert_eq!(paint.text, colors.fg);
                }
                assert_eq!(row_paint(theme, section, Touch::Rest).border, None);
                let hover = row_paint(theme, section, Touch::Hover).border;
                assert_eq!(hover, Some(Stroke::new(1.0, colors.fg)));
                let pressed = row_paint(theme, section, Touch::Pressed).border;
                assert_eq!(pressed, Some(Stroke::new(2.0, colors.fg)));
            }
        }
    }

    #[test]
    fn a_row_badge_text_reads_on_its_tint_in_every_state() {
        for theme in [&LIGHT, &DARK] {
            for section in Section::ALL {
                for touch in [Touch::Rest, Touch::Hover, Touch::Pressed] {
                    let paint = row_paint(theme, section, touch);
                    // tokens.md: light Services is 4.49:1 on its tint, for
                    // icons and bold text; the pill is 500 and has an icon.
                    let ratio = contrast(paint.text, paint.fill);
                    assert!(ratio >= TEXT - 0.02, "{section:?} {touch:?}: {ratio:.2}:1");
                }
            }
        }
    }

    #[test]
    fn a_row_badge_is_26_high_a_pill_and_wider_for_more_digits() {
        let harness = Harness::new(egui::Theme::Light);
        let mut rects = Vec::new();
        harness.run(|ui| {
            for count in [3, 99, 1000] {
                rects.push(ui.add(RowBadge::new(Section::Mail, count)).rect);
            }
        });
        for rect in &rects {
            assert_eq!(rect.height(), LIGHT.size.badge);
        }
        assert!(rects[1].width() > rects[0].width());
        // "99+" is as wide as the capped text, whatever the number.
        assert!(rects[2].width() > rects[1].width());
        assert_eq!(LIGHT.size.badge, 26.0);
    }

    #[test]
    fn a_row_badge_is_a_button_with_a_name_for_a_screen_reader() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            ui.add(RowBadge::new(Section::Mail, 3));
            ui.add(RowBadge::new(Section::Dumps, 1));
            ui.add(RowBadge::new(Section::Logs, 12).label("12 errors"));
        });
        for name in ["3 new mails", "1 dump", "12 errors"] {
            let node = tree.node_with_label(name).expect(name);
            assert_eq!(node.role(), egui::Role::Button);
        }
    }

    #[test]
    fn a_click_on_a_row_badge_is_reported() {
        let harness = Harness::new(egui::Theme::Light);
        let mut rect = Rect::NOTHING;
        harness.run(|ui| rect = ui.add(RowBadge::new(Section::Mail, 3)).rect);
        let mut clicked = false;
        harness.click(rect.center(), |ui| {
            clicked |= ui.add(RowBadge::new(Section::Mail, 3)).clicked();
        });
        assert!(clicked);
    }

    #[test]
    fn a_count_is_text_sized_and_a_dot_is_the_token_size() {
        let harness = Harness::new(egui::Theme::Light);
        let mut size = Vec2::ZERO;
        harness.run(|ui| size = ui.add(CountBadge::new(Section::Sites, 6)).rect.size());
        assert!(size.x > 0.0 && size.y > 0.0);
        assert_eq!(LIGHT.size.dot, 8.0);
        assert_eq!(LIGHT.size.dot_ring, 2.0);
    }
}
