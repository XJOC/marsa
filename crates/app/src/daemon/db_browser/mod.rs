//! The database browser (ADR 0028): the window and `marsa db` read the
//! user's local databases through these routes. Only `marsad` has a driver.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/db/sources` | `DbSources`: Marsa's servers and the sites' SQLite files |
//! | `POST /v1/db/tables` | `DbTables`: the tables and views of a source |
//! | `POST /v1/db/close` | `204`: every browser connection closes |
//!
//! | Error | When |
//! | --- | --- |
//! | 400, 415, or 422 `invalid_body` | the body is not JSON of the request type (axum's status); a field it does not know, such as `host` or `path`, is 422 |
//! | 400 `site_name_invalid` | the site's name cannot be a site's |
//! | 404 `site_not_found` | no site has that name |
//! | 404 `db_not_found` | the server has no such database, or it is one of the server's own |
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
mod servers;
mod sqlite;
mod store;

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use marsa_core::codes;
use marsa_core::databases::ServerKind;
use marsa_core::types::{
    DbDatabase, DbServer, DbServerInfo, DbSource, DbSources, DbTables, DbTablesRequest,
    ServiceState,
};
use tracing::info;

use crate::daemon::http::Rejected;
use crate::daemon::http::routes::HttpError;
use crate::daemon::state::Daemon;

/// The browser's own reads (the catalog, a page, a value) end within this
/// time.
const REQUEST_LIMIT: Duration = Duration::from_secs(10);

/// How long a server connection stays open with no request.
const IDLE: Duration = Duration::from_secs(60);

/// What the browser keeps in `marsad`: its server connections
/// ([`store`]). Made by the first request that needs one, so an idle
/// `marsad` holds nothing of it. SQLite files are never kept open.
pub struct Browser {
    store: Arc<store::Store<servers::Conn>>,
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
    pub fn close_server(&self, kind: ServerKind) {
        if let Some(server) = DbServer::ALL.into_iter().find(|s| s.kind() == kind) {
            self.store.close_server(server);
        }
    }

    pub fn close_all(&self) {
        self.store.close_all();
    }
}

/// The routes, for `http::routes::router` to merge before its fallbacks.
pub fn routes() -> Router<Arc<Daemon>> {
    Router::new()
        .route("/v1/db/sources", get(sources))
        .route("/v1/db/tables", post(tables))
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
    let (databases, problem) = if state == ServiceState::Running {
        match servers::with(daemon, server, None, servers::databases).await {
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
        DbSource::Server { server, database } => {
            let database = listed_database(&daemon, *server, database).await?;
            servers::with(&daemon, *server, Some(database), servers::tables).await?
        }
    };
    log(&source, "tables", started, tables.tables.len());
    Ok(Json(tables))
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
    info!(
        source = %source,
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
mod tests;
