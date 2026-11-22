//! Buttons (components.md, Buttons): primary, secondary, danger, quiet, and
//! icon. One struct with a builder: `ui.add(Button::primary("Add site…"))`.
//! (Laravel: a Blade component with props; `->disabled('why')` is a prop.)
//!
//! Every state is drawn here and nowhere else: rest, hover (6% toward `ink`),
//! pressed (12%), the focus ring, and disabled (55% opacity, a tooltip that
//! says why). A button whose action opens a dialog or a menu ends its label
//! with "…": that is the caller's text.

use std::sync::Arc;

use eframe::egui::text::Galley;
use eframe::egui::{
    Align2, Color32, CornerRadius, Rect, Response, Role, Sense, Stroke, StrokeKind, Ui, Widget,
    WidgetInfo, vec2,
};

use super::icons::Icon;
use super::{Look, Touch, layout, paint_focus_ring, quiet_fill, slot, spot, touched};
use crate::theme::{Direction, Face, TextToken, Theme};

/// What kind of button this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `accent` fill, `on_accent` text, 500 weight: the one main action of a
    /// screen or a dialog.
    Primary,
    /// `surface` fill, a 1 px `control` border, `ink` text: other actions.
    Secondary,
    /// `error` fill, `on_error` text, 600 weight: the confirm button of a
    /// destructive dialog, and the fix in an error banner.
    Danger,
    /// No fill, `link` or `error` text: rare or destructive entry points.
    Quiet,
    /// 30 by 30, no fill, a `mute` icon: row menus. Always has a label for
    /// screen readers and a tooltip.
    Icon,
}

/// The text color of a quiet button.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QuietTone {
    /// The link color (`accent`).
    #[default]
    Link,
    /// `error`: for something destructive, such as "Unlink this site…".
    Error,
}

/// What a button is made of in one state, before it is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    /// `None`: no fill.
    pub fill: Option<Color32>,
    /// `None`: no border.
    pub border: Option<Color32>,
    /// The text, or the icon of an icon button.
    pub text: Color32,
}

/// The colors of a button of `kind` while `touch` happens to it.
pub fn colors(theme: &Theme, kind: Kind, tone: QuietTone, touch: Touch) -> Colors {
    let c = &theme.color;
    // A button with no fill shows `selected` under the pointer, as a table
    // row does (see `quiet_fill`).
    let no_fill = || quiet_fill(theme, touch);
    match kind {
        Kind::Primary => Colors {
            fill: Some(touched(theme, c.accent, touch)),
            border: None,
            text: c.on_accent,
        },
        Kind::Secondary => Colors {
            fill: Some(touched(theme, c.surface, touch)),
            border: Some(c.control),
            text: c.ink,
        },
        Kind::Danger => Colors {
            fill: Some(touched(theme, c.error, touch)),
            border: None,
            text: c.on_error,
        },
        Kind::Quiet => Colors {
            fill: no_fill(),
            border: None,
            text: match tone {
                QuietTone::Link => c.accent,
                QuietTone::Error => c.error,
            },
        },
        Kind::Icon => Colors {
            fill: no_fill(),
            border: None,
            text: c.mute,
        },
    }
}

/// A button. Build it with [`Button::primary`] and the others, then add it:
/// `ui.add(Button::secondary("Open folder").disabled("No folder yet"))`.
/// `Response::clicked` tells that the user pressed it.
#[derive(Clone, Debug)]
/// What [`Button::measure`] finds.
struct Measure {
    galley: Option<Arc<Galley>>,
    text_width: f32,
    icon_width: f32,
    between: f32,
    content_width: f32,
    width: f32,
}

#[must_use = "a button does nothing until it is added to a Ui"]
pub struct Button<'a> {
    kind: Kind,
    /// The text. For an icon button: its name for screen readers and tooltip.
    label: &'a str,
    /// Before the text; the glyph of an icon button.
    icon: Option<Icon>,
    tone: QuietTone,
    in_row: bool,
    disabled: Option<&'a str>,
    look: Option<Look>,
}

impl<'a> Button<'a> {
    const fn new(kind: Kind, label: &'a str) -> Self {
        Self {
            kind,
            label,
            icon: None,
            tone: QuietTone::Link,
            in_row: false,
            disabled: None,
            look: None,
        }
    }

    pub const fn primary(label: &'a str) -> Self {
        Self::new(Kind::Primary, label)
    }

    pub const fn secondary(label: &'a str) -> Self {
        Self::new(Kind::Secondary, label)
    }

    pub const fn danger(label: &'a str) -> Self {
        Self::new(Kind::Danger, label)
    }

    pub const fn quiet(label: &'a str) -> Self {
        Self::new(Kind::Quiet, label)
    }

    /// An icon button: `label` is what a screen reader says and what the
    /// tooltip shows.
    pub const fn icon(icon: Icon, label: &'a str) -> Self {
        let mut button = Self::new(Kind::Icon, label);
        button.icon = Some(icon);
        button
    }

    /// An icon before the text.
    pub const fn leading(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The text color of a quiet button.
    pub const fn tone(mut self, tone: QuietTone) -> Self {
        self.tone = tone;
        self
    }

    /// 30 high, for a button inside a table row.
    pub const fn in_row(mut self) -> Self {
        self.in_row = true;
        self
    }

    /// Disables the button. Hovering it shows `reason` ("Starts and stops
    /// with the sites that need it").
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }

    /// [`Button::disabled`] when `condition` holds.
    pub const fn disabled_if(self, condition: bool, reason: &'a str) -> Self {
        if condition {
            self.disabled(reason)
        } else {
            self
        }
    }

    /// Draws the button as it looks in `look`, whatever the pointer and the
    /// keyboard do: for the design gallery and the tests.
    pub const fn looks(mut self, look: Look) -> Self {
        self.look = Some(look);
        self
    }

    pub const fn kind(&self) -> Kind {
        self.kind
    }

    /// The tooltip: the reason when disabled, the label of an icon button,
    /// and none otherwise.
    pub const fn tooltip(&self) -> Option<&'a str> {
        match (self.disabled, self.kind) {
            (Some(reason), _) => Some(reason),
            (None, Kind::Icon) => Some(self.label),
            (None, _) => None,
        }
    }

    /// 32, or 30 in a table row; an icon button is 30 by 30.
    pub fn height(&self, theme: &Theme) -> f32 {
        match self.kind {
            Kind::Icon => theme.size.icon_button,
            _ if self.in_row => theme.size.control_in_row,
            _ => theme.size.control,
        }
    }

    /// The space before and after the content: 16 for the main buttons, 12
    /// for the others (components.md: 12 to 16).
    pub const fn padding(&self, theme: &Theme) -> f32 {
        match self.kind {
            Kind::Primary | Kind::Danger => theme.space.s4,
            _ => theme.space.s3,
        }
    }

    /// The weight of the label.
    pub const fn face(&self) -> Face {
        match self.kind {
            Kind::Primary => Face::Medium,
            Kind::Danger => Face::SemiBold,
            _ => Face::Regular,
        }
    }

    /// The text laid out, and the widths of the parts of the button.
    fn measure(&self, ui: &Ui, theme: &Theme) -> Measure {
        let token = TextToken {
            size: theme.text.body_sm.size,
            face: self.face(),
        };
        let galley = (self.kind != Kind::Icon)
            .then(|| layout(ui.painter(), self.label, token, Color32::PLACEHOLDER));
        let text_width = galley.as_ref().map_or(0.0, |galley| galley.size().x);
        let icon_width = if self.icon.is_some() {
            theme.size.icon_inline
        } else {
            0.0
        };
        let between = if self.icon.is_some() && galley.is_some() {
            theme.space.s2
        } else {
            0.0
        };
        let content_width = icon_width + between + text_width;
        let width = match self.kind {
            Kind::Icon => self.height(theme),
            _ => content_width + self.padding(theme) + self.padding(theme),
        };
        Measure {
            galley,
            text_width,
            icon_width,
            between,
            content_width,
            width,
        }
    }

    /// How wide the button is when added to `ui`: a container that places
    /// buttons by hand (a notice) reserves the room first.
    pub fn width(&self, ui: &Ui) -> f32 {
        self.measure(ui, Theme::of(ui.ctx())).width
    }

    fn show(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let height = self.height(theme);
        let Measure {
            galley,
            text_width,
            icon_width,
            between,
            content_width,
            width,
        } = self.measure(ui, theme);
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
        if ui.is_rect_visible(rect) {
            let look = match self.look {
                Some(look) => look,
                None if ui.is_enabled() => Look::of(&response),
                None => Look::rest(),
            };
            let paint = colors(theme, self.kind, self.tone, look.touch);
            let radius = theme.radius.md;
            let corner = CornerRadius::same(radius);
            let painter = ui.painter();
            if let Some(fill) = paint.fill {
                painter.rect_filled(rect, corner, fill);
            }
            if let Some(border) = paint.border {
                painter.rect_stroke(
                    rect,
                    corner,
                    Stroke::new(theme.size.border, border),
                    StrokeKind::Inside,
                );
            }
            let content = Rect::from_center_size(rect.center(), vec2(content_width, height));
            if let Some(icon) = self.icon {
                let x = slot(direction, content, 0.0, icon_width);
                // The icon centres itself in a slot as high as the button.
                let spot = spot(x, rect.top(), icon_width, height);
                icon.paint(painter, theme, spot, paint.text);
            }
            if let Some(galley) = galley {
                let x = slot(direction, content, icon_width + between, text_width);
                let spot = spot(x, rect.top(), text_width, height);
                let at = Align2::LEFT_CENTER.align_size_within_rect(galley.size(), spot);
                painter.galley(at.min, galley, paint.text);
            }
            if look.focused {
                paint_focus_ring(painter, theme, rect, radius);
            }
        }
        let enabled = ui.is_enabled();
        let label = self.label;
        response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
        response
    }
}

impl Widget for Button<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let tooltip = self.tooltip();
        if self.disabled.is_some() {
            // A disabled child Ui draws at 55% opacity and gives a response
            // that shows the tooltip of a disabled widget.
            let response = ui.add_enabled_ui(false, |ui| self.show(ui)).inner;
            return match tooltip {
                Some(reason) => response.on_disabled_hover_text(reason),
                None => response,
            };
        }
        let response = self.show(ui);
        match tooltip {
            Some(text) => response.on_hover_text(text),
            None => response,
        }
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Rect};

    use super::*;
    use crate::theme::tests::{TEXT, UI, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::{Harness, over};

    const KINDS: [Kind; 5] = [
        Kind::Primary,
        Kind::Secondary,
        Kind::Danger,
        Kind::Quiet,
        Kind::Icon,
    ];
    const TOUCHES: [Touch; 3] = [Touch::Rest, Touch::Hover, Touch::Pressed];
    const TONES: [QuietTone; 2] = [QuietTone::Link, QuietTone::Error];

    #[test]
    fn each_kind_takes_its_fill_border_and_text_from_the_tokens() {
        for theme in [&LIGHT, &DARK] {
            let c = &theme.color;
            let rest = |kind, tone| colors(theme, kind, tone, Touch::Rest);
            let primary = rest(Kind::Primary, QuietTone::Link);
            assert_eq!(primary.fill, Some(c.accent));
            assert_eq!(primary.text, c.on_accent);
            assert_eq!(primary.border, None);
            let secondary = rest(Kind::Secondary, QuietTone::Link);
            assert_eq!(secondary.fill, Some(c.surface));
            assert_eq!(secondary.border, Some(c.control));
            assert_eq!(secondary.text, c.ink);
            let danger = rest(Kind::Danger, QuietTone::Link);
            assert_eq!(danger.fill, Some(c.error));
            assert_eq!(danger.text, c.on_error);
            let link = rest(Kind::Quiet, QuietTone::Link);
            assert_eq!((link.fill, link.border, link.text), (None, None, c.accent));
            let error = rest(Kind::Quiet, QuietTone::Error);
            assert_eq!(error.text, c.error);
            let icon = rest(Kind::Icon, QuietTone::Link);
            assert_eq!((icon.fill, icon.border, icon.text), (None, None, c.mute));
        }
    }

    #[test]
    fn hover_and_pressed_move_a_fill_toward_ink_by_6_and_12_percent() {
        for theme in [&LIGHT, &DARK] {
            for kind in [Kind::Primary, Kind::Secondary, Kind::Danger] {
                let fill = |touch| {
                    colors(theme, kind, QuietTone::Link, touch)
                        .fill
                        .expect("a filled kind")
                };
                let (rest, hover, pressed) =
                    (fill(Touch::Rest), fill(Touch::Hover), fill(Touch::Pressed));
                assert_eq!(hover, theme.shade(rest, 0.06));
                assert_eq!(pressed, theme.shade(rest, 0.12));
                let ink = theme.color.ink;
                assert!(contrast(rest, ink) > contrast(hover, ink));
                assert!(contrast(hover, ink) > contrast(pressed, ink));
            }
        }
    }

    #[test]
    fn a_button_with_no_fill_shows_selected_at_half_and_then_whole_under_the_pointer() {
        for theme in [&LIGHT, &DARK] {
            for kind in [Kind::Quiet, Kind::Icon] {
                let fill = |touch| colors(theme, kind, QuietTone::Link, touch).fill;
                assert_eq!(fill(Touch::Rest), None);
                assert_eq!(
                    fill(Touch::Hover),
                    Some(theme.color.selected.gamma_multiply(0.5))
                );
                assert_eq!(fill(Touch::Pressed), Some(theme.color.selected));
            }
        }
    }

    #[test]
    fn the_text_reads_in_every_state_and_both_themes() {
        for theme in [&LIGHT, &DARK] {
            let c = &theme.color;
            for kind in KINDS {
                for tone in TONES {
                    for touch in TOUCHES {
                        let paint = colors(theme, kind, tone, touch);
                        // An icon is a graphic: 3:1. Words: 4.5:1.
                        let min = if kind == Kind::Icon { UI } else { TEXT };
                        // A fill that lets the page through is seen over it.
                        let backgrounds: Vec<Color32> = [c.bg, c.surface, c.side]
                            .into_iter()
                            .map(|page| match paint.fill {
                                Some(fill) => over(fill, page),
                                None => page,
                            })
                            .collect();
                        for background in backgrounds {
                            let ratio = contrast(paint.text, background);
                            assert!(
                                ratio >= min,
                                "{kind:?} {tone:?} {touch:?} on {}: {ratio:.2}:1",
                                if theme.dark { "dark" } else { "light" }
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_secondary_border_stands_out_from_every_surface_it_sits_on() {
        for theme in [&LIGHT, &DARK] {
            let border = colors(theme, Kind::Secondary, QuietTone::Link, Touch::Rest)
                .border
                .expect("a border");
            for background in [theme.color.bg, theme.color.surface, theme.color.side] {
                assert!(contrast(border, background) >= UI);
            }
        }
    }

    #[test]
    fn the_main_buttons_are_heavier_than_the_others() {
        assert_eq!(Button::primary("x").face(), Face::Medium);
        assert_eq!(Button::danger("x").face(), Face::SemiBold);
        for button in [Button::secondary("x"), Button::quiet("x")] {
            assert_eq!(button.face(), Face::Regular);
        }
    }

    #[test]
    fn heights_and_padding_come_from_the_tokens() {
        let theme = &LIGHT;
        assert_eq!(Button::primary("x").height(theme), theme.size.control);
        assert_eq!(
            Button::secondary("x").in_row().height(theme),
            theme.size.control_in_row
        );
        assert_eq!(
            Button::icon(Icon::More, "More").height(theme),
            theme.size.icon_button
        );
        assert_eq!(Button::primary("x").padding(theme), theme.space.s4);
        assert_eq!(Button::danger("x").padding(theme), theme.space.s4);
        assert_eq!(Button::secondary("x").padding(theme), theme.space.s3);
        // components.md: 32, 30, padding 12 to 16.
        assert_eq!(theme.size.control, 32.0);
        assert_eq!(theme.size.icon_button, 30.0);
    }

    #[test]
    fn the_tooltip_is_the_reason_when_disabled_and_the_name_of_an_icon_button() {
        let reason = "Starts and stops with the sites that need it";
        assert_eq!(
            Button::secondary("Stop").disabled(reason).tooltip(),
            Some(reason)
        );
        assert_eq!(
            Button::icon(Icon::More, "More for PHP 8.4").tooltip(),
            Some("More for PHP 8.4")
        );
        // A disabled icon button says why, not what it is.
        assert_eq!(
            Button::icon(Icon::More, "More").disabled(reason).tooltip(),
            Some(reason)
        );
        assert_eq!(Button::primary("Add site…").tooltip(), None);
        assert_eq!(
            Button::primary("Add").disabled_if(false, reason).tooltip(),
            None
        );
        assert_eq!(
            Button::primary("Add").disabled_if(true, reason).tooltip(),
            Some(reason)
        );
    }

    /// The rect a button takes when added to a frame.
    fn rect_of(harness: &Harness, button: impl Fn() -> Button<'static>) -> Rect {
        let mut rect = Rect::NOTHING;
        harness.run(|ui| rect = ui.add(button()).rect);
        rect
    }

    #[test]
    fn a_button_is_as_high_as_its_token_and_as_wide_as_its_text_and_padding() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let short = rect_of(&harness, || Button::primary("Add"));
        let long = rect_of(&harness, || Button::primary("Add a new site…"));
        assert_eq!(short.height(), theme.size.control);
        assert_eq!(long.height(), theme.size.control);
        assert!(long.width() > short.width());
        assert!(short.width() > 2.0 * theme.space.s4);
        let in_row = rect_of(&harness, || Button::secondary("Open").in_row());
        assert_eq!(in_row.height(), theme.size.control_in_row);
        let icon = rect_of(&harness, || Button::icon(Icon::More, "More"));
        assert_eq!(icon.size(), egui::Vec2::splat(theme.size.icon_button));
        // An icon before the text widens the button by the icon and the gap.
        let plain = rect_of(&harness, || Button::secondary("Open"));
        let with_icon = rect_of(&harness, || Button::secondary("Open").leading(Icon::Plus));
        let extra = theme.size.icon_inline + theme.space.s2;
        assert!((with_icon.width() - plain.width() - extra).abs() < 0.01);
    }

    #[test]
    fn a_screen_reader_hears_the_label_the_role_and_whether_it_is_enabled() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            ui.add(Button::primary("Add site…"));
            ui.add(Button::secondary("Stop").disabled("Managed by the sites"));
            ui.add(Button::icon(Icon::More, "More for PHP 8.4"));
        });
        let add = tree.node_with_label("Add site…").expect("primary");
        assert_eq!(add.role(), egui::Role::Button);
        assert!(!add.is_disabled());
        let stop = tree.node_with_label("Stop").expect("disabled");
        assert!(stop.is_disabled());
        let more = tree.node_with_label("More for PHP 8.4").expect("icon");
        assert_eq!(more.role(), egui::Role::Button);
    }

    #[test]
    fn an_enabled_button_reports_a_click_and_a_disabled_one_does_not() {
        let harness = Harness::new(egui::Theme::Light);
        let show = |ui: &mut Ui, clicked: &mut [bool; 2]| {
            clicked[0] |= ui.add(Button::primary("Enabled")).clicked();
            clicked[1] |= ui
                .add(Button::primary("Disabled").disabled("Why"))
                .clicked();
        };
        let mut rects = [Rect::NOTHING; 2];
        harness.run(|ui| {
            rects[0] = ui.add(Button::primary("Enabled")).rect;
            rects[1] = ui.add(Button::primary("Disabled").disabled("Why")).rect;
        });
        for (index, expected) in [true, false].into_iter().enumerate() {
            let mut clicked = [false; 2];
            harness.click(rects[index].center(), |ui| show(ui, &mut clicked));
            assert_eq!(clicked[index], expected, "button {index}");
        }
    }

    #[test]
    fn the_pointer_changes_how_a_button_looks() {
        let harness = Harness::new(egui::Theme::Light);
        let rect = rect_of(&harness, || Button::secondary("Open"));
        let mut touch = Touch::Rest;
        harness.hover(rect.center(), |ui| {
            let response = ui.add(Button::secondary("Open"));
            touch = Touch::of(&response);
        });
        assert_eq!(touch, Touch::Hover);
    }

    #[test]
    fn the_focus_ring_is_two_points_of_accent_two_points_outside() {
        for (name, theme, active) in [
            ("light", &LIGHT, egui::Theme::Light),
            ("dark", &DARK, egui::Theme::Dark),
        ] {
            let harness = Harness::new(active);
            let mut rect = Rect::NOTHING;
            let ring = |look: Look, rect: &mut Rect| {
                let shapes = harness.shapes(|ui| {
                    *rect = ui.add(Button::primary("Add").looks(look)).rect;
                });
                shapes.into_iter().find_map(|shape| match shape {
                    egui::Shape::Rect(ring)
                        if ring.stroke.width > 0.0 && ring.stroke_kind == StrokeKind::Outside =>
                    {
                        Some(ring)
                    }
                    _ => None,
                })
            };
            let ring_of =
                ring(Look::focused(), &mut rect).unwrap_or_else(|| panic!("{name}: a ring"));
            assert_eq!(ring_of.stroke.width, theme.focus.width);
            assert_eq!(ring_of.stroke.width, 2.0);
            assert_eq!(ring_of.stroke.color, theme.color.accent);
            assert_eq!(ring_of.rect, rect.expand(theme.focus.offset));
            assert_eq!(theme.focus.offset, 2.0);
            // Never at rest, under the pointer, or pressed.
            for look in [Look::rest(), Look::hover(), Look::pressed()] {
                assert!(ring(look, &mut rect).is_none(), "{name}: {look:?}");
            }
        }
    }

    #[test]
    fn a_disabled_button_is_drawn_at_55_percent() {
        let harness = Harness::new(egui::Theme::Light);
        let fill_alpha = |button: fn() -> Button<'static>| {
            let shapes = harness.shapes(|ui| {
                ui.add(button());
            });
            shapes
                .into_iter()
                .find_map(|shape| match shape {
                    egui::Shape::Rect(fill) if fill.fill != Color32::TRANSPARENT => {
                        Some(fill.fill.a())
                    }
                    _ => None,
                })
                .expect("the fill")
        };
        assert_eq!(fill_alpha(|| Button::primary("Add")), 255);
        let faded = fill_alpha(|| Button::primary("Add").disabled("Why"));
        // 55% of 255, to the nearest whole.
        assert!((139..=141).contains(&faded), "{faded}");
        assert_eq!(LIGHT.state.disabled_opacity, 0.55);
    }
}
