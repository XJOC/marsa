//! Server-sent events: the format of `GET /v1/events` and
//! `GET /v1/logs/{id}?follow=true`. Task M3-08.
//!
//! [`SseParser`] is pure: it takes bytes in chunks cut anywhere, even inside
//! a line or a UTF-8 character, and gives back the `data` of each complete
//! event. [`EventStream`] feeds it from the HTTP response.
//!
//! The daemon sends each event as `data: <json>` and a blank line, and
//! keep-alive or `lagged` notes as comments (`:`). Following the SSE standard,
//! lines end with `\r\n`, `\n`, or `\r`, several `data` lines join with `\n`,
//! and other fields (`event`, `id`, `retry`) are read and ignored.

use std::collections::VecDeque;
use std::fmt;
use std::marker::PhantomData;

use serde::de::DeserializeOwned;

use super::ClientError;

/// The largest event, counting its unfinished line. Events are small; this
/// only stops a broken stream from filling memory.
const MAX_EVENT: usize = 8 * 1024 * 1024;

/// The event or line was larger than [`MAX_EVENT`]. The parser stops there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TooLarge;

#[derive(Debug, Default)]
pub(crate) struct SseParser {
    /// The line read so far, as bytes: a chunk can end inside a character.
    line: Vec<u8>,
    /// The `data` lines of the current event, each followed by `\n`.
    data: String,
    /// The last chunk ended with `\r`; a `\n` at the start of the next one
    /// belongs to the same line ending.
    after_cr: bool,
    events: VecDeque<Result<String, TooLarge>>,
    failed: bool,
}

impl SseParser {
    pub(crate) fn feed(&mut self, mut bytes: &[u8]) {
        if self.failed {
            return;
        }
        if self.after_cr {
            match bytes {
                [] => return,
                [b'\n', rest @ ..] => bytes = rest,
                _ => {}
            }
            self.after_cr = false;
        }
        while let Some(end) = bytes.iter().position(|&b| b == b'\n' || b == b'\r') {
            let (line, rest) = bytes.split_at(end);
            self.line.extend_from_slice(line);
            if !self.within_limit() {
                return;
            }
            bytes = match rest {
                [b'\r', b'\n', after @ ..] => after,
                [b'\r'] => {
                    self.after_cr = true;
                    &[]
                }
                [_, after @ ..] => after,
                [] => &[],
            };
            self.end_line();
        }
        self.line.extend_from_slice(bytes);
        self.within_limit();
    }

    /// The next complete event's `data`, in order. After a `TooLarge` there
    /// are no more events.
    pub(crate) fn next_event(&mut self) -> Option<Result<String, TooLarge>> {
        self.events.pop_front()
    }

    fn within_limit(&mut self) -> bool {
        if self.line.len() + self.data.len() <= MAX_EVENT {
            return true;
        }
        self.failed = true;
        self.line = Vec::new();
        self.data = String::new();
        self.events.push_back(Err(TooLarge));
        false
    }

    fn end_line(&mut self) {
        let line = self.line.as_slice();
        if line.is_empty() {
            // A blank line ends the event. No `data` line means no event.
            if !self.data.is_empty() {
                self.data.pop();
                self.events.push_back(Ok(std::mem::take(&mut self.data)));
            }
        } else if !line.starts_with(b":") {
            let (field, value) = match line.iter().position(|&b| b == b':') {
                Some(colon) => (&line[..colon], &line[colon + 1..]),
                None => (line, &[][..]),
            };
            if field == b"data" {
                let value = value.strip_prefix(b" ").unwrap_or(value);
                // The standard decodes with replacement characters. Our
                // daemon only sends JSON, which is valid UTF-8.
                self.data.push_str(&String::from_utf8_lossy(value));
                self.data.push('\n');
            }
        }
        self.line.clear();
    }
}

/// Events from an SSE response, each `data` read as JSON into `T`.
pub struct EventStream<T> {
    response: reqwest::Response,
    parser: SseParser,
    port: u16,
    finished: bool,
    _event: PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned> EventStream<T> {
    pub(super) fn new(response: reqwest::Response, port: u16) -> Self {
        Self {
            response,
            parser: SseParser::default(),
            port,
            finished: false,
            _event: PhantomData,
        }
    }

    /// The next event, or `None` once the daemon has closed the stream (for
    /// example after `shutting_down`). An event that is not the expected JSON
    /// gives an error and the stream goes on; after a connection error or an
    /// oversized event it ends.
    pub async fn next(&mut self) -> Option<Result<T, ClientError>> {
        loop {
            match self.parser.next_event() {
                Some(Ok(data)) => {
                    return Some(
                        serde_json::from_str(&data)
                            .map_err(|err| ClientError::bad_json("an event", &err)),
                    );
                }
                Some(Err(TooLarge)) => {
                    self.finished = true;
                    return Some(Err(ClientError::bad_response(
                        "an event is larger than 8 MiB",
                    )));
                }
                None if self.finished => return None,
                None => {}
            }
            match self.response.chunk().await {
                Ok(Some(chunk)) => self.parser.feed(&chunk),
                // An unfinished event at the end is dropped, as the standard says.
                Ok(None) => self.finished = true,
                Err(source) => {
                    self.finished = true;
                    return Some(Err(ClientError::Unreachable {
                        port: self.port,
                        source,
                    }));
                }
            }
        }
    }
}

/// The port only, like [`super::DaemonClient`].
impl<T> fmt::Debug for EventStream<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventStream")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds the chunks in order and returns every event.
    fn parse(chunks: &[&[u8]]) -> Vec<Result<String, TooLarge>> {
        let mut parser = SseParser::default();
        let mut out = Vec::new();
        for chunk in chunks {
            parser.feed(chunk);
            while let Some(event) = parser.next_event() {
                out.push(event);
            }
        }
        out
    }

    fn ok(events: &[&str]) -> Vec<Result<String, TooLarge>> {
        events.iter().map(|e| Ok((*e).to_owned())).collect()
    }

    /// Every feature at once: `\r\n`, `\n`, a comment, a keep-alive, two
    /// `data` lines, other fields, and Arabic text (two bytes per letter).
    const STREAM: &str = "data: {\"text\":\"مرسى\"}\r\n\r\n\
                          : keep-alive\n\n\
                          :\r\n\r\n\
                          event: log\nid: 7\nretry: 10\n\
                          data: {\"a\":\ndata:  1}\n\n\
                          : lagged 3\n\
                          data:last\r\n\r\n";

    fn stream_events() -> Vec<Result<String, TooLarge>> {
        ok(&["{\"text\":\"مرسى\"}", "{\"a\":\n 1}", "last"])
    }

    #[test]
    fn whole_stream_in_one_chunk() {
        assert_eq!(parse(&[STREAM.as_bytes()]), stream_events());
    }

    #[test]
    fn any_cut_into_two_chunks_gives_the_same_events() {
        let bytes = STREAM.as_bytes();
        for cut in 0..=bytes.len() {
            let (a, b) = bytes.split_at(cut);
            assert_eq!(parse(&[a, b]), stream_events(), "cut at byte {cut}");
        }
    }

    #[test]
    fn one_byte_at_a_time_gives_the_same_events() {
        let chunks: Vec<&[u8]> = STREAM.as_bytes().chunks(1).collect();
        assert_eq!(parse(&chunks), stream_events());
    }

    #[test]
    fn a_cut_inside_a_letter_keeps_the_letter() {
        let bytes = "data: م\n\n".as_bytes();
        // "م" is two bytes, at 6 and 7: cut between them.
        assert_eq!(parse(&[&bytes[..7], &bytes[7..]]), ok(&["م"]));
    }

    #[test]
    fn crlf_split_across_chunks_is_one_line_ending() {
        // If the `\n` were a second line ending, the blank line would end the
        // event early and give two events.
        assert_eq!(
            parse(&[b"data: a\r", b"\ndata: b\r", b"\n\r", b"\n"]),
            ok(&["a\nb"])
        );
    }

    #[test]
    fn a_lone_cr_ends_a_line() {
        assert_eq!(parse(&[b"data: a\r\rdata: b\r", b"\r"]), ok(&["a", "b"]));
    }

    #[test]
    fn comments_and_blank_lines_alone_give_no_event() {
        assert_eq!(
            parse(&[b": keep-alive\n\n:\n\n\n\r\n: lagged 2\n\n"]),
            ok(&[])
        );
    }

    #[test]
    fn data_without_value_is_an_empty_line_of_data() {
        assert_eq!(parse(&[b"data\n\ndata:\ndata: x\n\n"]), ok(&["", "\nx"]));
    }

    #[test]
    fn an_unfinished_event_is_not_given() {
        assert_eq!(parse(&[b"data: whole\n\ndata: half\n"]), ok(&["whole"]));
    }

    #[test]
    fn invalid_utf8_becomes_replacement_characters() {
        assert_eq!(parse(&[b"data: a\xffb\n\n"]), ok(&["a\u{fffd}b"]));
    }

    #[test]
    fn an_endless_line_stops_the_parser() {
        let big = vec![b'x'; MAX_EVENT];
        let events = parse(&[b"data: ok\n\ndata: ", &big, b"\n\ndata: later\n\n"]);
        assert_eq!(events, vec![Ok("ok".to_owned()), Err(TooLarge)]);
    }
}
