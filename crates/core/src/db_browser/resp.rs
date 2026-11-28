//! RESP2, the replies of Valkey (ADR 0028, section 8). Marsa has no client
//! library for it: [`Decoder`] reads one reply from the bytes of a
//! connection, as they arrive.
//!
//! The server is not trusted to be small or well formed: another program may
//! sit on the port, and any local user can put a value of 512 MB in a key.
//! So every length is checked before anything is read or reserved, and a
//! long value is cut while it arrives: its first [`Limits::keep`] bytes stay,
//! the rest is skipped, and only its length is remembered. Nothing here
//! panics, whatever the bytes are.
//!
//! A command is written by [`crate::databases::valkey::command`].

use crate::codes;

/// What one reply may weigh. A reply past a limit is an error, and the
/// connection that gave it must be closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes kept of one bulk string. A longer one is cut, not an error.
    pub keep: usize,
    /// Bytes kept of the whole reply.
    pub kept: usize,
    /// Bytes read from the connection for the reply, kept or not.
    pub read: u64,
    /// Elements of all the arrays together.
    pub items: u64,
    /// Arrays inside arrays.
    pub depth: usize,
    /// Bytes of one line: a length, a status, or an error's text.
    pub line: usize,
}

/// One reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// `+OK`
    Status(String),
    /// `-ERR unknown command`: the server refused, and the connection stays
    /// good.
    Error(String),
    /// `:12`
    Integer(i64),
    /// `$-1` or `*-1`: nothing.
    Null,
    /// `$5`: a string of any bytes.
    Bulk(Bulk),
    /// `*2`
    Array(Vec<Reply>),
}

/// A bulk string: its first bytes, and its whole length.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bulk {
    /// At most [`Limits::keep`] bytes.
    pub bytes: Vec<u8>,
    /// The length the server declared.
    pub len: u64,
}

/// Why the bytes are not a reply that Marsa takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RespError {
    #[error("the answer is not RESP2: {0}")]
    Malformed(&'static str),
    #[error("the answer is too large: {0}")]
    TooLarge(&'static str),
}

impl RespError {
    pub fn code(&self) -> &'static str {
        codes::DB_UNAVAILABLE
    }
}

/// What the next byte belongs to.
#[derive(Debug)]
enum State {
    /// A line that starts a reply: its type, then a length or a text.
    Line,
    /// The bytes of a bulk string, `left` of them to come.
    Body { bulk: Bulk, left: u64 },
    /// The `\r\n` after a bulk string, `seen` bytes of it read.
    End { bulk: Bulk, seen: u8 },
    /// The reply was given, or refused: no more bytes are taken.
    Over,
}

/// An array whose elements are still coming.
#[derive(Debug)]
struct Open {
    values: Vec<Reply>,
    left: u64,
}

/// The most elements an array reserves room for before they arrive.
const RESERVE: u64 = 64;

/// Reads one reply. Give it the bytes as they arrive ([`Decoder::feed`]);
/// use a new one for the next reply.
#[derive(Debug)]
pub struct Decoder {
    limits: Limits,
    state: State,
    /// The line so far, without its end.
    line: Vec<u8>,
    /// The arrays around the value being read, outermost first.
    open: Vec<Open>,
    read: u64,
    kept: usize,
    items: u64,
}

impl Decoder {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            state: State::Line,
            line: Vec::new(),
            open: Vec::new(),
            read: 0,
            kept: 0,
            items: 0,
        }
    }

    /// Takes the next bytes of the connection. `Ok(None)`: the reply is not
    /// whole yet.
    ///
    /// # Errors
    ///
    /// [`RespError`] for bytes that are not RESP2, a reply past a limit, or
    /// bytes after the reply: one command has one reply. The decoder then
    /// takes nothing more.
    pub fn feed(&mut self, mut input: &[u8]) -> Result<Option<Reply>, RespError> {
        while !input.is_empty() {
            let state = std::mem::replace(&mut self.state, State::Over);
            let (next, value, used) = self.step(state, input)?;
            input = input.get(used..).unwrap_or_default();
            self.read = self.read.saturating_add(used as u64);
            if self.read > self.limits.read {
                return Err(RespError::TooLarge("too many bytes"));
            }
            self.state = next;
            if let Some(reply) = value.and_then(|value| self.place(value)) {
                self.state = State::Over;
                return if input.is_empty() {
                    Ok(Some(reply))
                } else {
                    Err(RespError::Malformed("bytes after the reply"))
                };
            }
        }
        Ok(None)
    }

    /// Reads from `input`, which is not empty: the next state, the value that
    /// ended, and the bytes used (at least one).
    fn step(
        &mut self,
        state: State,
        input: &[u8],
    ) -> Result<(State, Option<Reply>, usize), RespError> {
        match state {
            State::Over => Err(RespError::Malformed("bytes after the reply")),
            State::Line => {
                let end = input.iter().position(|byte| *byte == b'\n');
                let part = input.get(..end.unwrap_or(input.len())).unwrap_or(input);
                // One more than the limit: the line's `\r`.
                if self.line.len().saturating_add(part.len()) > self.limits.line.saturating_add(1) {
                    return Err(RespError::TooLarge("a line is too long"));
                }
                self.line.extend_from_slice(part);
                let Some(end) = end else {
                    return Ok((State::Line, None, input.len()));
                };
                let mut line = std::mem::take(&mut self.line);
                if line.pop() != Some(b'\r') {
                    return Err(RespError::Malformed("a line does not end with CR LF"));
                }
                if line.contains(&b'\r') {
                    return Err(RespError::Malformed("a CR inside a line"));
                }
                let (next, value) = self.header(&line)?;
                Ok((next, value, end + 1))
            }
            State::Body { mut bulk, left } => {
                let here = usize::try_from(left).map_or(input.len(), |left| left.min(input.len()));
                let room = self.limits.keep.saturating_sub(bulk.bytes.len()).min(here);
                let kept = input.get(..room).unwrap_or_default();
                self.keep(kept.len())?;
                bulk.bytes.extend_from_slice(kept);
                let left = left.saturating_sub(here as u64);
                let next = if left == 0 {
                    State::End { bulk, seen: 0 }
                } else {
                    State::Body { bulk, left }
                };
                Ok((next, None, here))
            }
            State::End { bulk, seen } => match (seen, input.first()) {
                (0, Some(b'\r')) => Ok((State::End { bulk, seen: 1 }, None, 1)),
                (1, Some(b'\n')) => Ok((State::Line, Some(Reply::Bulk(bulk)), 1)),
                _ => Err(RespError::Malformed("a string is longer than it said")),
            },
        }
    }

    /// What a whole line says: a value, or the start of a string or an array.
    fn header(&mut self, line: &[u8]) -> Result<(State, Option<Reply>), RespError> {
        let (kind, rest) = line
            .split_first()
            .ok_or(RespError::Malformed("an empty line"))?;
        let value = match kind {
            b'+' => Reply::Status(self.text(rest)?),
            b'-' => Reply::Error(self.text(rest)?),
            b':' => Reply::Integer(integer(rest)?),
            b'$' => match length(rest)? {
                None => Reply::Null,
                Some(len) => {
                    // Refused before a byte of it is read.
                    if len > self.limits.read.saturating_sub(self.read) {
                        return Err(RespError::TooLarge("a string is too long"));
                    }
                    let bulk = Bulk {
                        bytes: Vec::new(),
                        len,
                    };
                    let next = if len == 0 {
                        State::End { bulk, seen: 0 }
                    } else {
                        State::Body { bulk, left: len }
                    };
                    return Ok((next, None));
                }
            },
            b'*' => match length(rest)? {
                None => Reply::Null,
                Some(0) => Reply::Array(Vec::new()),
                Some(count) => {
                    // Refused before room is reserved for any of it.
                    self.items = self.items.saturating_add(count);
                    if self.items > self.limits.items {
                        return Err(RespError::TooLarge("too many elements"));
                    }
                    if self.open.len() >= self.limits.depth {
                        return Err(RespError::TooLarge("arrays are nested too deep"));
                    }
                    self.open.push(Open {
                        values: Vec::with_capacity(count.min(RESERVE) as usize),
                        left: count,
                    });
                    return Ok((State::Line, None));
                }
            },
            _ => return Err(RespError::Malformed("an unknown type of reply")),
        };
        Ok((State::Line, Some(value)))
    }

    /// Puts a value in the array around it, closing each array it fills. The
    /// whole reply when no array is left.
    fn place(&mut self, mut value: Reply) -> Option<Reply> {
        loop {
            let Some(open) = self.open.last_mut() else {
                return Some(value);
            };
            open.values.push(value);
            open.left = open.left.saturating_sub(1);
            if open.left > 0 {
                return None;
            }
            value = Reply::Array(self.open.pop()?.values);
        }
    }

    /// A status or an error: text, counted as kept bytes.
    fn text(&mut self, bytes: &[u8]) -> Result<String, RespError> {
        self.keep(bytes.len())?;
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    fn keep(&mut self, bytes: usize) -> Result<(), RespError> {
        self.kept = self.kept.saturating_add(bytes);
        if self.kept > self.limits.kept {
            return Err(RespError::TooLarge("too much data"));
        }
        Ok(())
    }
}

/// A number as RESP writes it: digits, with `-` before a negative one. No
/// `+`, no space, nothing else.
fn integer(text: &[u8]) -> Result<i64, RespError> {
    let digits = text.strip_prefix(b"-").unwrap_or(text);
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return Err(RespError::Malformed("not a number"));
    }
    std::str::from_utf8(text)
        .ok()
        .and_then(|text| text.parse().ok())
        .ok_or(RespError::Malformed("a number out of range"))
}

/// The length of a string or an array: `None` for `-1`, which means nothing.
fn length(text: &[u8]) -> Result<Option<u64>, RespError> {
    match integer(text)? {
        -1 => Ok(None),
        n => u64::try_from(n)
            .map(Some)
            .map_err(|_| RespError::Malformed("a negative length")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        keep: 8,
        kept: 64,
        read: 1024,
        items: 16,
        depth: 3,
        line: 32,
    };

    /// The reply of `bytes` fed at once.
    fn decode(bytes: &[u8]) -> Result<Option<Reply>, RespError> {
        Decoder::new(LIMITS).feed(bytes)
    }

    /// The reply of `bytes` fed one at a time.
    fn drip(bytes: &[u8]) -> Result<Option<Reply>, RespError> {
        let mut decoder = Decoder::new(LIMITS);
        let mut reply = None;
        for byte in bytes {
            reply = decoder.feed(std::slice::from_ref(byte))?;
        }
        Ok(reply)
    }

    fn bulk(text: &str) -> Reply {
        Reply::Bulk(Bulk {
            bytes: text.into(),
            len: text.len() as u64,
        })
    }

    /// What Valkey answers to `SCAN`: the next cursor, and the keys.
    const SCAN: &[u8] = b"*2\r\n$2\r\n17\r\n*2\r\n$3\r\na:1\r\n$3\r\nb:2\r\n";

    #[test]
    fn each_type_of_reply_is_read() {
        let scan = Reply::Array(vec![
            bulk("17"),
            Reply::Array(vec![bulk("a:1"), bulk("b:2")]),
        ]);
        for (bytes, reply) in [
            (&b"+OK\r\n"[..], Reply::Status("OK".into())),
            (b"+\r\n", Reply::Status(String::new())),
            (
                b"-ERR no such key\r\n",
                Reply::Error("ERR no such key".into()),
            ),
            (b":0\r\n", Reply::Integer(0)),
            (b":-2\r\n", Reply::Integer(-2)),
            (b":9223372036854775807\r\n", Reply::Integer(i64::MAX)),
            (b"$-1\r\n", Reply::Null),
            (b"*-1\r\n", Reply::Null),
            (b"$0\r\n\r\n", bulk("")),
            (b"$6\r\na\r\nb\0c\r\n", bulk("a\r\nb\0c")),
            (b"*0\r\n", Reply::Array(Vec::new())),
            (
                b"*3\r\n:1\r\n$-1\r\n*0\r\n",
                Reply::Array(vec![
                    Reply::Integer(1),
                    Reply::Null,
                    Reply::Array(Vec::new()),
                ]),
            ),
            (SCAN, scan),
        ] {
            let shown = String::from_utf8_lossy(bytes);
            assert_eq!(decode(bytes), Ok(Some(reply.clone())), "{shown:?}");
            assert_eq!(drip(bytes), Ok(Some(reply)), "{shown:?}");
        }
    }

    #[test]
    fn a_reply_cut_anywhere_waits_for_the_rest() {
        for bytes in [SCAN, b"+PONG\r\n", b"$5\r\nhello\r\n", b":12\r\n"] {
            for end in 0..bytes.len() {
                assert_eq!(decode(&bytes[..end]), Ok(None), "{end}");
            }
            // In two parts, cut anywhere, as one.
            for cut in 0..bytes.len() {
                let mut decoder = Decoder::new(LIMITS);
                assert_eq!(decoder.feed(&bytes[..cut]), Ok(None));
                assert_eq!(decoder.feed(&bytes[cut..]), decode(bytes), "{cut}");
            }
        }
    }

    #[test]
    fn a_long_string_is_cut_and_keeps_its_length() {
        let reply = Reply::Array(vec![
            Reply::Bulk(Bulk {
                bytes: b"01234567".to_vec(),
                len: 20,
            }),
            bulk("next"),
        ]);
        let bytes = b"*2\r\n$20\r\n01234567890123456789\r\n$4\r\nnext\r\n";
        assert_eq!(decode(bytes), Ok(Some(reply.clone())));
        assert_eq!(drip(bytes), Ok(Some(reply)));
    }

    #[test]
    fn bytes_that_are_not_resp2_are_refused() {
        for bytes in [
            // An unknown type, RESP3's types, and an inline answer.
            &b"?1\r\n"[..],
            b"_\r\n",
            b"%1\r\n",
            b"#t\r\n",
            b",1.5\r\n",
            b"OK\r\n",
            // Line ends.
            b"\r\n",
            b"\n",
            b"+OK\n",
            b"+O\rK\r\n",
            // Numbers.
            b":\r\n",
            b":-\r\n",
            b":+1\r\n",
            b": 1\r\n",
            b":1.0\r\n",
            b":9223372036854775808\r\n",
            b"$x\r\n",
            b"$\r\n",
            b"$-2\r\n",
            b"*-2\r\n",
            b"*1x\r\n",
            b"$99999999999999999999\r\n",
            // A string that is not as long as it said.
            b"$2\r\nabc\r\n",
            b"$3\r\nabc\n",
            b"$3\r\nabc\r\r",
            // One command has one reply.
            b"+OK\r\n+OK\r\n",
            b"$1\r\na\r\nx",
            // A bad element inside an array.
            b"*2\r\n:1\r\n!\r\n",
        ] {
            let shown = String::from_utf8_lossy(bytes);
            for got in [decode(bytes), drip(bytes)] {
                assert!(
                    matches!(got, Err(RespError::Malformed(_))),
                    "{shown:?}: {got:?}"
                );
            }
        }
    }

    #[test]
    fn a_decoder_takes_nothing_after_its_reply_or_its_error() {
        for first in [&b"+OK\r\n"[..], b"?\r\n"] {
            let mut decoder = Decoder::new(LIMITS);
            let _ = decoder.feed(first);
            assert_eq!(decoder.feed(b""), Ok(None));
            assert!(decoder.feed(b"+OK\r\n").is_err());
        }
    }

    #[test]
    fn a_reply_past_a_limit_is_refused_as_soon_as_it_says_so() {
        let deep = b"*1\r\n*1\r\n*1\r\n*1\r\n";
        let wide = b"*17\r\n";
        // Eight elements, then an array of nine inside: 17 in all.
        let together = b"*8\r\n*9\r\n";
        let long_line = [b'+'; 40];
        let kept = [&b"*9\r\n"[..], &b"$8\r\n01234567\r\n".repeat(9)].concat();
        for (bytes, why) in [
            // Declared lengths: nothing of them has to arrive.
            (&b"$1025\r\n"[..], "a string is too long"),
            (b"$999999999999\r\n", "a string is too long"),
            (wide, "too many elements"),
            (b"*9223372036854775807\r\n", "too many elements"),
            (together, "too many elements"),
            (deep, "arrays are nested too deep"),
            (&long_line, "a line is too long"),
            (&kept, "too much data"),
        ] {
            let got = decode(bytes);
            assert_eq!(got, Err(RespError::TooLarge(why)));
            assert_eq!(drip(bytes), got, "{why}");
        }
    }

    #[test]
    fn the_limits_themselves_are_allowed() {
        let deep = b"*1\r\n*1\r\n*1\r\n:1\r\n";
        assert!(matches!(decode(deep), Ok(Some(_))));
        let wide = [&b"*16\r\n"[..], &b":1\r\n".repeat(16)].concat();
        assert!(matches!(decode(&wide), Ok(Some(_))));
        let line = format!("+{}\r\n", "a".repeat(31));
        assert!(matches!(decode(line.as_bytes()), Ok(Some(_))));
        assert!(decode(format!("+{}\r\n", "a".repeat(32)).as_bytes()).is_err());
    }

    #[test]
    fn bytes_read_count_even_when_they_are_not_kept() {
        let limits = Limits { read: 40, ..LIMITS };
        // Each string fits what is left when it is declared; their lines and
        // ends pass the limit.
        let bytes = [&b"*4\r\n"[..], &b"$9\r\n123456789\r\n".repeat(4)].concat();
        let got = Decoder::new(limits).feed(&bytes);
        assert!(matches!(got, Err(RespError::TooLarge(_))), "{got:?}");
    }

    #[test]
    fn status_and_error_texts_count_as_kept() {
        let line = format!("-{}\r\n", "e".repeat(30));
        let bytes = format!("*3\r\n{line}{line}{line}");
        assert_eq!(
            decode(bytes.as_bytes()),
            Err(RespError::TooLarge("too much data"))
        );
    }

    /// Many streams made of RESP's own characters, so most get past the
    /// first line: none may panic, and one byte at a time gives the same
    /// refusal or reply as all at once.
    #[test]
    fn no_bytes_make_it_panic() {
        const PARTS: [&[u8]; 14] = [
            b"*",
            b"$",
            b":",
            b"+",
            b"-",
            b"\r\n",
            b"\r",
            b"\n",
            b"0",
            b"1",
            b"2",
            b"9999999999",
            b"ab",
            b"\xff",
        ];
        // A fixed linear congruential sequence: the same streams every run.
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (seed >> 33) as usize
        };
        for _ in 0..20_000 {
            let length = next() % 24;
            let bytes: Vec<u8> = (0..length)
                .flat_map(|_| PARTS[next() % PARTS.len()].to_vec())
                .collect();
            let whole = decode(&bytes);
            let mut decoder = Decoder::new(LIMITS);
            let mut dripped = Ok(None);
            for (at, byte) in bytes.iter().enumerate() {
                dripped = decoder.feed(std::slice::from_ref(byte));
                // A reply before the last byte: fed whole, the rest is "bytes
                // after the reply".
                if matches!(dripped, Ok(Some(_))) && at + 1 < bytes.len() {
                    dripped = Err(RespError::Malformed("bytes after the reply"));
                }
                if dripped.is_err() {
                    break;
                }
            }
            assert_eq!(whole, dripped, "{:?}", String::from_utf8_lossy(&bytes));
        }
    }

    #[test]
    fn an_error_has_the_code_of_an_unreadable_database() {
        assert_eq!(RespError::Malformed("x").code(), codes::DB_UNAVAILABLE);
        assert_eq!(RespError::TooLarge("x").code(), codes::DB_UNAVAILABLE);
    }
}
