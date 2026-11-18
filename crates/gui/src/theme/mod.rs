//! The design tokens of `docs/design/tokens.md` as typed values. Every color,
//! size, radius, and shadow the window uses has a name here, and no other
//! module of the window writes one as a literal (rules.md 2). [Laravel: this
//! is the Tailwind config; the rest of the window uses its names.]
//!
//! [`Theme::light`] and [`Theme::dark`] differ in their colors and shadows
//! only. Both are built at compile time, so [`Theme::of`] hands out a
//! `&'static Theme` on every frame without allocating. [`style::apply`] turns
//! both into egui styles once, at start.

pub mod direction;
pub mod fonts;
pub mod page;
pub mod style;

pub use direction::Direction;

use eframe::egui::{self, Color32, FontId, Shadow, Vec2, vec2};

/// One look of the window, light or dark: every token of `tokens.md`.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    /// Light text on dark surfaces.
    pub dark: bool,
    pub color: Colors,
    /// The colors of each section; [`Theme::section`] picks one.
    pub sections: Sections,
    pub text: Type,
    pub space: Space,
    pub size: Size,
    pub radius: Radius,
    pub elevation: Elevation,
    pub focus: Focus,
    pub state: StateLayers,
}

/// `color.*`: the base and the state colors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colors {
    /// `color.bg`: the window background.
    pub bg: Color32,
    /// `color.side`: the sidebar and the title bar.
    pub side: Color32,
    /// `color.surface`: cards, tables, inputs.
    pub surface: Color32,
    /// `color.ink`: the main text.
    pub ink: Color32,
    /// `color.ink2`: secondary text.
    pub ink2: Color32,
    /// `color.mute`: hints and captions.
    pub mute: Color32,
    /// `color.line`: dividers and card borders (decorative, no contrast rule).
    pub line: Color32,
    /// `color.control`: borders of inputs and secondary buttons.
    pub control: Color32,
    /// `color.accent`: primary buttons, links, the selected tab, the focus ring.
    pub accent: Color32,
    /// `color.on_accent`: text on `accent`.
    pub on_accent: Color32,
    /// `color.selected`: the selected sidebar item and list row.
    pub selected: Color32,
    /// `color.scrim`: behind dialogs.
    pub scrim: Color32,
    /// `color.ok`: running, working.
    pub ok: Color32,
    /// `color.ok.tint`: the background of an ok icon.
    pub ok_tint: Color32,
    /// `color.error`: stopped, failed.
    pub error: Color32,
    /// `color.on_error`: text on an error button.
    pub on_error: Color32,
    /// `color.error.tint`: the background of an error icon.
    pub error_tint: Color32,
    /// `color.error.bg`: an error banner, an error log row.
    pub error_bg: Color32,
    /// `color.error.line`: the border of an error banner.
    pub error_line: Color32,
    /// `color.off`: a switch track when off.
    pub off: Color32,
    /// `color.knob.off`: the knob of a switch that is off. White in both
    /// themes, as in the mockups; the knob of a switch that is on is
    /// `on_accent`.
    pub knob_off: Color32,
}

/// A section of the window. Each has its own colors and icon shape, so
/// color is never the only clue (R8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Home,
    Sites,
    Runtimes,
    Services,
    Mail,
    Dumps,
    Logs,
    Databases,
    Queues,
}

impl Section {
    pub const ALL: [Self; 9] = [
        Self::Home,
        Self::Sites,
        Self::Runtimes,
        Self::Services,
        Self::Mail,
        Self::Dumps,
        Self::Logs,
        Self::Databases,
        Self::Queues,
    ];
}

/// `section.*`: a section's foreground (icon, count, badge text) and tint
/// (icon tile, badge background).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionColors {
    pub fg: Color32,
    pub tint: Color32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sections {
    pub home: SectionColors,
    pub sites: SectionColors,
    pub runtimes: SectionColors,
    pub services: SectionColors,
    pub mail: SectionColors,
    /// Also the warning color (tokens.md).
    pub dumps: SectionColors,
    pub logs: SectionColors,
    pub databases: SectionColors,
    pub queues: SectionColors,
}

/// A face of the bundled fonts ([`fonts`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    /// Readex Pro 300.
    Light,
    /// Readex Pro 400.
    Regular,
    /// Readex Pro 500.
    Medium,
    /// Readex Pro 600.
    SemiBold,
    /// JetBrains Mono Regular.
    Mono,
}

/// `type.*`: a text size and the face it is drawn in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextToken {
    pub size: f32,
    pub face: Face,
}

impl TextToken {
    pub fn font_id(self) -> FontId {
        FontId::new(self.size, fonts::family(self.face))
    }
}

/// `type.version.*`: a version button's text, in `selected` when chosen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VersionText {
    pub size: f32,
    pub face: Face,
    pub selected: Face,
}

/// `type.*`. No text is below 12 (R10).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Type {
    /// Hints, reasons ("from .php-version"), table headers, timestamps.
    pub caption: TextToken,
    /// `type.body.sm`: secondary text, buttons, inputs.
    pub body_sm: TextToken,
    /// Default text, list rows.
    pub body: TextToken,
    /// Site names, row titles.
    pub body_strong: TextToken,
    /// Section headings inside a screen.
    pub heading: TextToken,
    /// Screen titles, the Home status sentence.
    pub title: TextToken,
    /// A site's name on its page.
    pub display: TextToken,
    /// Large version buttons.
    pub version_lg: VersionText,
    /// Version buttons of the menu-bar panel.
    pub version_md: VersionText,
    /// `type.mono` at 13: paths, log lines, SQL values.
    pub mono: TextToken,
    /// `type.mono` at 12, next to captions.
    pub mono_sm: TextToken,
    /// Line height of body text, as a multiple of its size.
    pub line_height_body: f32,
    /// Line height of titles, as a multiple of their size.
    pub line_height_title: f32,
}

/// `space.*`, on a base of 4.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Space {
    /// `space.1`.
    pub s1: f32,
    /// `space.2`.
    pub s2: f32,
    /// `space.3`.
    pub s3: f32,
    /// `space.4`.
    pub s4: f32,
    /// `space.5`.
    pub s5: f32,
    /// `space.6`.
    pub s6: f32,
    /// `space.8`.
    pub s8: f32,
    /// Screen padding at the top.
    pub screen_top: f32,
    /// Screen padding at the start and the end.
    pub screen_side: f32,
    /// Between the sections of a screen.
    pub section_gap: f32,
    /// Between the items of a group: 8 to 10.
    pub group_gap: f32,
    pub group_gap_max: f32,
}

/// `size.*`, and the window's own sizes (`layout.md`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    /// Buttons, inputs, segmented controls.
    pub control: f32,
    /// A control inside a table row.
    pub control_in_row: f32,
    /// `size.row`: list and table rows with two lines, 48 to 54.
    pub row: f32,
    pub row_max: f32,
    /// `size.row.dense`: data grid rows.
    pub row_dense: f32,
    /// Log lines.
    pub log_row: f32,
    /// `size.nav.item`: a sidebar item.
    pub nav_item: f32,
    /// The icon tile of a sidebar item.
    pub nav_tile: f32,
    /// `size.version.lg`: a large version button.
    pub version_lg: Vec2,
    /// `size.version.md`: the height of a menu-bar panel version button.
    pub version_md: f32,
    /// `size.sidebar`, open.
    pub sidebar: f32,
    /// `size.sidebar`, folded to icons.
    pub sidebar_folded: f32,
    /// `size.target.min`: the smallest click target (R9).
    pub target_min: f32,
    /// The 1 px border of cards, inputs, and buttons (components.md).
    pub border: f32,
    /// An icon button: 30 by 30 (components.md, Buttons).
    pub icon_button: f32,
    /// The icon tile of a screen header: 32 by 32 (components.md, Section
    /// icon tile). The sidebar's is [`Size::nav_tile`].
    pub tile_header: f32,
    /// The icon inside a section tile: 15.
    pub icon_tile: f32,
    /// An icon next to text in a button, a field, or a message: 14.
    pub icon_inline: f32,
    /// The icon in a row badge: 12.
    pub icon_badge: f32,
    /// The stroke of an icon, as a share of its size: 1.5 on the 16 unit
    /// grid of the section icons.
    pub icon_stroke: f32,
    /// The stroke of a status icon (check, cross), as a share of its size:
    /// 1.9 on the 12 unit grid.
    pub icon_stroke_bold: f32,
    /// A switch track: 36 by 22 (components.md, Switch).
    pub switch: Vec2,
    /// The knob of a switch: 16.
    pub switch_knob: f32,
    /// The space between a switch's track and its knob: 3.
    pub switch_inset: f32,
    /// The height of a row badge: 26 (components.md, Badges).
    pub badge: f32,
    /// The dot on a folded sidebar item's tile: 8.
    pub dot: f32,
    /// The ring around that dot, in the sidebar's color: 2.
    pub dot_ring: f32,
    /// The space between a segmented control's track and its options: 2.
    pub segmented_inset: f32,
    /// The window when it opens.
    pub window: Vec2,
    /// The smallest the window gets: the sidebar and a table must fit.
    pub window_min: Vec2,
}

/// `radius.*`, in egui's whole points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Radius {
    /// Badges, small chips.
    pub sm: u8,
    /// Controls, sidebar items.
    pub md: u8,
    /// `radius.md` "7 for 26": the corner of the sidebar's 26 by 26 tile.
    pub tile_nav: u8,
    /// Cards, tables, version buttons.
    pub lg: u8,
    /// Dialogs, the menu-bar panel.
    pub xl: u8,
    /// Pills, switches, status dots: egui caps a radius at half the shape.
    pub full: u8,
}

/// `elevation.*`. `elevation.0` is no shadow: everything in the window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Elevation {
    /// `elevation.1`: menus, popovers.
    pub menu: Shadow,
    /// `elevation.2`: dialogs, the menu-bar panel.
    pub dialog: Shadow,
}

/// The focus ring: `color.accent`, outside the control. Never removed (R9).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Focus {
    pub width: f32,
    /// The gap between the control and the ring.
    pub offset: f32,
}

/// How a control's fill changes with its state (components.md).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StateLayers {
    /// Hover: this much toward `ink` (darker in light, lighter in dark).
    pub hover: f32,
    /// Pressed: this much toward `ink`.
    pub pressed: f32,
    /// A disabled control is drawn at this opacity.
    pub disabled_opacity: f32,
    /// The pointer over a row, or over a control with no fill: `selected` at
    /// this opacity (components.md, Tables and lists).
    pub row_hover: f32,
}

/// The light theme, built at compile time.
pub static LIGHT: Theme = Theme::light();
/// The dark theme, built at compile time.
pub static DARK: Theme = Theme::dark();

/// The type scale: the same in both themes.
pub const TYPE: Type = Type {
    caption: text(12.0, Face::Regular),
    body_sm: text(13.0, Face::Regular),
    body: text(14.0, Face::Regular),
    body_strong: text(14.0, Face::Medium),
    heading: text(15.0, Face::Medium),
    title: text(24.0, Face::Medium),
    display: text(30.0, Face::Medium),
    version_lg: VersionText {
        size: 26.0,
        face: Face::Light,
        selected: Face::Medium,
    },
    version_md: VersionText {
        size: 18.0,
        face: Face::Light,
        selected: Face::Medium,
    },
    mono: text(13.0, Face::Mono),
    mono_sm: text(12.0, Face::Mono),
    line_height_body: 1.4,
    line_height_title: 1.2,
};

/// The spacing: the same in both themes.
pub const SPACE: Space = Space {
    s1: 4.0,
    s2: 8.0,
    s3: 12.0,
    s4: 16.0,
    s5: 20.0,
    s6: 24.0,
    s8: 32.0,
    screen_top: 26.0,
    screen_side: 32.0,
    section_gap: 20.0,
    group_gap: 8.0,
    group_gap_max: 10.0,
};

/// The sizes: the same in both themes, so `main.rs` reads the window's
/// sizes here before egui knows the system's theme.
pub const SIZE: Size = Size {
    control: 32.0,
    control_in_row: 30.0,
    row: 48.0,
    row_max: 54.0,
    row_dense: 32.0,
    log_row: 26.0,
    nav_item: 38.0,
    nav_tile: 26.0,
    version_lg: vec2(92.0, 64.0),
    version_md: 44.0,
    sidebar: 224.0,
    sidebar_folded: 64.0,
    target_min: 24.0,
    border: 1.0,
    icon_button: 30.0,
    tile_header: 32.0,
    icon_tile: 15.0,
    icon_inline: 14.0,
    icon_badge: 12.0,
    icon_stroke: 1.5 / 16.0,
    icon_stroke_bold: 1.9 / 12.0,
    switch: vec2(36.0, 22.0),
    switch_knob: 16.0,
    switch_inset: 3.0,
    badge: 26.0,
    dot: 8.0,
    dot_ring: 2.0,
    segmented_inset: 2.0,
    window: vec2(1120.0, 720.0),
    window_min: vec2(760.0, 520.0),
};

/// The radii: the same in both themes.
pub const RADIUS: Radius = Radius {
    sm: 6,
    md: 8,
    tile_nav: 7,
    lg: 12,
    xl: 14,
    full: u8::MAX,
};

/// The focus ring: the same in both themes.
pub const FOCUS: Focus = Focus {
    width: 2.0,
    offset: 2.0,
};

/// The state layers of controls: the same in both themes.
pub const STATE: StateLayers = StateLayers {
    hover: 0.06,
    pressed: 0.12,
    disabled_opacity: 0.55,
    row_hover: 0.5,
};

impl Theme {
    pub const fn light() -> Self {
        Self {
            dark: false,
            color: Colors {
                bg: hex(0xF3F5F4),
                side: hex(0xEAEEED),
                surface: hex(0xFFFFFF),
                ink: hex(0x16232E),
                ink2: hex(0x2E3C47),
                mute: hex(0x56636E),
                line: hex(0xDCE2E2),
                control: hex(0x7A8790),
                accent: hex(0x1F5F8B),
                on_accent: hex(0xFFFFFF),
                selected: hex(0xDCE5EA),
                scrim: rgba(22, 35, 46, 0.35),
                ok: hex(0x1E7A4A),
                ok_tint: hex(0xDDF0E5),
                error: hex(0xB4232A),
                on_error: hex(0xFFFFFF),
                error_tint: hex(0xF8DCDD),
                error_bg: hex(0xFBECEC),
                error_line: hex(0xEBC3C4),
                off: hex(0xB4BEC6),
                knob_off: hex(0xFFFFFF),
            },
            sections: Sections {
                home: section(0x2E3C47, 0xE1E6EA),
                sites: section(0x1F5F8B, 0xDCE8F1),
                runtimes: section(0x5B4BB7, 0xE7E3F7),
                services: section(0x1B7045, 0xDDF0E5),
                mail: section(0xB42E5A, 0xF8E1E9),
                dumps: section(0x8A5A00, 0xF8ECCF),
                logs: section(0x4A5A68, 0xE3E8EC),
                databases: section(0x0A6878, 0xD7EFF2),
                queues: section(0xA4480F, 0xF9E4D6),
            },
            elevation: Elevation {
                menu: shadow(8, 24, rgba(20, 35, 50, 0.14)),
                dialog: shadow(18, 40, rgba(20, 35, 50, 0.16)),
            },
            text: TYPE,
            space: SPACE,
            size: SIZE,
            radius: RADIUS,
            focus: FOCUS,
            state: STATE,
        }
    }

    pub const fn dark() -> Self {
        Self {
            dark: true,
            color: Colors {
                bg: hex(0x131B22),
                side: hex(0x0E151B),
                surface: hex(0x18222B),
                ink: hex(0xE3E9EC),
                ink2: hex(0xC3CDD3),
                mute: hex(0x97A4AE),
                line: hex(0x25313B),
                control: hex(0x66757F),
                accent: hex(0x7AB8E0),
                on_accent: hex(0x0E1B26),
                selected: hex(0x1B2A36),
                scrim: rgba(5, 10, 14, 0.60),
                ok: hex(0x5CC690),
                ok_tint: hex(0x163A2A),
                error: hex(0xF0786F),
                on_error: hex(0x2B0806),
                error_tint: hex(0x3A1A1A),
                error_bg: hex(0x2A1717),
                error_line: hex(0x5A2A28),
                off: hex(0x4A545E),
                knob_off: hex(0xFFFFFF),
            },
            sections: Sections {
                home: section(0xC3CDD3, 0x25313B),
                sites: section(0x8CC4EA, 0x1B3245),
                runtimes: section(0xB3A8F5, 0x2A2550),
                services: section(0x6FD1A0, 0x163A2A),
                mail: section(0xF39AB8, 0x43202D),
                dumps: section(0xF0C565, 0x3D3015),
                logs: section(0xA9B8C4, 0x26323C),
                databases: section(0x6FD0DF, 0x123A40),
                queues: section(0xF2A57A, 0x40261A),
            },
            elevation: Elevation {
                menu: shadow(8, 24, rgba(0, 0, 0, 0.40)),
                dialog: shadow(18, 40, rgba(0, 0, 0, 0.45)),
            },
            text: TYPE,
            space: SPACE,
            size: SIZE,
            radius: RADIUS,
            focus: FOCUS,
            state: STATE,
        }
    }

    /// The theme egui draws with now: the system's, or the one the user
    /// chose.
    pub fn of(ctx: &egui::Context) -> &'static Self {
        match ctx.theme() {
            egui::Theme::Light => &LIGHT,
            egui::Theme::Dark => &DARK,
        }
    }

    /// `color` moved `amount` toward `ink`: darker in the light theme,
    /// lighter in the dark one (components.md: hover and pressed).
    pub fn shade(&self, color: Color32, amount: f32) -> Color32 {
        color.lerp_to_gamma(self.color.ink, amount)
    }

    pub const fn section(&self, section: Section) -> SectionColors {
        let all = &self.sections;
        match section {
            Section::Home => all.home,
            Section::Sites => all.sites,
            Section::Runtimes => all.runtimes,
            Section::Services => all.services,
            Section::Mail => all.mail,
            Section::Dumps => all.dumps,
            Section::Logs => all.logs,
            Section::Databases => all.databases,
            Section::Queues => all.queues,
        }
    }
}

/// A color written as in `tokens.md`: `hex(0x1F5F8B)` is `#1F5F8B`.
const fn hex(rgb: u32) -> Color32 {
    let [_, r, g, b] = rgb.to_be_bytes();
    Color32::from_rgb(r, g, b)
}

/// `rgba(r, g, b, alpha)` as in `tokens.md` and CSS.
const fn rgba(r: u8, g: u8, b: u8, alpha: f32) -> Color32 {
    // The nearest 8-bit alpha; `alpha` is between 0 and 1.
    Color32::from_rgba_unmultiplied_const(r, g, b, (alpha * 255.0 + 0.5) as u8)
}

const fn section(fg: u32, tint: u32) -> SectionColors {
    SectionColors {
        fg: hex(fg),
        tint: hex(tint),
    }
}

/// A shadow straight down by `y`, blurred by `blur`.
const fn shadow(y: i8, blur: u8, color: Color32) -> Shadow {
    Shadow {
        offset: [0, y],
        blur,
        spread: 0,
        color,
    }
}

const fn text(size: f32, face: Face) -> TextToken {
    TextToken { size, face }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::Path;

    use super::*;

    /// WCAG 2.2 text contrast: normal text, and the minimum the design asks
    /// for everything that carries words (R9).
    pub(crate) const TEXT: f64 = 4.5;
    /// WCAG 2.2 non-text contrast: borders of controls, icons, the focus
    /// ring.
    pub(crate) const UI: f64 = 3.0;

    /// The WCAG 2.2 relative luminance of an sRGB color.
    fn luminance(color: Color32) -> f64 {
        let channel = |value: u8| {
            let c = f64::from(value) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
    }

    /// The WCAG 2.2 contrast ratio, from 1 to 21.
    pub(crate) fn contrast(a: Color32, b: Color32) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// Checks every pair and lists all the failures at once.
    pub(crate) fn check(theme: &str, pairs: &[(&str, Color32, Color32, f64)]) {
        let failures: Vec<String> = pairs
            .iter()
            .filter(|(_, fg, bg, min)| contrast(*fg, *bg) < *min)
            .map(|(name, fg, bg, min)| {
                format!(
                    "{theme}: {name} is {:.2}:1, under {min}",
                    contrast(*fg, *bg)
                )
            })
            .collect();
        assert!(failures.is_empty(), "{failures:#?}");
    }

    fn themes() -> [(&'static str, &'static Theme); 2] {
        [("light", &LIGHT), ("dark", &DARK)]
    }

    #[test]
    fn the_contrast_formula_matches_wcag() {
        assert!((contrast(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 1e-9);
        assert!((contrast(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 1e-9);
        // The figures tokens.md gives for `ink` on `surface`.
        assert!((contrast(LIGHT.color.ink, LIGHT.color.surface) - 15.98).abs() < 0.01);
        assert!((contrast(DARK.color.ink, DARK.color.surface) - 13.15).abs() < 0.01);
    }

    #[test]
    fn every_text_color_reads_on_its_backgrounds_in_both_themes() {
        for (name, theme) in themes() {
            let c = &theme.color;
            let mut pairs = Vec::new();
            for (bg_name, bg) in [
                ("bg", c.bg),
                ("side", c.side),
                ("surface", c.surface),
                ("selected", c.selected),
            ] {
                for (fg_name, fg) in [
                    ("ink", c.ink),
                    ("ink2", c.ink2),
                    ("mute", c.mute),
                    ("accent", c.accent),
                    ("error", c.error),
                ] {
                    pairs.push((format!("{fg_name} on {bg_name}"), fg, bg, TEXT));
                }
            }
            for (bg_name, bg) in [("bg", c.bg), ("side", c.side), ("surface", c.surface)] {
                pairs.push((format!("ok on {bg_name}"), c.ok, bg, TEXT));
                let warning = theme.sections.dumps.fg;
                pairs.push((format!("warning on {bg_name}"), warning, bg, TEXT));
            }
            // A section color is text (a count, a badge) on every surface it
            // sits on: the sidebar, its selected item, the page, and its tint.
            for (bg_name, bg) in [
                ("bg", c.bg),
                ("side", c.side),
                ("surface", c.surface),
                ("selected", c.selected),
            ] {
                for section in Section::ALL {
                    let fg = theme.section(section).fg;
                    pairs.push((format!("{section:?} on {bg_name}"), fg, bg, TEXT));
                }
            }
            pairs.extend([
                ("on_accent on accent".into(), c.on_accent, c.accent, TEXT),
                ("on_error on error".into(), c.on_error, c.error, TEXT),
                ("ink on error.bg".into(), c.ink, c.error_bg, TEXT),
                ("error on error.bg".into(), c.error, c.error_bg, TEXT),
            ]);
            for section in Section::ALL {
                let colors = theme.section(section);
                pairs.push((
                    format!("{section:?} on its tint"),
                    colors.fg,
                    colors.tint,
                    TEXT,
                ));
            }
            let pairs: Vec<_> = pairs
                .iter()
                .map(|(pair, fg, bg, min)| (pair.as_str(), *fg, *bg, *min))
                .collect();
            check(name, &pairs);
        }
    }

    #[test]
    fn control_borders_icons_and_the_focus_ring_stand_out_in_both_themes() {
        for (name, theme) in themes() {
            let c = &theme.color;
            check(
                name,
                &[
                    ("control on bg", c.control, c.bg, UI),
                    ("control on side", c.control, c.side, UI),
                    ("control on surface", c.control, c.surface, UI),
                    ("focus ring on bg", c.accent, c.bg, UI),
                    ("focus ring on surface", c.accent, c.surface, UI),
                    ("ok icon on its tint", c.ok, c.ok_tint, UI),
                    ("error icon on its tint", c.error, c.error_tint, UI),
                ],
            );
        }
    }

    #[test]
    fn the_two_themes_share_every_token_but_colors_and_shadows() {
        assert_eq!(LIGHT.text, DARK.text);
        assert_eq!(LIGHT.space, DARK.space);
        assert_eq!(LIGHT.size, DARK.size);
        assert_eq!(LIGHT.radius, DARK.radius);
        assert_eq!(LIGHT.focus, DARK.focus);
        assert_eq!(LIGHT.state, DARK.state);
        assert!(!LIGHT.dark && DARK.dark);
        assert_ne!(LIGHT.color, DARK.color);
    }

    #[test]
    fn hex_and_rgba_read_like_tokens_md() {
        assert_eq!(hex(0x1F5F8B), Color32::from_rgb(0x1F, 0x5F, 0x8B));
        // rgba(22,35,46,0.35): alpha 0.35 is 89 of 255.
        assert_eq!(LIGHT.color.scrim.a(), 89);
        assert_eq!(DARK.color.scrim.a(), 153);
        assert_eq!(LIGHT.elevation.dialog.color.a(), 41);
    }

    /// How strict the literal check is with a file.
    #[derive(Clone, Copy, PartialEq)]
    enum Rules {
        /// `app.rs` and `main.rs`: no color word at all, and no float other
        /// than zero: egui sizes are `f32`.
        Drawing,
        /// `strings.rs`: no color word and no `f32`; its other floats are
        /// unit conversions.
        Strings,
        /// The widgets: they name the color type, so only a color made by
        /// hand or a named color is wrong; no float other than zero; no
        /// corner or margin written as a number. The tests at the end of a
        /// file are not checked.
        Widget,
        /// `widgets/icons.rs`: the coordinates of its icons are drawing
        /// data on the icon's own grid, so floats are allowed; the rest is
        /// as for a widget.
        Geometry,
    }

    /// Rules.md 2: no color or size literal outside `theme`. Comments and
    /// strings, such as a version "8.4" in a text, are skipped.
    #[test]
    fn the_drawing_code_has_no_color_or_size_literal() {
        const COLOR_WORDS: [&str; 5] = ["Color32", "Rgba", "from_rgb", "from_gray", "hex_color"];
        let mut files = vec![
            (
                "app.rs".to_owned(),
                include_str!("../app.rs").to_owned(),
                Rules::Drawing,
            ),
            (
                "main.rs".to_owned(),
                include_str!("../main.rs").to_owned(),
                Rules::Drawing,
            ),
            (
                "strings.rs".to_owned(),
                include_str!("../strings.rs").to_owned(),
                Rules::Strings,
            ),
        ];
        // Every file of `widgets/`, so a new widget is checked at once.
        let widgets = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/widgets");
        let mut names: Vec<_> = std::fs::read_dir(&widgets)
            .expect("the widgets folder")
            .map(|entry| entry.expect("a directory entry").file_name())
            .collect();
        names.sort();
        assert!(
            names.len() >= 9,
            "the widgets folder is missing files: {names:?}"
        );
        for name in names {
            let name = name.to_string_lossy().into_owned();
            let source = std::fs::read_to_string(widgets.join(&name)).expect("a widget file");
            let rules = if name == "icons.rs" {
                Rules::Geometry
            } else {
                Rules::Widget
            };
            files.push((
                format!("widgets/{name}"),
                before_tests(&source).to_owned(),
                rules,
            ));
        }
        let mut found = Vec::new();
        for (file, source, rules) in &files {
            for (index, code) in code_only(source).lines().enumerate() {
                let line = index + 1;
                match rules {
                    Rules::Drawing | Rules::Strings => {
                        for word in COLOR_WORDS {
                            if code.contains(word) {
                                found.push(format!("{file}:{line}: {word}"));
                            }
                        }
                    }
                    Rules::Widget | Rules::Geometry => {
                        for word in hand_made_colors(code) {
                            found.push(format!("{file}:{line}: {word}"));
                        }
                        for number in numbered_corners(code) {
                            found.push(format!("{file}:{line}: {number}"));
                        }
                    }
                }
                match rules {
                    Rules::Drawing | Rules::Widget => {
                        for literal in float_literals(code) {
                            found.push(format!("{file}:{line}: {literal}"));
                        }
                    }
                    Rules::Strings if code.contains("f32") => {
                        found.push(format!("{file}:{line}: f32"));
                    }
                    Rules::Strings | Rules::Geometry => {}
                }
            }
        }
        assert!(found.is_empty(), "literals outside theme: {found:#?}");
    }

    /// `source` up to its `#[cfg(test)]` module.
    fn before_tests(source: &str) -> &str {
        source
            .find("\n#[cfg(test)]")
            .map_or(source, |end| &source[..end])
    }

    /// A color made by hand (`from_rgb`, `hex_color`) or named
    /// (`Color32::WHITE`) in `code`. `PLACEHOLDER` (text with no color yet)
    /// and `TRANSPARENT` are not design colors.
    fn hand_made_colors(code: &str) -> Vec<String> {
        const MADE: [&str; 8] = [
            "from_rgb",
            "from_rgba",
            "from_gray",
            "from_black_alpha",
            "from_white_alpha",
            "hex_color",
            "Rgba",
            "Hsva",
        ];
        let mut found: Vec<String> = MADE
            .iter()
            .filter(|word| code.contains(*word))
            .map(|word| (*word).to_owned())
            .collect();
        let mut rest = code;
        while let Some(at) = rest.find("Color32::") {
            rest = &rest[at + "Color32::".len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let constant = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit());
            if constant && name != "PLACEHOLDER" && name != "TRANSPARENT" {
                found.push(format!("Color32::{name}"));
            }
        }
        found
    }

    /// A corner or margin written as a number: `CornerRadius::same(7)`.
    fn numbered_corners(code: &str) -> Vec<String> {
        const MAKERS: [&str; 4] = [
            "CornerRadius::same(",
            "Margin::same(",
            "Margin::symmetric(",
            "CornerRadius::from(",
        ];
        let mut found = Vec::new();
        for maker in MAKERS {
            let mut rest = code;
            while let Some(at) = rest.find(maker) {
                rest = &rest[at + maker.len()..];
                if rest.starts_with(|c: char| c.is_ascii_digit()) {
                    found.push(format!("{maker}{}", &rest[..1]));
                }
            }
        }
        found
    }

    /// `source` with its comments and string literals blanked out, line
    /// breaks kept. Enough for this repository's code: no raw strings with
    /// quotes inside, and `'"'` is the only char literal that matters.
    fn code_only(source: &str) -> String {
        #[derive(PartialEq)]
        enum State {
            Code,
            Text,
            LineComment,
            BlockComment,
        }
        let mut state = State::Code;
        let mut out = String::with_capacity(source.len());
        let mut chars = source.chars().peekable();
        while let Some(c) = chars.next() {
            let next = chars.peek().copied();
            match state {
                State::Code if c == '/' && next == Some('/') => state = State::LineComment,
                State::Code if c == '/' && next == Some('*') => state = State::BlockComment,
                State::Code if c == '"' => state = State::Text,
                State::Code if c == '\'' && next == Some('"') => {
                    // The char literal '"': keep the quote out of `Text`.
                    chars.next();
                }
                State::Code => {
                    out.push(c);
                    continue;
                }
                State::Text if c == '\\' => {
                    // An escape. A line break after it continues the text.
                    if chars.next() == Some('\n') {
                        out.push('\n');
                    }
                }
                State::Text if c == '"' => state = State::Code,
                State::LineComment if c == '\n' => state = State::Code,
                State::BlockComment if c == '*' && next == Some('/') => {
                    chars.next();
                    state = State::Code;
                }
                _ => {}
            }
            out.push(if c == '\n' { '\n' } else { ' ' });
        }
        out
    }

    /// The float literals in `code` that are not zero, such as `170.0`.
    fn float_literals(code: &str) -> Vec<String> {
        let chars: Vec<char> = code.chars().collect();
        let mut found = Vec::new();
        let mut index = 0;
        while index < chars.len() {
            let starts_number = chars[index].is_ascii_digit()
                && (index == 0
                    || !(chars[index - 1].is_alphanumeric()
                        || chars[index - 1] == '_'
                        || chars[index - 1] == '.'));
            if !starts_number {
                index += 1;
                continue;
            }
            let start = index;
            while index < chars.len() && (chars[index].is_ascii_digit() || chars[index] == '_') {
                index += 1;
            }
            let is_float =
                index + 1 < chars.len() && chars[index] == '.' && chars[index + 1].is_ascii_digit();
            if is_float {
                index += 1;
                while index < chars.len() && chars[index].is_ascii_digit() {
                    index += 1;
                }
                let literal: String = chars[start..index].iter().collect();
                if literal.replace('_', "").parse::<f64>() != Ok(0.0) {
                    found.push(literal);
                }
            }
        }
        found
    }

    #[test]
    fn the_literal_check_skips_comments_and_strings() {
        let source = "a(1.5) // 2.5\nb(\"8.4 \\\" 9.1\") /* 3.5 */ c(4.5)\nd('\"', 5.5)";
        let found: Vec<String> = code_only(source).lines().flat_map(float_literals).collect();
        assert_eq!(found, ["1.5", "4.5", "5.5"]);
        assert_eq!(code_only("x\n\"a \\\n b\"\ny").lines().count(), 4);
    }

    #[test]
    fn the_widget_check_finds_made_colors_and_numbered_corners_and_skips_the_rest() {
        assert_eq!(hand_made_colors("Color32::from_rgb(1, 2, 3)"), ["from_rgb"]);
        assert_eq!(hand_made_colors("fill: Color32::WHITE"), ["Color32::WHITE"]);
        assert!(hand_made_colors("fn f(color: Color32) -> Color32 { color }").is_empty());
        assert!(hand_made_colors("layout(p, t, k, Color32::PLACEHOLDER)").is_empty());
        assert!(hand_made_colors("fill: Color32::TRANSPARENT").is_empty());
        assert_eq!(numbered_corners("CornerRadius::same(7)").len(), 1);
        assert_eq!(numbered_corners("Margin::symmetric(8, 9)").len(), 1);
        assert!(numbered_corners("CornerRadius::same(theme.radius.md)").is_empty());
        assert!(numbered_corners("Margin::symmetric(whole(a), whole(b))").is_empty());
        let source = "fn a() {}\n#[cfg(test)]\nmod tests { fn b() { 1.5 } }";
        assert_eq!(before_tests(source), "fn a() {}");
        assert_eq!(before_tests("no tests"), "no tests");
    }

    #[test]
    fn the_literal_check_finds_sizes_and_skips_the_rest() {
        assert_eq!(float_literals("Column::auto().at_least(80.0)"), ["80.0"]);
        assert_eq!(float_literals("const W: f32 = 1_120.5;"), ["1_120.5"]);
        assert!(float_literals("min_scrolled_height(0.0)").is_empty());
        assert!(float_literals("rows[1].0 + v1.2 + 3").is_empty());
    }
}
