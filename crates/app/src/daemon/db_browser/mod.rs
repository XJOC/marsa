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
//! | 400 `invalid_body` | the body is not the request type; a field it does not know (`host`, `path`) is refused |
//! | 400 `site_name_invalid` | the site's name cannot be a site's |
//! | 404 `site_not_found` | no site has that name |
//! | 404 `db_file_not_found` | the site has no SQLite file; none is made |
//! | 403 `db_file_outside_site` | the file is not a regular file inside the site's folder |
//! | 409 `db_server_not_running` | the server is not Marsa's running one |
//! | 502 `db_unavailable` | the database could not be opened or read; `detail` says why |
//! | 504 `db_timeout` | the database did not answer within 10 seconds |
//!
//! What the log keeps: the source, the kind of request, its time, and the
//! number of rows. Never a table's name, a value, SQL text, or what a
//! database said: those go to the client that asked, and only there.

mod catalog;
mod sqlite;

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use marsa_core::codes;
use marsa_core::types::{DbSource, DbSources, DbTables, DbTablesRequest, ServiceState, SiteList};
use tracing::info;

use crate::daemon::http::Rejected;
use crate::daemon::http::routes::HttpError;
use crate::daemon::state::Daemon;

/// The browser's own reads (the catalog, a page, a value) end within this
/// time.
const REQUEST_LIMIT: Duration = Duration::from_secs(10);

/// The routes, for `http::routes::router` to merge before its fallbacks.
pub fn routes() -> Router<Arc<Daemon>> {
    Router::new()
        .route("/v1/db/sources", get(sources))
        .route("/v1/db/tables", post(tables))
        .route("/v1/db/close", post(close))
}

async fn sources(State(daemon): State<Arc<Daemon>>) -> Result<Json<DbSources>, HttpError> {
    let started = Instant::now();
    let list = site_list(&daemon).await?;
    let files =
        crate::daemon::http::blocking("db sources", move || Ok(sqlite::files(&list))).await?;
    info!(
        request = "sources",
        elapsed_ms = elapsed_ms(started),
        rows = files.len(),
        "database browser"
    );
    Ok(Json(DbSources {
        servers: Vec::new(),
        files,
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
        DbSource::Server { server, .. } => {
            if daemon.server(server.kind()).status().state != ServiceState::Running {
                return Err(HttpError::new(
                    StatusCode::CONFLICT,
                    codes::DB_SERVER_NOT_RUNNING,
                    "the server is not running",
                ));
            }
            return Err(HttpError::new(
                StatusCode::BAD_GATEWAY,
                codes::DB_UNAVAILABLE,
                "this daemon does not browse servers yet",
            ));
        }
    };
    log(&source, "tables", started, tables.tables.len());
    Ok(Json(tables))
}

/// Every connection of the browser closes. The window sends it when the
/// user leaves the screen or closes the window. A SQLite file is never kept
/// open between requests, so there is nothing to close yet.
async fn close() -> StatusCode {
    info!(request = "close", "database browser");
    StatusCode::NO_CONTENT
}

/// The site list, built first if the daemon has none yet.
async fn site_list(daemon: &Arc<Daemon>) -> Result<SiteList, HttpError> {
    match daemon.sites() {
        Some(list) => Ok(list),
        None => Ok(daemon.rebuild_sites().await?),
    }
}

/// `work`, or `db_timeout` after [`REQUEST_LIMIT`]. A connection the work
/// held is dropped with it, which closes it.
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
