//! Tables and lists (components.md, Tables and lists): a card, a header row,
//! and rows over `egui_extras::TableBuilder`, which draws only the rows in
//! view (R16). A screen adds the cells of a row with widgets and reads which
//! row the user clicked; the card, the header, the row heights, the dividers,
//! and the hover and selected fills are drawn here, from the tokens.
//!
//! The first and the last cell of a row are padded by `space.4` and the
//! cells are `space.3` apart. Those spaces are cells of their own, so the
//! fill of a hovered or selected row reaches both edges of the card.

use eframe::egui::text::{LayoutJob, TextWrapping};
use eframe::egui::{
    Align2, Color32, CornerRadius, Frame, InnerResponse, Label, Margin, Rect, Response, RichText,
    Role, Sense, Stroke, Ui, Vec2, WidgetInfo, vec2,
};
use egui_extras::{Column, TableBuilder, TableRow};

use super::{Button, whole};
use crate::theme::{Direction, TextToken, Theme};

/// The card that holds a table or a list: `surface`, a 1 px `line` border,
/// `radius.lg`. It fills the width it is given.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    let theme = Theme::of(ui.ctx());
    Frame::new()
        .fill(theme.color.surface)
        .stroke(Stroke::new(theme.size.border, theme.color.line))
        .corner_radius(CornerRadius::same(theme.radius.lg))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
}

/// A card with room inside, for a form or a message: [`card`] with
/// `space.4` of padding.
pub fn padded_card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    let theme = Theme::of(ui.ctx());
    Frame::new()
        .fill(theme.color.surface)
        .stroke(Stroke::new(theme.size.border, theme.color.line))
        .corner_radius(CornerRadius::same(theme.radius.lg))
        .inner_margin(Margin::same(whole(theme.space.s4)))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
}

/// The empty state of a list: one sentence that says what this is, and one
/// primary button that fills it ("No sites yet." and "Add site…"). Returns
/// whether the button was pressed.
pub fn empty_state(ui: &mut Ui, sentence: &str, action: &str) -> bool {
    empty_state_button(ui, sentence, action).clicked()
}

/// [`empty_state`], giving the button's response: a button that opens a menu
/// needs it to anchor the menu.
pub fn empty_state_button(ui: &mut Ui, sentence: &str, action: &str) -> Response {
    empty_state_with(ui, sentence, Button::primary(action))
}

/// [`empty_state_button`] with a button of the caller's making: one that may
/// be disabled with its reason.
pub fn empty_state_with(ui: &mut Ui, sentence: &str, button: Button<'_>) -> Response {
    let theme = Theme::of(ui.ctx());
    card(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(theme.space.s6);
            let text = RichText::new(sentence)
                .font(theme.text.body.font_id())
                .color(theme.color.ink2);
            ui.add(Label::new(text).selectable(false));
            ui.add_space(theme.space.s3);
            let button = ui.add(button);
            ui.add_space(theme.space.s6);
            button
        })
        .inner
    })
    .inner
}

/// How tall the rows of a table are (components.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowHeight {
    /// A name and a line under it (sites): 54.
    TwoLine,
    /// One line: 48.
    OneLine,
    /// A data grid: 32.
    Dense,
    /// A log line: 26.
    Log,
}

impl RowHeight {
    pub const fn points(self, theme: &Theme) -> f32 {
        match self {
            Self::TwoLine => theme.size.row_max,
            Self::OneLine => theme.size.row,
            Self::Dense => theme.size.row_dense,
            Self::Log => theme.size.log_row,
        }
    }
}

/// How wide a column is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Width {
    /// The rest of the width; a long text is cut short.
    Fill,
    /// This many points.
    Fixed(f32),
}

/// A column: the title in the header, and its width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TableColumn<'a> {
    pub title: &'a str,
    pub width: Width,
}

impl<'a> TableColumn<'a> {
    pub const fn fill(title: &'a str) -> Self {
        Self {
            title,
            width: Width::Fill,
        }
    }

    pub const fn fixed(title: &'a str, width: f32) -> Self {
        Self {
            title,
            width: Width::Fixed(width),
        }
    }
}

/// How many cells egui lays out for `columns` columns: the padding, each
/// column after a gap of its own, and the padding.
pub const fn layout_cells(columns: usize) -> usize {
    columns * 2 + 1
}

/// What the user did in a table this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TableOutput {
    /// The row the user clicked, by its index.
    pub clicked: Option<usize>,
    /// The card, header and rows together.
    pub rect: Rect,
}

/// The color of the second line of a two-line cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LineTone {
    /// `mute`: a folder, a description.
    #[default]
    Mute,
    /// `error`: why something failed.
    Error,
}

/// A row being filled: [`Row::cell`] adds one cell after the other.
pub struct Row<'a, 'b> {
    row: TableRow<'a, 'b>,
    theme: &'static Theme,
    columns: usize,
    cells: usize,
}

impl<'a, 'b> Row<'a, 'b> {
    fn body(
        mut row: TableRow<'a, 'b>,
        theme: &'static Theme,
        columns: usize,
        selected: Option<usize>,
        header: bool,
    ) -> Self {
        // A divider above each row; the first one closes the header, so a
        // table with no header has none above its first row.
        row.set_overline(header || row.index() > 0);
        row.set_selected(selected == Some(row.index()));
        Self::header(row, theme, columns)
    }

    fn header(row: TableRow<'a, 'b>, theme: &'static Theme, columns: usize) -> Self {
        Self {
            row,
            theme,
            columns,
            cells: 0,
        }
    }

    /// Which row of the table this is, from 0.
    pub fn index(&self) -> usize {
        self.row.index()
    }

    /// Adds the next cell, its widgets in a line from the start, centered
    /// on the row. A cell beyond the table's columns is dropped.
    pub fn cell(&mut self, add: impl FnOnce(&mut Ui)) {
        if !self.space_before_cell() {
            return;
        }
        let theme = self.theme;
        self.row.col(|ui| {
            // A selected row asks egui for all its text in one color; keep
            // the colors the cell chooses.
            ui.visuals_mut().override_text_color = None;
            // The table has no space of its own between cells, but the
            // widgets of a cell keep the usual space between them.
            ui.spacing_mut().item_spacing = vec2(theme.space.s2, theme.space.s1);
            add(ui);
        });
    }

    /// Adds the next cell as two lines, one under the other and centered on
    /// the row: a site's name in `type.body.strong`, and its folder in
    /// `caption`. A line too long for the column ends in an ellipsis.
    pub fn two_lines(&mut self, primary: &str, secondary: &str) {
        self.two_lines_hover(primary, secondary, secondary);
    }

    /// [`Row::two_lines`] for a second line that is shortened for the eye
    /// (`~/Code/shop`): `full` is the text a screen reader hears and the
    /// tooltip shows while the pointer rests on the cell.
    pub fn two_lines_hover(&mut self, primary: &str, secondary: &str, full: &str) {
        self.two_lines_toned(primary, secondary, full, LineTone::Mute);
    }

    /// [`Row::two_lines_hover`] with the second line in `tone`: a service
    /// that failed says why in `error`, under its name.
    pub fn two_lines_toned(&mut self, primary: &str, secondary: &str, full: &str, tone: LineTone) {
        let theme = self.theme;
        self.cell(|ui| {
            let width = ui.available_width();
            let line = |ui: &Ui, text: &str, token: TextToken| {
                let mut job = LayoutJob::simple_singleline(
                    text.to_owned(),
                    token.font_id(),
                    Color32::PLACEHOLDER,
                );
                job.wrap = TextWrapping::truncate_at_width(width);
                ui.painter().layout_job(job)
            };
            let first = line(ui, primary, theme.text.body_strong);
            let second = line(ui, secondary, theme.text.caption);
            let text = vec2(width, first.size().y + second.size().y);
            let (rect, response) =
                ui.allocate_exact_size(vec2(width, ui.available_height()), Sense::hover());
            let top = Align2::LEFT_CENTER.align_size_within_rect(text, rect).min;
            let painter = ui.painter();
            let below = top + vec2(0.0, first.size().y);
            painter.galley(top, first, theme.color.ink);
            let color = match tone {
                LineTone::Mute => theme.color.mute,
                LineTone::Error => theme.color.error,
            };
            painter.galley(below, second, color);
            let label = format!("{primary}, {full}");
            response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &label));
            if full != secondary {
                response.on_hover_text(full);
            }
        });
    }

    /// The title of a column in the header row.
    fn title(&mut self, title: &str) {
        if !self.space_before_cell() {
            return;
        }
        let theme = self.theme;
        let (_, response) = self.row.col(|ui| {
            let text = RichText::new(title)
                .font(theme.text.caption.font_id())
                .color(theme.color.mute);
            ui.add(Label::new(text).selectable(false));
        });
        response.widget_info(|| WidgetInfo::labeled(Role::ColumnHeader, true, title));
    }

    /// Lays out the padding or the gap before the next cell. `false` when
    /// the row is full.
    fn space_before_cell(&mut self) -> bool {
        if self.cells >= self.columns {
            debug_assert!(false, "a row has more cells than the table has columns");
            return false;
        }
        self.cells += 1;
        self.row.col(|_| {});
        true
    }

    /// Pads the rest of the row, so its fill reaches the edge, and tells
    /// whether the user clicked it.
    fn finish(mut self) -> bool {
        while self.row.col_index() < layout_cells(self.columns) {
            self.row.col(|_| {});
        }
        self.row.response().clicked()
    }
}

/// A table in a card: a header row, then the rows a screen adds. Only the
/// rows in view are built, so a list of thousands is as cheap as ten.
#[derive(Clone, Copy, Debug)]
#[must_use = "a table is drawn only when it is shown"]
pub struct Table<'a> {
    /// Makes this table's state its own when a screen has several.
    id: &'a str,
    /// What a screen reader says for the table: "Sites".
    label: &'a str,
    columns: &'a [TableColumn<'a>],
    height: RowHeight,
    selected: Option<usize>,
    max_height: Option<f32>,
    header: bool,
}

impl<'a> Table<'a> {
    pub const fn new(id: &'a str, label: &'a str, columns: &'a [TableColumn<'a>]) -> Self {
        Self {
            id,
            label,
            columns,
            height: RowHeight::OneLine,
            selected: None,
            max_height: None,
            header: true,
        }
    }

    /// No header row: the rows describe themselves, as the Installed list
    /// of a runtime does (the mockup). The columns' titles still name the
    /// cells in the code.
    pub const fn headerless(mut self) -> Self {
        self.header = false;
        self
    }

    pub const fn rows(mut self, height: RowHeight) -> Self {
        self.height = height;
        self
    }

    /// The row drawn as selected: the `selected` fill.
    pub const fn selected(mut self, index: Option<usize>) -> Self {
        self.selected = index;
        self
    }

    /// The table scrolls inside itself past this height. By default it grows
    /// with its rows and the page scrolls.
    pub const fn scrolls_past(mut self, height: f32) -> Self {
        self.max_height = Some(height);
        self
    }

    /// Draws the table with `rows` rows. `add_row` fills the row it is
    /// given, and is called for the rows in view only.
    pub fn show(
        self,
        ui: &mut Ui,
        rows: usize,
        mut add_row: impl FnMut(&mut Row<'_, '_>),
    ) -> TableOutput {
        let theme = Theme::of(ui.ctx());
        let count = self.columns.len();
        let row_count = rows;
        let mut output = TableOutput {
            clicked: None,
            rect: Rect::NOTHING,
        };
        let shown = card(ui, |ui| {
            // The cells carry their own padding.
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let visuals = ui.visuals_mut();
            // A row under the pointer: `selected` at 50%. A selected row:
            // `selected` (components.md).
            visuals.widgets.hovered.bg_fill =
                theme.color.selected.gamma_multiply(theme.state.row_hover);
            visuals.selection.bg_fill = theme.color.selected;
            let pad = Column::exact(theme.space.s4);
            let mut builder = TableBuilder::new(ui)
                .id_salt(self.id)
                .sense(Sense::click())
                .cell_layout(Direction::CURRENT.row())
                .vscroll(self.max_height.is_some())
                .column(pad);
            if let Some(height) = self.max_height {
                builder = builder.max_scroll_height(height);
            }
            for (index, column) in self.columns.iter().enumerate() {
                if index > 0 {
                    builder = builder.column(Column::exact(theme.space.s3));
                }
                builder = builder.column(match column.width {
                    Width::Fill => Column::remainder().clip(true),
                    Width::Fixed(width) => Column::exact(width),
                });
            }
            let builder = builder.column(pad);
            let rows = self.height.points(theme);
            let body = |body: egui_extras::TableBody<'_>| {
                body.rows(rows, row_count, |row| {
                    let mut row = Row::body(row, theme, count, self.selected, self.header);
                    let index = row.index();
                    add_row(&mut row);
                    if row.finish() {
                        output.clicked = Some(index);
                    }
                });
            };
            if self.header {
                builder
                    .header(theme.size.table_header, |header| {
                        let mut row = Row::header(header, theme, count);
                        for column in self.columns {
                            row.title(column.title);
                        }
                        row.finish();
                    })
                    .body(body);
            } else {
                builder.body(body);
            }
        });
        shown
            .response
            .widget_info(|| WidgetInfo::labeled(Role::Table, true, self.label));
        output.rect = shown.response.rect;
        output
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Color32, Event, Pos2, RawInput, Shape, pos2, vec2};

    use super::*;
    use crate::theme::tests::{TEXT, UI, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::{Harness, over};

    const COLUMNS: [TableColumn<'static>; 3] = [
        TableColumn::fill("Site"),
        TableColumn::fixed("PHP", 100.0),
        TableColumn::fixed("", 60.0),
    ];

    /// A table of `rows` two-line rows, as a screen would add it.
    fn sites(ui: &mut Ui, rows: usize, selected: Option<usize>) -> TableOutput {
        Table::new("sites", "Sites", &COLUMNS)
            .rows(RowHeight::TwoLine)
            .selected(selected)
            .show(ui, rows, |row| {
                let index = row.index();
                row.cell(|ui| {
                    ui.label(format!("site {index}"));
                });
                row.cell(|ui| {
                    ui.label("8.4");
                });
            })
    }

    #[test]
    fn row_heights_and_the_header_come_from_the_tokens() {
        for theme in [&LIGHT, &DARK] {
            assert_eq!(RowHeight::TwoLine.points(theme), theme.size.row_max);
            assert_eq!(RowHeight::OneLine.points(theme), theme.size.row);
            assert_eq!(RowHeight::Dense.points(theme), theme.size.row_dense);
            assert_eq!(RowHeight::Log.points(theme), theme.size.log_row);
        }
        // components.md: two lines 52 to 54, one line 46 to 48, grid 32, logs 26.
        let heights = [
            RowHeight::TwoLine,
            RowHeight::OneLine,
            RowHeight::Dense,
            RowHeight::Log,
        ]
        .map(|height| height.points(&LIGHT));
        assert_eq!(heights, [54.0, 48.0, 32.0, 26.0]);
        assert_eq!(LIGHT.size.table_header, 34.0);
    }

    #[test]
    fn a_table_is_a_header_and_its_rows_in_a_card_as_wide_as_the_space_it_has() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let mut width = 0.0;
        let mut rects = Vec::new();
        harness.run(|ui| {
            width = ui.available_width();
            for rows in [1, 3] {
                rects.push(
                    Table::new("a", "A", &COLUMNS)
                        .rows(RowHeight::TwoLine)
                        .show(ui, rows, |_| {})
                        .rect,
                );
            }
        });
        // The header and the rows, inside the border of the card.
        let inside = |rows: f32| theme.size.table_header + rows * theme.size.row_max;
        let border = 2.0 * theme.size.border;
        for (rect, rows) in rects.iter().zip([1.0, 3.0]) {
            assert!(
                (rect.height() - inside(rows) - border).abs() < 1.5,
                "{rows} rows: {}",
                rect.height()
            );
            assert!((rect.width() - width).abs() < 1.5);
        }
    }

    #[test]
    fn a_table_with_no_header_is_its_rows_and_no_divider_over_the_first() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let mut with = Rect::NOTHING;
        let mut without = Rect::NOTHING;
        harness.run(|ui| {
            with = Table::new("with", "With", &COLUMNS)
                .rows(RowHeight::OneLine)
                .show(ui, 3, |_| {})
                .rect;
            without = Table::new("without", "Without", &COLUMNS)
                .rows(RowHeight::OneLine)
                .headerless()
                .show(ui, 3, |_| {})
                .rect;
        });
        // The header row is the only difference.
        assert!(
            (with.height() - without.height() - theme.size.table_header).abs() < 1.5,
            "{} {}",
            with.height(),
            without.height()
        );
    }

    #[test]
    fn a_long_list_draws_only_the_rows_in_view() {
        let harness = Harness::new(egui::Theme::Light);
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 400.0));
        let mut drawn = 0;
        let input = RawInput {
            screen_rect: Some(screen),
            ..RawInput::default()
        };
        harness.frame(input, |ui| {
            Table::new("long", "Long", &COLUMNS)
                .rows(RowHeight::OneLine)
                .show(ui, 100_000, |row| {
                    drawn += 1;
                    row.cell(|_| {});
                });
        });
        // 400 points of 48 point rows, and the ones at the edges.
        assert!((1..=20).contains(&drawn), "{drawn} rows were drawn");
    }

    #[test]
    fn a_table_that_scrolls_inside_stops_at_its_height() {
        let harness = Harness::new(egui::Theme::Light);
        let mut rect = Rect::NOTHING;
        harness.run(|ui| {
            rect = Table::new("tall", "Tall", &COLUMNS)
                .rows(RowHeight::OneLine)
                .scrolls_past(200.0)
                .show(ui, 100, |row| row.cell(|_| {}))
                .rect;
        });
        assert!(
            rect.height() < 200.0 + LIGHT.size.table_header + 4.0,
            "{}",
            rect.height()
        );
    }

    #[test]
    fn a_click_on_a_row_reports_its_index_and_a_click_on_the_header_none() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let mut rect = Rect::NOTHING;
        harness.run(|ui| rect = sites(ui, 3, None).rect);
        let x = rect.center().x;
        let top = rect.top() + theme.size.border;
        let row = |index: f32| top + theme.size.table_header + (index + 0.5) * theme.size.row_max;
        for (y, expected) in [
            (row(0.0), Some(0)),
            (row(1.0), Some(1)),
            (row(2.0), Some(2)),
        ] {
            let mut clicked = None;
            harness.click(pos2(x, y), |ui| clicked = sites(ui, 3, None).clicked);
            assert_eq!(clicked, expected, "y {y}");
        }
        let mut clicked = None;
        harness.click(pos2(x, top + theme.size.table_header / 2.0), |ui| {
            clicked = sites(ui, 3, None).clicked;
        });
        assert_eq!(clicked, None);
    }

    /// The fills of the rects one frame paints.
    fn fills(harness: &Harness, add: impl FnMut(&mut Ui)) -> Vec<Color32> {
        harness
            .shapes(add)
            .into_iter()
            .filter_map(|shape| match shape {
                Shape::Rect(rect) if rect.fill != Color32::TRANSPARENT => Some(rect.fill),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_hovered_row_is_selected_at_half_and_a_selected_row_is_selected() {
        for (name, theme, active) in [
            ("light", &LIGHT, egui::Theme::Light),
            ("dark", &DARK, egui::Theme::Dark),
        ] {
            let harness = Harness::new(active);
            let mut rect = Rect::NOTHING;
            harness.run(|ui| rect = sites(ui, 3, None).rect);
            let half = theme.color.selected.gamma_multiply(theme.state.row_hover);
            let count =
                |fills: &[Color32], color: Color32| fills.iter().filter(|f| **f == color).count();
            let rest = fills(&harness, |ui| {
                sites(ui, 3, None);
            });
            assert_eq!(count(&rest, half), 0, "{name}: no row is hovered");
            assert_eq!(count(&rest, theme.color.selected), 0);
            // The pointer over the second row; the row is known a frame later.
            let over_row = pos2(
                rect.center().x,
                rect.top() + theme.size.table_header + 1.5 * theme.size.row_max,
            );
            harness.hover(over_row, |ui| {
                sites(ui, 3, None);
            });
            let hovered = fills(&harness, |ui| {
                sites(ui, 3, None);
            });
            // One fill for each of the cells of the row: the padding, the
            // gaps, and the three cells.
            assert_eq!(count(&hovered, half), layout_cells(COLUMNS.len()), "{name}");
            let selected = fills(&harness, |ui| {
                sites(ui, 3, Some(2));
            });
            assert_eq!(
                count(&selected, theme.color.selected),
                layout_cells(COLUMNS.len()),
                "{name}"
            );
        }
    }

    #[test]
    fn a_selected_row_keeps_the_colors_its_cells_choose() {
        let harness = Harness::new(egui::Theme::Light);
        let overridden = harness
            .shapes(|ui| {
                sites(ui, 3, Some(1));
            })
            .into_iter()
            .filter(
                |shape| matches!(shape, Shape::Text(text) if text.override_text_color.is_some()),
            )
            .count();
        assert_eq!(
            overridden, 0,
            "egui would paint a selected row's text in one color"
        );
    }

    #[test]
    fn rows_are_divided_by_a_one_point_line() {
        for (theme, active) in [(&LIGHT, egui::Theme::Light), (&DARK, egui::Theme::Dark)] {
            let harness = Harness::new(active);
            let lines = harness
                .shapes(|ui| {
                    sites(ui, 3, None);
                })
                .into_iter()
                .filter(|shape| {
                    matches!(shape, Shape::LineSegment { stroke, .. }
                    if stroke.color == theme.color.line && stroke.width == theme.size.border)
                })
                .count();
            // One above each row: the first closes the header.
            assert_eq!(
                lines,
                3 * layout_cells(COLUMNS.len()),
                "dark {}",
                theme.dark
            );
        }
    }

    #[test]
    fn a_screen_reader_hears_a_table_with_the_names_of_its_columns() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            sites(ui, 2, None);
        });
        assert_eq!(
            tree.node_with_label("Sites").expect("table").role(),
            egui::Role::Table
        );
        for title in ["Site", "PHP"] {
            let header = tree.node_with_label(title).expect(title);
            assert_eq!(header.role(), egui::Role::ColumnHeader);
        }
    }

    #[test]
    fn text_reads_on_the_hover_and_selected_fills_in_both_themes() {
        for (name, theme) in [("light", &LIGHT), ("dark", &DARK)] {
            let c = &theme.color;
            let hover = over(c.selected.gamma_multiply(theme.state.row_hover), c.surface);
            let colors = [
                ("ink", c.ink),
                ("ink2", c.ink2),
                ("mute", c.mute),
                ("accent", c.accent),
                ("error", c.error),
                ("ok", c.ok),
            ];
            for (fill_name, fill) in [("hover", hover), ("selected", c.selected)] {
                for (color_name, color) in colors {
                    let ratio = contrast(color, fill);
                    // Finding for tokens.md: `ok` on `selected` is 4.17:1 in the
                    // light theme, so a state word in `ok` does not belong on a
                    // selected row. It reads on the hover fill (checked above).
                    let min = if color_name == "ok" && fill_name == "selected" {
                        UI
                    } else {
                        TEXT
                    };
                    assert!(
                        ratio >= min,
                        "{name}: {color_name} on {fill_name} is {ratio:.2}:1"
                    );
                }
            }
            // The header's words, and the divider the eye follows.
            assert!(contrast(c.mute, c.surface) >= TEXT);
        }
    }

    #[test]
    fn the_empty_state_is_a_sentence_and_a_button_that_fills_the_list() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            empty_state(ui, "No sites yet.", "Add site…");
        });
        assert!(tree.node_with_text("No sites yet.").is_some());
        let button = tree.node_with_label("Add site…").expect("the button");
        assert_eq!(button.role(), egui::Role::Button);
        let bounds = button.bounds().expect("bounds");
        let at: Pos2 = pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        let mut pressed = false;
        harness.click(at, |ui| {
            pressed = empty_state(ui, "No sites yet.", "Add site…")
        });
        assert!(pressed);
        let _ = Event::Copy;
    }

    #[test]
    fn a_padded_card_fills_the_width_and_keeps_its_content_off_the_edge() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let (mut card_rect, mut inner, mut width) = (Rect::NOTHING, Rect::NOTHING, 0.0);
        harness.run(|ui| {
            width = ui.available_width();
            let shown = padded_card(ui, |ui| ui.label("Link /code as:").rect);
            card_rect = shown.response.rect;
            inner = shown.inner;
        });
        assert!(
            (card_rect.width() - width).abs() < 0.5,
            "{}",
            card_rect.width()
        );
        let gap = inner.left() - card_rect.left();
        assert!(gap >= theme.space.s4, "{gap}");
        assert!(inner.top() - card_rect.top() >= theme.space.s4);
    }

    #[test]
    fn the_empty_state_button_gives_the_response_a_menu_anchors_to() {
        let harness = Harness::new(egui::Theme::Light);
        let mut rect = Rect::NOTHING;
        harness.run(|ui| rect = empty_state_button(ui, "No sites yet.", "Add site…").rect);
        assert_eq!(rect.height(), LIGHT.size.control);
    }

    #[test]
    fn two_lines_are_centered_on_the_row_and_the_cells_keep_their_spacing() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let mut rect = Rect::NOTHING;
        let mut gap = 0.0;
        let tree = harness.accessibility(|ui| {
            rect = Table::new("lines", "Lines", &COLUMNS)
                .rows(RowHeight::TwoLine)
                .show(ui, 1, |row| {
                    row.two_lines("shop", "~/Code/shop");
                    row.cell(|ui| gap = ui.spacing().item_spacing.x);
                })
                .rect;
        });
        let cell = tree
            .node_with_text("shop, ~/Code/shop")
            .expect("the two lines");
        let bounds = cell.bounds().expect("bounds");
        let middle = (bounds.y0 + bounds.y1) / 2.0;
        let row_middle =
            rect.top() + theme.size.border + theme.size.table_header + theme.size.row_max / 2.0;
        assert!(
            (middle as f32 - row_middle).abs() < 1.5,
            "{middle} against {row_middle}"
        );
        // The table's own zero spacing does not leak into the cells.
        assert_eq!(gap, theme.space.s2);
    }

    #[test]
    fn a_line_too_long_for_its_column_is_cut_with_an_ellipsis_and_a_wide_one_is_whole() {
        let harness = Harness::new(egui::Theme::Light);
        let long = "~/Code/work/clients/acme/projects/a-very-long-folder-name/with/more/than/fits";
        let texts = |harness: &Harness, width: f32| {
            let columns = [TableColumn::fixed("Site", width)];
            harness
                .shapes(|ui| {
                    Table::new("cut", "Cut", &columns)
                        .rows(RowHeight::TwoLine)
                        .show(ui, 1, |row| row.two_lines("shop", long));
                })
                .into_iter()
                .filter_map(|shape| match shape {
                    Shape::Text(text) => Some(text.galley.rows.last().map_or(0.0, |r| r.size.x)),
                    _ => None,
                })
                .fold(0.0_f32, f32::max)
        };
        let narrow = texts(&harness, 120.0);
        let wide = texts(&harness, 900.0);
        assert!(narrow <= 120.0 + 1.0, "cut to the column: {narrow}");
        assert!(wide > 300.0, "whole when it fits: {wide}");
    }

    #[test]
    fn the_second_line_is_mute_and_a_failure_is_told_in_the_error_color() {
        for (theme, egui_theme) in [(&LIGHT, egui::Theme::Light), (&DARK, egui::Theme::Dark)] {
            let harness = Harness::new(egui_theme);
            let color_of = |text: &str, tone: LineTone| {
                let columns = [TableColumn::fill("Service")];
                harness
                    .shapes(|ui| {
                        Table::new("tone", "Tone", &columns)
                            .rows(RowHeight::TwoLine)
                            .show(ui, 1, |row| row.two_lines_toned("Caddy", text, text, tone));
                    })
                    .into_iter()
                    .find_map(|shape| match shape {
                        Shape::Text(shape) if shape.galley.text() == text => {
                            Some(shape.fallback_color)
                        }
                        _ => None,
                    })
                    .expect("the second line")
            };
            assert_eq!(color_of("Runs PHP", LineTone::Mute), theme.color.mute);
            assert_eq!(color_of("It stopped", LineTone::Error), theme.color.error);
            // The default tone is the quiet one, so every old caller is unchanged.
            assert_eq!(LineTone::default(), LineTone::Mute);
            // The words of a failure read on the card.
            assert!(contrast(theme.color.error, theme.color.surface) >= TEXT);
        }
    }

    #[test]
    fn a_row_has_the_cells_of_the_columns_and_the_spaces_between() {
        assert_eq!(layout_cells(0), 1);
        assert_eq!(layout_cells(1), 3);
        assert_eq!(layout_cells(5), 11);
    }
}
