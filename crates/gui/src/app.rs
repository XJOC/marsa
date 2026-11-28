//! The window's loop. egui is immediate mode: `ui` runs on every frame and
//! draws the whole window again from the [`Model`] through `shell`, like a
//! Blade view rendered again on every request. A frame runs only on input or
//! when the bridge sends an update; nothing here asks for continuous
//! repaints.
//!
//! Each frame: `logic` applies the bridge's updates, `shell::show` draws and
//! returns what the user did, and `ui` applies the actions and sends the
//! commands (code.md, State).

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Instant;

use eframe::egui;

use crate::bridge::{self, Command};
use crate::model::{Action, Connection, Effect, Model, Pick, Route, Update};
use crate::prefs::{Prefs, WindowSize};
use crate::review::ReviewAids;
use crate::{opener, shell, strings, theme};

pub struct App {
    model: Model,
    bridge: bridge::Handle,
    /// The window's own preferences, as `gui.toml` holds them.
    prefs: Prefs,
    /// Where `gui.toml` is.
    prefs_path: PathBuf,
    /// The size of the inside of the window in its last frame, while it was
    /// neither maximized, minimized, nor full screen: what `on_exit` saves.
    window: Option<egui::Vec2>,
    /// The folder dialog that is open, if any.
    picker: Option<Picker>,
    /// Set until the first frame, to log the time it took.
    started: Option<Instant>,
    /// The demo, the screenshot, the gallery, and the measurement: none of
    /// them is in a shipped binary (see `review`).
    aids: ReviewAids,
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
        prefs: Prefs,
        started: Instant,
    ) -> Self {
        // The fonts and the light and dark styles, once: egui then follows
        // the system's setting, or the Appearance the user chose.
        theme::style::apply(ctx);
        ctx.set_theme(prefs.appearance.preference());
        bridge.attach(ctx);
        let prefs_path = Prefs::path(&data_dir);
        let mut model = Model::new(data_dir);
        model.appearance = prefs.appearance;
        // Once: the window shows a folder under it as `~/…`.
        model.home = std::env::home_dir();
        ReviewAids::fill_demo(&mut model);
        model.route = ReviewAids::route().unwrap_or(Route::Home);
        Self {
            model,
            bridge,
            prefs,
            prefs_path,
            window: None,
            picker: None,
            started: Some(started),
            aids: ReviewAids::new(ctx),
        }
    }

    /// The window opens on `route` instead of Home.
    #[must_use]
    pub fn opening(mut self, route: Route) -> Self {
        self.model.route = route;
        self
    }

    /// The window in measurement mode: it shows made-up data and counts its
    /// frames (see `measure`).
    // Only with the `measure` feature (see the field).
    #[cfg(feature = "measure")]
    #[must_use]
    pub fn measuring(mut self, plan: crate::measure::Plan) -> Self {
        self.aids.measuring(&mut self.model, plan);
        self
    }

    /// Sends the next queued install once none of this window runs.
    fn send_next(&mut self) {
        if let Some(command) = self.model.next_command() {
            self.send(command);
        }
    }

    /// Sends `command` to the bridge; in measurement mode, its made-up
    /// source may answer it instead.
    fn send(&mut self, command: Command) {
        match self.aids.answer(&command) {
            Some(answer) => {
                if let Some(update) = answer {
                    self.model.update(update);
                }
            }
            None => self.bridge.send(self.model.session.number, command),
        }
    }

    /// Applies what the user did, and does what the model asks of the window.
    fn run(&mut self, ctx: &egui::Context, action: Action) {
        match self.model.apply_action(action) {
            Effect::None => {}
            Effect::Send(command) => self.send(command),
            Effect::OpenSite(name) => open_site(ctx, &self.model, &name),
            Effect::OpenFolder(folder) => {
                if let Err(code) = opener::open_folder(&folder) {
                    self.model.report(code);
                }
            }
            Effect::PickFolder(pick) => self.open_picker(ctx, pick),
            Effect::OpenUrl(url) => open_url(ctx, &url),
            Effect::CopyText(text) => ctx.copy_text(text),
        }
    }

    /// Opens the system's folder dialog for `pick` on its own thread.
    fn open_picker(&mut self, ctx: &egui::Context, pick: Pick) {
        if self.picker.is_some() {
            return;
        }
        let (send, folder) = mpsc::channel();
        let ctx = ctx.clone();
        let title = pick.title();
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

    /// Writes `gui.toml`. A failure is logged, never shown: the preference
    /// still holds until the window closes.
    fn save_prefs(&self) {
        if let Err(err) = self.prefs.save(&self.prefs_path) {
            tracing::warn!(error = %err, "cannot write gui.toml");
        }
    }

    /// Applies an Appearance the user chose: egui's theme at once, and the
    /// file, once per change.
    fn follow_appearance(&mut self, ctx: &egui::Context) {
        if self.model.appearance == self.prefs.appearance {
            return;
        }
        self.prefs.appearance = self.model.appearance;
        ctx.set_theme(self.prefs.appearance.preference());
        self.save_prefs();
    }

    /// Remembers the size of the window, for `on_exit`. A window that is
    /// maximized, minimized, or full screen has a size that is not its own.
    fn note_window(&mut self, ctx: &egui::Context) {
        let size = ctx.input(|input| {
            let viewport = input.viewport();
            let plain = [viewport.maximized, viewport.minimized, viewport.fullscreen]
                .iter()
                .all(|state| *state != Some(true));
            // Wayland gives no `inner_rect`: the content area is the window.
            let size = viewport
                .inner_rect
                .map_or_else(|| input.content_rect().size(), |rect| rect.size());
            plain.then_some(size)
        });
        if size.is_some() {
            self.window = size;
        }
    }

    /// The size the window closed at, if it is not the one in `gui.toml`.
    fn size_to_save(&self) -> Option<WindowSize> {
        let size = WindowSize::of(self.window?);
        (self.prefs.window != Some(size)).then_some(size)
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
        let effect = self.model.picked(pick, &folder);
        if let Effect::Send(command) = effect {
            self.send(command);
        }
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.aids.inject(raw_input);
    }

    /// The window closes: its size goes to `gui.toml` (once, and only when
    /// it changed).
    fn on_exit(&mut self) {
        if let Some(size) = self.size_to_save() {
            self.prefs.window = Some(size);
            self.save_prefs();
        }
    }

    /// Takes every waiting update before the frame is drawn.
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        take_updates(&self.bridge, &mut self.model);
        self.take_pick();
        // An install that ended may start the next one.
        self.send_next();
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if let Some(started) = self.started.take() {
            let ms = started.elapsed().as_millis();
            tracing::debug!(ms, "first frame");
            self.aids.first_frame(ms);
        }
        // What building the window takes, for the measurement mode.
        let building = Instant::now();
        if self.aids.show_gallery(ui) {
            return;
        }
        if let Some(action) = self.aids.wanted() {
            self.run(ui.ctx(), action);
        }
        if !self.aids.show_editor(ui) {
            let picking = self.picker.is_some();
            let output = shell::show(ui, &self.model, picking);
            for action in output.actions {
                self.run(ui.ctx(), action);
            }
        }
        let cpu = frame.info().cpu_usage;
        let connected = self.model.is_connected();
        self.aids
            .frame_done(ui.ctx(), connected, building.elapsed(), cpu);
        self.follow_appearance(ui.ctx());
        self.note_window(ui.ctx());
    }
}

/// Applies every update the bridge has sent, before a frame is drawn. A
/// bridge that has stopped is a failed connection, said once.
pub(crate) fn take_updates(bridge: &bridge::Handle, model: &mut Model) {
    loop {
        match bridge.updates.try_recv() {
            Ok(update) => model.receive(update),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                let failed = Connection::Failed(strings::BRIDGE_FAILED.to_owned());
                if model.session.connection != failed {
                    model.update(Update::Failed(strings::BRIDGE_FAILED.to_owned()));
                }
                break;
            }
        }
    }
}

/// Opens the site `name` in the browser: a local action, done by the system
/// and never by a screen (code.md, State). The window and the menu-bar panel
/// both use it.
pub(crate) fn open_site(ctx: &egui::Context, model: &Model, name: &str) {
    let Some(url) = model.site_url(name) else {
        return;
    };
    if ReviewAids::is_demo() {
        tracing::info!(%url, "demo: a site would open in the browser");
        return;
    }
    ctx.open_url(egui::OpenUrl::new_tab(url));
}

/// Opens a page of Mailpit's in the browser, as [`open_site`] does a site.
fn open_url(ctx: &egui::Context, url: &str) {
    if ReviewAids::is_demo() {
        tracing::info!(%url, "demo: a page would open in the browser");
        return;
    }
    ctx.open_url(egui::OpenUrl::new_tab(url));
}

#[cfg(test)]
mod tests {
    use eframe::App as _;

    use super::*;
    use crate::prefs::Appearance;
    use crate::widgets::harness::Harness;

    /// A size in points, from whole numbers.
    fn size(width: u32, height: u32) -> egui::Vec2 {
        WindowSize { width, height }.points()
    }

    /// A window with no daemon, whose preferences are in `data_dir`.
    fn app(ctx: &egui::Context, data_dir: &std::path::Path, prefs: Prefs) -> App {
        App::new(
            ctx,
            bridge::Handle::detached(),
            data_dir.to_path_buf(),
            prefs,
            Instant::now(),
        )
    }

    // Only with the `measure` feature, as the mode is.
    #[cfg(feature = "measure")]
    #[test]
    fn the_measuring_window_shows_made_up_sites_and_counts_its_frames() {
        use crate::measure::Plan;
        let dir = tempfile::tempdir().expect("temp dir");
        let ctx = egui::Context::default();
        let plan = Plan {
            sites: 12,
            logs: 0,
            dumps: 0,
            idle: std::time::Duration::from_secs(1),
            scroll: 0,
            sql: 0,
            typing: 0,
            table: 0,
        };
        let measured = app(&ctx, dir.path(), Prefs::default()).measuring(plan);
        assert!(measured.model.is_connected());
        assert_eq!(measured.model.session.sites.sites.len(), 12);
        assert_eq!(measured.model.route, Route::Sites);
        assert!(measured.aids.measure().is_some());
        // Not measuring: no sites, and no count.
        let plain = app(&ctx, dir.path(), Prefs::default());
        assert!(plain.aids.measure().is_none() && plain.model.session.sites.sites.is_empty());
    }

    #[test]
    fn the_window_opens_on_home_or_on_the_screen_it_was_asked_for() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ctx = egui::Context::default();
        let plain = app(&ctx, dir.path(), Prefs::default());
        assert_eq!(plain.model.route, Route::Home);
        let sites = app(&ctx, dir.path(), Prefs::default()).opening(Route::Sites);
        assert_eq!(sites.model.route, Route::Sites);
    }

    #[test]
    fn the_saved_appearance_is_what_the_window_starts_with() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ctx = egui::Context::default();
        let prefs = Prefs {
            appearance: Appearance::Dark,
            window: None,
        };
        let app = app(&ctx, dir.path(), prefs);
        assert_eq!(app.model.appearance, Appearance::Dark);
        assert_eq!(ctx.theme(), egui::Theme::Dark);
        // Opening the window writes nothing.
        assert!(!Prefs::path(dir.path()).exists());
    }

    #[test]
    fn a_new_appearance_applies_at_once_and_is_written_once() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = Prefs::path(dir.path());
        let ctx = egui::Context::default();
        let mut app = app(&ctx, dir.path(), Prefs::default());
        app.follow_appearance(&ctx);
        assert!(!path.exists(), "nothing changed, nothing is written");
        app.model
            .apply_action(Action::SetAppearance(Appearance::Light));
        app.follow_appearance(&ctx);
        assert_eq!(ctx.theme(), egui::Theme::Light);
        assert_eq!(Prefs::load(&path).appearance, Appearance::Light);
        // The file is not written again for the same choice.
        std::fs::remove_file(&path).expect("removed");
        app.follow_appearance(&ctx);
        assert!(!path.exists());
    }

    #[test]
    fn the_size_is_saved_when_the_window_closes_and_only_if_it_changed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = Prefs::path(dir.path());
        let ctx = egui::Context::default();
        let mut app = app(&ctx, dir.path(), Prefs::default());
        // No frame has told the size yet: nothing to save.
        app.on_exit();
        assert!(!path.exists());
        app.window = Some(size(900, 600));
        app.on_exit();
        let saved = Prefs::load(&path);
        assert_eq!(saved.window.map(WindowSize::points), Some(size(900, 600)));
        assert_eq!(saved.appearance, Appearance::System);
        // The same size again writes nothing.
        std::fs::remove_file(&path).expect("removed");
        app.on_exit();
        assert!(!path.exists());
        // A window smaller than the smallest saves the smallest.
        app.window = Some(size(10, 10));
        app.on_exit();
        assert_eq!(
            Prefs::load(&path).window_size(),
            crate::theme::SIZE.window_min
        );
    }

    #[test]
    fn the_size_comes_from_the_frames_and_not_from_a_maximized_window() {
        let dir = tempfile::tempdir().expect("temp dir");
        let harness = Harness::new(egui::Theme::Light);
        let ctx = harness.ctx.clone();
        let mut app = app(&ctx, dir.path(), Prefs::default());
        let frame = |maximized: bool, size: egui::Vec2| {
            let viewport = egui::ViewportInfo {
                inner_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                maximized: Some(maximized),
                ..egui::ViewportInfo::default()
            };
            let mut input = egui::RawInput::default();
            input.viewports.insert(input.viewport_id, viewport);
            harness.frame(input, |_| {});
        };
        frame(false, size(1000, 650));
        app.note_window(&ctx);
        assert_eq!(app.window, Some(size(1000, 650)));
        // Maximized: the size is the screen's, not the window's own.
        frame(true, size(1800, 1000));
        app.note_window(&ctx);
        assert_eq!(app.window, Some(size(1000, 650)));
    }
}
