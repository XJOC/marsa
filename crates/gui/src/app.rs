//! Drawing. egui is immediate mode: `ui` runs on every frame and draws the
//! whole window again from the [`Model`], like a Blade view rendered again on
//! every request. A frame runs only on input or when the bridge sends an
//! update; nothing here asks for continuous repaints.

use std::sync::mpsc::TryRecvError;
use std::time::Instant;

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use marsa_core::types::{LogStream, ServiceAction};

use crate::bridge::{self, Command};
use crate::model::{self, Connection, Model, Update};
use crate::strings;

const ACTIONS: [ServiceAction; 3] = [
    ServiceAction::Start,
    ServiceAction::Stop,
    ServiceAction::Restart,
];

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
        egui::CentralPanel::default().show(ui, |ui| log(ui, &self.model));
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
                            pressed = Some(Command {
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
