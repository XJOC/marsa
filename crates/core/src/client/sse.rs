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
use std::time::Duration;

use serde::de::DeserializeOwned;
use tokio::time::{Instant, timeout_at};

use super::ClientError;

/// The largest event, counting its unfinished line. Events are small; this
/// only stops a broken stream from filling memory.
const MAX_EVENT: usize = 8 * 1024 * 1024;
const MIB: usize = 1024 * 1024;
/// A stream with no byte for this long is dead: three times the daemon's
/// 15 s keep-alive (task M3-06).
const STREAM_IDLE: Duration = Duration::from_secs(45);

/// What the parser found, in stream order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Parsed {
    /// The `data` of one complete event.
    Data(String),
    /// A `: lagged <n>` comment.
    Lagged(u64),
    /// The event or line was larger than [`MAX_EVENT`]. The parser stops.
    TooLarge,
}

#[derive(Debug, Default)]
pub(crate) struct SseParser {
    /// The line read so far, as bytes: a chunk can end inside a character.
    line: Vec<u8>,
    /// The `data` lines of the current event, each followed by `\n`.
    data: String,
    /// The last chunk ended with `\r`; a `\n` at the start of the next one
    /// belongs to the same line ending.
    after_cr: bool,
    found: VecDeque<Parsed>,
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
            bytes = match rest {
                [b'\r', b'\n', after @ ..] => after,
                [b'\r'] => {
                    self.after_cr = true;
                    &[]
                }
                [_, after @ ..] => after,
                [] => &[],
            };
            if !self.within_limit() {
                return;
            }
            self.end_line();
            // Checked again after the line became text: each invalid byte
            // becomes a 3-byte replacement character.
            if !self.within_limit() {
                return;
            }
        }
        self.line.extend_from_slice(bytes);
        self.within_limit();
    }

    /// The next thing found, in order. After `TooLarge` there is nothing more.
    pub(crate) fn next_found(&mut self) -> Option<Parsed> {
        self.found.pop_front()
    }

    fn within_limit(&mut self) -> bool {
        if self.line.len() + self.data.len() <= MAX_EVENT {
            return true;
        }
        self.failed = true;
        self.line = Vec::new();
        self.data = String::new();
        self.found.push_back(Parsed::TooLarge);
        false
    }

    fn end_line(&mut self) {
        let line = self.line.as_slice();
        if line.is_empty() {
            // A blank line ends the event. No `data` line means no event.
            if !self.data.is_empty() {
                self.data.pop();
                self.found
                    .push_back(Parsed::Data(std::mem::take(&mut self.data)));
            }
        } else if let Some(comment) = line.strip_prefix(b":") {
            if let Some(count) = lagged(comment) {
                self.found.push_back(Parsed::Lagged(count));
            }
        } else {
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

/// The count in a `lagged <n>` comment (task M3-06). A missing or unreadable
/// count gives 0, so a lag is never lost. Any other comment gives `None`.
fn lagged(comment: &[u8]) -> Option<u64> {
    let comment = comment.strip_prefix(b" ").unwrap_or(comment);
    let rest = comment.strip_prefix(b"lagged")?;
    if rest.is_empty() {
        return Some(0);
    }
    let count = rest.strip_prefix(b" ")?;
    Some(
        std::str::from_utf8(count)
            .ok()
            .and_then(|count| count.trim().parse().ok())
            .unwrap_or(0),
    )
}

/// One item of an [`EventStream`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamItem<T> {
    /// The next event.
    Item(T),
    /// The daemon dropped this many events because this client read too
    /// slowly. The stream goes on, but anything built from earlier events
    /// may be stale: fetch it again, for example with
    /// [`super::DaemonClient::services`]. 0 if the daemon gave no count.
    Lagged(u64),
}

/// Events from an SSE response, each `data` read as JSON into `T`.
pub struct EventStream<T> {
    response: reqwest::Response,
    parser: SseParser,
    port: u16,
    finished: bool,
    /// The stream is dead if no byte has come by then. Kept here, not in
    /// `next`, so a cancelled `next` does not restart it.
    idle_deadline: Instant,
    _event: PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned> EventStream<T> {
    pub(super) fn new(response: reqwest::Response, port: u16) -> Self {
        Self {
            response,
            parser: SseParser::default(),
            port,
            finished: false,
            idle_deadline: Instant::now() + STREAM_IDLE,
            _event: PhantomData,
        }
    }

    /// The next item, or `None` once the daemon has closed the stream (for
    /// example after `shutting_down`).
    ///
    /// - An event that is not the expected JSON gives `daemon_bad_response`
    ///   and the stream goes on.
    /// - A connection error, an oversized event, or 45 seconds without a
    ///   byte (not even a keep-alive) gives an error, then `None`.
    ///
    /// Cancel-safe: if the future is dropped before it finishes, for example
    /// in `tokio::select!`, nothing is lost. Bytes leave the response only
    /// when a read completes, and parsed items wait for the next call.
    pub async fn next(&mut self) -> Option<Result<StreamItem<T>, ClientError>> {
        loop {
            match self.parser.next_found() {
                Some(Parsed::Data(data)) => {
                    return Some(
                        serde_json::from_str(&data)
                            .map(StreamItem::Item)
                            .map_err(|err| ClientError::bad_json("an event", &err)),
                    );
                }
                Some(Parsed::Lagged(count)) => return Some(Ok(StreamItem::Lagged(count))),
                Some(Parsed::TooLarge) => {
                    self.finished = true;
                    return Some(Err(ClientError::bad_response(format!(
                        "an event is larger than {} MiB",
                        MAX_EVENT / MIB
                    ))));
                }
                None if self.finished => return None,
                None => {}
            }
            match timeout_at(self.idle_deadline, self.response.chunk()).await {
                Ok(Ok(Some(chunk))) => {
                    self.idle_deadline = Instant::now() + STREAM_IDLE;
                    self.parser.feed(&chunk);
                }
                // An unfinished event at the end is dropped, as the standard says.
                Ok(Ok(None)) => self.finished = true,
                Ok(Err(source)) => {
                    self.finished = true;
                    return Some(Err(ClientError::Unreachable {
                        port: self.port,
                        source: Box::new(source),
                    }));
                }
                Err(_) => {
                    self.finished = true;
                    return Some(Err(ClientError::timed_out(
                        self.port,
                        "no event or keep-alive",
                        STREAM_IDLE,
                    )));
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

    /// Feeds the chunks in order and returns everything found.
    fn parse(chunks: &[&[u8]]) -> Vec<Parsed> {
        let mut parser = SseParser::default();
        let mut out = Vec::new();
        for chunk in chunks {
            parser.feed(chunk);
            while let Some(found) = parser.next_found() {
                out.push(found);
            }
        }
        out
    }

    fn data(events: &[&str]) -> Vec<Parsed> {
        events
            .iter()
            .map(|e| Parsed::Data((*e).to_owned()))
            .collect()
    }

    /// Every feature at once: `\r\n`, `\n`, a comment, a keep-alive, a lag
    /// note, two `data` lines, other fields, and Arabic text (two bytes per
    /// letter).
    const STREAM: &str = "data: {\"text\":\"مرسى\"}\r\n\r\n\
                          : keep-alive\n\n\
                          :\r\n\r\n\
                          event: log\nid: 7\nretry: 10\n\
                          data: {\"a\":\ndata:  1}\n\n\
                          : lagged 3\n\
                          data:last\r\n\r\n";

    fn stream_found() -> Vec<Parsed> {
        vec![
            Parsed::Data("{\"text\":\"مرسى\"}".to_owned()),
            Parsed::Data("{\"a\":\n 1}".to_owned()),
            Parsed::Lagged(3),
            Parsed::Data("last".to_owned()),
        ]
    }

    #[test]
    fn whole_stream_in_one_chunk() {
        assert_eq!(parse(&[STREAM.as_bytes()]), stream_found());
    }

    #[test]
    fn any_cut_into_two_chunks_gives_the_same_events() {
        let bytes = STREAM.as_bytes();
        for cut in 0..=bytes.len() {
            let (a, b) = bytes.split_at(cut);
            assert_eq!(parse(&[a, b]), stream_found(), "cut at byte {cut}");
        }
    }

    #[test]
    fn one_byte_at_a_time_gives_the_same_events() {
        let chunks: Vec<&[u8]> = STREAM.as_bytes().chunks(1).collect();
        assert_eq!(parse(&chunks), stream_found());
    }

    #[test]
    fn a_cut_inside_a_letter_keeps_the_letter() {
        let bytes = "data: م\n\n".as_bytes();
        // "م" is two bytes, at 6 and 7: cut between them.
        assert_eq!(parse(&[&bytes[..7], &bytes[7..]]), data(&["م"]));
    }

    #[test]
    fn crlf_split_across_chunks_is_one_line_ending() {
        // If the `\n` were a second line ending, the blank line would end the
        // event early and give two events.
        assert_eq!(
            parse(&[b"data: a\r", b"\ndata: b\r", b"\n\r", b"\n"]),
            data(&["a\nb"])
        );
    }

    #[test]
    fn a_lone_cr_ends_a_line() {
        assert_eq!(parse(&[b"data: a\r\rdata: b\r", b"\r"]), data(&["a", "b"]));
    }

    #[test]
    fn comments_and_blank_lines_alone_give_no_event() {
        assert_eq!(
            parse(&[b": keep-alive\n\n:\n\n\n\r\n: hello\n\n"]),
            data(&[])
        );
    }

    #[test]
    fn lag_notes_are_found_with_their_count() {
        let found = parse(&[
            b": lagged 5\n:lagged 7\n: lagged\n: lagged x\n: lagged  9 \n",
            b": laggedy 3\n: lag 3\n:  lagged 4\n",
        ]);
        let counts = [5, 7, 0, 0, 9].map(Parsed::Lagged);
        assert_eq!(found, counts);
    }

    #[test]
    fn data_without_value_is_an_empty_line_of_data() {
        assert_eq!(parse(&[b"data\n\ndata:\ndata: x\n\n"]), data(&["", "\nx"]));
    }

    #[test]
    fn an_unfinished_event_is_not_given() {
        assert_eq!(parse(&[b"data: whole\n\ndata: half\n"]), data(&["whole"]));
    }

    #[test]
    fn invalid_utf8_becomes_replacement_characters() {
        assert_eq!(parse(&[b"data: a\xffb\n\n"]), data(&["a\u{fffd}b"]));
    }

    #[test]
    fn an_endless_line_stops_the_parser() {
        let big = vec![b'x'; MAX_EVENT];
        let found = parse(&[b"data: ok\n\ndata: ", &big, b"\n\ndata: later\n\n"]);
        assert_eq!(found, vec![Parsed::Data("ok".to_owned()), Parsed::TooLarge]);
    }

    #[test]
    fn the_limit_counts_replacement_characters() {
        // Under the limit as bytes, three times larger as text.
        let invalid = vec![0xff_u8; MAX_EVENT / 2];
        let found = parse(&[b"data: ", &invalid, b"\n\ndata: later\n\n"]);
        assert_eq!(found, vec![Parsed::TooLarge]);
    }
}
