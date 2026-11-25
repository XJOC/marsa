//! The SQL editor's text (components.md, SQL editor): lines, a cursor, a
//! selection, editing, and undo. Pure: no window, no layout, so every rule
//! is tested on its own. The widget draws it, one line at a time.
//!
//! A position is a line and a byte offset in it, always on a character's
//! edge. The text is kept as lines so that a key press changes one `String`
//! and the widget lays out only that line again (egui's `TextEdit` lays out
//! the whole text on each change: about 7 MiB per key press, measured).

use std::cmp::Ordering;

/// A place in the text: a line, and a byte offset on a character's edge.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl PartialOrd for Pos {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Pos {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.line, self.col).cmp(&(other.line, other.col))
    }
}

/// The text would pass the limit: nothing changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLong {
    pub limit: usize,
}

/// One change, as undo needs it: the text from `start` that was removed,
/// and the text put there, with the cursor before.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Edit {
    start: Pos,
    removed: String,
    inserted: String,
    before: (Pos, Option<Pos>),
}

/// Lines an edit changed: from `line`, `removed` lines became `inserted`
/// ones. The widget moves its per-line layout to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    pub line: usize,
    pub removed: usize,
    pub inserted: usize,
}

/// How the cursor moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
    /// The start and the end of the line.
    Home,
    End,
    /// The start and the end of the text.
    Top,
    Bottom,
    /// A word to the start or to the end.
    WordLeft,
    WordRight,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Buffer {
    /// Never empty: an empty text is one empty line.
    lines: Vec<String>,
    cursor: Pos,
    /// The other end of the selection, when there is one.
    anchor: Option<Pos>,
    /// The column, in characters, that Up and Down keep.
    goal: Option<usize>,
    /// The bytes of the text, line breaks included.
    bytes: usize,
    limit: usize,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// The changes since [`Buffer::take_changes`].
    changes: Vec<Change>,
}

impl Buffer {
    /// `text`, with the cursor at its start. `limit` is the most bytes the
    /// text may have; a longer `text` is kept whole (it was the user's).
    pub fn new(text: &str, limit: usize) -> Self {
        let lines: Vec<String> = split(text);
        let bytes = lines.iter().map(String::len).sum::<usize>() + lines.len() - 1;
        Self {
            lines,
            cursor: Pos::default(),
            anchor: None,
            goal: None,
            bytes,
            limit,
            undo: Vec::new(),
            redo: Vec::new(),
            changes: Vec::new(),
        }
    }

    /// The lines changed since the last call, in order.
    pub fn take_changes(&mut self) -> Vec<Change> {
        std::mem::take(&mut self.changes)
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn len(&self) -> usize {
        self.bytes
    }

    pub fn is_empty(&self) -> bool {
        self.bytes == 0
    }

    pub fn cursor(&self) -> Pos {
        self.cursor
    }

    /// The selection, start first, if it is not empty.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let anchor = self.anchor?;
        match anchor.cmp(&self.cursor) {
            Ordering::Less => Some((anchor, self.cursor)),
            Ordering::Greater => Some((self.cursor, anchor)),
            Ordering::Equal => None,
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection()?;
        Some(self.slice(start, end))
    }

    /// The byte offset of `pos` in [`Buffer::text`].
    pub fn offset(&self, pos: Pos) -> usize {
        self.lines[..pos.line]
            .iter()
            .map(|line| line.len() + 1)
            .sum::<usize>()
            + pos.col
    }

    /// The position of a byte offset of [`Buffer::text`], on a character's
    /// edge; past the end is the end.
    pub fn pos_at(&self, mut offset: usize) -> Pos {
        for (line, text) in self.lines.iter().enumerate() {
            if offset <= text.len() {
                return Pos {
                    line,
                    col: floor_edge(text, offset),
                };
            }
            offset -= text.len() + 1;
        }
        self.end()
    }

    fn end(&self) -> Pos {
        let line = self.lines.len() - 1;
        Pos {
            line,
            col: self.lines[line].len(),
        }
    }

    /// Puts the cursor at `pos` (made valid); with `select`, the selection
    /// grows to it.
    pub fn set_cursor(&mut self, pos: Pos, select: bool) {
        let pos = self.valid(pos);
        self.place(pos, select);
        self.goal = None;
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(Pos::default());
        self.cursor = self.end();
        self.goal = None;
    }

    /// Moves the cursor; with `select` the selection grows, without it a
    /// selection collapses (Left and Right to its start and its end).
    pub fn move_cursor(&mut self, to: Move, select: bool) {
        if !select && let Some((start, end)) = self.selection() {
            match to {
                Move::Left => return self.set_cursor(start, false),
                Move::Right => return self.set_cursor(end, false),
                _ => {}
            }
        }
        let Pos { line, col } = self.cursor;
        let text = &self.lines[line];
        let goal = self.goal;
        let pos = match to {
            Move::Left => self.left_of(self.cursor),
            Move::Right => self.right_of(self.cursor),
            Move::Up | Move::Down => {
                let chars = goal.unwrap_or_else(|| text[..col].chars().count());
                let target = match to {
                    Move::Up if line > 0 => line - 1,
                    Move::Down if line + 1 < self.lines.len() => line + 1,
                    // Past the first or the last line: its start or end.
                    Move::Up => return self.place_keeping(Pos { line, col: 0 }, select, chars),
                    _ => return self.place_keeping(self.end(), select, chars),
                };
                let col = byte_of_char(&self.lines[target], chars);
                return self.place_keeping(Pos { line: target, col }, select, chars);
            }
            Move::Home => Pos { line, col: 0 },
            Move::End => Pos {
                line,
                col: text.len(),
            },
            Move::Top => Pos::default(),
            Move::Bottom => self.end(),
            Move::WordLeft => Pos {
                line,
                col: word_start(text, col),
            },
            Move::WordRight => Pos {
                line,
                col: word_end(text, col),
            },
        };
        self.place(pos, select);
        self.goal = None;
    }

    /// Up and Down keep the column they started from.
    fn place_keeping(&mut self, pos: Pos, select: bool, goal: usize) {
        self.place(pos, select);
        self.goal = Some(goal);
    }

    fn place(&mut self, pos: Pos, select: bool) {
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = pos;
    }

    /// Types or pastes `text` over the selection. Line breaks may be `\r\n`.
    /// Refused, with nothing changed, if the text would pass the limit.
    pub fn insert(&mut self, text: &str) -> Result<(), TooLong> {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let (start, end) = self.selection().unwrap_or((self.cursor, self.cursor));
        let removed = self.offset(end) - self.offset(start);
        if self.bytes - removed + text.len() > self.limit {
            return Err(TooLong { limit: self.limit });
        }
        self.edit(start, end, &text);
        Ok(())
    }

    /// Backspace: the selection, else the character before the cursor (at
    /// a line's start, the line break).
    pub fn backspace(&mut self) {
        if let Some((start, end)) = self.selection() {
            return self.edit(start, end, "");
        }
        let end = self.cursor;
        let start = self.left_of(end);
        if start != end {
            self.edit(start, end, "");
        }
    }

    /// Delete: the selection, else the character after the cursor.
    pub fn delete(&mut self) {
        if let Some((start, end)) = self.selection() {
            return self.edit(start, end, "");
        }
        let start = self.cursor;
        let end = self.right_of(start);
        if start != end {
            self.edit(start, end, "");
        }
    }

    /// One character before `pos`: at a line's start, the end of the line
    /// before.
    fn left_of(&self, pos: Pos) -> Pos {
        if pos.col > 0 {
            Pos {
                line: pos.line,
                col: previous_edge(&self.lines[pos.line], pos.col),
            }
        } else if pos.line > 0 {
            Pos {
                line: pos.line - 1,
                col: self.lines[pos.line - 1].len(),
            }
        } else {
            pos
        }
    }

    fn right_of(&self, pos: Pos) -> Pos {
        let text = &self.lines[pos.line];
        if pos.col < text.len() {
            Pos {
                line: pos.line,
                col: next_edge(text, pos.col),
            }
        } else if pos.line + 1 < self.lines.len() {
            Pos {
                line: pos.line + 1,
                col: 0,
            }
        } else {
            pos
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(edit) = self.undo.pop() else {
            return false;
        };
        let end = after(edit.start, &edit.inserted);
        self.replace(edit.start, end, &edit.removed);
        (self.cursor, self.anchor) = edit.before;
        self.goal = None;
        self.redo.push(edit);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(edit) = self.redo.pop() else {
            return false;
        };
        let end = after(edit.start, &edit.removed);
        self.replace(edit.start, end, &edit.inserted);
        self.cursor = after(edit.start, &edit.inserted);
        self.anchor = None;
        self.goal = None;
        self.undo.push(edit);
        true
    }

    /// Replaces `start..end` with `text`, keeps the change for undo, and
    /// puts the cursor after the text. Typing one character after another
    /// joins one undo step until a space, a line break, or a move.
    fn edit(&mut self, start: Pos, end: Pos, text: &str) {
        let before = (self.cursor, self.anchor);
        let removed = self.replace(start, end, text);
        self.cursor = after(start, text);
        self.anchor = None;
        self.goal = None;
        self.redo.clear();
        if let Some(last) = self.undo.last_mut()
            && removed.is_empty()
            && last.removed.is_empty()
            && is_word(text)
            && is_word(&last.inserted)
            && after(last.start, &last.inserted) == start
        {
            last.inserted.push_str(text);
            return;
        }
        self.undo.push(Edit {
            start,
            removed,
            inserted: text.to_owned(),
            before,
        });
    }

    /// Replaces `start..end` with `text`: the removed text.
    fn replace(&mut self, start: Pos, end: Pos, text: &str) -> String {
        let removed = self.slice(start, end);
        let tail = self.lines[end.line][end.col..].to_owned();
        let head = &self.lines[start.line][..start.col];
        let mut new = split(&format!("{head}{text}{tail}"));
        if new.is_empty() {
            new.push(String::new());
        }
        self.changes.push(Change {
            line: start.line,
            removed: end.line - start.line + 1,
            inserted: new.len(),
        });
        self.lines.splice(start.line..=end.line, new);
        self.bytes = self.bytes - removed.len() + text.len();
        removed
    }

    fn slice(&self, start: Pos, end: Pos) -> String {
        if start.line == end.line {
            return self.lines[start.line][start.col..end.col].to_owned();
        }
        let mut text = self.lines[start.line][start.col..].to_owned();
        for line in &self.lines[start.line + 1..end.line] {
            text.push('\n');
            text.push_str(line);
        }
        text.push('\n');
        text.push_str(&self.lines[end.line][..end.col]);
        text
    }

    /// `pos` inside the text and on a character's edge.
    fn valid(&self, pos: Pos) -> Pos {
        let line = pos.line.min(self.lines.len() - 1);
        Pos {
            line,
            col: floor_edge(&self.lines[line], pos.col),
        }
    }
}

/// Where `text` put from `start` ends.
fn after(start: Pos, text: &str) -> Pos {
    match text.rsplit_once('\n') {
        None => Pos {
            line: start.line,
            col: start.col + text.len(),
        },
        Some((_, last)) => Pos {
            line: start.line + text.matches('\n').count(),
            col: last.len(),
        },
    }
}

fn split(text: &str) -> Vec<String> {
    text.split('\n').map(str::to_owned).collect()
}

/// Typing that joins one undo step: no space, no line break.
fn is_word(text: &str) -> bool {
    !text.is_empty() && !text.chars().any(char::is_whitespace)
}

fn floor_edge(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn previous_edge(text: &str, at: usize) -> usize {
    text[..at].char_indices().next_back().map_or(0, |(i, _)| i)
}

fn next_edge(text: &str, at: usize) -> usize {
    text[at..].chars().next().map_or(at, |c| at + c.len_utf8())
}

/// The byte where the `chars`-th character starts, or the line's end.
fn byte_of_char(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(i, _)| i)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The start of the word before `at`, past any spaces.
fn word_start(text: &str, at: usize) -> usize {
    let before: Vec<(usize, char)> = text[..at].char_indices().collect();
    let mut index = before.len();
    while index > 0 && !is_word_char(before[index - 1].1) {
        index -= 1;
    }
    while index > 0 && is_word_char(before[index - 1].1) {
        index -= 1;
    }
    before.get(index).map_or(at, |(i, _)| *i)
}

/// The end of the word after `at`, past any spaces.
fn word_end(text: &str, at: usize) -> usize {
    let mut chars = text[at..].char_indices().peekable();
    while chars.next_if(|(_, c)| !is_word_char(*c)).is_some() {}
    while chars.next_if(|(_, c)| is_word_char(*c)).is_some() {}
    chars.peek().map_or(text.len(), |(i, _)| at + i)
}

#[cfg(test)]
mod tests;
