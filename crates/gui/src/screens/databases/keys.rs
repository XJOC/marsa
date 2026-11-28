//! Valkey's keys (layout.md, "Valkey keys"): "Find a key", the type menu,
//! and the list of names on the start side; the chosen key's facts and value
//! on the end side. Read-only.
//!
//! Only the rows in view are drawn, in the list and in the grid. The list is
//! one stop for the keyboard: with the focus on it, the arrows open the key
//! before or after.

use eframe::egui::text::LayoutJob;
use eframe::egui::{
    Align2, CornerRadius, EventFilter, Key, Modifiers, Rect, Role, ScrollArea, Sense, Stroke,
    StrokeKind, Ui, Vec2, WidgetInfo, vec2,
};
use marsa_core::codes;
use marsa_core::types::{DbCell, DbColumnKind, DbKeyKind, DbPageColumn, ServiceAction, ServiceId};

use super::super::lines::{message, state};
use super::REFRESH_KEYS;
use super::pane::{self, Pane, PaneAction};
use crate::model::{
    Action, KEY_LIST_LIMIT, KeyBrowser, KeyList, KeyShape, KeyValue, KeysAction, OpenKey,
};
use crate::strings;
use crate::theme::page::{DB_LIST_WIDTH, DB_VALUE_HEIGHT};
use crate::theme::{Direction, Section, Theme};
use crate::widgets::grid::{Grid, GridEvent, column_widths};
use crate::widgets::list_item;
use crate::widgets::menu::{Item as MenuItem, Menu};
use crate::widgets::table::card;
use crate::widgets::tag::Tag;
use crate::widgets::{Button, Field, Look, Text, Touch, paint_focus_ring};

/// The characters a key's name keeps at its end when it is cut in the middle.
const NAME_END: usize = 12;
/// The rows kept above the key the arrows reach.
const ROWS_ABOVE: usize = 3;
/// The longest field name that titles the value pane.
const TITLE_CHARS: usize = 40;

/// "Start Valkey".
pub(super) fn start() -> Action {
    Action::Service {
        service: ServiceId::Database(marsa_core::databases::ServerKind::Valkey),
        action: ServiceAction::Start,
    }
}

pub(super) fn show(ui: &mut Ui, theme: &Theme, keys: &KeyBrowser, actions: &mut Vec<Action>) {
    let refresh = ui.input_mut(|input| {
        REFRESH_KEYS
            .iter()
            .any(|shortcut| input.consume_shortcut(shortcut))
    });
    if refresh {
        actions.push(Action::Keys(KeysAction::Refresh));
    }
    let size = ui.available_size();
    ui.allocate_ui_with_layout(size, Direction::CURRENT.row(), |ui| {
        ui.spacing_mut().item_spacing.x = theme.space.section_gap;
        let height = ui.available_height();
        let column = Direction::CURRENT.column();
        ui.allocate_ui_with_layout(vec2(DB_LIST_WIDTH, height), column, |ui| {
            master(ui, theme, keys, actions);
        });
        let rest = vec2(ui.available_width(), height);
        ui.allocate_ui_with_layout(rest, column, |ui| detail(ui, theme, keys, actions));
    });
}

/// The search, the type, the names, and what the list counts.
fn master(ui: &mut Ui, theme: &Theme, keys: &KeyBrowser, actions: &mut Vec<Action>) {
    ui.spacing_mut().item_spacing.y = theme.space.s2;
    let mut search = keys.search.clone();
    let field = ui.add(Field::search(&mut search, strings::FIND_KEY).hint(strings::FIND_KEY));
    if search != keys.search {
        actions.push(Action::Keys(KeysAction::Search(search)));
    }
    // A one-line field gives up the focus on Enter.
    if field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
        actions.push(Action::Keys(KeysAction::Submit));
        field.request_focus();
    }
    type_menu(ui, keys, actions);
    let captions = captions(keys);
    let under = captions.len() as f32 * theme.size.control_in_row;
    let height = (ui.available_height() - under).max(theme.size.control);
    let size = vec2(ui.available_width(), height);
    ui.allocate_ui_with_layout(size, Direction::CURRENT.column(), |ui| {
        card(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            ui.set_min_height(height);
            if let Some(list) = &keys.list {
                names(ui, theme, keys, list, actions);
            }
        });
    });
    for caption in captions {
        ui.add(Text::new(&caption, theme.text.caption, theme.color.mute).truncate());
    }
}

fn type_menu(ui: &mut Ui, keys: &KeyBrowser, actions: &mut Vec<Action>) {
    let button = ui.add(Button::secondary(strings::key_filter(keys.kind)).in_row());
    let items: Vec<MenuItem<'_, Option<DbKeyKind>>> = std::iter::once(None)
        .chain(DbKeyKind::ALL.into_iter().map(Some))
        .map(|kind| MenuItem::new(kind, strings::key_filter(kind)))
        .collect();
    if let Some(kind) = Menu::new(&items).show(&button)
        && kind != keys.kind
    {
        actions.push(Action::Keys(KeysAction::Kind(kind)));
    }
}

/// The lines under the list.
fn captions(keys: &KeyBrowser) -> Vec<String> {
    let Some(list) = &keys.list else {
        let reading = keys.loading.then(|| strings::DB_READING_KEYS.to_owned());
        return reading.into_iter().collect();
    };
    let mut lines = Vec::new();
    if list.names.is_empty() {
        lines.push(
            match (list.more(), keys.narrowed()) {
                (true, _) => strings::NO_KEYS_YET,
                (false, true) => strings::NO_KEYS_MATCH,
                (false, false) => strings::NO_KEYS,
            }
            .to_owned(),
        );
    }
    if keys.loading {
        lines.push(strings::DB_READING_KEYS.to_owned());
    } else if list.full() {
        lines.push(strings::key_list_full(KEY_LIST_LIMIT));
    }
    lines.push(strings::keys_in_database(list.total));
    if list.unshown > 0 {
        lines.push(strings::keys_unshown(list.unshown));
    }
    lines
}

/// The names, then "More keys" while Valkey has more.
fn names(ui: &mut Ui, theme: &Theme, keys: &KeyBrowser, list: &KeyList, actions: &mut Vec<Action>) {
    let row = theme.size.control;
    let area = ui.available_rect_before_wrap();
    let id = ui.make_persistent_id("db_keys_list");
    // The list's one focus stop, under its rows, which take the clicks.
    let focus = ui.interact(area, id, Sense::click());
    focus.widget_info(|| WidgetInfo::labeled(Role::List, true, strings::DB_KEY_LIST));
    let focused = focus.has_focus();
    if focused {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                id,
                EventFilter {
                    vertical_arrows: true,
                    ..EventFilter::default()
                },
            );
        });
    }
    let chosen = keys.open.as_ref().map(|open| open.name.as_str());
    let moved = step(ui, focused).and_then(|step| neighbour(&list.names, chosen, step));
    if let Some(name) = moved {
        actions.push(Action::Keys(KeysAction::Open(name.to_owned())));
    }
    let rows = list.names.len() + usize::from(list.more());
    let mut scroll = ScrollArea::vertical()
        .id_salt("db_keys_scroll")
        .auto_shrink(false)
        .max_height(area.height());
    if let Some(at) = moved.and_then(|name| list.names.iter().position(|n| n == name)) {
        // The key the arrows reached stays in view, a few rows from the top.
        scroll = scroll.vertical_scroll_offset(at.saturating_sub(ROWS_ABOVE) as f32 * row);
    }
    scroll.show_rows(ui, row, rows, |ui, range| {
        for index in range {
            let Some(name) = list.names.get(index) else {
                let more = Button::secondary(strings::DB_MORE_KEYS)
                    .in_row()
                    .disabled_if(keys.loading, strings::DB_READING_KEYS);
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), row),
                    Direction::CURRENT.row(),
                    |ui| {
                        ui.add_space(theme.space.s2);
                        if ui.add(more).clicked() {
                            actions.push(Action::Keys(KeysAction::More));
                        }
                    },
                );
                continue;
            };
            if name_row(ui, theme, name, chosen == Some(name.as_str()), row) {
                ui.memory_mut(|memory| memory.request_focus(id));
                actions.push(Action::Keys(KeysAction::Open(name.clone())));
            }
        }
    });
    if focused {
        paint_focus_ring(ui.painter(), theme, area, theme.radius.lg);
    }
}

/// The arrows with the focus on the list, or with Alt from anywhere.
fn step(ui: &mut Ui, focused: bool) -> Option<isize> {
    ui.input_mut(|input| {
        let mut pressed = |key| {
            input.consume_key(Modifiers::ALT, key)
                || (focused && input.consume_key(Modifiers::NONE, key))
        };
        if pressed(Key::ArrowDown) {
            Some(1)
        } else if pressed(Key::ArrowUp) {
            Some(-1)
        } else {
            None
        }
    })
}

/// The name `step` rows from `chosen`; the first with nothing chosen, and
/// none past an end.
fn neighbour<'a>(names: &'a [String], chosen: Option<&str>, step: isize) -> Option<&'a str> {
    let now = chosen.and_then(|chosen| names.iter().position(|name| name == chosen));
    let next = match now {
        Some(at) => at.checked_add_signed(step)?,
        None => 0,
    };
    names.get(next).map(String::as_str)
}

/// One name, in mono so its cut is exact. Its tooltip and what a screen
/// reader hears are the whole name. `true` when clicked.
fn name_row(ui: &mut Ui, theme: &Theme, name: &str, chosen: bool, height: f32) -> bool {
    let size = vec2(ui.available_width(), height);
    let (rect, response) = ui.allocate_exact_size(size, Sense::CLICK);
    let look = Look {
        touch: Touch::of(&response),
        focused: false,
    };
    let paint = list_item::paint(theme, chosen, look);
    let corner = CornerRadius::same(theme.radius.md);
    if let Some(fill) = paint.fill {
        ui.painter().rect_filled(rect, corner, fill);
    }
    if let Some(border) = paint.border {
        let stroke = Stroke::new(theme.size.border, border);
        ui.painter()
            .rect_stroke(rect, corner, stroke, StrokeKind::Inside);
    }
    let font = theme.text.mono_sm.font_id();
    let width = ui.fonts_mut(|fonts| fonts.glyph_width(&font, '0'));
    let inner = rect.shrink2(vec2(theme.space.s3, 0.0));
    let most = (inner.width() / width) as usize;
    let shown = middle(name, most);
    let mut job = LayoutJob::simple_singleline(shown, font, theme.color.ink);
    // A wide script is still cut at the edge.
    job.wrap.max_width = inner.width();
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = ui.painter().layout_job(job);
    let room = Rect::from_min_size(inner.min, vec2(inner.width(), height));
    let at = Align2::LEFT_CENTER.align_size_within_rect(galley.size(), room);
    ui.painter().galley(at.min, galley, theme.color.ink);
    response.widget_info(|| WidgetInfo::selected(Role::Link, true, chosen, name));
    response.on_hover_text(name).clicked()
}

/// `name` in at most `most` characters, cut in the middle: Laravel's keys
/// share a long start, so their ends tell them apart. Control characters
/// show as spaces: a row is one line.
fn middle(name: &str, most: usize) -> String {
    let clean = |c: char| if c.is_control() { ' ' } else { c };
    let count = name.chars().count();
    if count <= most {
        return name.chars().map(clean).collect();
    }
    let end = NAME_END.min(most.saturating_sub(2));
    let start = most.saturating_sub(end + 1);
    let head = name.chars().take(start).map(clean);
    let tail = name.chars().skip(count - end).map(clean);
    head.chain(std::iter::once('…')).chain(tail).collect()
}

/// The chosen key, or why there is none.
fn detail(ui: &mut Ui, theme: &Theme, keys: &KeyBrowser, actions: &mut Vec<Action>) {
    ui.spacing_mut().item_spacing.y = theme.space.s3;
    if let Some((code, words)) = &keys.error {
        let sentence = strings::error_text(code);
        pane::banner(ui, theme, &sentence, words.as_deref());
    }
    let Some(open) = &keys.open else {
        empty(ui, theme, keys);
        return;
    };
    header(ui, theme, open, actions);
    match (&open.error, &open.value) {
        (Some((code, _)), _) if code == codes::DB_KEY_NOT_FOUND => {
            let button = Button::primary(strings::DB_REFRESH);
            if state(
                ui,
                theme,
                strings::KEY_GONE,
                strings::KEY_GONE_WHY,
                Some(button),
            ) {
                actions.push(Action::Keys(KeysAction::Refresh));
            }
        }
        (Some((code, words)), _) => {
            let sentence = strings::error_text(code);
            pane::banner(ui, theme, &sentence, words.as_deref());
        }
        (None, Some(value)) => shown(ui, theme, open, value, actions),
        (None, None) => message(ui, theme, strings::DB_READING_KEY),
    }
}

/// With no key chosen: why the list is empty, else what to do.
fn empty(ui: &mut Ui, theme: &Theme, keys: &KeyBrowser) {
    let none = keys
        .list
        .as_ref()
        .is_some_and(|list| list.names.is_empty() && !list.more());
    if !none {
        message(ui, theme, strings::CHOOSE_KEY);
    } else if keys.narrowed() {
        state(
            ui,
            theme,
            strings::NO_KEYS_MATCH,
            strings::NO_KEYS_MATCH_HOW,
            None,
        );
    } else {
        let sentence = strings::no_keys_in(keys.db);
        state(ui, theme, &sentence, strings::NO_KEYS_HOW, None);
    }
}

/// The key's name, and Refresh at the end.
fn header(ui: &mut Ui, theme: &Theme, open: &OpenKey, actions: &mut Vec<Action>) {
    let line = vec2(ui.available_width(), theme.size.control);
    ui.allocate_ui_with_layout(line, Direction::CURRENT.row(), |ui| {
        let refresh = Button::secondary(strings::DB_REFRESH).in_row();
        let room = (ui.available_width() - refresh.width(ui) - theme.space.s3).max(0.0);
        let name = middle(&open.name, usize::MAX);
        ui.allocate_ui_with_layout(
            vec2(room, theme.size.control),
            Direction::CURRENT.row(),
            |ui| {
                ui.set_min_width(room);
                ui.add(Text::new(&name, theme.text.heading, theme.color.ink).truncate());
            },
        );
        if ui.add(refresh).clicked() {
            actions.push(Action::Keys(KeysAction::Refresh));
        }
    });
}

/// The facts line, then the value by its shape.
fn shown(ui: &mut Ui, theme: &Theme, open: &OpenKey, value: &KeyValue, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme.space.s3;
        let word = strings::key_type(&value.shape);
        ui.add(Tag::key(word, theme, Section::Databases));
        let expiry = strings::key_expiry(value.ttl);
        ui.add(Text::new(&expiry, theme.text.caption, theme.color.mute));
        if let Some(size) = strings::key_size(&value.shape, value.total) {
            ui.add(Text::new(&size, theme.text.caption, theme.color.mute));
        }
    });
    match value.shape {
        KeyShape::Other(_) => message(ui, theme, strings::KEY_NOT_SHOWN),
        KeyShape::String => {
            let Some(cell) = value.rows.first().and_then(|row| row.first()) else {
                return;
            };
            let height = ui.available_height().max(DB_VALUE_HEIGHT);
            let pane = Pane {
                title: strings::KEY_VALUE,
                caption: Some(strings::string_caption(value.total)),
                cell,
                height,
                closes: false,
            };
            let action = pane::show(ui, theme, pane, |ui| cut_line(ui, theme, cell));
            if action == Some(PaneAction::Copy) {
                actions.push(Action::Keys(KeysAction::Copy));
            }
        }
        _ => collection(ui, theme, open, value, actions),
    }
}

/// "The first 8 KB of 2.1 MB.", under a cut value.
fn cut_line(ui: &mut Ui, theme: &Theme, cell: &DbCell) {
    if let DbCell::Cut { text, bytes } = cell {
        let cut = strings::key_cut(text.len() as u64, *bytes);
        ui.add(Text::new(&cut, theme.text.caption, theme.color.mute));
    }
}

/// The columns of a collection's grid.
fn columns(shape: &KeyShape) -> Vec<DbPageColumn> {
    strings::key_columns(shape)
        .iter()
        .map(|name| DbPageColumn {
            name: (*name).to_owned(),
            kind: if *name == strings::KEY_INDEX || *name == strings::KEY_SCORE {
                DbColumnKind::Number
            } else {
                DbColumnKind::Text
            },
        })
        .collect()
}

/// The grid of a hash, a list, a set, or a sorted set, the line under it, and
/// the value pane.
fn collection(
    ui: &mut Ui,
    theme: &Theme,
    open: &OpenKey,
    value: &KeyValue,
    actions: &mut Vec<Action>,
) {
    let cell = open.current_cell().filter(|_| open.pane);
    let below = cell.map_or(0.0, |_| DB_VALUE_HEIGHT + theme.space.s3);
    let height = (ui.available_height() - below).max(theme.size.table_header);
    let columns = columns(&value.shape);
    let size = vec2(ui.available_width(), height);
    ui.allocate_ui_with_layout(size, Direction::CURRENT.column(), |ui| {
        card(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            ui.set_min_height(height);
            let rows = height - theme.size.grid_footer;
            let room = vec2(ui.available_width(), rows);
            ui.allocate_ui_with_layout(room, Direction::CURRENT.column(), |ui| {
                ui.set_min_height(rows);
                grid(ui, open, value, &columns, actions);
            });
            footer(ui, theme, value);
        });
    });
    let Some(cell) = cell else {
        return;
    };
    let pane = Pane {
        title: title(open, value, &columns),
        caption: Some(strings::element_caption(cell)),
        cell,
        height: DB_VALUE_HEIGHT,
        closes: true,
    };
    match pane::show(ui, theme, pane, |ui| cut_line(ui, theme, cell)) {
        Some(PaneAction::Copy) => actions.push(Action::Keys(KeysAction::Copy)),
        Some(PaneAction::Close) => actions.push(Action::Keys(KeysAction::ClosePane)),
        None => {}
    }
}

/// The pane's title: the column's name; a hash's value goes by its field's
/// name, when that is short and one line.
fn title<'a>(open: &OpenKey, value: &'a KeyValue, columns: &'a [DbPageColumn]) -> &'a str {
    let (row, column) = open.current;
    if value.shape == KeyShape::Hash && column == 1 {
        let field = row.and_then(|row| value.rows.get(row)?.first());
        if let Some(DbCell::Text(field)) = field
            && field.chars().count() <= TITLE_CHARS
            && !field.chars().any(char::is_control)
        {
            return field;
        }
    }
    columns
        .get(column)
        .map_or(strings::KEY_VALUE, |column| column.name.as_str())
}

fn grid(
    ui: &mut Ui,
    open: &OpenKey,
    value: &KeyValue,
    columns: &[DbPageColumn],
    actions: &mut Vec<Action>,
) {
    // Measured once for each value that arrives.
    let id = ui.make_persistent_id(("db_key_widths", open.serial, &open.name));
    let widths = match ui.data(|data| data.get_temp::<Vec<f32>>(id)) {
        Some(widths) => widths,
        None => {
            let widths = column_widths(ui, columns, &value.rows);
            ui.data_mut(|data| data.insert_temp(id, widths.clone()));
            widths
        }
    };
    let events = Grid::new("db_key", columns, &value.rows, &widths, open.current).show(ui);
    for event in events {
        let action = match event {
            GridEvent::Current { row, column } => KeysAction::Current { row, column },
            GridEvent::TogglePane => KeysAction::TogglePane,
            GridEvent::ClosePane => KeysAction::ClosePane,
            GridEvent::Copy => KeysAction::Copy,
            // A key's value has no pages, and its grid does not sort.
            GridEvent::Sort(_) | GridEvent::NextPage | GridEvent::PreviousPage => continue,
        };
        actions.push(Action::Keys(action));
    }
}

/// The card's last line: how many elements, or how many of them show.
fn footer(ui: &mut Ui, theme: &Theme, value: &KeyValue) {
    let width = ui.available_width();
    let line = vec2(width, theme.size.border);
    let (rect, _) = ui.allocate_exact_size(line, Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, theme.color.line);
    let height = theme.size.grid_footer - theme.size.border;
    ui.allocate_ui_with_layout(vec2(width, height), Direction::CURRENT.row(), |ui| {
        ui.add_space(theme.space.s3);
        let shown = value.rows.len() as u64;
        let text = strings::key_footer(&value.shape, shown, value.total);
        ui.add(Text::new(&text, theme.text.body_sm, theme.color.mute));
    });
}

#[cfg(test)]
mod tests;
