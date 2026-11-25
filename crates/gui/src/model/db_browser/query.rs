//! The SQL view (layout.md, "SQL view"): one statement of the user's at a
//! time, its run, and its rows. The text itself stays with the screen (a
//! key press must not copy it); only the statement to run comes here.
//!
//! A run starts a job and learns its id at once, so Cancel can stop it in
//! the database; the rows come when it ends. Leaving the screen drops the
//! result (the daemon's close cancels a statement still running).

use std::time::Instant;

use marsa_core::codes;
use marsa_core::types::{DbQueryRequest, DbResult};

use super::super::*;
use super::DbView;
use crate::bridge::{DbCommand, DbUpdate};

/// The statement the user ran last.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryRun {
    /// When it started: "Running for 12 s…".
    pub started: Instant,
    /// The daemon's job, once it answered: what Cancel stops.
    pub job: Option<u64>,
    pub state: QueryState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryState {
    Running,
    /// Cancel was asked; the statement stops in the database.
    Cancelling,
    Done(DbResult),
    /// "Cancelled." in the run bar: not an error.
    Cancelled,
    /// A refused statement: a client error code, and the server's own words
    /// when it gave some.
    Failed {
        code: String,
        detail: Option<String>,
    },
}

impl QueryRun {
    pub fn running(&self) -> bool {
        matches!(self.state, QueryState::Running | QueryState::Cancelling)
    }
}

impl Model {
    /// "Run": the statement goes to the daemon, for the chosen source. One
    /// runs at a time.
    pub(super) fn run_db_query(&mut self, sql: String) -> Option<Command> {
        if !self.is_connected() || sql.trim().is_empty() {
            return None;
        }
        let db = &mut self.session.db;
        if db.query.as_ref().is_some_and(QueryRun::running) {
            return None;
        }
        let source = db.chosen.clone()?;
        let id = db.next_id();
        db.query_id = Some(id);
        db.query = Some(QueryRun {
            started: Instant::now(),
            job: None,
            state: QueryState::Running,
        });
        let request = DbQueryRequest { source, sql };
        Some(Command::Db(DbCommand::RunQuery { id, request }))
    }

    /// "Cancel", or Esc in the editor.
    pub(super) fn cancel_db_query(&mut self) -> Option<Command> {
        let run = self.session.db.query.as_mut()?;
        if run.state != QueryState::Running {
            return None;
        }
        run.state = QueryState::Cancelling;
        // Not started yet: the cancel goes when its id comes.
        let job = run.job?;
        Some(Command::Db(DbCommand::CancelQuery { job }))
    }

    /// The SQL view or the tables.
    pub(super) fn db_view(&mut self, view: DbView) {
        self.session.db.view = view;
    }

    /// An answer about the statement; one about an older run is dropped.
    pub(super) fn db_query_update(&mut self, update: DbUpdate) {
        let db = &mut self.session.db;
        match update {
            DbUpdate::QueryStarted { id, result } if db.query_id == Some(id) => {
                let Some(run) = db.query.as_mut() else {
                    return;
                };
                match result {
                    Ok(job) => {
                        run.job = Some(job);
                        // Cancel came before the id: it goes now.
                        if run.state == QueryState::Cancelling {
                            db.pending = Some(DbCommand::CancelQuery { job });
                        }
                    }
                    Err((code, detail)) => run.state = QueryState::Failed { code, detail },
                }
            }
            DbUpdate::Query { id, result } if db.query_id == Some(id) => {
                db.query_id = None;
                let Some(run) = db.query.as_mut() else {
                    return;
                };
                run.state = match result {
                    Ok(rows) => QueryState::Done(rows),
                    Err((code, _)) if code == codes::DB_QUERY_CANCELLED => QueryState::Cancelled,
                    Err((code, detail)) => QueryState::Failed { code, detail },
                };
            }
            _ => {}
        }
    }
}
