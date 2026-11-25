//! The widget gallery: one page per group of widgets, each widget in each of
//! its states, in the light or the dark theme, for the design review. Debug builds only (see `widgets/mod.rs`): a release build has no
//! gallery and reads no extra environment variable.
//!
//! `MARSA_GUI_GALLERY=1 marsa-gui --data-dir <scratch>` shows it instead of
//! the window. `MARSA_GUI_GALLERY_PAGE=<n>` picks the first page, counting
//! from 1 (the buttons at the top change it). With `MARSA_GUI_SCREENSHOT` and `MARSA_GUI_THEME`
//! (see `screenshot`) it saves that page as a picture.
//!
//! The states are forced with `looks(...)`, because a still picture has no
//! pointer. The labels below are for the designer who reads this page: they
//! are not texts of the product, so they stay out of `strings/`.

use std::time::Instant;

use eframe::egui::{self, Frame, InnerResponse, Margin, RichText, ScrollArea, Ui, Vec2};

use super::button::Button;
use super::dialog::Answer;
use super::notice::Notice;
use crate::theme::Theme;

mod controls;
mod dialogs;
mod tables;

/// PHP's versions, and Node's: the pickers work for any runtime.
const PHP_VERSIONS: [(usize, &str); 3] = [(0, "8.3"), (1, "8.4"), (2, "8.5")];
const NODE_VERSIONS: [(usize, &str); 2] = [(0, "22"), (1, "20")];

/// Set to `1` to show the gallery.
const GALLERY_VAR: &str = "MARSA_GUI_GALLERY";
/// The page to open first, counting from 1.
const PAGE_VAR: &str = "MARSA_GUI_GALLERY_PAGE";

/// What the reviewer reads: not product text.
mod label {
    pub(super) const TITLE: &str = "Marsa widgets";
    pub(super) const ICONS: &str = "Icons";
    pub(super) const TILES: &str = "Section tiles: 26, 32, and the folded dot";
    pub(super) const MONOGRAMS: &str = "Runtime monograms";
    pub(super) const COUNTS: &str = "Sidebar counts: new, then seen";
    pub(super) const ROW_BADGES: &str = "Row badges: rest, hover, pressed, focus";
    pub(super) const BUTTONS: &str = "Buttons";
    pub(super) const SWITCH: &str = "Switch";
    pub(super) const SEGMENTED: &str = "Segmented control: rest, rest, focus, disabled";
    pub(super) const STATUS_DOT: &str = "Status dot with word: each health, then hover and focus";
    pub(super) const STATUS_SENTENCE: &str =
        "Status sentence: ok, and error with the path and the fixes";
    pub(super) const PATHS: &str =
        "Request path: the broken step at each place, and one not reached";
    pub(super) const PILLS: &str = "State pills";
    pub(super) const PICKER: &str =
        "Version picker: rest (PHP and Node), then hover, pressed, and focus on one button";
    pub(super) const PICKER_MORE: &str = "Version picker: medium (menu-bar panel), and disabled";
    pub(super) const MENU: &str = "Version menu: rest, hover, focus, disabled, and the menu open";
    pub(super) const VERSIONS_LIVE: &str =
        "Live: Tab (one stop, then the +), the arrows, Home, End, Enter, and Space";
    pub(super) const TABLE: &str =
        "Table: two-line rows, the second selected, hover a row to see it";
    pub(super) const TABLE_DENSE: &str = "Dense table (32): click a row to select it";
    pub(super) const EMPTY: &str = "Empty state";
    pub(super) const EDITOR: &str =
        "SQL editor: click in, type; Tab leaves, Cmd or Ctrl Enter runs (nothing here)";
    pub(super) const GRID: &str = "Data grid: Tab in, the arrows, Home, End, Enter on a header sorts; \
         numbers at the end, NULL as a tag, binary as its size";
    pub(super) const TABS: &str =
        "Tabs (the site page): click or use the arrows; only Overview ships now";
    pub(super) const FACTS: &str = "Facts card (the site page): a label column, then each value";
    pub(super) const DIALOG: &str =
        "Dialog over the scrim: destructive (Cancel has the focus), and calm";
    pub(super) const NOTICES: &str = "Notices: a success (closes in 4 s), and an error (stays)";
    pub(super) const PROGRESS: &str = "Progress: with a share, and with none yet";
    pub(super) const STEPS: &str =
        "Step cards (first run): done, to do now with its progress, and next";
    pub(super) const DIALOG_LIVE: &str = "Live: open the dialog (Esc cancels), or show a notice";
    pub(super) const FIELDS: &str = "Fields";
    pub(super) const LIVE: &str = "Live: click, type, and Tab through these";
    pub(super) const DISABLED_NOTE: &str =
        "Disabled is drawn at 55% opacity. Hover it for the reason: ";
    pub(super) const STATES: [&str; 5] = ["Rest", "Hover", "Pressed", "Focus", "Disabled"];
}

/// The reason a disabled widget gives.
const REASON: &str = "Starts and stops with the sites that need it";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Icons,
    Buttons,
    Controls,
    Status,
    Versions,
    Tables,
    Dialogs,
}

const PAGES: [(Page, &str); 7] = [
    (Page::Icons, "1  Icons, tiles, badges"),
    (Page::Buttons, "2  Buttons"),
    (Page::Controls, "3  Switch, segmented, fields"),
    (Page::Status, "4  Status"),
    (Page::Versions, "5  Versions"),
    (Page::Tables, "6  Tables"),
    (Page::Dialogs, "7  Dialogs, notices"),
];

/// What the live controls of the third page hold.
#[derive(Clone, Default)]
struct Live {
    on: bool,
    mode: usize,
    text: String,
    search: String,
    /// The version the live picker has chosen, and the live menu's setting
    /// (none: the site's files).
    version: usize,
    setting: Option<usize>,
    /// The row of the dense table the user clicked.
    row: Option<usize>,
    /// The tab the live tab strip has chosen.
    tab: usize,
    /// The live grid's current cell and sorted column.
    cell: (Option<usize>, usize),
    sorted: Option<(usize, bool)>,
    /// The live SQL editor's text.
    sql: Option<crate::widgets::editor::buffer::Buffer>,
    /// The live dialog is open, and what it answered last.
    dialog: bool,
    answer: Option<Answer>,
    /// The notice the buttons showed, and when it came on screen.
    notice: Option<(Notice, Instant)>,
}

/// Whether `MARSA_GUI_GALLERY=1`.
pub fn requested() -> bool {
    std::env::var(GALLERY_VAR).is_ok_and(|value| value == "1")
}

fn first_page() -> Page {
    page_numbered(std::env::var(PAGE_VAR).ok().as_deref())
}

/// The page with this number, counting from 1; the first page for none.
fn page_numbered(number: Option<&str>) -> Page {
    number
        .and_then(|number| number.parse::<usize>().ok())
        .and_then(|number| PAGES.get(number.checked_sub(1)?))
        .map_or(Page::Icons, |(page, _)| *page)
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
            // One button per page: a segmented control holds 2 to 4 options.
            for (index, (candidate, name)) in PAGES.iter().enumerate() {
                let button = if *candidate == page {
                    Button::primary(name)
                } else {
                    Button::secondary(name)
                };
                if ui.add(button).clicked() {
                    ui.ctx().data_mut(|data| data.insert_temp(page_id, index));
                }
            }
        });
        ScrollArea::vertical().show(ui, |ui| draw_page(ui, theme, page));
    });
}

fn draw_page(ui: &mut Ui, theme: &Theme, page: Page) {
    match page {
        Page::Icons => controls::icons_page(ui, theme),
        Page::Buttons => controls::buttons_page(ui, theme),
        Page::Controls => controls::controls_page(ui, theme),
        Page::Status => controls::status_page(ui, theme),
        Page::Versions => controls::versions_page(ui, theme),
        Page::Tables => tables::tables_page(ui, theme),
        Page::Dialogs => dialogs::dialogs_page(ui, theme),
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

#[cfg(test)]
mod tests {
    use eframe::egui::Role;

    use super::controls::dot_text;
    use super::*;
    use crate::strings;
    use crate::widgets::harness::{Harness, Tree};
    use crate::widgets::{Health, PillState};

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
    fn the_dialogs_page_shows_the_dialogs_the_notices_and_the_progress_in_both_themes() {
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let tree = tree_of(Page::Dialogs, theme);
            assert_eq!(
                tree.node_with_label("Unlink shop?").expect("dialog").role(),
                Role::AlertDialog
            );
            assert_eq!(
                tree.node_with_label("Install PHP 8.5?")
                    .expect("calm dialog")
                    .role(),
                Role::Dialog
            );
            assert_eq!(
                tree.node_with_label("Unlinked shop.")
                    .expect("success")
                    .role(),
                Role::Status
            );
            assert_eq!(
                tree.node_with_label("Caddy stopped.")
                    .expect("error")
                    .role(),
                Role::Alert
            );
            assert_eq!(
                tree.node_with_label("Downloading PHP 8.4.27: 18 of 31 MB")
                    .expect("progress")
                    .role(),
                Role::ProgressIndicator
            );
        }
    }

    #[test]
    fn the_tables_page_shows_a_table_a_dense_table_and_the_empty_state() {
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let tree = tree_of(Page::Tables, theme);
            assert_eq!(
                tree.node_with_label("Sites").expect("table").role(),
                Role::Table
            );
            assert_eq!(
                tree.node_with_label("Connection").expect("header").role(),
                Role::ColumnHeader
            );
            assert!(
                tree.node_with_label("PHP version for a site, 8.4")
                    .is_some()
            );
            assert!(tree.node_with_label("3 new mails").is_some());
            assert_eq!(
                tree.node_with_label("Add site…").expect("empty").role(),
                Role::Button
            );
        }
    }

    #[test]
    fn the_versions_page_shows_both_pickers_and_the_menu_in_both_themes() {
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let tree = tree_of(Page::Versions, theme);
            assert_eq!(
                tree.node_with_label("Default PHP").expect("php").role(),
                Role::RadioGroup
            );
            assert_eq!(
                tree.node_with_label("Default Node.js")
                    .expect("node")
                    .role(),
                Role::RadioGroup
            );
            assert_eq!(
                tree.node_with_label("Install another PHP version")
                    .expect("install")
                    .role(),
                Role::Button
            );
            assert!(tree.node_with_label("PHP version for shop, 8.4").is_some());
            // The open menu shows its rows.
            assert_eq!(
                tree.node_with_label(strings::USE_SITE_FILES)
                    .expect("the first row")
                    .role(),
                Role::MenuItemRadio
            );
        }
    }

    #[test]
    fn the_status_page_shows_every_health_state_and_path_in_both_themes() {
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let tree = tree_of(Page::Status, theme);
            for health in Health::ALL {
                let (title, caption) = dot_text(health);
                let label = format!("{title}, {caption}");
                assert_eq!(
                    tree.node_with_label(&label).expect("a dot").role(),
                    Role::Status
                );
            }
            assert_eq!(
                tree.node_with_label("Sites are not opening: Caddy stopped.")
                    .expect("the error sentence")
                    .role(),
                Role::Alert
            );
            assert_eq!(
                tree.node_with_label("Caddy, not working")
                    .expect("the broken step")
                    .role(),
                Role::ListItem
            );
            for state in PillState::ALL {
                assert!(tree.node_with_label(state.word()).is_some());
            }
        }
    }

    #[test]
    fn the_first_page_is_counted_from_one_and_falls_back_to_the_first() {
        assert_eq!(page_numbered(Some("2")), Page::Buttons);
        assert_eq!(page_numbered(Some("4")), Page::Status);
        assert_eq!(page_numbered(Some("5")), Page::Versions);
        assert_eq!(page_numbered(Some("6")), Page::Tables);
        assert_eq!(page_numbered(Some("7")), Page::Dialogs);
        for wrong in [None, Some("0"), Some("99"), Some("x"), Some("")] {
            assert_eq!(page_numbered(wrong), Page::Icons, "{wrong:?}");
        }
    }

    #[test]
    fn the_gallery_has_its_pages_and_the_five_states_of_components_md() {
        assert_eq!(PAGES.len(), 7);
        assert_eq!(
            label::STATES,
            ["Rest", "Hover", "Pressed", "Focus", "Disabled"]
        );
    }
}
