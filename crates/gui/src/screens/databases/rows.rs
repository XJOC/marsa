//! The Rows tab of an open table (layout.md, "Tables view, a table
//! chosen"): the data grid with the page bar as its card's last line, the
//! value pane under it, and a caption for a table with no primary key.
//!
//! The grid draws only the rows in view. The column widths are measured
//! once for each page (the model's `serial` tells a new page) and kept in
//! egui's memory, not on each frame.

use eframe::egui::{Align, CornerRadius, Layout, Sense, Ui, Vec2, vec2};
use marsa_core::types::{DbCell, DbPage};

use super::super::lines::message;
use super::pane::{self, Pane, PaneAction};
use crate::model::{Action, DbAction, OpenTable};
use crate::strings;
use crate::theme::page::DB_VALUE_HEIGHT;
use crate::theme::{Direction, Theme};
use crate::widgets::grid::{Grid, GridEvent, column_widths};
use crate::widgets::icons::{Glyph, Icon};
use crate::widgets::table::card;
use crate::widgets::{Button, Text};

pub(super) fn show(ui: &mut Ui, theme: &Theme, open: &OpenTable, actions: &mut Vec<Action>) {
    if let Some(code) = &open.error {
        message(ui, theme, &strings::error_text(code));
        return;
    }
    let gap = theme.space.s3;
    let pane = open.pane && open.pane_cell().is_some();
    let no_key = open.page.as_ref().is_some_and(|page| page.key.is_empty());
    // The grid's card takes what the pane and the caption leave.
    let mut below = 0.0;
    if pane {
        below += DB_VALUE_HEIGHT + gap;
    }
    if no_key {
        below += theme.size.control + gap;
    }
    ui.spacing_mut().item_spacing.y = gap;
    let height = (ui.available_height() - below).max(theme.size.table_header);
    let size = vec2(ui.available_width(), height);
    ui.allocate_ui_with_layout(size, Direction::CURRENT.column(), |ui| {
        card(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            ui.set_min_height(height);
            let rows = height - theme.size.grid_footer;
            ui.allocate_ui_with_layout(vec2(ui.available_width(), rows), column(), |ui| {
                ui.set_min_height(rows);
                if let Some(page) = &open.page {
                    grid(ui, theme, open, page, actions);
                }
            });
            page_bar(ui, theme, open, actions);
        });
    });
    if pane {
        value_pane(ui, theme, open, actions);
    }
    if no_key {
        ui.add(Text::new(
            strings::DB_NO_KEY,
            theme.text.caption,
            theme.color.mute,
        ));
    }
}

fn column() -> Layout {
    Direction::CURRENT.column()
}

fn grid(ui: &mut Ui, theme: &Theme, open: &OpenTable, page: &DbPage, actions: &mut Vec<Action>) {
    let id = ui.make_persistent_id(("db_grid_widths", open.serial, &open.table));
    let widths = match ui.data(|data| data.get_temp::<Vec<f32>>(id)) {
        Some(widths) => widths,
        None => {
            let widths = column_widths(ui, &page.columns, &page.rows);
            ui.data_mut(|data| data.insert_temp(id, widths.clone()));
            widths
        }
    };
    let sorted = open.sort.as_ref().and_then(|(name, descending)| {
        let index = page.columns.iter().position(|c| &c.name == name)?;
        Some((index, *descending))
    });
    if page.rows.is_empty() {
        let header = vec2(ui.available_width(), theme.size.table_header);
        ui.allocate_ui_with_layout(header, column(), |ui| {
            Grid::new("db_rows", &page.columns, &page.rows, &widths, open.current)
                .sortable(sorted)
                .show(ui)
        });
        ui.add_space(theme.space.s4);
        ui.vertical_centered(|ui| {
            ui.add(Text::new(
                strings::DB_NO_ROWS,
                theme.text.body,
                theme.color.ink2,
            ));
        });
        return;
    }
    let events = Grid::new("db_rows", &page.columns, &page.rows, &widths, open.current)
        .sortable(sorted)
        .show(ui);
    for event in events {
        actions.push(Action::Db(match event {
            GridEvent::Current { row, column } => DbAction::Current { row, column },
            GridEvent::Sort(column) => DbAction::Sort(column),
            GridEvent::TogglePane => DbAction::TogglePane,
            GridEvent::ClosePane => DbAction::ClosePane,
            GridEvent::Copy => DbAction::Copy,
            GridEvent::NextPage => DbAction::NextPage,
            GridEvent::PreviousPage => DbAction::PreviousPage,
        }));
    }
}

/// The card's last line: the range at the start, Previous and Next at the
/// end; while a page loads, a still arc and the rows it reads.
fn page_bar(ui: &mut Ui, theme: &Theme, open: &OpenTable, actions: &mut Vec<Action>) {
    let width = ui.available_width();
    let line = vec2(width, theme.size.border);
    let (rect, _) = ui.allocate_exact_size(line, Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, theme.color.line);
    let height = theme.size.grid_footer - theme.size.border;
    let pad = theme.space.s3;
    ui.allocate_ui_with_layout(vec2(width, height), Direction::CURRENT.row(), |ui| {
        ui.add_space(pad);
        let text = match (open.loading, &open.page) {
            (Some(offset), _) => {
                let last = offset + marsa_core::db_browser::PAGE_ROWS;
                let total = open.page.as_ref().map_or(last, |page| page.total.rows);
                strings::page_loading(offset + 1, last.min(total.max(offset + 1)))
            }
            (None, Some(page)) if !page.rows.is_empty() => {
                let last = page.offset + page.rows.len() as u64;
                strings::page_range(page.offset + 1, last, page.total.rows, page.total.exact)
            }
            _ => String::new(),
        };
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            if open.loading.is_some() {
                ui.add(Glyph::new(Icon::Spinner).color(theme.color.mute));
            }
            ui.add(Text::new(&text, theme.text.body_sm, theme.color.mute));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(pad);
            ui.spacing_mut().item_spacing.x = theme.space.s2;
            let next = Button::secondary(strings::DB_NEXT)
                .in_row()
                .leading(Icon::Arrow)
                .disabled_if(open.next_offset().is_none(), strings::DB_LAST_PAGE);
            if ui.add(next).clicked() {
                actions.push(Action::Db(DbAction::NextPage));
            }
            let previous = Button::secondary(strings::DB_PREVIOUS)
                .in_row()
                .leading(Icon::Back)
                .disabled_if(open.previous_offset().is_none(), strings::DB_FIRST_PAGE);
            if ui.add(previous).clicked() {
                actions.push(Action::Db(DbAction::PreviousPage));
            }
        });
    });
}

/// The current cell's whole value: its column, kind and size, then what is
/// still read or cut.
fn value_pane(ui: &mut Ui, theme: &Theme, open: &OpenTable, actions: &mut Vec<Action>) {
    let Some(page) = &open.page else {
        return;
    };
    let Some(cell) = open.pane_cell() else {
        return;
    };
    let shown = page.columns.get(open.current.1);
    let bytes = match cell {
        DbCell::Text(text) => Some(text.len() as u64),
        DbCell::Cut { bytes, .. } | DbCell::Binary { bytes } => Some(*bytes),
        DbCell::Null => None,
    };
    let pane = Pane {
        title: shown.map_or("", |column| column.name.as_str()),
        caption: shown
            .zip(bytes)
            .map(|(column, bytes)| strings::value_caption(column.kind, bytes)),
        cell,
        height: DB_VALUE_HEIGHT,
        closes: true,
    };
    match pane::show(ui, theme, pane, |ui| value_notes(ui, theme, open)) {
        Some(PaneAction::Copy) => actions.push(Action::Db(DbAction::Copy)),
        Some(PaneAction::Close) => actions.push(Action::Db(DbAction::ClosePane)),
        None => {}
    }
}

/// Under the value: the whole value still read, why it was not, or its cut.
fn value_notes(ui: &mut Ui, theme: &Theme, open: &OpenTable) {
    let read = open
        .value
        .as_ref()
        .filter(|v| (Some(v.row), v.column) == open.current);
    if open.reading_value() {
        ui.add(Text::new(
            strings::DB_READING_VALUE,
            theme.text.caption,
            theme.color.mute,
        ));
    } else if let Some(code) = read.and_then(|value| value.error.as_deref()) {
        let text = strings::error_text(code);
        ui.add(Text::new(&text, theme.text.caption, theme.color.mute));
    }
    // Only the read value is cut at 64 KB; a page's cell is cut shorter.
    let whole = read.and_then(|value| value.cell.as_ref());
    if let Some(DbCell::Cut { bytes, .. }) = whole {
        let cut = strings::value_cut(*bytes);
        ui.add(Text::new(&cut, theme.text.caption, theme.color.mute));
    }
}
