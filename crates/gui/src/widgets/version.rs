//! Version widgets (components.md): the large version picker and the compact
//! version menu. Both take the versions as a list of `(value, text)`, so they
//! work for any runtime: PHP "8.4", Node "22", a later Python "3.13" (R5).
//! They draw what they are given and return what the user chose; the screen
//! turns that into an action.

use eframe::egui::{
    Align2, Color32, CornerRadius, EventFilter, InnerResponse, Key, Label, Popup, Pos2, Rect,
    Response, RichText, Role, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, Widget, WidgetInfo, pos2,
    vec2,
};

use super::icons::Icon;
use super::menu;
use super::{
    Look, Touch, group_button, layout, paint_focus_ring, quiet_fill, slot, tab_stop, touched,
    with_disabled_reason,
};
use crate::theme::{Direction, Face, TextToken, Theme, VersionText};

/// A move of the keyboard focus inside a group of buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Previous,
    Next,
    First,
    Last,
}

/// The stop the focus moves to: it stops at both ends, it does not wrap.
/// `stops` is how many buttons the group has.
pub fn moved(from: usize, stops: usize, step: Step) -> usize {
    let last = stops.saturating_sub(1);
    match step {
        Step::Previous => from.min(last).saturating_sub(1),
        Step::Next => from.saturating_add(1).min(last),
        Step::First => 0,
        Step::Last => last,
    }
}

/// What a key does in the group: the arrows along the row, and up and down,
/// move the focus; Home and End go to the ends. "Forward" along the row is
/// to the right in a left-to-right language.
pub fn step_for(key: Key, direction: Direction) -> Option<Step> {
    let along = |forward: bool| {
        if forward != direction.is_rtl() {
            Step::Next
        } else {
            Step::Previous
        }
    };
    match key {
        Key::ArrowRight => Some(along(true)),
        Key::ArrowLeft => Some(along(false)),
        Key::ArrowDown => Some(Step::Next),
        Key::ArrowUp => Some(Step::Previous),
        Key::Home => Some(Step::First),
        Key::End => Some(Step::Last),
        _ => None,
    }
}

/// The keys the group reads, in the order the first match wins. Enter and
/// Space are not here: egui turns them into a click on the focused button.
const KEYS: [Key; 6] = [
    Key::ArrowRight,
    Key::ArrowLeft,
    Key::ArrowDown,
    Key::ArrowUp,
    Key::Home,
    Key::End,
];

/// How big the picker is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerSize {
    /// 92 by 64, `type.version.lg`: Home and the runtimes screen.
    Large,
    /// 44 high, `type.version.md`: the menu-bar panel.
    Medium,
}

impl PickerSize {
    /// The size of a version button.
    pub fn button(self, theme: &Theme) -> Vec2 {
        match self {
            Self::Large => theme.size.version_lg,
            Self::Medium => vec2(theme.size.version_lg.x, theme.size.version_md),
        }
    }

    /// components.md: the large size is `radius.lg`, the medium `radius.md`.
    pub const fn radius(self, theme: &Theme) -> u8 {
        match self {
            Self::Large => theme.radius.lg,
            Self::Medium => theme.radius.md,
        }
    }

    pub const fn text(self, theme: &Theme) -> VersionText {
        match self {
            Self::Large => theme.text.version_lg,
            Self::Medium => theme.text.version_md,
        }
    }

    /// The dashed install button is a square as high as the others.
    pub fn install(self, theme: &Theme) -> Vec2 {
        Vec2::splat(self.button(theme).y)
    }
}

/// What the picker reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionPick<T> {
    /// The user chose another version.
    Version(T),
    /// The user pressed the dashed "+": open Install.
    Install,
}

/// A version button in one state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonPaint {
    pub fill: Color32,
    pub border: Color32,
    pub text: Color32,
    pub face: Face,
}

/// The chosen version is `accent` with `on_accent` text at 500; the others
/// are `surface` with a 1 px `control` border at 300 (components.md): a
/// `line` border is under 3:1 on `bg` (WCAG 1.4.11).
pub fn button_paint(theme: &Theme, size: PickerSize, chosen: bool, touch: Touch) -> ButtonPaint {
    let c = &theme.color;
    let text = size.text(theme);
    if chosen {
        let fill = touched(theme, c.accent, touch);
        ButtonPaint {
            fill,
            border: fill,
            text: c.on_accent,
            face: text.selected,
        }
    } else {
        ButtonPaint {
            fill: touched(theme, c.surface, touch),
            border: c.control,
            text: c.ink,
            face: text.face,
        }
    }
}

/// The points of a rounded rectangle's outline, going around once and ending
/// where it began: egui dashes a polyline, not a rounded rect.
fn rounded_outline(rect: Rect, radius: f32) -> Vec<Pos2> {
    use std::f32::consts::FRAC_PI_2;
    // Each corner is a quarter of a circle around the rect shrunk by the
    // radius. `quarter` counts quarter turns from "to the right", with y
    // growing down; the corners go clockwise from the top right.
    let inner = rect.shrink(radius);
    let corners = [
        (inner.right_top(), 3),
        (inner.right_bottom(), 0),
        (inner.left_bottom(), 1),
        (inner.left_top(), 2),
    ];
    let mut points = Vec::with_capacity(corners.len() * (ARC_STEPS + 1) + 1);
    for (center, quarter) in corners {
        for step in 0..=ARC_STEPS {
            let turns = quarter as f32 + step as f32 / ARC_STEPS as f32;
            let angle = turns * FRAC_PI_2;
            points.push(center + vec2(angle.cos(), angle.sin()) * radius);
        }
    }
    points.push(points[0]);
    points
}

/// The straight pieces of one corner.
const ARC_STEPS: usize = 6;

/// The large version picker: one button per installed version, an optional
/// dashed "+" at the end. The versions are a radio group with one Tab stop
/// (the WAI-ARIA pattern, "roving focus"): Tab enters on the chosen version,
/// or on the first when none is chosen, and the next Tab leaves. Inside, the
/// arrow keys, Home, and End move the focus from version to version, and
/// Enter or Space chooses the focused one. Choosing is separate from moving,
/// so the arrows never switch a runtime by accident. The "+" is an action,
/// not an option: it is a button of its own and its own Tab stop right after
/// the group, and the arrows, Home, and End do not go to it.
#[derive(Clone, Copy, Debug)]
#[must_use = "a version picker is drawn only when it is added to a Ui"]
pub struct VersionPicker<'a, T> {
    /// What a screen reader says for the group: "Default PHP".
    label: &'a str,
    versions: &'a [(T, &'a str)],
    selected: Option<T>,
    /// The name of the dashed "+" button: "Install another PHP version".
    install: Option<&'a str>,
    size: PickerSize,
    disabled: Option<&'a str>,
    /// A button's index and the look to draw it in.
    forced: Option<(usize, Look)>,
}

impl<'a, T: Copy + PartialEq> VersionPicker<'a, T> {
    pub const fn new(label: &'a str, versions: &'a [(T, &'a str)], selected: Option<T>) -> Self {
        Self {
            label,
            versions,
            selected,
            install: None,
            size: PickerSize::Large,
            disabled: None,
            forced: None,
        }
    }

    /// Adds the dashed "+" after the versions; `label` names it.
    pub const fn install(mut self, label: &'a str) -> Self {
        self.install = Some(label);
        self
    }

    pub const fn size(mut self, size: PickerSize) -> Self {
        self.size = size;
        self
    }

    /// Disables the picker. Hovering it shows `reason`.
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }

    /// Draws the button at `index` (the "+" is after the last version) as it
    /// looks in `look`, and the others at rest (the gallery and the tests).
    pub const fn looks(mut self, index: usize, look: Look) -> Self {
        self.forced = Some((index, look));
        self
    }

    /// Draws the picker. `inner` is what the user chose this frame.
    pub fn show(self, ui: &mut Ui) -> InnerResponse<Option<VersionPick<T>>> {
        self.show_in(ui, Direction::CURRENT)
    }

    /// [`Self::show`] in a row that runs in `direction`.
    fn show_in(self, ui: &mut Ui, direction: Direction) -> InnerResponse<Option<VersionPick<T>>> {
        if let Some(reason) = self.disabled {
            let shown = ui
                .add_enabled_ui(false, |ui| self.draw(ui, direction))
                .inner;
            return InnerResponse::new(
                shown.inner,
                with_disabled_reason(shown.response, Some(reason)),
            );
        }
        self.draw(ui, direction)
    }

    fn draw(self, ui: &mut Ui, direction: Direction) -> InnerResponse<Option<VersionPick<T>>> {
        let theme = Theme::of(ui.ctx());
        let enabled = ui.is_enabled();
        let button = self.size.button(theme);
        let text = self.size.text(theme);
        let gap = theme.space.group_gap;
        // Sized for the heavier face, so a button keeps its width when the
        // choice moves to it.
        let token = TextToken {
            size: text.size,
            face: text.selected,
        };
        let galleys: Vec<_> = self
            .versions
            .iter()
            .map(|(_, name)| layout(ui.painter(), name, token, Color32::PLACEHOLDER))
            .collect();
        let mut widths: Vec<f32> = galleys
            .iter()
            .map(|galley| {
                button
                    .x
                    .max(galley.size().x + theme.space.s3 + theme.space.s3)
            })
            .collect();
        if self.install.is_some() {
            widths.push(self.size.install(theme).x);
        }
        let total = widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32;
        let (rect, group) = ui.allocate_exact_size(vec2(total, button.y), Sense::hover());
        let label = self.label;
        group.widget_info(|| WidgetInfo::labeled(Role::RadioGroup, enabled, label));

        let mut spots = Vec::with_capacity(widths.len());
        let mut offset = 0.0;
        for width in &widths {
            let x = slot(direction, rect, offset, *width);
            spots.push(Rect::from_min_size(
                pos2(x, rect.top()),
                vec2(*width, button.y),
            ));
            offset += width + gap;
        }
        // One Tab stop for the group of versions: the version that has the
        // focus, or else the chosen one, or else the first. The "+" after
        // them is no option of the group: it is a Tab stop of its own.
        let radios = self.versions.len();
        let chosen_at = self
            .versions
            .iter()
            .position(|(value, _)| self.selected == Some(*value));
        let stop = tab_stop(ui, group.id, radios, chosen_at);
        let responses: Vec<Response> = spots
            .iter()
            .enumerate()
            .map(|(index, spot)| {
                if index < radios {
                    group_button(ui, *spot, group.id, index, stop)
                } else {
                    ui.interact(*spot, group.id.with(index), Sense::click())
                }
            })
            .collect();

        let mut picked = None;
        for ((value, name), response) in self.versions.iter().zip(&responses) {
            let chosen = self.selected == Some(*value);
            if response.clicked() && !chosen {
                picked = Some(VersionPick::Version(*value));
            }
            let name = *name;
            response.widget_info(|| WidgetInfo::selected(Role::RadioButton, enabled, chosen, name));
        }
        if let (Some(name), Some(response)) = (self.install, responses.get(radios)) {
            if response.clicked() {
                picked = Some(VersionPick::Install);
            }
            response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, name));
        }
        self.move_focus(ui, direction, &responses[..radios]);

        if ui.is_rect_visible(rect) {
            for (index, response) in responses.iter().enumerate() {
                let look = match self.forced {
                    Some((forced, look)) if forced == index => look,
                    Some(_) => Look::rest(),
                    None if enabled => Look::of(response),
                    None => Look::rest(),
                };
                match self.versions.get(index) {
                    Some((value, name)) => {
                        let chosen = self.selected == Some(*value);
                        self.paint_version(ui, theme, spots[index], name, chosen, look);
                    }
                    None => Self::paint_install(ui, theme, self.size, spots[index], look),
                }
            }
        }
        InnerResponse::new(picked, group)
    }

    /// The arrow keys, Home, and End move the focus between the versions
    /// (`responses` holds no "+"); the focused version asks egui to leave
    /// them to the group. The version the focus moves to becomes the Tab
    /// stop on the next frame.
    fn move_focus(&self, ui: &Ui, direction: Direction, responses: &[Response]) {
        let Some(at) = responses.iter().position(Response::has_focus) else {
            return;
        };
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                responses[at].id,
                EventFilter {
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    ..EventFilter::default()
                },
            );
        });
        let step = ui.input(|input| {
            KEYS.into_iter()
                .find(|key| input.key_pressed(*key))
                .and_then(|key| step_for(key, direction))
        });
        if let Some(step) = step {
            responses[moved(at, responses.len(), step)].request_focus();
        }
    }

    fn paint_version(
        &self,
        ui: &Ui,
        theme: &Theme,
        spot: Rect,
        name: &str,
        chosen: bool,
        look: Look,
    ) {
        let paint = button_paint(theme, self.size, chosen, look.touch);
        let painter = ui.painter();
        let radius = self.size.radius(theme);
        let corner = CornerRadius::same(radius);
        painter.rect_filled(spot, corner, paint.fill);
        painter.rect_stroke(
            spot,
            corner,
            Stroke::new(theme.size.border, paint.border),
            StrokeKind::Inside,
        );
        let token = TextToken {
            size: self.size.text(theme).size,
            face: paint.face,
        };
        let galley = layout(painter, name, token, Color32::PLACEHOLDER);
        let at = Align2::CENTER_CENTER.align_size_within_rect(galley.size(), spot);
        painter.galley(at.min, galley, paint.text);
        if look.focused {
            paint_focus_ring(painter, theme, spot, radius);
        }
    }

    fn paint_install(ui: &Ui, theme: &Theme, size: PickerSize, spot: Rect, look: Look) {
        let painter = ui.painter();
        let radius = size.radius(theme);
        if let Some(fill) = quiet_fill(theme, look.touch) {
            painter.rect_filled(spot, CornerRadius::same(radius), fill);
        }
        let stroke = Stroke::new(theme.size.border, theme.color.control);
        let outline = rounded_outline(spot, f32::from(radius));
        painter.extend(Shape::dashed_line(
            &outline,
            stroke,
            theme.size.dash,
            theme.size.dash_gap,
        ));
        let icon =
            Rect::from_center_size(spot.center(), Vec2::splat(theme.size.version_install_icon));
        Icon::Plus.paint(painter, theme, icon, theme.color.mute);
        if look.focused {
            paint_focus_ring(painter, theme, spot, radius);
        }
    }
}

impl<T: Copy + PartialEq> Widget for VersionPicker<'_, T> {
    fn ui(self, ui: &mut Ui) -> Response {
        let shown = self.show(ui);
        let mut response = shown.response;
        if shown.inner.is_some() {
            response.mark_changed();
        }
        response
    }
}

/// What a site's version menu holds: no setting, so the site's own files
/// decide (`.php-version`, `composer.json`, the default), or one version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting<T> {
    SiteFiles,
    Version(T),
}

/// One row of the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a, T> {
    pub setting: Setting<T>,
    pub label: &'a str,
    /// The setting the site has now.
    pub checked: bool,
}

/// The rows of the menu, in order: "Use the site's files" first, then every
/// installed version.
pub fn entries<'a, T: Copy + PartialEq>(
    site_files: &'a str,
    versions: &'a [(T, &'a str)],
    current: Setting<T>,
) -> Vec<Entry<'a, T>> {
    let first = Entry {
        setting: Setting::SiteFiles,
        label: site_files,
        checked: current == Setting::SiteFiles,
    };
    let versions = versions.iter().map(|(value, label)| Entry {
        setting: Setting::Version(*value),
        label,
        checked: current == Setting::Version(*value),
    });
    std::iter::once(first).chain(versions).collect()
}

/// The compact version menu of a table row: a 30 high button with the
/// version and a chevron, then the reason in `caption` ("from
/// .php-version"). A click opens a menu with the choices.
#[derive(Clone, Copy, Debug)]
#[must_use = "a version menu is drawn only when it is added to a Ui"]
pub struct VersionMenu<'a, T> {
    /// What a screen reader says: "PHP version for shop".
    label: &'a str,
    /// The version in force, as the button shows it.
    shown: &'a str,
    /// The first row: "Use the site's files".
    site_files: &'a str,
    versions: &'a [(T, &'a str)],
    setting: Setting<T>,
    reason: Option<&'a str>,
    disabled: Option<&'a str>,
    look: Option<Look>,
}

impl<'a, T: Copy + PartialEq> VersionMenu<'a, T> {
    pub const fn new(
        label: &'a str,
        shown: &'a str,
        site_files: &'a str,
        versions: &'a [(T, &'a str)],
        setting: Setting<T>,
    ) -> Self {
        Self {
            label,
            shown,
            site_files,
            versions,
            setting,
            reason: None,
            disabled: None,
            look: None,
        }
    }

    /// Why the site has this version, after the button: "from .php-version".
    pub const fn reason(mut self, reason: &'a str) -> Self {
        self.reason = Some(reason);
        self
    }

    /// Disables the menu. Hovering it shows `reason`.
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }

    /// Draws the button as it looks in `look` (the gallery and the tests).
    pub const fn looks(mut self, look: Look) -> Self {
        self.look = Some(look);
        self
    }

    /// Draws the button and its reason. `inner` is the setting the user
    /// chose from the menu this frame.
    pub fn show(self, ui: &mut Ui) -> InnerResponse<Option<Setting<T>>> {
        if let Some(reason) = self.disabled {
            let shown = ui.add_enabled_ui(false, |ui| self.draw(ui)).inner;
            return InnerResponse::new(
                shown.inner,
                with_disabled_reason(shown.response, Some(reason)),
            );
        }
        self.draw(ui)
    }

    fn draw(self, ui: &mut Ui) -> InnerResponse<Option<Setting<T>>> {
        let theme = Theme::of(ui.ctx());
        ui.spacing_mut().item_spacing.x = theme.space.s2;
        // A line as high as the button, so a cell of a table centers it:
        // `ui.horizontal` is only as high as a label, and the button would
        // hang below the middle of the row.
        let line = vec2(ui.available_width(), theme.size.control_in_row);
        let (response, chosen) = ui
            .allocate_ui_with_layout(line, Direction::CURRENT.row(), |ui| {
                let (response, width) = self.button(ui, theme);
                let mut chosen = None;
                Popup::menu(&response).show(|ui| {
                    chosen = self.rows(ui, width);
                });
                if let Some(reason) = self.reason {
                    let caption = RichText::new(reason)
                        .font(theme.text.caption.font_id())
                        .color(theme.color.mute);
                    ui.add(Label::new(caption).selectable(false));
                }
                (response, chosen)
            })
            .inner;
        InnerResponse::new(chosen, response)
    }

    /// The button: the version, then a chevron. Its response and its width.
    fn button(&self, ui: &mut Ui, theme: &Theme) -> (Response, f32) {
        let direction = Direction::CURRENT;
        let token = TextToken {
            size: theme.text.body.size,
            face: Face::Medium,
        };
        let galley = layout(ui.painter(), self.shown, token, Color32::PLACEHOLDER);
        let chevron = theme.size.icon_badge;
        let (start, end) = (theme.space.s3, theme.space.s2);
        let content = start + galley.size().x + theme.space.s1 + chevron + end;
        let width = content.max(theme.size.version_menu_min);
        let height = theme.size.control_in_row;
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
        if ui.is_rect_visible(rect) {
            let enabled = ui.is_enabled();
            let look = match self.look {
                Some(look) => look,
                None if enabled => Look::of(&response),
                None => Look::rest(),
            };
            let painter = ui.painter();
            let corner = CornerRadius::same(theme.radius.md);
            painter.rect_filled(
                rect,
                corner,
                touched(theme, theme.color.surface, look.touch),
            );
            painter.rect_stroke(
                rect,
                corner,
                Stroke::new(theme.size.border, theme.color.control),
                StrokeKind::Inside,
            );
            let x = slot(direction, rect, start, galley.size().x);
            let spot = Rect::from_min_size(pos2(x, rect.top()), vec2(galley.size().x, height));
            let at = Align2::LEFT_CENTER.align_size_within_rect(galley.size(), spot);
            painter.galley(at.min, galley, theme.color.ink);
            let x = slot(direction, rect, width - end - chevron, chevron);
            let spot = Rect::from_min_size(pos2(x, rect.top()), vec2(chevron, height));
            Icon::ChevronDown.paint(painter, theme, spot, theme.color.mute);
            if look.focused {
                paint_focus_ring(painter, theme, rect, theme.radius.md);
            }
        }
        let (enabled, label) = (ui.is_enabled(), format!("{}, {}", self.label, self.shown));
        response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, &label));
        (response, width)
    }

    /// The rows of the menu, as wide as the space `ui` has: what the gallery
    /// shows open. Returns the setting of the row the user chose.
    pub fn list(&self, ui: &mut Ui) -> Option<Setting<T>> {
        let least = ui.available_width();
        self.rows(ui, least)
    }

    /// The rows, `least` points wide or as wide as the widest needs: the
    /// popup is as wide as the button, and wider for a long text.
    fn rows(&self, ui: &mut Ui, least: f32) -> Option<Setting<T>> {
        let theme = Theme::of(ui.ctx());
        // The rows touch: each has its own height and its own hover fill.
        ui.spacing_mut().item_spacing.y = 0.0;
        let entries = entries(self.site_files, self.versions, self.setting);
        let width = entries
            .iter()
            .map(|entry| menu::row_width(ui, theme, &menu_spec(entry)))
            .fold(least, f32::max);
        let mut chosen = None;
        for (index, entry) in entries.iter().enumerate() {
            if menu_row(ui, theme, entry, width).clicked() && !entry.checked {
                chosen = Some(entry.setting);
            }
            if index == 0 {
                ui.separator();
            }
        }
        chosen
    }
}

/// What a row of the menu says: a check when it is the current setting.
fn menu_spec<'a, T>(entry: &Entry<'a, T>) -> menu::RowSpec<'a> {
    menu::RowSpec {
        label: entry.label,
        mark: menu::Mark::Radio(entry.checked),
        tone: menu::Tone::Normal,
    }
}

/// One row of the menu, `width` wide: a check when it is the current
/// setting, then the text.
fn menu_row<T>(ui: &mut Ui, theme: &Theme, entry: &Entry<'_, T>, width: f32) -> Response {
    menu::row(ui, theme, &menu_spec(entry), width)
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Event, Id, Modifiers, RawInput};

    use super::*;
    use crate::strings;
    use crate::theme::tests::{TEXT, UI, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::Button;
    use crate::widgets::harness::{Harness, over};

    const PHP: [(u8, &str); 3] = [(83, "8.3"), (84, "8.4"), (85, "8.5")];
    const THEMES: [(&str, &Theme, egui::Theme); 2] = [
        ("light", &LIGHT, egui::Theme::Light),
        ("dark", &DARK, egui::Theme::Dark),
    ];

    #[test]
    fn the_focus_moves_one_stop_and_stops_at_both_ends() {
        assert_eq!(moved(0, 4, Step::Next), 1);
        assert_eq!(moved(2, 4, Step::Next), 3);
        assert_eq!(moved(3, 4, Step::Next), 3, "no wrap at the end");
        assert_eq!(moved(1, 4, Step::Previous), 0);
        assert_eq!(moved(0, 4, Step::Previous), 0, "no wrap at the start");
        assert_eq!(moved(2, 4, Step::First), 0);
        assert_eq!(moved(1, 4, Step::Last), 3);
        // An empty group and a stale position never go out of range.
        assert_eq!(moved(0, 0, Step::Next), 0);
        assert_eq!(moved(0, 0, Step::Last), 0);
        assert_eq!(moved(9, 4, Step::Previous), 2);
        assert_eq!(moved(usize::MAX, 4, Step::Next), 3);
    }

    #[test]
    fn the_keys_map_to_steps_and_a_right_to_left_row_swaps_the_arrows() {
        let ltr = |key| step_for(key, Direction::Ltr);
        let rtl = |key| step_for(key, Direction::Rtl);
        assert_eq!(ltr(Key::ArrowRight), Some(Step::Next));
        assert_eq!(ltr(Key::ArrowLeft), Some(Step::Previous));
        assert_eq!(rtl(Key::ArrowRight), Some(Step::Previous));
        assert_eq!(rtl(Key::ArrowLeft), Some(Step::Next));
        for direction in [Direction::Ltr, Direction::Rtl] {
            assert_eq!(step_for(Key::ArrowDown, direction), Some(Step::Next));
            assert_eq!(step_for(Key::ArrowUp, direction), Some(Step::Previous));
            assert_eq!(step_for(Key::Home, direction), Some(Step::First));
            assert_eq!(step_for(Key::End, direction), Some(Step::Last));
            // Enter and Space are clicks, and Tab is egui's.
            for key in [Key::Enter, Key::Space, Key::Tab, Key::A] {
                assert_eq!(step_for(key, direction), None);
            }
        }
        assert!(
            KEYS.iter()
                .all(|key| step_for(*key, Direction::Ltr).is_some())
        );
    }

    #[test]
    fn the_chosen_version_is_accent_at_500_and_the_others_are_surface_with_a_control_border_at_300()
    {
        for theme in [&LIGHT, &DARK] {
            let c = &theme.color;
            for size in [PickerSize::Large, PickerSize::Medium] {
                let chosen = button_paint(theme, size, true, Touch::Rest);
                assert_eq!(chosen.fill, c.accent);
                assert_eq!(chosen.border, c.accent);
                assert_eq!(chosen.text, c.on_accent);
                assert_eq!(chosen.face, Face::Medium);
                let other = button_paint(theme, size, false, Touch::Rest);
                assert_eq!(other.fill, c.surface);
                assert_eq!(other.border, c.control);
                assert_eq!(other.text, c.ink);
                assert_eq!(other.face, Face::Light);
            }
        }
    }

    #[test]
    fn hover_and_pressed_move_a_version_button_toward_ink_by_6_and_12_percent() {
        for theme in [&LIGHT, &DARK] {
            for chosen in [true, false] {
                let fill = |touch| button_paint(theme, PickerSize::Large, chosen, touch).fill;
                assert_eq!(fill(Touch::Hover), theme.shade(fill(Touch::Rest), 0.06));
                assert_eq!(fill(Touch::Pressed), theme.shade(fill(Touch::Rest), 0.12));
            }
        }
    }

    #[test]
    fn the_border_of_an_unselected_version_stands_out_in_both_themes() {
        for (name, theme, _) in THEMES {
            let c = &theme.color;
            for size in [PickerSize::Large, PickerSize::Medium] {
                let rest = button_paint(theme, size, false, Touch::Rest);
                assert_eq!(rest.border, c.control, "{name}: the control border");
                // The border is the button's edge: it stands out from the
                // page outside, and from the button's own fill at rest
                // (WCAG 1.4.11, 3:1).
                let ratio = contrast(rest.border, rest.fill);
                assert!(ratio >= UI, "{name} {size:?} on its fill: {ratio:.2}:1");
            }
            for (page_name, page) in [("bg", c.bg), ("side", c.side), ("surface", c.surface)] {
                let ratio = contrast(c.control, page);
                assert!(ratio >= UI, "{name} on {page_name}: {ratio:.2}:1");
            }
        }
    }

    #[test]
    fn the_version_text_reads_in_every_state_and_both_themes() {
        for (name, theme, _) in THEMES {
            for size in [PickerSize::Large, PickerSize::Medium] {
                for chosen in [true, false] {
                    for touch in [Touch::Rest, Touch::Hover, Touch::Pressed] {
                        let paint = button_paint(theme, size, chosen, touch);
                        let ratio = contrast(paint.text, paint.fill);
                        assert!(
                            ratio >= TEXT,
                            "{name} {size:?} chosen {chosen} {touch:?}: {ratio:.2}:1"
                        );
                    }
                }
            }
            // The dashed install button: its outline and its plus stand out
            // on the page, and the plus on its hover fill.
            let c = &theme.color;
            let hover = over(c.selected.gamma_multiply(theme.state.row_hover), c.bg);
            for page in [c.bg, c.surface, hover] {
                assert!(contrast(c.control, page) >= UI, "{name} dashed outline");
                assert!(contrast(c.mute, page) >= UI, "{name} plus");
            }
        }
    }

    /// The rect a picker takes.
    fn picker_rect(theme: egui::Theme, picker: impl Fn() -> VersionPicker<'static, u8>) -> Rect {
        let harness = Harness::new(theme);
        let mut rect = Rect::NOTHING;
        harness.run(|ui| rect = ui.add(picker()).rect);
        rect
    }

    #[test]
    fn a_large_button_is_92_by_64_a_medium_one_44_high_and_the_install_button_a_square() {
        let theme = &LIGHT;
        let large = |install: bool| {
            picker_rect(egui::Theme::Light, move || {
                let picker = VersionPicker::new("Default PHP", &PHP, Some(84));
                if install {
                    picker.install("Install")
                } else {
                    picker
                }
            })
        };
        let gap = theme.space.group_gap;
        assert_eq!(large(false).height(), 64.0);
        assert_eq!(large(false).width(), 3.0 * 92.0 + 2.0 * gap);
        // A square as high as the buttons, after one more gap.
        assert_eq!(large(true).width(), large(false).width() + gap + 64.0);
        let medium = picker_rect(egui::Theme::Light, || {
            VersionPicker::new("Default PHP", &PHP, Some(84))
                .size(PickerSize::Medium)
                .install("Install")
        });
        assert_eq!(medium.height(), 44.0);
        assert_eq!(medium.width(), 3.0 * 92.0 + 3.0 * gap + 44.0);
        assert_eq!(
            (
                theme.size.version_lg.x,
                theme.size.version_lg.y,
                theme.size.version_md
            ),
            (92.0, 64.0, 44.0)
        );
        // A text too long for 92 widens its button and nothing else.
        let wide = picker_rect(egui::Theme::Light, || {
            VersionPicker::new(
                "Default PHP",
                &[(1, "8.4.27-beta-long-name"), (2, "8.5")],
                Some(2),
            )
        });
        assert!(wide.width() > 2.0 * 92.0 + gap);
    }

    #[test]
    fn a_screen_reader_hears_a_radio_group_of_versions_and_the_install_button_as_a_button() {
        for (size, runtime) in [(PickerSize::Large, "PHP"), (PickerSize::Medium, "Node.js")] {
            let harness = Harness::new(egui::Theme::Light);
            let install = strings::install_another_label(runtime);
            let tree = harness.accessibility(|ui| {
                ui.add(
                    VersionPicker::new("Default PHP", &PHP, Some(84))
                        .size(size)
                        .install(&install),
                );
            });
            let group = tree.node_with_label("Default PHP").expect("group");
            assert_eq!(group.role(), egui::Role::RadioGroup);
            for (version, text) in PHP {
                let node = tree.node_with_label(text).expect(text);
                assert_eq!(node.role(), egui::Role::RadioButton);
                let expected = if version == 84 {
                    egui::accesskit::Toggled::True
                } else {
                    egui::accesskit::Toggled::False
                };
                assert_eq!(node.toggled(), Some(expected), "{text}");
            }
            // The "+" is an action, not an option: a button with no state of
            // being chosen, named "Install another <runtime> version".
            let install = tree.node_with_label(&install).expect("install");
            assert_eq!(install.role(), egui::Role::Button, "{size:?}");
            assert_eq!(install.toggled(), None, "{size:?}");
        }
        assert_eq!(
            strings::install_another_label("PHP"),
            "Install another PHP version"
        );
    }

    /// Where the keyboard focus is.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum At {
        Nowhere,
        Before,
        /// The version button of this index.
        Button(usize),
        /// The dashed "+" button.
        Install,
        After,
    }

    /// One frame of a picker between two buttons, so Tab has somewhere to go
    /// before it and after it: what it chose and where the focus is.
    struct Frame {
        picked: Option<VersionPick<u8>>,
        at: At,
        group: Id,
    }

    fn frame_with(
        harness: &Harness,
        input: RawInput,
        selected: Option<u8>,
        direction: Direction,
    ) -> Frame {
        let mut picked = None;
        let (mut before, mut group, mut after) = (Id::NULL, Id::NULL, Id::NULL);
        let draw = |ui: &mut Ui| {
            before = ui.add(Button::secondary("Before")).id;
            let shown = VersionPicker::new("Default PHP", &PHP, selected)
                .install("Install")
                .show_in(ui, direction);
            picked = shown.inner;
            group = shown.response.id;
            after = ui.add(Button::secondary("After")).id;
        };
        harness.frame(input, draw);
        let focused = harness.ctx.memory(|m| m.focused());
        let at = if focused.is_none() {
            At::Nowhere
        } else if focused == Some(before) {
            At::Before
        } else if focused == Some(after) {
            At::After
        } else if focused == Some(group.with(PHP.len())) {
            At::Install
        } else {
            (0..PHP.len())
                .find(|index| focused == Some(group.with(*index)))
                .map_or(At::Nowhere, At::Button)
        };
        Frame { picked, at, group }
    }

    fn key_input(press: Option<(Key, Modifiers)>) -> RawInput {
        let events = press.map_or_else(Vec::new, |(key, modifiers)| {
            vec![Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers,
            }]
        });
        RawInput {
            events,
            ..RawInput::default()
        }
    }

    fn frame(harness: &Harness, press: Option<Key>, selected: Option<u8>) -> Frame {
        let input = key_input(press.map(|key| (key, Modifiers::NONE)));
        frame_with(harness, input, selected, Direction::Ltr)
    }

    /// A key press, then a frame with no input. The app runs that frame too:
    /// egui asks for a repaint after every frame that had input. It moves the
    /// focus of a Shift+Tab a frame late, and a button that has just got the
    /// focus asks for the arrows in the frame after. What the key chose, and
    /// where the focus is then.
    fn press_in(
        harness: &Harness,
        key: Key,
        modifiers: Modifiers,
        selected: Option<u8>,
        direction: Direction,
    ) -> (Option<VersionPick<u8>>, At) {
        let input = key_input(Some((key, modifiers)));
        let pressed = frame_with(harness, input, selected, direction);
        let settled = frame_with(harness, key_input(None), selected, direction);
        (pressed.picked, settled.at)
    }

    fn press(harness: &Harness, key: Key, selected: Option<u8>) -> (Option<VersionPick<u8>>, At) {
        press_in(harness, key, Modifiers::NONE, selected, Direction::Ltr)
    }

    fn shift_tab(harness: &Harness, selected: Option<u8>) -> At {
        press_in(
            harness,
            Key::Tab,
            Modifiers::SHIFT,
            selected,
            Direction::Ltr,
        )
        .1
    }

    #[test]
    fn the_versions_are_one_tab_stop_entered_on_the_chosen_one_and_the_plus_is_the_next() {
        let harness = Harness::new(egui::Theme::Light);
        let selected = Some(84);
        assert_eq!(frame(&harness, None, selected).at, At::Nowhere);
        // Tab walks Before, the group, the "+", After: one stop for the three
        // versions, on the chosen one (8.4, the second), then the "+" alone.
        assert_eq!(press(&harness, Key::Tab, selected), (None, At::Before));
        assert_eq!(press(&harness, Key::Tab, selected), (None, At::Button(1)));
        // The next Tab leaves the group; it does not visit 8.5.
        assert_eq!(press(&harness, Key::Tab, selected), (None, At::Install));
        assert_eq!(press(&harness, Key::Tab, selected), (None, At::After));
        assert_eq!(press(&harness, Key::Tab, selected).1, At::Before, "a loop");
        // Shift+Tab goes back the same way: After, "+", the chosen version.
        press(&harness, Key::Tab, selected);
        press(&harness, Key::Tab, selected);
        press(&harness, Key::Tab, selected);
        assert_eq!(shift_tab(&harness, selected), At::Install);
        assert_eq!(shift_tab(&harness, selected), At::Button(1));
        assert_eq!(shift_tab(&harness, selected), At::Before);
        // Tab leaves from a version the arrows moved to, for the "+"; the way
        // back in is on the chosen version, not on the one that was left.
        press(&harness, Key::Tab, selected);
        assert_eq!(press(&harness, Key::ArrowRight, selected).1, At::Button(2));
        assert_eq!(press(&harness, Key::Tab, selected).1, At::Install);
        assert_eq!(shift_tab(&harness, selected), At::Button(1));
    }

    #[test]
    fn with_no_version_chosen_tab_enters_the_picker_on_the_first_button() {
        let harness = Harness::new(egui::Theme::Light);
        press(&harness, Key::Tab, None);
        assert_eq!(press(&harness, Key::Tab, None).1, At::Button(0));
        assert_eq!(press(&harness, Key::Tab, None).1, At::Install);
        // A chosen version that is not in the list is no choice either.
        let harness = Harness::new(egui::Theme::Light);
        press(&harness, Key::Tab, Some(99));
        assert_eq!(press(&harness, Key::Tab, Some(99)).1, At::Button(0));
    }

    #[test]
    fn tab_enters_on_whichever_version_is_chosen() {
        for (selected, button) in [(83, 0), (84, 1), (85, 2)] {
            let harness = Harness::new(egui::Theme::Light);
            press(&harness, Key::Tab, Some(selected));
            let entered = press(&harness, Key::Tab, Some(selected));
            assert_eq!(entered, (None, At::Button(button)), "{selected}");
        }
    }

    /// What has the focus after each Tab, for a picker with these versions
    /// and this "+", between a button before it and one after it.
    fn tab_walk(versions: &[(u8, &str)], install: bool, tabs: usize) -> Vec<String> {
        let harness = Harness::new(egui::Theme::Light);
        let ids = std::cell::Cell::new([Id::NULL; 3]);
        let mut draw = |ui: &mut Ui| {
            let before = ui.add(Button::secondary("Before")).id;
            let picker = VersionPicker::new("Default PHP", versions, versions.first().map(|v| v.0));
            let picker = if install {
                picker.install("Install")
            } else {
                picker
            };
            let group = picker.show(ui).response.id;
            let after = ui.add(Button::secondary("After")).id;
            ids.set([before, group, after]);
        };
        (0..tabs)
            .map(|_| {
                harness.frame(key_input(Some((Key::Tab, Modifiers::NONE))), &mut draw);
                harness.frame(key_input(None), &mut draw);
                let [before, group, after] = ids.get();
                let focused = harness.ctx.memory(|m| m.focused());
                match focused {
                    f if f == Some(before) => "before".to_owned(),
                    f if f == Some(after) => "after".to_owned(),
                    f => (0..=versions.len())
                        .find(|index| f == Some(group.with(*index)))
                        .map_or("nowhere".to_owned(), |index| format!("button {index}")),
                }
            })
            .collect()
    }

    #[test]
    fn the_plus_is_a_tab_stop_whatever_is_around_it() {
        let two = [(83, "8.3"), (84, "8.4")];
        // Without a "+" the group is a single stop, then After.
        assert_eq!(
            tab_walk(&two, false, 4),
            ["before", "button 0", "after", "before"]
        );
        // With one: the group, then the "+" (index 2, after the versions).
        assert_eq!(
            tab_walk(&two, true, 4),
            ["before", "button 0", "button 2", "after"]
        );
        // No version installed: the "+" is the only button, and still a stop.
        assert_eq!(tab_walk(&[], true, 3), ["before", "button 0", "after"]);
        assert_eq!(tab_walk(&[], false, 2), ["before", "after"]);
    }

    #[test]
    fn the_arrows_move_the_focus_and_enter_or_space_choose() {
        let harness = Harness::new(egui::Theme::Light);
        let selected = Some(84);
        frame(&harness, None, selected);
        press(&harness, Key::Tab, selected);
        // Tab enters on the chosen version, and chooses nothing.
        assert_eq!(press(&harness, Key::Tab, selected), (None, At::Button(1)));
        let arrow = |key| press(&harness, key, selected);
        // The arrows move the focus and never choose.
        assert_eq!(arrow(Key::ArrowRight), (None, At::Button(2)));
        // The last version is the end: the "+" is not one of the options.
        assert_eq!(arrow(Key::ArrowRight), (None, At::Button(2)));
        assert_eq!(arrow(Key::ArrowDown), (None, At::Button(2)));
        assert_eq!(arrow(Key::Home), (None, At::Button(0)));
        assert_eq!(arrow(Key::ArrowLeft), (None, At::Button(0)));
        assert_eq!(arrow(Key::ArrowUp), (None, At::Button(0)));
        assert_eq!(arrow(Key::End), (None, At::Button(2)), "not the \"+\"");
        assert_eq!(arrow(Key::ArrowUp), (None, At::Button(1)));
        assert_eq!(arrow(Key::ArrowDown), (None, At::Button(2)));
        assert_eq!(arrow(Key::ArrowLeft), (None, At::Button(1)));
        assert_eq!(arrow(Key::ArrowLeft), (None, At::Button(0)));
        // Enter chooses the focused version, and the focus stays on it.
        assert_eq!(
            press(&harness, Key::Enter, selected),
            (Some(VersionPick::Version(83)), At::Button(0))
        );
        // On the chosen version, Space does not.
        arrow(Key::ArrowRight);
        assert_eq!(press(&harness, Key::Space, selected), (None, At::Button(1)));
        // Space on another version.
        arrow(Key::ArrowRight);
        assert_eq!(
            press(&harness, Key::Space, selected),
            (Some(VersionPick::Version(85)), At::Button(2))
        );
        // Enter and Space on the "+", which Tab reaches, ask for Install.
        assert_eq!(press(&harness, Key::Tab, selected).1, At::Install);
        assert_eq!(
            press(&harness, Key::Enter, selected),
            (Some(VersionPick::Install), At::Install)
        );
        assert_eq!(
            press(&harness, Key::Space, selected),
            (Some(VersionPick::Install), At::Install)
        );
    }

    #[test]
    fn in_a_right_to_left_row_the_arrows_are_mirrored_and_the_tab_stops_are_the_same() {
        let harness = Harness::new(egui::Theme::Light);
        let selected = Some(84);
        let rtl = |key| press_in(&harness, key, Modifiers::NONE, selected, Direction::Rtl);
        rtl(Key::Tab);
        assert_eq!(rtl(Key::Tab).1, At::Button(1));
        // The first button is at the right: the left arrow goes on, the
        // right arrow goes back, and neither chooses.
        assert_eq!(rtl(Key::ArrowLeft), (None, At::Button(2)));
        assert_eq!(rtl(Key::ArrowLeft), (None, At::Button(2)));
        assert_eq!(rtl(Key::ArrowRight), (None, At::Button(1)));
        assert_eq!(rtl(Key::ArrowRight), (None, At::Button(0)));
        assert_eq!(rtl(Key::ArrowRight), (None, At::Button(0)));
        assert_eq!(rtl(Key::End).1, At::Button(2));
        assert_eq!(rtl(Key::Home).1, At::Button(0));
        // Up and Down do not depend on the direction.
        assert_eq!(rtl(Key::ArrowDown).1, At::Button(1));
        // The "+" is after the group in the row's own direction, and Tab goes
        // to it next.
        assert_eq!(rtl(Key::Tab).1, At::Install);
        assert_eq!(rtl(Key::Tab).1, At::After);
    }

    #[test]
    fn a_screen_reader_can_focus_any_version_not_only_the_tab_stop() {
        let harness = Harness::new(egui::Theme::Light);
        let selected = Some(84);
        let group = frame(&harness, None, selected).group;
        let tree = harness.accessibility(|ui| {
            ui.add(VersionPicker::new("Default PHP", &PHP, selected).install("Install"));
        });
        for label in ["8.3", "8.4", "8.5", "Install"] {
            let node = tree.node_with_label(label).expect(label);
            assert!(
                node.supports_action(egui::accesskit::Action::Focus),
                "{label}"
            );
        }
        // It asks for the focus on 8.5, which is not the Tab stop.
        let request = egui::accesskit::ActionRequest {
            action: egui::accesskit::Action::Focus,
            target_node: group.with(2).accesskit_id(),
            target_tree: egui::accesskit::TreeId::ROOT,
            data: None,
        };
        let input = RawInput {
            events: vec![Event::AccessKitActionRequest(request)],
            ..RawInput::default()
        };
        frame_with(&harness, input, selected, Direction::Ltr);
        assert_eq!(frame(&harness, None, selected).at, At::Button(2));
        // The focus is on 8.5 now, so it is the Tab stop; Tab goes to the "+".
        assert_eq!(press(&harness, Key::Tab, selected).1, At::Install);
    }

    #[test]
    fn a_click_chooses_another_version_and_the_chosen_one_and_a_disabled_picker_report_nothing() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let origin = picker_rect(egui::Theme::Light, || {
            VersionPicker::new("Default PHP", &PHP, Some(84))
        });
        let at = |index: usize| {
            let x = origin.left()
                + index as f32 * (theme.size.version_lg.x + theme.space.group_gap)
                + 10.0;
            pos2(x, origin.center().y)
        };
        // A widget is known to egui from the frame before.
        harness.run(|ui| {
            ui.add(VersionPicker::new("Default PHP", &PHP, Some(84)));
        });
        let click = |index: usize, disabled: bool| {
            let mut picked = None;
            harness.click(at(index), |ui| {
                let picker = VersionPicker::new("Default PHP", &PHP, Some(84));
                let picker = if disabled {
                    picker.disabled("Why")
                } else {
                    picker
                };
                picked = picker.show(ui).inner;
            });
            picked
        };
        assert_eq!(click(0, false), Some(VersionPick::Version(83)));
        assert_eq!(click(2, false), Some(VersionPick::Version(85)));
        assert_eq!(click(1, false), None, "84 was already chosen");
        assert_eq!(click(0, true), None);
    }

    #[test]
    fn it_works_for_any_runtime_not_only_php() {
        // Node's majors, and a later Python: values and texts are the caller's.
        let node = [("22", "22"), ("20", "20"), ("18", "18")];
        let python = [(3_u8, "3.13"), (4, "3.12")];
        let harness = Harness::new(egui::Theme::Light);
        let mut rects = Vec::new();
        harness.run(|ui| {
            rects.push(
                ui.add(VersionPicker::new("Default Node.js", &node, Some("20")))
                    .rect,
            );
            rects.push(
                ui.add(VersionPicker::new("Default Python", &python, None))
                    .rect,
            );
        });
        assert_eq!(rects[0].height(), LIGHT.size.version_lg.y);
        assert!(rects[0].width() > rects[1].width());
        // Nothing chosen is allowed: no button is accent.
        let chosen = |version: Option<&str>| {
            harness
                .shapes(|ui| {
                    ui.add(VersionPicker::new("Default Node.js", &node, version));
                })
                .into_iter()
                .filter(|shape| matches!(shape, Shape::Rect(r) if r.fill == LIGHT.color.accent))
                .count()
        };
        assert_eq!(chosen(Some("20")), 1);
        assert_eq!(chosen(None), 0);
    }

    #[test]
    fn the_install_button_has_a_dashed_control_colored_outline_and_the_focus_ring() {
        for (name, theme, active) in THEMES {
            let harness = Harness::new(active);
            let shapes = |look: Option<Look>| {
                harness.shapes(|ui| {
                    let picker =
                        VersionPicker::new("Default PHP", &PHP, Some(84)).install("Install");
                    let picker = match look {
                        Some(look) => picker.looks(3, look),
                        None => picker,
                    };
                    ui.add(picker);
                })
            };
            let dashes = shapes(None)
                .into_iter()
                .filter(|shape| matches!(shape, Shape::LineSegment { stroke, .. } if stroke.color == theme.color.control))
                .count();
            assert!(dashes > 8, "{name}: {dashes} dashes");
            let ring = |look| {
                shapes(Some(look)).into_iter().any(|shape| matches!(shape, Shape::Rect(r) if r.stroke_kind == StrokeKind::Outside && r.stroke.color == theme.color.accent))
            };
            assert!(ring(Look::focused()));
            assert!(!ring(Look::rest()));
        }
    }

    #[test]
    fn the_dashed_outline_of_the_plus_is_a_1_px_control_line_in_every_look_and_size() {
        for (name, theme, active) in THEMES {
            let harness = Harness::new(active);
            for size in [PickerSize::Large, PickerSize::Medium] {
                for look in [
                    Look::rest(),
                    Look::hover(),
                    Look::pressed(),
                    Look::focused(),
                ] {
                    let strokes: Vec<Stroke> = harness
                        .shapes(|ui| {
                            ui.add(
                                VersionPicker::new("Default PHP", &PHP, Some(84))
                                    .size(size)
                                    .install("Install")
                                    .looks(3, look),
                            );
                        })
                        .into_iter()
                        .filter_map(|shape| match shape {
                            Shape::LineSegment { stroke, .. } => Some(stroke),
                            _ => None,
                        })
                        .collect();
                    // The lines are the dashes (`control`) and the plus
                    // (`mute`); none is `line` or any other color.
                    let c = &theme.color;
                    let what = format!("{name} {size:?} {look:?}");
                    assert!(
                        strokes
                            .iter()
                            .all(|stroke| stroke.color == c.control || stroke.color == c.mute),
                        "{what}: {strokes:?}"
                    );
                    let dashes: Vec<_> = strokes
                        .iter()
                        .filter(|stroke| stroke.color == c.control)
                        .collect();
                    assert!(dashes.len() > 8, "{what}: {} dashes", dashes.len());
                    assert!(
                        dashes
                            .iter()
                            .all(|stroke| stroke.width == theme.size.border),
                        "{what}: the dashes are {} wide",
                        theme.size.border
                    );
                }
            }
        }
    }

    #[test]
    fn the_dashed_outline_goes_around_the_rect_once_and_closes() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(64.0, 64.0));
        let points = rounded_outline(rect, 12.0);
        assert_eq!(points.len(), 4 * (ARC_STEPS + 1) + 1);
        assert_eq!(points.first(), points.last());
        let bounds = points.iter().fold(Rect::NOTHING, |bounds, point| {
            bounds.union(Rect::from_min_max(*point, *point))
        });
        assert!((bounds.min - rect.min).length() < 1e-3);
        assert!((bounds.max - rect.max).length() < 1e-3);
        // Every point is 12 from a corner's center or on a straight side.
        for point in &points {
            assert!(rect.expand(1e-3).contains(*point));
        }
    }

    #[test]
    fn the_menu_lists_the_site_files_first_then_every_version_with_a_check_on_the_setting() {
        let rows = |setting| entries("Use the site's files", &PHP, setting);
        let labels: Vec<&str> = rows(Setting::SiteFiles).iter().map(|e| e.label).collect();
        assert_eq!(labels, ["Use the site's files", "8.3", "8.4", "8.5"]);
        let checks = |setting| rows(setting).iter().map(|e| e.checked).collect::<Vec<_>>();
        assert_eq!(checks(Setting::SiteFiles), [true, false, false, false]);
        assert_eq!(checks(Setting::Version(84)), [false, false, true, false]);
        assert_eq!(checks(Setting::Version(99)), [false, false, false, false]);
        assert_eq!(rows(Setting::SiteFiles)[0].setting, Setting::SiteFiles);
        assert_eq!(rows(Setting::SiteFiles)[3].setting, Setting::Version(85));
        assert_eq!(strings::USE_SITE_FILES, "Use the site's files");
    }

    fn menu(setting: Setting<u8>) -> VersionMenu<'static, u8> {
        VersionMenu::new(
            "PHP version for shop",
            "8.4",
            strings::USE_SITE_FILES,
            &PHP,
            setting,
        )
        .reason("from .php-version")
    }

    #[test]
    fn the_menu_button_is_30_high_at_least_58_wide_and_the_reason_follows_it() {
        let harness = Harness::new(egui::Theme::Light);
        let mut rects = Vec::new();
        let tree = harness.accessibility(|ui| {
            rects.push(menu(Setting::SiteFiles).show(ui).response.rect);
            let long = VersionMenu::new(
                "PHP version for api",
                "8.4.27-long",
                "Files",
                &PHP,
                Setting::SiteFiles,
            );
            rects.push(long.show(ui).response.rect);
            let short = VersionMenu::new("Node version", "9", "Files", &PHP, Setting::SiteFiles);
            rects.push(short.show(ui).response.rect);
        });
        let theme = &LIGHT;
        for rect in &rects {
            assert_eq!(rect.height(), theme.size.control_in_row);
            assert!(rect.width() >= theme.size.version_menu_min);
        }
        assert_eq!(rects[2].width(), theme.size.version_menu_min);
        assert!(rects[1].width() > rects[0].width());
        assert_eq!(
            (theme.size.control_in_row, theme.size.version_menu_min),
            (30.0, 58.0)
        );
        let button = tree
            .node_with_label("PHP version for shop, 8.4")
            .expect("button");
        assert_eq!(button.role(), egui::Role::Button);
        assert!(
            tree.node_with_text("from .php-version").is_some(),
            "the reason"
        );
        // The reason reads on a row of a table, at rest and under the pointer.
        for (name, theme, _) in THEMES {
            let hover = over(
                theme.color.selected.gamma_multiply(theme.state.row_hover),
                theme.color.surface,
            );
            for page in [theme.color.surface, hover] {
                assert!(contrast(theme.color.mute, page) >= TEXT, "{name}");
            }
        }
    }

    #[test]
    fn a_click_opens_the_menu_and_a_row_chooses_a_setting() {
        let harness = Harness::new(egui::Theme::Light);
        let mut button = Rect::NOTHING;
        harness.run(|ui| button = menu(Setting::SiteFiles).show(ui).response.rect);
        let mut chosen = None;
        harness.click(button.center(), |ui| {
            chosen = menu(Setting::SiteFiles).show(ui).inner;
        });
        assert_eq!(chosen, None, "opening the menu chooses nothing");
        assert!(Popup::is_any_open(&harness.ctx));
        // The menu is open: its rows are in the tree, in order.
        let tree = harness.accessibility(|ui| {
            menu(Setting::Version(84)).show(ui);
        });
        for label in ["Use the site's files", "8.3", "8.4", "8.5"] {
            let row = tree.node_with_label(label).expect(label);
            assert_eq!(row.role(), egui::Role::MenuItemRadio, "{label}");
        }
        let checked = |label: &str| tree.node_with_label(label).and_then(|n| n.toggled());
        assert_eq!(checked("8.4"), Some(egui::accesskit::Toggled::True));
        assert_eq!(checked("8.5"), Some(egui::accesskit::Toggled::False));
        // A click on the row of 8.5 chooses it.
        let bounds = tree
            .node_with_label("8.5")
            .and_then(|n| n.bounds())
            .expect("bounds");
        let at = pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        let mut chosen = None;
        harness.click(at, |ui| {
            chosen = menu(Setting::Version(84)).show(ui).inner;
        });
        assert_eq!(chosen, Some(Setting::Version(85)));
    }

    #[test]
    fn the_rows_of_the_menu_choose_the_site_files_and_ignore_the_current_setting() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            menu(Setting::Version(84)).list(ui);
        });
        let spot = |label: &str| {
            let b = tree
                .node_with_label(label)
                .and_then(|n| n.bounds())
                .expect(label);
            pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32)
        };
        let pick = |label: &str| {
            let mut chosen = None;
            harness.click(spot(label), |ui| {
                chosen = menu(Setting::Version(84)).list(ui)
            });
            chosen
        };
        assert_eq!(pick("Use the site's files"), Some(Setting::SiteFiles));
        assert_eq!(pick("8.3"), Some(Setting::Version(83)));
        assert_eq!(pick("8.4"), None, "8.4 is the setting already");
    }

    #[test]
    fn a_disabled_menu_says_why_and_opens_nothing() {
        let harness = Harness::new(egui::Theme::Light);
        let mut button = Rect::NOTHING;
        let draw = |ui: &mut Ui| {
            menu(Setting::SiteFiles)
                .disabled("No PHP is installed")
                .show(ui)
                .response
                .rect
        };
        harness.run(|ui| button = draw(ui));
        harness.click(button.center(), |ui| {
            draw(ui);
        });
        assert!(!Popup::is_any_open(&harness.ctx));
    }

    #[test]
    fn the_check_in_the_menu_stands_out_from_the_menu() {
        for (name, theme, _) in THEMES {
            assert!(
                contrast(theme.color.accent, theme.color.surface) >= UI,
                "{name}"
            );
            assert!(
                contrast(theme.color.ink, theme.color.surface) >= TEXT,
                "{name}"
            );
        }
    }

    #[test]
    fn the_large_size_has_the_large_corner_and_the_medium_size_the_medium_one() {
        for theme in [&crate::theme::LIGHT, &crate::theme::DARK] {
            assert_eq!(PickerSize::Large.radius(theme), theme.radius.lg);
            assert_eq!(PickerSize::Medium.radius(theme), theme.radius.md);
        }
    }
}
