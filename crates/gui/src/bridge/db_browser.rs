//! The Databases screen's reads (ADR 0028): through `marsa_core::client`,
//! like every call of the window. The window has no driver and no
//! connection of its own.

use marsa_core::client::DaemonClient;
use marsa_core::types::{DbSource, DbSources, DbTables};

use crate::model::Update;

/// What the Databases screen asks the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbCommand {
    /// What can be opened. `id` comes back with the answer.
    ReadSources { id: u64 },
    /// The tables of `source`.
    ReadTables { id: u64, source: DbSource },
    /// The screen was left: the daemon closes the browser's connections.
    Close,
}

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
}

/// Runs `command`, and the update it gives, if any.
pub(super) async fn run(client: &DaemonClient, command: DbCommand) -> Option<Update> {
    let code = |err: marsa_core::client::ClientError| err.code().to_owned();
    match command {
        DbCommand::ReadSources { id } => Some(Update::Db(DbUpdate::Sources {
            id,
            result: client.db_sources().await.map_err(code),
        })),
        DbCommand::ReadTables { id, source } => Some(Update::Db(DbUpdate::Tables {
            id,
            result: client.db_tables(&source).await.map_err(code),
        })),
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
