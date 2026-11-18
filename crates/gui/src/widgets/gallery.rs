//! The widget gallery: one page per group of widgets, each widget in each of
//! its states, in the light or the dark theme, for the design review (task
//! M5-D2). Debug builds only (see `widgets/mod.rs`): a release build has no
//! gallery and reads no extra environment variable.
//!
//! `MARSA_GUI_GALLERY=1 marsa-gui --data-dir <scratch>` shows it instead of
//! the window. `MARSA_GUI_GALLERY_PAGE=1|2|3` picks the first page (the tabs
//! at the top change it). With `MARSA_GUI_SCREENSHOT` and `MARSA_GUI_THEME`
//! (see `screenshot`) it saves that page as a picture.
//!
//! The states are forced with `looks(...)`, because a still picture has no
//! pointer. The labels below are for the designer who reads this page: they
//! are not texts of the product, so they stay out of `strings.rs`.

use eframe::egui::{
    self, Frame, Grid, InnerResponse, Margin, RichText, ScrollArea, Ui, Vec2, vec2,
};
use marsa_core::runtimes::Runtime;

use super::Look;
use super::badge::{CountBadge, RowBadge};
use super::button::{Button, QuietTone};
use super::field::Field;
use super::icons::{Glyph, Icon};
use super::segmented::Segmented;
use super::switch::Switch;
use super::tile::{Monogram, SectionTile, TileSize};
use crate::strings;
use crate::theme::{Section, Theme};

/// Set to `1` to show the gallery.
const GALLERY_VAR: &str = "MARSA_GUI_GALLERY";
/// The page to open first: 1, 2, or 3.
const PAGE_VAR: &str = "MARSA_GUI_GALLERY_PAGE";

/// What the reviewer reads: not product text.
mod label {
    pub const TITLE: &str = "Marsa widgets";
    pub const ICONS: &str = "Icons";
    pub const TILES: &str = "Section tiles: 26, 32, and the folded dot";
    pub const MONOGRAMS: &str = "Runtime monograms";
    pub const COUNTS: &str = "Sidebar counts: new, then seen";
    pub const ROW_BADGES: &str = "Row badges: rest, hover, pressed, focus";
    pub const BUTTONS: &str = "Buttons";
    pub const SWITCH: &str = "Switch";
    pub const SEGMENTED: &str = "Segmented control: rest, rest, focus, disabled";
    pub const FIELDS: &str = "Fields";
    pub const LIVE: &str = "Live: click, type, and Tab through these";
    pub const DISABLED_NOTE: &str = "Disabled is drawn at 55% opacity. Hover it for the reason: ";
    pub const STATES: [&str; 5] = ["Rest", "Hover", "Pressed", "Focus", "Disabled"];
}

/// The reason a disabled widget gives.
const REASON: &str = "Starts and stops with the sites that need it";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Icons,
    Buttons,
    Controls,
}

const PAGES: [(Page, &str); 3] = [
    (Page::Icons, "1  Icons, tiles, badges"),
    (Page::Buttons, "2  Buttons"),
    (Page::Controls, "3  Switch, segmented, fields"),
];

/// What the live controls of the third page hold.
#[derive(Clone, Default)]
struct Live {
    on: bool,
    mode: usize,
    text: String,
    search: String,
}

/// Whether `MARSA_GUI_GALLERY=1`.
pub fn requested() -> bool {
    std::env::var(GALLERY_VAR).is_ok_and(|value| value == "1")
}

fn first_page() -> Page {
    match std::env::var(PAGE_VAR).as_deref() {
        Ok("2") => Page::Buttons,
        Ok("3") => Page::Controls,
        _ => Page::Icons,
    }
}

/// Draws the gallery over the whole window.
pub fn show(ui: &mut Ui) {
    let theme = Theme::of(ui.ctx());
    let page_id = ui.make_persistent_id("gallery_page");
    let page = ui
        .ctx()
        .data(|data| data.get_temp::<usize>(page_id))
        .and_then(|index| PAGES.get(index).map(|(page, _)| *page))
        .unwrap_or_else(first_page);
    let frame = Frame::NONE
        .fill(theme.color.bg)
        .inner_margin(Margin::symmetric(
            whole(theme.space.screen_side),
            whole(theme.space.screen_top),
        ));
    egui::CentralPanel::default().frame(frame).show(ui, |ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(theme.space.s3);
        ui.horizontal(|ui| {
            heading(ui, theme, label::TITLE);
            if let Some(chosen) = Segmented::new("Gallery page", &PAGES, page).show(ui).inner
                && let Some(index) = PAGES.iter().position(|(page, _)| *page == chosen)
            {
                ui.ctx().data_mut(|data| data.insert_temp(page_id, index));
            }
        });
        ScrollArea::vertical().show(ui, |ui| draw_page(ui, theme, page));
    });
}

fn draw_page(ui: &mut Ui, theme: &Theme, page: Page) {
    match page {
        Page::Icons => icons_page(ui, theme),
        Page::Buttons => buttons_page(ui, theme),
        Page::Controls => controls_page(ui, theme),
    }
}

/// A whole-point margin from a space token (the tokens are whole numbers).
fn whole(points: f32) -> i8 {
    points as i8
}

fn heading(ui: &mut Ui, theme: &Theme, text: &str) {
    ui.label(
        RichText::new(text)
            .font(theme.text.heading.font_id())
            .color(theme.color.ink),
    );
}

fn caption(ui: &mut Ui, theme: &Theme, text: &str) {
    ui.label(
        RichText::new(text)
            .font(theme.text.caption.font_id())
            .color(theme.color.mute),
    );
}

/// A row of items from the start, as high as its tallest item.
fn row<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    ui.horizontal(add)
}

/// `items` in rows of `per_row` cells, each `width` wide.
fn cells<T>(
    ui: &mut Ui,
    items: &[T],
    per_row: usize,
    width: f32,
    mut cell: impl FnMut(&mut Ui, &T),
) {
    for chunk in items.chunks(per_row) {
        ui.horizontal_top(|ui| {
            for item in chunk {
                ui.vertical(|ui| {
                    ui.set_width(width);
                    cell(ui, item);
                });
            }
        });
    }
}

fn icons_page(ui: &mut Ui, theme: &Theme) {
    heading(ui, theme, label::ICONS);
    cells(ui, &Icon::ALL, 10, theme.size.version_lg.x, |ui, icon| {
        row(ui, |ui| {
            ui.add(
                Glyph::new(*icon)
                    .size(theme.size.tile_header)
                    .color(theme.color.ink),
            );
            ui.add(
                Glyph::new(*icon)
                    .size(theme.size.icon_tile)
                    .color(theme.color.mute),
            );
            ui.add(
                Glyph::new(*icon)
                    .size(theme.size.icon_badge)
                    .color(theme.color.mute),
            );
        });
        caption(ui, theme, &format!("{icon:?}"));
    });

    heading(ui, theme, label::TILES);
    cells(
        ui,
        &Section::ALL,
        5,
        theme.size.version_lg.x + theme.size.version_lg.x,
        |ui, section| {
            row(ui, |ui| {
                ui.add(SectionTile::new(*section));
                ui.add(SectionTile::new(*section).size(TileSize::Header));
                ui.add(SectionTile::new(*section).dot(true));
            });
            caption(ui, theme, strings::section_name(*section));
        },
    );

    heading(ui, theme, label::MONOGRAMS);
    row(ui, |ui| {
        for runtime in [
            Runtime::Php,
            Runtime::Node,
            Runtime::Composer,
            Runtime::Caddy,
        ] {
            ui.add(Monogram::of(runtime));
        }
        ui.add(Monogram::new("Py"));
        ui.add(Monogram::new("Co").size(TileSize::Nav));
    });

    heading(ui, theme, label::COUNTS);
    row(ui, |ui| {
        for (section, count) in [
            (Section::Sites, 6),
            (Section::Mail, 3),
            (Section::Dumps, 12),
            (Section::Logs, 120),
        ] {
            ui.add(CountBadge::new(section, count).fresh(true));
            ui.add(CountBadge::new(section, count));
        }
    });

    heading(ui, theme, label::ROW_BADGES);
    row(ui, |ui| {
        for section in [Section::Mail, Section::Dumps] {
            ui.add(RowBadge::new(section, 3));
            ui.add(RowBadge::new(section, 9).looks(Look::hover()));
            ui.add(RowBadge::new(section, 12).looks(Look::pressed()));
            ui.add(RowBadge::new(section, 120).looks(Look::focused()));
        }
    });
}

/// Builds a button for one state: the look to force, or none.
type Build = fn(Option<Look>) -> Button<'static>;

/// A button as it looks in `look`, or really when there is none.
fn forced(button: Button<'static>, look: Option<Look>) -> Button<'static> {
    match look {
        Some(look) => button.looks(look),
        None => button,
    }
}

fn buttons_page(ui: &mut Ui, theme: &Theme) {
    heading(ui, theme, label::BUTTONS);
    Grid::new("buttons")
        .spacing(vec2(theme.space.s6, theme.space.s3))
        .show(ui, |ui| {
            ui.label("");
            for state in label::STATES {
                caption(ui, theme, state);
            }
            ui.end_row();
            // The first button of a row is the real one, so a mouse can hover
            // it; the others are forced to look as they would.
            let kinds: [(&str, Build); 8] = [
                ("Primary", |look| forced(Button::primary("Add site…"), look)),
                ("Secondary", |look| {
                    forced(Button::secondary("Open folder"), look)
                }),
                ("Danger", |look| forced(Button::danger("Unlink"), look)),
                ("Quiet, link", |look| {
                    forced(Button::quiet("Show log"), look)
                }),
                ("Quiet, error", |look| {
                    forced(
                        Button::quiet("Unlink this site…").tone(QuietTone::Error),
                        look,
                    )
                }),
                ("Icon", |look| {
                    forced(Button::icon(Icon::More, "More for PHP 8.4"), look)
                }),
                ("With icon", |look| {
                    forced(
                        Button::secondary("Install a version…").leading(Icon::Plus),
                        look,
                    )
                }),
                ("In a table row", |look| {
                    forced(Button::secondary("Restart").in_row(), look)
                }),
            ];
            for (name, build) in kinds {
                caption(ui, theme, name);
                ui.add(build(None));
                ui.add(build(Some(Look::hover())));
                ui.add(build(Some(Look::pressed())));
                ui.add(build(Some(Look::focused())));
                ui.add(build(None).disabled(REASON));
                ui.end_row();
            }
        });
    caption(ui, theme, &format!("{}{REASON}", label::DISABLED_NOTE));
}

fn controls_page(ui: &mut Ui, theme: &Theme) {
    let id = ui.make_persistent_id("gallery_live");
    let mut live = ui
        .ctx()
        .data(|data| data.get_temp::<Live>(id))
        .unwrap_or_default();

    heading(ui, theme, label::SWITCH);
    Grid::new("switches")
        .spacing(vec2(theme.space.s6, theme.space.s3))
        .show(ui, |ui| {
            for state in label::STATES {
                caption(ui, theme, state);
            }
            ui.end_row();
            for (on, name) in [(true, "Start at login"), (false, "Xdebug")] {
                ui.add(Switch::new(on, name));
                ui.add(Switch::new(on, name).looks(Look::hover()));
                ui.add(Switch::new(on, name).looks(Look::pressed()));
                ui.add(Switch::new(on, name).looks(Look::focused()));
                ui.add(Switch::new(on, name).disabled(REASON));
                ui.end_row();
            }
        });

    heading(ui, theme, label::SEGMENTED);
    let modes = [(0, "System"), (1, "Light"), (2, "Dark")];
    row(ui, |ui| {
        ui.add(Segmented::new("Appearance", &modes, 0));
        ui.add(Segmented::new("Appearance", &modes, 1));
        ui.add(Segmented::new("Appearance", &modes, 2).looks(Look::focused()));
        ui.add(Segmented::new("Appearance", &modes, 1).disabled(REASON));
    });

    heading(ui, theme, label::FIELDS);
    let width = theme.size.sidebar;
    let mut empty = String::new();
    let mut filled = String::from("shop");
    let mut focused = String::from("shop");
    let mut wrong = String::from("my shop");
    let mut search = String::new();
    let mut found = String::from("sho");
    let mut off = String::from("shop");
    Grid::new("fields")
        .spacing(vec2(theme.space.s6, theme.space.s3))
        .show(ui, |ui| {
            caption(ui, theme, "Rest, empty");
            ui.add(
                Field::new(&mut empty, "Site name")
                    .hint("Site name")
                    .width(width),
            );
            caption(ui, theme, "Rest, filled");
            ui.add(Field::new(&mut filled, "Site name").width(width));
            ui.end_row();
            caption(ui, theme, "Focus");
            ui.add(
                Field::new(&mut focused, "Site name")
                    .width(width)
                    .looks(Look::focused()),
            );
            caption(ui, theme, "Disabled");
            ui.add(
                Field::new(&mut off, "Site name")
                    .width(width)
                    .disabled(REASON),
            );
            ui.end_row();
            caption(ui, theme, "Search, empty");
            ui.add(
                Field::search(&mut search, "Find a site")
                    .hint("Find a site")
                    .key_hint("Cmd K")
                    .width(width),
            );
            caption(ui, theme, "Search, filled");
            ui.add(Field::search(&mut found, "Find a site").width(width));
            ui.end_row();
            caption(ui, theme, "Error");
            ui.add(
                Field::new(&mut wrong, "Site name")
                    .width(width)
                    .error("Use letters, digits, and dashes."),
            );
            ui.end_row();
        });

    heading(ui, theme, label::LIVE);
    row(ui, |ui| {
        if ui.add(Switch::new(live.on, "Start at login")).clicked() {
            live.on = !live.on;
        }
        if let Some(chosen) = Segmented::new("Appearance", &modes, live.mode)
            .show(ui)
            .inner
        {
            live.mode = chosen;
        }
        ui.add(
            Field::new(&mut live.text, "Site name")
                .hint("Type here")
                .width(width),
        );
        ui.add(
            Field::search(&mut live.search, "Find")
                .hint("Find a site")
                .width(width),
        );
    });
    ui.ctx().data_mut(|data| data.insert_temp(id, live));
}

#[cfg(test)]
mod tests {
    use eframe::egui::Role;

    use super::*;
    use crate::widgets::harness::{Harness, Tree};

    fn tree_of(page: Page, theme: egui::Theme) -> Tree {
        let harness = Harness::new(theme);
        let active = Theme::of(&harness.ctx);
        harness.accessibility(|ui| draw_page(ui, active, page))
    }

    #[test]
    fn every_page_draws_in_both_themes_with_its_widgets() {
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let icons = tree_of(Page::Icons, theme);
            // A section tile, a row badge, and a monogram.
            assert_eq!(
                icons.node_with_label("Mail").expect("tile").role(),
                Role::Image
            );
            assert_eq!(
                icons.node_with_label("3 new mails").expect("badge").role(),
                Role::Button
            );
            assert!(icons.node_with_label("PHP").is_some());

            let buttons = tree_of(Page::Buttons, theme);
            assert_eq!(
                buttons
                    .node_with_label("Add site…")
                    .expect("primary")
                    .role(),
                Role::Button
            );
            assert!(buttons.node_with_label("More for PHP 8.4").is_some());

            let controls = tree_of(Page::Controls, theme);
            assert_eq!(
                controls.node_with_label("Xdebug").expect("switch").role(),
                Role::Switch
            );
            assert_eq!(
                controls
                    .node_with_label("Appearance")
                    .expect("group")
                    .role(),
                Role::RadioGroup
            );
            assert_eq!(
                controls
                    .node_with_label("Find a site")
                    .expect("search")
                    .role(),
                Role::SearchInput
            );
        }
    }

    #[test]
    fn the_gallery_has_three_pages_and_the_five_states_of_components_md() {
        assert_eq!(PAGES.len(), 3);
        assert_eq!(
            label::STATES,
            ["Rest", "Hover", "Pressed", "Focus", "Disabled"]
        );
    }
}
