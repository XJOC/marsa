//! `table=100000`: the Databases screen on a made-up table of that many
//! rows (ADR 0028, "Window, 100,000 rows"). A made-up source answers the
//! window's page requests the way the bridge would, at once, so the
//! measurement needs no daemon. After the idle time the run clicks into
//! the grid, then for each of up to 200 pages turns the wheel down the page
//! and asks for the next one (Alt with the arrow toward the end). The peak
//! resident memory is sampled as it goes; at the end the window leaves the
//! screen, and its memory is read again a few seconds later.

use std::time::Duration;

use eframe::egui;
use marsa_core::db_browser::PAGE_ROWS;
use marsa_core::types::{
    DbCell, DbColumnKind, DbDatabase, DbPage, DbPageColumn, DbServer, DbServerInfo, DbSource,
    DbSources, DbTableInfo, DbTableKind, DbTableRef, DbTables, DbTotal, ServiceState,
};

use crate::bridge::Command;
use crate::bridge::{DbCommand, DbUpdate};
use crate::model::{Model, Route, Update};

/// The most pages the run walks.
pub(super) const MAX_PAGES: u64 = 200;
/// Frames of the wheel on each page, and how far each one moves: about
/// the 500 rows of a page.
pub(super) const WHEEL_FRAMES: u32 = 6;
const WHEEL_STEP: f32 = 2_700.0;
/// How long the window rests after leaving the screen, before its memory
/// is read.
pub(super) const AFTER_LEAVING: Duration = Duration::from_secs(3);
/// How often, in frames of the walk, the memory is sampled.
pub(super) const SAMPLE_EVERY: u32 = 30;

/// The made-up table.
pub(super) fn table() -> DbTableRef {
    DbTableRef {
        schema: None,
        name: "events".into(),
    }
}

fn source() -> DbSource {
    DbSource::Server {
        server: DbServer::Mysql,
        database: "shop".into(),
    }
}

/// The screen with a running MySQL, its database `shop`, and the table.
pub(super) fn fill(model: &mut Model, rows: u64) {
    model.route = Route::Databases;
    let db = &mut model.session.db;
    db.sources = Some(DbSources {
        servers: vec![DbServerInfo {
            server: DbServer::Mysql,
            version: "8.4.3".into(),
            state: ServiceState::Running,
            databases: vec![DbDatabase {
                name: "shop".into(),
            }],
            problem: None,
        }],
        files: Vec::new(),
        valkey: None,
    });
    db.chosen = Some(source());
    db.tables = Some(DbTables {
        tables: vec![DbTableInfo {
            table: table(),
            kind: DbTableKind::Table,
            estimate: Some(rows),
        }],
    });
}

/// The pages the run walks for a table of `rows` rows: the first is shown
/// when it opens, the others one by one.
pub(super) fn pages(rows: u64) -> u64 {
    rows.div_ceil(PAGE_ROWS).saturating_sub(1).min(MAX_PAGES)
}

/// The made-up source's answer to `command`: `None` when it is not one of
/// its own, `Some(None)` when it needs no answer.
pub(super) fn answer(command: &Command, rows: u64) -> Option<Option<Update>> {
    let Command::Db(command) = command else {
        return None;
    };
    Some(match command {
        DbCommand::ReadRows { id, request } => Some(Update::Db(DbUpdate::Rows {
            id: *id,
            result: Ok(page(request.offset, rows)),
        })),
        _ => None,
    })
}

/// The events of frame `at` of the walk: a click into the grid, its
/// release, then for each page the wheel and the next page.
pub(super) fn events(at: u32, input: &mut egui::RawInput, point: egui::Pos2) {
    let button = |pressed| egui::Event::PointerButton {
        pos: point,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let event = match at {
        0 => {
            input.events.push(egui::Event::PointerMoved(point));
            button(true)
        }
        1 => button(false),
        n if (n - 2) % (WHEEL_FRAMES + 1) < WHEEL_FRAMES => egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -WHEEL_STEP),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
            source: egui::MouseWheelSource::Wheel,
        },
        _ => egui::Event::Key {
            key: egui::Key::ArrowRight,
            physical_key: Some(egui::Key::ArrowRight),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::ALT,
        },
    };
    input.events.push(event);
}

/// The frames of the walk.
pub(super) fn frames(rows: u64) -> u32 {
    let pages = u32::try_from(pages(rows)).unwrap_or(u32::MAX);
    2 + pages * (WHEEL_FRAMES + 1)
}

/// The page from `offset`: an id, a name, an e-mail, a long text cut as the
/// daemon cuts it, a time, and a binary column, as a Laravel table might.
fn page(offset: u64, rows: u64) -> DbPage {
    let column = |name: &str, kind| DbPageColumn {
        name: name.into(),
        kind,
    };
    let end = (offset + PAGE_ROWS).min(rows);
    DbPage {
        columns: vec![
            column("id", DbColumnKind::Number),
            column("name", DbColumnKind::Text),
            column("email", DbColumnKind::Text),
            column("payload", DbColumnKind::Text),
            column("created_at", DbColumnKind::Time),
            column("avatar", DbColumnKind::Binary),
        ],
        key: vec![0],
        rows: (offset..end)
            .map(|n| {
                vec![
                    DbCell::Text((n + 1).to_string()),
                    DbCell::Text(format!("Customer {n}")),
                    DbCell::Text(format!("customer{n}@example.test")),
                    DbCell::Cut {
                        text: format!("{{\"order\":{n},\"lines\":[") + &"x".repeat(200),
                        bytes: 4_096 + n % 1_000,
                    },
                    DbCell::Text(format!("2026-11-{:02} 10:{:02}:00", n % 28 + 1, n % 60)),
                    if n % 3 == 0 {
                        DbCell::Null
                    } else {
                        DbCell::Binary { bytes: 14_200 }
                    },
                ]
            })
            .collect(),
        offset,
        total: DbTotal { rows, exact: true },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_500_rows_from_its_offset_and_the_last_one_is_short() {
        let first = page(0, 100_000);
        assert_eq!(first.rows.len(), 500);
        assert_eq!(first.rows[0][0], DbCell::Text("1".into()));
        let last = page(99_800, 100_000);
        assert_eq!(last.rows.len(), 200);
        assert!(last.total.exact);
        assert_eq!(pages(100_000), 199);
        assert_eq!(pages(1_000_000), MAX_PAGES);
        assert_eq!(pages(10), 0);
    }

    #[test]
    fn the_source_answers_page_reads_and_nothing_else() {
        let request = marsa_core::types::DbRowsRequest {
            source: source(),
            table: table(),
            offset: 500,
            sort: None,
            count: false,
        };
        let read = Command::Db(DbCommand::ReadRows { id: 7, request });
        let Some(Some(Update::Db(DbUpdate::Rows {
            id: 7,
            result: Ok(page),
        }))) = answer(&read, 100_000)
        else {
            panic!("no page");
        };
        assert_eq!(page.offset, 500);
        assert!(matches!(
            answer(&Command::Db(DbCommand::Close), 100_000),
            Some(None)
        ));
        assert!(answer(&Command::Park("/code".into()), 100_000).is_none());
    }

    #[test]
    fn the_walk_clicks_once_then_turns_the_wheel_and_asks_for_the_next_page() {
        let point = egui::pos2(700.0, 400.0);
        let events_at = |at| {
            let mut input = egui::RawInput::default();
            events(at, &mut input, point);
            input.events
        };
        assert!(matches!(
            events_at(0)[1],
            egui::Event::PointerButton { pressed: true, .. }
        ));
        assert!(matches!(
            events_at(1)[0],
            egui::Event::PointerButton { pressed: false, .. }
        ));
        assert!(matches!(events_at(2)[0], egui::Event::MouseWheel { .. }));
        let next = 2 + WHEEL_FRAMES;
        assert!(matches!(
            events_at(next)[0],
            egui::Event::Key { key: egui::Key::ArrowRight, modifiers, .. } if modifiers.alt
        ));
        assert_eq!(frames(100_000), 2 + 199 * (WHEEL_FRAMES + 1));
    }
}
