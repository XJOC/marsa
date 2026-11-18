//! The widget gallery: one page per group of widgets, each widget in each of
//! its states, in the light or the dark theme, for the design review (task
//! M5-D2). Debug builds only (see `widgets/mod.rs`): a release build has no
//! gallery and reads no extra environment variable.
//!
//! `MARSA_GUI_GALLERY=1 marsa-gui --data-dir <scratch>` shows it instead of
//! the window. `MARSA_GUI_GALLERY_PAGE=<n>` picks the first page, counting
//! from 1 (the buttons at the top change it). With `MARSA_GUI_SCREENSHOT` and `MARSA_GUI_THEME`
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
use super::status::{
    Health, PathStep, PillState, RequestPath, StatePill, StatusDot, StatusSentence, StepState,
};
use super::switch::Switch;
use super::table::{RowHeight, Table, TableColumn, empty_state};
use super::tile::{Monogram, SectionTile, TileSize};
use super::version::{PickerSize, Setting, VersionMenu, VersionPick, VersionPicker};
use crate::strings;
use crate::theme::{Section, Theme};

/// Set to `1` to show the gallery.
const GALLERY_VAR: &str = "MARSA_GUI_GALLERY";
/// The page to open first, counting from 1.
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
    pub const STATUS_DOT: &str = "Status dot with word: each health, then hover and focus";
    pub const STATUS_SENTENCE: &str = "Status sentence: ok, and error with the path and the fixes";
    pub const PATHS: &str = "Request path: the broken step at each place, and one not reached";
    pub const PILLS: &str = "State pills";
    pub const PICKER: &str =
        "Version picker: rest (PHP and Node), then hover, pressed, and focus on one button";
    pub const PICKER_MORE: &str = "Version picker: medium (menu-bar panel), and disabled";
    pub const MENU: &str = "Version menu: rest, hover, focus, disabled, and the menu open";
    pub const VERSIONS_LIVE: &str =
        "Live: Tab (one stop, then the +), the arrows, Home, End, Enter, and Space";
    pub const TABLE: &str = "Table: two-line rows, the second selected, hover a row to see it";
    pub const TABLE_DENSE: &str = "Dense table (32): click a row to select it";
    pub const EMPTY: &str = "Empty state";
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
    Status,
    Versions,
    Tables,
}

const PAGES: [(Page, &str); 6] = [
    (Page::Icons, "1  Icons, tiles, badges"),
    (Page::Buttons, "2  Buttons"),
    (Page::Controls, "3  Switch, segmented, fields"),
    (Page::Status, "4  Status"),
    (Page::Versions, "5  Versions"),
    (Page::Tables, "6  Tables"),
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
        Page::Icons => icons_page(ui, theme),
        Page::Buttons => buttons_page(ui, theme),
        Page::Controls => controls_page(ui, theme),
        Page::Status => status_page(ui, theme),
        Page::Versions => versions_page(ui, theme),
        Page::Tables => tables_page(ui, theme),
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

/// One site of the sample table.
struct Site {
    name: &'static str,
    folder: &'static str,
    version: (usize, &'static str),
    reason: &'static str,
    mail: u32,
    dumps: u32,
}

const SITES: [Site; 3] = [
    Site {
        name: "shop",
        folder: "~/Code/shop",
        version: (1, "8.4"),
        reason: "from .php-version",
        mail: 3,
        dumps: 12,
    },
    Site {
        name: "blog",
        folder: "~/Code/blog",
        version: (0, "8.3"),
        reason: "from composer.json",
        mail: 0,
        dumps: 0,
    },
    Site {
        name: "docs-with-a-long-name",
        folder: "~/Code/work/clients/acme/projects/docs-with-a-long-name",
        version: (2, "8.5"),
        reason: "set for this site",
        mail: 1,
        dumps: 0,
    },
];

fn tables_page(ui: &mut Ui, theme: &Theme) {
    let id = ui.make_persistent_id("gallery_tables");
    let mut live = ui
        .ctx()
        .data(|data| data.get_temp::<Live>(id))
        .unwrap_or_default();

    heading(ui, theme, label::TABLE);
    let columns = [
        TableColumn::fill("Site"),
        TableColumn::fixed("PHP", theme.size.sidebar - theme.space.s4),
        TableColumn::fixed("Connection", theme.size.version_lg.x),
        TableColumn::fixed("Caught", theme.size.version_lg.x + theme.space.s6),
        TableColumn::fixed("", theme.size.icon_button),
    ];
    Table::new("gallery_sites", "Sites", &columns)
        .rows(RowHeight::TwoLine)
        .selected(Some(1))
        .show(ui, SITES.len(), |row| {
            let site = &SITES[row.index()];
            row.two_lines(site.name, site.folder);
            row.cell(|ui| {
                let setting = Setting::Version(site.version.0);
                VersionMenu::new(
                    "PHP version for a site",
                    site.version.1,
                    strings::USE_SITE_FILES,
                    &PHP_VERSIONS,
                    setting,
                )
                .reason(site.reason)
                .show(ui);
            });
            row.cell(|ui| {
                ui.add(Glyph::new(Icon::Lock).color(theme.color.ink2));
                caption(ui, theme, "HTTPS");
            });
            row.cell(|ui| {
                if site.mail > 0 {
                    ui.add(RowBadge::new(Section::Mail, site.mail));
                }
                if site.dumps > 0 {
                    ui.add(RowBadge::new(Section::Dumps, site.dumps));
                }
            });
            row.cell(|ui| {
                ui.add(Button::icon(Icon::More, "More for a site"));
            });
        });

    heading(ui, theme, label::TABLE_DENSE);
    let columns = [
        TableColumn::fill("Name"),
        TableColumn::fixed("Rows", theme.size.version_lg.x),
    ];
    let output = Table::new("gallery_grid", "Tables", &columns)
        .rows(RowHeight::Dense)
        .selected(live.row)
        .show(ui, 4, |row| {
            let index = row.index();
            row.cell(|ui| {
                ui.label(["users", "sites", "mail", "jobs"][index]);
            });
            row.cell(|ui| {
                caption(ui, theme, ["1,204", "6", "48", "0"][index]);
            });
        });
    if output.clicked.is_some() {
        live.row = output.clicked;
    }

    heading(ui, theme, label::EMPTY);
    empty_state(ui, "No sites yet.", "Add site…");
    ui.ctx().data_mut(|data| data.insert_temp(id, live));
}

/// PHP's versions, and Node's: the pickers work for any runtime.
const PHP_VERSIONS: [(usize, &str); 3] = [(0, "8.3"), (1, "8.4"), (2, "8.5")];
const NODE_VERSIONS: [(usize, &str); 2] = [(0, "22"), (1, "20")];

fn versions_page(ui: &mut Ui, theme: &Theme) {
    let id = ui.make_persistent_id("gallery_versions");
    let mut live = ui
        .ctx()
        .data(|data| data.get_temp::<Live>(id))
        .unwrap_or_default();
    let php = strings::install_another_label("PHP");
    let node = strings::install_another_label("Node.js");
    let picker = |selected| VersionPicker::new("Default PHP", &PHP_VERSIONS, Some(selected));

    heading(ui, theme, label::PICKER);
    row(ui, |ui| {
        ui.add(picker(1).install(&php));
        ui.add_space(theme.space.s6);
        ui.add(VersionPicker::new("Default Node.js", &NODE_VERSIONS, Some(1)).install(&node));
    });
    row(ui, |ui| {
        ui.add(picker(1).looks(0, Look::hover()));
        ui.add_space(theme.space.s6);
        ui.add(picker(1).looks(1, Look::pressed()));
        ui.add_space(theme.space.s6);
        ui.add(picker(1).looks(2, Look::focused()));
    });

    heading(ui, theme, label::PICKER_MORE);
    row(ui, |ui| {
        ui.add(
            picker(1)
                .size(PickerSize::Medium)
                .install(&php)
                .looks(3, Look::hover()),
        );
        ui.add_space(theme.space.s6);
        ui.add(picker(1).disabled(REASON));
    });

    heading(ui, theme, label::MENU);
    let menu = |setting, shown| {
        VersionMenu::new(
            "PHP version for shop",
            shown,
            strings::USE_SITE_FILES,
            &PHP_VERSIONS,
            setting,
        )
        .reason("from .php-version")
    };
    ui.horizontal_top(|ui| {
        // One menu button a line, so each shows its reason after it.
        ui.vertical(|ui| {
            menu(Setting::SiteFiles, "8.4").show(ui);
            menu(Setting::Version(2), "8.5")
                .looks(Look::hover())
                .show(ui);
            menu(Setting::SiteFiles, "8.4")
                .looks(Look::focused())
                .show(ui);
            menu(Setting::SiteFiles, "8.4").disabled(REASON).show(ui);
        });
        ui.add_space(theme.space.s6);
        // The menu open: the same rows the popup shows.
        Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_width(theme.size.sidebar);
            ui.vertical(|ui| menu(Setting::Version(1), "8.4").list(ui));
        });
    });

    heading(ui, theme, label::VERSIONS_LIVE);
    row(ui, |ui| {
        if let Some(VersionPick::Version(chosen)) =
            picker(live.version).install(&php).show(ui).inner
        {
            live.version = chosen;
        }
        let setting = live.setting.map_or(Setting::SiteFiles, Setting::Version);
        let shown = PHP_VERSIONS[live.setting.unwrap_or(1)].1;
        if let Some(chosen) = menu(setting, shown).show(ui).inner {
            live.setting = match chosen {
                Setting::SiteFiles => None,
                Setting::Version(version) => Some(version),
            };
        }
    });
    ui.ctx().data_mut(|data| data.insert_temp(id, live));
}

/// The title and the caption a status dot has for `health`.
const fn dot_text(health: Health) -> (&'static str, &'static str) {
    match health {
        Health::Ok => ("All sites working", "DNS, Caddy, PHP"),
        Health::Warning => ("Check the setup", "HTTPS is not trusted"),
        Health::Error => ("Sites not opening", "Caddy stopped"),
        Health::Busy => ("Starting Marsa", "Starting Caddy"),
    }
}

fn status_page(ui: &mut Ui, theme: &Theme) {
    heading(ui, theme, label::STATUS_DOT);
    row(ui, |ui| {
        for health in Health::ALL {
            let (title, caption) = dot_text(health);
            ui.add(StatusDot::new(health, title, caption));
        }
    });
    row(ui, |ui| {
        let (title, caption) = dot_text(Health::Ok);
        ui.add(StatusDot::new(Health::Ok, title, caption).looks(Look::hover()));
        ui.add(StatusDot::new(Health::Ok, title, caption).looks(Look::focused()));
    });

    heading(ui, theme, label::STATUS_SENTENCE);
    ui.add(StatusSentence::new(
        Health::Ok,
        "All 6 sites are working.",
        "DNS, Caddy, and PHP 8.4 are running. PHP 8.3 starts when one of its sites gets a visit.",
    ));
    StatusSentence::new(
        Health::Error,
        "Sites are not opening: Caddy stopped.",
        "Caddy is the web server that answers every .test address. It stopped at 14:02.",
    )
    .show_with(ui, |ui| {
        ui.horizontal(|ui| {
            ui.add(RequestPath::new(strings::REQUEST_PATH, &SHOP_PATH));
            ui.add(Button::danger("Start Caddy"));
            ui.add(Button::secondary("Show log"));
        });
    });

    heading(ui, theme, label::PATHS);
    let paths: Vec<_> = (0..SHOP_PATH.len()).map(broken_at).collect();
    for pair in paths.chunks(2) {
        row(ui, |ui| {
            for steps in pair {
                ui.add(RequestPath::new(strings::REQUEST_PATH, steps));
                ui.add_space(theme.space.s6);
            }
        });
    }

    heading(ui, theme, label::PILLS);
    row(ui, |ui| {
        for state in PillState::ALL {
            ui.add(StatePill::new(state));
        }
    });
}

/// The shop path with the step at `broken` stopped: the steps before it
/// work, the steps after it are not reached.
fn broken_at(broken: usize) -> [PathStep<'static>; 4] {
    let mut steps = SHOP_PATH;
    for (index, step) in steps.iter_mut().enumerate() {
        step.state = match index.cmp(&broken) {
            std::cmp::Ordering::Less => StepState::Working,
            std::cmp::Ordering::Equal => StepState::Broken,
            std::cmp::Ordering::Greater => StepState::Unreached,
        };
    }
    steps
}

/// The steps of a request to `shop.test`, in the order it takes them.
const SHOP_PATH: [PathStep<'static>; 4] = [
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
                assert!(
                    tree.node_with_label(strings::pill_state_word(state))
                        .is_some()
                );
            }
        }
    }

    #[test]
    fn the_first_page_is_counted_from_one_and_falls_back_to_the_first() {
        assert_eq!(page_numbered(Some("2")), Page::Buttons);
        assert_eq!(page_numbered(Some("4")), Page::Status);
        assert_eq!(page_numbered(Some("5")), Page::Versions);
        assert_eq!(page_numbered(Some("6")), Page::Tables);
        for wrong in [None, Some("0"), Some("99"), Some("x"), Some("")] {
            assert_eq!(page_numbered(wrong), Page::Icons, "{wrong:?}");
        }
    }

    #[test]
    fn the_gallery_has_its_pages_and_the_five_states_of_components_md() {
        assert_eq!(PAGES.len(), 6);
        assert_eq!(
            label::STATES,
            ["Rest", "Hover", "Pressed", "Focus", "Disabled"]
        );
    }
}
