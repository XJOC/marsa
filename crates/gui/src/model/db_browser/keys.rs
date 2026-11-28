//! Valkey's keys on the Databases screen (layout.md, "Valkey keys"): the
//! keys of one database, read page by page with the search and the type, and
//! the chosen key's value. Leaving the screen drops what was read; the
//! database, the search, the type, and the open key's name stay.

use std::borrow::Cow;
use std::collections::HashSet;

use marsa_core::db_browser::key_pattern;
use marsa_core::types::{
    DbCell, DbKeyDetail, DbKeyKind, DbKeyRequest, DbKeyValue, DbKeys, DbKeysRequest, DbValkeyInfo,
};

use super::super::*;
use super::DbScreen;
use crate::bridge::{DbCommand, DbUpdate};

/// The keys the list holds at most: past it the user narrows the search.
pub const KEY_LIST_LIMIT: usize = 5_000;

/// What the user did on the keys. As [`super::DbAction`] for a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeysAction {
    /// A database of the source menu: its keys are read.
    Database(u8),
    /// "Find a key" changed. Nothing is read until Enter.
    Search(String),
    /// Enter in "Find a key".
    Submit,
    /// The type menu; `None` is every type.
    Kind(Option<DbKeyKind>),
    /// "More keys".
    More,
    /// A key of the list: its value is read.
    Open(String),
    /// The current cell of the value's grid moved.
    Current {
        row: Option<usize>,
        column: usize,
    },
    TogglePane,
    ClosePane,
    Copy,
    /// The list and the open key are read again.
    Refresh,
}

/// Why a read failed: a client error code, and Valkey's own words.
pub type ReadError = (String, Option<String>);

/// The chosen database of Valkey.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyBrowser {
    pub db: u8,
    /// What "Find a key" holds.
    pub search: String,
    pub kind: Option<DbKeyKind>,
    /// The keys, once the first page came. While the list is read again, the
    /// old one stays.
    pub list: Option<KeyList>,
    /// A page of keys is being read.
    pub loading: bool,
    /// The key the user opened.
    pub open: Option<OpenKey>,
    /// Why the last read of the list failed.
    pub error: Option<ReadError>,
    /// The pattern and the type the list is read with: "More keys" goes on
    /// with them, whatever the field holds by then.
    sent: (Option<String>, Option<DbKeyKind>),
}

/// The keys read so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyList {
    pub names: Vec<String>,
    /// Where the next page starts; 0 when Valkey has no more.
    pub cursor: u64,
    /// Keys left out: names that cannot be shown.
    pub unshown: u64,
    /// Every key of the database.
    pub total: u64,
}

impl KeyList {
    /// "More keys" shows.
    pub fn more(&self) -> bool {
        self.cursor != 0 && self.names.len() < KEY_LIST_LIMIT
    }

    /// The list is at its limit and Valkey has more.
    pub fn full(&self) -> bool {
        self.cursor != 0 && self.names.len() >= KEY_LIST_LIMIT
    }

    /// A page after this one. `SCAN` may give a key twice while the database
    /// grows: it is listed once.
    fn append(&mut self, page: DbKeys) {
        let seen: HashSet<&str> = self.names.iter().map(String::as_str).collect();
        let new: Vec<String> = page
            .keys
            .into_iter()
            .filter(|name| !seen.contains(name.as_str()))
            .collect();
        self.names.extend(new);
        self.names.truncate(KEY_LIST_LIMIT);
        self.cursor = page.cursor;
        self.unshown += page.unshown;
        self.total = page.total;
    }
}

/// The key the user opened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenKey {
    pub name: String,
    /// What was read. While it is read again, the old one stays.
    pub detail: Option<DbKeyDetail>,
    /// Grows with each value that arrives: the screen measures the grid once
    /// for each.
    pub serial: u64,
    pub loading: bool,
    /// A row of the value's grid, or `None` for its header, and a column.
    pub current: (Option<usize>, usize),
    pub pane: bool,
    pub error: Option<ReadError>,
}

impl OpenKey {
    fn named(name: String) -> Self {
        Self {
            name,
            ..Self::default()
        }
    }

    /// The rows of the value's grid; one for a string.
    pub fn rows(&self) -> usize {
        match self.detail.as_ref().map(|detail| &detail.value) {
            Some(DbKeyValue::String { .. }) => 1,
            Some(DbKeyValue::Hash { fields, .. }) => fields.len(),
            Some(DbKeyValue::List { items, .. }) => items.len(),
            Some(DbKeyValue::Set { members, .. }) => members.len(),
            Some(DbKeyValue::Zset { members, .. }) => members.len(),
            Some(DbKeyValue::Other { .. }) | None => 0,
        }
    }

    /// The cell at `row` and `column` of the value's grid: a hash's field and
    /// value, a list's index and item, a set's member, a sorted set's member
    /// and score. A string is the one cell.
    pub fn cell(&self, row: usize, column: usize) -> Option<Cow<'_, DbCell>> {
        let text = |text: String| Some(Cow::Owned(DbCell::Text(text)));
        match &self.detail.as_ref()?.value {
            DbKeyValue::String { value } => (row == 0).then_some(Cow::Borrowed(value)),
            DbKeyValue::Hash { fields, .. } => {
                let (field, value) = fields.get(row)?;
                [field, value].get(column).map(|cell| Cow::Borrowed(*cell))
            }
            DbKeyValue::List { items, .. } => {
                let item = items.get(row)?;
                match column {
                    0 => text(row.to_string()),
                    _ => Some(Cow::Borrowed(item)),
                }
            }
            DbKeyValue::Set { members, .. } => members.get(row).map(Cow::Borrowed),
            DbKeyValue::Zset { members, .. } => {
                let (member, score) = members.get(row)?;
                match column {
                    0 => Some(Cow::Borrowed(member)),
                    _ => text(score.clone()),
                }
            }
            DbKeyValue::Other { .. } => None,
        }
    }

    /// The column whose cell is the value: where a new value's current cell
    /// starts.
    fn value_column(&self) -> usize {
        match self.detail.as_ref().map(|detail| &detail.value) {
            Some(DbKeyValue::Hash { .. } | DbKeyValue::List { .. }) => 1,
            _ => 0,
        }
    }

    /// What "Copy" puts on the clipboard: the current cell's text.
    fn copy_text(&self) -> Option<String> {
        let (row, column) = self.current;
        match self.cell(row?, column)?.as_ref() {
            DbCell::Text(text) | DbCell::Cut { text, .. } => Some(text.clone()),
            DbCell::Null | DbCell::Binary { .. } => None,
        }
    }
}

/// The screen of the keys, by what the list of sources says of Valkey.
pub(super) fn screen(info: &DbValkeyInfo) -> DbScreen {
    if info.databases.is_empty() {
        if let Some(problem) = info.problem.as_deref() {
            return if problem == marsa_core::codes::DB_SIGN_IN_FAILED {
                DbScreen::KeysSignInFailed
            } else {
                DbScreen::Failed(problem.to_owned())
            };
        }
        // Its list is read again once it runs (`db_sources_changed`).
        if info.state == ServiceState::Starting {
            return DbScreen::Loading;
        }
        if info.state != ServiceState::Running {
            return DbScreen::KeysStopped;
        }
    }
    DbScreen::Keys
}

impl Model {
    /// The user's action on the keys. `Copy` is done by `apply_action`.
    pub(in crate::model) fn keys_action(&mut self, action: KeysAction) -> Option<Command> {
        if let KeysAction::Database(db) = action {
            return self.choose_db_keys(db);
        }
        let keys = self.session.db.keys.as_mut()?;
        match action {
            KeysAction::Database(_) | KeysAction::Copy => None,
            KeysAction::Search(text) => {
                keys.search = text;
                None
            }
            KeysAction::Submit => self.search_keys(),
            KeysAction::Kind(kind) => {
                keys.kind = kind;
                self.search_keys()
            }
            KeysAction::More => {
                let more = keys.list.as_ref().is_some_and(KeyList::more);
                (more && !keys.loading)
                    .then(|| self.read_keys(true))
                    .flatten()
            }
            KeysAction::Open(name) => {
                let same = keys.open.as_ref().is_some_and(|open| open.name == name);
                if same {
                    return None;
                }
                keys.open = Some(OpenKey::named(name));
                self.read_key()
            }
            KeysAction::Current { row, column } => {
                keys.open.as_mut()?.current = (row, column);
                None
            }
            KeysAction::TogglePane => {
                let open = keys.open.as_mut()?;
                open.pane = !open.pane && open.current.0.is_some();
                None
            }
            KeysAction::ClosePane => {
                keys.open.as_mut()?.pane = false;
                None
            }
            KeysAction::Refresh => {
                let list = self.read_keys(false);
                // The key's read follows the list's answer.
                if let Some(Command::Db(command)) = self.read_key() {
                    self.session.db.pending = Some(command);
                }
                list
            }
        }
    }

    /// What "Copy" puts on the clipboard, if anything.
    pub(in crate::model) fn db_key_copy_text(&self) -> Option<String> {
        self.session.db.keys.as_ref()?.open.as_ref()?.copy_text()
    }

    /// Database `db` of Valkey becomes the source: its keys are read.
    fn choose_db_keys(&mut self, db: u8) -> Option<Command> {
        let state = &mut self.session.db;
        let shown = state.keys.as_ref();
        if shown.is_some_and(|keys| keys.db == db && keys.list.is_some()) {
            return None;
        }
        state.choose_keys(db);
        self.read_keys(false)
    }

    /// The search or the type changed: the list starts again with them.
    fn search_keys(&mut self) -> Option<Command> {
        let keys = self.session.db.keys.as_mut()?;
        keys.sent = (key_pattern(&keys.search), keys.kind);
        // The old keys answer another search.
        keys.list = None;
        self.read_keys(false)
    }

    /// Reads the first page of keys, or the one after the list (`more`).
    fn read_keys(&mut self, more: bool) -> Option<Command> {
        if !self.is_connected() || !self.session.db.valkey_runs() {
            return None;
        }
        let state = &mut self.session.db;
        let id = state.next_id();
        let keys = state.keys.as_mut()?;
        let cursor = keys
            .list
            .as_ref()
            .filter(|_| more)
            .map_or(0, |list| list.cursor);
        keys.loading = true;
        let request = DbKeysRequest {
            db: keys.db,
            pattern: keys.sent.0.clone(),
            kind: keys.sent.1,
            cursor,
        };
        state.keys_id = Some((id, cursor != 0));
        Some(Command::Db(DbCommand::ReadKeys { id, request }))
    }

    fn read_key(&mut self) -> Option<Command> {
        if !self.is_connected() || !self.session.db.valkey_runs() {
            return None;
        }
        let state = &mut self.session.db;
        let id = state.next_id();
        let keys = state.keys.as_mut()?;
        let db = keys.db;
        let open = keys.open.as_mut()?;
        open.loading = true;
        let request = DbKeyRequest {
            db,
            key: open.name.clone(),
        };
        state.key_id = Some(id);
        Some(Command::Db(DbCommand::ReadKey { id, request }))
    }

    /// An answer about the keys. One to a read the user moved on from is
    /// dropped.
    pub(super) fn db_keys_update(&mut self, update: DbUpdate) {
        let state = &mut self.session.db;
        match update {
            DbUpdate::Keys { id, result } => {
                let Some((_, more)) = state.keys_id.filter(|(read, _)| *read == id) else {
                    return;
                };
                state.keys_id = None;
                let Some(keys) = state.keys.as_mut() else {
                    return;
                };
                keys.loading = false;
                match result {
                    Ok(page) => {
                        keys.error = None;
                        if !more {
                            keys.list = Some(KeyList::default());
                        }
                        if let Some(list) = keys.list.as_mut() {
                            list.append(page);
                        }
                        self.reopen_db_key();
                    }
                    Err(error) => keys.error = Some(error),
                }
            }
            DbUpdate::Key { id, result } if state.key_id == Some(id) => {
                state.key_id = None;
                let Some(open) = state.keys.as_mut().and_then(|keys| keys.open.as_mut()) else {
                    return;
                };
                open.loading = false;
                match result {
                    Ok(detail) => {
                        open.detail = Some(detail);
                        open.serial += 1;
                        open.error = None;
                        let first = (open.rows() > 0).then_some(0);
                        open.current = (first, open.value_column());
                    }
                    Err(error) => {
                        open.detail = None;
                        open.error = Some(error);
                    }
                }
            }
            _ => {}
        }
    }

    /// Back on the screen, the open key is read again once its list came.
    fn reopen_db_key(&mut self) {
        let unread = self
            .session
            .db
            .keys
            .as_ref()
            .and_then(|keys| keys.open.as_ref())
            .is_some_and(|open| open.detail.is_none() && open.error.is_none() && !open.loading);
        if unread && let Some(Command::Db(command)) = self.read_key() {
            self.session.db.pending = Some(command);
        }
    }

    /// A new list of sources while the keys are chosen: what a stopped
    /// Valkey gave goes, and a running one's keys are read if not shown.
    pub(super) fn db_keys_sources_read(&mut self) {
        let state = &mut self.session.db;
        let runs = state.valkey_runs();
        let Some(keys) = state.keys.as_mut() else {
            return;
        };
        if !runs {
            keys.drop_read();
            state.keys_id = None;
            state.key_id = None;
        } else if keys.list.is_none()
            && !keys.loading
            && let Some(Command::Db(command)) = self.read_keys(false)
        {
            self.session.db.pending = Some(command);
        }
    }
}

impl KeyBrowser {
    pub(super) fn of(db: u8) -> Self {
        Self {
            db,
            ..Self::default()
        }
    }

    /// What was read goes; the database, the search, the type, and the open
    /// key's name stay.
    pub(super) fn drop_read(&mut self) {
        self.list = None;
        self.loading = false;
        self.error = None;
        if let Some(open) = self.open.as_mut() {
            *open = OpenKey::named(std::mem::take(&mut open.name));
        }
    }
}

#[cfg(test)]
mod tests;
