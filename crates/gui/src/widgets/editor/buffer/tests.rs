use super::*;

const LIMIT: usize = 256 * 1024;

fn at(line: usize, col: usize) -> Pos {
    Pos { line, col }
}

fn typed(buffer: &mut Buffer, text: &str) {
    for c in text.chars() {
        buffer.insert(&c.to_string()).expect("under the limit");
    }
}

#[test]
fn an_empty_text_is_one_empty_line() {
    let buffer = Buffer::new("", LIMIT);
    assert_eq!(buffer.lines(), [""]);
    assert!(buffer.is_empty());
    assert_eq!(buffer.cursor(), at(0, 0));
}

#[test]
fn typing_and_a_line_break_change_only_their_lines() {
    let mut buffer = Buffer::new("select 1;\nselect 2;", LIMIT);
    buffer.set_cursor(at(0, 8), false);
    typed(&mut buffer, "0");
    assert_eq!(buffer.lines(), ["select 10;", "select 2;"]);
    buffer.insert("\n").expect("fits");
    assert_eq!(buffer.lines(), ["select 10", ";", "select 2;"]);
    assert_eq!(buffer.cursor(), at(1, 0));
    assert_eq!(buffer.len(), buffer.text().len());
}

#[test]
fn backspace_at_a_lines_start_joins_it_and_delete_at_its_end_too() {
    let mut buffer = Buffer::new("ab\ncd", LIMIT);
    buffer.set_cursor(at(1, 0), false);
    buffer.backspace();
    assert_eq!(buffer.lines(), ["abcd"]);
    assert_eq!(buffer.cursor(), at(0, 2));
    buffer.insert("\n").expect("fits");
    buffer.set_cursor(at(0, 2), false);
    buffer.delete();
    assert_eq!(buffer.lines(), ["abcd"]);
    // Nothing before the start, nothing after the end.
    buffer.set_cursor(at(0, 0), false);
    buffer.backspace();
    buffer.move_cursor(Move::Bottom, false);
    buffer.delete();
    assert_eq!(buffer.text(), "abcd");
}

#[test]
fn a_selection_over_lines_is_replaced_and_its_text_read() {
    let mut buffer = Buffer::new("one\ntwo\nthree", LIMIT);
    buffer.set_cursor(at(0, 1), false);
    buffer.set_cursor(at(2, 2), true);
    assert_eq!(buffer.selection(), Some((at(0, 1), at(2, 2))));
    assert_eq!(buffer.selected_text().as_deref(), Some("ne\ntwo\nth"));
    buffer.insert("X").expect("fits");
    assert_eq!(buffer.lines(), ["oXree"]);
    assert_eq!(buffer.selection(), None);
}

#[test]
fn a_selection_made_backwards_reads_start_first_and_left_collapses_it() {
    let mut buffer = Buffer::new("select * from users", LIMIT);
    buffer.move_cursor(Move::End, false);
    buffer.move_cursor(Move::WordLeft, true);
    assert_eq!(buffer.selected_text().as_deref(), Some("users"));
    buffer.move_cursor(Move::Left, false);
    assert_eq!(buffer.cursor(), at(0, 14));
    assert_eq!(buffer.selection(), None);
    buffer.select_all();
    assert_eq!(
        buffer.selected_text().as_deref(),
        Some("select * from users")
    );
}

#[test]
fn a_pasted_text_of_many_lines_with_crlf_becomes_lines() {
    let mut buffer = Buffer::new("-- before\n", LIMIT);
    buffer.move_cursor(Move::Bottom, false);
    buffer
        .insert("select *\r\nfrom posts\r\nwhere id = 1;")
        .expect("fits");
    assert_eq!(
        buffer.lines(),
        ["-- before", "select *", "from posts", "where id = 1;"]
    );
    assert_eq!(buffer.cursor(), at(3, 13));
}

#[test]
fn undo_takes_back_a_word_at_a_time_and_redo_puts_it_again() {
    let mut buffer = Buffer::new("", LIMIT);
    typed(&mut buffer, "select");
    typed(&mut buffer, " ");
    typed(&mut buffer, "1");
    assert_eq!(buffer.text(), "select 1");
    assert!(buffer.undo());
    assert_eq!(buffer.text(), "select ");
    assert!(buffer.undo());
    assert!(buffer.undo());
    assert_eq!(buffer.text(), "");
    assert!(!buffer.undo());
    assert!(buffer.redo());
    assert_eq!(buffer.text(), "select");
    assert_eq!(buffer.cursor(), at(0, 6));
    // A new change drops what could be redone.
    typed(&mut buffer, "x");
    assert!(!buffer.redo());
}

#[test]
fn undo_of_a_delete_puts_the_text_and_the_cursor_back() {
    let mut buffer = Buffer::new("ab\ncd", LIMIT);
    buffer.set_cursor(at(1, 0), false);
    buffer.backspace();
    assert!(buffer.undo());
    assert_eq!(buffer.lines(), ["ab", "cd"]);
    assert_eq!(buffer.cursor(), at(1, 0));
    buffer.set_cursor(at(0, 0), false);
    buffer.set_cursor(at(1, 1), true);
    buffer.insert("").expect("fits");
    assert_eq!(buffer.text(), "d");
    assert!(buffer.undo());
    assert_eq!(buffer.text(), "ab\ncd");
    assert_eq!(buffer.selection(), Some((at(0, 0), at(1, 1))));
}

#[test]
fn arabic_and_emoji_move_and_delete_a_whole_character() {
    let mut buffer = Buffer::new("-- مرسى 🚢", LIMIT);
    buffer.move_cursor(Move::End, false);
    buffer.backspace();
    assert_eq!(buffer.text(), "-- مرسى ");
    buffer.move_cursor(Move::Left, false);
    buffer.move_cursor(Move::Left, false);
    let col = buffer.cursor().col;
    assert!(buffer.text().is_char_boundary(col));
    buffer.delete();
    assert_eq!(buffer.text(), "-- مرس ");
    typed(&mut buffer, "ى");
    assert_eq!(buffer.text(), "-- مرسى ");
    // A position inside a character is moved to its edge.
    buffer.set_cursor(at(0, 4), false);
    assert_eq!(buffer.cursor(), at(0, 3));
}

#[test]
fn up_and_down_keep_the_column_in_characters() {
    let mut buffer = Buffer::new("select name\nفي\nfrom users", LIMIT);
    buffer.set_cursor(at(0, 9), false);
    buffer.move_cursor(Move::Down, false);
    assert_eq!(buffer.cursor(), at(1, "في".len()), "a short line: its end");
    buffer.move_cursor(Move::Down, false);
    assert_eq!(buffer.cursor(), at(2, 9), "the column comes back");
    buffer.move_cursor(Move::Down, false);
    assert_eq!(buffer.cursor(), at(2, 10), "past the last line: its end");
    buffer.move_cursor(Move::Top, true);
    assert_eq!(buffer.selection(), Some((at(0, 0), at(2, 10))));
}

#[test]
fn a_paste_past_the_limit_is_refused_and_changes_nothing() {
    let mut buffer = Buffer::new("select 1", 10);
    assert_eq!(buffer.insert("234"), Err(TooLong { limit: 10 }));
    assert_eq!(buffer.text(), "select 1");
    buffer.insert("2;").expect("ten bytes fit");
    // Over a selection, the selected bytes make room.
    buffer.select_all();
    buffer.insert("select 99;").expect("fits in place");
}

#[test]
fn offsets_and_positions_agree_with_the_text() {
    let buffer = Buffer::new("ab\nمرسى\n", LIMIT);
    let text = buffer.text();
    for offset in 0..=text.len() {
        if !text.is_char_boundary(offset) {
            continue;
        }
        let pos = buffer.pos_at(offset);
        assert_eq!(buffer.offset(pos), offset, "{offset}");
    }
    assert_eq!(buffer.pos_at(usize::MAX), at(2, 0));
}
