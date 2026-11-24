//! Marsa's own MySQL and PostgreSQL, for the browser (ADR 0028, sections 2,
//! 3, and 6).
//!
//! Only while the supervisor says the server runs: another program on its
//! port is never asked. The connection options are set in code, never read
//! from a URL or the environment: `127.0.0.1`, the port of `global.toml`,
//! `root` with no password, and no TLS. Never the socket, and never the
//! socket's administrator account (ADR 0023).
//!
//! Each connection is read-only, and has a 10 second limit per statement,
//! from statements run right after it opens. They are a guard against
//! accidents, not a security boundary: the user's SQL could switch them
//! back, and `root` over TCP has no right the user does not already have.

use std::net::Ipv4Addr;
use std::sync::Arc;

use axum::http::StatusCode;
use marsa_core::codes;
use marsa_core::db_browser::{self, Engine};
use marsa_core::types::{DbServer, DbTables, ServiceState};
use sqlx::mysql::{MySqlConnectOptions, MySqlConnection, MySqlSslMode};
use sqlx::postgres::{PgConnectOptions, PgConnection, PgSslMode};
use sqlx::{ConnectOptions, Row};

use super::{REQUEST_LIMIT, catalog, unavailable, within};
use crate::daemon::http::routes::HttpError;
use crate::daemon::state::Daemon;

/// An open connection to one server source.
pub(super) enum Conn {
    Mysql(MySqlConnection),
    Postgres(PgConnection),
}

/// The account the browser signs in as, over TCP, with no password.
const USER: &str = "root";
/// PostgreSQL needs a database to sign in to; this one's catalog lists the
/// others. It is never listed or opened for browsing.
const POSTGRES_HOME: &str = "postgres";

/// Runs `work` on the connection of `server` and `database` (`None`: the
/// server's own catalog connection), opening it first if needed. One request
/// at a time on each connection. A request that fails or passes
/// [`REQUEST_LIMIT`] drops its connection: it is never used again, in
/// whatever state the failure left it.
pub(super) async fn with<T>(
    daemon: &Arc<Daemon>,
    server: DbServer,
    database: Option<String>,
    work: impl AsyncFnOnce(&mut Conn) -> Result<T, HttpError>,
) -> Result<T, HttpError> {
    let manager = daemon.server(server.kind());
    if manager.status().state != ServiceState::Running {
        return Err(HttpError::new(
            StatusCode::CONFLICT,
            codes::DB_SERVER_NOT_RUNNING,
            "the server is not running",
        ));
    }
    let port = manager.port(daemon.data_dir());
    let browser = daemon.db_browser();
    let slot = browser.slot((server, database.clone()), daemon);
    // The token of the source: held across `.await` on purpose (store.rs).
    let mut held = slot.lock().await;
    let conn = match &mut *held {
        Some(conn) => conn,
        None => held.insert(within(connect(server, port, database.as_deref())).await?),
    };
    let result = within(work(conn)).await;
    if result.is_err() {
        *held = None;
    }
    result
}

async fn connect(server: DbServer, port: u16, database: Option<&str>) -> Result<Conn, HttpError> {
    let limit = REQUEST_LIMIT.as_millis();
    match server {
        DbServer::Mysql => {
            // No `sql_mode` or time zone of the driver's own: the user's SQL
            // runs as the server's defaults say.
            let mut options = MySqlConnectOptions::new()
                .host("127.0.0.1")
                .port(port)
                .username(USER)
                .ssl_mode(MySqlSslMode::Disabled)
                .pipes_as_concat(false)
                .no_engine_substitution(false)
                .timezone(None)
                .disable_statement_logging();
            if let Some(database) = database {
                options = options.database(database);
            }
            let mut conn = options.connect().await.map_err(db_error)?;
            for sql in [
                "SET SESSION TRANSACTION READ ONLY".to_owned(),
                format!("SET SESSION max_execution_time = {limit}"),
            ] {
                sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                    .execute(&mut conn)
                    .await
                    .map_err(db_error)?;
            }
            Ok(Conn::Mysql(conn))
        }
        DbServer::Postgres => {
            // `new_without_pgpass` still reads the `PG*` variables: each one
            // that could matter is set here instead.
            let options = PgConnectOptions::new_without_pgpass()
                .host("127.0.0.1")
                .hostaddr(Ipv4Addr::LOCALHOST)
                .port(port)
                .username(USER)
                .password("")
                .database(database.unwrap_or(POSTGRES_HOME))
                .ssl_mode(PgSslMode::Disable)
                .application_name("marsa")
                .disable_statement_logging();
            let mut conn = options.connect().await.map_err(db_error)?;
            for sql in [
                "SET default_transaction_read_only = on".to_owned(),
                format!("SET statement_timeout = {limit}"),
            ] {
                sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                    .execute(&mut conn)
                    .await
                    .map_err(db_error)?;
            }
            Ok(Conn::Postgres(conn))
        }
    }
}

/// The server's databases, without its own.
pub(super) async fn databases(conn: &mut Conn) -> Result<Vec<String>, HttpError> {
    let rows: Vec<Option<String>> = match conn {
        Conn::Mysql(conn) => sqlx::query(db_browser::databases_sql(DbServer::Mysql))
            .fetch_all(conn)
            .await
            .map_err(db_error)?
            .iter()
            .map(|row| row.try_get(0))
            .collect::<Result<_, _>>()
            .map_err(db_error)?,
        Conn::Postgres(conn) => sqlx::query(db_browser::databases_sql(DbServer::Postgres))
            .fetch_all(conn)
            .await
            .map_err(db_error)?
            .iter()
            .map(|row| row.try_get(0))
            .collect::<Result<_, _>>()
            .map_err(db_error)?,
    };
    Ok(rows.into_iter().flatten().collect())
}

/// The tables and views of the connection's database.
pub(super) async fn tables(conn: &mut Conn) -> Result<DbTables, HttpError> {
    match conn {
        Conn::Mysql(conn) => {
            let rows = sqlx::query(db_browser::tables_sql(Engine::Mysql))
                .fetch_all(conn)
                .await
                .map_err(db_error)?;
            catalog::tables(&rows)
        }
        Conn::Postgres(conn) => {
            let rows = sqlx::query(db_browser::tables_sql(Engine::Postgres))
                .fetch_all(conn)
                .await
                .map_err(db_error)?;
            catalog::tables(&rows)
        }
    }
}

/// What the server said goes to the client as `detail`, never in `message`.
/// A refused sign-in (SQLSTATE class 28, as MySQL's 1045 and PostgreSQL's
/// `28P01` are) is `db_sign_in_failed`: `root` has a password now.
fn db_error(err: sqlx::Error) -> HttpError {
    let sign_in = err
        .as_database_error()
        .and_then(|err| err.code())
        .is_some_and(|code| code.starts_with("28"));
    if !sign_in {
        return unavailable(err.to_string());
    }
    let mut http = HttpError::new(
        StatusCode::BAD_GATEWAY,
        codes::DB_SIGN_IN_FAILED,
        "the server refused root with no password",
    );
    http.detail = Some(err.to_string());
    http
}
