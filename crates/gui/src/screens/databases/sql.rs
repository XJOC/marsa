//! The SQL view (layout.md, "SQL view"): the editor, the run bar, and the
//! result. Read-only: the daemon refuses a statement that changes data.
//!
//! The text of each source lives here, in the window's memory, until the
//! window closes; it is never written to disk or logged. Only the statement
//! to run goes to the model: a key press copies nothing.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

use eframe::egui::{Align, Layout, Ui, Vec2, vec2};
use marsa_core::codes;
use marsa_core::db_browser::{Engine, SQL_BYTES, statement_at};
use marsa_core::types::{DbCell, DbResult, DbSource};

use super::super::lines::message;
use super::pane;
use crate::model::{Action, DbAction, QueryRun, QueryState};
use crate::strings;
use crate::theme::page::SQL_EDITOR_LINES;
use crate::theme::{Direction, Theme};
use crate::widgets::editor::buffer::Buffer;
use crate::widgets::editor::{Editor, EditorOutput};
use crate::widgets::grid::{Grid, GridEvent, column_widths};
use crate::widgets::icons::{Glyph, Icon};
use crate::widgets::table::card;
use crate::widgets::tag::Tag;
use crate::widgets::{Button, Text};

/// The texts of the sources, and whether the last paste was refused.
#[derive(Clone, Default)]
struct Texts {
    texts: HashMap<DbSource, Buffer>,
    refused: bool,
}

pub(super) fn show(
    ui: &mut Ui,
    theme: &Theme,
    source: &DbSource,
    run: Option<&QueryRun>,
    seed: Option<&str>,
    actions: &mut Vec<Action>,
) {
    let engine = engine_of(source);
    let id = ui.make_persistent_id("db_sql_texts");
    let mut texts = ui
        .data_mut(|data| data.remove_temp::<Texts>(id))
        .unwrap_or_default();
    let buffer = texts
        .texts
        .entry(source.clone())
        .or_insert_with(|| Buffer::new(seed.unwrap_or_default(), SQL_BYTES));
    ui.spacing_mut().item_spacing.y = theme.space.s3;
    let output = Editor::new(
        "db_sql",
        strings::DB_EDITOR,
        buffer,
        engine,
        SQL_EDITOR_LINES,
    )
    .keyed(key_of(source))
    .show(ui);
    let running = run.is_some_and(QueryRun::running);
    let statement = (output.run || ran(ui, theme, run, actions))
        .then(|| statement(buffer, engine))
        .flatten();
    if let Some(sql) = statement {
        actions.push(Action::Db(DbAction::Run(sql)));
    }
    if output.cancel && running {
        actions.push(Action::Db(DbAction::Cancel));
    }
    note_refused(&mut texts, output);
    if texts.refused {
        let text = strings::sql_too_long();
        ui.add(Text::new(&text, theme.text.caption, theme.color.error));
    }
    ui.data_mut(|data| data.insert_temp(id, texts));
    result(ui, theme, source, run);
}

fn note_refused(texts: &mut Texts, output: EditorOutput) {
    if output.refused {
        texts.refused = true;
    } else if output.changed {
        texts.refused = false;
    }
}

/// The run bar: Run or Cancel, the Read-only tag, the hint, and at the end
/// the outcome. `true` when Run was clicked.
fn ran(ui: &mut Ui, theme: &Theme, run: Option<&QueryRun>, actions: &mut Vec<Action>) -> bool {
    let running = run.is_some_and(QueryRun::running);
    let line = vec2(ui.available_width(), theme.size.control);
    let mut clicked = false;
    ui.allocate_ui_with_layout(line, Direction::CURRENT.row(), |ui| {
        ui.spacing_mut().item_spacing.x = theme.space.s3;
        if running {
            let cancelling = run.is_some_and(|run| run.state == QueryState::Cancelling);
            let cancel = Button::secondary(strings::DB_CANCEL)
                .disabled_if(cancelling, strings::DB_CANCELLED);
            if ui.add(cancel).clicked() {
                actions.push(Action::Db(DbAction::Cancel));
            }
        } else {
            clicked = ui.add(Button::primary(strings::DB_RUN)).clicked();
        }
        ui.add(Tag::new(strings::DB_READ_ONLY_TAG, theme));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            outcome(ui, theme, run);
            if !running {
                let mac = ui.ctx().os() == eframe::egui::os::OperatingSystem::Mac;
                let hint = strings::run_hint(mac);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(Text::new(hint, theme.text.body_sm, theme.color.mute).truncate());
                });
            }
        });
    });
    clicked
}

/// The end of the run bar: "8 rows in 4 ms", "Running for 12 s…" with a
/// still arc (the text changes once a second; nothing else repaints), or
/// "Cancelled.".
fn outcome(ui: &mut Ui, theme: &Theme, run: Option<&QueryRun>) {
    let Some(run) = run else {
        return;
    };
    let text = match &run.state {
        QueryState::Running | QueryState::Cancelling => {
            let elapsed = run.started.elapsed();
            let next = Duration::from_secs(elapsed.as_secs() + 1).saturating_sub(elapsed);
            ui.ctx().request_repaint_after(next);
            strings::running_for(elapsed.as_secs())
        }
        QueryState::Done(result) => strings::rows_in(result.rows.len(), result.elapsed_ms),
        QueryState::Cancelled => strings::DB_CANCELLED.to_owned(),
        QueryState::Failed { .. } => return,
    };
    ui.add(Text::new(&text, theme.text.body_sm, theme.color.ink2));
    if run.running() {
        ui.add(Glyph::new(Icon::Spinner).color(theme.color.mute));
    }
}

/// Under the run bar: the rows, the refused statement's banner, or nothing
/// before the first run.
fn result(ui: &mut Ui, theme: &Theme, source: &DbSource, run: Option<&QueryRun>) {
    let Some(run) = run else {
        return;
    };
    match &run.state {
        QueryState::Running | QueryState::Cancelling => message(ui, theme, strings::DB_ROWS_LATER),
        QueryState::Cancelled => {}
        QueryState::Done(result) if result.columns.is_empty() => {
            message(ui, theme, strings::DB_DONE_NO_ROWS);
        }
        QueryState::Done(result) => rows(ui, theme, result),
        QueryState::Failed { code, detail } => refused(ui, theme, source, code, detail.as_deref()),
    }
}

/// The result in the grid, with no sort and no pages, and a caption when
/// it was cut.
fn rows(ui: &mut Ui, theme: &Theme, result: &DbResult) {
    let caption = if result.cut { theme.size.control } else { 0.0 };
    let height = (ui.available_height() - caption).max(theme.size.table_header);
    let id = ui.make_persistent_id((
        "db_result_widths",
        result.rows.len(),
        result.elapsed_ms,
        result.columns.len(),
    ));
    let widths = match ui.data(|data| data.get_temp::<Vec<f32>>(id)) {
        Some(widths) => widths,
        None => {
            let widths = column_widths(ui, &result.columns, &result.rows);
            ui.data_mut(|data| data.insert_temp(id, widths.clone()));
            widths
        }
    };
    let cell_id = ui.make_persistent_id("db_result_cell");
    let current = ui.data(|data| data.get_temp(cell_id)).unwrap_or((None, 0));
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), height),
        Direction::CURRENT.column(),
        |ui| {
            card(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                ui.set_min_height(height);
                let events =
                    Grid::new("db_result", &result.columns, &result.rows, &widths, current)
                        .show(ui);
                for event in events {
                    match event {
                        GridEvent::Current { row, column } => {
                            ui.data_mut(|data| data.insert_temp(cell_id, (row, column)));
                        }
                        GridEvent::Copy => copy(ui, result, current),
                        _ => {}
                    }
                }
            });
        },
    );
    if result.cut {
        ui.add(Text::new(
            strings::DB_RESULT_CUT,
            theme.text.caption,
            theme.color.mute,
        ));
    }
}

fn copy(ui: &Ui, result: &DbResult, current: (Option<usize>, usize)) {
    let (Some(row), column) = current else {
        return;
    };
    let text = match result.rows.get(row).and_then(|cells| cells.get(column)) {
        Some(DbCell::Text(text) | DbCell::Cut { text, .. }) => text.clone(),
        Some(DbCell::Null) => strings::NULL_TAG.to_owned(),
        Some(DbCell::Binary { .. }) | None => return,
    };
    ui.ctx().copy_text(text);
}

/// A refused statement: one sentence of ours, then the server's own words.
fn refused(ui: &mut Ui, theme: &Theme, source: &DbSource, code: &str, detail: Option<&str>) {
    let sentence = match code {
        codes::DB_QUERY_FAILED => strings::query_failed(source),
        codes::DB_TIMEOUT => strings::DB_QUERY_TIMEOUT.to_owned(),
        code => strings::error_text(code),
    };
    pane::banner(ui, theme, &sentence, detail);
}

/// What Run sends: the selection when there is one, else the statement at
/// the cursor (statements end at `;`).
pub(crate) fn statement(buffer: &Buffer, engine: Engine) -> Option<String> {
    if let Some(selected) = buffer.selected_text()
        && !selected.trim().is_empty()
    {
        return Some(selected);
    }
    let text = buffer.text();
    let range = statement_at(engine, &text, buffer.offset(buffer.cursor()))?;
    let sql = text[range].trim();
    (!sql.is_empty()).then(|| sql.to_owned())
}

pub(crate) fn engine_of(source: &DbSource) -> Engine {
    match source {
        DbSource::Server { server, .. } => Engine::from(*server),
        DbSource::SiteFile { .. } => Engine::Sqlite,
    }
}

/// Each source's text keeps its own layout in the editor.
fn key_of(source: &DbSource) -> u64 {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use marsa_core::types::DbServer;

    use super::*;
    use crate::widgets::editor::buffer::Pos;

    #[test]
    fn run_sends_the_selection_or_else_the_statement_at_the_cursor() {
        let mut buffer = Buffer::new("select 1;\nselect 2;\n\nselect 3", SQL_BYTES);
        buffer.set_cursor(Pos { line: 1, col: 3 }, false);
        assert_eq!(
            statement(&buffer, Engine::Mysql).as_deref(),
            Some("select 2")
        );
        buffer.move_cursor(crate::widgets::editor::buffer::Move::Bottom, false);
        assert_eq!(
            statement(&buffer, Engine::Mysql).as_deref(),
            Some("select 3")
        );
        buffer.set_cursor(Pos { line: 0, col: 0 }, false);
        buffer.set_cursor(Pos { line: 0, col: 8 }, true);
        assert_eq!(
            statement(&buffer, Engine::Mysql).as_deref(),
            Some("select 1")
        );
        assert_eq!(
            statement(&Buffer::new("  \n", SQL_BYTES), Engine::Mysql),
            None
        );
    }

    #[test]
    fn a_site_file_speaks_sqlite_and_a_server_its_own() {
        let file = DbSource::SiteFile {
            site: "blog".into(),
        };
        assert_eq!(engine_of(&file), Engine::Sqlite);
        let postgres = DbSource::Server {
            server: DbServer::Postgres,
            database: "app".into(),
        };
        assert_eq!(engine_of(&postgres), Engine::Postgres);
        assert_ne!(key_of(&file), key_of(&postgres));
    }
}
