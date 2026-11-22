//! The tables page of the gallery.

use eframe::egui::Ui;

use crate::strings;
use crate::theme::{Section, Theme};
use crate::widgets::badge::RowBadge;
use crate::widgets::button::Button;
use crate::widgets::facts::fact_card;
use crate::widgets::icons::{Glyph, Icon};
use crate::widgets::switch::Switch;
use crate::widgets::table::{RowHeight, Table, TableColumn, empty_state};
use crate::widgets::tabs::Tabs;
use crate::widgets::version::{Setting, VersionMenu};

/// Set to `1` to show the gallery.
use super::{Live, PHP_VERSIONS, caption, heading, label};

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

pub(super) fn tables_page(ui: &mut Ui, theme: &Theme) {
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

    heading(ui, theme, label::TABS);
    let tabs = [(0, "Overview"), (1, "Mail"), (2, "Dumps"), (3, "Logs")];
    if let Some(chosen) = Tabs::new("Site", &tabs, live.tab).show(ui).inner {
        live.tab = chosen;
    }

    heading(ui, theme, label::FACTS);
    fact_card(ui, theme.size.version_lg.x, |rows| {
        rows.row("Connection", |ui| {
            ui.add(Switch::new(true, "Serve over HTTPS"));
        });
        rows.row("Folder", |ui| {
            ui.label("~/Code/shop");
        });
        rows.row("Kind", |ui| {
            ui.label("Laravel");
        });
    });
    ui.ctx().data_mut(|data| data.insert_temp(id, live));
}
