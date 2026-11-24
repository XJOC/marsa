//! The user's SQL text: its statements, and the kind of each word for its
//! color. A small lexer of Marsa's own, not a parser. It knows what can hide a
//! `;`: strings, quoted names, comments, and PostgreSQL's dollar quotes. What
//! it does not know stays inside one statement, and the server judges it.
//!
//! Every place it stops at is an ASCII byte, so every range it gives starts
//! and ends at a character.

use std::ops::Range;

use super::Engine;

/// What a token is, for its color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    /// A quoted text, or a PostgreSQL dollar-quoted body.
    String,
    Number,
    Comment,
    /// Names (quoted or not), operators, and punctuation.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub range: Range<usize>,
}

/// Where a line starts: in code, or inside something an earlier line opened.
/// The editor keeps one for each line, so a changed line is lexed alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LineState {
    #[default]
    Code,
    /// Inside text quoted by `quote`. `backslash`: a `\` escapes the next
    /// character (MySQL, and PostgreSQL's `E'...'`).
    Quoted { quote: u8, backslash: bool },
    /// Inside `/* */`, `depth` deep: PostgreSQL nests them.
    Comment { depth: u32 },
    /// Inside PostgreSQL's `$tag$ ... $tag$`.
    Dollar { tag: String },
}

/// The words colored as keywords, upper case and sorted, for a binary search.
pub(super) const KEYWORDS: &[&str] = &[
    "ALL",
    "ALTER",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CASE",
    "CAST",
    "COMMIT",
    "CREATE",
    "CROSS",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DESCRIBE",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "EXCEPT",
    "EXISTS",
    "EXPLAIN",
    "FALSE",
    "FETCH",
    "FOR",
    "FROM",
    "FULL",
    "GROUP",
    "HAVING",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NOT",
    "NULL",
    "OFFSET",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PRIMARY",
    "RETURNING",
    "RIGHT",
    "ROLLBACK",
    "SELECT",
    "SET",
    "SHOW",
    "TABLE",
    "THEN",
    "TRUE",
    "UNION",
    "UPDATE",
    "USING",
    "VALUES",
    "VIEW",
    "WHEN",
    "WHERE",
    "WITH",
];

/// The tokens of one line that starts in `state`, and the state the next
/// line starts in.
pub fn tokens(engine: Engine, line: &str, state: &LineState) -> (Vec<Token>, LineState) {
    let mut lexer = Lexer::new(engine, line, state.clone());
    let tokens = std::iter::from_fn(|| lexer.next_token()).collect();
    (tokens, lexer.state)
}

/// Each statement of `text` that holds some code, without the `;` that ends
/// it and without the space around it. A part with only comments is not a
/// statement.
pub fn statements(engine: Engine, text: &str) -> Vec<Range<usize>> {
    parts(engine, text)
        .into_iter()
        .filter_map(|part| part.statement)
        .collect()
}

/// The statement the cursor (a byte offset) is in, or at the end of: the
/// cursor right after a `;` still runs the statement before it. In a part
/// with no code, the nearest statement before it, else the one after it.
pub fn statement_at(engine: Engine, text: &str, cursor: usize) -> Option<Range<usize>> {
    let parts = parts(engine, text);
    let here = parts
        .iter()
        .position(|part| cursor <= part.until)
        .unwrap_or(parts.len().saturating_sub(1));
    let before = parts[..=here].iter().rev();
    let after = parts[here..].iter();
    before.chain(after).find_map(|part| part.statement.clone())
}

/// The text up to and with one `;` (or to the end).
struct Part {
    /// From its first token to its last before the `;`; `None` when it has
    /// only comments.
    statement: Option<Range<usize>>,
    /// Just after its `;`, or the end of the text.
    until: usize,
}

fn parts(engine: Engine, text: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut lexer = Lexer::new(engine, text, LineState::Code);
    let mut span: Option<Range<usize>> = None;
    let mut has_code = false;
    while let Some(token) = lexer.next_token() {
        if token.kind == TokenKind::Other && &text[token.range.clone()] == ";" {
            parts.push(Part {
                statement: span.take().filter(|_| has_code),
                until: token.range.end,
            });
            has_code = false;
            continue;
        }
        has_code |= token.kind != TokenKind::Comment;
        let start = span.as_ref().map_or(token.range.start, |span| span.start);
        span = Some(start..token.range.end);
    }
    // Text after the last `;`, or no `;` at all: the last part.
    if parts.last().is_none_or(|part| part.until < text.len()) {
        parts.push(Part {
            statement: span.filter(|_| has_code),
            until: text.len(),
        });
    }
    parts
}

pub(super) struct Lexer<'a> {
    engine: Engine,
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    state: LineState,
}

impl<'a> Lexer<'a> {
    pub(super) fn new(engine: Engine, text: &'a str, state: LineState) -> Self {
        Self {
            engine,
            text,
            bytes: text.as_bytes(),
            pos: 0,
            state,
        }
    }

    fn peek(&self, ahead: usize) -> Option<u8> {
        self.bytes.get(self.pos + ahead).copied()
    }

    fn next_token(&mut self) -> Option<Token> {
        if !matches!(self.state, LineState::Code) {
            return (self.pos < self.bytes.len()).then(|| self.resume(self.pos));
        }
        while self.peek(0).is_some_and(|b| b.is_ascii_whitespace()) {
            self.pos += 1;
        }
        let start = self.pos;
        let b = self.peek(0)?;
        let mysql = self.engine == Engine::Mysql;
        let postgres = self.engine == Engine::Postgres;
        let next = self.peek(1);
        let open = |quote, backslash| LineState::Quoted { quote, backslash };
        let opened = match b {
            b'\'' => Some(open(b'\'', mysql)),
            b'"' => Some(open(b'"', mysql)),
            b'`' if !postgres => Some(open(b'`', false)),
            b'/' if next == Some(b'*') => Some(LineState::Comment { depth: 1 }),
            b'$' if postgres => self.dollar_tag().map(|tag| LineState::Dollar { tag }),
            _ => None,
        };
        if let Some(state) = opened {
            self.pos += match &state {
                LineState::Comment { .. } => 2,
                LineState::Dollar { tag } => tag.len() + 2,
                _ => 1,
            };
            self.state = state;
            return Some(self.resume(start));
        }
        // MySQL needs a space or a control character after `--`.
        let dashes =
            b == b'-' && next == Some(b'-') && (!mysql || self.peek(2).is_none_or(|c| c <= b' '));
        if dashes || (b == b'#' && mysql) {
            let rest = &self.bytes[start..];
            self.pos += rest.iter().position(|&c| c == b'\n').unwrap_or(rest.len());
            return Some(self.token(TokenKind::Comment, start));
        }
        if b.is_ascii_digit() || (b == b'.' && next.is_some_and(|c| c.is_ascii_digit())) {
            return Some(self.number(start));
        }
        if is_word_start(b) {
            return Some(self.word(start));
        }
        self.pos += self.text[start..].chars().next().map_or(1, char::len_utf8);
        Some(self.token(TokenKind::Other, start))
    }

    /// The rest of what `self.state` opened: up to its end, or the end of
    /// the text. The callers call it only when the state is not `Code`.
    fn resume(&mut self, start: usize) -> Token {
        let end = self.bytes.len();
        let kind = match self.state.clone() {
            LineState::Code => TokenKind::Other,
            LineState::Quoted { quote, backslash } => {
                while let Some(b) = self.peek(0) {
                    self.pos += 1;
                    if backslash && b == b'\\' {
                        self.pos = (self.pos + 1).min(end);
                    } else if b == quote && self.peek(0) == Some(quote) {
                        self.pos += 1;
                    } else if b == quote {
                        self.state = LineState::Code;
                        break;
                    }
                }
                // MySQL's `"` quotes a text; elsewhere `"` and `` ` `` quote
                // a name.
                if quote == b'\'' || (quote == b'"' && self.engine == Engine::Mysql) {
                    TokenKind::String
                } else {
                    TokenKind::Other
                }
            }
            LineState::Comment { mut depth } => {
                while self.pos < end && depth > 0 {
                    let pair = (self.peek(0), self.peek(1));
                    if pair == (Some(b'*'), Some(b'/')) {
                        depth -= 1;
                        self.pos += 2;
                    } else if pair == (Some(b'/'), Some(b'*')) && self.engine == Engine::Postgres {
                        depth += 1;
                        self.pos += 2;
                    } else {
                        self.pos += 1;
                    }
                }
                self.state = if depth == 0 {
                    LineState::Code
                } else {
                    LineState::Comment { depth }
                };
                TokenKind::Comment
            }
            LineState::Dollar { tag } => {
                let close = format!("${tag}$");
                match self.text[self.pos..].find(&close) {
                    Some(at) => {
                        self.pos += at + close.len();
                        self.state = LineState::Code;
                    }
                    None => self.pos = end,
                }
                TokenKind::String
            }
        };
        self.token(kind, start)
    }

    /// At a `$`: the tag of a dollar quote (`$$` or `$name$`), or `None` for
    /// a `$1` parameter or a lone `$`.
    fn dollar_tag(&self) -> Option<String> {
        let rest = &self.bytes[self.pos + 1..];
        let len = match rest.first() {
            Some(&b) if is_word_start(b) => rest
                .iter()
                .position(|&c| !(is_word_start(c) || c.is_ascii_digit()))
                .unwrap_or(rest.len()),
            _ => 0,
        };
        (rest.get(len) == Some(&b'$')).then(|| self.text[self.pos + 1..self.pos + 1 + len].into())
    }

    fn number(&mut self, start: usize) -> Token {
        while let Some(b) = self.peek(0) {
            let exponent = matches!(b, b'+' | b'-')
                && matches!(self.bytes[self.pos - 1], b'e' | b'E')
                && self.bytes[start].is_ascii_digit();
            if !(b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || exponent) {
                break;
            }
            self.pos += 1;
        }
        self.token(TokenKind::Number, start)
    }

    fn word(&mut self, start: usize) -> Token {
        while self
            .peek(0)
            .is_some_and(|b| is_word_start(b) || b.is_ascii_digit() || b == b'$')
        {
            self.pos += 1;
        }
        let word = &self.text[start..self.pos];
        // PostgreSQL's `E'...'`: a text where `\` escapes.
        if self.engine == Engine::Postgres
            && word.eq_ignore_ascii_case("e")
            && self.peek(0) == Some(b'\'')
        {
            self.pos += 1;
            self.state = LineState::Quoted {
                quote: b'\'',
                backslash: true,
            };
            return self.resume(start);
        }
        let upper = word.to_ascii_uppercase();
        let kind = if KEYWORDS.binary_search(&upper.as_str()).is_ok() {
            TokenKind::Keyword
        } else {
            TokenKind::Other
        };
        self.token(kind, start)
    }

    #[cfg(test)]
    pub(super) fn tokens(mut self) -> Vec<Token> {
        std::iter::from_fn(|| self.next_token()).collect()
    }

    fn token(&self, kind: TokenKind, start: usize) -> Token {
        Token {
            kind,
            range: start..self.pos,
        }
    }
}

/// A letter, `_`, or any byte of a non-ASCII character: names may hold those.
fn is_word_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}
