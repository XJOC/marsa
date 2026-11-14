//! Drawing. egui is immediate mode: `ui` runs on every frame and draws the
//! whole window again from the [`Model`], like a Blade view rendered again on
//! every request. A frame runs only on input or when the bridge sends an
//! update; nothing here asks for continuous repaints.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Instant;

use eframe::egui;
use egui_extras::{Column, Size, StripBuilder, TableBuilder};
use marsa_core::recipes::Platform;
use marsa_core::types::{LogStream, ServiceAction, Site, SkippedFolder};

use crate::bridge::{self, Command};
use crate::model::{
    self, Autostart, Change, Choice, Connection, FirstRun, InstallStep, Model, Pick, Setup,
    SitesNotice, Update,
};
use crate::strings;

const ACTIONS: [ServiceAction; 3] = [
    ServiceAction::Start,
    ServiceAction::Stop,
    ServiceAction::Restart,
];

/// The first height of the log panel at the bottom. The user can drag it.
const LOG_HEIGHT: f32 = 170.0;
/// The most skipped folders shown at once; more scroll.
const SKIPPED_ROWS: usize = 4;
/// The narrowest PHP column: a closed list with a version in it.
const PHP_COLUMN_WIDTH: f32 = 110.0;
/// The width of a version field in the Runtimes section.
const VERSION_FIELD_WIDTH: f32 = 70.0;
/// The width of the site name field of the link form.
const NAME_FIELD_WIDTH: f32 = 180.0;
/// The width of an install's progress bar.
const PROGRESS_WIDTH: f32 = 420.0;
/// The most height the Settings section takes before it scrolls.
const SETTINGS_HEIGHT: f32 = 240.0;

pub struct App {
    model: Model,
    bridge: bridge::Handle,
    /// The folder dialog that is open, if any.
    picker: Option<Picker>,
    /// Set until the first frame, to log the time it took.
    started: Option<Instant>,
}

/// A folder dialog that is open on its own thread, so the window keeps
/// drawing and answering the system while the user picks (a blocked window
/// is reported as "not responding" on Linux).
struct Picker {
    pick: Pick,
    /// The folder, or `None` if the user cancelled. Sent once.
    folder: Receiver<Option<PathBuf>>,
}

impl App {
    pub fn new(
        ctx: &egui::Context,
        bridge: bridge::Handle,
        data_dir: PathBuf,
        started: Instant,
    ) -> Self {
        bridge.attach(ctx);
        Self {
            model: Model::new(data_dir),
            bridge,
            picker: None,
            started: Some(started),
        }
    }

    /// Sends the next queued install once none of this window runs.
    fn send_next_install(&mut self) {
        if let Some(job) = self.model.next_install() {
            self.bridge.send(Command::Install(job));
        }
    }

    /// Opens the system's folder dialog for `pick` on its own thread.
    fn open_picker(&mut self, ctx: &egui::Context, pick: Pick) {
        if self.picker.is_some() {
            return;
        }
        let (send, folder) = mpsc::channel();
        let ctx = ctx.clone();
        let title = strings::pick_title(pick);
        // rfd runs the dialog on the main thread where the system needs it
        // (macOS), and blocks only this thread until the user is done.
        let spawned = thread::Builder::new()
            .name("folder-dialog".into())
            .spawn(move || {
                let folder = rfd::FileDialog::new().set_title(title).pick_folder();
                // The window may have closed meanwhile.
                let _ = send.send(folder);
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => self.picker = Some(Picker { pick, folder }),
            Err(err) => tracing::warn!(error = %err, "cannot open the folder dialog"),
        }
    }

    /// Takes the picked folder once the dialog has closed.
    fn take_pick(&mut self) {
        let Some(picker) = &self.picker else {
            return;
        };
        let folder = match picker.folder.try_recv() {
            Ok(folder) => folder,
            Err(TryRecvError::Empty) => return,
            // The dialog thread ended without an answer.
            Err(TryRecvError::Disconnected) => None,
        };
        let pick = picker.pick;
        self.picker = None;
        let Some(folder) = folder else {
            return;
        };
        match pick {
            Pick::Park => {
                if let Some(path) = self.model.park_request(&folder) {
                    self.bridge.send(Command::Park(path));
                }
            }
            Pick::Link => self.model.pick_link_folder(&folder),
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
        self.take_pick();
        // An install that ended may start the next one.
        self.send_next_install();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(started) = self.started.take() {
            tracing::debug!(ms = started.elapsed().as_millis(), "first frame");
        }
        let mut commands = Vec::new();
        let mut pick = None;
        egui::Panel::top("status").show(ui, |ui| {
            commands.extend(status(ui, &mut self.model));
        });
        if self.model.first_run.is_some() && self.model.is_connected() {
            egui::Panel::top("first_run").show(ui, |ui| {
                commands.extend(first_run(ui, &mut self.model));
            });
        }
        egui::Panel::top("services").show(ui, |ui| {
            if let Some(command) = services(ui, &self.model) {
                // The notice was about the last action.
                self.model.notice = None;
                commands.push(command);
            }
        });
        egui::Panel::top("runtimes").show(ui, |ui| {
            if let Some(change) = runtimes(ui, &mut self.model) {
                commands.push(Command::Save(change));
            }
        });
        egui::Panel::bottom("log")
            .resizable(true)
            .default_size(LOG_HEIGHT)
            .show(ui, |ui| log(ui, &self.model));
        // The sites get the space that is left: their list grows with the
        // user's projects.
        let picking = self.picker.is_some();
        egui::CentralPanel::default().show(ui, |ui| match sites(ui, &mut self.model, picking) {
            Some(SitesAction::Send(command)) => commands.push(command),
            Some(SitesAction::Pick(chosen)) => pick = Some(chosen),
            None => {}
        });
        for command in commands {
            self.bridge.send(command);
        }
        if let Some(pick) = pick {
            let ctx = ui.ctx().clone();
            self.open_picker(&ctx, pick);
        }
        // A button may have queued an install.
        self.send_next_install();
    }
}

/// The status line, the last notice, and the Settings section.
fn status(ui: &mut egui::Ui, model: &mut Model) -> Option<Command> {
    ui.label(strings::status_line(&model.connection));
    if let Some(code) = &model.notice {
        ui.colored_label(ui.visuals().error_fg_color, strings::error_text(code));
    }
    let mut command = None;
    egui::CollapsingHeader::new(strings::SETTINGS)
        .id_salt("settings")
        .default_open(false)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("settings_scroll")
                .max_height(SETTINGS_HEIGHT)
                .show(ui, |ui| command = settings(ui, model));
        });
    command
}

/// Start at login, the data folder, the versions, and setup.
fn settings(ui: &mut egui::Ui, model: &mut Model) -> Option<Command> {
    let mut command = None;
    ui.horizontal(|ui| {
        let mut on = model.autostart == Autostart::On;
        let usable = matches!(
            model.autostart,
            Autostart::Off | Autostart::On | Autostart::Stale
        );
        let enabled = model.is_connected() && usable;
        let response = ui
            .add_enabled(enabled, egui::Checkbox::new(&mut on, strings::AUTOSTART))
            .on_hover_text(strings::AUTOSTART_HINT);
        if response.changed() {
            command = model.toggle_autostart().map(Command::SetAutostart);
        }
        if let Some(note) = strings::autostart_note(model.autostart) {
            ui.weak(note);
        }
    });
    if let Some(code) = &model.settings_notice {
        ui.colored_label(ui.visuals().error_fg_color, strings::error_text(code));
    }
    egui::Grid::new("settings_grid")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label(strings::DATA_FOLDER)
                .on_hover_text(strings::DATA_FOLDER_HINT);
            let folder = model.data_dir.display().to_string();
            ui.add(egui::Label::new(folder).selectable(true));
            ui.end_row();
            ui.label(strings::APP_VERSION);
            ui.label(model::APP_VERSION);
            ui.end_row();
            ui.label(strings::DAEMON_VERSION);
            ui.label(strings::daemon_version(&model.connection));
            ui.end_row();
        });
    ui.separator();
    if let Some(setup) = setup(ui, model) {
        command = Some(setup);
    }
    command
}

/// What setup does, its button, and its last report. In the first-run panel
/// and in Settings.
fn setup(ui: &mut egui::Ui, model: &mut Model) -> Option<Command> {
    let platform = Platform::current();
    ui.label(strings::setup_explanation(platform));
    let running = model.setup == Setup::Running;
    let mut command = None;
    ui.horizontal(|ui| {
        let enabled = model.is_connected() && !running;
        let clicked = ui
            .add_enabled(enabled, egui::Button::new(strings::RUN_SETUP))
            .on_hover_text(strings::RUN_SETUP_HINT)
            .clicked();
        if clicked && model.start_setup() {
            command = Some(Command::Setup);
        }
        if running {
            ui.weak(strings::setup_running(platform));
        }
    });
    let error = ui.visuals().error_fg_color;
    match &model.setup {
        Setup::Finished(report) => {
            for line in strings::setup_lines(report) {
                if line.problem {
                    ui.colored_label(error, line.text);
                } else {
                    ui.label(line.text);
                }
            }
        }
        Setup::Failed(code) => {
            ui.colored_label(error, strings::error_text(code));
        }
        Setup::Idle | Setup::Running => {}
    }
    command
}

/// The first-run panel: one button that installs PHP and Caddy, then setup.
fn first_run(ui: &mut egui::Ui, model: &mut Model) -> Option<Command> {
    ui.strong(strings::FIRST_RUN_TITLE);
    let mut command = None;
    match model.first_run {
        Some(stage @ (FirstRun::Offer | FirstRun::Installing)) => {
            ui.label(strings::FIRST_RUN_TEXT);
            ui.horizontal(|ui| {
                let offer = stage == FirstRun::Offer;
                let enabled = offer && !model.installing();
                let clicked = ui
                    .add_enabled(enabled, egui::Button::new(strings::FIRST_RUN_INSTALL))
                    .on_hover_text(strings::FIRST_RUN_INSTALL_HINT)
                    .clicked();
                if clicked {
                    // The bridge sends the first job at the end of this frame.
                    model.start_first_run();
                }
                if offer && ui.button(strings::NOT_NOW).clicked() {
                    model.close_first_run();
                }
            });
            install_progress(ui, model);
        }
        Some(FirstRun::Setup) => {
            ui.label(strings::FIRST_RUN_READY);
            command = setup(ui, model);
            let done = matches!(model.setup, Setup::Finished(_));
            let close = if done {
                strings::CLOSE
            } else {
                strings::NOT_NOW
            };
            if ui.button(close).clicked() {
                model.close_first_run();
            }
        }
        None => {}
    }
    command
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

/// The Runtimes section: each runtime, its installed versions, its default
/// (a list for PHP and Node), and a version field with an Install button.
/// Four rows, so a plain grid. Returns the default the user picked, if it is
/// a change; an Install press queues a job in the model instead.
fn runtimes(ui: &mut egui::Ui, model: &mut Model) -> Option<Change> {
    ui.strong(strings::RUNTIMES);
    let mut picked = None;
    let mut install = None;
    if model.runtimes.is_empty() {
        ui.weak(strings::NO_RUNTIMES);
    } else {
        let connected = model.is_connected();
        let can_install = connected && !model.installing();
        // Out of the model while the rows borrow it; put back below.
        let mut inputs = std::mem::take(&mut model.install_input);
        egui::Grid::new("runtimes")
            .striped(true)
            .num_columns(4)
            .show(ui, |ui| {
                for title in [
                    strings::COLUMN_RUNTIME,
                    strings::COLUMN_INSTALLED,
                    strings::COLUMN_DEFAULT,
                    strings::COLUMN_INSTALL,
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
                    ui.end_row();
                }
            });
        model.install_input = inputs;
    }
    if let Some(runtime) = install {
        // Sent at the end of this frame.
        model.request_install(runtime);
    }
    install_progress(ui, model);
    if let Some(code) = &model.runtimes_notice {
        ui.colored_label(ui.visuals().error_fg_color, strings::error_text(code));
    }
    let (runtime, version) = picked?;
    model.choose_default(runtime, version)
}

/// The install the model shows: a progress bar while it runs, then one
/// line. No animation: the bar moves with the daemon's events.
fn install_progress(ui: &mut egui::Ui, model: &Model) {
    let Some(view) = model.install_view() else {
        return;
    };
    let text = strings::install_status(view);
    match &view.step {
        InstallStep::Failed(_) => {
            ui.colored_label(ui.visuals().error_fg_color, text);
        }
        InstallStep::Done { .. } => {
            ui.label(text);
        }
        step => {
            let bar = egui::ProgressBar::new(step.fraction())
                .desired_width(PROGRESS_WIDTH)
                .text(text);
            ui.add(bar);
        }
    }
}

/// What the user did in the sites section.
enum SitesAction {
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
}

/// The sites section: the park and link buttons, the link form, the sites
/// table, and the folders that did not become a site. `picking`: a folder
/// dialog is open. Returns what the user asked for, if anything.
fn sites(ui: &mut egui::Ui, model: &mut Model, picking: bool) -> Option<SitesAction> {
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
    match &model.sites_notice {
        Some(SitesNotice::Failed(code)) => {
            ui.colored_label(ui.visuals().error_fg_color, strings::error_text(code));
        }
        Some(SitesNotice::Parked(folder)) => {
            ui.weak(strings::parked(folder));
        }
        Some(SitesNotice::Linked { name, folder }) => {
            ui.weak(strings::linked(name, folder));
        }
        None => {}
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
    let mut row_action = None;
    StripBuilder::new(ui)
        .size(Size::remainder().at_least(line * 2.0))
        .size(Size::exact(skipped_height))
        .vertical(|mut strip| {
            strip.cell(|ui| {
                if list.sites.is_empty() {
                    ui.weak(strings::NO_SITES);
                } else {
                    row_action = site_table(ui, view, connected);
                }
            });
            strip.cell(|ui| skipped(ui, &list.skipped));
        });
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
        None => {}
    }
    action
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
    let mut action = None;
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
        .column(Column::auto().at_least(PHP_COLUMN_WIDTH))
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
