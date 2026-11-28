//! The table of Symfony's `Cloner\Data`, written as text the way its
//! `CliDumper` writes it, without colors.
//!
//! `Data` is a flat list of rows. Row 0 holds the root. An item of a row is a
//! plain value, a `Stub` (a string, an array, an object, a resource, or a
//! reference, each with the row of its children), or a short array
//! `[kind => row]` (kind 1 is an array with keys, 2 a list; `0 => n` is the
//! count of items the cloner left out). Nothing is run, and the text is
//! bounded (see [`MAX_TEXT_BYTES`]).

use std::collections::{HashMap, HashSet};

use super::DumpError;
use super::php_serial::{Id, Key, Node, Tree};

/// The text of one dump stops here, and `truncated` is set.
pub const MAX_TEXT_BYTES: usize = 256 * 1024;
/// The text of one dump stops here, and `truncated` is set.
pub const MAX_TEXT_LINES: usize = 20_000;

/// Nesting is cut here. A hostile row can point at itself.
const MAX_RENDER_DEPTH: usize = 64;
/// Items looked at, so a row used again and again cannot run for long even
/// when each item writes little.
const MAX_ITEMS: usize = 400_000;

/// Stub types, from `Cloner\Stub`.
const TYPE_REF: i64 = 1;
const TYPE_STRING: i64 = 2;
const TYPE_ARRAY: i64 = 3;
const TYPE_OBJECT: i64 = 4;
const TYPE_RESOURCE: i64 = 5;
const TYPE_SCALAR: i64 = 6;
/// `Stub::class` of a binary string, and of arrays with keys and lists.
const STRING_BINARY: i64 = 1;
const ARRAY_ASSOC: i64 = 1;

pub(super) struct Rendered {
    pub text: String,
    pub truncated: bool,
}

/// The rows by their position, built once per render: one item's lookup
/// is a map hit, not a scan of the whole list. The first row of a
/// position wins, as the scan it replaces chose it.
struct RowIndex {
    places: HashMap<i64, usize>,
}

impl RowIndex {
    fn new(rows: &[(Key, Id)]) -> Self {
        let mut places = HashMap::with_capacity(rows.len());
        for (place, (key, _)) in rows.iter().enumerate() {
            if let Key::Int(position) = key {
                places.entry(*position).or_insert(place);
            }
        }
        Self { places }
    }
}

/// The text of the item at row `position`, key `key`, of `rows`.
pub(super) fn render(
    tree: &Tree,
    rows: &[(Key, Id)],
    position: i64,
    key: &Key,
) -> Result<Rendered, DumpError> {
    let mut writer = Writer {
        tree,
        rows,
        index: RowIndex::new(rows),
        out: String::new(),
        lines: 0,
        stopped: false,
        items: 0,
        seen: HashSet::new(),
        refs: HashMap::new(),
    };
    let root = writer
        .row(position)
        .and_then(|row| row.iter().find(|(k, _)| k == key))
        .map(|&(_, id)| id)
        .ok_or(DumpError::Invalid("the root item is missing"))?;
    writer.value(root, 0, true)?;
    Ok(Rendered {
        text: writer.out,
        truncated: writer.stopped,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Assoc,
    Indexed,
    Object,
    Resource,
}

/// What an item is, read from its node.
enum Shape<'a> {
    Plain(String),
    Str {
        bytes: &'a [u8],
        binary: bool,
        cut: i64,
    },
    Hash(HashInfo),
    Ref {
        handle: i64,
        inner: Id,
    },
}

/// An array, an object, or a resource, with the row of its children.
struct HashInfo {
    kind: Kind,
    /// The class of an object, or the type of a resource.
    name: String,
    /// The count an array says it has, with the items left out.
    count: i64,
    handle: i64,
    ref_count: i64,
    position: i64,
    cut: i64,
}

struct Writer<'a> {
    tree: &'a Tree,
    rows: &'a [(Key, Id)],
    /// The rows by position, so [`Writer::row`] does not scan `rows`.
    index: RowIndex,
    out: String,
    lines: usize,
    stopped: bool,
    items: usize,
    /// Handles of the objects already written.
    seen: HashSet<i64>,
    /// Handles of the references already written, and their number (`&1`).
    refs: HashMap<i64, usize>,
}

impl<'a> Writer<'a> {
    fn row(&self, position: i64) -> Option<&'a [(Key, Id)]> {
        let (_, id) = self.rows.get(*self.index.places.get(&position)?)?;
        match self.tree.get(*id) {
            Node::Array(entries) => Some(entries),
            _ => None,
        }
    }

    fn stop(&mut self) {
        self.stopped = true;
    }

    /// Text on the current line, cut at the limit. It holds no line break.
    fn push(&mut self, text: &str) {
        if self.stopped {
            return;
        }
        let room = MAX_TEXT_BYTES.saturating_sub(self.out.len());
        if text.len() > room {
            let mut end = room;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            self.out.push_str(&text[..end]);
            self.stop();
        } else {
            self.out.push_str(text);
        }
    }

    fn newline(&mut self, depth: usize) {
        if self.stopped {
            return;
        }
        self.lines += 1;
        if self.lines >= MAX_TEXT_LINES || self.out.len() >= MAX_TEXT_BYTES {
            self.stop();
            return;
        }
        self.out.push('\n');
        self.indent(depth);
    }

    fn indent(&mut self, depth: usize) {
        for _ in 0..depth {
            self.push("  ");
        }
    }

    fn int(&self, entries: &[(Key, Id)], name: &[u8], default: i64) -> i64 {
        match self.tree.find(entries, name).map(|id| self.tree.get(id)) {
            Some(Node::Int(n)) => *n,
            _ => default,
        }
    }

    fn text_of(&self, id: Id) -> Option<String> {
        match self.tree.get(id) {
            Node::Str(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
            Node::Int(n) => Some(n.to_string()),
            _ => None,
        }
    }

    /// Reads what the node `id` is.
    fn shape(&self, id: Id) -> Result<Shape<'a>, DumpError> {
        let tree = self.tree;
        Ok(match tree.get(id) {
            Node::Null => Shape::Plain("null".into()),
            Node::Bool(flag) => Shape::Plain(flag.to_string()),
            Node::Int(n) => Shape::Plain(n.to_string()),
            Node::Float(text) => Shape::Plain(float(text)),
            Node::Enum(bytes) => Shape::Plain(String::from_utf8_lossy(bytes).replace(':', "::")),
            Node::Str(bytes) => Shape::Str {
                bytes,
                binary: false,
                cut: 0,
            },
            Node::Alias(_) => return Err(DumpError::Invalid("an alias is not an item")),
            Node::Array(entries) => match entries.last() {
                None => Shape::Plain("[]".into()),
                Some(_) => {
                    // `[kind => row]`, with `0 => cut` when items were left out.
                    let mut kind = ARRAY_ASSOC;
                    let mut position = 0;
                    let mut cut = 0;
                    for (key, value) in entries {
                        let number = match tree.get(*value) {
                            Node::Int(n) => *n,
                            _ => return Err(DumpError::Invalid("a short array has a bad entry")),
                        };
                        match key {
                            Key::Int(0) => cut = number,
                            Key::Int(k) => {
                                kind = *k;
                                position = number;
                            }
                            Key::Str(_) => {
                                return Err(DumpError::Invalid("a short array has a bad key"));
                            }
                        }
                    }
                    let in_row = self
                        .row(position)
                        .map_or(0, |row| i64::try_from(row.len()).unwrap_or(i64::MAX));
                    Shape::Hash(HashInfo {
                        kind: if kind == ARRAY_ASSOC {
                            Kind::Assoc
                        } else {
                            Kind::Indexed
                        },
                        name: String::new(),
                        count: cut.saturating_add(if position > 0 { in_row } else { 0 }),
                        handle: 0,
                        ref_count: 0,
                        position,
                        cut,
                    })
                }
            },
            Node::Object { class, props } => {
                if !is_stub(class) {
                    return Err(DumpError::Invalid("an object that is not a stub"));
                }
                self.stub(props)?
            }
        })
    }

    fn stub(&self, props: &'a [(Key, Id)]) -> Result<Shape<'a>, DumpError> {
        let tree = self.tree;
        let kind = self.int(props, b"type", TYPE_REF);
        let cut = self.int(props, b"cut", 0);
        let handle = self.int(props, b"handle", 0);
        let ref_count = self.int(props, b"refCount", 0);
        let position = self.int(props, b"position", 0);
        let class = tree.find(props, b"class");
        let value = tree.find(props, b"value");
        let class_name = class.and_then(|id| self.text_of(id)).unwrap_or_default();
        let class_number = class.map_or(0, |id| match tree.get(id) {
            Node::Int(n) => *n,
            _ => 0,
        });
        Ok(match kind {
            TYPE_REF => Shape::Ref {
                handle,
                inner: value.ok_or(DumpError::Invalid("a reference without a value"))?,
            },
            TYPE_STRING => match value.map(|id| tree.get(id)) {
                Some(Node::Str(bytes)) => Shape::Str {
                    bytes,
                    binary: class_number == STRING_BINARY,
                    cut,
                },
                _ => return Err(DumpError::Invalid("a string stub without a string")),
            },
            // For an array, `class` says keys or list, and `value` the count.
            TYPE_ARRAY => Shape::Hash(HashInfo {
                kind: if class_number == ARRAY_ASSOC {
                    Kind::Assoc
                } else {
                    Kind::Indexed
                },
                name: String::new(),
                count: value.map_or(0, |id| match tree.get(id) {
                    Node::Int(n) => *n,
                    _ => 0,
                }),
                handle,
                ref_count,
                position,
                cut,
            }),
            TYPE_OBJECT | TYPE_RESOURCE => Shape::Hash(HashInfo {
                kind: if kind == TYPE_OBJECT {
                    Kind::Object
                } else {
                    Kind::Resource
                },
                name: class_name,
                count: 0,
                handle,
                ref_count,
                position,
                cut,
            }),
            TYPE_SCALAR => {
                // A constant or a similar stub: its `attr['value']`, else its name.
                let from_attr = tree
                    .find(props, b"attr")
                    .and_then(|attr| match tree.get(attr) {
                        Node::Array(entries) => tree.find(entries, b"value"),
                        _ => None,
                    });
                match from_attr.map(|id| tree.get(id)) {
                    Some(Node::Null) => Shape::Plain("null".into()),
                    Some(Node::Bool(flag)) => Shape::Plain(flag.to_string()),
                    Some(Node::Int(n)) => Shape::Plain(n.to_string()),
                    Some(Node::Float(text)) => Shape::Plain(float(text)),
                    Some(Node::Str(bytes)) => Shape::Plain(escape(bytes, false)),
                    _ => Shape::Plain(escape(class_name.as_bytes(), false)),
                }
            }
            _ => return Err(DumpError::Invalid("an unknown stub type")),
        })
    }

    /// One item, from the current position on the line. `expand` is false
    /// for an item that stands for one already written.
    fn value(&mut self, id: Id, depth: usize, expand: bool) -> Result<(), DumpError> {
        if self.stopped {
            return Ok(());
        }
        self.items += 1;
        if self.items > MAX_ITEMS {
            self.stop();
            return Ok(());
        }
        if depth > MAX_RENDER_DEPTH {
            self.push("…");
            return Ok(());
        }
        match self.shape(id)? {
            Shape::Plain(text) => self.push(&text),
            Shape::Str { bytes, binary, cut } => self.string(bytes, binary, cut, depth),
            Shape::Ref { handle, inner } => {
                let next = self.refs.len() + 1;
                let first = !self.refs.contains_key(&handle);
                let number = *self.refs.entry(handle).or_insert(next);
                self.push(&format!("&{number} "));
                self.value(inner, depth, expand && first)?;
            }
            Shape::Hash(info) => {
                // An object seen before is named, not written again.
                let mut expand = expand;
                if info.ref_count > 0 && info.handle > 0 && !self.seen.insert(info.handle) {
                    expand = false;
                }
                self.hash(&info, depth, expand)?;
            }
        }
        Ok(())
    }

    fn hash(&mut self, info: &HashInfo, depth: usize, expand: bool) -> Result<(), DumpError> {
        let kind = info.kind;
        let children = if info.position > 0 && expand {
            self.row(info.position).filter(|row| !row.is_empty())
        } else {
            None
        };
        match kind {
            Kind::Object => {
                if !info.name.is_empty() && info.name != "stdClass" {
                    self.push(&escape(info.name.as_bytes(), false));
                    self.push(" ");
                }
                self.push("{");
                if info.handle > 0 {
                    self.push(&format!("#{}", info.handle));
                }
            }
            Kind::Resource => {
                self.push(&escape(info.name.as_bytes(), false));
                self.push(" resource");
                if info.handle > 0 {
                    self.push(&format!(" @{}", info.handle));
                }
                if children.is_some() {
                    self.push(" {");
                }
            }
            Kind::Assoc | Kind::Indexed => self.push(&format!("array:{} [", info.count)),
        }
        match children {
            Some(entries) => {
                for (key, child) in entries {
                    if self.stopped {
                        return Ok(());
                    }
                    self.newline(depth + 1);
                    self.key(kind, key);
                    self.value(*child, depth + 1, true)?;
                }
                if info.cut != 0 {
                    self.newline(depth + 1);
                    self.ellipsis(info.cut);
                }
                self.newline(depth);
            }
            None if info.cut != 0 => {
                self.push(" ");
                self.ellipsis(info.cut);
            }
            None => {}
        }
        match kind {
            Kind::Object => self.push("}"),
            Kind::Resource if children.is_some() => self.push("}"),
            Kind::Resource => {}
            Kind::Assoc | Kind::Indexed => self.push("]"),
        }
        Ok(())
    }

    fn ellipsis(&mut self, cut: i64) {
        self.push("…");
        if cut > 0 {
            self.push(&cut.to_string());
        }
    }

    /// The key of a child, and what comes between it and the value.
    fn key(&mut self, kind: Kind, key: &Key) {
        match (kind, key) {
            (Kind::Assoc | Kind::Indexed, Key::Int(n)) => self.push(&format!("{n} => ")),
            (Kind::Assoc | Kind::Indexed, Key::Str(bytes)) => {
                self.push(&quoted(bytes, !is_utf8(bytes)));
                self.push(" => ");
            }
            (_, Key::Int(n)) => self.push(&format!("{n}: ")),
            (_, Key::Str(bytes)) => {
                let (bullet, shown) = property(bytes, kind == Kind::Resource);
                self.push(&bullet);
                self.push(&shown);
                self.push(": ");
            }
        }
    }

    fn string(&mut self, bytes: &[u8], binary: bool, cut: i64, depth: usize) {
        if bytes.is_empty() {
            self.push("\"\"");
            self.cut_mark(cut);
            return;
        }
        let parts: Vec<&[u8]> = if binary && bytes.contains(&0) {
            vec![bytes]
        } else {
            bytes.split(|&b| b == b'\n').collect()
        };
        // "text\n" stays on one line.
        let block = parts.len() > 2 || (parts.len() == 2 && !parts[1].is_empty());
        if !block {
            self.push(&quoted(bytes, binary));
            self.cut_mark(cut);
            return;
        }
        let pad = if depth > 0 { depth + 1 } else { 0 };
        if binary {
            self.push("b");
        }
        self.push("\"\"\"");
        let last = parts.len() - 1;
        for (i, part) in parts.iter().enumerate() {
            self.newline(pad);
            self.push(&escape(part, binary));
            if i == last {
                self.newline(pad);
                self.push("\"\"\"");
            }
        }
        self.cut_mark(cut);
    }

    fn cut_mark(&mut self, cut: i64) {
        if cut != 0 {
            self.ellipsis(cut);
        }
    }
}

/// `Symfony\Component\VarDumper\Cloner\Stub` and its subclasses
/// (`ClassStub`, `CutStub`, `ConstStub`, and so on).
fn is_stub(class: &[u8]) -> bool {
    class.starts_with(b"Symfony\\Component\\VarDumper\\") && class.ends_with(b"Stub")
}

fn is_utf8(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok()
}

/// `"text"`, or `b"text"` for bytes that are not text.
fn quoted(bytes: &[u8], binary: bool) -> String {
    format!(
        "{}\"{}\"",
        if binary { "b" } else { "" },
        escape(bytes, binary)
    )
}

/// The text of `bytes` that is safe to show: line breaks, tabs, and other
/// control characters are written as `\n`, `\t`, `\x1B`, and so on.
fn escape(bytes: &[u8], binary: bool) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mark = |c: char, out: &mut String| match c {
        '\t' => out.push_str("\\t"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\u{c}' => out.push_str("\\f"),
        '\u{1b}' => out.push_str("\\e"),
        c if c.is_control() => out.push_str(&format!("\\x{:02X}", u32::from(c))),
        c => out.push(c),
    };
    if binary {
        // Symfony shows each byte as the character of that number.
        for &byte in bytes {
            mark(char::from(byte), &mut out);
        }
    } else {
        for c in String::from_utf8_lossy(bytes).chars() {
            mark(c, &mut out);
        }
    }
    out
}

/// The mark and the name of an object property, from PHP's key:
/// `name` is public, `\0*\0name` protected, `\0Class\0name` private,
/// `\0+\0name` added at run time, `\0~\0name` a note of the caster.
fn property(raw: &[u8], resource: bool) -> (String, String) {
    let shown = |name: &[u8]| escape(name, !is_utf8(name));
    if raw.first() != Some(&0) {
        return (if resource { "" } else { "+" }.into(), shown(raw));
    }
    let rest = &raw[1..];
    let Some(end) = rest.iter().position(|&b| b == 0) else {
        return ("+".into(), shown(raw));
    };
    let (owner, name) = (&rest[..end], &rest[end + 1..]);
    match owner.first() {
        Some(b'*') => ("#".into(), shown(name)),
        Some(b'+') => ("+".into(), format!("\"{}\"", shown(name))),
        Some(b'~') => (String::new(), shown(name)),
        _ => ("-".into(), shown(name)),
    }
}

/// A float as Symfony writes it: `1.0`, not `1`.
fn float(text: &str) -> String {
    let plain =
        !text.contains(['.', 'E', 'e']) && text.bytes().all(|b| b.is_ascii_digit() || b == b'-');
    if plain {
        format!("{text}.0")
    } else {
        text.to_owned()
    }
}
