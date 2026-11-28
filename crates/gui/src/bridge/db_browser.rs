//! The Databases screen's reads (ADR 0028): through `marsa_core::client`,
//! like every call of the window. The window has no driver and no
//! connection of its own.

use marsa_core::client::DaemonClient;
use marsa_core::types::{
    DbCellRequest, DbKeyDetail, DbKeyRequest, DbKeys, DbKeysRequest, DbPage, DbQueryRequest,
    DbResult, DbRowsRequest, DbSource, DbSources, DbTable, DbTableRef, DbTables, DbValue,
};

use crate::model::Update;

/// What the Databases screen asks the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbCommand {
    /// What can be opened. `id` comes back with the answer.
    ReadSources { id: u64 },
    /// The tables of `source`.
    ReadTables { id: u64, source: DbSource },
    /// A page of a table's rows.
    ReadRows { id: u64, request: DbRowsRequest },
    /// The whole value of one cell, for the value pane.
    ReadValue { id: u64, request: DbCellRequest },
    /// A table's columns.
    ReadTable {
        id: u64,
        source: DbSource,
        table: DbTableRef,
    },
    /// A statement of the user's: its job starts, then its rows come.
    RunQuery { id: u64, request: DbQueryRequest },
    /// Stops the running statement's job.
    CancelQuery { job: u64 },
    /// A page of Valkey's keys.
    ReadKeys { id: u64, request: DbKeysRequest },
    /// One key's value.
    ReadKey { id: u64, request: DbKeyRequest },
    /// The screen was left: the daemon closes the browser's connections.
    Close,
}

/// A refused statement or read: the client error code, and the server's own
/// words.
pub(crate) type QueryError = (String, Option<String>);

/// The daemon's answer to a [`DbCommand`], with the read's `id`, or a
/// client error code.
#[derive(Debug)]
pub enum DbUpdate {
    Sources {
        id: u64,
        result: Result<DbSources, String>,
    },
    Tables {
        id: u64,
        result: Result<DbTables, String>,
    },
    Rows {
        id: u64,
        result: Result<DbPage, String>,
    },
    Value {
        id: u64,
        result: Result<DbValue, String>,
    },
    /// The columns of `table` of `source`.
    Table {
        id: u64,
        source: DbSource,
        table: DbTableRef,
        result: Result<DbTable, String>,
    },
    Keys {
        id: u64,
        result: Result<DbKeys, QueryError>,
    },
    Key {
        id: u64,
        result: Result<DbKeyDetail, QueryError>,
    },
    /// The statement's job started: its id, for Cancel.
    QueryStarted {
        id: u64,
        result: Result<u64, QueryError>,
    },
    /// The statement ended: its rows, or why not.
    Query {
        id: u64,
        result: Result<DbResult, QueryError>,
    },
}

/// Runs `command`, and the update it gives, if any. A statement sends its
/// start through `window` first.
pub(super) async fn run(
    client: &DaemonClient,
    command: DbCommand,
    window: &super::Window,
) -> Option<Update> {
    let code = |err: marsa_core::client::ClientError| err.code().to_owned();
    let failed = |err: marsa_core::client::ClientError| {
        let detail = err.detail().map(str::to_owned);
        (err.code().to_owned(), detail)
    };
    match command {
        DbCommand::ReadSources { id } => Some(Update::Db(DbUpdate::Sources {
            id,
            result: client.db_sources().await.map_err(code),
        })),
        DbCommand::ReadTables { id, source } => Some(Update::Db(DbUpdate::Tables {
            id,
            result: client.db_tables(&source).await.map_err(code),
        })),
        DbCommand::ReadRows { id, request } => Some(Update::Db(DbUpdate::Rows {
            id,
            result: client.db_rows(&request).await.map_err(code),
        })),
        DbCommand::ReadValue { id, request } => Some(Update::Db(DbUpdate::Value {
            id,
            result: client.db_cell(&request).await.map_err(code),
        })),
        DbCommand::ReadTable { id, source, table } => {
            let result = client.db_table(&source, &table).await.map_err(code);
            Some(Update::Db(DbUpdate::Table {
                id,
                source,
                table,
                result,
            }))
        }
        DbCommand::ReadKeys { id, request } => Some(Update::Db(DbUpdate::Keys {
            id,
            result: client.db_keys(&request).await.map_err(failed),
        })),
        DbCommand::ReadKey { id, request } => Some(Update::Db(DbUpdate::Key {
            id,
            result: client.db_key(&request).await.map_err(failed),
        })),
        DbCommand::RunQuery { id, request } => {
            let job = match client.start_db_query(&request).await {
                Ok(job) => job,
                Err(err) => {
                    let result = Err(failed(err));
                    return Some(Update::Db(DbUpdate::QueryStarted { id, result }));
                }
            };
            window.send(Update::Db(DbUpdate::QueryStarted {
                id,
                result: Ok(job),
            }));
            let result = client.db_query_result(job).await.map_err(failed);
            Some(Update::Db(DbUpdate::Query { id, result }))
        }
        DbCommand::CancelQuery { job } => {
            if let Err(err) = client.cancel_job(job).await {
                // It may have ended meanwhile: its end still comes.
                tracing::debug!(code = err.code(), "the statement was not cancelled");
            }
            None
        }
        DbCommand::Close => {
            if let Err(err) = client.db_close().await {
                tracing::debug!(
                    code = err.code(),
                    "the browser's connections stay until they idle out"
                );
            }
            None
        }
    }
}
