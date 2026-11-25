//! The Columns tab of an open table (layout.md, "Tables view, a table
//! chosen"): a table of Column, Type, Null, Default, and Key, one line each.

use eframe::egui::Ui;

use super::super::lines::message;
use crate::model::OpenTable;
use crate::strings;
use crate::theme::page::{DB_COLUMN_KEY, DB_COLUMN_NULL, DB_COLUMN_TYPE};
use crate::theme::{Section, Theme};
use crate::widgets::Text;
use crate::widgets::table::{RowHeight, Table, TableColumn};
use crate::widgets::tag::Tag;

pub(super) fn show(ui: &mut Ui, theme: &Theme, open: &OpenTable) {
    if let Some(code) = &open.error {
        message(ui, theme, &strings::error_text(code));
        return;
    }
    let Some(table) = &open.columns else {
        return;
    };
    let headers = [
        TableColumn::fill(strings::DB_COLUMN),
        TableColumn::fixed(strings::DB_TYPE, DB_COLUMN_TYPE),
        TableColumn::fixed(strings::DB_NULL, DB_COLUMN_NULL),
        TableColumn::fill(strings::DB_DEFAULT),
        TableColumn::fixed(strings::DB_KEY, DB_COLUMN_KEY),
    ];
    Table::new("db_columns", strings::DB_COLUMNS_LABEL, &headers)
        .rows(RowHeight::OneLine)
        .show(ui, table.columns.len(), |row| {
            let column = &table.columns[row.index()];
            row.cell(|ui| {
                ui.add(Text::new(&column.name, theme.text.body, theme.color.ink).truncate());
            });
            row.cell(|ui| {
                let text = Text::new(&column.type_name, theme.text.mono_sm, theme.color.ink2);
                ui.add(text.truncate());
            });
            row.cell(|ui| {
                let text = strings::yes_no(column.nullable);
                ui.add(Text::new(text, theme.text.body_sm, theme.color.ink2));
            });
            row.cell(|ui| match &column.default {
                Some(default) => {
                    let text = Text::new(default, theme.text.mono_sm, theme.color.ink2);
                    ui.add(text.truncate());
                }
                // No default: a column that takes NULL starts as NULL; one
                // that does not has no value to show.
                None if column.nullable => {
                    ui.add(Tag::new(strings::NULL_TAG, theme));
                }
                None => {}
            });
            row.cell(|ui| {
                if let Some(key) = column.key {
                    ui.add(Tag::key(strings::key_name(key), theme, Section::Databases));
                }
            });
        });
}
