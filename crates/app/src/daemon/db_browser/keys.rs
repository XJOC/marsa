//! Marsa's own Valkey, for the browser (ADR 0028, sections 6 and 8).
//!
//! Only the server this daemon runs, on `127.0.0.1`, at the port where it
//! said it took commands. The connection sends nothing but the closed list of
//! [`KeyCommand`], all reads, and reads each reply within its command's
//! limits. One connection, used by one request at a time; it closes as the
//! SQL connections do ([`super::store`]), and after any failure.
//!
//! What a key's name or value holds goes to the client that asked, and
//! nowhere else: never to the log.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use marsa_core::codes;
use marsa_core::databases::ServerKind;
use marsa_core::db_browser::resp::{Bulk, Decoder, Reply, RespError};
use marsa_core::db_browser::{
    KEY_BYTES, KEY_ELEMENTS, KEY_PAGE, KeyCommand, PATTERN_BYTES, SCAN_CALLS, VALKEY_DATABASES,
    bulk_cell, bulks_reply, count_reply, pairs, scan_reply,
};
use marsa_core::types::{
    DbCell, DbKeyDetail, DbKeyKind, DbKeyRequest, DbKeyValue, DbKeys, DbKeysRequest,
    DbValkeyDatabase,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

use super::{log, unavailable, within};
use crate::daemon::http::Rejected;
use crate::daemon::http::routes::HttpError;
use crate::daemon::state::Daemon;

/// Bytes read from the socket at a time.
const CHUNK: usize = 16 * 1024;

/// An open connection to Valkey.
pub(super) struct Conn {
    stream: TcpStream,
    chunk: Vec<u8>,
}

impl Conn {
    async fn open(port: u16) -> Result<Self, HttpError> {
        let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|err| unavailable(err.to_string()))?;
        Ok(Self {
            stream,
            chunk: vec![0; CHUNK],
        })
    }

    /// Sends `command` and reads its one reply. An error reply is an error:
    /// no command here has a use for one.
    async fn ask(&mut self, command: KeyCommand<'_>) -> Result<Reply, HttpError> {
        let io = |err: std::io::Error| unavailable(err.to_string());
        self.stream.write_all(&command.encode()).await.map_err(io)?;
        let mut decoder = Decoder::new(command.limits());
        loop {
            let read = self.stream.read(&mut self.chunk).await.map_err(io)?;
            if read == 0 {
                return Err(unavailable("Valkey closed the connection".to_owned()));
            }
            let bytes = self.chunk.get(..read).unwrap_or_default();
            match decoder.feed(bytes).map_err(unreadable)? {
                Some(Reply::Error(said)) => return Err(refused(said)),
                Some(reply) => return Ok(reply),
                None => {}
            }
        }
    }

    async fn select(&mut self, db: u8) -> Result<(), HttpError> {
        match self.ask(KeyCommand::Select(db)).await? {
            Reply::Status(status) if status == "OK" => Ok(()),
            _ => Err(unavailable("SELECT did not answer OK".to_owned())),
        }
    }

    async fn count(&mut self, command: KeyCommand<'_>) -> Result<u64, HttpError> {
        count_reply(self.ask(command).await?).map_err(unreadable)
    }

    /// The elements of a collection through `command`'s cursor, until there
    /// are `wanted` or none is left.
    async fn collect<'a>(
        &mut self,
        wanted: usize,
        command: impl Fn(u64) -> KeyCommand<'a>,
    ) -> Result<Vec<Bulk>, HttpError> {
        let mut found = Vec::new();
        let mut cursor = 0;
        for _ in 0..SCAN_CALLS {
            let reply = self.ask(command(cursor)).await?;
            let (next, bulks) = scan_reply(reply).map_err(unreadable)?;
            found.extend(bulks);
            cursor = next;
            if cursor == 0 || found.len() >= wanted {
                break;
            }
        }
        found.truncate(wanted);
        Ok(found)
    }
}

/// A reply past a limit, or one that is not RESP2 or not the command's.
fn unreadable(err: RespError) -> HttpError {
    unavailable(err.to_string())
}

/// Valkey's own refusal, as `detail`. A password set on the default user is
/// the one refusal with its own code.
fn refused(said: String) -> HttpError {
    let no_access = ["NOAUTH", "WRONGPASS", "NOPERM"];
    if !no_access.iter().any(|word| said.starts_with(word)) {
        return unavailable(said);
    }
    let mut err = HttpError::new(
        StatusCode::BAD_GATEWAY,
        codes::DB_SIGN_IN_FAILED,
        "Valkey refused a connection with no password",
    );
    err.detail = Some(said);
    err
}

/// The port of the Valkey this daemon runs, once it took commands.
fn port(daemon: &Daemon) -> Result<u16, HttpError> {
    if daemon.shutdown().is_cancelled() {
        return Err(unavailable("marsad is stopping".to_owned()));
    }
    daemon
        .server(ServerKind::Valkey)
        .ready_port(daemon)
        .ok_or_else(|| {
            HttpError::new(
                StatusCode::CONFLICT,
                codes::DB_SERVER_NOT_RUNNING,
                "the server is not running",
            )
        })
}

/// Runs `work` on the browser's connection, with database `db` chosen. As the
/// SQL servers' `with`: the connection goes back only after `work` succeeded
/// or refused the request itself, so one that failed, timed out, or was
/// dropped with its request is closed.
async fn with<T>(
    daemon: &Arc<Daemon>,
    db: u8,
    work: impl AsyncFnOnce(&mut Conn) -> Result<T, HttpError>,
) -> Result<T, HttpError> {
    if db >= VALKEY_DATABASES {
        return Err(HttpError::new(
            StatusCode::BAD_REQUEST,
            codes::DB_SOURCE_INVALID,
            "Valkey has no database by that number",
        ));
    }
    port(daemon)?;
    let slot = daemon.db_browser().valkey_slot(daemon);
    // The token of the one connection: held across `.await` on purpose
    // (store.rs).
    let mut held = slot.lock().await;
    // Again after the wait: the server may have stopped meanwhile.
    let port = port(daemon)?;
    // A kept connection proves itself with `SELECT`; one that no longer
    // answers (its server restarted) is replaced.
    let kept = match held.take() {
        Some(mut conn) => conn.select(db).await.is_ok().then_some(conn),
        None => None,
    };
    let mut conn = match kept {
        Some(conn) => conn,
        None => {
            let mut conn = Conn::open(port).await?;
            conn.select(db).await?;
            conn
        }
    };
    let result = work(&mut conn).await;
    let known = match &result {
        Ok(_) => true,
        Err(err) => err.status.is_client_error(),
    };
    if known {
        *held = Some(conn);
    }
    result
}

/// The databases that hold keys, and always database 0.
pub(super) async fn databases(daemon: &Arc<Daemon>) -> Result<Vec<DbValkeyDatabase>, HttpError> {
    with(daemon, 0, async |conn| {
        let mut databases = Vec::new();
        for db in 0..VALKEY_DATABASES {
            conn.select(db).await?;
            let keys = conn.count(KeyCommand::DbSize).await?;
            if db == 0 || keys > 0 {
                databases.push(DbValkeyDatabase { db, keys });
            }
        }
        Ok(databases)
    })
    .await
}

/// `POST /v1/db/keys`.
pub(super) async fn keys(
    State(daemon): State<Arc<Daemon>>,
    body: Result<Json<DbKeysRequest>, JsonRejection>,
) -> Result<Json<DbKeys>, HttpError> {
    let Json(request) = body.map_err(|err| err.reject(codes::INVALID_BODY))?;
    if request
        .pattern
        .as_ref()
        .is_some_and(|pattern| pattern.len() > PATTERN_BYTES)
    {
        return Err(HttpError::new(
            StatusCode::BAD_REQUEST,
            codes::DB_PATTERN_TOO_LONG,
            "the search pattern is too long",
        ));
    }
    let started = Instant::now();
    let found = within(with(&daemon, request.db, async |conn| {
        page(conn, &request).await
    }))
    .await?;
    log(source(request.db), "keys", started, found.keys.len());
    Ok(Json(found))
}

/// A database as the log and the command line name it.
fn source(db: u8) -> String {
    format!("{}/{db}", ServerKind::Valkey.id())
}

/// `SCAN` from the request's cursor until the page is full, the database ends,
/// or [`SCAN_CALLS`] found too little.
async fn page(conn: &mut Conn, request: &DbKeysRequest) -> Result<DbKeys, HttpError> {
    let mut found = DbKeys::default();
    let mut cursor = request.cursor;
    let mut seen = 0;
    for _ in 0..SCAN_CALLS {
        let command = KeyCommand::Scan {
            cursor,
            pattern: request.pattern.as_deref(),
            kind: request.kind,
        };
        let reply = conn.ask(command).await?;
        let (next, names) = scan_reply(reply).map_err(unreadable)?;
        seen += names.len();
        for name in names {
            match key_name(name) {
                Some(name) => found.keys.push(name),
                None => found.unshown += 1,
            }
        }
        cursor = next;
        if cursor == 0 || seen >= KEY_PAGE {
            break;
        }
    }
    found.cursor = cursor;
    found.total = conn.count(KeyCommand::DbSize).await?;
    Ok(found)
}

/// A key's name the browser can show and open: whole, short, and UTF-8.
fn key_name(name: Bulk) -> Option<String> {
    let whole = name.len <= KEY_BYTES as u64 && name.bytes.len() as u64 == name.len;
    whole.then(|| String::from_utf8(name.bytes).ok()).flatten()
}

/// `POST /v1/db/key`.
pub(super) async fn key(
    State(daemon): State<Arc<Daemon>>,
    body: Result<Json<DbKeyRequest>, JsonRejection>,
) -> Result<Json<DbKeyDetail>, HttpError> {
    let Json(DbKeyRequest { db, key }) = body.map_err(|err| err.reject(codes::INVALID_BODY))?;
    if key.len() > KEY_BYTES {
        return Err(HttpError::new(
            StatusCode::BAD_REQUEST,
            codes::DB_SOURCE_INVALID,
            "the key's name is too long",
        ));
    }
    let started = Instant::now();
    let detail = within(with(&daemon, db, async |conn| detail(conn, &key).await)).await?;
    log(source(db), "key", started, elements(&detail.value));
    Ok(Json(detail))
}

fn elements(value: &DbKeyValue) -> usize {
    match value {
        DbKeyValue::String { .. } => 1,
        DbKeyValue::Hash { fields, .. } => fields.len(),
        DbKeyValue::List { items, .. } => items.len(),
        DbKeyValue::Set { members, .. } => members.len(),
        DbKeyValue::Zset { members, .. } => members.len(),
        DbKeyValue::Other { .. } => 0,
    }
}

fn gone() -> HttpError {
    HttpError::new(
        StatusCode::NOT_FOUND,
        codes::DB_KEY_NOT_FOUND,
        "the key is gone",
    )
}

/// A key's type, its value's start, and, read last, its time to live: a key
/// that expired between the reads answers `-2` there, so what was read of it
/// is never shown as its value.
async fn detail(conn: &mut Conn, key: &str) -> Result<DbKeyDetail, HttpError> {
    let Reply::Status(word) = conn.ask(KeyCommand::Type(key)).await? else {
        return Err(unavailable("TYPE did not answer a type".to_owned()));
    };
    if word == "none" {
        return Err(gone());
    }
    let value = match DbKeyKind::from_word(&word) {
        Some(DbKeyKind::String) => DbKeyValue::String {
            value: string(conn, key).await?,
        },
        Some(DbKeyKind::Hash) => {
            let total = conn.count(KeyCommand::HLen(key)).await?;
            let flat = conn
                .collect(2 * KEY_ELEMENTS, |cursor| KeyCommand::HScan { key, cursor })
                .await?;
            let fields = pairs(flat)
                .map_err(unreadable)?
                .iter()
                .map(|(field, value)| (bulk_cell(field), bulk_cell(value)))
                .collect();
            DbKeyValue::Hash { fields, total }
        }
        Some(DbKeyKind::List) => {
            let total = conn.count(KeyCommand::LLen(key)).await?;
            let reply = conn.ask(KeyCommand::LRange(key)).await?;
            let items = cells(bulks_reply(reply).map_err(unreadable)?);
            DbKeyValue::List { items, total }
        }
        Some(DbKeyKind::Set) => {
            let total = conn.count(KeyCommand::SCard(key)).await?;
            let members = conn
                .collect(KEY_ELEMENTS, |cursor| KeyCommand::SScan { key, cursor })
                .await?;
            DbKeyValue::Set {
                members: cells(members),
                total,
            }
        }
        Some(DbKeyKind::Zset) => {
            let total = conn.count(KeyCommand::ZCard(key)).await?;
            let reply = conn.ask(KeyCommand::ZRange(key)).await?;
            let flat = bulks_reply(reply).map_err(unreadable)?;
            let members = pairs(flat)
                .map_err(unreadable)?
                .into_iter()
                .take(KEY_ELEMENTS)
                .map(|(member, score)| {
                    let score = String::from_utf8_lossy(&score.bytes).into_owned();
                    (bulk_cell(&member), score)
                })
                .collect();
            DbKeyValue::Zset { members, total }
        }
        Some(DbKeyKind::Stream) | None => DbKeyValue::Other { name: word },
    };
    let ttl = match conn.ask(KeyCommand::Ttl(key)).await? {
        Reply::Integer(-2) => return Err(gone()),
        Reply::Integer(-1) => None,
        reply => Some(count_reply(reply).map_err(unreadable)?),
    };
    Ok(DbKeyDetail { ttl, value })
}

/// The start of a string, and its whole length.
async fn string(conn: &mut Conn, key: &str) -> Result<DbCell, HttpError> {
    let length = conn.count(KeyCommand::StrLen(key)).await?;
    let Reply::Bulk(mut start) = conn.ask(KeyCommand::GetRange(key)).await? else {
        return Err(unavailable("GETRANGE did not answer a string".to_owned()));
    };
    start.len = start.len.max(length);
    Ok(bulk_cell(&start))
}

fn cells(mut bulks: Vec<Bulk>) -> Vec<DbCell> {
    bulks.truncate(KEY_ELEMENTS);
    bulks.iter().map(bulk_cell).collect()
}

#[cfg(test)]
mod tests;
