//! The section icon tile and the runtime monogram tile (components.md,
//! Section icon tile). A section's tile is its tint with its icon in its
//! color; a runtime's tile is two or three letters in the Runtimes colors, so
//! a new language needs a monogram and no new icon (rules.md 9).

use super::badge::paint_dot;
use super::icons::Icon;
use super::layout;
use crate::strings;
use crate::theme::{Face, Section, SectionColors, TextToken, Theme};
use eframe::egui::{
    Align, Align2, CornerRadius, Painter, Rect, Response, Role, Sense, Ui, Vec2, Widget,
    WidgetInfo, pos2,
};

/// The two sizes of a tile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TileSize {
    /// 26 by 26, in the sidebar.
    #[default]
    Nav,
    /// 32 by 32, in a screen header and in the runtimes list.
    Header,
}

impl TileSize {
    pub const fn side(self, theme: &Theme) -> f32 {
        match self {
            Self::Nav => theme.size.nav_tile,
            Self::Header => theme.size.tile_header,
        }
    }

    /// `radius.md`, 7 for the 26 tile (components.md).
    pub const fn radius(self, theme: &Theme) -> u8 {
        match self {
            Self::Nav => theme.radius.tile_nav,
            Self::Header => theme.radius.md,
        }
    }
}

/// The colors of a section's tile: its tint behind, its color on top.
pub const fn section_colors(theme: &Theme, section: Section) -> SectionColors {
    theme.section(section)
}

/// What a screen reader says for a tile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Access<'a> {
    /// The section's name, or the monogram's letters.
    #[default]
    Named,
    /// Something else names it, so a screen reader skips the tile.
    Decorative,
    Label(&'a str),
}

fn tile_info(access: Access, name: &str) -> WidgetInfo {
    match access {
        Access::Named => WidgetInfo::labeled(Role::Image, true, name),
        Access::Label(label) => WidgetInfo::labeled(Role::Image, true, label),
        Access::Decorative => WidgetInfo::new(Role::GenericContainer),
    }
}

/// The tile of a section: its tint, with its icon in its color. In the
/// folded sidebar a dot in the section's color says that something is new.
#[derive(Clone, Copy, Debug)]
#[must_use = "a tile is drawn only when it is added to a Ui"]
pub struct SectionTile<'a> {
    section: Section,
    size: TileSize,
    dot: bool,
    access: Access<'a>,
}

impl<'a> SectionTile<'a> {
    pub const fn new(section: Section) -> Self {
        Self {
            section,
            size: TileSize::Nav,
            dot: false,
            access: Access::Named,
        }
    }

    pub const fn size(mut self, size: TileSize) -> Self {
        self.size = size;
        self
    }

    /// The dot of a folded sidebar item with something new.
    pub const fn dot(mut self, dot: bool) -> Self {
        self.dot = dot;
        self
    }

    /// The text next to the tile names it already: a screen reader skips it.
    pub const fn decorative(mut self) -> Self {
        self.access = Access::Decorative;
        self
    }

    /// What a screen reader says, instead of the section's name.
    pub const fn label(mut self, label: &'a str) -> Self {
        self.access = Access::Label(label);
        self
    }
}

impl Widget for SectionTile<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let side = self.size.side(theme);
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
        if ui.is_rect_visible(rect) {
            paint_section(ui.painter(), theme, rect, self.section, self.size, self.dot);
        }
        let (access, name) = (self.access, strings::section_name(self.section));
        response.widget_info(|| tile_info(access, name));
        response
    }
}

/// Paints the tile of `section` in `rect`, with the dot of something new
/// when `dot` is set. The sidebar's items paint their tiles with it too.
pub fn paint_section(
    painter: &Painter,
    theme: &Theme,
    rect: Rect,
    section: Section,
    size: TileSize,
    dot: bool,
) {
    let colors = section_colors(theme, section);
    painter.rect_filled(rect, CornerRadius::same(size.radius(theme)), colors.tint);
    let icon = Rect::from_center_size(rect.center(), Vec2::splat(theme.size.icon_tile));
    Icon::of(section).paint(painter, theme, icon, colors.fg);
    if dot {
        let corner = Rect::from_min_size(
            pos2(rect.right() - theme.size.dot, rect.top()),
            Vec2::splat(theme.size.dot),
        );
        // The ring is the sidebar's color, so the dot stands off the tile.
        paint_dot(painter, theme, corner.center(), colors.fg, theme.color.side);
    }
}

/// Paints the monogram `text` in `rect`: the tint of `section` behind, the
/// letters in its color on top. The runtimes list paints its entries with it.
pub fn paint_monogram(
    painter: &Painter,
    theme: &Theme,
    rect: Rect,
    text: &str,
    size: TileSize,
    section: Section,
) {
    let colors = section_colors(theme, section);
    painter.rect_filled(rect, CornerRadius::same(size.radius(theme)), colors.tint);
    let galley = layout(painter, text, Monogram::token(theme), colors.fg);
    let at = Align2([Align::Center, Align::Center]).align_size_within_rect(galley.size(), rect);
    painter.galley(at.min, galley, colors.fg);
}

/// The tile of a runtime: its letters in the Runtimes colors, or in the
/// colors of another section.
#[derive(Clone, Copy, Debug)]
#[must_use = "a tile is drawn only when it is added to a Ui"]
pub struct Monogram<'a> {
    text: &'a str,
    size: TileSize,
    section: Section,
    access: Access<'a>,
}

impl<'a> Monogram<'a> {
    /// A tile with these letters ("PHP", "JS", "Py"). Three letters need the
    /// header size; the nav tile is for two.
    pub const fn new(text: &'a str) -> Self {
        Self {
            text,
            size: TileSize::Header,
            section: Section::Runtimes,
            access: Access::Named,
        }
    }

    pub const fn size(mut self, size: TileSize) -> Self {
        self.size = size;
        self
    }

    /// The colors of this section instead of the Runtimes colors.
    pub const fn section(mut self, section: Section) -> Self {
        self.section = section;
        self
    }

    pub const fn decorative(mut self) -> Self {
        self.access = Access::Decorative;
        self
    }

    pub const fn label(mut self, label: &'a str) -> Self {
        self.access = Access::Label(label);
        self
    }

    pub const fn text(&self) -> &'a str {
        self.text
    }

    /// 12 px, 600 weight (the mockups').
    pub const fn token(theme: &Theme) -> TextToken {
        TextToken {
            size: theme.text.caption.size,
            face: Face::SemiBold,
        }
    }
}

impl Widget for Monogram<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let side = self.size.side(theme);
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
        if ui.is_rect_visible(rect) {
            paint_monogram(
                ui.painter(),
                theme,
                rect,
                self.text,
                self.size,
                self.section,
            );
        }
        let (access, name) = (self.access, self.text);
        response.widget_info(|| tile_info(access, name));
        response
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui;

    use marsa_core::runtimes::Runtime;

    use super::*;
    use crate::theme::tests::TEXT;
    use crate::theme::tests::contrast;
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::Harness;

    #[test]
    fn each_section_tile_is_its_tint_with_its_color_in_both_themes() {
        for theme in [&LIGHT, &DARK] {
            for section in Section::ALL {
                let colors = section_colors(theme, section);
                assert_eq!(colors, theme.section(section));
                assert_ne!(colors.fg, colors.tint);
            }
            // A few the mockups give by value.
            let mail = section_colors(theme, Section::Mail);
            let expected = if theme.dark {
                (0xF39AB8, 0x43202D)
            } else {
                (0xB42E5A, 0xF8E1E9)
            };
            let rgb = |c: egui::Color32| {
                u32::from(c.r()) << 16 | u32::from(c.g()) << 8 | u32::from(c.b())
            };
            assert_eq!((rgb(mail.fg), rgb(mail.tint)), expected);
        }
    }

    #[test]
    fn no_two_sections_share_their_colors() {
        for theme in [&LIGHT, &DARK] {
            for (index, a) in Section::ALL.iter().enumerate() {
                for b in &Section::ALL[index + 1..] {
                    assert_ne!(
                        section_colors(theme, *a),
                        section_colors(theme, *b),
                        "{a:?} and {b:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn runtimes_share_the_runtimes_colors_and_have_their_own_letters() {
        // The colors of a tile are its section's: the Runtimes ones unless a
        // screen asks for another.
        for theme in [&LIGHT, &DARK] {
            let shapes = |monogram: Monogram<'static>| {
                let harness = Harness::new(if theme.dark {
                    egui::Theme::Dark
                } else {
                    egui::Theme::Light
                });
                harness.shapes(|ui| {
                    ui.add(monogram);
                })
            };
            let fill = |shapes: Vec<egui::Shape>| {
                shapes.into_iter().find_map(|shape| match shape {
                    egui::Shape::Rect(rect) => Some(rect.fill),
                    _ => None,
                })
            };
            let runtimes = fill(shapes(Monogram::new("Py")));
            assert_eq!(runtimes, Some(theme.section(Section::Runtimes).tint));
            let databases = fill(shapes(Monogram::new("Pg").section(Section::Databases)));
            assert_eq!(databases, Some(theme.section(Section::Databases).tint));
        }
        let letters: Vec<String> = Runtime::ALL
            .into_iter()
            .map(strings::runtime_monogram)
            .collect();
        assert_eq!(letters, ["PHP", "JS", "Co", "Ca", "Po"]);
        assert!(letters.iter().all(|text| (2..=3).contains(&text.len())));
    }

    #[test]
    fn the_nav_tile_is_26_with_a_7_corner_and_the_header_tile_32_with_8() {
        for theme in [&LIGHT, &DARK] {
            assert_eq!(TileSize::Nav.side(theme), 26.0);
            assert_eq!(TileSize::Nav.radius(theme), 7);
            assert_eq!(TileSize::Header.side(theme), 32.0);
            assert_eq!(TileSize::Header.radius(theme), theme.radius.md);
            // The icon inside is smaller than the smaller tile.
            assert!(theme.size.icon_tile < TileSize::Nav.side(theme));
        }
    }

    #[test]
    fn a_section_color_reads_on_its_tile() {
        for theme in [&LIGHT, &DARK] {
            for section in Section::ALL {
                let colors = section_colors(theme, section);
                // tokens.md: the light Services icon is 4.49:1, fine for an
                // icon and for bold text.
                assert!(
                    contrast(colors.fg, colors.tint) >= TEXT - 0.02,
                    "{section:?}"
                );
            }
        }
    }

    #[test]
    fn a_tile_is_drawn_at_its_size_and_a_screen_reader_hears_its_name() {
        let harness = Harness::new(egui::Theme::Light);
        let mut sizes = Vec::new();
        let tree = harness.accessibility(|ui| {
            sizes.push(ui.add(SectionTile::new(Section::Mail)).rect.size());
            sizes.push(
                ui.add(SectionTile::new(Section::Dumps).size(TileSize::Header))
                    .rect
                    .size(),
            );
            sizes.push(ui.add(Monogram::new("PHP")).rect.size());
            ui.add(SectionTile::new(Section::Logs).label("Log files"));
            ui.add(SectionTile::new(Section::Queues).decorative());
        });
        assert_eq!(
            sizes,
            [Vec2::splat(26.0), Vec2::splat(32.0), Vec2::splat(32.0)]
        );
        for name in ["Mail", "Dumps", "PHP", "Log files"] {
            let node = tree.node_with_label(name).expect(name);
            assert_eq!(node.role(), Role::Image);
        }
        // A decorative tile has no name, and Queues is nowhere.
        assert!(tree.node_with_label("Queues").is_none());
    }

    #[test]
    fn the_letters_fit_the_tile() {
        let harness = Harness::new(egui::Theme::Light);
        harness.run(|ui| {
            for runtime in Runtime::ALL {
                let letters = strings::runtime_monogram(runtime);
                let galley = layout(
                    ui.painter(),
                    &letters,
                    Monogram::token(&LIGHT),
                    LIGHT.color.ink,
                );
                let side = TileSize::Header.side(&LIGHT);
                // At least 3 points each side, so "PHP" does not touch the edge.
                assert!(galley.size().x + 6.0 <= side, "{runtime:?}");
                assert!(galley.size().y <= side);
            }
        });
    }
}
