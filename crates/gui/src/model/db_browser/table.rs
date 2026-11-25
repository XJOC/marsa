//! The open table (layout.md, "Tables view, a table chosen"): one page of
//! its rows, the sort, the current cell, the value pane, and its columns.
//! The window keeps one page: a new one replaces it. Sorting and paging are
//! reads, never done here. Leaving the screen drops the page and the
//! columns; the chosen table and its sort stay for the next visit.

use marsa_core::db_browser::PAGE_ROWS;
use marsa_core::types::{
    DbCell, DbCellRequest, DbPage, DbRowsRequest, DbSort, DbSource, DbTable, DbTableRef,
};

use super::super::*;
use crate::bridge::{DbCommand, DbUpdate};

/// What the user did on the open table. The screen turns the grid's events
/// into these; the model knows nothing of the widget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbAction {
    /// A table of the list: its first page is read.
    ChooseTable(DbTableRef),
    Tab(DbTab),
    /// The current cell moved: a row of the page, or `None` for the header.
    Current {
        row: Option<usize>,
        column: usize,
    },
    /// A header: ascending, then descending, then the table's own order.
    Sort(usize),
    /// Enter or Space on a cell: the value pane opens or closes.
    TogglePane,
    ClosePane,
    /// Cmd or Ctrl C, or "Copy": the current value goes to the clipboard.
    Copy,
    NextPage,
    PreviousPage,
    /// The page and its count, or the columns, are read again.
    Refresh,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DbTab {
    #[default]
    Rows,
    Columns,
}

/// The table the user opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTable {
    pub table: DbTableRef,
    pub tab: DbTab,
    /// The page shown. While the next one loads it stays.
    pub page: Option<DbPage>,
    /// Grows with each page that arrives: the screen measures the column
    /// widths once for each page, not on each frame.
    pub serial: u64,
    /// The offset of the page being read, if any.
    pub loading: Option<u64>,
    /// The sorted column, by name, and whether it is descending.
    pub sort: Option<(String, bool)>,
    /// A row of the page, or `None` for the header, and a column.
    pub current: (Option<usize>, usize),
    pub pane: bool,
    /// The whole value of a cut cell, read for the pane.
    pub value: Option<PaneValue>,
    pub columns: Option<DbTable>,
    /// Why the last read failed: a client error code.
    pub error: Option<String>,
}

/// The whole value of the cell at `row`, `column`: `None` while it is read,
/// or when the read failed (`error`, a client error code).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneValue {
    pub row: usize,
    pub column: usize,
    pub cell: Option<DbCell>,
    pub error: Option<String>,
}

impl OpenTable {
    /// The table, with nothing read yet.
    pub(crate) fn closed(table: DbTableRef) -> Self {
        Self {
            table,
            tab: DbTab::Rows,
            page: None,
            serial: 0,
            loading: None,
            sort: None,
            current: (None, 0),
            pane: false,
            value: None,
            columns: None,
            error: None,
        }
    }

    /// The page after this one, if there is one: a full page with more rows
    /// after it, by the count or, for an estimate, by its size.
    pub fn next_offset(&self) -> Option<u64> {
        let page = self.page.as_ref()?;
        let end = page.offset + page.rows.len() as u64;
        let more = if page.total.exact {
            end < page.total.rows
        } else {
            page.rows.len() as u64 == PAGE_ROWS
        };
        more.then_some(end)
    }

    pub fn previous_offset(&self) -> Option<u64> {
        let page = self.page.as_ref()?;
        (page.offset > 0).then(|| page.offset.saturating_sub(PAGE_ROWS))
    }

    /// The cell the pane shows: the whole value once read, else the page's.
    pub fn pane_cell(&self) -> Option<&DbCell> {
        let (Some(row), column) = self.current else {
            return None;
        };
        let read = self
            .value
            .as_ref()
            .filter(|v| (v.row, v.column) == (row, column));
        if let Some(cell) = read.and_then(|value| value.cell.as_ref()) {
            return Some(cell);
        }
        self.page.as_ref()?.rows.get(row)?.get(column)
    }

    /// The whole value of the current cell is being read.
    pub fn reading_value(&self) -> bool {
        self.value.as_ref().is_some_and(|value| {
            value.cell.is_none()
                && value.error.is_none()
                && self.current == (Some(value.row), value.column)
        })
    }

    /// The text "Copy" puts on the clipboard: what the pane shows.
    fn copy_text(&self) -> Option<String> {
        match self.pane_cell()? {
            DbCell::Null => Some(crate::strings::NULL_TAG.to_owned()),
            DbCell::Text(text) | DbCell::Cut { text, .. } => Some(text.clone()),
            DbCell::Binary { .. } => None,
        }
    }
}

impl Model {
    /// The open table's action. `Copy` is done by `apply_action`.
    pub(in crate::model) fn db_action(&mut self, action: DbAction) -> Option<Command> {
        match action {
            DbAction::ChooseTable(table) => self.choose_db_table(table),
            DbAction::Tab(tab) => {
                let open = self.session.db.open.as_mut()?;
                open.tab = tab;
                let read = tab == DbTab::Columns && open.columns.is_none();
                read.then(|| self.read_db_table()).flatten()
            }
            DbAction::Current { row, column } => {
                let open = self.session.db.open.as_mut()?;
                open.current = (row, column);
                self.read_pane_value()
            }
            DbAction::Sort(column) => self.sort_db_rows(column),
            DbAction::TogglePane => {
                let open = self.session.db.open.as_mut()?;
                open.pane = !open.pane && open.current.0.is_some();
                self.read_pane_value()
            }
            DbAction::ClosePane => {
                self.session.db.open.as_mut()?.pane = false;
                None
            }
            DbAction::Copy => None,
            DbAction::NextPage => {
                let offset = self.session.db.open.as_ref()?.next_offset()?;
                self.read_db_rows(offset, false)
            }
            DbAction::PreviousPage => {
                let offset = self.session.db.open.as_ref()?.previous_offset()?;
                self.read_db_rows(offset, false)
            }
            DbAction::Refresh => {
                let open = self.session.db.open.as_ref()?;
                match open.tab {
                    DbTab::Rows => {
                        let offset = open.page.as_ref().map_or(0, |page| page.offset);
                        self.read_db_rows(offset, true)
                    }
                    DbTab::Columns => self.read_db_table(),
                }
            }
        }
    }

    /// What "Copy" puts on the clipboard, if anything.
    pub(in crate::model) fn db_copy_text(&self) -> Option<String> {
        self.session.db.open.as_ref()?.copy_text()
    }

    fn choose_db_table(&mut self, table: DbTableRef) -> Option<Command> {
        let db = &mut self.session.db;
        if db.open.as_ref().is_some_and(|open| open.table == table) {
            return None;
        }
        db.open = Some(OpenTable::closed(table));
        self.read_db_rows(0, true)
    }

    fn sort_db_rows(&mut self, column: usize) -> Option<Command> {
        let open = self.session.db.open.as_mut()?;
        let name = open.page.as_ref()?.columns.get(column)?.name.clone();
        open.sort = match open.sort.take() {
            Some((sorted, false)) if sorted == name => Some((name, true)),
            Some((sorted, true)) if sorted == name => None,
            _ => Some((name, false)),
        };
        open.current = (None, column);
        self.read_db_rows(0, false)
    }

    /// Reads the page at `offset` of the open table; `count` asks for the
    /// exact total (when it opens, and on Refresh).
    pub(super) fn read_db_rows(&mut self, offset: u64, count: bool) -> Option<Command> {
        if !self.is_connected() {
            return None;
        }
        let db = &mut self.session.db;
        let source = db.chosen.clone()?;
        let id = db.next_id();
        let open = db.open.as_mut()?;
        open.loading = Some(offset);
        let request = DbRowsRequest {
            source,
            table: open.table.clone(),
            offset,
            sort: open
                .sort
                .clone()
                .map(|(column, descending)| DbSort { column, descending }),
            count,
        };
        db.rows_id = Some(id);
        Some(Command::Db(DbCommand::ReadRows { id, request }))
    }

    fn read_db_table(&mut self) -> Option<Command> {
        if !self.is_connected() {
            return None;
        }
        let db = &mut self.session.db;
        let source = db.chosen.clone()?;
        let table = db.open.as_ref()?.table.clone();
        let id = db.next_id();
        db.table_id = Some(id);
        Some(Command::Db(DbCommand::ReadTable { id, source, table }))
    }

    /// With the pane open on a cut cell of a table with a primary key, the
    /// whole value is read. A cut key cell cannot find its row: the pane
    /// then shows the cut text.
    fn read_pane_value(&mut self) -> Option<Command> {
        if !self.is_connected() {
            return None;
        }
        let db = &mut self.session.db;
        let source: DbSource = db.chosen.clone()?;
        let open = db.open.as_mut()?;
        let (Some(row), column) = open.current else {
            return None;
        };
        if !open.pane
            || open
                .value
                .as_ref()
                .is_some_and(|v| (v.row, v.column) == (row, column))
        {
            return None;
        }
        let page = open.page.as_ref()?;
        let cells = page.rows.get(row)?;
        if !matches!(cells.get(column), Some(DbCell::Cut { .. })) || page.key.is_empty() {
            return None;
        }
        let key = page
            .key
            .iter()
            .map(|index| match cells.get(*index as usize) {
                Some(DbCell::Text(text)) => Some(text.clone()),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        let request = DbCellRequest {
            source,
            table: open.table.clone(),
            column: page.columns.get(column)?.name.clone(),
            key,
        };
        open.value = Some(PaneValue {
            row,
            column,
            cell: None,
            error: None,
        });
        let id = db.next_id();
        db.value_id = Some(id);
        Some(Command::Db(DbCommand::ReadValue { id, request }))
    }

    /// An answer about the open table. One to a read the user moved on
    /// from is dropped.
    pub(super) fn db_table_update(&mut self, update: DbUpdate) {
        let db = &mut self.session.db;
        match update {
            DbUpdate::Rows { id, result } if db.rows_id == Some(id) => {
                db.rows_id = None;
                let Some(open) = db.open.as_mut() else {
                    return;
                };
                open.loading = None;
                match result {
                    Ok(page) => {
                        open.current = (
                            (!page.rows.is_empty()).then_some(0),
                            open.current.1.min(page.columns.len().saturating_sub(1)),
                        );
                        open.page = Some(page);
                        open.serial += 1;
                        open.value = None;
                        open.error = None;
                    }
                    Err(code) => open.error = Some(code),
                }
            }
            DbUpdate::Value { id, result } if db.value_id == Some(id) => {
                db.value_id = None;
                let Some(open) = db.open.as_mut() else {
                    return;
                };
                // On an error the pane keeps the page's cut text, and says why.
                if let Some(pane) = open.value.as_mut() {
                    match result {
                        Ok(value) => pane.cell = Some(value.cell),
                        Err(code) => pane.error = Some(code),
                    }
                }
            }
            DbUpdate::Table { id, result } if db.table_id == Some(id) => {
                db.table_id = None;
                let Some(open) = db.open.as_mut() else {
                    return;
                };
                match result {
                    Ok(table) => {
                        open.columns = Some(table);
                        open.error = None;
                    }
                    Err(code) => open.error = Some(code),
                }
            }
            _ => {}
        }
    }
}
