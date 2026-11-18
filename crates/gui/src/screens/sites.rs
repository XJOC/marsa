//! Sites: park and link, the sites table, the parked and the skipped
//! folders.

use std::path::PathBuf;

use eframe::egui;
use egui_extras::{Column, Size, StripBuilder, TableBuilder};
use marsa_core::types::{Site, SkippedFolder};

use crate::bridge::Command;
use crate::model::{self, Choice, HostsSync, Model, Pick, Removal, Setup, SitesNotice};
use crate::strings;
use crate::theme::page::{
    NAME_FIELD_WIDTH, PHP_COLUMN_WIDTH, SITE_FOLDER_COLUMN_MIN, SITE_KIND_COLUMN_MIN,
    SITE_NAME_COLUMN_MAX, SITE_NAME_COLUMN_MIN, SITE_URL_COLUMN_MAX, SITE_URL_COLUMN_MIN,
    SITES_MIN_LINES,
};

/// The most skipped folders shown at once; more scroll.
const SKIPPED_ROWS: usize = 4;
/// The most parked folders shown at once; more scroll.
const PARKED_ROWS: usize = 3;

/// What the user did in the sites section.
pub enum SitesAction {
    Send(Command),
    /// Pressed "Park a folder…" or "Link a folder…": the dialog opens.
    Pick(Pick),
}

/// What the user did in the sites table.
enum SiteAction {
    /// Flipped the HTTPS switch.
    Secure(Command),
    /// Picked an entry in the PHP list of the site `site`.
    Php { site: String, choice: Choice },
    /// Pressed Unlink for this site: the confirmation opens.
    Unlink(String),
}

/// The sites section: the park and link buttons, the link form, the sites
/// table, the parked folders, and the folders that did not become a site.
/// `picking`: a folder dialog is open. Returns what the user asked for, if
/// anything; an Unlink or Forget press opens the confirmation instead.
pub fn show(ui: &mut egui::Ui, model: &mut Model, picking: bool) -> Option<SitesAction> {
    let connected = model.is_connected();
    let mut action = None;
    ui.horizontal(|ui| {
        ui.strong(strings::SITES);
        let enabled = connected && !picking;
        for (pick, text, hint) in [
            (Pick::Park, strings::PARK_BUTTON, strings::PARK_BUTTON_HINT),
            (Pick::Link, strings::LINK_BUTTON, strings::LINK_BUTTON_HINT),
        ] {
            let clicked = ui
                .add_enabled(enabled, egui::Button::new(text))
                .on_hover_text(hint)
                .on_disabled_hover_text(if picking { strings::PICKING_HINT } else { hint })
                .clicked();
            if clicked {
                action = Some(SitesAction::Pick(pick));
            }
        }
    });
    if let Some(command) = link_form(ui, model, connected) {
        action = Some(SitesAction::Send(command));
    }
    if let Some(removal @ (Removal::Unlink { .. } | Removal::Forget { .. })) = model.removing() {
        ui.weak(strings::removing(removal));
    }
    let mut forget = sites_notice(ui, model);
    if let Some(command) = hosts_line(ui, model) {
        action = Some(SitesAction::Send(command));
    }

    let view: &Model = model;
    let list = &view.sites;
    let line = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y;
    let skipped_height = if list.skipped.is_empty() {
        0.0
    } else {
        // The title and up to `SKIPPED_ROWS` lines.
        line * (list.skipped.len().min(SKIPPED_ROWS) + 1) as f32
    };
    let parked_height = if list.parked.is_empty() {
        0.0
    } else {
        // The title and up to `PARKED_ROWS` rows with a button.
        let row = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
        line + row * list.parked.len().min(PARKED_ROWS) as f32
    };
    let mut row_action = None;
    StripBuilder::new(ui)
        .size(Size::remainder().at_least(line * SITES_MIN_LINES))
        .size(Size::exact(parked_height))
        .size(Size::exact(skipped_height))
        .vertical(|mut strip| {
            strip.cell(|ui| {
                if list.sites.is_empty() {
                    ui.weak(strings::NO_SITES);
                } else {
                    row_action = site_table(ui, view, connected);
                }
            });
            strip.cell(|ui| {
                if let Some(folder) = parked(ui, view) {
                    forget = Some(folder);
                }
            });
            strip.cell(|ui| skipped(ui, &list.skipped));
        });
    if let Some(folder) = forget {
        model.ask_removal(Removal::Forget { folder });
    }
    match row_action {
        Some(SiteAction::Secure(switched)) => {
            // The notice was about the last action.
            model.sites_notice = None;
            action = Some(SitesAction::Send(switched));
        }
        Some(SiteAction::Php { site, choice }) => {
            if let Some(change) = model.choose_site_php(&site, choice) {
                action = Some(SitesAction::Send(Command::Save(change)));
            }
        }
        Some(SiteAction::Unlink(site)) => {
            model.ask_removal(Removal::Unlink { site });
        }
        None => {}
    }
    action
}

/// Linux on the hosts way (task M4-18k3): the sites the hosts file lacks,
/// such as one the folder watcher found, with the button that adds them.
/// The click is the only way the window asks for the password for this.
fn hosts_line(ui: &mut egui::Ui, model: &mut Model) -> Option<Command> {
    let failed = match &model.hosts_sync {
        HostsSync::Failed(code) => Some(strings::error_text(code)),
        HostsSync::Idle | HostsSync::Running => None,
    };
    if !model.hosts.used || model.hosts.missing.is_empty() {
        if let Some(text) = failed {
            ui.colored_label(ui.visuals().error_fg_color, text);
        }
        return None;
    }
    ui.colored_label(
        ui.visuals().warn_fg_color,
        strings::hosts_waiting(&model.hosts.missing),
    );
    let running = model.hosts_sync == HostsSync::Running;
    let mut command = None;
    ui.horizontal(|ui| {
        let enabled = model.is_connected() && !running && model.setup != Setup::Running;
        let clicked = ui
            .add_enabled(enabled, egui::Button::new(strings::ADD_TO_HOSTS))
            .on_hover_text(strings::ADD_TO_HOSTS_HINT)
            .clicked();
        if clicked && model.start_hosts_sync() {
            command = Some(Command::SyncHosts);
        }
        if running {
            ui.weak(strings::HOSTS_ADDING);
        }
    });
    if let Some(text) = failed {
        ui.colored_label(ui.visuals().error_fg_color, text);
    }
    command
}

/// The line about the last sites action. After `site_is_parked` it offers
/// to forget the site's parked folder: returns that folder if pressed.
fn sites_notice(ui: &mut egui::Ui, model: &Model) -> Option<PathBuf> {
    let error = ui.visuals().error_fg_color;
    match model.sites_notice.as_ref()? {
        SitesNotice::Failed(code) => {
            ui.colored_label(error, strings::error_text(code));
        }
        SitesNotice::Parked(folder) => {
            ui.weak(strings::parked(folder));
        }
        SitesNotice::Linked { name, folder } => {
            ui.weak(strings::linked(name, folder));
        }
        SitesNotice::Unlinked(site) => {
            ui.weak(strings::unlinked(site));
        }
        SitesNotice::Forgot(folder) => {
            ui.weak(strings::forgot(folder));
        }
        SitesNotice::SiteIsParked { site, folder } => {
            ui.colored_label(error, strings::site_is_parked(site, folder.as_deref()));
            let folder = folder.as_ref()?;
            let button = egui::Button::new(strings::forget_folder(folder));
            let clicked = ui
                .add_enabled(model.can_remove(), button)
                .on_hover_text(strings::FORGET_HINT)
                .clicked();
            if clicked {
                return Some(folder.clone());
            }
        }
    }
    None
}

/// The parked folders, each with its Forget button. `show_rows` lays out the
/// visible rows only. Returns the folder whose Forget was pressed.
fn parked(ui: &mut egui::Ui, model: &Model) -> Option<PathBuf> {
    let folders = &model.sites.parked;
    if folders.is_empty() {
        return None;
    }
    ui.strong(strings::PARKED_TITLE);
    let enabled = model.can_remove();
    let busy = model.is_connected() && !enabled;
    let row_height = ui.spacing().interact_size.y;
    let mut pressed = None;
    egui::ScrollArea::vertical()
        .id_salt("parked")
        .auto_shrink(false)
        .show_rows(ui, row_height, folders.len(), |ui, rows| {
            for folder in &folders[rows] {
                ui.horizontal(|ui| {
                    let button = egui::Button::new(strings::FORGET_BUTTON);
                    let mut response = ui
                        .add_enabled(enabled, button)
                        .on_hover_text(strings::FORGET_HINT);
                    if busy {
                        response = response.on_disabled_hover_text(strings::REMOVE_BUSY_HINT);
                    }
                    if response.clicked() {
                        pressed = Some(folder.clone());
                    }
                    let shown = folder.display().to_string();
                    ui.add(egui::Label::new(shown).truncate());
                });
            }
        });
    pressed
}

/// The link form, once a folder to link was picked: the site name, Link,
/// and Cancel. Returns the link to send, if the user confirmed a valid name.
fn link_form(ui: &mut egui::Ui, model: &mut Model, connected: bool) -> Option<Command> {
    let mut submit = false;
    let mut cancel = false;
    {
        let form = model.link.as_mut()?;
        ui.label(strings::link_folder(&form.folder));
        ui.horizontal(|ui| {
            ui.label(strings::LINK_NAME);
            let field = egui::TextEdit::singleline(&mut form.name)
                .hint_text(strings::LINK_NAME_HINT)
                .desired_width(NAME_FIELD_WIDTH);
            let field = ui.add(field);
            let entered =
                field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            ui.weak(strings::site_suffix());
            let clicked = ui
                .add_enabled(connected, egui::Button::new(strings::LINK_CONFIRM))
                .clicked();
            submit = connected && (clicked || entered);
            cancel = ui.button(strings::CANCEL).clicked();
        });
    }
    if let Some(site) = model.link_replaces() {
        ui.weak(strings::link_replaces(site));
    }
    if cancel {
        model.link = None;
        return None;
    }
    if !submit {
        return None;
    }
    let (name, folder) = model.link_request()?;
    Some(Command::Link { name, folder })
}

/// The sites table. `rows` draws the visible rows only. Returns what the
/// user did in it, if anything.
fn site_table(ui: &mut egui::Ui, model: &Model, connected: bool) -> Option<SiteAction> {
    let sites = &model.sites.sites;
    let row_height = ui.spacing().interact_size.y;
    let can_remove = model.can_remove();
    let busy = connected && !can_remove;
    let mut action = None;
    TableBuilder::new(ui)
        .id_salt("sites")
        .striped(true)
        // Fill the space above the skipped folders, and scroll inside it.
        .auto_shrink(false)
        .min_scrolled_height(0.0)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(
            Column::auto()
                .at_least(SITE_NAME_COLUMN_MIN)
                .at_most(SITE_NAME_COLUMN_MAX)
                .clip(true),
        )
        .column(
            Column::auto()
                .at_least(SITE_URL_COLUMN_MIN)
                .at_most(SITE_URL_COLUMN_MAX)
                .clip(true),
        )
        .column(Column::auto())
        .column(Column::auto().at_least(SITE_KIND_COLUMN_MIN))
        .column(Column::auto().at_least(PHP_COLUMN_WIDTH))
        .column(Column::auto())
        .column(
            Column::remainder()
                .at_least(SITE_FOLDER_COLUMN_MIN)
                .clip(true),
        )
        .header(row_height, |mut header| {
            for title in [
                strings::COLUMN_NAME,
                strings::COLUMN_URL,
                strings::COLUMN_HTTPS,
                strings::COLUMN_KIND,
                strings::COLUMN_PHP,
                strings::COLUMN_REMOVE,
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
                        action = Some(SiteAction::Secure(Command::Secure {
                            site: site.name.clone(),
                            secure,
                        }));
                    }
                });
                row.col(|ui| {
                    ui.label(strings::kind_text(site.kind));
                });
                row.col(|ui| {
                    if let Some(choice) = site_php(ui, model, site, connected) {
                        let site = site.name.clone();
                        action = Some(SiteAction::Php { site, choice });
                    }
                });
                row.col(|ui| {
                    // A site from a parked folder goes with its folder only.
                    let holder = model.parked_folder(site);
                    let enabled = can_remove && holder.is_none();
                    let button = egui::Button::new(strings::UNLINK_BUTTON);
                    let mut response = ui
                        .add_enabled(enabled, button)
                        .on_hover_text(strings::UNLINK_HINT);
                    if let Some(folder) = holder {
                        response = response.on_disabled_hover_ui(|ui| {
                            ui.label(strings::unlink_parked_hint(folder));
                        });
                    } else if busy {
                        response = response.on_disabled_hover_text(strings::REMOVE_BUSY_HINT);
                    }
                    if response.clicked() {
                        action = Some(SiteAction::Unlink(site.name.clone()));
                    }
                });
                row.col(|ui| {
                    let folder = site.root.display().to_string();
                    ui.add(egui::Label::new(folder).truncate());
                });
            });
        });
    action
}

/// The PHP list of one site: first what the site runs with no setting of its
/// own, then each installed PHP. Like the HTTPS switch, it shows the model's
/// value on every frame. Returns the entry the user clicked, if any.
fn site_php(ui: &mut egui::Ui, model: &Model, site: &Site, connected: bool) -> Option<Choice> {
    let installed = model.php_versions();
    if installed.is_empty() && site.php.is_none() {
        ui.label(strings::NO_PHP);
        return None;
    }
    let chosen = model.site_choice(site);
    let mut clicked = None;
    ui.add_enabled_ui(connected, |ui| {
        let list = egui::ComboBox::from_id_salt(("php", &site.name))
            .selected_text(strings::php_choice_text(&chosen, site))
            .show_ui(ui, |ui| {
                let auto = ui
                    .selectable_label(chosen == Choice::Auto, strings::auto_php(site))
                    .on_hover_text(strings::AUTO_PHP_HINT);
                if auto.clicked() {
                    clicked = Some(Choice::Auto);
                }
                for version in installed {
                    let selected = matches!(&chosen, Choice::Version(v) if v == version);
                    if ui.selectable_label(selected, version.to_string()).clicked() {
                        clicked = Some(Choice::Version(version.clone()));
                    }
                }
            });
        list.response.on_hover_text(strings::site_php_hint(site));
    });
    clicked
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
