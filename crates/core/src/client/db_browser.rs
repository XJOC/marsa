//! The database browser's calls (ADR 0028). The window and `marsa db` read
//! the databases only through these: neither has a driver of its own.

use reqwest::Method;

use super::{ClientError, DaemonClient, REQUEST_TIMEOUT};
use crate::codes;
use crate::types::{
    DbCellRequest, DbPage, DbQueryRequest, DbResult, DbRowsRequest, DbSource, DbSources, DbTable,
    DbTableRef, DbTableRequest, DbTables, DbTablesRequest, DbValue, JobOutcome,
};

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

    /// `POST /v1/db/table`: the columns of `table`.
    ///
    /// # Errors
    ///
    /// As [`Self::db_tables`]; `db_table_not_found` (404).
    pub async fn db_table(
        &self,
        source: &DbSource,
        table: &DbTableRef,
    ) -> Result<DbTable, ClientError> {
        let body = DbTableRequest {
            source: source.clone(),
            table: table.clone(),
        };
        self.call_with(Method::POST, &["v1", "db", "table"], &body)
            .await
    }

    /// `POST /v1/db/rows`: one page of a table (`marsa_core::db_browser::PAGE_ROWS`
    /// rows after `request.offset`).
    ///
    /// # Errors
    ///
    /// As [`Self::db_table`]; `db_source_invalid` (400) for a sort column the
    /// table does not have.
    pub async fn db_rows(&self, request: &DbRowsRequest) -> Result<DbPage, ClientError> {
        self.call_with(Method::POST, &["v1", "db", "rows"], request)
            .await
    }

    /// `POST /v1/db/cell`: one whole value, up to
    /// `marsa_core::db_browser::VALUE_BYTES`, by the row's primary key.
    ///
    /// # Errors
    ///
    /// As [`Self::db_table`]; `db_row_not_found` (404) when no row has the
    /// key or the table has none.
    pub async fn db_cell(&self, request: &DbCellRequest) -> Result<DbValue, ClientError> {
        self.call_with(Method::POST, &["v1", "db", "cell"], request)
            .await
    }

    /// `POST /v1/jobs/db/query`, then follows the job to its end, then
    /// `GET /v1/db/results/{id}`: the rows of one statement, and the job's
    /// id. No request waits long: the statement runs as a job (ADR 0026).
    ///
    /// # Errors
    ///
    /// Any [`ClientError`]; `db_one_statement` and `db_sql_too_long` (400),
    /// `db_busy` (409), and the job's own failure: `db_read_only`,
    /// `db_query_failed`, `db_timeout`, with the server's words in
    /// [`ClientError::detail`]. `db_result_gone` when the daemon stopped
    /// before the end, or the rows were read by another client.
    ///
    /// With `cancel_after`, the statement is cancelled once that time passed:
    /// then `db_query_cancelled`.
    pub async fn run_db_query(
        &self,
        request: &DbQueryRequest,
        cancel_after: Option<std::time::Duration>,
    ) -> Result<(u64, DbResult), ClientError> {
        let segments = ["v1", "jobs", "db", "query"];
        let (id, outcome) = self
            .run_job_with(
                &segments,
                Some(request),
                codes::DB_RESULT_GONE,
                cancel_after,
            )
            .await?;
        let JobOutcome::DbQuery(_) = outcome else {
            return Err(ClientError::bad_response(
                "the job ended with the result of another kind of job",
            ));
        };
        let result = self
            .call(Method::GET, &["v1", "db", "results", &id.to_string()])
            .await?;
        Ok((id, result))
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
