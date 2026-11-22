//! The icons, buttons, controls, versions, and status pages of the gallery.

use eframe::egui::{Frame, Grid, Ui, vec2};
use marsa_core::runtimes::Runtime;

use crate::strings;
use crate::theme::{Section, Theme};
use crate::widgets::Look;
use crate::widgets::badge::{CountBadge, RowBadge};
use crate::widgets::button::{Button, QuietTone};
use crate::widgets::field::Field;
use crate::widgets::icons::{Glyph, Icon};
use crate::widgets::segmented::Segmented;
use crate::widgets::status::{
    Health, PathStep, PillState, RequestPath, StatePill, StatusDot, StatusSentence, StepState,
};
use crate::widgets::switch::Switch;
use crate::widgets::tile::{Monogram, SectionTile, TileSize, runtime_monogram};
use crate::widgets::version::{PickerSize, Setting, VersionMenu, VersionPick, VersionPicker};

/// Set to `1` to show the gallery.
use super::{Live, NODE_VERSIONS, PHP_VERSIONS, REASON, caption, cells, heading, label, row};

pub(super) fn icons_page(ui: &mut Ui, theme: &Theme) {
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
        for runtime in Runtime::ALL {
            ui.add(Monogram::new(runtime_monogram(runtime)));
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

pub(super) fn buttons_page(ui: &mut Ui, theme: &Theme) {
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

pub(super) fn controls_page(ui: &mut Ui, theme: &Theme) {
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

pub(super) fn versions_page(ui: &mut Ui, theme: &Theme) {
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
pub(super) const fn dot_text(health: Health) -> (&'static str, &'static str) {
    match health {
        Health::Ok => ("All sites working", "DNS, Caddy, PHP"),
        Health::Warning => ("Check the setup", "HTTPS is not trusted"),
        Health::Error => ("Sites not opening", "Caddy stopped"),
        Health::Busy => ("Starting Marsa", "Starting Caddy"),
    }
}

pub(super) fn status_page(ui: &mut Ui, theme: &Theme) {
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
