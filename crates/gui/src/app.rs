//! Drawing. egui is immediate mode: `ui` runs on every frame and draws the
//! whole window again from the [`Model`], like a Blade view rendered again on
//! every request. A frame runs only on input or when the bridge sends an
//! update; nothing here asks for continuous repaints.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Instant;

use eframe::egui;

use crate::bridge::{self, Command};
use crate::model::{Connection, Model, Pick, Removal, Update};
use crate::screens::{self, sites::SitesAction};
use crate::strings;
use crate::theme;
use crate::theme::page::{CONFIRM_WIDTH, LOG_HEIGHT};

pub struct App {
    model: Model,
    bridge: bridge::Handle,
    /// The folder dialog that is open, if any.
    picker: Option<Picker>,
    /// Set until the first frame, to log the time it took.
    started: Option<Instant>,
    /// The picture of the window that `MARSA_GUI_SCREENSHOT` asks for.
    // Debug builds only: a design-review aid (see `screenshot`).
    #[cfg(debug_assertions)]
    screenshot: Option<crate::screenshot::Screenshot>,
    /// `MARSA_GUI_GALLERY=1`: the window shows the widget gallery instead of
    /// the screens.
    // Debug builds only: a design-review aid (see `widgets::gallery`).
    #[cfg(debug_assertions)]
    gallery: bool,
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
        // The fonts and the light and dark styles, once: egui then follows
        // the system's setting.
        theme::style::apply(ctx);
        bridge.attach(ctx);
        Self {
            model: Model::new(data_dir),
            bridge,
            picker: None,
            started: Some(started),
            // Debug builds only: a design-review aid (see `screenshot`).
            #[cfg(debug_assertions)]
            screenshot: crate::screenshot::Screenshot::start(ctx),
            // Debug builds only: a design-review aid (see `widgets::gallery`).
            #[cfg(debug_assertions)]
            gallery: crate::widgets::gallery::requested(),
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
        // Debug builds only: the widget gallery replaces the screens.
        #[cfg(debug_assertions)]
        if self.gallery {
            crate::widgets::gallery::show(ui);
            if let Some(screenshot) = &mut self.screenshot {
                // The gallery needs no daemon: it is ready at once.
                screenshot.frame(ui.ctx(), true);
            }
            return;
        }
        let mut commands = Vec::new();
        let mut pick = None;
        egui::Panel::top("status").show(ui, |ui| {
            commands.extend(screens::home::show(ui, &mut self.model));
        });
        if self.model.first_run.is_some() && self.model.is_connected() {
            egui::Panel::top("first_run").show(ui, |ui| {
                commands.extend(screens::first_run::show(ui, &mut self.model));
            });
        }
        egui::Panel::top("services").show(ui, |ui| {
            if let Some(command) = screens::services::show(ui, &self.model) {
                // The notice was about the last action.
                self.model.notice = None;
                commands.push(command);
            }
        });
        egui::Panel::top("runtimes").show(ui, |ui| {
            screens::runtimes::show(ui, &mut self.model, &mut commands);
        });
        egui::Panel::bottom("log")
            .resizable(true)
            .default_size(LOG_HEIGHT)
            .show(ui, |ui| screens::logs::show(ui, &self.model));
        // The sites get the space that is left: their list grows with the
        // user's projects.
        let picking = self.picker.is_some();
        egui::CentralPanel::default().show(ui, |ui| {
            match screens::sites::show(ui, &mut self.model, picking) {
                Some(SitesAction::Send(command)) => commands.push(command),
                Some(SitesAction::Pick(chosen)) => pick = Some(chosen),
                None => {}
            }
        });
        // Opened by an Uninstall, Unlink, or Forget button above.
        let ctx = ui.ctx().clone();
        commands.extend(confirm(&ctx, &mut self.model));
        for command in commands {
            self.bridge.send(command);
        }
        if let Some(pick) = pick {
            let ctx = ui.ctx().clone();
            self.open_picker(&ctx, pick);
        }
        // A button may have queued an install.
        self.send_next_install();
        // Debug builds only: a design-review aid (see `screenshot`).
        #[cfg(debug_assertions)]
        if let Some(screenshot) = &mut self.screenshot {
            screenshot.frame(ui.ctx(), self.model.is_connected());
        }
    }
}

/// The confirmation of an Uninstall, Unlink, or Forget, over the whole
/// window. Escape or a click beside it cancels. Returns the removal to send
/// once the user confirms.
fn confirm(ctx: &egui::Context, model: &mut Model) -> Option<Command> {
    let removal = model.confirm.clone()?;
    let text = match &removal {
        Removal::Unlink { site } => strings::confirm_text(&removal, model.site_root(site), 0),
        Removal::Forget { folder } => strings::confirm_text(&removal, None, model.sites_in(folder)),
        Removal::Uninstall { .. } => strings::confirm_text(&removal, None, 0),
    };
    let mut confirmed = false;
    let mut cancelled = false;
    let modal = egui::Modal::new(egui::Id::unique("confirm_removal")).show(ctx, |ui| {
        ui.set_max_width(CONFIRM_WIDTH);
        ui.strong(strings::confirm_title(&removal));
        ui.label(text);
        ui.horizontal(|ui| {
            confirmed = ui.button(strings::removal_button(&removal)).clicked();
            cancelled = ui.button(strings::CANCEL).clicked();
        });
    });
    if confirmed {
        return model.confirm_removal().map(Command::Remove);
    }
    if cancelled || modal.should_close() {
        model.cancel_removal();
    }
    None
}
