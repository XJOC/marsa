//! The data grid (components.md, Data grid): one page of a table, or one SQL
//! result, read-only. It knows nothing of the model or the daemon: it draws
//! the cells it is given and returns what the user did ([`GridEvent`]).
//! Sorting and paging are the caller's requests; the grid never sorts rows.
//!
//! Only the rows in view are drawn, and a cell outside the clip rectangle
//! lays out no text, so a page of 500 rows costs what its visible rows do.
//! The header stays while the rows scroll; both scroll sideways together.
//!
//! One Tab stop (the WAI-ARIA grid pattern): the arrows move the current
//! cell, Home and End go to the first and last column, Cmd or Ctrl with
//! them to the first and last row, Page Up and Down by a view; up from the
//! first row is the header, where Enter or Space sorts. Enter or Space on a
//! cell opens or closes the value pane, Esc closes it, Cmd or Ctrl C copies.
//! Alt with the arrow toward the end is "Next", toward the start "Previous".

use eframe::egui::text::{LayoutJob, TextWrapping};
use eframe::egui::{
    Align2, Color32, CornerRadius, Event, EventFilter, FontId, Key, Modifiers, Painter, Pos2, Rect,
    Role, ScrollArea, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, pos2, vec2,
};
use marsa_core::types::{DbCell, DbColumnKind, DbPageColumn};

use super::tag;
use crate::strings;
use crate::theme::Theme;

/// What the user did in the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridEvent {
    /// The current cell moved: a row of the page, or `None` for the header.
    Current {
        row: Option<usize>,
        column: usize,
    },
    /// A header was chosen: ascending, descending, then the table's order.
    Sort(usize),
    /// Enter or Space on a cell: the value pane opens or closes.
    TogglePane,
    /// Esc: the value pane closes.
    ClosePane,
    /// Cmd or Ctrl C on a cell.
    Copy,
    NextPage,
    PreviousPage,
}

/// The current cell: a row, or `None` for the header, and a column.
pub type Current = (Option<usize>, usize);

pub struct Grid<'a> {
    id_salt: &'static str,
    columns: &'a [DbPageColumn],
    rows: &'a [Vec<DbCell>],
    widths: &'a [f32],
    current: Current,
    /// The sorted column and whether it is descending.
    sorted: Option<(usize, bool)>,
    sortable: bool,
}

impl<'a> Grid<'a> {
    /// `widths` comes from [`column_widths`], once for each page.
    pub fn new(
        id_salt: &'static str,
        columns: &'a [DbPageColumn],
        rows: &'a [Vec<DbCell>],
        widths: &'a [f32],
        current: Current,
    ) -> Self {
        Self {
            id_salt,
            columns,
            rows,
            widths,
            current,
            sorted: None,
            sortable: false,
        }
    }

    /// The headers sort, and this one is sorted.
    pub fn sortable(mut self, sorted: Option<(usize, bool)>) -> Self {
        self.sortable = true;
        self.sorted = sorted;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Vec<GridEvent> {
        let theme = Theme::of(ui.ctx()).clone();
        let mut events = Vec::new();
        let id = ui.make_persistent_id(self.id_salt);
        let pad = theme.space.s3;
        let width: f32 = self.widths.iter().sum::<f32>() + pad + pad;
        let header = theme.size.table_header;
        let row_height = theme.size.row_dense;
        let area = ui.available_rect_before_wrap();
        // The grid's one focus stop, under the rows, which take the clicks.
        let focus = ui.interact(area, id, Sense::click());
        focus.widget_info(|| {
            let label = strings::grid_label(self.rows.len(), self.columns.len());
            WidgetInfo::labeled(Role::Grid, true, label)
        });
        let focused = focus.has_focus();
        if focused {
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    id,
                    EventFilter {
                        tab: false,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                );
            });
            let view = (((area.height() - header) / row_height).floor() as usize).max(1);
            self.keys(ui, view, &mut events);
        }
        // Scroll to the current cell only when it moved.
        let moved = ui.data_mut(|data| {
            let last = data.get_temp_mut_or_insert_with(id.with("current"), || self.current);
            let moved = *last != self.current;
            *last = self.current;
            moved
        });
        ScrollArea::horizontal()
            .id_salt((self.id_salt, "across"))
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.set_min_width(width);
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                let (rect, response) = ui.allocate_exact_size(vec2(width, header), Sense::click());
                self.header(ui, &theme, rect, focused);
                if response.clicked()
                    && let Some(column) = self.column_at(rect, response.interact_pointer_pos())
                {
                    ui.memory_mut(|memory| memory.request_focus(id));
                    events.push(GridEvent::Current { row: None, column });
                    if self.sortable {
                        events.push(GridEvent::Sort(column));
                    }
                }
                ScrollArea::vertical()
                    .id_salt((self.id_salt, "down"))
                    .auto_shrink(false)
                    .show_rows(ui, row_height, self.rows.len(), |ui, range| {
                        for index in range {
                            let (rect, response) =
                                ui.allocate_exact_size(vec2(width, row_height), Sense::click());
                            if moved && self.current.0 == Some(index) {
                                ui.scroll_to_rect(rect, None);
                            }
                            self.row(ui, &theme, rect, index, focused);
                            if response.clicked()
                                && let Some(column) =
                                    self.column_at(rect, response.interact_pointer_pos())
                            {
                                ui.memory_mut(|memory| memory.request_focus(id));
                                let row = Some(index);
                                events.push(GridEvent::Current { row, column });
                            }
                        }
                    });
            });
        events
    }

    fn keys(&self, ui: &Ui, view: usize, events: &mut Vec<GridEvent>) {
        let last_row = self.rows.len().saturating_sub(1);
        let last_column = self.columns.len().saturating_sub(1);
        let (row, column) = self.current;
        let mut to = |row: Option<usize>, column: usize| {
            if (row, column) != self.current {
                events.push(GridEvent::Current { row, column });
            }
        };
        let pressed =
            |key, modifiers: Modifiers| ui.input_mut(|input| input.consume_key(modifiers, key));
        let command = Modifiers::COMMAND;
        if pressed(Key::ArrowRight, Modifiers::ALT) {
            events.push(GridEvent::NextPage);
        } else if pressed(Key::ArrowLeft, Modifiers::ALT) {
            events.push(GridEvent::PreviousPage);
        } else if pressed(Key::Home, command) {
            to((!self.rows.is_empty()).then_some(0), column);
        } else if pressed(Key::End, command) {
            to((!self.rows.is_empty()).then_some(last_row), column);
        } else if pressed(Key::Home, Modifiers::NONE) {
            to(row, 0);
        } else if pressed(Key::End, Modifiers::NONE) {
            to(row, last_column);
        } else if pressed(Key::ArrowLeft, Modifiers::NONE) {
            to(row, column.saturating_sub(1));
        } else if pressed(Key::ArrowRight, Modifiers::NONE) {
            to(row, (column + 1).min(last_column));
        } else if pressed(Key::ArrowUp, Modifiers::NONE) {
            to(row.and_then(|row| row.checked_sub(1)), column);
        } else if pressed(Key::ArrowDown, Modifiers::NONE) && !self.rows.is_empty() {
            to(Some(row.map_or(0, |row| (row + 1).min(last_row))), column);
        } else if pressed(Key::PageUp, Modifiers::NONE) {
            to(row.map(|row| row.saturating_sub(view)), column);
        } else if pressed(Key::PageDown, Modifiers::NONE) && !self.rows.is_empty() {
            to(
                Some(row.map_or(0, |row| (row + view).min(last_row))),
                column,
            );
        } else if pressed(Key::Enter, Modifiers::NONE) || pressed(Key::Space, Modifiers::NONE) {
            match row {
                None if self.sortable => events.push(GridEvent::Sort(column)),
                None => {}
                Some(_) => events.push(GridEvent::TogglePane),
            }
        } else if pressed(Key::Escape, Modifiers::NONE) {
            events.push(GridEvent::ClosePane);
        }
        let copied = ui.input(|input| input.events.iter().any(|e| matches!(e, Event::Copy)));
        if copied && row.is_some() {
            events.push(GridEvent::Copy);
        }
    }

    /// The column under `at`, a point on a row of `rect`.
    fn column_at(&self, rect: Rect, at: Option<eframe::egui::Pos2>) -> Option<usize> {
        let mut x = at?.x - rect.left();
        for (index, width) in self.widths.iter().enumerate() {
            if x < *width {
                return Some(index);
            }
            x -= width;
        }
        None
    }

    /// The cells of a row of `rect`: the rectangle of each column.
    fn cells(&self, rect: Rect, pad: f32) -> impl Iterator<Item = Rect> + '_ {
        let mut x = rect.left() + pad;
        self.widths.iter().map(move |width| {
            let cell = Rect::from_min_size(pos2(x, rect.top()), vec2(*width, rect.height()));
            x += width;
            cell
        })
    }

    fn header(&self, ui: &Ui, theme: &Theme, rect: Rect, focused: bool) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::ZERO, theme.color.bg);
        divider(&painter, theme, rect);
        let font = theme.text.body_sm.font_id();
        for (index, cell) in self.cells(rect, theme.space.s3).enumerate() {
            if !ui.clip_rect().intersects(cell) {
                continue;
            }
            let column = &self.columns[index];
            // A number column's header sits at the end, over its numbers.
            let end = column.kind == DbColumnKind::Number;
            let color = theme.color.ink2;
            let sorted = self.sorted.filter(|(sorted, _)| *sorted == index);
            // Room for the sort mark on the side away from the cell's edge.
            let mark = theme.space.s2 + theme.space.s1;
            let text_cell = match (sorted, end) {
                (None, _) => cell,
                (Some(_), false) => cell.with_max_x(cell.right() - mark),
                (Some(_), true) => cell.with_min_x(cell.left() + mark),
            };
            let drawn = text_in(
                &painter,
                theme,
                text_cell,
                &column.name,
                font.clone(),
                color,
                end,
            );
            if let Some((_, descending)) = sorted {
                let x = if end {
                    drawn.left() - mark
                } else {
                    drawn.right() + theme.space.s1
                };
                sort_mark(&painter, theme, pos2(x, cell.center().y), descending);
            }
            if focused && self.current == (None, index) {
                ring(&painter, theme, cell);
            }
        }
    }

    fn row(&self, ui: &Ui, theme: &Theme, rect: Rect, index: usize, focused: bool) {
        let painter = ui.painter_at(rect);
        if self.current.0 == Some(index) {
            painter.rect_filled(rect, CornerRadius::ZERO, theme.color.selected);
        }
        divider(&painter, theme, rect);
        let Some(cells) = self.rows.get(index) else {
            return;
        };
        for (column, cell_rect) in self.cells(rect, theme.space.s3).enumerate() {
            if !ui.clip_rect().intersects(cell_rect) {
                continue;
            }
            let kind = self
                .columns
                .get(column)
                .map_or(DbColumnKind::Other, |c| c.kind);
            let mono = matches!(kind, DbColumnKind::Number | DbColumnKind::Time);
            let font = if mono {
                theme.text.mono_sm.font_id()
            } else {
                theme.text.body_sm.font_id()
            };
            let end = kind == DbColumnKind::Number;
            match cells.get(column) {
                Some(DbCell::Null) | None => {
                    let height = theme.size.tag_in_grid;
                    let room = cell_rect.with_min_x(cell_rect.left() + theme.space.s3);
                    let size = vec2(room.width(), height);
                    let at = Align2::LEFT_CENTER.align_size_within_rect(size, room).min;
                    tag::paint(&painter, theme, at, strings::NULL_TAG, height);
                }
                Some(DbCell::Text(text)) => {
                    text_in(&painter, theme, cell_rect, text, font, theme.color.ink, end);
                }
                Some(DbCell::Cut { text, .. }) => {
                    let text = format!("{text}…");
                    text_in(
                        &painter,
                        theme,
                        cell_rect,
                        &text,
                        font,
                        theme.color.ink,
                        end,
                    );
                }
                Some(DbCell::Binary { bytes }) => {
                    let text = strings::binary_value(*bytes);
                    let font = theme.text.body_sm.font_id();
                    text_in(
                        &painter,
                        theme,
                        cell_rect,
                        &text,
                        font,
                        theme.color.mute,
                        false,
                    );
                }
            }
            if focused && self.current == (Some(index), column) {
                ring(&painter, theme, cell_rect);
            }
        }
    }
}

/// The widths of the columns: each from its header and the page's first
/// rows, within `size.grid.column`. Once for each page, not each frame.
pub fn column_widths(ui: &Ui, columns: &[DbPageColumn], rows: &[Vec<DbCell>]) -> Vec<f32> {
    let theme = Theme::of(ui.ctx());
    let sample = &rows[..rows.len().min(50)];
    let measure = |text: &str, font: FontId| {
        ui.painter()
            .layout_no_wrap(text.to_owned(), font, theme.color.ink)
            .size()
            .x
    };
    columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            // The header with room for the sort mark.
            let header = measure(&column.name, theme.text.body_sm.font_id()) + theme.space.s4;
            let widest = sample
                .iter()
                .filter_map(|row| match row.get(index) {
                    // The face the cell is drawn in (see `row`).
                    Some(DbCell::Text(text) | DbCell::Cut { text, .. }) => {
                        let mono = matches!(column.kind, DbColumnKind::Number | DbColumnKind::Time);
                        let font = if mono {
                            theme.text.mono_sm.font_id()
                        } else {
                            theme.text.body_sm.font_id()
                        };
                        Some(measure(text, font))
                    }
                    Some(DbCell::Binary { bytes }) => {
                        let text = strings::binary_value(*bytes);
                        Some(measure(&text, theme.text.body_sm.font_id()))
                    }
                    Some(DbCell::Null) | None => None,
                })
                .fold(header, f32::max);
            (widest + theme.space.s3 + theme.space.s3)
                .clamp(theme.size.grid_column_min, theme.size.grid_column_max)
        })
        .collect()
}

/// One line of `text` in `cell`, cut with "…" when it does not fit; at the
/// end of the cell when `end`. The rectangle the text took.
fn text_in(
    painter: &Painter,
    theme: &Theme,
    cell: Rect,
    text: &str,
    font: FontId,
    color: Color32,
    end: bool,
) -> Rect {
    let room = (cell.width() - theme.space.s3 - theme.space.s3).max(0.0);
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = TextWrapping {
        max_width: room,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    let inner = cell.shrink2(vec2(theme.space.s3, 0.0));
    let align = if end {
        Align2::RIGHT_CENTER
    } else {
        Align2::LEFT_CENTER
    };
    let drawn = align.align_size_within_rect(galley.size(), inner);
    painter.galley(drawn.min, galley, color);
    drawn
}

/// The sorted column's mark: a small triangle up (ascending) or down, its
/// start at `at`. Drawn, not a glyph: the window's fonts lack the arrows.
fn sort_mark(painter: &Painter, theme: &Theme, at: Pos2, descending: bool) {
    let half = theme.space.s1;
    let (tip, base) = if descending {
        (at.y + half, at.y - half)
    } else {
        (at.y - half, at.y + half)
    };
    let points = vec![
        pos2(at.x, base),
        pos2(at.x + half + half, base),
        pos2(at.x + half, tip),
    ];
    painter.add(Shape::convex_polygon(
        points,
        theme.color.ink2,
        Stroke::NONE,
    ));
}

fn divider(painter: &Painter, theme: &Theme, rect: Rect) {
    let line = rect.with_min_y(rect.bottom() - theme.size.border);
    painter.rect_filled(line, CornerRadius::ZERO, theme.color.line);
}

/// The current cell's 2 px ring, inside its edges, while the grid has focus.
fn ring(painter: &Painter, theme: &Theme, cell: Rect) {
    let stroke = Stroke::new(theme.focus.width, theme.color.accent);
    painter.rect_stroke(cell, CornerRadius::ZERO, stroke, StrokeKind::Inside);
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Event, Key, Modifiers, Pos2, RawInput, Rect, Role, Shape};
    use marsa_core::types::{DbCell, DbColumnKind, DbPageColumn};

    use super::*;
    use crate::theme::LIGHT;
    use crate::widgets::harness::Harness;

    fn columns() -> Vec<DbPageColumn> {
        ["id", "email", "note"]
            .into_iter()
            .zip([DbColumnKind::Number, DbColumnKind::Text, DbColumnKind::Text])
            .map(|(name, kind)| DbPageColumn {
                name: name.into(),
                kind,
            })
            .collect()
    }

    fn rows(count: usize) -> Vec<Vec<DbCell>> {
        (0..count)
            .map(|index| {
                vec![
                    DbCell::Text(index.to_string()),
                    DbCell::Text(format!("user{index}@example.test")),
                    DbCell::Null,
                ]
            })
            .collect()
    }

    /// A grid of `count` rows that keeps its own current cell and sort,
    /// the way a screen does, and every event it returned.
    struct Live {
        harness: Harness,
        columns: Vec<DbPageColumn>,
        rows: Vec<Vec<DbCell>>,
        current: Current,
        events: Vec<GridEvent>,
        rect: Rect,
    }

    impl Live {
        fn new(count: usize) -> Self {
            Self {
                harness: Harness::new(egui::Theme::Light),
                columns: columns(),
                rows: rows(count),
                current: (None, 0),
                events: Vec::new(),
                rect: Rect::NOTHING,
            }
        }

        fn add(&mut self) -> impl FnMut(&mut Ui) + '_ {
            |ui: &mut Ui| {
                let widths = vec![100.0; self.columns.len()];
                let events = Grid::new(
                    "test_grid",
                    &self.columns,
                    &self.rows,
                    &widths,
                    self.current,
                )
                .sortable(None)
                .show(ui);
                self.rect = ui.min_rect();
                for event in events {
                    if let GridEvent::Current { row, column } = event {
                        self.current = (row, column);
                    }
                    self.events.push(event);
                }
            }
        }

        fn frame(&mut self, input: RawInput) {
            let harness = Harness {
                ctx: self.harness.ctx.clone(),
            };
            harness.frame(input, self.add());
        }

        /// A click on the first cell of the first row: the grid takes focus.
        fn focus(&mut self) {
            self.frame(RawInput::default());
            let theme = &LIGHT;
            let at = self.rect.min + vec2(theme.space.s3 + 10.0, theme.size.table_header + 10.0);
            let harness = Harness {
                ctx: self.harness.ctx.clone(),
            };
            harness.click(at, self.add());
            self.frame(RawInput::default());
            self.events.clear();
        }

        fn key(&mut self, key: Key, modifiers: Modifiers) {
            let harness = Harness {
                ctx: self.harness.ctx.clone(),
            };
            harness.key_with(key, modifiers, self.add());
        }
    }

    #[test]
    fn a_click_takes_the_focus_and_the_arrows_home_and_end_move_the_current_cell() {
        let mut live = Live::new(50);
        live.focus();
        assert_eq!(live.current, (Some(0), 0));
        live.key(Key::ArrowRight, Modifiers::NONE);
        assert_eq!(live.current, (Some(0), 1));
        live.key(Key::ArrowDown, Modifiers::NONE);
        assert_eq!(live.current, (Some(1), 1));
        live.key(Key::End, Modifiers::NONE);
        assert_eq!(live.current, (Some(1), 2));
        live.key(Key::ArrowRight, Modifiers::NONE);
        assert_eq!(live.current, (Some(1), 2), "stops at the last column");
        live.key(Key::Home, Modifiers::NONE);
        assert_eq!(live.current, (Some(1), 0));
        live.key(Key::End, Modifiers::COMMAND);
        assert_eq!(live.current, (Some(49), 0));
        live.key(Key::ArrowDown, Modifiers::NONE);
        assert_eq!(live.current, (Some(49), 0), "stops at the last row");
        live.key(Key::Home, Modifiers::COMMAND);
        assert_eq!(live.current, (Some(0), 0));
    }

    #[test]
    fn up_from_the_first_row_is_the_header_where_enter_sorts() {
        let mut live = Live::new(5);
        live.focus();
        live.key(Key::ArrowRight, Modifiers::NONE);
        live.key(Key::ArrowUp, Modifiers::NONE);
        assert_eq!(live.current, (None, 1));
        live.events.clear();
        live.key(Key::Enter, Modifiers::NONE);
        assert_eq!(live.events, [GridEvent::Sort(1)]);
        live.key(Key::ArrowDown, Modifiers::NONE);
        assert_eq!(live.current, (Some(0), 1));
    }

    #[test]
    fn enter_and_space_open_the_pane_esc_closes_it_and_alt_arrows_page() {
        let mut live = Live::new(5);
        live.focus();
        live.key(Key::Enter, Modifiers::NONE);
        live.key(Key::Space, Modifiers::NONE);
        live.key(Key::Escape, Modifiers::NONE);
        live.key(Key::ArrowRight, Modifiers::ALT);
        live.key(Key::ArrowLeft, Modifiers::ALT);
        assert_eq!(
            live.events,
            [
                GridEvent::TogglePane,
                GridEvent::TogglePane,
                GridEvent::ClosePane,
                GridEvent::NextPage,
                GridEvent::PreviousPage,
            ]
        );
        assert_eq!(
            live.current,
            (Some(0), 0),
            "Alt with an arrow moves no cell"
        );
    }

    #[test]
    fn copy_on_a_cell_is_reported() {
        let mut live = Live::new(5);
        live.focus();
        live.frame(RawInput {
            events: vec![Event::Copy],
            ..RawInput::default()
        });
        assert_eq!(live.events, [GridEvent::Copy]);
    }

    #[test]
    fn without_focus_the_keys_do_nothing() {
        let mut live = Live::new(5);
        live.frame(RawInput::default());
        live.key(Key::ArrowDown, Modifiers::NONE);
        live.key(Key::Enter, Modifiers::NONE);
        assert!(live.events.is_empty());
    }

    #[test]
    fn a_page_of_ten_thousand_rows_lays_out_only_the_rows_in_view() {
        let harness = Harness::new(egui::Theme::Light);
        let columns = columns();
        let widths = vec![100.0; columns.len()];
        // A window 600 high holds about 17 rows of 32.
        let count = |rows: &[Vec<DbCell>]| {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
                ..RawInput::default()
            };
            harness
                .clipped_shapes(input, |ui| {
                    Grid::new("big", &columns, rows, &widths, (None, 0)).show(ui);
                })
                .iter()
                .filter(|(_, shape)| matches!(shape, Shape::Text(_)))
                .count()
        };
        let small = count(&rows(40));
        let big = count(&rows(10_000));
        assert_eq!(
            small, big,
            "text drawn: {small} for 40 rows, {big} for 10,000"
        );
        assert!(small < 3 * 25, "only the rows in view: {small}");
    }

    #[test]
    fn the_grid_is_one_stop_named_for_a_screen_reader() {
        let harness = Harness::new(egui::Theme::Light);
        let columns = columns();
        let rows = rows(3);
        let widths = vec![100.0; columns.len()];
        let tree = harness.accessibility(|ui| {
            Grid::new("named", &columns, &rows, &widths, (None, 0)).show(ui);
        });
        let node = tree
            .node_with_label("Grid, 3 rows, 3 columns")
            .expect("grid");
        assert_eq!(node.role(), Role::Grid);
    }

    #[test]
    fn widths_follow_the_text_within_the_tokens() {
        let harness = Harness::new(egui::Theme::Light);
        let columns = columns();
        let mut rows = rows(3);
        rows[0][1] = DbCell::Text("x".repeat(400));
        let mut widths = Vec::new();
        harness.run(|ui| widths = column_widths(ui, &columns, &rows));
        assert_eq!(widths.len(), 3);
        assert_eq!(widths[1], LIGHT.size.grid_column_max);
        assert!(widths.iter().all(|w| *w >= LIGHT.size.grid_column_min));
    }

    #[test]
    fn null_the_text_null_and_an_empty_text_draw_three_ways() {
        let harness = Harness::new(egui::Theme::Light);
        let columns = vec![DbPageColumn {
            name: "v".into(),
            kind: DbColumnKind::Text,
        }];
        let widths = vec![100.0];
        // The texts and the framed rectangles one cell draws.
        let drawn = |cell: DbCell| {
            let rows = vec![vec![cell]];
            let shapes = harness.shapes(|ui| {
                Grid::new("three", &columns, &rows, &widths, (None, 0)).show(ui);
            });
            let texts: Vec<String> = shapes
                .iter()
                .filter_map(|shape| match shape {
                    Shape::Text(text) => Some(text.galley.text().to_owned()),
                    _ => None,
                })
                .filter(|text| text != "v")
                .collect();
            let frames = shapes
                .iter()
                .filter(|shape| matches!(shape, Shape::Rect(rect) if rect.stroke.width > 0.0))
                .count();
            (texts, frames)
        };
        let null = drawn(DbCell::Null);
        let text = drawn(DbCell::Text("NULL".into()));
        let empty = drawn(DbCell::Text(String::new()));
        assert_eq!(null, (vec!["NULL".to_owned()], 1), "a framed tag");
        assert_eq!(text, (vec!["NULL".to_owned()], 0), "plain text");
        assert_eq!(empty.1, 0);
        assert!(empty.0.iter().all(String::is_empty), "nothing to read");
    }

    #[test]
    fn a_binary_value_shows_only_its_size() {
        assert_eq!(strings::binary_value(512), "(binary, 512 B)");
        assert_eq!(strings::binary_value(14_200), "(binary, 14.2 KB)");
        assert_eq!(strings::binary_value(3_400_000), "(binary, 3.4 MB)");
    }
}
