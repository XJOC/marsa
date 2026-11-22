//! The pieces of the menu-bar panel (components.md, Menu-bar panel): the
//! frame of the borderless window, a site with its key, a footer link, a
//! divider, and the Xdebug row (a switch with a line under its name). The
//! panel is the one place that uses them: the status dot, the buttons, the
//! version picker, and the switch come from the other widgets.
//!
//! None of them reads the model. A screen adds them and turns the response
//! into an action (code.md, Widgets).

use eframe::egui::text::{LayoutJob, TextWrapping};
use eframe::egui::{
    Align2, CornerRadius, Frame, InnerResponse, Rect, Response, Role, Sense, Stroke, Ui, UiBuilder,
    Widget, WidgetInfo, pos2, vec2,
};

use super::{Look, Switch, layout, paint_focus_ring, quiet_fill, slot, spot, with_disabled_reason};
use crate::theme::{Direction, Theme};

/// The frame of the panel: `surface`, a 1 px `line` border, and the corner
/// of `radius.xl`, as wide as `size.panel_width`. The window around it is
/// transparent, so the corners show what is behind. The shadow of
/// `elevation.2` is the system's: a painted one needs a transparent margin
/// around the window, and a click in that margin would not close the panel.
pub fn frame<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    let theme = Theme::of(ui.ctx());
    let border = theme.size.border;
    let shown = Frame::new()
        .fill(theme.color.surface)
        .stroke(Stroke::new(border, theme.color.line))
        .corner_radius(CornerRadius::same(theme.radius.xl));
    shown.show(ui, |ui| {
        // The border is drawn outside the content: 1 px on each side.
        ui.set_width(theme.size.panel_width - border - border);
        // The sections sit flush against each other: each has its own room.
        ui.spacing_mut().item_spacing.y = 0.0;
        add(ui)
    })
}

/// A 1 px `line` across the panel, between two sections.
pub fn divider(ui: &mut Ui) {
    let theme = Theme::of(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme.size.border),
        Sense::hover(),
    );
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 0.0, theme.color.line);
    }
}

/// The line between the two links of the footer: 1 px `line`, as high as a
/// link.
pub fn footer_divider(ui: &mut Ui) {
    let theme = Theme::of(ui.ctx());
    let size = vec2(theme.size.border, footer_link_height(theme));
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, 0.0, theme.color.line);
    }
}

/// How high a footer link is: the footer less its room above and below.
pub fn footer_link_height(theme: &Theme) -> f32 {
    theme.size.panel_footer - theme.space.s1 - theme.space.s1
}

/// A site in the panel: its key (1 to 9) in `mute` mono, its name at 500,
/// and a caption at the end ("PHP 8.4"). A click, or the key, opens it in
/// the browser. A screen reader hears a link named `label`.
#[derive(Clone, Copy, Debug)]
#[must_use = "a site row is drawn only when it is added to a Ui"]
pub struct SiteRow<'a> {
    key: &'a str,
    name: &'a str,
    caption: &'a str,
    label: &'a str,
    look: Option<Look>,
}

impl<'a> SiteRow<'a> {
    pub const fn new(key: &'a str, name: &'a str, caption: &'a str, label: &'a str) -> Self {
        Self {
            key,
            name,
            caption,
            label,
            look: None,
        }
    }

    /// Draws the row as it looks in `look` (the tests).
    pub const fn looks(mut self, look: Look) -> Self {
        self.look = Some(look);
        self
    }
}

impl Widget for SiteRow<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let size = vec2(ui.available_width(), theme.size.panel_row);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        if ui.is_rect_visible(rect) {
            let look = self.look.unwrap_or_else(|| Look::of(&response));
            let painter = ui.painter();
            let corner = CornerRadius::same(theme.radius.md);
            if let Some(fill) = quiet_fill(theme, look.touch) {
                painter.rect_filled(rect, corner, fill);
            }
            let (pad, gap) = (theme.space.s3, theme.space.s3);
            let line = |x: f32, width: f32| spot(x, rect.top(), width, rect.height());
            // The key, centered in its column.
            let key = layout(painter, self.key, theme.text.mono_sm, theme.color.mute);
            let column = line(
                slot(direction, rect, pad, theme.size.panel_key),
                theme.size.panel_key,
            );
            let at = Align2::CENTER_CENTER.align_size_within_rect(key.size(), column);
            painter.galley(at.min, key, theme.color.mute);
            // The caption at the end, then the name in what is left.
            let caption = layout(painter, self.caption, theme.text.caption, theme.color.mute);
            let end = line(
                slot(
                    direction,
                    rect,
                    rect.width() - pad - caption.size().x,
                    caption.size().x,
                ),
                caption.size().x,
            );
            let at = Align2::CENTER_CENTER.align_size_within_rect(caption.size(), end);
            let caption_width = caption.size().x;
            painter.galley(at.min, caption, theme.color.mute);
            let start = pad + theme.size.panel_key + gap;
            let room = (rect.width() - start - gap - caption_width - pad).max(0.0);
            let mut job = LayoutJob::simple_singleline(
                self.name.to_owned(),
                theme.text.body_strong.font_id(),
                theme.color.ink,
            );
            job.wrap = TextWrapping::truncate_at_width(room);
            let name = painter.layout_job(job);
            let spot = line(slot(direction, rect, start, name.size().x), name.size().x);
            let at = Align2::CENTER_CENTER.align_size_within_rect(name.size(), spot);
            painter.galley(at.min, name, theme.color.ink);
            if look.focused {
                paint_focus_ring(painter, theme, rect, theme.radius.md);
            }
        }
        let (label, enabled) = (self.label, ui.is_enabled());
        response.widget_info(|| WidgetInfo::labeled(Role::Link, enabled, label));
        response
    }
}

/// A link of the footer, as wide as `width`: "Stop Marsa" and "Quit". `ink2`
/// text, and the fill of a quiet control under the pointer.
#[derive(Clone, Copy, Debug)]
#[must_use = "a footer link is drawn only when it is added to a Ui"]
pub struct FooterLink<'a> {
    label: &'a str,
    width: f32,
    disabled: Option<&'a str>,
    tooltip: Option<&'a str>,
    look: Option<Look>,
}

impl<'a> FooterLink<'a> {
    pub const fn new(label: &'a str, width: f32) -> Self {
        Self {
            label,
            width,
            disabled: None,
            tooltip: None,
            look: None,
        }
    }

    /// Says what the link does, under the pointer, when it is not disabled.
    pub const fn tooltip(mut self, text: &'a str) -> Self {
        self.tooltip = Some(text);
        self
    }

    /// Disables the link. Hovering it shows `reason`.
    pub const fn disabled_if(mut self, condition: bool, reason: &'a str) -> Self {
        if condition {
            self.disabled = Some(reason);
        }
        self
    }

    /// Draws the link as it looks in `look` (the tests).
    pub const fn looks(mut self, look: Look) -> Self {
        self.look = Some(look);
        self
    }

    fn show(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let height = footer_link_height(theme);
        let (rect, response) = ui.allocate_exact_size(vec2(self.width, height), Sense::click());
        if ui.is_rect_visible(rect) {
            let look = match self.look {
                Some(look) => look,
                None if ui.is_enabled() => Look::of(&response),
                None => Look::rest(),
            };
            let painter = ui.painter();
            if let Some(fill) = quiet_fill(theme, look.touch) {
                painter.rect_filled(rect, CornerRadius::same(theme.radius.md), fill);
            }
            let text = layout(painter, self.label, theme.text.body_sm, theme.color.ink2);
            let at = Align2::CENTER_CENTER.align_size_within_rect(text.size(), rect);
            painter.galley(at.min, text, theme.color.ink2);
            if look.focused {
                paint_focus_ring(painter, theme, rect, theme.radius.md);
            }
        }
        let (label, enabled) = (self.label, ui.is_enabled());
        response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
        response
    }
}

impl Widget for FooterLink<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let (reason, tooltip) = (self.disabled, self.tooltip);
        if reason.is_none() {
            let response = self.show(ui);
            return match tooltip {
                Some(text) => response.on_hover_text(text),
                None => response,
            };
        }
        let response = ui.add_enabled_ui(false, |ui| self.show(ui)).inner;
        with_disabled_reason(response, reason)
    }
}

/// A switch with a name and a line under it: Xdebug in the panel. The name
/// and the line are at the start, the track at the end. A click on the text
/// flips it as a click on the track does.
#[derive(Clone, Copy, Debug)]
#[must_use = "a switch row is drawn only when it is added to a Ui"]
pub struct SwitchRow<'a> {
    on: bool,
    label: &'a str,
    caption: &'a str,
    disabled: Option<&'a str>,
}

impl<'a> SwitchRow<'a> {
    pub const fn new(on: bool, label: &'a str, caption: &'a str) -> Self {
        Self {
            on,
            label,
            caption,
            disabled: None,
        }
    }

    /// Disables the switch. Hovering it shows `reason`.
    pub const fn disabled_if(mut self, condition: bool, reason: &'a str) -> Self {
        if condition {
            self.disabled = Some(reason);
        }
        self
    }

    /// Draws the row. `true` when the user asked to flip the switch, on the
    /// track or on the text.
    pub fn show(self, ui: &mut Ui) -> bool {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let name = layout(ui.painter(), self.label, theme.text.body, theme.color.ink);
        let caption = layout(
            ui.painter(),
            self.caption,
            theme.text.caption,
            theme.color.mute,
        );
        let stack = name.size().y + caption.size().y;
        let height = stack.max(theme.size.switch.y).max(theme.size.target_min);
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
        // The track, at the end of the row.
        let mut end = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(direction.row_from_end()),
        );
        let track = Switch::new(self.on, self.label)
            .bare()
            .disabled_if(self.disabled.is_some(), self.disabled.unwrap_or_default());
        let track = end.add(track);
        // The text, at the start, a click target of its own (not a Tab stop:
        // the track is).
        let words = Rect::from_min_size(
            pos2(
                slot(direction, rect, 0.0, name.size().x.max(caption.size().x)),
                rect.top(),
            ),
            vec2(name.size().x.max(caption.size().x), height),
        );
        let top = Align2::LEFT_CENTER
            .align_size_within_rect(vec2(words.width(), stack), words)
            .min;
        let below = top + vec2(0.0, name.size().y);
        let painter = ui.painter();
        painter.galley(top, name, theme.color.ink);
        painter.galley(below, caption, theme.color.mute);
        let text = ui.interact(
            words,
            ui.scope_id().with(("switch_row", self.label)),
            Sense::CLICK,
        );
        track.clicked() || (self.disabled.is_none() && text.clicked())
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Shape};

    use super::*;
    use crate::theme::tests::{TEXT, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::Harness;

    #[test]
    fn the_frame_is_as_wide_as_the_panel_with_the_corner_of_a_dialog() {
        for (theme, mode) in [(&LIGHT, egui::Theme::Light), (&DARK, egui::Theme::Dark)] {
            let harness = Harness::new(mode);
            let mut rect = Rect::NOTHING;
            let shapes = harness.shapes(|ui| {
                rect = frame(ui, |ui| ui.allocate_space(vec2(10.0, 20.0)).1)
                    .response
                    .rect;
            });
            assert_eq!(rect.width(), theme.size.panel_width);
            let corner = shapes.iter().find_map(|shape| match shape {
                Shape::Rect(rect) if rect.fill == theme.color.surface => Some(rect.corner_radius),
                _ => None,
            });
            assert_eq!(corner, Some(CornerRadius::same(theme.radius.xl)));
        }
    }

    #[test]
    fn a_site_row_is_42_high_with_its_key_its_name_and_a_caption() {
        let harness = Harness::new(egui::Theme::Light);
        let mut rect = Rect::NOTHING;
        let tree = harness.accessibility(|ui| {
            ui.set_width(344.0);
            rect = ui
                .add(SiteRow::new(
                    "3",
                    "billing-api",
                    "PHP 8.4",
                    "Open billing-api",
                ))
                .rect;
        });
        assert_eq!(rect.height(), LIGHT.size.panel_row);
        assert_eq!(rect.width(), 344.0);
        let node = tree.node_with_label("Open billing-api").expect("a link");
        assert_eq!(node.role(), Role::Link);
    }

    #[test]
    fn a_site_row_has_its_key_at_the_start_its_caption_at_the_end_and_cuts_a_long_name() {
        let harness = Harness::new(egui::Theme::Light);
        let long = "a-very-long-site-name-that-cannot-fit-in-a-panel-of-this-width-at-all";
        let texts = |name: &str| -> Vec<Rect> {
            harness
                .shapes(|ui| {
                    ui.set_width(344.0);
                    ui.add(SiteRow::new("1", name, "PHP 8.4", "Open"));
                })
                .iter()
                .filter_map(|shape| match shape {
                    Shape::Text(text) => Some(text.visual_bounding_rect()),
                    _ => None,
                })
                .collect()
        };
        for name in ["shop", long] {
            // The key, the caption, the name: in the order they are drawn.
            let [key, caption, shown] = texts(name)[..] else {
                panic!("three texts");
            };
            let pad = LIGHT.space.s3;
            // The key is centered in its column at the start.
            assert!(key.center().x > pad && key.center().x < pad + LIGHT.size.panel_key);
            // The caption ends `pad` from the end; nothing sticks out.
            assert!((344.0 - pad - caption.right()).abs() < 1.0, "{caption:?}");
            let start = pad + LIGHT.size.panel_key + pad;
            assert!((shown.left() - start).abs() < 1.0, "{shown:?}");
            assert!(
                shown.right() <= caption.left(),
                "{name}: {shown:?} {caption:?}"
            );
        }
    }

    #[test]
    fn a_site_row_under_the_pointer_has_the_quiet_fill_and_its_text_still_reads() {
        for theme in [&LIGHT, &DARK] {
            let hover = quiet_fill(theme, crate::widgets::Touch::Hover).expect("a fill");
            let page = theme.color.surface;
            let seen = crate::widgets::harness::over(hover, page);
            for ink in [theme.color.ink, theme.color.mute, theme.color.ink2] {
                assert!(contrast(ink, seen) >= TEXT, "{ink:?} on {seen:?}");
            }
        }
    }

    #[test]
    fn a_footer_link_is_as_wide_as_asked_and_says_why_when_disabled() {
        let harness = Harness::new(egui::Theme::Light);
        let mut rect = Rect::NOTHING;
        let tree = harness.accessibility(|ui| {
            rect = ui.add(FooterLink::new("Stop Marsa", 170.0)).rect;
            ui.add(FooterLink::new("Quit", 170.0).disabled_if(true, "Not now"));
        });
        assert_eq!(rect.width(), 170.0);
        assert_eq!(rect.height(), footer_link_height(&LIGHT));
        assert!(rect.height() >= LIGHT.size.target_min);
        let quit = tree.node_with_label("Quit").expect("a button");
        assert_eq!(quit.role(), Role::Button);
        assert!(quit.is_disabled());
        assert!(
            !tree
                .node_with_label("Stop Marsa")
                .expect("a button")
                .is_disabled()
        );
    }

    #[test]
    fn a_footer_link_says_what_it_does_under_the_pointer() {
        let harness = Harness::new(egui::Theme::Light);
        // No wait for the tooltip, and none for the pointer to be still.
        harness.ctx.all_styles_mut(|style| {
            style.interaction.tooltip_delay = 0.0;
            style.interaction.show_tooltips_only_when_still = false;
        });
        let words = "Closes this icon. Sites keep working.";
        let link = |ui: &mut Ui| {
            ui.add(FooterLink::new("Quit", 170.0).tooltip(words));
        };
        let texts = |shapes: Vec<Shape>| -> Vec<String> {
            shapes
                .iter()
                .filter_map(|shape| match shape {
                    Shape::Text(text) => Some(text.galley.text().to_owned()),
                    _ => None,
                })
                .collect()
        };
        harness.run(link);
        // Away from the link: no tooltip.
        harness.hover(egui::pos2(300.0, 300.0), link);
        assert!(!texts(harness.shapes(link)).contains(&words.to_owned()));
        // Over it: the words.
        harness.hover(egui::pos2(40.0, 15.0), link);
        harness.hover(egui::pos2(41.0, 15.0), link);
        let over = texts(harness.shapes(link));
        assert!(over.contains(&words.to_owned()), "{over:?}");
    }

    #[test]
    fn a_click_on_the_footer_link_reports_it() {
        let harness = Harness::new(egui::Theme::Light);
        let mut clicked = false;
        // One frame first, so the pointer finds the link.
        harness.run(|ui| {
            ui.add(FooterLink::new("Quit", 170.0));
        });
        harness.click(egui::pos2(40.0, 15.0), |ui| {
            clicked |= ui.add(FooterLink::new("Quit", 170.0)).clicked();
        });
        assert!(clicked);
    }

    #[test]
    fn the_switch_row_flips_on_a_click_on_its_text_as_on_the_track() {
        let harness = Harness::new(egui::Theme::Light);
        let mut rect = Rect::NOTHING;
        harness.run(|ui| {
            ui.set_width(328.0);
            let start = ui.cursor().top();
            SwitchRow::new(false, "Xdebug", "PHP runs slower while it is on").show(ui);
            rect = Rect::from_min_max(
                egui::pos2(ui.min_rect().left(), start),
                egui::pos2(ui.min_rect().right(), ui.cursor().top()),
            );
        });
        assert!(rect.height() >= LIGHT.size.target_min);
        for at in [
            // The text, at the start.
            egui::pos2(rect.left() + 10.0, rect.center().y),
            // The track, at the end.
            egui::pos2(rect.right() - 10.0, rect.center().y),
        ] {
            let mut clicked = false;
            harness.click(at, |ui| {
                ui.set_width(328.0);
                clicked |=
                    SwitchRow::new(false, "Xdebug", "PHP runs slower while it is on").show(ui);
            });
            assert!(clicked, "{at:?}");
        }
    }

    #[test]
    fn a_disabled_switch_row_does_not_flip_from_its_text() {
        let harness = Harness::new(egui::Theme::Light);
        let mut clicked = false;
        harness.click(egui::pos2(10.0, 12.0), |ui| {
            ui.set_width(328.0);
            clicked |= SwitchRow::new(false, "Xdebug", "PHP runs slower while it is on")
                .disabled_if(true, "Marsa has not read it yet.")
                .show(ui);
        });
        assert!(!clicked);
    }

    #[test]
    fn the_divider_is_one_line_wide() {
        let harness = Harness::new(egui::Theme::Light);
        let mut height = 0.0;
        harness.run(|ui| {
            ui.set_width(100.0);
            let before = ui.cursor().top();
            divider(ui);
            height = ui.cursor().top() - before;
        });
        assert!(height >= LIGHT.size.border);
    }
}
