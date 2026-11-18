//! The window's loop. egui is immediate mode: `ui` runs on every frame and
//! draws the whole window again from the [`Model`] through `shell`, like a
//! Blade view rendered again on every request. A frame runs only on input or
//! when the bridge sends an update; nothing here asks for continuous
//! repaints.
//!
//! Each frame: `logic` applies the bridge's updates, `shell::show` draws and
//! returns what the user did, and `ui` applies the actions and sends the
//! commands (code.md, State).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Instant;

use eframe::egui;

use crate::bridge::{self, Command};
use crate::model::{Action, Connection, Model, Pick, Route, Update};
use crate::{opener, shell, strings, theme};

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
        let mut model = Model::new(data_dir);
        // Once: the window shows a folder under it as `~/…`.
        model.home = std::env::home_dir();
        // Debug builds only: fixed data for the design review (see `demo`).
        #[cfg(debug_assertions)]
        if let Some(demo) = crate::demo::requested() {
            crate::demo::fill(&mut model, demo);
        }
        model.route = first_route();
        Self {
            model,
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
    // Debug builds only: the design review's click (see `screenshot`).
    #[cfg(debug_assertions)]
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(screenshot) = &mut self.screenshot {
            screenshot.inject(raw_input);
        }
    }

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
        let picking = self.picker.is_some();
        let output = shell::show(ui, &mut self.model, picking);
        for action in output.actions {
            // Local actions: the system opens the site, a folder, and the folder
            // dialog, not a screen (code.md, State).
            match &action {
                Action::OpenSite(name) => {
                    if let Some(url) = self.model.site_url(name) {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                    }
                    continue;
                }
                Action::OpenFolder(name) => {
                    let folder = self.model.site_root(name).map(Path::to_path_buf);
                    if let Some(Err(code)) = folder.as_deref().map(opener::open_folder) {
                        self.model.notice = Some(code.to_owned());
                    }
                    continue;
                }
                Action::PickFolder(pick) => {
                    let ctx = ui.ctx().clone();
                    self.open_picker(&ctx, *pick);
                    continue;
                }
                _ => {}
            }
            if let Some(command) = self.model.apply_action(action) {
                self.bridge.send(command);
            }
        }
        for command in output.commands {
            self.bridge.send(command);
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

/// The screen the window opens on: Home.
fn first_route() -> Route {
    // Debug builds only: `MARSA_GUI_ROUTE` opens another screen for the
    // design-review pictures (see `screenshot`).
    #[cfg(debug_assertions)]
    if let Some(route) = crate::screenshot::route() {
        return route;
    }
    Route::Home
}
