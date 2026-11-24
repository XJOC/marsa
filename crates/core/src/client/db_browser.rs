//! The database browser's calls (ADR 0028). The window and `marsa db` read
//! the databases only through these: neither has a driver of its own.

use reqwest::Method;

use super::{ClientError, DaemonClient, REQUEST_TIMEOUT};
use crate::types::{DbSource, DbSources, DbTables, DbTablesRequest};

impl DaemonClient {
    /// `GET /v1/db/sources`: Marsa's database servers and the sites' SQLite
    /// files.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`].
    pub async fn db_sources(&self) -> Result<DbSources, ClientError> {
        self.call(Method::GET, &["v1", "db", "sources"]).await
    }

    /// `POST /v1/db/tables`: the tables and views of `source`.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `db_server_not_running` (409), `site_not_found`
    /// and `db_file_not_found` (404), `db_file_outside_site` (403),
    /// `db_unavailable` (502), and `db_timeout` (504).
    pub async fn db_tables(&self, source: &DbSource) -> Result<DbTables, ClientError> {
        let body = DbTablesRequest {
            source: source.clone(),
        };
        self.call_with(Method::POST, &["v1", "db", "tables"], &body)
            .await
    }

    /// `POST /v1/db/close`: every connection of the browser closes. The window
    /// sends it when the user leaves the screen.
    ///
    /// # Errors
    ///
    /// Any [`ClientError`].
    pub async fn db_close(&self) -> Result<(), ClientError> {
        let request = self.http.post(self.url(&["v1", "db", "close"])?);
        self.send(request.timeout(REQUEST_TIMEOUT)).await?;
        Ok(())
    }
}
