//! The three tabs. egui is immediate mode: this code runs on every frame and
//! describes the whole window again from the data, like a Blade view that is
//! rendered again on every request. Only the visible rows are built.

use eframe::egui;
use egui_extras::syntax_highlighting::{CodeTheme, highlight};
use egui_extras::{Column, TableBuilder};
use s09_common::data;

use crate::strings;

/// Table row height in points, the same as the Tauri table (20 px).
const TABLE_ROW_HEIGHT: f32 = 20.0;
const HEADER_HEIGHT: f32 = 22.0;
/// Initial column widths; the last column takes the rest.
const COLUMN_WIDTHS: [f32; 7] = [70.0, 140.0, 230.0, 100.0, 80.0, 90.0, 150.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Table,
    Sql,
    Logs,
}

/// A scroll area the bench can drive.
#[derive(Default)]
pub struct Scroll {
    /// Offset to apply on the next frame (bench only).
    pub force: Option<f32>,
    /// Largest offset, learned from the previous frame's layout.
    pub max: f32,
}

pub struct View {
    pub tab: Tab,
    /// Loaded when the SQL tab opens for the first time.
    pub sql: Option<String>,
    pub table: Scroll,
    pub logs: Scroll,
    renderer: &'static str,
}

impl View {
    pub fn new(renderer: &'static str) -> Self {
        Self {
            tab: Tab::Table,
            sql: None,
            table: Scroll::default(),
            logs: Scroll::default(),
            renderer,
        }
    }

    pub fn scroll_mut(&mut self, tab: Tab) -> &mut Scroll {
        match tab {
            Tab::Logs => &mut self.logs,
            Tab::Table | Tab::Sql => &mut self.table,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Table, strings::TAB_TABLE);
                ui.selectable_value(&mut self.tab, Tab::Sql, strings::TAB_SQL);
                ui.selectable_value(&mut self.tab, Tab::Logs, strings::TAB_LOGS);
                ui.separator();
                ui.label(format!("{} {}", strings::RENDERER_LABEL, self.renderer));
            });
        });
        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Table => table_tab(ui, &mut self.table),
            Tab::Sql => {
                let sql = self.sql.get_or_insert_with(data::sql_text);
                sql_tab(ui, sql);
            }
            Tab::Logs => logs_tab(ui, &mut self.logs),
        });
    }
}

/// 100,000 rows. `TableBody::rows` asks only for the visible rows; each row
/// is generated from its index on the frame that shows it.
fn table_tab(ui: &mut egui::Ui, scroll: &mut Scroll) {
    let mut table = TableBuilder::new(ui)
        .id_salt("table")
        .striped(true)
        .resizable(true)
        .auto_shrink(false)
        .animate_scrolling(false)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
    for width in COLUMN_WIDTHS {
        table = table.column(Column::initial(width).at_least(40.0).clip(true));
    }
    table = table.column(Column::remainder().clip(true));
    if let Some(offset) = scroll.force.take() {
        table = table.vertical_scroll_offset(offset);
    }
    let output = table
        .header(HEADER_HEIGHT, |mut header| {
            for title in data::TABLE_COLUMNS {
                header.col(|ui| {
                    ui.strong(title);
                });
            }
        })
        .body(|body| {
            body.rows(TABLE_ROW_HEIGHT, data::TABLE_ROWS, |mut row| {
                for cell in data::table_row(row.index()) {
                    row.col(|ui| {
                        ui.add(egui::Label::new(cell).truncate());
                    });
                }
            });
        });
    scroll.max = (output.content_size.y - output.inner_rect.height()).max(0.0);
}

/// 5,000 lines in one `TextEdit`. `highlight` memoizes the syntect result, so
/// the text is highlighted again only when it changes.
fn sql_tab(ui: &mut egui::Ui, sql: &mut String) {
    let theme = CodeTheme::from_style(ui.style());
    let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, _wrap_width: f32| {
        let mut job = highlight(ui.ctx(), ui.style(), &theme, text.as_str(), "sql");
        // No line wrapping, like CodeMirror's default.
        job.wrap.max_width = f32::INFINITY;
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    };
    egui::ScrollArea::both()
        .id_salt("sql")
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(sql)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .layouter(&mut layouter),
            );
        });
}

/// 50,000 lines. `show_rows` lays out only the visible range.
fn logs_tab(ui: &mut egui::Ui, scroll: &mut Scroll) {
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    let mut area = egui::ScrollArea::vertical()
        .id_salt("logs")
        .auto_shrink(false)
        .animated(false);
    if let Some(offset) = scroll.force.take() {
        area = area.vertical_scroll_offset(offset);
    }
    let output = area.show_rows(ui, row_height, data::LOG_LINES, |ui, rows| {
        for index in rows {
            ui.add(
                egui::Label::new(egui::RichText::new(data::log_line(index)).monospace()).truncate(),
            );
        }
    });
    scroll.max = (output.content_size.y - output.inner_rect.height()).max(0.0);
}
