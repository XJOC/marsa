//! A bounded reader of PHP's `serialize` text, for lines that come from any
//! local process.
//!
//! It never runs PHP and never builds objects: `O:` becomes a class name and
//! a list of properties, nothing more. It knows `N`, `b`, `i`, `d`, `s`, `a`,
//! `O`, `r`, `R`, and `E`. Any other tag (`C:` custom serialization, `o:`,
//! `S:`, `x:`) is refused.
//!
//! Every value takes the next number, as PHP counts them, except `R:`. That
//! lets `r:N` and `R:N` point back at an earlier value. Values live in one
//! list and point at each other by [`Id`], so a reference costs one number
//! and never copies a tree: a line that refers to itself cannot grow.

use super::DumpError;

/// Nesting deeper than this is refused, so a hostile line cannot overflow the
/// stack.
pub const MAX_DEPTH: usize = 512;

/// More values than this in one line are refused. A line is at most 1 MiB,
/// and the shortest value (`N;`) takes 2 bytes.
pub const MAX_NODES: usize = 300_000;

/// The longest text of a float, `-1.7976931348623157E+308` is 24.
const MAX_FLOAT_TEXT: usize = 40;

/// The most digits of an integer or a length, so no scan runs far.
const MAX_NUMBER_TEXT: usize = 24;

/// The shortest entry of an array: `i:0;N;`.
const MIN_ENTRY_BYTES: usize = 6;

/// The place of a value in a [`Tree`]. Its order is PHP's: the first value is
/// number 1 for `r:1;`, and `Id(0)` here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Id(usize);

/// The key of an array entry or of an object property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Int(i64),
    Str(Vec<u8>),
}

impl Key {
    /// The property name without PHP's marks: `\0*\0age` and `\0User\0age`
    /// are `age`. Other keys are returned as they are.
    pub fn name(&self) -> &[u8] {
        match self {
            Self::Str(bytes) if bytes.first() == Some(&0) => bytes[1..]
                .iter()
                .position(|&b| b == 0)
                .map_or(&bytes[..], |end| &bytes[end + 2..]),
            Self::Str(bytes) => bytes,
            Self::Int(_) => &[],
        }
    }
}

/// One value.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Null,
    Bool(bool),
    Int(i64),
    /// The text PHP wrote, such as `3.14`, `1.0E+25`, `INF`, or `NAN`. Only
    /// characters of a number are kept, so it is safe to show.
    Float(String),
    /// Bytes, not necessarily UTF-8.
    Str(Vec<u8>),
    Array(Vec<(Key, Id)>),
    Object {
        class: Vec<u8>,
        props: Vec<(Key, Id)>,
    },
    /// `E:` an enum case, `Class:Case`.
    Enum(Vec<u8>),
    /// `r:N;` the value with that number. It always points at an earlier one.
    Alias(Id),
}

/// A line read in full.
#[derive(Debug)]
pub struct Tree {
    nodes: Vec<Node>,
}

impl Tree {
    /// Reads one value, and nothing after it.
    pub fn parse(input: &[u8]) -> Result<Self, DumpError> {
        let mut parser = Parser {
            input,
            pos: 0,
            nodes: Vec::new(),
        };
        parser.value(0)?;
        if parser.pos != input.len() {
            return Err(invalid("bytes after the value"));
        }
        Ok(Self {
            nodes: parser.nodes,
        })
    }

    /// The first value: the whole line.
    pub fn root(&self) -> Id {
        Id(0)
    }

    /// The value at `id`, with aliases followed. An alias points at an
    /// earlier value, so this ends.
    pub fn get(&self, mut id: Id) -> &Node {
        loop {
            match self.nodes.get(id.0) {
                Some(Node::Alias(target)) => id = *target,
                Some(node) => return node,
                // `Id`s only come from this tree.
                None => return &Node::Null,
            }
        }
    }

    /// The value of the entry named `name` (see [`Key::name`]).
    pub fn find(&self, entries: &[(Key, Id)], name: &[u8]) -> Option<Id> {
        entries
            .iter()
            .find(|(key, _)| key.name() == name)
            .map(|&(_, id)| id)
    }
}

fn invalid(reason: &'static str) -> DumpError {
    DumpError::Invalid(reason)
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    nodes: Vec<Node>,
}

impl Parser<'_> {
    fn rest(&self) -> &[u8] {
        &self.input[self.pos..]
    }

    fn expect(&mut self, bytes: &[u8]) -> Result<(), DumpError> {
        if self.rest().starts_with(bytes) {
            self.pos += bytes.len();
            Ok(())
        } else {
            Err(invalid("unexpected byte"))
        }
    }

    /// The bytes up to `end` (not included), then past `end`. At most `max`
    /// bytes are looked at.
    fn until(&mut self, end: u8, max: usize) -> Result<&[u8], DumpError> {
        let window = &self.input[self.pos..self.input.len().min(self.pos + max + 1)];
        let len = window
            .iter()
            .position(|&b| b == end)
            .ok_or_else(|| invalid("a number is not closed"))?;
        let start = self.pos;
        self.pos += len + 1;
        Ok(&self.input[start..start + len])
    }

    fn integer(&mut self, end: u8) -> Result<i64, DumpError> {
        let text = self.until(end, MAX_NUMBER_TEXT)?;
        std::str::from_utf8(text)
            .ok()
            .and_then(|text| text.parse().ok())
            .ok_or_else(|| invalid("not an integer"))
    }

    /// A length or a count: not negative, and checked against what is left
    /// by the caller before anything is reserved.
    fn size(&mut self, end: u8) -> Result<usize, DumpError> {
        usize::try_from(self.integer(end)?).map_err(|_| invalid("a negative size"))
    }

    /// `LEN:"bytes"`, the part of `s:` and of `O:` and `E:` after the tag.
    fn quoted(&mut self) -> Result<Vec<u8>, DumpError> {
        let len = self.size(b':')?;
        self.expect(b"\"")?;
        // The text, the closing quote: a declared length that does not fit
        // is refused before any memory is reserved.
        if len > self.rest().len().saturating_sub(1) {
            return Err(invalid("a string is longer than the line"));
        }
        let bytes = self.input[self.pos..self.pos + len].to_vec();
        self.pos += len;
        self.expect(b"\"")?;
        Ok(bytes)
    }

    fn push(&mut self, node: Node) -> Result<Id, DumpError> {
        if self.nodes.len() >= MAX_NODES {
            return Err(invalid("too many values"));
        }
        self.nodes.push(node);
        Ok(Id(self.nodes.len() - 1))
    }

    /// One value. Returns where it is: for `R:` that is an earlier value,
    /// otherwise a new one.
    fn value(&mut self, depth: usize) -> Result<Id, DumpError> {
        if depth > MAX_DEPTH {
            return Err(invalid("nested too deep"));
        }
        let Some(&tag) = self.rest().first() else {
            return Err(invalid("the line ends early"));
        };
        if tag == b'N' {
            self.expect(b"N;")?;
            return self.push(Node::Null);
        }
        let Some(&b':') = self.rest().get(1) else {
            return Err(invalid("unknown tag"));
        };
        self.pos += 2;
        match tag {
            b'b' => {
                let flag = self.until(b';', 1)?;
                match flag {
                    b"0" => self.push(Node::Bool(false)),
                    b"1" => self.push(Node::Bool(true)),
                    _ => Err(invalid("not a boolean")),
                }
            }
            b'i' => {
                let value = self.integer(b';')?;
                self.push(Node::Int(value))
            }
            b'd' => {
                let text = self.until(b';', MAX_FLOAT_TEXT)?;
                let text = float_text(text).ok_or_else(|| invalid("not a float"))?;
                self.push(Node::Float(text))
            }
            b's' => {
                let bytes = self.quoted()?;
                self.expect(b";")?;
                self.push(Node::Str(bytes))
            }
            b'E' => {
                let bytes = self.quoted()?;
                self.expect(b";")?;
                self.push(Node::Enum(bytes))
            }
            b'a' => {
                // Reserved first, so the array is number N and its entries follow.
                let id = self.push(Node::Null)?;
                let entries = self.entries(depth)?;
                self.nodes[id.0] = Node::Array(entries);
                Ok(id)
            }
            b'O' => {
                let id = self.push(Node::Null)?;
                let class = self.quoted()?;
                self.expect(b":")?;
                let props = self.entries(depth)?;
                self.nodes[id.0] = Node::Object { class, props };
                Ok(id)
            }
            b'r' => {
                // This alias is a value too, and points at an earlier one.
                let number = self.size(b';')?;
                let own = self.nodes.len();
                if number == 0 || number > own {
                    return Err(invalid("r: points at no earlier value"));
                }
                self.push(Node::Alias(Id(number - 1)))
            }
            b'R' => {
                // The same value again, not a new one.
                let number = self.size(b';')?;
                if number == 0 || number > self.nodes.len() {
                    return Err(invalid("R: points at no value"));
                }
                Ok(Id(number - 1))
            }
            _ => Err(invalid("unknown tag")),
        }
    }

    /// `COUNT:{key value ...}` of an array or an object.
    fn entries(&mut self, depth: usize) -> Result<Vec<(Key, Id)>, DumpError> {
        let count = self.size(b':')?;
        self.expect(b"{")?;
        // A count the rest of the line cannot hold is refused before
        // the list is reserved.
        if count > self.rest().len() / MIN_ENTRY_BYTES {
            return Err(invalid("more entries than the line can hold"));
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let key = self.key()?;
            let id = self.value(depth + 1)?;
            entries.push((key, id));
        }
        self.expect(b"}")?;
        Ok(entries)
    }

    /// A key is an `i:` or an `s:`, and is not a value: it has no number.
    fn key(&mut self) -> Result<Key, DumpError> {
        if self.rest().starts_with(b"i:") {
            self.pos += 2;
            Ok(Key::Int(self.integer(b';')?))
        } else if self.rest().starts_with(b"s:") {
            self.pos += 2;
            let bytes = self.quoted()?;
            self.expect(b";")?;
            Ok(Key::Str(bytes))
        } else {
            Err(invalid("a key is not an integer or a string"))
        }
    }
}

/// `text` if it is a number as PHP writes a float, `INF`, `-INF`, or `NAN`.
fn float_text(text: &[u8]) -> Option<String> {
    let number = !text.is_empty()
        && text
            .iter()
            .all(|&b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'E' | b'e'));
    if number || matches!(text, b"INF" | b"-INF" | b"NAN") {
        // Only ASCII was accepted.
        String::from_utf8(text.to_vec()).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Tree, DumpError> {
        Tree::parse(text.as_bytes())
    }

    fn root(text: &str) -> Node {
        let tree = parse(text).expect("parses");
        tree.get(tree.root()).clone()
    }

    #[test]
    fn scalars() {
        assert_eq!(root("N;"), Node::Null);
        assert_eq!(root("b:1;"), Node::Bool(true));
        assert_eq!(root("b:0;"), Node::Bool(false));
        assert_eq!(root("i:-42;"), Node::Int(-42));
        assert_eq!(root("d:3.14;"), Node::Float("3.14".into()));
        assert_eq!(root("d:1.0E+25;"), Node::Float("1.0E+25".into()));
        assert_eq!(root("d:-INF;"), Node::Float("-INF".into()));
        assert_eq!(root("d:NAN;"), Node::Float("NAN".into()));
        assert_eq!(root("s:3:\"a\"b\";"), Node::Str(b"a\"b".to_vec()));
        assert_eq!(root("s:0:\"\";"), Node::Str(Vec::new()));
        assert_eq!(root("E:8:\"Suit:Ace\";"), Node::Enum(b"Suit:Ace".to_vec()));
    }

    #[test]
    fn arrays_and_objects_keep_their_order_and_names() {
        let tree =
            parse("a:2:{s:1:\"k\";i:1;i:7;O:3:\"Foo\":1:{s:6:\"\0*\0age\";N;}}").expect("parses");
        let Node::Array(items) = tree.get(tree.root()) else {
            panic!("not an array");
        };
        assert_eq!(items[0].0, Key::Str(b"k".to_vec()));
        assert_eq!(items[1].0, Key::Int(7));
        let Node::Object { class, props } = tree.get(items[1].1) else {
            panic!("not an object");
        };
        assert_eq!(class, b"Foo");
        assert_eq!(props[0].0.name(), b"age");
        assert_eq!(tree.find(props, b"age"), Some(props[0].1));
    }

    #[test]
    fn key_names_drop_the_marks_of_php() {
        assert_eq!(Key::Str(b"\0*\0age".to_vec()).name(), b"age");
        assert_eq!(Key::Str(b"\0User\0secret".to_vec()).name(), b"secret");
        assert_eq!(Key::Str(b"name".to_vec()).name(), b"name");
        assert_eq!(Key::Str(b"\0broken".to_vec()).name(), b"\0broken");
    }

    #[test]
    fn r_and_big_r_count_as_php_counts() {
        // 1 array, 2 string "x", 3 the alias r:2, then R:2 is the string again.
        let tree = parse("a:3:{i:0;s:1:\"x\";i:1;r:2;i:2;R:2;}").expect("parses");
        let Node::Array(items) = tree.get(tree.root()) else {
            panic!("not an array");
        };
        assert_eq!(tree.get(items[1].1), &Node::Str(b"x".to_vec()));
        assert_eq!(items[2].1, items[0].1, "R: is the same value");
        // r:3 after the alias counted: valid.
        assert!(parse("a:3:{i:0;s:1:\"x\";i:1;r:2;i:2;r:3;}").is_ok());
    }

    #[test]
    fn a_value_that_points_at_its_own_array_cannot_grow_the_tree() {
        // `R:1` inside the array it belongs to: a cycle, read as one pointer.
        let tree = parse("a:1:{i:0;R:1;}").expect("parses");
        let Node::Array(items) = tree.get(tree.root()) else {
            panic!("not an array");
        };
        assert_eq!(items[0].1, tree.root());
    }

    #[test]
    fn refusals() {
        let cases: &[(&str, &str)] = &[
            ("", "empty"),
            ("N", "no semicolon"),
            ("N;N;", "bytes after the value"),
            ("C:3:\"Foo\":0:{}", "custom serialization"),
            ("o:3:\"Foo\":0:{}", "old object tag"),
            ("S:1:\"a\";", "escaped string"),
            ("x:1;", "unknown tag"),
            ("b:2;", "bad boolean"),
            ("i:abc;", "bad integer"),
            ("i:;", "empty integer"),
            ("i:99999999999999999999;", "integer overflow"),
            ("d:1;DROP;", "bad float"),
            ("s:999999999:\"a\";", "declared length over the line"),
            ("s:-1:\"\";", "negative length"),
            ("s:2:\"a\";", "length longer than the text"),
            ("s:1:\"a\"", "string not closed"),
            ("a:999999999:{}", "declared count over the line"),
            ("a:1:{N;N;}", "key that is not a key"),
            ("a:1:{i:0;N;", "array not closed"),
            ("r:1;", "alias with nothing before"),
            ("a:1:{i:0;r:2;}", "alias to itself"),
            ("a:1:{i:0;R:9;}", "R: past the end"),
            ("a:1:{i:0;r:0;}", "r:0"),
        ];
        for (text, why) in cases {
            assert!(parse(text).is_err(), "{why}: {text:?} was accepted");
        }
    }

    #[test]
    fn nesting_over_the_limit_is_refused_and_at_the_limit_is_read() {
        let nest = |levels: usize| {
            let mut text = String::new();
            for _ in 0..levels {
                text.push_str("a:1:{i:0;");
            }
            text.push_str("N;");
            for _ in 0..levels {
                text.push('}');
            }
            text
        };
        assert!(parse(&nest(MAX_DEPTH)).is_ok());
        assert_eq!(
            parse(&nest(MAX_DEPTH + 1)).unwrap_err(),
            DumpError::Invalid("nested too deep")
        );
        // A deep line is refused without a stack overflow.
        assert!(parse(&nest(100_000)).is_err());
    }

    #[test]
    fn too_many_values_are_refused() {
        let mut text = format!("a:{}:{{", MAX_NODES);
        for i in 0..MAX_NODES {
            text.push_str(&format!("i:{i};N;"));
        }
        text.push('}');
        assert_eq!(
            parse(&text).unwrap_err(),
            DumpError::Invalid("too many values")
        );
    }

    #[test]
    fn random_bytes_never_panic() {
        // A fixed seed, so a failure repeats (xorshift64).
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let alphabet = b"NbidsaOrRE:;{}\"0123456789-.eE+\0 \xff";
        for _ in 0..20_000 {
            let len = (next() % 64) as usize;
            let bytes: Vec<u8> = (0..len)
                .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                .collect();
            let _ = Tree::parse(&bytes);
        }
        // Pure noise too.
        for _ in 0..2_000 {
            let len = (next() % 256) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let _ = Tree::parse(&bytes);
        }
    }
}
