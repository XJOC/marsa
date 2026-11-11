//! The menu-bar icon (task M5-04): a `tao` event loop on the main thread
//! (macOS requires it) that owns the icon and its menu, and a worker thread
//! with the daemon client ([`worker`]). The worker's updates and the menu's
//! clicks reach the loop through its `EventLoopProxy`. [`menu`] decides what
//! the menu shows; this module copies that onto the real menu items.
//!
//! The menu is the whole UI: Linux sends no click events for the icon
//! (architecture.md section 10), so none are used. The icon starts `marsad`
//! and stays after the window closes; "Quit" closes the icon only.

pub mod icon;
pub mod menu;
pub mod worker;

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use marsa_core::platform::spawn::sibling_program;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder};
use tray_icon::menu::{
    CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::strings;
use menu::{Item, Layout, State, Update};

/// The window program, next to the tray.
const WINDOW_PROGRAM: &str = "marsa-gui";

/// Why the icon cannot run.
#[derive(Debug, thiserror::Error)]
pub enum TrayError {
    #[error("cannot find the window program next to the tray")]
    WindowProgram(#[source] io::Error),
    #[error("cannot start the worker thread")]
    Worker(#[source] io::Error),
    #[error("cannot draw the icon")]
    Icon(#[source] tray_icon::BadIcon),
    #[error("cannot build the menu")]
    Menu(#[source] tray_icon::menu::Error),
    #[error("cannot put the icon in the menu bar")]
    Build(#[source] tray_icon::Error),
}

/// What wakes the event loop.
enum UserEvent {
    /// From the worker thread.
    Update(Update),
    /// A click on a menu line, from the menu's own callback.
    Menu(MenuEvent),
}

/// What a click asks for.
enum Click {
    Open,
    /// A line of the PHP submenu, by its position.
    Php(usize),
    Autostart,
    Power,
    Quit,
}

/// Runs the icon until "Quit", which ends the process.
///
/// # Errors
///
/// `marsa-gui` cannot be located, or the worker thread cannot start. A menu
/// or icon that cannot be built ends the process with exit code 1 instead:
/// the event loop is running by then.
pub fn run(data_dir: PathBuf) -> Result<(), TrayError> {
    let window = sibling_program(WINDOW_PROGRAM).map_err(TrayError::WindowProgram)?;
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    platform_setup(&mut event_loop);

    // Clicks arrive on the menu's callback; the proxy wakes the event loop.
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        // Fails only when the event loop is gone, that is while quitting.
        let _ = proxy.send_event(UserEvent::Menu(event));
    }));
    let proxy = event_loop.create_proxy();
    let worker = worker::start(data_dir.clone(), move |update| {
        proxy.send_event(UserEvent::Update(update)).is_ok()
    })
    .map_err(TrayError::Worker)?;

    let mut state = State::default();
    // Built once the loop runs: tray-icon on macOS wants that. Dropping it
    // would remove the icon from the menu bar.
    let mut items: Option<Items> = None;
    event_loop.run(move |event, _target, control_flow| {
        match event {
            // The first event. `control_flow` keeps its value between events.
            Event::NewEvents(StartCause::Init) => match Items::build(&menu::layout(&state)) {
                Ok(built) => {
                    items = Some(built);
                    // Sleep until the next click or update: tao polls by
                    // default, which would keep a core busy.
                    *control_flow = ControlFlow::Wait;
                }
                Err(err) => {
                    tracing::error!(
                        error = format!("{err:#}"),
                        "cannot create the menu-bar icon"
                    );
                    *control_flow = ControlFlow::ExitWithCode(1);
                }
            },
            Event::UserEvent(UserEvent::Update(update)) => {
                state.apply(update);
                if let Some(items) = &mut items {
                    items.apply(&menu::layout(&state));
                }
            }
            Event::UserEvent(UserEvent::Menu(click)) => {
                let Some(items) = &mut items else {
                    return;
                };
                match items.clicked(&click.id) {
                    Some(Click::Open) => {
                        if !open_window(&window, &data_dir) {
                            state.apply(Update::Failed(strings::WINDOW_START_FAILED.to_owned()));
                        }
                    }
                    Some(Click::Php(index)) => {
                        if let Some(command) = state.pick_php(index) {
                            worker.send(command);
                        }
                    }
                    Some(Click::Autostart) => {
                        if let Some(command) = state.toggle_autostart() {
                            worker.send(command);
                        }
                    }
                    Some(Click::Power) => {
                        if let Some(command) = state.power() {
                            worker.send(command);
                        }
                    }
                    Some(Click::Quit) => {
                        *control_flow = ControlFlow::Exit;
                        return;
                    }
                    None => return,
                }
                items.apply(&menu::layout(&state));
            }
            _ => {}
        }
    })
}

/// macOS only: a menu-bar program has no Dock icon and no app menu
/// (`Accessory`), and its icon is a template image that the system recolours
/// for a light or dark menu bar. Both APIs exist only on macOS, hence the
/// `cfg` outside `platform` (AGENTS.md section 6).
#[cfg(target_os = "macos")]
fn platform_setup(event_loop: &mut EventLoop<UserEvent>) {
    use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};

    event_loop.set_activation_policy(ActivationPolicy::Accessory);
}

#[cfg(not(target_os = "macos"))]
fn platform_setup(_event_loop: &mut EventLoop<UserEvent>) {}

/// See [`platform_setup`].
#[cfg(target_os = "macos")]
fn with_icon(builder: TrayIconBuilder, icon: Icon) -> TrayIconBuilder {
    builder.with_icon_templated(icon)
}

#[cfg(not(target_os = "macos"))]
fn with_icon(builder: TrayIconBuilder, icon: Icon) -> TrayIconBuilder {
    builder.with_icon(icon)
}

/// The real menu lines, kept so each update changes them in place.
struct Items {
    _tray: TrayIcon,
    menu: Menu,
    status: MenuItem,
    notice: MenuItem,
    /// Whether `notice` is in the menu now.
    notice_shown: bool,
    open: MenuItem,
    php: Submenu,
    php_versions: Vec<CheckMenuItem>,
    autostart: CheckMenuItem,
    power: MenuItem,
    quit: MenuItem,
}

impl Items {
    fn build(layout: &Layout) -> Result<Self, TrayError> {
        let status = text_item(&layout.status);
        let notice = MenuItem::new("", false, None);
        let open = text_item(&layout.open);
        let php = Submenu::new(&layout.php.label, layout.php.enabled);
        let php_versions: Vec<CheckMenuItem> = layout.php_versions.iter().map(check_item).collect();
        for item in &php_versions {
            php.append(item).map_err(TrayError::Menu)?;
        }
        let autostart = check_item(&layout.autostart);
        let power = text_item(&layout.power);
        let quit = text_item(&layout.quit);
        let menu = Menu::new();
        menu.append_items(&[
            &status,
            &PredefinedMenuItem::separator(),
            &open,
            &PredefinedMenuItem::separator(),
            &php,
            &autostart,
            &PredefinedMenuItem::separator(),
            &power,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(TrayError::Menu)?;
        let icon =
            Icon::from_rgba(icon::rgba(), icon::SIZE, icon::SIZE).map_err(TrayError::Icon)?;
        let builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu.clone()))
            .with_tooltip(strings::TRAY_TOOLTIP);
        let tray = with_icon(builder, icon).build().map_err(TrayError::Build)?;
        let mut items = Self {
            _tray: tray,
            menu,
            status,
            notice,
            notice_shown: false,
            open,
            php,
            php_versions,
            autostart,
            power,
            quit,
        };
        items.show_notice(layout.notice.as_ref());
        Ok(items)
    }

    /// Copies `layout` onto the lines.
    fn apply(&mut self, layout: &Layout) {
        set_text_item(&self.status, &layout.status);
        self.show_notice(layout.notice.as_ref());
        set_text_item(&self.open, &layout.open);
        self.php.set_text(&layout.php.label);
        self.php.set_enabled(layout.php.enabled);
        self.set_versions(&layout.php_versions);
        set_check_item(&self.autostart, &layout.autostart);
        set_text_item(&self.power, &layout.power);
        set_text_item(&self.quit, &layout.quit);
    }

    /// Puts the notice line under the status line, or takes it out.
    fn show_notice(&mut self, notice: Option<&Item>) {
        match (notice, self.notice_shown) {
            (Some(item), shown) => {
                self.notice.set_text(&item.label);
                if !shown {
                    match self.menu.insert(&self.notice, 1) {
                        Ok(()) => self.notice_shown = true,
                        Err(err) => tracing::warn!(error = %err, "cannot add the notice line"),
                    }
                }
            }
            (None, true) => match self.menu.remove(&self.notice) {
                Ok(()) => self.notice_shown = false,
                Err(err) => tracing::warn!(error = %err, "cannot remove the notice line"),
            },
            (None, false) => {}
        }
    }

    /// The version lines: built again when the versions changed, else only
    /// their marks and enabled flags are set.
    fn set_versions(&mut self, versions: &[Item]) {
        let same = self.php_versions.len() == versions.len()
            && self
                .php_versions
                .iter()
                .zip(versions)
                .all(|(have, want)| have.text() == want.label);
        if same {
            for (item, want) in self.php_versions.iter().zip(versions) {
                set_check_item(item, want);
            }
            return;
        }
        for item in self.php_versions.drain(..) {
            if let Err(err) = self.php.remove(&item) {
                tracing::warn!(error = %err, "cannot remove a PHP line");
            }
        }
        for want in versions {
            let item = check_item(want);
            match self.php.append(&item) {
                Ok(()) => self.php_versions.push(item),
                Err(err) => tracing::warn!(error = %err, "cannot add a PHP line"),
            }
        }
    }

    /// The line with this id, if it is one a click does something with.
    fn clicked(&self, id: &MenuId) -> Option<Click> {
        if id == self.open.id() {
            Some(Click::Open)
        } else if id == self.autostart.id() {
            Some(Click::Autostart)
        } else if id == self.power.id() {
            Some(Click::Power)
        } else if id == self.quit.id() {
            Some(Click::Quit)
        } else {
            self.php_versions
                .iter()
                .position(|item| item.id() == id)
                .map(Click::Php)
        }
    }
}

fn text_item(item: &Item) -> MenuItem {
    MenuItem::new(&item.label, item.enabled, None)
}

fn check_item(item: &Item) -> CheckMenuItem {
    CheckMenuItem::new(&item.label, item.enabled, item.checked == Some(true), None)
}

fn set_text_item(line: &MenuItem, item: &Item) {
    line.set_text(&item.label);
    line.set_enabled(item.enabled);
}

fn set_check_item(line: &CheckMenuItem, item: &Item) {
    line.set_text(&item.label);
    line.set_enabled(item.enabled);
    line.set_checked(item.checked == Some(true));
}

/// Starts the window as its own process, with the same data folder. A second
/// window finds the window lock taken and exits at once
/// (`startup::window_lock`). The exit status is collected on a small thread,
/// so the window never stays a zombie. `false` if it could not be started.
fn open_window(program: &Path, data_dir: &Path) -> bool {
    let spawned = Command::new(program)
        .arg("--data-dir")
        .arg(data_dir)
        .stdin(Stdio::null())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(err) => {
            tracing::warn!(error = %err, program = %program.display(), "cannot start the window");
            return false;
        }
    };
    tracing::info!(pid = child.id(), "window started");
    let reaper = thread::Builder::new()
        .name("marsa-gui-reaper".to_owned())
        .stack_size(64 * 1024)
        .spawn(move || {
            // The status is not needed, only its collection.
            let _ = child.wait();
        });
    if let Err(err) = reaper {
        // The `Child` is dropped, which does not stop the window.
        tracing::debug!(error = %err, "cannot start a thread to reap the window");
    }
    true
}
