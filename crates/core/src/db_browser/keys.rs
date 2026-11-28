//! Valkey's part of the browser (ADR 0028, section 8): the only commands
//! `marsad` sends it, their limits, and what their answers hold.
//!
//! [`KeyCommand`] is a closed list of reads. The daemon's connection takes
//! nothing else, and a client never sends a command: it names a database, a
//! pattern, and a key. Each of those is one argument of its own, with its
//! length before it, so no space or line break in it can start a command.

use super::resp::{Bulk, Limits, Reply, RespError};
use super::{VALUE_BYTES, cut_cell};
use crate::databases::valkey::command;
use crate::types::{DbCell, DbKeyKind};

/// Valkey's databases: a request names one of `0..VALKEY_DATABASES`.
pub const VALKEY_DATABASES: u8 = 16;
/// A page of keys: `SCAN` is asked again until it gave this many.
pub const KEY_PAGE: usize = 400;
/// The keys `SCAN` is asked to look at in one call. A page holds every key of
/// its calls, so this is how far past [`KEY_PAGE`] it can go.
const SCAN_COUNT: u32 = 100;
/// The most `SCAN` calls of one page: a search that matches little stops
/// here, and the user asks for more.
pub const SCAN_CALLS: u32 = 2_000;
/// The longest key name the browser lists or opens.
pub const KEY_BYTES: usize = 4 * 1024;
/// The longest search pattern.
pub const PATTERN_BYTES: usize = 1024;
/// The elements of a hash, list, set, or sorted set that travel.
pub const KEY_ELEMENTS: usize = 500;
/// Bytes of one element of a collection. Valkey cannot give part of one, so
/// the rest is skipped as it arrives.
pub const ELEMENT_BYTES: usize = 8 * 1024;

/// The limits of the reply to a command that gives names or numbers, or the
/// elements of a collection: each string cut at `keep` bytes.
const fn limits(keep: usize) -> Limits {
    Limits {
        keep,
        // 500 fields and 500 values of 8 KiB, and room for a server that
        // gives more than it was asked for.
        kept: 16 * 1024 * 1024,
        // What is read and skipped to find the next element.
        read: 64 * 1024 * 1024,
        items: 20_000,
        depth: 4,
        line: 4 * 1024,
    }
}

/// A command Marsa sends to Valkey for the browser. All of them read; none
/// writes, and none is `KEYS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCommand<'a> {
    /// Chooses the database the next commands read.
    Select(u8),
    DbSize,
    Scan {
        cursor: u64,
        pattern: Option<&'a str>,
        kind: Option<DbKeyKind>,
    },
    Type(&'a str),
    Ttl(&'a str),
    StrLen(&'a str),
    /// The first [`VALUE_BYTES`] of a string.
    GetRange(&'a str),
    LLen(&'a str),
    /// The first [`KEY_ELEMENTS`] of a list.
    LRange(&'a str),
    HLen(&'a str),
    HScan {
        key: &'a str,
        cursor: u64,
    },
    SCard(&'a str),
    SScan {
        key: &'a str,
        cursor: u64,
    },
    ZCard(&'a str),
    /// The first [`KEY_ELEMENTS`] of a sorted set, with their scores.
    ZRange(&'a str),
}

impl KeyCommand<'_> {
    /// The command as Valkey reads it. Every number but the database and a
    /// cursor is fixed here.
    pub fn encode(&self) -> Vec<u8> {
        let count = SCAN_COUNT.to_string();
        let last = (KEY_ELEMENTS - 1).to_string();
        let (number, end);
        let words: Vec<&str> = match *self {
            Self::Select(db) => {
                number = db.to_string();
                vec!["SELECT", &number]
            }
            Self::DbSize => vec!["DBSIZE"],
            Self::Scan {
                cursor,
                pattern,
                kind,
            } => {
                number = cursor.to_string();
                let mut words = vec!["SCAN", number.as_str()];
                if let Some(pattern) = pattern {
                    words.extend(["MATCH", pattern]);
                }
                words.extend(["COUNT", count.as_str()]);
                if let Some(kind) = kind {
                    words.extend(["TYPE", kind.word()]);
                }
                words
            }
            Self::Type(key) => vec!["TYPE", key],
            Self::Ttl(key) => vec!["TTL", key],
            Self::StrLen(key) => vec!["STRLEN", key],
            Self::GetRange(key) => {
                end = (VALUE_BYTES - 1).to_string();
                vec!["GETRANGE", key, "0", &end]
            }
            Self::LLen(key) => vec!["LLEN", key],
            Self::LRange(key) => vec!["LRANGE", key, "0", &last],
            Self::HLen(key) => vec!["HLEN", key],
            Self::HScan { key, cursor } => {
                number = cursor.to_string();
                vec!["HSCAN", key, &number, "COUNT", &count]
            }
            Self::SCard(key) => vec!["SCARD", key],
            Self::SScan { key, cursor } => {
                number = cursor.to_string();
                vec!["SSCAN", key, &number, "COUNT", &count]
            }
            Self::ZCard(key) => vec!["ZCARD", key],
            Self::ZRange(key) => vec!["ZRANGE", key, "0", &last, "WITHSCORES"],
        };
        command(&words)
    }

    /// What the reply to this command may weigh.
    pub fn limits(&self) -> Limits {
        limits(match self {
            Self::GetRange(_) => VALUE_BYTES,
            // A cursor and key names: one name past the longest shown, so a
            // longer one is told from it.
            Self::Scan { .. } => KEY_BYTES + 1,
            _ => ELEMENT_BYTES,
        })
    }
}

/// What the search field's `text` looks for: as typed when it holds `*`, `?`,
/// or `[`, so it is a Valkey pattern; else every key that contains it.
pub fn key_pattern(text: &str) -> Option<String> {
    if text.is_empty() {
        None
    } else if text.contains(['*', '?', '[']) {
        Some(text.to_owned())
    } else {
        // `\` escapes in a pattern, and PHP class names in keys hold it.
        Some(format!("*{}*", text.replace('\\', "\\\\")))
    }
}

/// A string of a reply as a cell. One the decoder cut keeps its start, up to
/// a whole character, and its length.
pub fn bulk_cell(bulk: &Bulk) -> DbCell {
    if bulk.bytes.len() as u64 == bulk.len {
        return cut_cell(&bulk.bytes, bulk.bytes.len());
    }
    let whole = match std::str::from_utf8(&bulk.bytes) {
        Ok(text) => text,
        // The cut fell inside the last character.
        Err(err) if err.error_len().is_none() => {
            let valid = bulk.bytes.get(..err.valid_up_to()).unwrap_or_default();
            std::str::from_utf8(valid).unwrap_or_default()
        }
        Err(_) => return DbCell::Binary { bytes: bulk.len },
    };
    DbCell::Cut {
        text: whole.to_owned(),
        bytes: bulk.len,
    }
}

/// An answer of another shape than the command's.
fn unexpected() -> RespError {
    RespError::Malformed("an answer of another shape")
}

/// A number that counts something: `DBSIZE`, `STRLEN`, `LLEN`, and the like.
pub fn count_reply(reply: Reply) -> Result<u64, RespError> {
    match reply {
        Reply::Integer(count) => u64::try_from(count).map_err(|_| unexpected()),
        _ => Err(unexpected()),
    }
}

/// The strings of an array, as `LRANGE` and `ZRANGE` answer.
pub fn bulks_reply(reply: Reply) -> Result<Vec<Bulk>, RespError> {
    let Reply::Array(values) = reply else {
        return Err(unexpected());
    };
    values
        .into_iter()
        .map(|value| match value {
            Reply::Bulk(bulk) => Ok(bulk),
            _ => Err(unexpected()),
        })
        .collect()
}

/// The answer to `SCAN`, `HSCAN`, and `SSCAN`: the cursor to go on from (0
/// at the end), and what this call found.
pub fn scan_reply(reply: Reply) -> Result<(u64, Vec<Bulk>), RespError> {
    let Reply::Array(values) = reply else {
        return Err(unexpected());
    };
    let Ok([Reply::Bulk(cursor), found]) = <[Reply; 2]>::try_from(values) else {
        return Err(unexpected());
    };
    let cursor = std::str::from_utf8(&cursor.bytes)
        .ok()
        .and_then(|text| text.parse().ok())
        .ok_or_else(unexpected)?;
    Ok((cursor, bulks_reply(found)?))
}

/// A flat list of pairs, as `HSCAN` and `ZRANGE WITHSCORES` give them.
pub fn pairs(bulks: Vec<Bulk>) -> Result<Vec<(Bulk, Bulk)>, RespError> {
    if !bulks.len().is_multiple_of(2) {
        return Err(unexpected());
    }
    let mut bulks = bulks.into_iter();
    let mut pairs = Vec::with_capacity(bulks.len() / 2);
    while let (Some(first), Some(second)) = (bulks.next(), bulks.next()) {
        pairs.push((first, second));
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests;
