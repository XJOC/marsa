//! Services: one row per process the daemon runs, with its actions.

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use marsa_core::types::ServiceAction;

use crate::bridge::Command;
use crate::model::{self, Model};
use crate::strings;
use crate::theme::page::{
    PID_COLUMN_MIN, SERVICE_COLUMN_MIN, STATE_COLUMN_MIN, STATE_COLUMN_WIDTH,
};

const ACTIONS: [ServiceAction; 3] = [
    ServiceAction::Start,
    ServiceAction::Stop,
    ServiceAction::Restart,
];

/// The services table. Returns the button pressed, if any.
pub fn show(ui: &mut egui::Ui, model: &Model) -> Option<Command> {
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
        .column(Column::auto().at_least(SERVICE_COLUMN_MIN))
        .column(
            Column::initial(STATE_COLUMN_WIDTH)
                .at_least(STATE_COLUMN_MIN)
                .clip(true),
        )
        .column(Column::auto().at_least(PID_COLUMN_MIN))
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
                    let pool = model::is_pool(&service.id);
                    for action in ACTIONS {
                        let enabled = connected && !pool && model::can(&service.state, action);
                        let button = egui::Button::new(strings::action_text(action));
                        let mut response = ui.add_enabled(enabled, button);
                        if pool {
                            response =
                                response.on_disabled_hover_text(strings::MANAGED_BY_SITES_HINT);
                        }
                        if response.clicked() {
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
