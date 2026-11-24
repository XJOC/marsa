//! What `dump()` sends to Marsa: one line of Symfony's dump server protocol,
//! `base64(serialize([$data, $context]))`, read into text (ADR 0027).
//!
//! The functions are pure: bytes in, a value out. They never run PHP, never
//! build objects from the data, and never panic on any input. The listener
//! and the store are the daemon's.

pub mod php_serial;
mod render;

use php_serial::{Key, Node, Tree};

use crate::types::Dump;

pub use render::{MAX_TEXT_BYTES, MAX_TEXT_LINES};

/// A line over this many bytes is not read. The listener skips it up to the
/// next line break, without keeping it.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Why a line is not a dump.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DumpError {
    #[error("a dump line is over {MAX_LINE_BYTES} bytes")]
    TooLarge,
    /// The reason is for the log, not for the user.
    #[error("not a dump of Symfony's var-dumper: {0}")]
    Invalid(&'static str),
}

impl DumpError {
    /// The stable code the clients keep a text for.
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLarge => crate::codes::DUMP_TOO_LARGE,
            Self::Invalid(_) => crate::codes::DUMP_INVALID,
        }
    }
}

/// The longest file path, URL, method, or label kept from the context. They
/// come from PHP, so they are cut and cleaned before anyone shows them.
const MAX_CONTEXT_CHARS: usize = 2048;
const MAX_LABEL_CHARS: usize = 256;

/// Reads one line the dump server got: `base64(serialize([$data, $context]))`,
/// with or without its line break. The `Dump` it returns has `id` 0, and
/// `received_at` from the dump's own clock (0 if it has none): the store sets
/// both.
pub fn parse_line(line: &[u8]) -> Result<Dump, DumpError> {
    if line.len() > MAX_LINE_BYTES {
        return Err(DumpError::TooLarge);
    }
    let line = line.trim_ascii();
    let bytes = crate::base64::decode(line).ok_or(DumpError::Invalid("not base64"))?;
    let tree = Tree::parse(&bytes)?;
    let Node::Array(top) = tree.get(tree.root()) else {
        return Err(DumpError::Invalid("the line is not [data, context]"));
    };
    let data = top
        .iter()
        .find(|(key, _)| *key == Key::Int(0))
        .map(|&(_, id)| tree.get(id))
        .ok_or(DumpError::Invalid("the line has no data"))?;
    let Node::Object { class, props } = data else {
        return Err(DumpError::Invalid("the data is not a Data object"));
    };
    if !class.ends_with(b"\\Cloner\\Data") {
        return Err(DumpError::Invalid("the data is not a Data object"));
    }
    let number = |name: &[u8], default: i64| match tree.find(props, name).map(|id| tree.get(id)) {
        Some(Node::Int(n)) => *n,
        _ => default,
    };
    let rows = match tree.find(props, b"data").map(|id| tree.get(id)) {
        Some(Node::Array(rows)) => rows,
        _ => return Err(DumpError::Invalid("the data has no rows")),
    };
    let key = match tree.find(props, b"key").map(|id| tree.get(id)) {
        Some(Node::Str(bytes)) => Key::Str(bytes.clone()),
        Some(Node::Int(n)) => Key::Int(*n),
        _ => Key::Int(0),
    };
    let rendered = render::render(&tree, rows, number(b"position", 0), &key)?;

    // The label sits in the data's own context, the rest in the line's.
    let label = text_in(
        &tree,
        tree.find(props, b"context"),
        &[b"label"],
        MAX_LABEL_CHARS,
    );
    let context = top
        .iter()
        .find(|(key, _)| *key == Key::Int(1))
        .map(|&(_, id)| id);
    let source = sub(&tree, context, b"source");
    let request = sub(&tree, context, b"request");
    let received_at = match context.and_then(|id| node_in(&tree, id, &[b"timestamp"])) {
        Some(Node::Float(text)) => seconds_to_millis(text),
        _ => 0,
    };
    Ok(Dump {
        id: 0,
        received_at,
        source_file: text_in(&tree, source, &[b"file"], MAX_CONTEXT_CHARS),
        source_line: match source.and_then(|id| node_in(&tree, id, &[b"line"])) {
            Some(Node::Int(n)) => u32::try_from(*n).ok(),
            _ => None,
        },
        uri: text_in(&tree, request, &[b"uri"], MAX_CONTEXT_CHARS),
        method: text_in(&tree, request, &[b"method"], 16),
        label,
        text: rendered.text,
        truncated: rendered.truncated,
    })
}

/// The array stored under `name` in the array at `id`.
fn sub(tree: &Tree, id: Option<php_serial::Id>, name: &[u8]) -> Option<php_serial::Id> {
    let Node::Array(entries) = tree.get(id?) else {
        return None;
    };
    let found = tree.find(entries, name)?;
    matches!(tree.get(found), Node::Array(_)).then_some(found)
}

/// The node under `path` (one name) in the array at `id`.
fn node_in<'a>(tree: &'a Tree, id: php_serial::Id, path: &[&[u8]]) -> Option<&'a Node> {
    let Node::Array(entries) = tree.get(id) else {
        return None;
    };
    let found = tree.find(entries, path.first()?)?;
    Some(tree.get(found))
}

/// A text from the context: valid UTF-8 without control characters, and at
/// most `max` characters. `None` if it is missing, not text, or empty.
fn text_in(tree: &Tree, id: Option<php_serial::Id>, path: &[&[u8]], max: usize) -> Option<String> {
    let Node::Str(bytes) = node_in(tree, id?, path)? else {
        return None;
    };
    let text: String = String::from_utf8_lossy(bytes)
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect();
    (!text.is_empty()).then_some(text)
}

/// `1795492984.900873` (seconds) as milliseconds, or 0 when it is not a time.
fn seconds_to_millis(text: &str) -> u64 {
    match text.parse::<f64>() {
        Ok(seconds) if seconds.is_finite() && (0.0..1.0e12).contains(&seconds) => {
            // In range, so the cast cannot overflow; the fraction is dropped.
            (seconds * 1000.0) as u64
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests;
