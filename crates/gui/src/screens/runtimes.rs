//! Runtimes: each runtime with its versions, installs, uninstalls, and
//! Xdebug.

use eframe::egui;

use super::{install_progress, report_line};
use crate::bridge::Command;
use crate::model::{self, Model, Removal};
use crate::strings;
use crate::theme::page::VERSION_FIELD_WIDTH;

/// The Runtimes section: each runtime, its installed versions, its default
/// (a list for PHP and Node), a version field with an Install button, and a
/// list of the versions to uninstall; then the install, the last uninstall,
/// and the Xdebug box. Four rows, so a plain grid. Adds to `commands` the
/// default the user picked, if it is a change, and an Xdebug change; an
/// Install press queues a job in the model instead, and an uninstall opens
/// the confirmation.
pub fn show(ui: &mut egui::Ui, model: &mut Model, commands: &mut Vec<Command>) {
    ui.strong(strings::RUNTIMES);
    let mut picked = None;
    let mut install = None;
    let mut uninstall = None;
    if model.runtimes.is_empty() {
        ui.weak(strings::NO_RUNTIMES);
    } else {
        let connected = model.is_connected();
        let can_install = connected && !model.installing() && !model.uninstalling();
        let can_uninstall = model.can_uninstall();
        // Out of the model while the rows borrow it; put back below.
        let mut inputs = std::mem::take(&mut model.install_input);
        egui::Grid::new("runtimes")
            .striped(true)
            .num_columns(5)
            .show(ui, |ui| {
                for title in [
                    strings::COLUMN_RUNTIME,
                    strings::COLUMN_INSTALLED,
                    strings::COLUMN_DEFAULT,
                    strings::COLUMN_INSTALL,
                    strings::COLUMN_REMOVE,
                ] {
                    ui.strong(title);
                }
                ui.end_row();
                for row in &model.runtimes {
                    ui.label(strings::runtime_name(row.runtime));
                    ui.label(strings::installed_text(&row.installed));
                    if model::has_default(row.runtime) {
                        let chosen = model.default_choice(row.runtime);
                        let text = strings::default_text(chosen.as_ref(), row.default.as_deref());
                        let enabled = connected && !row.installed.is_empty();
                        ui.add_enabled_ui(enabled, |ui| {
                            let list =
                                egui::ComboBox::from_id_salt(("default", row.runtime.dir_name()))
                                    .selected_text(text)
                                    .show_ui(ui, |ui| {
                                        for version in &row.installed {
                                            let selected = chosen.as_ref() == Some(version);
                                            let entry =
                                                ui.selectable_label(selected, version.to_string());
                                            if entry.clicked() {
                                                picked = Some((row.runtime, version.clone()));
                                            }
                                        }
                                    });
                            list.response.on_hover_text(strings::DEFAULT_HINT);
                        });
                    } else {
                        ui.weak(strings::NEWEST).on_hover_text(strings::NEWEST_HINT);
                    }
                    ui.horizontal(|ui| {
                        let input = &mut inputs[model::runtime_index(row.runtime)];
                        let field = egui::TextEdit::singleline(input)
                            .hint_text(strings::VERSION_FIELD_EMPTY)
                            .desired_width(VERSION_FIELD_WIDTH);
                        let field = ui
                            .add_enabled(can_install, field)
                            .on_hover_text(strings::version_field_hint(row.runtime));
                        let entered = field.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter));
                        let button = egui::Button::new(strings::INSTALL_BUTTON);
                        let clicked = ui
                            .add_enabled(can_install, button)
                            .on_hover_text(strings::INSTALL_HINT)
                            .on_disabled_hover_text(strings::INSTALL_BUSY_HINT)
                            .clicked();
                        if can_install && (clicked || entered) {
                            install = Some(row.runtime);
                        }
                    });
                    let enabled = can_uninstall && !row.installed.is_empty();
                    ui.add_enabled_ui(enabled, |ui| {
                        let menu = ui.menu_button(strings::UNINSTALL_BUTTON, |ui| {
                            for version in &row.installed {
                                if ui.button(version.to_string()).clicked() {
                                    uninstall = Some(Removal::Uninstall {
                                        runtime: row.runtime,
                                        version: version.clone(),
                                    });
                                }
                            }
                        });
                        let response = menu.response.on_hover_text(strings::UNINSTALL_HINT);
                        if connected && !can_uninstall {
                            response.on_disabled_hover_text(strings::UNINSTALL_BUSY_HINT);
                        }
                    });
                    ui.end_row();
                }
            });
        model.install_input = inputs;
    }
    if let Some(runtime) = install {
        // Sent at the end of this frame.
        model.request_install(runtime);
    }
    if let Some(removal) = uninstall {
        model.ask_removal(removal);
    }
    install_progress(ui, model);
    uninstall_status(ui, model);
    if let Some(code) = &model.runtimes_notice {
        ui.colored_label(ui.visuals().error_fg_color, strings::error_text(code));
    }
    commands.extend(xdebug(ui, model));
    if let Some((runtime, version)) = picked
        && let Some(change) = model.choose_default(runtime, version)
    {
        commands.push(Command::Save(change));
    }
}

/// The uninstall that runs, or the lines about the last one.
fn uninstall_status(ui: &mut egui::Ui, model: &Model) {
    if let Some(removal @ Removal::Uninstall { .. }) = model.removing() {
        ui.weak(strings::removing(removal));
    }
    if let Some(notice) = &model.uninstall_notice {
        for line in strings::uninstall_lines(notice) {
            report_line(ui, line);
        }
    }
}

/// The Xdebug box, what it does now, and why the last read or change
/// failed. Returns the change to send, if the box was clicked.
fn xdebug(ui: &mut egui::Ui, model: &mut Model) -> Option<Command> {
    let lines = model.xdebug.as_ref().map_or_else(Vec::new, |state| {
        strings::xdebug_lines(state, model.xdebug_pending())
    });
    let mut on = model.xdebug_on().unwrap_or(false);
    let enabled =
        model.is_connected() && model.xdebug.is_some() && model.xdebug_pending().is_none();
    let response = ui
        .add_enabled(enabled, egui::Checkbox::new(&mut on, strings::XDEBUG))
        .on_hover_text(strings::XDEBUG_HINT);
    let command = if response.changed() {
        model.toggle_xdebug().map(Command::SetXdebug)
    } else {
        None
    };
    // Under the box, not beside it: a vertical layout wraps long lines.
    for line in &lines {
        ui.weak(line);
    }
    if let Some(code) = &model.xdebug_notice {
        ui.colored_label(ui.visuals().error_fg_color, strings::error_text(code));
    }
    command
}
