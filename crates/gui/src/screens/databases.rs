//! Databases (layout.md, "Databases screen"; ADR 0028): Marsa's servers and
//! the sites' SQLite files, and the tables of the chosen one. Read-only.
//!
//! The menu in the header picks a source. The tables list on the start side
//! filters as the user types, with no read; the detail lists the tables
//! with the server's row estimates. Both draw only the rows in view, so a
//! database with thousands of tables costs what a small one does.

use eframe::egui::{Align, Layout, ScrollArea, Ui, Vec2, vec2};
use marsa_core::types::{DbServer, DbSource, DbTableKind, ServiceAction, ServiceId};

use super::lines::{message, state};
use crate::model::{Action, DbScreen, Model};
use crate::strings;
use crate::theme::page::{DB_DETAIL_END, DB_LIST_END, DB_LIST_WIDTH};
use crate::theme::{Direction, Section, Theme};
use crate::widgets::menu::{Item as MenuItem, Menu};
use crate::widgets::table::card;
use crate::widgets::{Button, Field, Text};

/// What the screen shows, from the model.
pub(crate) struct DatabasesView<'a> {
    /// The menu's button: the chosen source, if any.
    pub label: String,
    /// The menu's rows.
    pub choices: Vec<(Choice, String)>,
    pub body: Body<'a>,
}

/// A row of the source menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Choice {
    Source(DbSource),
    /// A stopped server: its row starts it.
    Start(DbServer),
}

pub(crate) enum Body<'a> {
    /// No daemon to ask.
    Offline,
    Loading,
    Empty,
    Stopped(DbServer),
    SignInFailed(DbServer),
    Failed(String),
    Tables(Tables<'a>),
}

pub(crate) struct Tables<'a> {
    pub filter: &'a str,
    /// The tables after the filter.
    pub rows: Vec<Row>,
    /// "No tables in blog yet.", when the database has none.
    pub none: Option<String>,
}

pub(crate) struct Row {
    pub name: String,
    pub view: bool,
    /// "12.4k", for the list.
    pub short: String,
    /// "about 12,408", for the detail.
    pub about: String,
}

pub(crate) fn view(model: &Model) -> DatabasesView<'_> {
    let db = &model.session.db;
    let label = db
        .chosen
        .as_ref()
        .map_or_else(|| strings::CHOOSE_SOURCE.to_owned(), strings::source_label);
    let mut choices = Vec::new();
    for info in db.sources.iter().flat_map(|sources| &sources.servers) {
        if info.databases.is_empty() && info.state != marsa_core::types::ServiceState::Running {
            choices.push((
                Choice::Start(info.server),
                strings::start_server(info.server),
            ));
        }
        for database in &info.databases {
            let source = DbSource::Server {
                server: info.server,
                database: database.name.clone(),
            };
            let text = strings::source_label(&source);
            choices.push((Choice::Source(source), text));
        }
    }
    for file in db.sources.iter().flat_map(|sources| &sources.files) {
        let source = DbSource::SiteFile {
            site: file.site.clone(),
        };
        let text = strings::source_label(&source);
        choices.push((Choice::Source(source), text));
    }
    let body = if !model.is_connected() {
        Body::Offline
    } else {
        match db.screen() {
            DbScreen::Loading => Body::Loading,
            DbScreen::Empty => Body::Empty,
            DbScreen::Stopped(server) => Body::Stopped(server),
            DbScreen::SignInFailed(server) => Body::SignInFailed(server),
            DbScreen::Failed(code) => Body::Failed(strings::error_text(&code)),
            DbScreen::Tables => Body::Tables(tables(model)),
        }
    };
    DatabasesView {
        label,
        choices,
        body,
    }
}

fn tables(model: &Model) -> Tables<'_> {
    let db = &model.session.db;
    let rows = db
        .shown_tables()
        .into_iter()
        .map(|info| Row {
            name: match info.table.schema.as_deref() {
                Some(schema) if schema != "public" => format!("{schema}.{}", info.table.name),
                _ => info.table.name.clone(),
            },
            view: info.kind == DbTableKind::View,
            short: info.estimate.map(strings::rows_short).unwrap_or_default(),
            about: info.estimate.map(strings::rows_about).unwrap_or_default(),
        })
        .collect();
    let empty = db.tables.as_ref().is_some_and(|t| t.tables.is_empty());
    let none = empty
        .then(|| db.chosen.as_ref().map(strings::no_tables))
        .flatten();
    Tables {
        filter: &db.filter,
        rows,
        none,
    }
}

pub(crate) fn show(ui: &mut Ui, view: &DatabasesView<'_>, theme: &Theme) -> Vec<Action> {
    let mut actions = Vec::new();
    ui.spacing_mut().item_spacing.y = theme.space.section_gap;
    let title = strings::section_name(Section::Databases);
    super::header(ui, theme, Section::Databases, title, |ui| {
        if view.choices.is_empty() {
            return;
        }
        let button = ui.add(Button::secondary(&view.label));
        let items: Vec<MenuItem<'_, usize>> = view
            .choices
            .iter()
            .enumerate()
            .map(|(index, (_, text))| MenuItem::new(index, text.as_str()))
            .collect();
        if let Some(index) = Menu::new(&items).from_end().show(&button) {
            actions.push(match &view.choices[index].0 {
                Choice::Source(source) => Action::ChooseDbSource(source.clone()),
                Choice::Start(server) => start(*server),
            });
        }
    });
    match &view.body {
        Body::Offline => message(ui, theme, strings::DAEMON_NOT_CONNECTED),
        Body::Loading => {}
        Body::Empty => {
            state(
                ui,
                theme,
                strings::NO_DATABASES,
                strings::NO_DATABASES_HOW,
                None,
            );
        }
        Body::Stopped(server) => {
            let label = strings::start_server(*server);
            let sentence = strings::db_server_stopped(*server);
            let how = strings::db_server_stopped_how(*server);
            if state(ui, theme, &sentence, &how, Some(Button::primary(&label))) {
                actions.push(start(*server));
            }
        }
        Body::SignInFailed(server) => {
            let sentence = strings::db_sign_in_failed(*server);
            let button = Button::primary(strings::TRY_AGAIN);
            if state(ui, theme, &sentence, strings::DB_SIGN_IN_HOW, Some(button)) {
                actions.push(Action::ReadDbSources);
            }
        }
        Body::Failed(text) => message(ui, theme, text),
        Body::Tables(tables) => match &tables.none {
            Some(none) => {
                state(ui, theme, none, strings::NO_TABLES_HOW, None);
            }
            None => split(ui, theme, tables, &mut actions),
        },
    }
    actions
}

fn start(server: DbServer) -> Action {
    Action::Service {
        service: ServiceId::Database(server.kind()),
        action: ServiceAction::Start,
    }
}

/// The tables list on the start side, the table of tables on the end side.
fn split(ui: &mut Ui, theme: &Theme, tables: &Tables<'_>, actions: &mut Vec<Action>) {
    let size = ui.available_size();
    ui.allocate_ui_with_layout(size, Direction::CURRENT.row(), |ui| {
        ui.spacing_mut().item_spacing.x = theme.space.section_gap;
        let height = ui.available_height();
        let column = Direction::CURRENT.column();
        ui.allocate_ui_with_layout(vec2(DB_LIST_WIDTH, height), column, |ui| {
            list(ui, theme, tables, actions);
        });
        let rest = vec2(ui.available_width(), height);
        ui.allocate_ui_with_layout(rest, column, |ui| detail(ui, theme, tables));
    });
}

/// "Find a table", then the names, with the server's estimate.
fn list(ui: &mut Ui, theme: &Theme, tables: &Tables<'_>, actions: &mut Vec<Action>) {
    let mut filter = tables.filter.to_owned();
    ui.add(Field::search(&mut filter, strings::FIND_TABLE).hint(strings::FIND_TABLE));
    if filter != tables.filter {
        actions.push(Action::FilterDbTables(filter));
    }
    let row = theme.size.control;
    card(ui, |ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let height = ui.available_height();
        ui.set_min_height(height);
        ScrollArea::vertical()
            .id_salt("db_tables_list")
            .auto_shrink(false)
            .max_height(height)
            .show_rows(ui, row, tables.rows.len(), |ui, range| {
                for item in &tables.rows[range] {
                    let caption = if item.view {
                        strings::DB_VIEW
                    } else {
                        item.short.as_str()
                    };
                    line(ui, theme, row, &item.name, caption, DB_LIST_END);
                }
            });
    });
}

/// The tables with their kind and estimate, then a caption.
fn detail(ui: &mut Ui, theme: &Theme, tables: &Tables<'_>) {
    let row = theme.size.control;
    // The caption under the card keeps a line of its own.
    let room = vec2(ui.available_width(), ui.available_height() - row);
    ui.allocate_ui_with_layout(room, Direction::CURRENT.column(), |ui| {
        card(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let height = ui.available_height() - row;
            let header = format!("{}  ·  {}", strings::DB_KIND, strings::DB_ROWS);
            line(ui, theme, row, strings::DB_TABLE, &header, DB_DETAIL_END);
            ScrollArea::vertical()
                .id_salt("db_tables_detail")
                .auto_shrink(false)
                .max_height(height.max(row))
                .show_rows(ui, row, tables.rows.len(), |ui, range| {
                    for item in &tables.rows[range] {
                        let kind = if item.view {
                            strings::DB_VIEW
                        } else {
                            strings::DB_TABLE_KIND
                        };
                        let end = format!("{kind}  ·  {}", item.about);
                        line(
                            ui,
                            theme,
                            row,
                            &item.name,
                            end.trim_end_matches("  ·  "),
                            DB_DETAIL_END,
                        );
                    }
                });
        });
    });
    ui.add(Text::new(
        strings::DB_ESTIMATES,
        theme.text.caption,
        theme.color.mute,
    ));
}

/// One line: the name at the start, `end` in the muted caption face at the
/// end, `end_width` wide.
fn line(ui: &mut Ui, theme: &Theme, height: f32, name: &str, end: &str, end_width: f32) {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(vec2(width, height), Direction::CURRENT.row(), |ui| {
        ui.add_space(theme.space.s3);
        let room = (ui.available_width() - end_width).max(0.0);
        ui.allocate_ui_with_layout(
            vec2(room, height),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.add(Text::new(name, theme.text.body_sm, theme.color.ink).truncate());
            },
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(theme.space.s3);
            ui.add(Text::new(end, theme.text.caption, theme.color.mute));
        });
    });
}
