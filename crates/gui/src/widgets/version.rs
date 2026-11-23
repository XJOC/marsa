//! Version widgets (components.md): the large version picker and the compact
//! version menu. Both take the versions as a list of `(value, text)`, so they
//! work for any runtime: PHP "8.4", Node "22", a later Python "3.13" (R5).
//! They draw what they are given and return what the user chose; the screen
//! turns that into an action.

use eframe::egui::{
    Align2, Color32, CornerRadius, InnerResponse, Label, Painter, Popup, Pos2, Rect, Response,
    RichText, Role, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, Widget, WidgetInfo, vec2,
};

use super::icons::Icon;
use super::keys::roving_keys;
use super::menu;
use super::{
    Look, Touch, group_button, layout, paint_focus_ring, quiet_fill, slot, spot, tab_stop, touched,
    with_disabled_reason,
};
use crate::theme::{Direction, Face, TextToken, Theme, UNIT, VersionText};

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

/// The width of one of `columns` equal columns that fill `room` with `gap`
/// between them, in whole points, and at least `least`.
pub fn column_width(room: f32, gap: f32, columns: usize, least: f32) -> f32 {
    let columns = columns.max(1) as f32;
    ((room - gap * (columns - UNIT)) / columns)
        .floor()
        .max(least)
}

/// Where a button of a picker starts: its row, and how far from the start of
/// the row.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cell {
    row: usize,
    offset: f32,
}

/// The place of each button, in order. One row, unless `room` is the width a
/// row may have: a button that would end past it starts the next row (the
/// first button of a row always stays on it).
fn cells(widths: &[f32], gap: f32, room: Option<f32>) -> Vec<Cell> {
    let (mut row, mut offset) = (0, 0.0);
    widths
        .iter()
        .map(|width| {
            if offset > 0.0 && room.is_some_and(|room| offset + width > room) {
                row += 1;
                offset = 0.0;
            }
            let cell = Cell { row, offset };
            offset += width + gap;
            cell
        })
        .collect()
}

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
    /// Why the "+" cannot be pressed now (another install runs).
    install_off: Option<&'a str>,
    size: PickerSize,
    disabled: Option<&'a str>,
    /// A button's index and the look to draw it in.
    forced: Option<(usize, Look)>,
    /// `Some(n)`: equal columns, `n` to a row, as wide as the room gives
    /// them ([`column_width`]); more versions go on the next row.
    columns: Option<usize>,
}

impl<'a, T: Copy + PartialEq> VersionPicker<'a, T> {
    pub const fn new(label: &'a str, versions: &'a [(T, &'a str)], selected: Option<T>) -> Self {
        Self {
            label,
            versions,
            selected,
            install: None,
            install_off: None,
            size: PickerSize::Large,
            disabled: None,
            forced: None,
            columns: None,
        }
    }

    /// Adds the dashed "+" after the versions; `label` names it.
    pub const fn install(mut self, label: &'a str) -> Self {
        self.install = Some(label);
        self
    }

    /// Disables the dashed "+" alone, with `reason` over it: an install runs,
    /// and the versions still work.
    pub const fn install_disabled(mut self, reason: &'a str) -> Self {
        self.install_off = Some(reason);
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

    /// Equal columns that fill the width, `columns` to a row: the menu-bar
    /// panel (a fixed width) shows every installed version. A version past
    /// the last column goes on the next row, in a column of the same width,
    /// so a short last row does not look broken. It stays one group: the
    /// arrow keys move along the order of the buttons, row after row.
    pub const fn columns(mut self, columns: usize) -> Self {
        self.columns = Some(columns);
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
        let room = ui.available_width();
        let mut widths: Vec<f32> = galleys
            .iter()
            .map(|galley| {
                button
                    .x
                    .max(galley.size().x + theme.space.s3 + theme.space.s3)
            })
            .collect();
        if let Some(columns) = self.columns {
            // One width for all, that of a column; wider only for a text that
            // does not fit in it.
            let text = galleys
                .iter()
                .map(|galley| galley.size().x + theme.space.s3 + theme.space.s3)
                .fold(0.0, f32::max);
            let column = column_width(room, gap, columns, theme.size.version_menu_min).max(text);
            widths.iter_mut().for_each(|width| *width = column);
        }
        if self.install.is_some() {
            widths.push(self.size.install(theme).x);
        }
        let cells = cells(&widths, gap, self.columns.map(|_| room));
        let rows = cells.last().map_or(1, |cell| cell.row + 1);
        let across = cells
            .iter()
            .zip(&widths)
            .map(|(cell, width)| cell.offset + width)
            .fold(0.0, f32::max);
        let high = button.y * rows as f32 + gap * rows.saturating_sub(1) as f32;
        let (rect, group) = ui.allocate_exact_size(vec2(across, high), Sense::hover());
        let label = self.label;
        group.widget_info(|| WidgetInfo::labeled(Role::RadioGroup, enabled, label));

        let spots: Vec<Rect> = cells
            .iter()
            .zip(&widths)
            .map(|(cell, width)| {
                let x = slot(direction, rect, cell.offset, *width);
                let y = rect.top() + cell.row as f32 * (button.y + gap);
                spot(x, y, *width, button.y)
            })
            .collect();
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
                } else if self.install_off.is_some() {
                    // Not a Tab stop and not pressable; the pointer still
                    // finds it, for the reason over it.
                    ui.interact(*spot, group.id.with(index), Sense::hover())
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
            let off = self.install_off;
            if response.clicked() && off.is_none() {
                picked = Some(VersionPick::Install);
            }
            let works = enabled && off.is_none();
            response.widget_info(|| WidgetInfo::labeled(Role::Button, works, name));
            if let Some(reason) = off {
                response.clone().on_hover_text(reason);
            }
        }
        // The "+" is not one of the versions the arrows walk.
        roving_keys(ui, direction, &responses[..radios]);

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
                    None if self.install_off.is_some() => {
                        // Drawn at the opacity of a disabled control, as a
                        // disabled Ui would.
                        let mut painter = ui.painter().clone();
                        painter.multiply_opacity(theme.state.disabled_opacity);
                        Self::paint_install(&painter, theme, self.size, spots[index], Look::rest());
                    }
                    None => {
                        Self::paint_install(ui.painter(), theme, self.size, spots[index], look);
                    }
                }
            }
        }
        InnerResponse::new(picked, group)
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

    fn paint_install(painter: &Painter, theme: &Theme, size: PickerSize, spot: Rect, look: Look) {
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
            let area = spot(x, rect.top(), galley.size().x, height);
            let at = Align2::LEFT_CENTER.align_size_within_rect(galley.size(), area);
            painter.galley(at.min, galley, theme.color.ink);
            let x = slot(direction, rect, width - end - chevron, chevron);
            let area = spot(x, rect.top(), chevron, height);
            Icon::ChevronDown.paint(painter, theme, area, theme.color.mute);
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
mod tests;
