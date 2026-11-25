//! Status widgets (components.md, Status): the status dot with its word, the
//! status sentence of Home, the request path, and the state pill of Services.
//!
//! A state is never shown by color alone (R8): each one is a color, an icon
//! shape, and a word. The widgets draw what they are given: the screen reads
//! the model and chooses the [`Health`], the [`StepState`], or the
//! [`PillState`].

use eframe::egui::{
    Align2, Color32, CornerRadius, Frame, InnerResponse, Label, Margin, Painter, Rect, Response,
    RichText, Role, Sense, Stroke, StrokeKind, Ui, Vec2, Widget, WidgetInfo, vec2,
};

use super::icons::Icon;
use super::{Look, layout, paint_focus_ring, quiet_fill, slot, spot, whole};
use crate::strings;
use crate::theme::{Direction, Face, TextToken, Theme};

/// How well Marsa works as a whole: what the status dot and the status
/// sentence say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// Everything works: a check, `ok`.
    Ok,
    /// Something needs a look: an alert, the warning amber.
    Warning,
    /// Something is broken: a cross, `error`.
    Error,
    /// Marsa is starting or working: a ring with a gap, `accent`.
    Busy,
}

impl Health {
    pub const ALL: [Self; 4] = [Self::Ok, Self::Warning, Self::Error, Self::Busy];
}

/// The circle of a [`Health`]: its icon, the icon's color, and the tint
/// behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub icon: Icon,
    pub fg: Color32,
    pub tint: Color32,
}

pub fn mark(theme: &Theme, health: Health) -> Mark {
    let c = &theme.color;
    match health {
        Health::Ok => Mark {
            icon: Icon::Check,
            fg: c.ok,
            tint: c.ok_tint,
        },
        // A warning uses the amber of the Dumps section (tokens.md).
        Health::Warning => Mark {
            icon: Icon::Alert,
            fg: theme.sections.dumps.fg,
            tint: theme.sections.dumps.tint,
        },
        Health::Error => Mark {
            icon: Icon::Cross,
            fg: c.error,
            tint: c.error_tint,
        },
        Health::Busy => Mark {
            icon: Icon::Spinner,
            fg: c.accent,
            tint: c.accent_tint,
        },
    }
}

/// A circle in the tint of `mark` with its icon, `icon_size` wide.
fn paint_mark(painter: &Painter, theme: &Theme, circle: Rect, mark: Mark, icon_size: f32) {
    painter.rect_filled(circle, CornerRadius::same(theme.radius.full), mark.tint);
    let icon = Rect::from_center_size(circle.center(), Vec2::splat(icon_size));
    mark.icon.paint(painter, theme, icon, mark.fg);
}

/// The status dot with its word: a 26 circle in the state tint with the
/// state icon, then a title and a caption. At the bottom of the sidebar and
/// in the header of the menu-bar panel. A click opens the status: see
/// `Response::clicked`. In the folded sidebar the circle shows alone and the
/// words show in a tooltip.
#[derive(Clone, Copy, Debug)]
#[must_use = "a status dot is drawn only when it is added to a Ui"]
pub struct StatusDot<'a> {
    health: Health,
    title: &'a str,
    caption: &'a str,
    width: Option<f32>,
    folded: bool,
    look: Option<Look>,
}

impl<'a> StatusDot<'a> {
    pub const fn new(health: Health, title: &'a str, caption: &'a str) -> Self {
        Self {
            health,
            title,
            caption,
            width: None,
            folded: false,
            look: None,
        }
    }

    /// At least this wide, as the items above it in the sidebar.
    pub const fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// The circle alone, in the middle; the words in a tooltip.
    pub const fn folded(mut self, folded: bool) -> Self {
        self.folded = folded;
        self
    }

    /// Draws the dot as it looks in `look` (the gallery and the tests).
    pub const fn looks(mut self, look: Look) -> Self {
        self.look = Some(look);
        self
    }
}

impl Widget for StatusDot<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let circle = theme.size.status_dot;
        let pad = theme.space.s2;
        // The circle lines up with the tiles of the sidebar items.
        let inset = theme.size.nav_inset();
        let gap = theme.space.group_gap_max;
        let title_token = TextToken {
            size: theme.text.body_sm.size,
            face: Face::Medium,
        };
        let title = layout(ui.painter(), self.title, title_token, Color32::PLACEHOLDER);
        let caption = layout(
            ui.painter(),
            self.caption,
            theme.text.caption,
            Color32::PLACEHOLDER,
        );
        let text = if self.folded {
            Vec2::ZERO
        } else {
            vec2(
                title.size().x.max(caption.size().x),
                title.size().y + caption.size().y,
            )
        };
        let content = if self.folded {
            circle
        } else {
            circle + gap + text.x
        };
        let size = vec2(
            (inset + content + inset).max(self.width.unwrap_or_default()),
            circle.max(text.y) + pad + pad,
        );
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        if ui.is_rect_visible(rect) {
            let look = self.look.unwrap_or_else(|| Look::of(&response));
            let corner = theme.radius.md;
            let painter = ui.painter();
            if let Some(fill) = quiet_fill(theme, look.touch) {
                painter.rect_filled(rect, CornerRadius::same(corner), fill);
            }
            let inner = rect.shrink2(vec2(inset, pad));
            let spot = |offset: f32, width: f32| {
                let x = slot(direction, inner, offset, width);
                spot(x, inner.top(), width, inner.height())
            };
            let circle_rect = if self.folded {
                Rect::from_center_size(rect.center(), Vec2::splat(circle))
            } else {
                Align2::LEFT_CENTER.align_size_within_rect(Vec2::splat(circle), spot(0.0, circle))
            };
            paint_mark(
                painter,
                theme,
                circle_rect,
                mark(theme, self.health),
                theme.size.status_dot_icon,
            );
            if !self.folded {
                let block = spot(circle + gap, text.x);
                let top = Align2::LEFT_CENTER.align_size_within_rect(text, block).min;
                let caption_top = top + vec2(0.0, title.size().y);
                painter.galley(top, title, theme.color.ink);
                painter.galley(caption_top, caption, theme.color.mute);
            }
            if look.focused {
                paint_focus_ring(painter, theme, rect, corner);
            }
        }
        let label = format!("{}, {}", self.title, self.caption);
        response.widget_info(|| WidgetInfo::labeled(Role::Status, true, &label));
        if self.folded {
            return response.on_hover_text(label);
        }
        response
    }
}

/// The card and the line of a status sentence: `surface` and `line`, and on
/// error `error.bg` and `error.line`.
pub fn sentence_card(theme: &Theme, health: Health) -> (Color32, Color32) {
    match health {
        Health::Error => (theme.color.error_bg, theme.color.error_line),
        _ => (theme.color.surface, theme.color.line),
    }
}

/// The status sentence of Home: a 44 circle, a sentence in `type.title`, and
/// one line that explains it. On error the card turns `error.bg`, and
/// [`StatusSentence::show_with`] adds what sits under the explanation: the
/// request path and the fix buttons.
#[derive(Clone, Copy, Debug)]
#[must_use = "a status sentence is drawn only when it is added to a Ui"]
pub struct StatusSentence<'a> {
    health: Health,
    sentence: &'a str,
    explanation: &'a str,
}

impl<'a> StatusSentence<'a> {
    pub const fn new(health: Health, sentence: &'a str, explanation: &'a str) -> Self {
        Self {
            health,
            sentence,
            explanation,
        }
    }

    /// The card, with `below` added under the explanation and lined up with
    /// the text, not the circle.
    pub fn show_with<R>(
        self,
        ui: &mut Ui,
        below: impl FnOnce(&mut Ui) -> R,
    ) -> InnerResponse<Option<R>> {
        self.draw(ui, Some(below))
    }

    fn draw<R, F: FnOnce(&mut Ui) -> R>(
        self,
        ui: &mut Ui,
        below: Option<F>,
    ) -> InnerResponse<Option<R>> {
        let theme = Theme::of(ui.ctx());
        let (fill, line) = sentence_card(theme, self.health);
        let card = Frame::new()
            .fill(fill)
            .stroke(Stroke::new(theme.size.border, line))
            .corner_radius(CornerRadius::same(theme.radius.lg))
            .inner_margin(Margin::same(whole(theme.space.s5)));
        let shown = card.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(theme.space.s4, theme.space.s3);
            // Left to right until Arabic ships (rtl.md).
            ui.horizontal(|ui| {
                let size = Vec2::splat(theme.size.status_hero);
                let (circle, _) = ui.allocate_exact_size(size, Sense::hover());
                paint_mark(
                    ui.painter(),
                    theme,
                    circle,
                    mark(theme, self.health),
                    theme.size.status_hero_icon,
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = theme.space.s1;
                    let sentence = RichText::new(self.sentence)
                        .font(theme.text.title.font_id())
                        .color(theme.color.ink);
                    ui.add(Label::new(sentence));
                    let explanation = RichText::new(self.explanation)
                        .font(theme.text.body_sm.font_id())
                        .color(theme.color.ink2);
                    ui.add(Label::new(explanation));
                });
            });
            below.map(|below| {
                ui.horizontal(|ui| {
                    ui.add_space(theme.size.status_hero + theme.space.s4);
                    ui.vertical(below).inner
                })
                .inner
            })
        });
        // An error is an alert; the rest is a status a screen reader reads
        // when it changes.
        let role = if self.health == Health::Error {
            Role::Alert
        } else {
            Role::Status
        };
        let sentence = self.sentence;
        shown
            .response
            .widget_info(|| WidgetInfo::labeled(role, true, sentence));
        shown
    }
}

impl Widget for StatusSentence<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.draw(ui, None::<fn(&mut Ui)>).response
    }
}

/// Whether the request reaches a step of the path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    /// A check, `ok`.
    Working,
    /// The step that breaks the path: a cross, `error`, 600 weight, and an
    /// `error` border.
    Broken,
    /// Not known: the request stops before it. A dash, `mute`.
    Unreached,
    /// Not known: the window cannot check it (DNS on macOS and Windows).
    /// Drawn like `Unreached`; only a screen reader hears the difference.
    NotChecked,
}

impl StepState {
    /// The word a screen reader says for the step.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Working => strings::STEP_WORKING,
            Self::Broken => strings::STEP_BROKEN,
            Self::Unreached => strings::STEP_UNREACHED,
            Self::NotChecked => strings::STEP_NOT_CHECKED,
        }
    }
}

/// How a step of the path is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepLook {
    pub icon: Icon,
    /// The icon and the word.
    pub color: Color32,
    pub border: Color32,
    pub face: Face,
}

pub fn step_look(theme: &Theme, state: StepState) -> StepLook {
    let c = &theme.color;
    match state {
        StepState::Working => StepLook {
            icon: Icon::Check,
            color: c.ok,
            border: c.line,
            face: Face::Medium,
        },
        StepState::Broken => StepLook {
            icon: Icon::Cross,
            color: c.error,
            border: c.error,
            face: Face::SemiBold,
        },
        StepState::Unreached | StepState::NotChecked => StepLook {
            icon: Icon::Dash,
            color: c.mute,
            border: c.line,
            face: Face::Medium,
        },
    }
}

/// One step of the request path: its name and whether it works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathStep<'a> {
    pub label: &'a str,
    pub state: StepState,
}

/// Where the pills and the arrows of a path sit, measured from the start of
/// the row.
#[derive(Clone, Debug, PartialEq)]
pub struct PathSlots {
    /// The start offset and the width of each pill, in the order of the
    /// steps.
    pub pills: Vec<(f32, f32)>,
    /// The start offset of the arrow after each pill but the last.
    pub arrows: Vec<f32>,
    pub total: f32,
}

/// The width of a pill for a text `text_width` wide: padding, icon, gap, and
/// the text.
pub fn pill_width(theme: &Theme, text_width: f32) -> f32 {
    theme.space.s3 + theme.size.icon_badge + theme.space.s2 + text_width + theme.space.s3
}

/// Lays the pills out in order, each joined to the next by an arrow with a
/// gap on both sides.
pub fn path_slots(theme: &Theme, pill_widths: &[f32]) -> PathSlots {
    let gap = theme.space.s2;
    let arrow = theme.size.icon_badge;
    let mut x = 0.0;
    let mut pills = Vec::with_capacity(pill_widths.len());
    let mut arrows = Vec::new();
    for (index, width) in pill_widths.iter().enumerate() {
        pills.push((x, *width));
        x += width;
        if index + 1 < pill_widths.len() {
            arrows.push(x + gap);
            x += gap + arrow + gap;
        }
    }
    PathSlots {
        pills,
        arrows,
        total: x,
    }
}

/// The request path: pills for DNS, Caddy, the runtime, and the site, joined
/// by arrows, in the order a request takes. It is shown only when something
/// is broken (R2). `label` names the group for a screen reader.
#[derive(Clone, Copy, Debug)]
#[must_use = "a request path is drawn only when it is added to a Ui"]
pub struct RequestPath<'a> {
    label: &'a str,
    steps: &'a [PathStep<'a>],
}

impl<'a> RequestPath<'a> {
    pub const fn new(label: &'a str, steps: &'a [PathStep<'a>]) -> Self {
        Self { label, steps }
    }
}

impl Widget for RequestPath<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let gap = theme.space.s2;
        let icon = theme.size.icon_badge;
        let galleys: Vec<_> = self
            .steps
            .iter()
            .map(|step| {
                let token = TextToken {
                    size: theme.text.body_sm.size,
                    face: step_look(theme, step.state).face,
                };
                layout(ui.painter(), step.label, token, Color32::PLACEHOLDER)
            })
            .collect();
        let widths: Vec<f32> = galleys
            .iter()
            .map(|galley| pill_width(theme, galley.size().x))
            .collect();
        let slots = path_slots(theme, &widths);
        let height = theme.size.badge;
        let (rect, group) = ui.allocate_exact_size(vec2(slots.total, height), Sense::hover());
        let label = self.label;
        group.widget_info(|| WidgetInfo::labeled(Role::List, true, label));
        for (index, (step, galley)) in self.steps.iter().zip(galleys).enumerate() {
            let (offset, width) = slots.pills[index];
            let pill = spot(
                slot(direction, rect, offset, width),
                rect.top(),
                width,
                height,
            );
            let item = ui.interact(pill, group.id.with(index), Sense::hover());
            let name = strings::path_step_label(step.label, step.state.word());
            item.widget_info(|| WidgetInfo::labeled(Role::ListItem, true, &name));
            if !ui.is_rect_visible(pill) {
                continue;
            }
            let look = step_look(theme, step.state);
            let painter = ui.painter();
            let corner = CornerRadius::same(theme.radius.full);
            painter.rect_filled(pill, corner, theme.color.surface);
            painter.rect_stroke(
                pill,
                corner,
                Stroke::new(theme.size.border, look.border),
                StrokeKind::Inside,
            );
            let inner = pill.shrink2(vec2(theme.space.s3, 0.0));
            let icon_spot = spot(slot(direction, inner, 0.0, icon), pill.top(), icon, height);
            look.icon.paint(painter, theme, icon_spot, look.color);
            let text_x = slot(direction, inner, icon + gap, galley.size().x);
            let text_spot = spot(text_x, pill.top(), galley.size().x, height);
            let at = Align2::LEFT_CENTER.align_size_within_rect(galley.size(), text_spot);
            painter.galley(at.min, galley, look.color);
        }
        let painter = ui.painter();
        for offset in &slots.arrows {
            let spot = spot(
                slot(direction, rect, *offset, icon),
                rect.top(),
                icon,
                height,
            );
            Icon::Arrow.paint(painter, theme, spot, theme.color.mute);
        }
        group
    }
}

/// What a service's state pill shows (components.md, Status).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PillState {
    /// A check, `ok`.
    Running,
    /// A hollow ring, `ink2`: it starts when a site needs it.
    Waiting,
    /// A cross, `error`.
    Stopped,
    /// A ring with a gap, `accent`.
    Starting,
    /// A dash, `accent` like Starting: the program is on its way down.
    Stopping,
    /// An alert, `error`: the program stopped by an error (the reason is
    /// the screen's to say).
    Failed,
    /// A small ring, `mute`, the word in `ink2`: a site's worker whose
    /// switch is off. Not a problem, so not a cross.
    Off,
}

impl PillState {
    /// The word of the pill, for the eye and for a screen reader.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Running => strings::PILL_RUNNING,
            Self::Waiting => strings::PILL_WAITING,
            Self::Stopped => strings::PILL_STOPPED,
            Self::Starting => strings::PILL_STARTING,
            Self::Stopping => strings::PILL_STOPPING,
            Self::Failed => strings::PILL_FAILED,
            Self::Off => strings::PILL_OFF,
        }
    }

    pub const ALL: [Self; 7] = [
        Self::Running,
        Self::Waiting,
        Self::Stopped,
        Self::Starting,
        Self::Stopping,
        Self::Failed,
        Self::Off,
    ];
}

/// How a state pill is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PillLook {
    /// The fill of the 18 circle; none for the hollow ring.
    pub fill: Option<Color32>,
    /// The ring of a state that waits.
    pub ring: Option<Color32>,
    /// The icon in the circle and its color; none for the hollow ring.
    pub icon: Option<(Icon, Color32)>,
    /// The word.
    pub text: Color32,
}

pub fn pill_look(theme: &Theme, state: PillState) -> PillLook {
    let c = &theme.color;
    match state {
        PillState::Running => PillLook {
            fill: Some(c.ok_tint),
            ring: None,
            icon: Some((Icon::Check, c.ok)),
            text: c.ok,
        },
        PillState::Waiting => PillLook {
            fill: None,
            ring: Some(c.mute),
            icon: None,
            text: c.ink2,
        },
        PillState::Stopped => PillLook {
            fill: Some(c.error_tint),
            ring: None,
            icon: Some((Icon::Cross, c.error)),
            text: c.error,
        },
        PillState::Starting => PillLook {
            fill: Some(c.accent_tint),
            ring: None,
            icon: Some((Icon::Spinner, c.accent)),
            text: c.accent,
        },
        PillState::Stopping => PillLook {
            fill: Some(c.accent_tint),
            ring: None,
            icon: Some((Icon::Dash, c.accent)),
            text: c.accent,
        },
        PillState::Failed => PillLook {
            fill: Some(c.error_tint),
            ring: None,
            icon: Some((Icon::Alert, c.error)),
            text: c.error,
        },
        PillState::Off => PillLook {
            fill: None,
            ring: None,
            icon: Some((Icon::Ring, c.mute)),
            text: c.ink2,
        },
    }
}

/// The state of a service in Services: an icon in an 18 circle plus a word.
#[derive(Clone, Copy, Debug)]
#[must_use = "a state pill is drawn only when it is added to a Ui"]
pub struct StatePill {
    state: PillState,
}

impl StatePill {
    pub const fn new(state: PillState) -> Self {
        Self { state }
    }
}

impl Widget for StatePill {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let word = self.state.word();
        let token = TextToken {
            size: theme.text.body_sm.size,
            face: Face::Medium,
        };
        let galley = layout(ui.painter(), word, token, Color32::PLACEHOLDER);
        let circle = theme.size.state_circle;
        let gap = theme.space.s2;
        let height = circle.max(galley.size().y);
        let size = vec2(circle + gap + galley.size().x, height);
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
        if ui.is_rect_visible(rect) {
            let look = pill_look(theme, self.state);
            let painter = ui.painter();
            let spot = |offset: f32, width: f32| {
                let x = slot(direction, rect, offset, width);
                spot(x, rect.top(), width, height)
            };
            let ring =
                Align2::LEFT_CENTER.align_size_within_rect(Vec2::splat(circle), spot(0.0, circle));
            let corner = CornerRadius::same(theme.radius.full);
            if let Some(fill) = look.fill {
                painter.rect_filled(ring, corner, fill);
            }
            if let Some(color) = look.ring {
                let stroke = Stroke::new(theme.size.state_ring, color);
                painter.rect_stroke(ring, corner, stroke, StrokeKind::Inside);
            }
            if let Some((icon, color)) = look.icon {
                let icon_rect =
                    Rect::from_center_size(ring.center(), Vec2::splat(theme.size.state_icon));
                icon.paint(painter, theme, icon_rect, color);
            }
            let text_spot = spot(circle + gap, galley.size().x);
            let at = Align2::LEFT_CENTER.align_size_within_rect(galley.size(), text_spot);
            painter.galley(at.min, galley, look.text);
        }
        response.widget_info(|| WidgetInfo::labeled(Role::Status, true, word));
        response
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Shape};

    use super::*;
    use crate::theme::tests::{TEXT, UI, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::{Harness, over};

    const THEMES: [(&str, &Theme, egui::Theme); 2] = [
        ("light", &LIGHT, egui::Theme::Light),
        ("dark", &DARK, egui::Theme::Dark),
    ];

    #[test]
    fn each_health_has_its_own_icon_and_a_mark_that_stands_out_from_its_tint() {
        for (name, theme, _) in THEMES {
            let marks: Vec<Mark> = Health::ALL.iter().map(|h| mark(theme, *h)).collect();
            for (index, a) in marks.iter().enumerate() {
                for b in &marks[index + 1..] {
                    // R8: the icon shape tells the states apart, not the color.
                    assert_ne!(a.icon, b.icon);
                }
                let ratio = contrast(a.fg, a.tint);
                assert!(ratio >= UI, "{name} {:?}: {ratio:.2}:1", a.icon);
            }
            assert_eq!(mark(theme, Health::Ok).fg, theme.color.ok);
            assert_eq!(mark(theme, Health::Error).fg, theme.color.error);
            assert_eq!(mark(theme, Health::Error).tint, theme.color.error_tint);
            assert_eq!(mark(theme, Health::Warning).fg, theme.sections.dumps.fg);
            assert_eq!(mark(theme, Health::Busy).fg, theme.color.accent);
        }
    }

    #[test]
    fn an_error_card_is_error_bg_with_an_error_line_and_the_rest_are_surface() {
        for theme in [&LIGHT, &DARK] {
            let c = &theme.color;
            assert_eq!(
                sentence_card(theme, Health::Error),
                (c.error_bg, c.error_line)
            );
            for health in [Health::Ok, Health::Warning, Health::Busy] {
                assert_eq!(sentence_card(theme, health), (c.surface, c.line));
            }
            // The sentence and its explanation read on both cards.
            for (fill, _) in [
                sentence_card(theme, Health::Ok),
                sentence_card(theme, Health::Error),
            ] {
                assert!(contrast(c.ink, fill) >= TEXT);
                assert!(contrast(c.ink2, fill) >= TEXT);
            }
        }
    }

    #[test]
    fn the_broken_step_has_a_cross_the_error_color_and_a_heavier_weight() {
        for (name, theme, _) in THEMES {
            let working = step_look(theme, StepState::Working);
            let broken = step_look(theme, StepState::Broken);
            let unreached = step_look(theme, StepState::Unreached);
            assert_eq!(step_look(theme, StepState::NotChecked), unreached);
            assert_eq!(broken.icon, Icon::Cross);
            assert_eq!(broken.color, theme.color.error);
            assert_eq!(broken.border, theme.color.error);
            assert_eq!(broken.face, Face::SemiBold);
            assert_eq!(working.icon, Icon::Check);
            assert_eq!(working.color, theme.color.ok);
            assert_eq!(unreached.icon, Icon::Dash);
            for other in [working, unreached] {
                assert_eq!(other.face, Face::Medium);
                assert_eq!(other.border, theme.color.line);
                assert_ne!(other.icon, broken.icon);
            }
            // The word on the pill, and the broken border on the card it
            // sits in.
            for look in [working, broken, unreached] {
                let ratio = contrast(look.color, theme.color.surface);
                assert!(ratio >= TEXT, "{name} {:?}: {ratio:.2}:1", look.icon);
            }
            assert!(contrast(broken.border, theme.color.error_bg) >= UI);
        }
    }

    #[test]
    fn the_steps_sit_in_order_from_the_start_joined_by_arrows() {
        let theme = &LIGHT;
        let widths = [60.0, 80.0, 90.0, 100.0];
        let slots = path_slots(theme, &widths);
        assert_eq!(slots.pills.len(), 4);
        assert_eq!(slots.arrows.len(), 3);
        assert_eq!(slots.pills[0].0, 0.0);
        for (index, (offset, width)) in slots.pills.iter().enumerate() {
            assert_eq!(*width, widths[index]);
            if let Some((next, _)) = slots.pills.get(index + 1) {
                assert!(next > offset, "the steps run from the start");
                // The arrow sits between the two pills, with a gap on both sides.
                let arrow = slots.arrows[index];
                assert_eq!(arrow, offset + width + theme.space.s2);
                assert_eq!(*next, arrow + theme.size.icon_badge + theme.space.s2);
            }
        }
        let (last, width) = slots.pills[3];
        assert_eq!(slots.total, last + width);
        assert_eq!(path_slots(theme, &[]).total, 0.0);
        assert!(path_slots(theme, &[40.0]).arrows.is_empty());
    }

    #[test]
    fn a_screen_reader_hears_the_steps_in_order_with_their_state() {
        const STEPS: [PathStep<'static>; 4] = [
            PathStep {
                label: "DNS",
                state: StepState::Working,
            },
            PathStep {
                label: "Caddy",
                state: StepState::Broken,
            },
            PathStep {
                label: "PHP 8.4",
                state: StepState::Working,
            },
            PathStep {
                label: "shop.test",
                state: StepState::Unreached,
            },
        ];
        assert_eq!(
            STEPS.map(|step| strings::path_step_label(step.label, step.state.word())),
            [
                "DNS, working",
                "Caddy, not working",
                "PHP 8.4, working",
                "shop.test, not reached"
            ]
        );
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            ui.add(RequestPath::new(strings::REQUEST_PATH, &STEPS));
        });
        let group = tree.node_with_label(strings::REQUEST_PATH).expect("group");
        assert_eq!(group.role(), egui::Role::List);
        for step in STEPS {
            let name = strings::path_step_label(step.label, step.state.word());
            let item = tree.node_with_label(&name).expect("a step");
            assert_eq!(item.role(), egui::Role::ListItem);
        }
    }

    /// The pill rects a path takes, and the shapes it paints.
    fn path_shapes(theme: egui::Theme, steps: &[PathStep<'_>]) -> (Rect, Vec<Shape>) {
        let harness = Harness::new(theme);
        let mut rect = Rect::NOTHING;
        let shapes = harness.shapes(|ui| {
            rect = ui.add(RequestPath::new("Path", steps)).rect;
        });
        (rect, shapes)
    }

    #[test]
    fn only_the_broken_step_is_outlined_in_the_error_color() {
        let steps = [
            PathStep {
                label: "DNS",
                state: StepState::Working,
            },
            PathStep {
                label: "Caddy",
                state: StepState::Broken,
            },
            PathStep {
                label: "PHP",
                state: StepState::Working,
            },
        ];
        for (name, theme, active) in THEMES {
            let (rect, shapes) = path_shapes(active, &steps);
            let outlines: Vec<Rect> = shapes
                .iter()
                .filter_map(|shape| match shape {
                    Shape::Rect(r) if r.stroke.color == theme.color.error => Some(r.rect),
                    _ => None,
                })
                .collect();
            assert_eq!(outlines.len(), 1, "{name}");
            // It is the middle pill: after DNS and before PHP.
            assert!(outlines[0].left() > rect.left());
            assert!(outlines[0].right() < rect.right());
            assert_eq!(outlines[0].height(), theme.size.badge);
        }
    }

    #[test]
    fn a_path_is_26_high_and_wider_with_more_steps() {
        let harness = Harness::new(egui::Theme::Light);
        let mut sizes = Vec::new();
        harness.run(|ui| {
            for count in [1, 2, 4] {
                let steps: Vec<PathStep<'_>> = (0..count)
                    .map(|_| PathStep {
                        label: "Caddy",
                        state: StepState::Working,
                    })
                    .collect();
                sizes.push(ui.add(RequestPath::new("Path", &steps)).rect.size());
            }
        });
        assert!(sizes.iter().all(|size| size.y == LIGHT.size.badge));
        assert!(sizes[0].x < sizes[1].x && sizes[1].x < sizes[2].x);
        assert_eq!(LIGHT.size.badge, 26.0);
    }

    #[test]
    fn each_pill_state_has_its_own_icon_shape_and_a_word_that_reads() {
        for (name, theme, _) in THEMES {
            let c = &theme.color;
            let looks: Vec<PillLook> = PillState::ALL
                .iter()
                .map(|s| pill_look(theme, *s))
                .collect();
            let icons: Vec<Option<Icon>> =
                looks.iter().map(|l| l.icon.map(|(icon, _)| icon)).collect();
            for (index, a) in icons.iter().enumerate() {
                for b in &icons[index + 1..] {
                    assert_ne!(a, b, "{name}: two states share an icon");
                }
            }
            assert_eq!(pill_look(theme, PillState::Waiting).ring, Some(c.mute));
            assert_eq!(pill_look(theme, PillState::Waiting).text, c.ink2);
            assert_eq!(pill_look(theme, PillState::Running).text, c.ok);
            assert_eq!(pill_look(theme, PillState::Stopped).text, c.error);
            assert_eq!(pill_look(theme, PillState::Starting).text, c.accent);
            assert_eq!(pill_look(theme, PillState::Stopping).text, c.accent);
            assert_eq!(pill_look(theme, PillState::Failed).text, c.error);
            assert_eq!(pill_look(theme, PillState::Off).text, c.ink2);
            assert_eq!(pill_look(theme, PillState::Off).fill, None);
            for state in PillState::ALL {
                let look = pill_look(theme, state);
                // The word on a row of a table: at rest, and under the
                // pointer (`selected` at 50% over `surface`).
                let hover = over(c.selected.gamma_multiply(theme.state.row_hover), c.surface);
                for page in [c.surface, hover] {
                    let ratio = contrast(look.text, page);
                    assert!(ratio >= TEXT, "{name} {state:?}: {ratio:.2}:1");
                }
                if let (Some(fill), Some((_, color))) = (look.fill, look.icon) {
                    assert!(contrast(color, fill) >= UI, "{name} {state:?} icon");
                }
                if let Some(ring) = look.ring {
                    assert!(contrast(ring, c.surface) >= UI);
                }
            }
        }
    }

    #[test]
    fn the_words_of_the_pill_states_are_in_strings() {
        let words = PillState::ALL.map(PillState::word);
        assert_eq!(
            words,
            [
                "Running", "Waiting", "Stopped", "Starting", "Stopping", "Failed", "Off"
            ]
        );
    }

    #[test]
    fn the_dot_the_sentence_and_the_pill_take_their_sizes_from_the_tokens() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let mut sizes = Vec::new();
        harness.run(|ui| {
            sizes.push(
                ui.add(StatusDot::new(
                    Health::Ok,
                    "All sites working",
                    "DNS, Caddy, PHP",
                ))
                .rect
                .size(),
            );
            sizes.push(ui.add(StatePill::new(PillState::Running)).rect.size());
            sizes.push(ui.add(StatePill::new(PillState::Waiting)).rect.size());
        });
        // A circle of 26 with 8 above and below, or the two lines of text
        // when they are taller.
        assert!(sizes[0].y >= theme.size.status_dot + 2.0 * theme.space.s2);
        // The pill is as high as its circle or its word, never less.
        assert!(sizes[1].y >= theme.size.state_circle);
        assert_eq!(sizes[1].y, sizes[2].y);
        assert!(sizes[1].x > theme.size.state_circle);
        assert_eq!(
            (
                theme.size.status_dot,
                theme.size.status_hero,
                theme.size.state_circle
            ),
            (26.0, 44.0, 18.0)
        );
    }

    #[test]
    fn the_sentence_card_fills_the_width_and_shows_what_is_added_below() {
        let harness = Harness::new(egui::Theme::Light);
        let mut plain = Rect::NOTHING;
        let mut with_below = Rect::NOTHING;
        harness.run(|ui| {
            plain = ui
                .add(StatusSentence::new(
                    Health::Ok,
                    "All 6 sites are working.",
                    "DNS, Caddy, and PHP 8.4 are running.",
                ))
                .rect;
            let shown = StatusSentence::new(
                Health::Error,
                "Sites are not opening: Caddy stopped.",
                "It stopped at 14:02.",
            )
            .show_with(ui, |ui| {
                ui.label("under");
                7
            });
            assert_eq!(shown.inner, Some(7));
            with_below = shown.response.rect;
        });
        assert_eq!(plain.width(), with_below.width());
        // The circle is 44, so the card is taller than that plus its margins.
        let margins = 2.0 * LIGHT.space.s5;
        assert!(plain.height() >= LIGHT.size.status_hero + margins);
        assert!(with_below.height() > plain.height());
    }

    #[test]
    fn a_screen_reader_hears_an_error_as_an_alert_and_the_rest_as_a_status() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            ui.add(StatusSentence::new(
                Health::Ok,
                "All 6 sites are working.",
                "Fine.",
            ));
            ui.add(StatusSentence::new(
                Health::Error,
                "Sites are not opening.",
                "Caddy stopped.",
            ));
            ui.add(StatusDot::new(
                Health::Error,
                "Sites not opening",
                "Caddy stopped",
            ));
            ui.add(StatePill::new(PillState::Stopped));
        });
        let role = |label: &str| tree.node_with_label(label).expect(label).role();
        assert_eq!(role("All 6 sites are working."), egui::Role::Status);
        assert_eq!(role("Sites are not opening."), egui::Role::Alert);
        assert_eq!(role("Sites not opening, Caddy stopped"), egui::Role::Status);
        assert_eq!(role("Stopped"), egui::Role::Status);
    }

    #[test]
    fn the_status_dot_fills_its_width_and_folds_to_its_circle() {
        let harness = Harness::new(egui::Theme::Light);
        let dot = StatusDot::new(Health::Error, "Sites not opening", "Caddy stopped");
        let (mut wide, mut folded) = (Rect::NOTHING, Rect::NOTHING);
        harness.run(|ui| {
            wide = ui.add(dot.width(LIGHT.size.sidebar)).rect;
            folded = ui.add(dot.folded(true)).rect;
        });
        assert_eq!(wide.width(), LIGHT.size.sidebar);
        // The circle with the inset of a sidebar tile on each side: as wide
        // as a folded sidebar item.
        assert_eq!(folded.width(), LIGHT.size.nav_item);
        let tree = harness.accessibility(|ui| {
            ui.add(dot.folded(true));
        });
        let node = tree.node_with_label("Sites not opening, Caddy stopped");
        assert!(node.is_some(), "a folded dot keeps its words");
    }

    #[test]
    fn the_status_dot_is_clickable_and_shows_the_ring_when_focused() {
        let harness = Harness::new(egui::Theme::Light);
        let mut rect = Rect::NOTHING;
        harness.run(|ui| {
            rect = ui
                .add(StatusDot::new(Health::Ok, "All sites working", "DNS"))
                .rect
        });
        let mut clicked = false;
        harness.click(rect.center(), |ui| {
            clicked |= ui
                .add(StatusDot::new(Health::Ok, "All sites working", "DNS"))
                .clicked();
        });
        assert!(clicked);
        let ring = |look: Look| {
            harness
                .shapes(|ui| {
                    ui.add(StatusDot::new(Health::Ok, "All sites working", "DNS").looks(look));
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
