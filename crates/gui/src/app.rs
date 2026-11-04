//! Drawing. egui is immediate mode: `ui` runs on every frame and draws the
//! whole window again from the [`Model`], like a Blade view rendered again on
//! every request. A frame runs only on input or when the bridge sends an
//! update; nothing here asks for continuous repaints.

use std::sync::mpsc::TryRecvError;
use std::time::Instant;

use eframe::egui;
use egui_extras::{Column, Size, StripBuilder, TableBuilder};
use marsa_core::types::{LogStream, ServiceAction, Site, SkippedFolder};

use crate::bridge::{self, Command};
use crate::model::{self, Connection, Model, SitesNotice, Update};
use crate::strings;

const ACTIONS: [ServiceAction; 3] = [
    ServiceAction::Start,
    ServiceAction::Stop,
    ServiceAction::Restart,
];

/// The first height of the log panel at the bottom. The user can drag it.
const LOG_HEIGHT: f32 = 170.0;
/// The width of the park field.
const PARK_FIELD_WIDTH: f32 = 360.0;
/// The most skipped folders shown at once; more scroll.
const SKIPPED_ROWS: usize = 4;

pub struct App {
    model: Model,
    bridge: bridge::Handle,
    /// Set until the first frame, to log the time it took.
    started: Option<Instant>,
}

impl App {
    pub fn new(ctx: &egui::Context, bridge: bridge::Handle, started: Instant) -> Self {
        bridge.attach(ctx);
        Self {
            model: Model::default(),
            bridge,
            started: Some(started),
        }
    }
}

impl eframe::App for App {
    /// Takes every waiting update before the frame is drawn.
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        loop {
            match self.bridge.updates.try_recv() {
                Ok(update) => self.model.update(update),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let failed = Connection::Failed(strings::BRIDGE_FAILED.to_owned());
                    if self.model.connection != failed {
                        self.model
                            .update(Update::Failed(strings::BRIDGE_FAILED.to_owned()));
                    }
                    break;
                }
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(started) = self.started.take() {
            tracing::debug!(ms = started.elapsed().as_millis(), "first frame");
        }
        egui::Panel::top("status").show(ui, |ui| status(ui, &self.model));
        egui::Panel::top("services").show(ui, |ui| {
            if let Some(command) = services(ui, &self.model) {
                // The notice was about the last action.
                self.model.notice = None;
                self.bridge.send(command);
            }
        });
        egui::Panel::bottom("log")
            .resizable(true)
            .default_size(LOG_HEIGHT)
            .show(ui, |ui| log(ui, &self.model));
        // The sites get the space that is left: their list grows with the
        // user's projects.
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(command) = sites(ui, &mut self.model) {
                self.bridge.send(command);
            }
        });
    }
}

fn status(ui: &mut egui::Ui, model: &Model) {
    ui.label(strings::status_line(&model.connection));
    if let Some(code) = &model.notice {
        ui.colored_label(ui.visuals().error_fg_color, strings::error_text(code));
    }
}

/// The services table. Returns the button pressed, if any.
fn services(ui: &mut egui::Ui, model: &Model) -> Option<Command> {
    ui.strong(strings::SERVICES);
    if model.services.is_empty() {
        ui.weak(strings::NO_SERVICES);
        return None;
    }
    let connected = model.is_connected();
    let row_height = ui.spacing().interact_size.y;
    let mut pressed = None;
    TableBuilder::new(ui)
        .id_salt("services")
        .striped(true)
        .vscroll(false)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::auto().at_least(80.0))
        .column(Column::initial(260.0).at_least(80.0).clip(true))
        .column(Column::auto().at_least(50.0))
        .column(Column::remainder())
        .header(row_height, |mut header| {
            for title in [
                strings::COLUMN_SERVICE,
                strings::COLUMN_STATE,
                strings::COLUMN_PID,
                strings::COLUMN_ACTIONS,
            ] {
                header.col(|ui| {
                    ui.strong(title);
                });
            }
        })
        .body(|body| {
            body.rows(row_height, model.services.len(), |mut row| {
                let service = &model.services[row.index()];
                row.col(|ui| {
                    ui.label(&service.id);
                });
                row.col(|ui| {
                    ui.add(egui::Label::new(strings::state_text(&service.state)).truncate());
                });
                row.col(|ui| {
                    if let Some(pid) = service.pid {
                        ui.label(pid.to_string());
                    }
                });
                row.col(|ui| {
                    for action in ACTIONS {
                        let enabled = connected && model::can(&service.state, action);
                        let button = egui::Button::new(strings::action_text(action));
                        if ui.add_enabled(enabled, button).clicked() {
                            pressed = Some(Command::Service {
                                service: service.id.clone(),
                                action,
                            });
                        }
                    }
                });
            });
        });
    pressed
}

/// The sites section: the park field, the sites table, and the folders that
/// did not become a site. Returns what the user asked for, if anything.
fn sites(ui: &mut egui::Ui, model: &mut Model) -> Option<Command> {
    ui.strong(strings::SITES);
    let connected = model.is_connected();
    let mut command = None;
    ui.horizontal(|ui| {
        let field = egui::TextEdit::singleline(&mut model.park_input)
            .hint_text(strings::PARK_FIELD_HINT)
            .desired_width(PARK_FIELD_WIDTH);
        let field = ui.add(field);
        let entered = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        let ready = connected && !model.park_input.trim().is_empty();
        let button = egui::Button::new(strings::PARK_BUTTON);
        let clicked = ui
            .add_enabled(ready, button)
            .on_hover_text(strings::PARK_BUTTON_HINT)
            .clicked();
        if ready && (clicked || entered) {
            command = model.park_request().map(Command::Park);
        }
    });
    match &model.sites_notice {
        Some(SitesNotice::Failed(code)) => {
            ui.colored_label(ui.visuals().error_fg_color, strings::error_text(code));
        }
        Some(SitesNotice::Parked(folder)) => {
            ui.weak(strings::parked(folder));
        }
        None => {}
    }

    let list = &model.sites;
    let line = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y;
    let skipped_height = if list.skipped.is_empty() {
        0.0
    } else {
        // The title and up to `SKIPPED_ROWS` lines.
        line * (list.skipped.len().min(SKIPPED_ROWS) + 1) as f32
    };
    let mut switched = None;
    StripBuilder::new(ui)
        .size(Size::remainder().at_least(line * 2.0))
        .size(Size::exact(skipped_height))
        .vertical(|mut strip| {
            strip.cell(|ui| {
                if list.sites.is_empty() {
                    ui.weak(strings::NO_SITES);
                } else {
                    switched = site_table(ui, &list.sites, connected);
                }
            });
            strip.cell(|ui| skipped(ui, &list.skipped));
        });
    if switched.is_some() {
        // The notice was about the last action.
        model.sites_notice = None;
        command = switched;
    }
    command
}

/// The sites table. `rows` draws the visible rows only. Returns the HTTPS
/// switch the user flipped, if any.
fn site_table(ui: &mut egui::Ui, sites: &[Site], connected: bool) -> Option<Command> {
    let row_height = ui.spacing().interact_size.y;
    let mut switched = None;
    TableBuilder::new(ui)
        .id_salt("sites")
        .striped(true)
        // Fill the space above the skipped folders, and scroll inside it.
        .auto_shrink(false)
        .min_scrolled_height(0.0)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::auto().at_least(80.0).at_most(240.0).clip(true))
        .column(Column::auto().at_least(120.0).at_most(320.0).clip(true))
        .column(Column::auto())
        .column(Column::auto().at_least(60.0))
        .column(Column::auto().at_least(40.0))
        .column(Column::remainder().at_least(80.0).clip(true))
        .header(row_height, |mut header| {
            for title in [
                strings::COLUMN_NAME,
                strings::COLUMN_URL,
                strings::COLUMN_HTTPS,
                strings::COLUMN_KIND,
                strings::COLUMN_PHP,
                strings::COLUMN_FOLDER,
            ] {
                header.col(|ui| {
                    ui.strong(title);
                });
            }
        })
        .body(|body| {
            body.rows(row_height, sites.len(), |mut row| {
                let site = &sites[row.index()];
                row.col(|ui| {
                    ui.add(egui::Label::new(&site.name).truncate());
                });
                row.col(|ui| {
                    let url = model::url(site);
                    if ui.link(&url).clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                    }
                });
                row.col(|ui| {
                    // A copy: the box shows the daemon's value again on the
                    // next frame, and the `sites_changed` event then sets it.
                    let mut secure = site.secure;
                    let switch = egui::Checkbox::without_text(&mut secure);
                    let response = ui
                        .add_enabled(connected, switch)
                        .on_hover_text(strings::HTTPS_HINT);
                    if response.changed() {
                        switched = Some(Command::Secure {
                            site: site.name.clone(),
                            secure,
                        });
                    }
                });
                row.col(|ui| {
                    ui.label(strings::kind_text(site.kind));
                });
                row.col(|ui| {
                    ui.label(strings::php_text(site));
                });
                row.col(|ui| {
                    let folder = site.root.display().to_string();
                    ui.add(egui::Label::new(folder).truncate());
                });
            });
        });
    switched
}

/// The folders that did not become a site, each with the reason.
/// `show_rows` lays out the visible lines only.
fn skipped(ui: &mut egui::Ui, folders: &[SkippedFolder]) {
    if folders.is_empty() {
        return;
    }
    ui.strong(strings::SKIPPED_TITLE);
    let row_height = ui.text_style_height(&egui::TextStyle::Body);
    egui::ScrollArea::vertical()
        .id_salt("skipped")
        .auto_shrink(false)
        .show_rows(ui, row_height, folders.len(), |ui, rows| {
            for folder in &folders[rows] {
                let line = strings::skipped_line(&folder.path, &folder.code);
                ui.add(egui::Label::new(line).truncate());
            }
        });
}

/// The log of [`model::LOG_SERVICE`]. `show_rows` lays out the visible lines
/// only. It follows new lines while scrolled to the end, and stays put once
/// the user scrolls up. Caddy writes its whole log to stderr, so stderr keeps
/// the normal text color and stdout is the one drawn weak.
fn log(ui: &mut egui::Ui, model: &Model) {
    ui.strong(strings::LOG_TITLE);
    if model.log.is_empty() {
        ui.weak(strings::LOG_EMPTY);
        return;
    }
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    egui::ScrollArea::vertical()
        .id_salt("log")
        .auto_shrink(false)
        .stick_to_bottom(true)
        .show_rows(ui, row_height, model.log.len(), |ui, rows| {
            for row in model.log.range(rows) {
                let mut text = egui::RichText::new(&row.text).monospace();
                if row.stream == LogStream::Stdout {
                    text = text.weak();
                }
                ui.add(egui::Label::new(text).truncate());
            }
        });
}
