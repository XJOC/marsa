//! Marsa's own MySQL and PostgreSQL, for the browser (ADR 0028, sections 2,
//! 3, and 6).
//!
//! Only the server this daemon runs, and only at the port where it said it
//! took connections ([`ServerManager::ready_port`]): another program on a
//! port is never asked, also when `global.toml` changed or cannot be read.
//! The connection options are set in code: `127.0.0.1`, `root` with no
//! password, and no TLS. Never the socket, and never the socket's
//! administrator account (ADR 0023). PostgreSQL's driver reads the `PG*`
//! variables; each one is set here, and `PGOPTIONS`, which cannot be undone,
//! makes the browser refuse.
//!
//! Each connection is read-only, and has a 10 second limit per statement,
//! from statements run right after it opens and read back before it is
//! used. They are a guard against accidents, not a security boundary: the
//! user's SQL could switch them back, and `root` over TCP has no right the
//! user does not already have.
//!
//! [`ServerManager::ready_port`]: crate::daemon::services::databases::ServerManager::ready_port

use std::net::Ipv4Addr;
use std::sync::Arc;

use axum::http::StatusCode;
use marsa_core::codes;
use marsa_core::db_browser::{self, Engine};
use marsa_core::types::{DbServer, DbTables};
use sqlx::mysql::{MySqlConnectOptions, MySqlConnection, MySqlDatabaseError, MySqlSslMode};
use sqlx::postgres::{PgConnectOptions, PgConnection, PgSslMode};
use sqlx::{ConnectOptions, Connection, Row};

use super::{REQUEST_LIMIT, catalog, unavailable};
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
/// at a time on each connection. The caller bounds the whole request with
/// `within`.
///
/// The connection is out of its slot while `work` runs, and goes back only
/// when `work` succeeds: one that failed, timed out, or whose request was
/// dropped (the client hung up) is closed, never used again.
pub(super) async fn with<T>(
    daemon: &Arc<Daemon>,
    server: DbServer,
    database: Option<String>,
    work: impl AsyncFnOnce(&mut Conn) -> Result<T, HttpError>,
) -> Result<T, HttpError> {
    port(daemon, server)?;
    let slot = daemon.db_browser().slot((server, database.clone()), daemon);
    // The token of the source: held across `.await` on purpose (store.rs).
    let mut held = slot.lock().await;
    // Again after the wait: the server may have stopped meanwhile.
    let port = port(daemon, server)?;
    let reused = match held.take() {
        Some(mut conn) => ping(&mut conn).await.then_some(conn),
        None => None,
    };
    // A connection that no longer answers (its server restarted) is dropped,
    // and a new one opens.
    let mut conn = match reused {
        Some(conn) => conn,
        None => connect(server, port, database.as_deref()).await?,
    };
    let result = work(&mut conn).await;
    if result.is_ok() {
        *held = Some(conn);
    }
    result
}

/// The port of the server this daemon runs, once it took connections.
fn port(daemon: &Daemon, server: DbServer) -> Result<u16, HttpError> {
    if daemon.shutdown().is_cancelled() {
        return Err(unavailable("marsad is stopping".to_owned()));
    }
    daemon
        .server(server.kind())
        .ready_port(daemon)
        .ok_or_else(|| {
            HttpError::new(
                StatusCode::CONFLICT,
                codes::DB_SERVER_NOT_RUNNING,
                "the server is not running",
            )
        })
}

async fn ping(conn: &mut Conn) -> bool {
    match conn {
        Conn::Mysql(conn) => conn.ping().await.is_ok(),
        Conn::Postgres(conn) => conn.ping().await.is_ok(),
    }
}

/// What the session settings must read back as, after they are set.
const LIMIT_MS: u128 = REQUEST_LIMIT.as_millis();

async fn connect(server: DbServer, port: u16, database: Option<&str>) -> Result<Conn, HttpError> {
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
                format!("SET SESSION max_execution_time = {LIMIT_MS}"),
            ] {
                sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                    .execute(&mut conn)
                    .await
                    .map_err(db_error)?;
            }
            let row = sqlx::query(
                "SELECT CAST(@@session.transaction_read_only AS CHAR), \
                 CAST(@@session.max_execution_time AS CHAR)",
            )
            .fetch_one(&mut conn)
            .await
            .map_err(db_error)?;
            check_settings(&row, "1")?;
            Ok(Conn::Mysql(conn))
        }
        DbServer::Postgres => {
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
            no_startup_options(&options)?;
            let mut conn = options.connect().await.map_err(db_error)?;
            for sql in [
                "SET default_transaction_read_only = on".to_owned(),
                format!("SET statement_timeout = {LIMIT_MS}"),
            ] {
                sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                    .execute(&mut conn)
                    .await
                    .map_err(db_error)?;
            }
            let row = sqlx::query(
                "SELECT pg_catalog.current_setting('default_transaction_read_only'), \
                 (SELECT setting FROM pg_catalog.pg_settings WHERE name = 'statement_timeout')",
            )
            .fetch_one(&mut conn)
            .await
            .map_err(db_error)?;
            check_settings(&row, "on")?;
            Ok(Conn::Postgres(conn))
        }
    }
}

/// The driver takes `PGOPTIONS` from marsad's environment and has no way to
/// drop it: it could set the role or the search path. Then the browser
/// refuses PostgreSQL.
pub(super) fn no_startup_options(options: &PgConnectOptions) -> Result<(), HttpError> {
    match options.get_options() {
        None => Ok(()),
        Some(_) => Err(unavailable(
            "PGOPTIONS is set in marsad's environment, and the browser does not use it".to_owned(),
        )),
    }
}

/// The read-only flag reads back as `read_only`, and the limit as
/// [`LIMIT_MS`] milliseconds; else the connection is not used.
fn check_settings<R>(row: &R, read_only: &str) -> Result<(), HttpError>
where
    R: Row,
    for<'r> String: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    usize: sqlx::ColumnIndex<R>,
{
    let got: (String, String) = (
        row.try_get(0).map_err(db_error)?,
        row.try_get(1).map_err(db_error)?,
    );
    if got == (read_only.to_owned(), LIMIT_MS.to_string()) {
        Ok(())
    } else {
        Err(unavailable(
            "the read-only setting or the time limit did not hold".to_owned(),
        ))
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

/// MySQL's error number when `max_execution_time` stopped a statement.
const MYSQL_EXECUTION_TIME: u16 = 3024;

/// What the server said goes to the client as `detail`, never in `message`.
/// A refused sign-in (SQLSTATE class 28, as MySQL's 1045 and PostgreSQL's
/// `28P01` are) is `db_sign_in_failed`: `root` has a password now. A
/// statement the server stopped at the session's limit (PostgreSQL's
/// `57014`, MySQL's 3024) is `db_timeout`.
pub(super) fn db_error(err: sqlx::Error) -> HttpError {
    let (status, code, message) = match err.as_database_error() {
        Some(db) if db.code().is_some_and(|code| code.starts_with("28")) => (
            StatusCode::BAD_GATEWAY,
            codes::DB_SIGN_IN_FAILED,
            "the server refused root with no password",
        ),
        Some(db)
            if db.code().as_deref() == Some("57014")
                || db
                    .try_downcast_ref::<MySqlDatabaseError>()
                    .is_some_and(|db| db.number() == MYSQL_EXECUTION_TIME) =>
        {
            (
                StatusCode::GATEWAY_TIMEOUT,
                codes::DB_TIMEOUT,
                "the server stopped the statement at its time limit",
            )
        }
        _ => return unavailable(err.to_string()),
    };
    let mut http = HttpError::new(status, code, message);
    http.detail = Some(err.to_string());
    http
}
