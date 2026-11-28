//! The Databases screen's state (ADR 0028): what can be opened, the source
//! the user chose, and its tables, or Valkey's keys ([`keys`]). No event carries them, so the screen
//! reads them as it opens, when the user picks a source, and when a server
//! or the site list changes while it is open. Leaving the screen drops the
//! lists and closes the daemon's connections; the chosen source stays.
//!
//! Every read has a number, and its answer carries it: an answer to a read
//! the user has moved on from is dropped.

use marsa_core::types::{DbServer, DbSource, DbSources, DbTables};

use super::*;
use crate::bridge::{DbCommand, DbUpdate};

mod keys;
mod query;
mod table;
pub use keys::{KEY_LIST_LIMIT, KeyBrowser, KeyList, KeysAction, OpenKey, ReadError};
pub use query::{QueryRun, QueryState};
pub use table::{DbAction, DbTab, OpenTable, PaneValue};

/// The detail of the screen: a source's tables, or its SQL.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DbView {
    #[default]
    Tables,
    Sql,
}

/// What the window knows of the databases. Part of the [`Session`]: a new
/// connection reads them again.
#[derive(Debug, Default)]
pub struct DbBrowserState {
    /// What can be opened, as last read. `None` until read, and after the
    /// screen was left.
    pub sources: Option<DbSources>,
    /// The source the user chose, or the first one the screen opened.
    pub chosen: Option<DbSource>,
    /// Set while the chosen source is a database of Valkey: then `chosen`
    /// is `None`.
    pub keys: Option<KeyBrowser>,
    /// The tables of `chosen`, once read.
    pub tables: Option<DbTables>,
    /// What "Find a table" holds: the list is filtered here, with no read.
    pub filter: String,
    /// Why the last read failed: a client error code.
    pub error: Option<String>,
    /// The table the user opened, of `chosen`.
    pub open: Option<OpenTable>,
    /// Tables, or SQL.
    pub view: DbView,
    /// The statement the user ran last, and its rows.
    pub query: Option<QueryRun>,
    /// The demo's SQL text, for the editor's first showing (review aids
    /// build in debug only).
    #[cfg(debug_assertions)]
    pub demo_sql: Option<String>,
    query_id: Option<u64>,
    last_id: u64,
    sources_id: Option<u64>,
    tables_id: Option<u64>,
    rows_id: Option<u64>,
    value_id: Option<u64>,
    table_id: Option<u64>,
    /// The read of keys, and whether it adds to the list.
    keys_id: Option<(u64, bool)>,
    key_id: Option<u64>,
    /// A read the model asks for after an answer or an event.
    pending: Option<DbCommand>,
}

/// What the Databases screen shows, decided here so it is tested without a
/// window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbScreen {
    /// No list yet.
    Loading,
    /// Nothing to open: no server installed, no SQLite file.
    Empty,
    /// The chosen server is installed and does not run.
    Stopped(DbServer),
    /// The chosen server refused `root` with no password.
    SignInFailed(DbServer),
    /// A read failed, with this code.
    Failed(String),
    /// The tables of the chosen source (an empty list says "No tables").
    Tables,
    /// Valkey is installed and does not run.
    KeysStopped,
    /// Valkey asked for a password.
    KeysSignInFailed,
    /// The keys of the chosen database.
    Keys,
}

impl DbBrowserState {
    fn next_id(&mut self) -> u64 {
        self.last_id += 1;
        self.last_id
    }

    /// The tables, filtered by "Find a table" (any case).
    pub fn shown_tables(&self) -> Vec<&marsa_core::types::DbTableInfo> {
        let filter = self.filter.to_lowercase();
        self.tables
            .iter()
            .flat_map(|tables| &tables.tables)
            .filter(|info| filter.is_empty() || info.table.name.to_lowercase().contains(&filter))
            .collect()
    }

    /// Whether `source` is in the last list, so its tables can be read.
    fn listed(&self, source: &DbSource) -> bool {
        let Some(sources) = &self.sources else {
            return false;
        };
        match source {
            DbSource::Server { server, database } => sources.servers.iter().any(|info| {
                info.server == *server && info.databases.iter().any(|db| &db.name == database)
            }),
            DbSource::SiteFile { site } => sources.files.iter().any(|file| &file.site == site),
        }
    }

    /// The first source that can be opened: a database of a running server,
    /// then a site's file; else a server, to say why it cannot be opened.
    fn first(&self) -> Option<DbSource> {
        let sources = self.sources.as_ref()?;
        let database = sources.servers.iter().find_map(|info| {
            info.databases.first().map(|db| DbSource::Server {
                server: info.server,
                database: db.name.clone(),
            })
        });
        let file = || {
            sources.files.first().map(|file| DbSource::SiteFile {
                site: file.site.clone(),
            })
        };
        database.or_else(file)
    }

    /// Whether the last list says Valkey takes commands.
    fn valkey_runs(&self) -> bool {
        self.sources
            .as_ref()
            .and_then(|sources| sources.valkey.as_ref())
            .is_some_and(|info| !info.databases.is_empty())
    }

    /// Database `db` of Valkey becomes the source: what the SQL source gave
    /// goes.
    fn choose_keys(&mut self, db: u8) {
        self.chosen = None;
        self.tables = None;
        self.open = None;
        self.query = None;
        self.query_id = None;
        self.tables_id = None;
        self.error = None;
        self.filter.clear();
        self.keys_id = None;
        self.key_id = None;
        self.keys = Some(KeyBrowser::of(db));
    }

    /// With no database and no file to open: Valkey, when it runs or when no
    /// SQL server says why it cannot be opened.
    fn opens_keys_first(&self) -> bool {
        let Some(sources) = &self.sources else {
            return false;
        };
        sources.valkey.is_some() && (self.valkey_runs() || sources.servers.is_empty())
    }

    /// The screen's state now.
    pub fn screen(&self) -> DbScreen {
        let Some(sources) = &self.sources else {
            return match &self.error {
                Some(code) => DbScreen::Failed(code.clone()),
                None => DbScreen::Loading,
            };
        };
        if sources.servers.is_empty() && sources.files.is_empty() && sources.valkey.is_none() {
            return DbScreen::Empty;
        }
        if self.keys.is_some()
            && let Some(info) = &sources.valkey
        {
            return keys::screen(info);
        }
        // A server with nothing to open says why.
        let server = match &self.chosen {
            Some(DbSource::Server { server, .. }) => Some(*server),
            None => sources.servers.first().map(|info| info.server),
            Some(DbSource::SiteFile { .. }) => None,
        };
        if let Some(info) = server.and_then(|s| sources.servers.iter().find(|i| i.server == s))
            && info.databases.is_empty()
        {
            if info.problem.as_deref() == Some(marsa_core::codes::DB_SIGN_IN_FAILED) {
                return DbScreen::SignInFailed(info.server);
            }
            // Its list is read again once it runs (`db_sources_changed`).
            if info.state == ServiceState::Starting {
                return DbScreen::Loading;
            }
            if info.state != ServiceState::Running {
                return DbScreen::Stopped(info.server);
            }
        }
        if let Some(code) = &self.error {
            return DbScreen::Failed(code.clone());
        }
        if self.chosen.is_some() && self.tables.is_none() {
            return DbScreen::Loading;
        }
        DbScreen::Tables
    }
}

impl Model {
    /// The sidebar has a Databases item when something could be opened: a
    /// SQL server or Valkey is installed, or a Laravel site may hold a
    /// SQLite file.
    /// Reading the list itself would open the servers' connections at start.
    pub fn has_databases(&self) -> bool {
        !self.installed(Runtime::Mysql).is_empty()
            || !self.installed(Runtime::Postgres).is_empty()
            || !self.installed(Runtime::Valkey).is_empty()
            || self
                .session
                .sites
                .sites
                .iter()
                .any(|site| site.kind == marsa_core::types::SiteKind::Laravel)
    }

    /// The Databases screen opened: read what can be opened.
    pub(super) fn open_databases(&mut self) -> Option<Command> {
        if !self.is_connected() {
            return None;
        }
        // Back before the close went: the connections may stay.
        self.session.db.pending = None;
        Some(Command::Db(self.read_db_sources()))
    }

    fn read_db_sources(&mut self) -> DbCommand {
        let db = &mut self.session.db;
        let id = db.next_id();
        db.sources_id = Some(id);
        DbCommand::ReadSources { id }
    }

    fn read_db_tables(&mut self, source: DbSource) -> DbCommand {
        let db = &mut self.session.db;
        let id = db.next_id();
        db.tables_id = Some(id);
        DbCommand::ReadTables { id, source }
    }

    /// The Databases screen was left: its lists go, and the daemon closes
    /// its connections (queued, so the next screen's own read still goes).
    /// The chosen source stays for the next visit.
    pub(super) fn leave_databases(&mut self) {
        let connected = self.is_connected();
        let db = &mut self.session.db;
        db.sources = None;
        db.tables = None;
        db.error = None;
        db.sources_id = None;
        db.tables_id = None;
        db.rows_id = None;
        db.value_id = None;
        db.table_id = None;
        db.keys_id = None;
        db.key_id = None;
        if let Some(keys) = db.keys.as_mut() {
            keys.drop_read();
        }
        // The close cancels a statement still running.
        db.query = None;
        db.query_id = None;
        if let Some(open) = db.open.as_mut() {
            // The table and its sort stay; what was read goes.
            *open = OpenTable {
                tab: open.tab,
                sort: open.sort.take(),
                ..OpenTable::closed(open.table.clone())
            };
        }
        db.pending = connected.then_some(DbCommand::Close);
    }

    /// A source of the menu: its tables are read.
    pub(super) fn choose_db_source(&mut self, source: DbSource) -> Option<Command> {
        let db = &mut self.session.db;
        if db.chosen.as_ref() == Some(&source) && db.tables.is_some() {
            return None;
        }
        db.keys = None;
        db.keys_id = None;
        db.key_id = None;
        if db.chosen.as_ref() != Some(&source) {
            db.open = None;
            db.query = None;
            db.query_id = None;
        }
        db.chosen = Some(source.clone());
        db.tables = None;
        db.error = None;
        db.filter.clear();
        if !self.is_connected() || !self.session.db.listed(&source) {
            return None;
        }
        Some(Command::Db(self.read_db_tables(source)))
    }

    pub(super) fn filter_db_tables(&mut self, text: String) {
        self.session.db.filter = text;
    }

    /// An answer of the bridge. One to a read the user moved on from is
    /// dropped.
    pub(super) fn db_update(&mut self, update: DbUpdate) {
        match update {
            DbUpdate::Sources { id, result } => {
                if self.session.db.sources_id != Some(id) {
                    return;
                }
                match result {
                    Ok(sources) => self.db_sources_read(sources),
                    Err(code) => self.session.db.error = Some(code),
                }
            }
            DbUpdate::Tables { id, result } => {
                let db = &mut self.session.db;
                if db.tables_id != Some(id) {
                    return;
                }
                match result {
                    Ok(tables) => {
                        db.tables = Some(tables);
                        db.error = None;
                        self.reopen_db_table();
                    }
                    Err(code) => db.error = Some(code),
                }
            }
            update @ (DbUpdate::QueryStarted { .. } | DbUpdate::Query { .. }) => {
                self.db_query_update(update);
            }
            update @ (DbUpdate::Keys { .. } | DbUpdate::Key { .. }) => {
                self.db_keys_update(update);
            }
            update => self.db_table_update(update),
        }
    }

    /// The tables came: the open table, back from another screen, reads its
    /// page again; one no longer listed closes.
    fn reopen_db_table(&mut self) {
        let db = &mut self.session.db;
        let Some(open) = &db.open else {
            return;
        };
        let listed = db
            .tables
            .iter()
            .flat_map(|tables| &tables.tables)
            .any(|info| info.table == open.table);
        if !listed {
            db.open = None;
        } else if open.page.is_none()
            && open.loading.is_none()
            && let Some(Command::Db(command)) = self.read_db_rows(0, true)
        {
            self.session.db.pending = Some(command);
        }
    }

    /// A new list: the chosen source stays if it is still there; else the
    /// first one is chosen. Its tables are read if they are not shown yet.
    fn db_sources_read(&mut self, sources: DbSources) {
        let db = &mut self.session.db;
        if db.sources.as_ref() == Some(&sources) {
            return;
        }
        db.sources = Some(sources);
        db.error = None;
        if db.keys.is_some() {
            if db.sources.as_ref().is_some_and(|s| s.valkey.is_some()) {
                self.db_keys_sources_read();
                return;
            }
            // Valkey was uninstalled: another source is chosen.
            db.keys = None;
        }
        let keep = db.chosen.as_ref().is_some_and(|chosen| db.listed(chosen));
        if !keep {
            let first = db.first();
            if first != db.chosen {
                db.tables = None;
            }
            db.chosen = first;
            // With no SQL source to open, Valkey's database 0.
            if db.chosen.is_none() && db.opens_keys_first() {
                db.choose_keys(0);
                self.db_keys_sources_read();
                return;
            }
        }
        let read = db.chosen.clone().filter(|_| db.tables.is_none());
        if let Some(source) = read {
            let command = self.read_db_tables(source);
            self.session.db.pending = Some(command);
        }
    }

    /// A read the model asks for by itself, for [`Model::next_command`].
    pub(super) fn next_db_command(&mut self) -> Option<Command> {
        self.session.db.pending.take().map(Command::Db)
    }

    /// A server started or stopped, or the site list changed: while the
    /// screen is open, the list is read again.
    pub(super) fn db_sources_changed(&mut self) {
        if self.route == Route::Databases && self.is_connected() {
            let command = self.read_db_sources();
            self.session.db.pending = Some(command);
        }
    }
}

#[cfg(test)]
mod tests;
