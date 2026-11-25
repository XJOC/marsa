//! The Databases screen's state (ADR 0028): what can be opened, the source
//! the user chose, and its tables. No event carries them, so the screen
//! reads them as it opens, when the user picks a source, and when a server
//! or the site list changes while it is open. Leaving the screen drops the
//! lists and closes the daemon's connections; the chosen source stays.
//!
//! Every read has a number, and its answer carries it: an answer to a read
//! the user has moved on from is dropped.

use marsa_core::types::{DbServer, DbSource, DbSources, DbTables};

use super::*;
use crate::bridge::{DbCommand, DbUpdate};

/// What the window knows of the databases. Part of the [`Session`]: a new
/// connection reads them again.
#[derive(Debug, Default)]
pub struct DbBrowserState {
    /// What can be opened, as last read. `None` until read, and after the
    /// screen was left.
    pub sources: Option<DbSources>,
    /// The source the user chose, or the first one the screen opened.
    pub chosen: Option<DbSource>,
    /// The tables of `chosen`, once read.
    pub tables: Option<DbTables>,
    /// What "Find a table" holds: the list is filtered here, with no read.
    pub filter: String,
    /// Why the last read failed: a client error code.
    pub error: Option<String>,
    last_id: u64,
    sources_id: Option<u64>,
    tables_id: Option<u64>,
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

    /// The screen's state now.
    pub fn screen(&self) -> DbScreen {
        let Some(sources) = &self.sources else {
            return match &self.error {
                Some(code) => DbScreen::Failed(code.clone()),
                None => DbScreen::Loading,
            };
        };
        if sources.servers.is_empty() && sources.files.is_empty() {
            return DbScreen::Empty;
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
    /// SQL server is installed, or a Laravel site may hold a SQLite file.
    /// Reading the list itself would open the servers' connections at start.
    pub fn has_databases(&self) -> bool {
        !self.installed(Runtime::Mysql).is_empty()
            || !self.installed(Runtime::Postgres).is_empty()
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
        db.pending = connected.then_some(DbCommand::Close);
    }

    /// A source of the menu: its tables are read.
    pub(super) fn choose_db_source(&mut self, source: DbSource) -> Option<Command> {
        let db = &mut self.session.db;
        if db.chosen.as_ref() == Some(&source) && db.tables.is_some() {
            return None;
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
                    }
                    Err(code) => db.error = Some(code),
                }
            }
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
        let keep = db.chosen.as_ref().is_some_and(|chosen| db.listed(chosen));
        if !keep {
            let first = db.first();
            if first != db.chosen {
                db.tables = None;
            }
            db.chosen = first;
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
