//! The three tabs. egui is immediate mode: this code runs on every frame and
//! describes the whole window again from the data, like a Blade view that is
//! rendered again on every request. Only the visible rows are built.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use eframe::egui;
use egui::text::{CCursor, CCursorRange, LayoutJob, LayoutSection};
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

/// Bench requests for the SQL editor (`sql-typing` phase).
#[derive(Default)]
pub struct Typing {
    /// Put the cursor at the start of this line (1-based) and focus the editor.
    pub place_cursor_at_line: Option<usize>,
    /// Type this character on this frame, as a real key press would.
    pub char_this_frame: Option<char>,
}

pub struct View {
    pub tab: Tab,
    /// Loaded when the SQL tab opens for the first time.
    pub sql: Option<String>,
    pub table: Scroll,
    pub logs: Scroll,
    pub typing: Typing,
    highlighter: LineHighlighter,
    renderer: &'static str,
}

impl View {
    pub fn new(renderer: &'static str) -> Self {
        Self {
            tab: Tab::Table,
            sql: None,
            table: Scroll::default(),
            logs: Scroll::default(),
            typing: Typing::default(),
            highlighter: LineHighlighter::default(),
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
                sql_tab(ui, sql, &mut self.highlighter, &mut self.typing);
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

/// The `TextEdit` id, so the bench can place the cursor.
const SQL_EDITOR_ID: &str = "sql-editor";

/// 5,000 lines in one `TextEdit`, highlighted line by line.
///
/// S-09 used `highlight` on the whole text. It memoizes the result, but the
/// text changes with every key press, so syntect highlighted all 5,000 lines
/// again on every character. `LineHighlighter` keeps one result per line, so
/// a key press highlights only the line that changed. epaint already caches
/// the layout of each paragraph (line), and the tessellator skips rows outside
/// the view, so the unchanged lines cost almost nothing.
fn sql_tab(
    ui: &mut egui::Ui,
    sql: &mut String,
    highlighter: &mut LineHighlighter,
    typing: &mut Typing,
) {
    let id = egui::Id::unique(SQL_EDITOR_ID);
    if let Some(line) = typing.place_cursor_at_line.take() {
        let char_index = line_start_char_index(sql, line);
        let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(char_index))));
        state.store(ui.ctx(), id);
        ui.memory_mut(|memory| memory.request_focus(id));
    }
    if let Some(character) = typing.char_this_frame.take() {
        // Goes through the same path as a real key press: `TextEdit` reads
        // the event, inserts at the cursor, and scrolls to it.
        ui.ctx()
            .input_mut(|input| input.events.push(egui::Event::Text(character.to_string())));
    }

    let theme = CodeTheme::from_style(ui.style());
    let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, _wrap_width: f32| {
        highlighter.galley(ui, &theme, text.as_str())
    };
    egui::ScrollArea::both()
        .id_salt("sql")
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(sql)
                    .id(id)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .layouter(&mut layouter),
            );
        });
}

/// Char index of the start of `line` (1-based); the end of the text if the
/// text is shorter.
fn line_start_char_index(text: &str, line: usize) -> usize {
    let mut chars = 0;
    for (number, content) in text.split('\n').enumerate() {
        if number + 1 == line {
            return chars;
        }
        chars += content.chars().count() + 1;
    }
    text.chars().count()
}

/// Syntax highlighting cached per line, plus the last laid-out text.
#[derive(Default)]
struct LineHighlighter {
    /// Highlighted line, keyed by the line's text. Rebuilt on every change
    /// with only the lines that are still in the text.
    lines: HashMap<String, LayoutJob>,
    /// The galley of the last text, keyed by a hash of the text and the
    /// pixels per point. `TextEdit` calls the layouter on every frame.
    last: Option<(u64, Arc<egui::Galley>)>,
}

impl LineHighlighter {
    fn galley(&mut self, ui: &egui::Ui, theme: &CodeTheme, text: &str) -> Arc<egui::Galley> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        ui.ctx().pixels_per_point().to_bits().hash(&mut hasher);
        let key = hasher.finish();
        if let Some((last_key, galley)) = &self.last
            && *last_key == key
        {
            return Arc::clone(galley);
        }
        let job = self.job(ui, theme, text);
        let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
        self.last = Some((key, Arc::clone(&galley)));
        galley
    }

    /// One `LayoutJob` for the whole text. Only lines not seen before go
    /// through syntect.
    ///
    /// Limit: each line is highlighted on its own, so a comment that spans
    /// several lines (`/* ... */`) would be colored wrong. The S-09 SQL text
    /// has none; a real editor keeps syntect's parse state per line.
    fn job(&mut self, ui: &egui::Ui, theme: &CodeTheme, text: &str) -> LayoutJob {
        let newline = egui::TextFormat::simple(
            egui::TextStyle::Monospace.resolve(ui.style()),
            ui.visuals().text_color(),
        );
        let mut job = LayoutJob {
            text: String::with_capacity(text.len()),
            ..Default::default()
        };
        let mut next: HashMap<String, LayoutJob> = HashMap::with_capacity(self.lines.len() + 1);
        for (number, line) in text.split('\n').enumerate() {
            if number > 0 {
                job.append("\n", 0.0, newline.clone());
            }
            if !next.contains_key(line) {
                let (key, line_job) = match self.lines.remove_entry(line) {
                    Some(entry) => entry,
                    None => (
                        line.to_owned(),
                        highlight(ui.ctx(), ui.style(), theme, line, "sql"),
                    ),
                };
                next.insert(key, line_job);
            }
            let Some(line_job) = next.get(line) else {
                continue;
            };
            let offset = job.text.len();
            job.text.push_str(&line_job.text);
            job.sections
                .extend(line_job.sections.iter().map(|section| LayoutSection {
                    leading_space: section.leading_space,
                    byte_range: section.byte_range.start + offset..section.byte_range.end + offset,
                    format: section.format.clone(),
                }));
        }
        self.lines = next;
        // No line wrapping, like CodeMirror's default.
        job.wrap.max_width = f32::INFINITY;
        job
    }
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
