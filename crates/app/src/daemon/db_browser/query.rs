//! The user's SQL (ADR 0028, sections 3 to 5): one statement, run as a job
//! on a connection of its own, read-only, within [`QUERY_LIMIT`].
//!
//! The job's event says only what the rows are (`DbQueryDone`): every
//! reader of the event stream gets it. The rows wait here for
//! `GET /v1/db/results/{id}`, read once, for at most [`RESULT_KEEP`], and
//! are dropped by `POST /v1/db/close`. A result is not paged: the first
//! `RESULT_ROWS` rows or `RESULT_BYTES` of text, marked as cut; the
//! connection then closes, so the server stops sending.
//!
//! The statement's text runs as the user wrote it: that is the feature. It
//! is never logged, and nor is anything the database answers.
//!
//! A cancel (`DELETE /v1/jobs/{id}`, or `POST /v1/db/close`) or the time
//! limit stops the statement in the database, not only the wait: a second
//! short connection runs `pg_cancel_backend` (PostgreSQL) or `KILL QUERY`
//! (MySQL), which `root` may do to its own statements; SQLite's progress
//! handler sees a flag and interrupts it. The statement's connection is
//! then dropped, never reused.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use marsa_core::codes;
use marsa_core::db_browser::{self, Engine, column_kind};
use marsa_core::types::{DbCell, DbPageColumn, DbQueryDone, DbResult, DbServer, DbSource};
use sqlx::{Column, Connection, Row, TypeInfo};
use tokio_stream::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;

use super::servers::{self, Conn};
use super::{rows, sqlite, unavailable};
use crate::daemon::http::routes::HttpError;
use crate::daemon::state::Daemon;

/// The longest a statement of the user's may run.
pub(super) const QUERY_LIMIT: Duration = Duration::from_secs(5 * 60);
/// How long the rows of a finished statement wait to be read.
const RESULT_KEEP: Duration = Duration::from_secs(60);

/// What the browser keeps of the user's statements.
#[derive(Default)]
pub(super) struct Queries {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// The sources with a statement running: one at a time on each.
    busy: Vec<DbSource>,
    /// The rows of finished statements, by job id, and when they ended.
    results: HashMap<u64, (DbResult, Instant)>,
    /// The statements that run, by job id: their cancel, and how to stop
    /// them in the database once they are connected.
    running: HashMap<u64, (CancellationToken, Option<Stop>)>,
}

/// How to stop a statement inside its database.
#[derive(Clone)]
pub(super) enum Stop {
    /// The flag SQLite's progress handler reads.
    Sqlite(Arc<AtomicBool>),
    /// The server's id of the statement's connection.
    Server {
        server: DbServer,
        database: String,
        backend: u64,
    },
}

impl Queries {
    /// Marks `source` busy, or 409 `db_busy` if a statement runs on it.
    pub(super) fn reserve(&self, source: &DbSource) -> Result<(), HttpError> {
        let mut inner = self.lock();
        if inner.busy.contains(source) {
            return Err(HttpError::new(
                StatusCode::CONFLICT,
                codes::DB_BUSY,
                "another statement runs on this database",
            ));
        }
        inner.busy.push(source.clone());
        Ok(())
    }

    pub(super) fn release(&self, source: &DbSource) {
        self.lock().busy.retain(|busy| busy != source);
    }

    /// Keeps the rows of job `id`, and drops the ones kept too long.
    pub(super) fn keep(&self, id: u64, result: DbResult) {
        let mut inner = self.lock();
        let now = Instant::now();
        inner
            .results
            .retain(|_, (_, ended)| now.duration_since(*ended) < RESULT_KEEP);
        inner.results.insert(id, (result, now));
    }

    /// The rows of job `id`, once.
    pub(super) fn take(&self, id: u64) -> Option<DbResult> {
        let (result, ended) = self.lock().results.remove(&id)?;
        (ended.elapsed() < RESULT_KEEP).then_some(result)
    }

    pub(super) fn drop_results(&self) {
        self.lock().results.clear();
    }

    fn start(&self, id: u64) -> CancellationToken {
        let token = CancellationToken::new();
        self.lock().running.insert(id, (token.clone(), None));
        token
    }

    fn set_stop(&self, id: u64, stop: Stop) {
        if let Some((_, slot)) = self.lock().running.get_mut(&id) {
            *slot = Some(stop);
        }
    }

    fn finish(&self, id: u64) -> Option<Stop> {
        self.lock().running.remove(&id).and_then(|(_, stop)| stop)
    }

    /// Cancels statement `id`, if it runs.
    pub(super) fn cancel(&self, id: u64) {
        if let Some((token, _)) = self.lock().running.get(&id) {
            token.cancel();
        }
    }

    /// Cancels every statement that runs.
    pub(super) fn cancel_all(&self) {
        for (token, _) in self.lock().running.values() {
            token.cancel();
        }
    }

    /// A panic while the lock was held leaves whole values: each change is
    /// one call.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Frees its source when the job ends, however it ends.
pub(super) struct Busy {
    pub(super) daemon: Arc<Daemon>,
    pub(super) source: DbSource,
}

impl Drop for Busy {
    fn drop(&mut self) {
        if let Some(browser) = self.daemon.open_db_browser() {
            browser.queries.release(&self.source);
        }
    }
}

/// Where a statement runs, checked before the job starts.
pub(super) enum Target {
    File(std::path::PathBuf),
    Server(DbServer, String),
}

/// The one statement of `sql`, or 400: too long, or not exactly one.
pub(super) fn one_statement(engine: Engine, sql: &str) -> Result<String, HttpError> {
    if sql.len() > db_browser::SQL_BYTES {
        return Err(HttpError::new(
            StatusCode::BAD_REQUEST,
            codes::DB_SQL_TOO_LONG,
            "the statement is longer than 256 KiB",
        ));
    }
    match db_browser::statements(engine, sql).as_slice() {
        [one] => Ok(sql[one.clone()].to_owned()),
        _ => Err(HttpError::new(
            StatusCode::BAD_REQUEST,
            codes::DB_ONE_STATEMENT,
            "the text is not exactly one statement",
        )),
    }
}

/// Runs `statement` on `target` as job `id` and reads its rows, within
/// [`QUERY_LIMIT`], unless it is cancelled first.
pub(super) async fn run(
    daemon: &Daemon,
    id: u64,
    target: Target,
    statement: String,
) -> Result<(DbResult, DbQueryDone), HttpError> {
    let started = Instant::now();
    let queries = &daemon.db_browser().queries;
    let cancel = queries.start(id);
    let read = tokio::select! {
        read = tokio::time::timeout(QUERY_LIMIT, read(daemon, id, target, statement)) => {
            read.unwrap_or_else(|_| {
                Err(HttpError::new(
                    StatusCode::GATEWAY_TIMEOUT,
                    codes::DB_TIMEOUT,
                    "the statement ran longer than 5 minutes",
                ))
            })
        }
        () = cancel.cancelled() => Err(HttpError::new(
            StatusCode::CONFLICT,
            codes::DB_QUERY_CANCELLED,
            "the statement was cancelled",
        )),
    };
    let stop = queries.finish(id);
    if let (Err(err), Some(stop)) = (&read, stop)
        && (err.code == codes::DB_QUERY_CANCELLED || err.code == codes::DB_TIMEOUT)
    {
        stop_in_database(daemon, stop).await;
    }
    let (columns, rows, cut) = read?;
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let done = DbQueryDone {
        columns: columns.clone(),
        rows: rows.len() as u64,
        cut,
        elapsed_ms,
    };
    let result = DbResult {
        columns,
        rows,
        cut,
        elapsed_ms,
    };
    Ok((result, done))
}

/// Stops a statement the wait gave up on, in its database.
async fn stop_in_database(daemon: &Daemon, stop: Stop) {
    match stop {
        Stop::Sqlite(flag) => flag.store(true, Ordering::Relaxed),
        Stop::Server {
            server,
            database,
            backend,
        } => servers::stop_statement(daemon, server, &database, backend).await,
    }
}

type Read = (Vec<DbPageColumn>, Vec<Vec<DbCell>>, bool);

async fn read(
    daemon: &Daemon,
    id: u64,
    target: Target,
    statement: String,
) -> Result<Read, HttpError> {
    let queries = &daemon.db_browser().queries;
    let sql = sqlx::raw_sql(sqlx::AssertSqlSafe(statement));
    match target {
        Target::File(file) => {
            let mut conn = sqlite::open(&file).await?;
            let flag = Arc::new(AtomicBool::new(false));
            {
                let flag = Arc::clone(&flag);
                let mut handle = conn
                    .lock_handle()
                    .await
                    .map_err(|err| unavailable(err.to_string()))?;
                // Every 1,000 steps of SQLite's machine: false interrupts.
                handle.set_progress_handler(1000, move || !flag.load(Ordering::Relaxed));
            }
            queries.set_stop(id, Stop::Sqlite(flag));
            let read = collect(Engine::Sqlite, sql.fetch(&mut conn), rows::sqlite_cell).await;
            let _ = conn.close().await;
            read
        }
        // The connection is dropped after the read, closed: never reused,
        // and a cut result stops the server's sending.
        Target::Server(server, database) => {
            let mut conn =
                servers::query_connection(daemon, server, &database, QUERY_LIMIT).await?;
            let backend = servers::backend_id(&mut conn).await?;
            queries.set_stop(
                id,
                Stop::Server {
                    server,
                    database,
                    backend,
                },
            );
            match conn {
                Conn::Mysql(mut conn) => {
                    collect(Engine::Mysql, sql.fetch(&mut conn), rows::mysql_cell).await
                }
                Conn::Postgres(mut conn) => {
                    collect(Engine::Postgres, sql.fetch(&mut conn), rows::pg_cell).await
                }
            }
        }
    }
}

/// The rows of `stream`, cell by cell, until [`db_browser::RESULT_ROWS`]
/// rows or [`db_browser::RESULT_BYTES`] of text: then `cut`, and the rest
/// is never read.
async fn collect<R, S>(
    engine: Engine,
    mut stream: S,
    cell: fn(&R, usize, usize) -> Result<DbCell, HttpError>,
) -> Result<Read, HttpError>
where
    R: Row,
    S: Stream<Item = Result<R, sqlx::Error>> + Unpin,
{
    let mut columns = Vec::new();
    let mut found = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = stream.next().await {
        let row = row.map_err(query_error)?;
        if found.len() == db_browser::RESULT_ROWS || bytes > db_browser::RESULT_BYTES {
            return Ok((columns, found, true));
        }
        if columns.is_empty() {
            columns = row
                .columns()
                .iter()
                .map(|column| DbPageColumn {
                    name: column.name().to_owned(),
                    kind: column_kind(engine, column.type_info().name()),
                })
                .collect();
        }
        let cells = (0..row.len())
            .map(|index| cell(&row, index, db_browser::VALUE_BYTES))
            .collect::<Result<Vec<_>, _>>()?;
        bytes += cells.iter().map(weight).sum::<usize>();
        found.push(cells);
    }
    Ok((columns, found, false))
}

fn weight(cell: &DbCell) -> usize {
    match cell {
        DbCell::Text(text) | DbCell::Cut { text, .. } => text.len(),
        DbCell::Null | DbCell::Binary { .. } => 0,
    }
}

/// MySQL's error for a write in a read-only transaction.
const MYSQL_READ_ONLY: u16 = 1792;
/// SQLite's primary result code for a write to a read-only database.
const SQLITE_READONLY: i32 = 8;

/// What a failed statement becomes. A write the engine refused is
/// `db_read_only`; the server's limit is `db_timeout`; any other refusal is
/// `db_query_failed`. What the database said is always the `detail`.
fn query_error(err: sqlx::Error) -> HttpError {
    let Some(db) = err.as_database_error() else {
        return unavailable(err.to_string());
    };
    let said = db.message().to_owned();
    let code = db.code().map(|code| code.into_owned());
    let read_only = code.as_deref() == Some("25006")
        || code
            .as_deref()
            .and_then(|code| code.parse::<i32>().ok())
            .is_some_and(|code| code & 0xff == SQLITE_READONLY)
        || db
            .try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()
            .is_some_and(|db| db.number() == MYSQL_READ_ONLY);
    let mut http = if read_only {
        HttpError::new(
            StatusCode::CONFLICT,
            codes::DB_READ_ONLY,
            "the statement changes data, and the browser is read-only",
        )
    } else {
        let mapped = servers::db_error(err);
        if mapped.code != codes::DB_UNAVAILABLE {
            return mapped;
        }
        HttpError::new(
            StatusCode::BAD_REQUEST,
            codes::DB_QUERY_FAILED,
            "the database did not run the statement",
        )
    };
    http.detail = Some(said);
    http
}
