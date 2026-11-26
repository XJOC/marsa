//! The database browser (ADR 0028): the window and `marsa db` read the
//! user's local databases through these routes. Only `marsad` has a driver.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/db/sources` | `DbSources`: Marsa's servers and the sites' SQLite files |
//! | `POST /v1/db/tables` | `DbTables`: the tables and views of a source |
//! | `POST /v1/db/table` | `DbTable`: the columns of a table |
//! | `POST /v1/db/rows` | `DbPage`: 500 rows after `offset`, in the key's order |
//! | `POST /v1/db/cell` | `DbValue`: one whole value, up to 64 KiB, by the row's key |
//! | `POST /v1/jobs/db/query` | `202 JobAccepted`: one statement of the user's runs as a job ([`query`]) |
//! | `GET /v1/db/results/{id}` | `DbResult`: the rows of a finished statement, once |
//! | `POST /v1/db/close` | `204`: every browser connection closes |
//!
//! | Error | When |
//! | --- | --- |
//! | 400, 415, or 422 `invalid_body` | the body is not JSON of the request type (axum's status); a field it does not know, such as `host` or `path`, is 422 |
//! | 400 `site_name_invalid` | the site's name cannot be a site's |
//! | 404 `site_not_found` | no site has that name |
//! | 404 `db_not_found` | the server has no such database, or it is one of the server's own |
//! | 404 `db_table_not_found` | the catalog does not list the table |
//! | 404 `db_row_not_found` | no row has the key, or the table has no key |
//! | 404 `db_result_gone` | the rows were read already, kept too long, or closed |
//! | 400 `db_one_statement`, `db_sql_too_long` | the text is not one statement of at most 256 KiB |
//! | 409 `db_busy` | a statement of the user's runs on that source already |
//! | 400 `db_source_invalid` | the sort column is not the table's, or a name cannot be quoted |
//! | 404 `db_file_not_found` | the site has no SQLite file; none is made |
//! | 403 `db_file_outside_site` | the file is not a regular file inside the site's folder |
//! | 409 `db_server_not_running` | the server is not Marsa's running one |
//! | 502 `db_sign_in_failed` | the server refused `root` with no password; `detail` says why |
//! | 502 `db_unavailable` | the database could not be opened or read; `detail` says why |
//! | 504 `db_timeout` | the database did not answer within 10 seconds |
//!
//! What the log keeps: the source, the kind of request, its time, and the
//! number of rows. Never a table's name, a value, SQL text, or what a
//! database said: those go to the client that asked, and only there.

mod catalog;
mod query;
mod rows;
mod servers;
mod sqlite;
mod store;

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use marsa_core::codes;
use marsa_core::databases::ServerKind;
use marsa_core::types::{
    DbCellRequest, DbDatabase, DbPage, DbQueryRequest, DbResult, DbRowsRequest, DbServer,
    DbServerInfo, DbSource, DbSources, DbTable, DbTableRequest, DbTables, DbTablesRequest, DbValue,
    JobAccepted, JobKind, JobOutcome,
};
use tracing::info;

use crate::daemon::http::Rejected;
use crate::daemon::http::routes::HttpError;
use crate::daemon::state::Daemon;

/// The browser's own reads (the catalog, a page, a value) end within this
/// time.
const REQUEST_LIMIT: Duration = Duration::from_secs(10);

/// How long an exact count of a table's rows may take; past it, the total
/// is the server's estimate.
const COUNT_LIMIT: Duration = Duration::from_secs(2);

/// How long a server connection stays open with no request.
const IDLE: Duration = Duration::from_secs(60);

/// What the browser keeps in `marsad`: its server connections
/// ([`store`]). Made by the first request that needs one, so an idle
/// `marsad` holds nothing of it. SQLite files are never kept open.
pub(crate) struct Browser {
    store: Arc<store::Store<servers::Conn>>,
    queries: query::Queries,
}

/// Says nothing of what it holds: a connection is not for logs.
impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser").finish_non_exhaustive()
    }
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            store: Arc::new(store::Store::new(IDLE)),
            queries: query::Queries::default(),
        }
    }
}

impl Browser {
    /// The slot of `key`; the first one starts the task that closes idle
    /// connections.
    fn slot(&self, key: store::Key, daemon: &Daemon) -> store::Slot<servers::Conn> {
        let (slot, start) = self.store.slot(&key);
        if start {
            self.store.spawn_reaper(daemon.shutdown().clone());
        }
        slot
    }

    /// Closes the connections to `kind`, before it stops.
    pub(crate) fn close_server(&self, kind: ServerKind) {
        if let Some(server) = DbServer::ALL.into_iter().find(|s| s.kind() == kind) {
            self.store.close_server(server);
        }
    }

    pub(crate) fn close_all(&self) {
        self.store.close_all();
    }
}

/// The routes, for `http::routes::router` to merge before its fallbacks.
pub fn routes() -> Router<Arc<Daemon>> {
    Router::new()
        .route("/v1/db/sources", get(sources))
        .route("/v1/db/tables", post(tables))
        .route("/v1/db/table", post(table))
        .route("/v1/db/rows", post(rows))
        .route("/v1/db/cell", post(cell))
        .route("/v1/jobs/db/query", post(start_query))
        .route("/v1/db/results/{id}", get(result))
        .route("/v1/db/close", post(close))
}

async fn sources(State(daemon): State<Arc<Daemon>>) -> Result<Json<DbSources>, HttpError> {
    let started = Instant::now();
    let list = daemon.site_list().await?;
    let files =
        crate::daemon::http::blocking("db sources", move || Ok(sqlite::files(&list))).await?;
    let mut servers = Vec::new();
    for server in DbServer::ALL {
        if let Some(info) = server_info(&daemon, server).await? {
            servers.push(info);
        }
    }
    info!(
        request = "sources",
        elapsed_ms = elapsed_ms(started),
        rows = files.len() + servers.len(),
        "database browser"
    );
    Ok(Json(DbSources { servers, files }))
}

/// An installed server, with its databases while it runs. A server that
/// runs but cannot be read shows why as `problem`, and the list still
/// answers.
async fn server_info(
    daemon: &Arc<Daemon>,
    server: DbServer,
) -> Result<Option<DbServerInfo>, HttpError> {
    let manager = daemon.server(server.kind());
    let data_dir = daemon.data_dir().to_path_buf();
    let listed = crate::daemon::http::blocking("db server", {
        let daemon = Arc::clone(daemon);
        move || {
            let manager = daemon.server(server.kind());
            Ok(manager
                .listed(&data_dir)
                .then(|| manager.version(&data_dir)))
        }
    })
    .await?;
    let Some(version) = listed else {
        return Ok(None);
    };
    let state = manager.status().state;
    // Only once it said it takes connections: a server that starts is
    // listed with its state, no databases yet, and no problem.
    let (databases, problem) = if manager.ready_port(daemon).is_some() {
        match within(servers::with(daemon, server, None, servers::databases)).await {
            Ok(names) => (
                names.into_iter().map(|name| DbDatabase { name }).collect(),
                None,
            ),
            Err(err) => (Vec::new(), Some(err.code.to_owned())),
        }
    } else {
        (Vec::new(), None)
    };
    Ok(Some(DbServerInfo {
        server,
        version: version
            .map(|version| version.to_string())
            .unwrap_or_default(),
        state,
        databases,
        problem,
    }))
}

async fn tables(
    State(daemon): State<Arc<Daemon>>,
    body: Result<Json<DbTablesRequest>, JsonRejection>,
) -> Result<Json<DbTables>, HttpError> {
    let Json(DbTablesRequest { source }) = body.map_err(|err| err.reject(codes::INVALID_BODY))?;
    let started = Instant::now();
    let tables = match &source {
        DbSource::SiteFile { site } => {
            let file = sqlite::site_file(&daemon, site).await?;
            within(sqlite::tables(file)).await?
        }
        // One limit for the whole request: the wait for the source, two
        // connections, and their reads.
        DbSource::Server { server, database } => {
            within(async {
                let database = listed_database(&daemon, *server, database).await?;
                servers::with(&daemon, *server, Some(database), servers::tables).await
            })
            .await?
        }
    };
    log(&source, "tables", started, tables.tables.len());
    Ok(Json(tables))
}

async fn table(
    State(daemon): State<Arc<Daemon>>,
    body: Result<Json<DbTableRequest>, JsonRejection>,
) -> Result<Json<DbTable>, HttpError> {
    let Json(DbTableRequest { source, table }) =
        body.map_err(|err| err.reject(codes::INVALID_BODY))?;
    let started = Instant::now();
    let answer = match &source {
        DbSource::SiteFile { site } => {
            let file = sqlite::site_file(&daemon, site).await?;
            within(sqlite::table(file, table)).await?
        }
        DbSource::Server { server, database } => {
            within(async {
                let database = listed_database(&daemon, *server, database).await?;
                servers::with(&daemon, *server, Some(database), async |conn| {
                    servers::table(conn, &table).await
                })
                .await
            })
            .await?
        }
    };
    log(&source, "table", started, answer.columns.len());
    Ok(Json(answer))
}

async fn rows(
    State(daemon): State<Arc<Daemon>>,
    body: Result<Json<DbRowsRequest>, JsonRejection>,
) -> Result<Json<DbPage>, HttpError> {
    let Json(request) = body.map_err(|err| err.reject(codes::INVALID_BODY))?;
    let source = request.source.clone();
    let started = Instant::now();
    let page = match &source {
        DbSource::SiteFile { site } => {
            let file = sqlite::site_file(&daemon, site).await?;
            within(sqlite::page(file, request)).await?
        }
        DbSource::Server { server, database } => {
            within(async {
                let database = listed_database(&daemon, *server, database).await?;
                servers::with(&daemon, *server, Some(database), async |conn| {
                    servers::page(conn, &request).await
                })
                .await
            })
            .await?
        }
    };
    log(&source, "rows", started, page.rows.len());
    Ok(Json(page))
}

async fn cell(
    State(daemon): State<Arc<Daemon>>,
    body: Result<Json<DbCellRequest>, JsonRejection>,
) -> Result<Json<DbValue>, HttpError> {
    let Json(request) = body.map_err(|err| err.reject(codes::INVALID_BODY))?;
    let source = request.source.clone();
    let started = Instant::now();
    let value = match &source {
        DbSource::SiteFile { site } => {
            let file = sqlite::site_file(&daemon, site).await?;
            within(sqlite::cell(file, request)).await?
        }
        DbSource::Server { server, database } => {
            within(async {
                let database = listed_database(&daemon, *server, database).await?;
                servers::with(&daemon, *server, Some(database), async |conn| {
                    servers::cell(conn, &request).await
                })
                .await
            })
            .await?
        }
    };
    log(&source, "cell", started, 1);
    Ok(Json(value))
}

/// Starts one statement of the user's as a job. The source is checked first,
/// so a missing site or database answers this request, not the job.
async fn start_query(
    State(daemon): State<Arc<Daemon>>,
    body: Result<Json<DbQueryRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<JobAccepted>), HttpError> {
    let Json(DbQueryRequest { source, sql }) =
        body.map_err(|err| err.reject(codes::INVALID_BODY))?;
    let statement = query::one_statement(source.engine(), &sql)?;
    let target = match &source {
        DbSource::SiteFile { site } => query::Target::File(sqlite::site_file(&daemon, site).await?),
        DbSource::Server { server, database } => {
            let database = within(listed_database(&daemon, *server, database)).await?;
            query::Target::Server(*server, database)
        }
    };
    daemon.db_browser().queries.reserve(&source)?;
    let busy = query::Busy {
        daemon: Arc::clone(&daemon),
        source: source.clone(),
    };
    let (id_sent, id) = tokio::sync::oneshot::channel::<u64>();
    let work = {
        let daemon = Arc::clone(&daemon);
        let source = source.clone();
        async move {
            let _busy = busy;
            let started = Instant::now();
            let id = id
                .await
                .map_err(|_| unavailable("the job has no id".to_owned()))?;
            let (result, done) = query::run(&daemon, id, target, statement).await?;
            log(&source, "query", started, result.rows.len());
            daemon.db_browser().queries.keep(id, result);
            Ok(done)
        }
    };
    let (id, _ended) = crate::daemon::jobs::start(
        &daemon,
        JobKind::DbQuery,
        "db query",
        JobOutcome::DbQuery,
        work,
        |_, _| {},
    );
    let _ = id_sent.send(id);
    Ok((StatusCode::ACCEPTED, Json(JobAccepted { id })))
}

/// Cancels the user's statement `id`, if it runs: `DELETE /v1/jobs/{id}`.
pub(crate) fn cancel_query(daemon: &Daemon, id: u64) {
    if let Some(browser) = daemon.open_db_browser() {
        browser.queries.cancel(id);
    }
}

/// The rows of a finished statement, once.
async fn result(
    State(daemon): State<Arc<Daemon>>,
    Path(id): Path<u64>,
) -> Result<Json<DbResult>, HttpError> {
    daemon
        .open_db_browser()
        .and_then(|browser| browser.queries.take(id))
        .map(Json)
        .ok_or_else(|| {
            HttpError::new(
                StatusCode::NOT_FOUND,
                codes::DB_RESULT_GONE,
                "no rows are kept for that statement",
            )
        })
}

/// `database`, if the server's catalog lists it. A system database is never
/// listed, so it is never opened.
async fn listed_database(
    daemon: &Arc<Daemon>,
    server: DbServer,
    database: &str,
) -> Result<String, HttpError> {
    let names = servers::with(daemon, server, None, servers::databases).await?;
    if names.iter().any(|name| name == database) {
        Ok(database.to_owned())
    } else {
        Err(HttpError::new(
            StatusCode::NOT_FOUND,
            codes::DB_NOT_FOUND,
            "the server has no such database",
        ))
    }
}

/// Every connection of the browser closes. The window sends it when the
/// user leaves the screen or closes the window. A SQLite file is never kept
/// open between requests.
async fn close(State(daemon): State<Arc<Daemon>>) -> StatusCode {
    if let Some(browser) = daemon.open_db_browser() {
        browser.close_all();
        browser.queries.cancel_all();
        browser.queries.drop_results();
    }
    info!(request = "close", "database browser");
    StatusCode::NO_CONTENT
}

/// `work`, or `db_timeout` after [`REQUEST_LIMIT`]. A connection the work
/// held is dropped with it. A SQLite statement that runs then still ends on
/// the driver's own thread first: the driver does not interrupt it on drop.
async fn within<T>(work: impl Future<Output = Result<T, HttpError>>) -> Result<T, HttpError> {
    tokio::time::timeout(REQUEST_LIMIT, work)
        .await
        .unwrap_or_else(|_| {
            Err(HttpError::new(
                StatusCode::GATEWAY_TIMEOUT,
                codes::DB_TIMEOUT,
                format!(
                    "the database did not answer within {} seconds",
                    REQUEST_LIMIT.as_secs()
                ),
            ))
        })
}

/// The one log line of a request: what the ADR allows, and nothing more.
fn log(source: &DbSource, request: &'static str, started: Instant, rows: usize) {
    // A database's name may hold a line break: escaped, it cannot forge a
    // line of the log.
    info!(
        source = %source.to_string().escape_debug(),
        request,
        elapsed_ms = elapsed_ms(started),
        rows,
        "database browser"
    );
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// A failure of the database itself: 502 `db_unavailable`, with what it said
/// as `detail` and only a fixed text as `message`, which may be logged.
fn unavailable(said: String) -> HttpError {
    let mut err = HttpError::new(
        StatusCode::BAD_GATEWAY,
        codes::DB_UNAVAILABLE,
        "the database could not be read",
    );
    err.detail = Some(said);
    err
}

#[cfg(test)]
mod query_tests;
#[cfg(test)]
mod tests;
