use eframe::egui::{self, Event, Key, Modifiers, RawInput};
use marsa_core::db_browser::{Engine, LineState};

use super::*;
use crate::widgets::harness::Harness;

const LIMIT: usize = 256 * 1024;

/// An editor that keeps its buffer, and what each frame asked.
struct Live {
    harness: Harness,
    buffer: Buffer,
    output: EditorOutput,
    rect: Rect,
    laid_out: u64,
}

impl Live {
    fn new(text: &str) -> Self {
        Self {
            harness: Harness::new(egui::Theme::Light),
            buffer: Buffer::new(text, LIMIT),
            output: EditorOutput::default(),
            rect: Rect::NOTHING,
            laid_out: 0,
        }
    }

    fn frame(&mut self, events: Vec<Event>) {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
            events,
            ..RawInput::default()
        };
        let buffer = &mut self.buffer;
        let (output, rect, laid_out) = (&mut self.output, &mut self.rect, &mut self.laid_out);
        self.harness.frame(input, |ui| {
            *output = Editor::new("test_editor", "SQL", buffer, Engine::Mysql, 10).show(ui);
            *rect = ui.min_rect();
            *laid_out = super::laid_out(ui, "test_editor");
        });
    }

    /// A click in the text: the editor takes the focus.
    fn focus(&mut self) {
        self.frame(Vec::new());
        let at = self.rect.min + vec2(80.0, 12.0);
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        self.frame(vec![Event::PointerMoved(at), button(true)]);
        self.frame(vec![button(false)]);
        self.frame(Vec::new());
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) {
        self.frame(vec![Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        }]);
    }

    fn typed(&mut self, text: &str) {
        self.frame(vec![Event::Text(text.to_owned())]);
    }
}

fn long_text(lines: usize) -> String {
    (0..lines)
        .map(|n| format!("select {n} from users where id = {n}; -- line {n}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_key_press_in_five_thousand_lines_lays_out_one_line() {
    let mut live = Live::new(&long_text(5_000));
    live.focus();
    let shown = live.laid_out;
    assert!(shown > 0 && shown < 60, "only the lines in view: {shown}");
    live.typed("x");
    assert!(live.output.changed);
    assert_eq!(live.laid_out - shown, 1, "one line again");
    live.frame(Vec::new());
    assert_eq!(live.laid_out - shown, 1, "nothing on a quiet frame");
}

#[test]
fn a_comment_over_lines_colors_each_line_and_closing_it_stops_there() {
    let mut buffer = Buffer::new("select 1;\nselect 2;\nselect 3;", LIMIT);
    let mut cache = Cache::default();
    cache.sync(&mut buffer, Engine::Mysql, false);
    buffer.set_cursor(Pos { line: 0, col: 0 }, false);
    buffer.insert("/*").expect("fits");
    cache.sync(&mut buffer, Engine::Mysql, false);
    let comment = LineState::Comment { depth: 1 };
    assert_eq!(cache.states, [LineState::Code, comment.clone(), comment]);
    buffer.set_cursor(Pos { line: 1, col: 0 }, false);
    buffer.insert("*/").expect("fits");
    cache.sync(&mut buffer, Engine::Mysql, false);
    assert_eq!(cache.states[2], LineState::Code);
}

#[test]
fn tab_leaves_the_editor_and_inserts_nothing() {
    let mut live = Live::new("select 1;");
    live.focus();
    live.key(Key::Tab, Modifiers::NONE);
    live.frame(Vec::new());
    assert_eq!(live.buffer.text(), "select 1;");
    assert!(!live.output.changed);
}

#[test]
fn command_enter_runs_and_changes_nothing_and_esc_cancels() {
    let mut live = Live::new("select 1;");
    live.focus();
    live.key(Key::Enter, Modifiers::COMMAND);
    assert!(live.output.run);
    assert_eq!(live.buffer.text(), "select 1;");
    live.key(Key::Escape, Modifiers::NONE);
    assert!(live.output.cancel);
    live.key(Key::Enter, Modifiers::NONE);
    assert_eq!(live.buffer.lines().len(), 2, "a plain Enter adds a line");
}

#[test]
fn a_paste_past_the_limit_is_refused() {
    let mut live = Live::new("");
    live.buffer = Buffer::new("", 8);
    live.focus();
    live.frame(vec![Event::Paste("select 1, 2, 3;".into())]);
    assert!(live.output.refused);
    assert!(live.buffer.is_empty());
}

#[test]
fn text_from_the_input_method_enters_only_when_committed() {
    let mut live = Live::new("");
    live.focus();
    live.frame(vec![Event::Ime(ImeEvent::Preedit {
        text: "مر".into(),
        active_range_chars: None,
    })]);
    assert!(live.buffer.is_empty());
    live.frame(vec![Event::Ime(ImeEvent::Commit("مرسى".into()))]);
    assert_eq!(live.buffer.text(), "مرسى");
}

#[test]
fn the_editor_is_a_named_text_input_for_a_screen_reader() {
    let harness = Harness::new(egui::Theme::Light);
    let mut buffer = Buffer::new("select 1;", LIMIT);
    let tree = harness.accessibility(|ui| {
        Editor::new("named", "SQL", &mut buffer, Engine::Postgres, 4).show(ui);
    });
    let node = tree.node_with_label("SQL").expect("editor");
    assert_eq!(node.role(), Role::MultilineTextInput);
}
