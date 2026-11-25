//! The SQL editor (components.md, SQL editor): a plain text editor in a
//! card, with a line-number gutter and SQL colors.
//!
//! **It lays out only the lines that changed and draws only the lines in
//! view** (ADR 0028's budget: typing causes no memory growth). The text is
//! a [`Buffer`] of lines. The editor keeps, for each line, where it starts
//! (in code, or inside a comment or a string an earlier line opened) and
//! its laid-out galley. An edit tells which lines changed: only those lose
//! their galley, and the lines after them are lexed again until their start
//! is what it was. A line gets a galley when it first comes into view.
//!
//! Keys are the system's text keys. Tab moves the focus out (never a tab
//! character); Cmd or Ctrl Enter asks to run; Esc asks to cancel. Text from
//! the input method is drawn at the cursor while it is composed and enters
//! the buffer only when committed, so undo never sees half a word.

pub mod buffer;

use std::sync::Arc;

use eframe::egui::output::IMEOutput;
use eframe::egui::text::{CCursor, LayoutJob, TextFormat};
use eframe::egui::{
    Align2, Color32, CornerRadius, Event, EventFilter, FontId, Galley, IMEPurpose, ImeEvent, Key,
    Modifiers, Pos2, Rect, Role, ScrollArea, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2,
    WidgetInfo, pos2, vec2,
};
use marsa_core::db_browser::{Engine, LineState, TokenKind, tokens};

use self::buffer::{Buffer, Change, Move, Pos};
use super::paint_focus_ring;
use crate::theme::{Section, Theme};

/// What the user asked of the editor in this frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EditorOutput {
    /// Cmd or Ctrl Enter.
    pub run: bool,
    /// Esc.
    pub cancel: bool,
    /// A paste would have passed the limit and was refused.
    pub refused: bool,
    /// The text changed.
    pub changed: bool,
}

pub struct Editor<'a> {
    id_salt: &'static str,
    label: &'a str,
    buffer: &'a mut Buffer,
    engine: Engine,
    /// How many lines high.
    lines: usize,
}

impl<'a> Editor<'a> {
    /// An editor of `buffer`, `lines` lines high; `label` is what a screen
    /// reader says.
    pub fn new(
        id_salt: &'static str,
        label: &'a str,
        buffer: &'a mut Buffer,
        engine: Engine,
        lines: usize,
    ) -> Self {
        Self {
            id_salt,
            label,
            buffer,
            engine,
            lines,
        }
    }

    pub fn show(self, ui: &mut Ui) -> EditorOutput {
        let theme = Theme::of(ui.ctx()).clone();
        let id = ui.make_persistent_id(self.id_salt);
        let mut cache = ui
            .data_mut(|data| data.remove_temp::<Cache>(id))
            .unwrap_or_default();
        let dark = ui.visuals().dark_mode;
        cache.sync(self.buffer, self.engine, dark);

        let line_height = theme.size.editor_line;
        let pad = theme.space.s2;
        let height = line_height * self.lines as f32 + pad + pad;
        let size = vec2(ui.available_width(), height);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        response.widget_info(|| WidgetInfo::labeled(Role::MultilineTextInput, true, self.label));
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        let focused = response.has_focus();
        let mut output = EditorOutput::default();
        if focused {
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    response.id,
                    EventFilter {
                        tab: false,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                );
            });
            keys(ui, self.buffer, &mut cache, &mut output);
            cache.sync(self.buffer, self.engine, dark);
        }

        // The card: `surface`; its border `accent`, with the ring, when focused.
        let painter = ui.painter_at(rect.expand(theme.focus.width + theme.focus.offset));
        let corner = CornerRadius::same(theme.radius.md);
        painter.rect_filled(rect, corner, theme.color.surface);
        let border = if focused {
            theme.color.accent
        } else {
            theme.color.line
        };
        let stroke = Stroke::new(theme.size.border, border);
        painter.rect_stroke(rect, corner, stroke, StrokeKind::Inside);
        if focused {
            paint_focus_ring(&painter, &theme, rect, theme.radius.md);
        }

        let inner = rect.shrink(theme.size.border);
        // The lines sit inside the card's padding, above and below.
        let lines = inner.shrink2(vec2(0.0, pad));
        let mut child = ui.new_child(UiBuilder::new().max_rect(lines));
        child.set_clip_rect(lines);
        let moved = cache.cursor != Some(self.buffer.cursor());
        cache.cursor = Some(self.buffer.cursor());
        let mut drawn: Vec<(usize, Rect)> = Vec::new();
        let mut caret = None;
        {
            let buffer = &*self.buffer;
            let engine = self.engine;
            ScrollArea::both()
                .id_salt((self.id_salt, "scroll"))
                .auto_shrink(false)
                .show_rows(
                    &mut child,
                    line_height,
                    buffer.lines().len(),
                    |ui, range| {
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        for line in range {
                            let galley = cache.galley(ui, &theme, buffer, engine, line);
                            let text_width = galley.size().x + theme.size.editor_gutter + pad + pad;
                            let width = text_width.max(ui.available_width());
                            let (row, _) =
                                ui.allocate_exact_size(vec2(width, line_height), Sense::hover());
                            if moved && buffer.cursor().line == line {
                                ui.scroll_to_rect(row, None);
                            }
                            let place = Row {
                                rect: row,
                                line,
                                text_x: row.left() + theme.size.editor_gutter + pad,
                            };
                            let shown = paint_row(
                                ui,
                                &theme,
                                buffer,
                                &galley,
                                &place,
                                focused,
                                &cache.preedit,
                            );
                            caret = caret.or(shown);
                            drawn.push((line, row));
                        }
                    },
                );
        }

        // The pointer places the cursor; a drag, or Shift, selects.
        if let Some(at) = response.interact_pointer_pos()
            && (response.is_pointer_button_down_on() || response.clicked())
        {
            let select =
                (response.dragged() && !response.drag_started()) || ui.input(|i| i.modifiers.shift);
            let pad_x = theme.size.editor_gutter + pad;
            if let Some(pos) = cache.pos_at(ui, &theme, self.buffer, self.engine, &drawn, pad_x, at)
            {
                self.buffer.set_cursor(pos, select);
            }
        }

        if let Some(caret) = caret {
            let to_global = ui
                .ctx()
                .layer_transform_to_global(ui.layer_id())
                .unwrap_or_default();
            ui.output_mut(|platform| {
                platform.ime = Some(IMEOutput {
                    purpose: IMEPurpose::Normal,
                    rect: to_global * inner,
                    cursor_rect: to_global * caret,
                    should_interrupt_composition: false,
                });
            });
        }
        ui.data_mut(|data| data.insert_temp(id, cache));
        output
    }
}

/// How many lines the editor of `id_salt` has laid out so far: the tests
/// count them.
#[cfg(test)]
fn laid_out(ui: &Ui, id_salt: &'static str) -> u64 {
    let id = ui.make_persistent_id(id_salt);
    ui.data(|data| data.get_temp::<Cache>(id))
        .map_or(0, |cache| cache.laid_out)
}

/// Where a line is drawn.
struct Row {
    rect: Rect,
    line: usize,
    /// Where the text starts, after the gutter.
    text_x: f32,
}

/// The editor's memory between frames: each line's start and galley.
#[derive(Clone, Default)]
struct Cache {
    /// Where each line starts. As long as the buffer's lines.
    states: Vec<LineState>,
    /// Each line's galley, once it came into view.
    galleys: Vec<Option<Arc<Galley>>>,
    /// What the galleys were laid out for.
    dark: bool,
    engine: Option<Engine>,
    /// The cursor at the last frame: the view follows it when it moves.
    cursor: Option<Pos>,
    /// Text the input method is composing, drawn at the cursor.
    preedit: String,
    /// How many lines were laid out, ever.
    laid_out: u64,
}

impl Cache {
    /// Follows the buffer's changes. Everything is lexed and laid out again
    /// only for a new theme, a new engine, or a new buffer.
    fn sync(&mut self, buffer: &mut Buffer, engine: Engine, dark: bool) {
        let changes = buffer.take_changes();
        let lines = buffer.lines();
        let fresh = self.engine != Some(engine) || self.dark != dark || self.states.is_empty();
        if !fresh {
            for Change {
                line,
                removed,
                inserted,
            } in changes
            {
                let end = line + removed;
                if end > self.states.len() || line >= lines.len() {
                    break;
                }
                let blank = std::iter::repeat_n(LineState::Code, inserted);
                self.states.splice(line..end, blank);
                self.galleys
                    .splice(line..end, std::iter::repeat_n(None, inserted));
                // The first changed line starts where the line before ends.
                let start = match line.checked_sub(1) {
                    Some(before) => tokens(engine, &lines[before], &self.states[before]).1,
                    None => LineState::Code,
                };
                self.states[line] = start;
                self.relex(lines, engine, line, line + inserted);
            }
        }
        // A new buffer, or a cache that lost track: all again.
        if fresh || self.states.len() != lines.len() {
            self.engine = Some(engine);
            self.dark = dark;
            self.states = vec![LineState::Code; lines.len()];
            self.galleys = vec![None; lines.len()];
            self.relex(lines, engine, 0, lines.len());
        }
    }

    /// Lexes from line `from` on: each line's end is the next one's start.
    /// Lines before `settle` are always done; after them, it stops as soon
    /// as a start is what it was. A line whose start changes loses its
    /// galley.
    fn relex(&mut self, lines: &[String], engine: Engine, from: usize, settle: usize) {
        let mut line = from;
        while line + 1 < lines.len() {
            let (_, next) = tokens(engine, &lines[line], &self.states[line]);
            if line + 1 >= settle && self.states[line + 1] == next {
                break;
            }
            if self.states[line + 1] != next {
                self.states[line + 1] = next;
                self.galleys[line + 1] = None;
            }
            line += 1;
        }
    }

    /// Line `line`'s galley: kept, or laid out now.
    fn galley(
        &mut self,
        ui: &Ui,
        theme: &Theme,
        buffer: &Buffer,
        engine: Engine,
        line: usize,
    ) -> Arc<Galley> {
        if let Some(Some(galley)) = self.galleys.get(line) {
            return Arc::clone(galley);
        }
        let text = &buffer.lines()[line];
        let state = self.states.get(line).cloned().unwrap_or_default();
        let (spans, _) = tokens(engine, text, &state);
        let font = theme.text.mono.font_id();
        let mut job = LayoutJob::default();
        let mut at = 0;
        for span in spans {
            if span.range.start > at {
                append(
                    &mut job,
                    &text[at..span.range.start],
                    &font,
                    theme.color.ink,
                );
            }
            let color = match span.kind {
                TokenKind::Keyword => theme.color.accent,
                TokenKind::String => theme.color.ok,
                TokenKind::Number => theme.section(Section::Runtimes).fg,
                TokenKind::Comment => theme.color.mute,
                TokenKind::Other => theme.color.ink,
            };
            append(&mut job, &text[span.range.clone()], &font, color);
            at = span.range.end;
        }
        if at < text.len() || text.is_empty() {
            append(&mut job, &text[at..], &font, theme.color.ink);
        }
        let galley = ui.painter().layout_job(job);
        self.laid_out += 1;
        if let Some(slot) = self.galleys.get_mut(line) {
            *slot = Some(Arc::clone(&galley));
        }
        galley
    }

    /// The position under `at`, from the rows drawn this frame. Above them
    /// is their first line, below them their last.
    #[allow(clippy::too_many_arguments)] // The editor's parts, passed alone.
    fn pos_at(
        &mut self,
        ui: &Ui,
        theme: &Theme,
        buffer: &Buffer,
        engine: Engine,
        drawn: &[(usize, Rect)],
        text_x: f32,
        at: Pos2,
    ) -> Option<Pos> {
        let (first, top) = drawn.first()?;
        let (last, _) = drawn.last()?;
        let line = drawn
            .iter()
            .find(|(_, rect)| rect.y_range().contains(at.y))
            .map_or(
                if at.y < top.top() { *first } else { *last },
                |(line, _)| *line,
            );
        let galley = self.galley(ui, theme, buffer, engine, line);
        let x = at.x - (top.left() + text_x);
        let chars = galley.cursor_from_pos(vec2(x, 0.0)).index.0;
        let text = &buffer.lines()[line];
        let col = text
            .char_indices()
            .nth(chars)
            .map_or(text.len(), |(i, _)| i);
        Some(Pos { line, col })
    }
}

fn append(job: &mut LayoutJob, text: &str, font: &FontId, color: Color32) {
    job.append(text, 0.0, TextFormat::simple(font.clone(), color));
}

/// One line: the current line's fill, its number in the gutter, the
/// selection, the text, the cursor. The cursor's rectangle, if it is here.
fn paint_row(
    ui: &Ui,
    theme: &Theme,
    buffer: &Buffer,
    galley: &Arc<Galley>,
    row: &Row,
    focused: bool,
    preedit: &str,
) -> Option<Rect> {
    let painter = ui.painter();
    let cursor = buffer.cursor();
    let rect = row.rect;
    if cursor.line == row.line {
        painter.rect_filled(rect, CornerRadius::ZERO, theme.color.bg);
    }
    let gutter = rect.with_max_x(rect.left() + theme.size.editor_gutter);
    painter.rect_filled(gutter, CornerRadius::ZERO, theme.color.bg);
    let font = theme.text.mono_sm.font_id();
    let number = painter.layout_no_wrap((row.line + 1).to_string(), font, theme.color.mute);
    let room = gutter.shrink2(vec2(theme.space.s2, 0.0));
    let at = Align2::RIGHT_CENTER.align_size_within_rect(number.size(), room);
    painter.galley(at.min, number, theme.color.mute);

    let text = &buffer.lines()[row.line];
    let x_of = |col: usize| row.text_x + galley.pos_from_cursor(char_cursor(text, col)).min.x;
    if let Some((start, end)) = buffer.selection()
        && (start.line..=end.line).contains(&row.line)
    {
        let from = if start.line == row.line {
            x_of(start.col)
        } else {
            row.text_x
        };
        let to = if end.line == row.line {
            x_of(end.col)
        } else {
            // A selected line break: a little past the text.
            x_of(text.len()) + theme.space.s2
        };
        let band = Rect::from_x_y_ranges(from..=to, rect.y_range());
        painter.rect_filled(band, CornerRadius::ZERO, theme.color.selected);
    }
    let text_area = rect.with_min_x(row.text_x);
    let top = Align2::LEFT_CENTER
        .align_size_within_rect(galley.size(), text_area)
        .top();
    painter.galley(pos2(row.text_x, top), Arc::clone(galley), theme.color.ink);

    if cursor.line != row.line || !focused {
        return None;
    }
    let mut x = x_of(cursor.col);
    if !preedit.is_empty() {
        let font = theme.text.mono.font_id();
        let composed = painter.layout_no_wrap(preedit.to_owned(), font, theme.color.ink);
        let width = composed.size().x;
        let under = rect.bottom() - theme.size.border..=rect.bottom();
        painter.galley(pos2(x, top), composed, theme.color.ink);
        let line = Rect::from_x_y_ranges(x..=x + width, under);
        painter.rect_filled(line, CornerRadius::ZERO, theme.color.ink);
        x += width;
    }
    // A still caret: nothing blinks (rules.md 10).
    let caret = Rect::from_x_y_ranges(x..=x + theme.focus.width, rect.y_range());
    painter.rect_filled(caret, CornerRadius::ZERO, theme.color.ink);
    Some(caret)
}

/// The character cursor at byte `col` of `text`.
fn char_cursor(text: &str, col: usize) -> CCursor {
    CCursor::new(text[..col].chars().count())
}

/// The keys and the text of this frame, for a focused editor.
fn keys(ui: &Ui, buffer: &mut Buffer, cache: &mut Cache, output: &mut EditorOutput) {
    let mac = ui.ctx().os() == eframe::egui::os::OperatingSystem::Mac;
    let events = ui.input(|input| input.events.clone());
    for event in events {
        match event {
            Event::Text(text) if !text.chars().any(char::is_control) => {
                insert(buffer, &text, output);
            }
            Event::Paste(text) => insert(buffer, &text, output),
            Event::Copy => {
                if let Some(text) = buffer.selected_text() {
                    ui.ctx().copy_text(text);
                }
            }
            Event::Cut => {
                if let Some(text) = buffer.selected_text() {
                    ui.ctx().copy_text(text);
                    buffer.backspace();
                    output.changed = true;
                }
            }
            Event::Ime(ImeEvent::Preedit { text, .. }) => cache.preedit = text,
            Event::Ime(ImeEvent::Commit(text)) => {
                cache.preedit.clear();
                if text != "\n" && text != "\r" {
                    insert(buffer, &text, output);
                }
            }
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => key_press(buffer, key, modifiers, mac, output),
            _ => {}
        }
    }
}

fn insert(buffer: &mut Buffer, text: &str, output: &mut EditorOutput) {
    if text.is_empty() {
        return;
    }
    match buffer.insert(text) {
        Ok(()) => output.changed = true,
        Err(_) => output.refused = true,
    }
}

fn key_press(
    buffer: &mut Buffer,
    key: Key,
    modifiers: Modifiers,
    mac: bool,
    output: &mut EditorOutput,
) {
    let select = modifiers.shift;
    // Word moves: Alt on macOS, Ctrl elsewhere. On macOS, Cmd with an arrow
    // goes to the line's or the text's end.
    let word = if mac { modifiers.alt } else { modifiers.ctrl };
    let ends = mac && modifiers.mac_cmd;
    let to = match key {
        Key::ArrowLeft if ends => Some(Move::Home),
        Key::ArrowRight if ends => Some(Move::End),
        Key::ArrowLeft if word => Some(Move::WordLeft),
        Key::ArrowRight if word => Some(Move::WordRight),
        Key::ArrowLeft => Some(Move::Left),
        Key::ArrowRight => Some(Move::Right),
        Key::ArrowUp if ends => Some(Move::Top),
        Key::ArrowDown if ends => Some(Move::Bottom),
        Key::ArrowUp => Some(Move::Up),
        Key::ArrowDown => Some(Move::Down),
        Key::Home if modifiers.command => Some(Move::Top),
        Key::End if modifiers.command => Some(Move::Bottom),
        Key::Home => Some(Move::Home),
        Key::End => Some(Move::End),
        _ => None,
    };
    if let Some(to) = to {
        return buffer.move_cursor(to, select);
    }
    match key {
        Key::Enter if modifiers.command => output.run = true,
        Key::Enter => insert(buffer, "\n", output),
        Key::Escape => output.cancel = true,
        Key::Backspace => {
            buffer.backspace();
            output.changed = true;
        }
        Key::Delete => {
            buffer.delete();
            output.changed = true;
        }
        Key::A if modifiers.command => buffer.select_all(),
        Key::Z if modifiers.command && modifiers.shift => output.changed |= buffer.redo(),
        Key::Z if modifiers.command => output.changed |= buffer.undo(),
        Key::Y if modifiers.command && !mac => output.changed |= buffer.redo(),
        _ => {}
    }
}

#[cfg(test)]
mod tests;
