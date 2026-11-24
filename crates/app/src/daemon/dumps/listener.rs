//! The TCP listener of the dump server and its connections.
//!
//! PHP's `ServerDumper` connects, writes one line per dump
//! (`base64(serialize([$data, $context]))` and `\n`), and expects no answer.
//! Whatever connects here is some local program, so every limit is checked
//! before memory is used: a line over 1 MiB is skipped without being kept, at
//! most 16 connections are served, and a connection that sends nothing for a
//! while is closed. Nothing is ever written back.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use marsa_core::dumps::{DumpError, MAX_LINE_BYTES, parse_line};
use marsa_core::types::Dump;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::store::Store;
use crate::daemon::services::supervisor::{lock, now_ms};

/// At most this many connections at once; the next one is closed at once.
pub(super) const MAX_CONNECTIONS: usize = 16;
/// A connection that sends no byte for this long is closed.
pub(super) const IDLE: Duration = Duration::from_secs(60);
/// A line over this size is parsed on a blocking thread, so a big dump does
/// not hold the others up. Under it, parsing takes microseconds.
const BLOCKING_FROM: usize = 64 * 1024;

/// What the connections share.
pub(super) struct Shared {
    pub(super) store: Mutex<Store>,
    /// Told after each change of the store, for the `DumpsChanged` event.
    pub(super) changed: Notify,
}

/// Binds `127.0.0.1:port`, never `0.0.0.0` or `::` (AGENTS.md section 6).
pub(super) async fn bind(port: u16) -> io::Result<(TcpListener, SocketAddr)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let addr = listener.local_addr()?;
    Ok((listener, addr))
}

/// Accepts connections until `cancel`, then waits for them to end.
pub(super) async fn serve(
    listener: TcpListener,
    shared: Arc<Shared>,
    cancel: CancellationToken,
    idle: Duration,
) {
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            // Finished connections are collected as they end.
            Some(_) = connections.join_next() => {}
            accepted = listener.accept() => {
                let stream = match accepted {
                    Ok((stream, _)) => stream,
                    Err(err) => {
                        // Out of file handles, for example: wait, do not spin.
                        warn!(%err, "the dump server could not accept a connection");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                match Arc::clone(&permits).try_acquire_owned() {
                    Ok(permit) => {
                        let shared = Arc::clone(&shared);
                        let cancel = cancel.clone();
                        connections.spawn(connection(stream, shared, permit, cancel, idle));
                    }
                    // The next one is closed at once.
                    Err(_) => drop(stream),
                }
            }
        }
    }
    drop(listener);
    // Each connection ends on `cancel` too.
    while connections.join_next().await.is_some() {}
}

/// What the reader found.
enum Line {
    Complete(Vec<u8>),
    /// A line over [`MAX_LINE_BYTES`], read to its end and thrown away.
    TooLarge,
    /// The peer closed, or sent nothing for the idle time.
    End,
}

async fn connection(
    stream: TcpStream,
    shared: Arc<Shared>,
    // Held until the connection ends.
    _permit: OwnedSemaphorePermit,
    cancel: CancellationToken,
    idle: Duration,
) {
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        let line = tokio::select! {
            () = cancel.cancelled() => return,
            line = next_line(&mut reader, &mut buf, idle) => line,
        };
        let dump = match line {
            Line::End => return,
            Line::TooLarge => Ok(too_large()),
            Line::Complete(bytes) if bytes.is_empty() => continue,
            Line::Complete(bytes) => read(bytes).await,
        };
        match dump {
            Ok(dump) => keep(&shared, dump),
            // Not a dump: dropped, and the reason is for the log only.
            Err(err) => debug!(%err, "the dump server dropped a line"),
        }
    }
}

/// The next line, at most [`MAX_LINE_BYTES`] of it kept. `buf` is the
/// partial line between calls.
async fn next_line(reader: &mut BufReader<TcpStream>, buf: &mut Vec<u8>, idle: Duration) -> Line {
    let mut skipping = false;
    loop {
        // The idle time is for each wait, not for the whole line.
        let available = match timeout(idle, reader.fill_buf()).await {
            Ok(Ok(available)) if !available.is_empty() => available,
            // Closed, an error, or silent for too long. A last line without
            // its `\n` is not a whole dump, so it is dropped.
            _ => return Line::End,
        };
        let end = available.iter().position(|&b| b == b'\n');
        let chunk = &available[..end.unwrap_or(available.len())];
        if !skipping {
            if buf.len() + chunk.len() > MAX_LINE_BYTES {
                // From here on the line is only read to its end.
                skipping = true;
                buf.clear();
            } else {
                buf.extend_from_slice(chunk);
            }
        }
        let used = end.map_or(available.len(), |at| at + 1);
        reader.consume(used);
        if end.is_some() {
            return if skipping {
                Line::TooLarge
            } else {
                // A line break of CRLF: the `\r` stays and `parse_line` trims it.
                Line::Complete(std::mem::take(buf))
            };
        }
    }
}

/// `parse_line`, on a blocking thread when the line is big.
async fn read(bytes: Vec<u8>) -> Result<Dump, DumpError> {
    if bytes.len() < BLOCKING_FROM {
        return parse_line(&bytes);
    }
    tokio::task::spawn_blocking(move || parse_line(&bytes))
        .await
        .unwrap_or(Err(DumpError::Invalid("the reader stopped")))
}

/// The one entry a line over the limit leaves.
fn too_large() -> Dump {
    Dump {
        id: 0,
        received_at: 0,
        source_file: None,
        source_line: None,
        uri: None,
        method: None,
        label: None,
        text: "A dump over 1 MiB was dropped.".to_owned(),
        truncated: true,
    }
}

fn keep(shared: &Shared, dump: Dump) {
    lock(&shared.store).push(dump, now_ms());
    shared.changed.notify_one();
}
