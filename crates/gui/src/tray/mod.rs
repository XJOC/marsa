//! The menu-bar icon (task M5-04): a `tao` event loop on the main thread
//! (macOS requires it) that owns the icon and its menu, and a worker thread
//! with the daemon client ([`worker`]). The worker's updates and the menu's
//! clicks reach the loop through its `EventLoopProxy`. [`menu`] decides what
//! the menu shows; this module copies that onto the real menu items.
//!
//! On macOS and Windows a left click starts the panel ([`panel`], ADR 0022):
//! `marsa-gui --panel`, a borderless window under the icon. The right click
//! shows the menu. On Linux the menu is the whole UI: the default backend
//! sends no click events and no place for the icon (architecture.md section
//! 10), and Wayland cannot place a window. The icon starts `marsad` and stays
//! after the window closes; "Quit" closes the icon only.

pub mod icon;
pub mod menu;
pub mod panel;
pub mod worker;

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Instant;

use marsa_core::platform::spawn::sibling_program;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopWindowTarget};
use tray_icon::menu::{
    CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::panel::PanelExit;
use crate::{opener, strings};
use menu::{Item, Layout, State, Update};
use panel::{Monitor, Px, Slot, Toggle};

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
    /// A click on the icon itself, from the icon's own callback.
    Icon(TrayIconEvent),
}

/// What a click asks for.
enum Click {
    Open,
    /// A line of the PHP submenu, by its position.
    Php(usize),
    /// A site, by its position: it opens in the browser.
    Site(usize),
    Xdebug,
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
    TrayIconEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::Icon(event));
    }));
    let proxy = event_loop.create_proxy();
    let worker = worker::start(data_dir.clone(), move |update| {
        proxy.send_event(UserEvent::Update(update)).is_ok()
    })
    .map_err(TrayError::Worker)?;

    let mut state = State::default();
    // The one panel that may be open.
    let mut slot: Slot<Child> = Slot::new();
    // Debug builds only: the design review's click (see `selftest`).
    #[cfg(debug_assertions)]
    let (mut selftest, test_proxy) = (selftest_clicks(), event_loop.create_proxy());
    // Built once the loop runs: tray-icon on macOS wants that. Dropping it
    // would remove the icon from the menu bar.
    let mut items: Option<Items> = None;
    event_loop.run(move |event, target, control_flow| {
        // A panel that has ended asks something of the icon (its exit code).
        // Looked at on every wake; while one is open the loop wakes often.
        if let Some(exit) = slot.poll(Instant::now()) {
            if after_panel(exit, &mut state, &worker, &window, &data_dir) {
                *control_flow = ControlFlow::Exit;
                return;
            }
            if let Some(items) = &mut items {
                items.apply(&menu::layout(&state));
            }
        }
        match event {
            // The first event. `control_flow` keeps its value between events.
            Event::NewEvents(StartCause::Init) => match Items::build(&menu::layout(&state)) {
                Ok(built) => {
                    items = Some(built);
                    // Sleep until the next click or update: tao polls by
                    // default, which would keep a core busy.
                    *control_flow = ControlFlow::Wait;
                    // Debug builds only: wake once, to click the icon.
                    #[cfg(debug_assertions)]
                    if selftest > 0 {
                        *control_flow = ControlFlow::WaitUntil(Instant::now() + SELFTEST_WAIT);
                    }
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
            // Debug builds only: the click of the self-test, on the icon's
            // real place, as the system would have sent it.
            #[cfg(debug_assertions)]
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) if selftest > 0 => {
                selftest -= 1;
                if let Some(rect) = items.as_ref().and_then(Items::rect) {
                    let click = selftest_click(rect);
                    let _ = test_proxy.send_event(UserEvent::Icon(click));
                } else {
                    tracing::warn!("the self-test found no place for the icon");
                }
                if selftest > 0 {
                    *control_flow = ControlFlow::WaitUntil(Instant::now() + SELFTEST_GAP);
                }
            }
            // The left click: open the panel, or close the one that is open.
            Event::UserEvent(UserEvent::Icon(TrayIconEvent::Click {
                rect,
                button: MouseButton::Left,
                button_state,
                ..
            })) if acts_on(button_state) => {
                let toggle = slot.click(Instant::now());
                if toggle != Toggle::Open {
                    tracing::info!(?toggle, "a click on the icon with a panel to deal with");
                }
                if toggle == Toggle::Open {
                    let icon = Px {
                        x: rect.position.x,
                        y: rect.position.y,
                        width: f64::from(rect.size.width),
                        height: f64::from(rect.size.height),
                    };
                    let anchor = panel::anchor(icon, &monitors(target));
                    tracing::info!(?icon, ?anchor, "starting the panel");
                    let args = panel::args(&data_dir, anchor.as_ref());
                    match panel::spawn(&window, &args) {
                        Ok(child) => slot.start(child),
                        Err(err) => {
                            tracing::warn!(error = %err, "cannot start the panel");
                            state.apply(Update::Failed(strings::WINDOW_START_FAILED.to_owned()));
                            if let Some(items) = &mut items {
                                items.apply(&menu::layout(&state));
                            }
                        }
                    }
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
                    Some(Click::Site(index)) => {
                        let opened = state.site_url(index).map(opener::open_url);
                        if let Some(Err(code)) = opened {
                            state.apply(Update::Failed(code.to_owned()));
                        }
                    }
                    Some(Click::Xdebug) => {
                        if let Some(command) = state.toggle_xdebug() {
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
        // While a panel is open the loop wakes to see it end; otherwise it
        // sleeps until the next click or update. (The self-test's wake-up is
        // left to come.)
        #[cfg(debug_assertions)]
        let waiting_to_click = selftest > 0;
        #[cfg(not(debug_assertions))]
        let waiting_to_click = false;
        if !waiting_to_click
            && matches!(*control_flow, ControlFlow::Wait | ControlFlow::WaitUntil(_))
        {
            *control_flow = if slot.is_open() {
                ControlFlow::WaitUntil(Instant::now() + panel::POLL)
            } else {
                ControlFlow::Wait
            };
        }
    })
}

/// Does what a panel that has ended asked for. `true` when the icon quits.
fn after_panel(
    exit: PanelExit,
    state: &mut State,
    worker: &worker::Handle,
    window: &Path,
    data_dir: &Path,
) -> bool {
    match exit {
        PanelExit::Closed | PanelExit::FocusLost => {}
        PanelExit::OpenWindow => {
            if !open_window(window, data_dir) {
                state.apply(Update::Failed(strings::WINDOW_START_FAILED.to_owned()));
            }
        }
        PanelExit::StopMarsa => {
            if let Some(command) = state.stop() {
                worker.send(command);
            }
        }
        PanelExit::StartMarsa => {
            if let Some(command) = state.start() {
                worker.send(command);
            }
        }
        PanelExit::Quit => return true,
    }
    false
}

/// The monitors, the primary one first, in the physical pixels the system
/// reports.
fn monitors(target: &EventLoopWindowTarget<UserEvent>) -> Vec<Monitor> {
    let primary = target.primary_monitor();
    primary
        .into_iter()
        .chain(target.available_monitors())
        .map(|monitor| {
            let (at, size) = (monitor.position(), monitor.size());
            Monitor {
                rect: Px {
                    x: f64::from(at.x),
                    y: f64::from(at.y),
                    width: f64::from(size.width),
                    height: f64::from(size.height),
                },
                scale: monitor.scale_factor(),
            }
        })
        .collect()
}

/// Debug builds only: with this variable set to a number (`1` if it is not
/// one), the icon clicks itself that many times: the first three seconds after
/// it is up, then every second and a half, with the place the system gives
/// it. It starts the panel under the real icon, so the design review can see
/// where it lands, what its exit code asks for, and that a second click
/// closes it, with no one at the mouse (task M5-D11).
#[cfg(debug_assertions)]
const SELFTEST_VAR: &str = "MARSA_TRAY_SELFTEST";
#[cfg(debug_assertions)]
const SELFTEST_WAIT: std::time::Duration = std::time::Duration::from_secs(3);
#[cfg(debug_assertions)]
const SELFTEST_GAP: std::time::Duration = std::time::Duration::from_millis(1500);

/// How many times the self-test clicks: none unless it is asked for.
#[cfg(debug_assertions)]
fn selftest_clicks() -> u32 {
    std::env::var(SELFTEST_VAR)
        .ok()
        .map_or(0, |text| text.parse().unwrap_or(1))
}

/// The click of the self-test: the half that [`acts_on`] reads.
#[cfg(debug_assertions)]
fn selftest_click(rect: tray_icon::Rect) -> TrayIconEvent {
    let state = if acts_on(MouseButtonState::Down) {
        MouseButtonState::Down
    } else {
        MouseButtonState::Up
    };
    TrayIconEvent::Click {
        id: tray_icon::TrayIconId::new("selftest"),
        position: rect.position,
        rect,
        button: MouseButton::Left,
        button_state: state,
    }
}

/// Which half of a click starts the panel. macOS sends only the press of the
/// icon (no release), and acts on it at once; the other systems send both,
/// and a click is complete on the release (ADR 0022). Linux sends none. The
/// `cfg` is here and not in `platform` because it only names which of
/// tray-icon's two events to use.
#[cfg(target_os = "macos")]
const fn acts_on(state: MouseButtonState) -> bool {
    matches!(state, MouseButtonState::Down)
}

#[cfg(not(target_os = "macos"))]
const fn acts_on(state: MouseButtonState) -> bool {
    matches!(state, MouseButtonState::Up)
}

/// macOS and Windows: the left click starts the panel, so it no longer opens
/// the menu; the right click does. Linux keeps the click that opens the
/// menu, which is all there is there.
#[cfg(any(target_os = "macos", windows))]
fn with_click(builder: TrayIconBuilder) -> TrayIconBuilder {
    builder.with_menu_on_left_click(false)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn with_click(builder: TrayIconBuilder) -> TrayIconBuilder {
    builder
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
    /// Kept: dropping it removes the icon.
    _tray: TrayIcon,
    menu: Menu,
    status: MenuItem,
    notice: MenuItem,
    /// Whether `notice` is in the menu now.
    notice_shown: bool,
    /// The sites, between the first separator and `sites_end`.
    sites: Vec<MenuItem>,
    /// The line under the sites: they are put in before it.
    sites_end: PredefinedMenuItem,
    open: MenuItem,
    php: Submenu,
    php_versions: Vec<CheckMenuItem>,
    xdebug: CheckMenuItem,
    autostart: CheckMenuItem,
    power: MenuItem,
    quit: MenuItem,
}

impl Items {
    /// Where the icon is, if the system says (macOS and Windows).
    #[cfg(debug_assertions)]
    fn rect(&self) -> Option<tray_icon::Rect> {
        self._tray.rect()
    }

    fn build(layout: &Layout) -> Result<Self, TrayError> {
        let status = text_item(&layout.status);
        let notice = MenuItem::new("", false, None);
        let open = text_item(&layout.open);
        let php = Submenu::new(&layout.php.label, layout.php.enabled);
        let php_versions: Vec<CheckMenuItem> = layout.php_versions.iter().map(check_item).collect();
        for item in &php_versions {
            php.append(item).map_err(TrayError::Menu)?;
        }
        let xdebug = check_item(&layout.xdebug);
        let autostart = check_item(&layout.autostart);
        let power = text_item(&layout.power);
        let quit = text_item(&layout.quit);
        let sites: Vec<MenuItem> = layout.sites.iter().map(text_item).collect();
        let sites_end = PredefinedMenuItem::separator();
        let menu = Menu::new();
        menu.append(&status).map_err(TrayError::Menu)?;
        menu.append(&PredefinedMenuItem::separator())
            .map_err(TrayError::Menu)?;
        for site in &sites {
            menu.append(site).map_err(TrayError::Menu)?;
        }
        menu.append_items(&[
            &sites_end,
            &open,
            &PredefinedMenuItem::separator(),
            &php,
            &xdebug,
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
        let tray = with_icon(with_click(builder), icon)
            .build()
            .map_err(TrayError::Build)?;
        let mut items = Self {
            _tray: tray,
            menu,
            status,
            notice,
            notice_shown: false,
            sites,
            sites_end,
            open,
            php,
            php_versions,
            xdebug,
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
        self.set_sites(&layout.sites);
        set_text_item(&self.open, &layout.open);
        self.php.set_text(&layout.php.label);
        self.php.set_enabled(layout.php.enabled);
        self.set_versions(&layout.php_versions);
        set_check_item(&self.xdebug, &layout.xdebug);
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

    /// The site lines: put in again, before the line that ends them, when the
    /// sites changed; else only their enabled flags are set.
    fn set_sites(&mut self, sites: &[Item]) {
        let same = self.sites.len() == sites.len()
            && self
                .sites
                .iter()
                .zip(sites)
                .all(|(have, want)| have.text() == want.label);
        if same {
            for (item, want) in self.sites.iter().zip(sites) {
                set_text_item(item, want);
            }
            return;
        }
        for item in self.sites.drain(..) {
            if let Err(err) = self.menu.remove(&item) {
                tracing::warn!(error = %err, "cannot remove a site line");
            }
        }
        let end = self
            .menu
            .items()
            .iter()
            .position(|kind| kind.id() == self.sites_end.id());
        let Some(end) = end else {
            tracing::warn!("cannot find where the site lines go");
            return;
        };
        for (offset, want) in sites.iter().enumerate() {
            let item = text_item(want);
            match self.menu.insert(&item, end + offset) {
                Ok(()) => self.sites.push(item),
                Err(err) => tracing::warn!(error = %err, "cannot add a site line"),
            }
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
        } else if id == self.xdebug.id() {
            Some(Click::Xdebug)
        } else if id == self.autostart.id() {
            Some(Click::Autostart)
        } else if id == self.power.id() {
            Some(Click::Power)
        } else if id == self.quit.id() {
            Some(Click::Quit)
        } else if let Some(index) = self.sites.iter().position(|item| item.id() == id) {
            Some(Click::Site(index))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The icon acts on one half of a click: the press on macOS, which sends
    /// no release, and the release elsewhere. Never both, so one click is one
    /// panel.
    #[test]
    fn one_click_is_one_half_of_the_click() {
        let (down, up) = (MouseButtonState::Down, MouseButtonState::Up);
        assert_eq!(acts_on(down), cfg!(target_os = "macos"));
        assert_eq!(acts_on(up), !cfg!(target_os = "macos"));
        assert_ne!(acts_on(down), acts_on(up));
        assert!(!acts_on(MouseButtonState::Down) || !acts_on(MouseButtonState::Up));
    }
}
