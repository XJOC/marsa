//! The tables page of the gallery.

use eframe::egui::{Ui, vec2};
use marsa_core::db_browser::Engine;
use marsa_core::types::{DbCell, DbColumnKind, DbPageColumn};

use crate::strings;
use crate::theme::{Section, Theme};
use crate::widgets::badge::RowBadge;
use crate::widgets::button::Button;
use crate::widgets::editor::Editor;
use crate::widgets::editor::buffer::Buffer;
use crate::widgets::facts::fact_card;
use crate::widgets::grid::{Grid, GridEvent, column_widths};
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

    heading(ui, theme, label::GRID);
    grid(ui, theme, &mut live);

    heading(ui, theme, label::EDITOR);
    editor(ui, &mut live);

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

/// The rows the sample grid shows at once.
const GRID_ROWS: usize = 8;

/// A page of 200 sample rows, in a box `GRID_ROWS` high.
fn grid(ui: &mut Ui, theme: &Theme, live: &mut Live) {
    let column = |name: &str, kind| DbPageColumn {
        name: name.into(),
        kind,
    };
    let columns = [
        column("id", DbColumnKind::Number),
        column("email", DbColumnKind::Text),
        column("created_at", DbColumnKind::Time),
        column("avatar", DbColumnKind::Binary),
        column("deleted_at", DbColumnKind::Time),
    ];
    let rows: Vec<Vec<DbCell>> = (1..=200_u64)
        .map(|id| {
            vec![
                DbCell::Text(id.to_string()),
                DbCell::Text(format!("user{id}@example.test")),
                DbCell::Text(format!("2026-11-{:02} 09:30:00", id % 28 + 1)),
                DbCell::Binary { bytes: id * 731 },
                if id % 3 == 0 {
                    DbCell::Text("2026-11-24 18:00:00".into())
                } else {
                    DbCell::Null
                },
            ]
        })
        .collect();
    let widths = column_widths(ui, &columns, &rows);
    let height = theme.size.table_header + GRID_ROWS as f32 * theme.size.row_dense;
    let size = vec2(ui.available_width(), height);
    let events = ui
        .allocate_ui(size, |ui| {
            Grid::new("gallery_data_grid", &columns, &rows, &widths, live.cell)
                .sortable(live.sorted)
                .show(ui)
        })
        .inner;
    for event in events {
        match event {
            GridEvent::Current { row, column } => live.cell = (row, column),
            GridEvent::Sort(column) => {
                live.sorted = match live.sorted {
                    Some((sorted, false)) if sorted == column => Some((column, true)),
                    Some((sorted, true)) if sorted == column => None,
                    _ => Some((column, false)),
                };
            }
            _ => {}
        }
    }
}

/// A short query, with a comment over two lines and a string.
const SAMPLE_SQL: &str = "/* The orders of one customer,
   newest first. */
select id, total, 'paid' as state
from orders
where customer_id = 42
order by id desc
limit 50;";

/// The rows the sample editor shows.
const EDITOR_LINES: usize = 6;

fn editor(ui: &mut Ui, live: &mut Live) {
    let limit = marsa_core::db_browser::SQL_BYTES;
    let buffer = live
        .sql
        .get_or_insert_with(|| Buffer::new(SAMPLE_SQL, limit));
    Editor::new("gallery_sql", "SQL", buffer, Engine::Postgres, EDITOR_LINES).show(ui);
}
