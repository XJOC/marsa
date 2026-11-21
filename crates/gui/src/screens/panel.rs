//! The menu-bar panel (components.md, Menu-bar panel; ADR 0022): the status,
//! up to nine sites that the keys 1 to 9 open, the default PHP as medium
//! version buttons, the Xdebug switch, and a footer with "Stop Marsa" and
//! "Quit". It is drawn in its own borderless window, `marsa-gui --panel`
//! (`crate::panel`), from the same [`Model`] as the window.
//!
//! The screen is two functions, as the others are. [`view`] reads the model
//! and decides what the panel says: pure, so the tests assert it without a
//! window. [`show_limited`] draws that [`PanelView`] and returns what the user did as
//! [`PanelAction`]s. The panel shows only what the window already knows how
//! to do: a site opens in the browser, a version is saved as the default,
//! Xdebug flips. The footer asks the tray, by the way the panel exits
//! ([`PanelExit`]).

use eframe::egui::{Align2, Key, Label, Margin, Rect, RichText, ScrollArea, Ui, vec2};

use super::home::{self, DefaultPhp};
use super::sites::{Mark, mute, status_line};
use crate::model::NoticeSource;
use crate::model::{Action, Connection, MAIN_RUNTIME, Model};
use crate::panel::{MAX_SITES, PanelExit};
use crate::shell::sidebar;
use crate::strings;
use crate::theme::{Direction, Theme, UNIT};
use crate::widgets::panel::{
    FooterLink, SiteRow, SwitchRow, divider, footer_divider, footer_link_height, frame,
};
use crate::widgets::{Button, Health, PickerSize, StatusDot, VersionPick, VersionPicker, whole};

/// How many version buttons stand in a row of the panel.
const VERSION_COLUMNS: usize = 4;

/// The keys that open the sites, in order: the first opens the first site.
const DIGITS: [Key; MAX_SITES] = [
    Key::Num1,
    Key::Num2,
    Key::Num3,
    Key::Num4,
    Key::Num5,
    Key::Num6,
    Key::Num7,
    Key::Num8,
    Key::Num9,
];

/// What the user did in the panel.
#[derive(Debug, PartialEq)]
pub enum PanelAction {
    /// What the window does too: [`Action::OpenSite`], [`Action::SetDefault`],
    /// and [`Action::ToggleXdebug`]. `PanelApp` applies it like the window.
    Do(Action),
    /// The panel closes, and the tray reads why.
    Exit(PanelExit),
}

/// What the panel says.
#[derive(Debug, PartialEq)]
pub struct PanelView {
    pub status: StatusLine,
    /// `None` while there is no daemon to ask.
    pub sites: Option<SitesView>,
    /// `None` while there is no daemon, or no PHP installed.
    pub php: Option<DefaultPhp>,
    /// `None` while there is no daemon to ask.
    pub xdebug: Option<XdebugLine>,
    /// "Stop Marsa", or "Start Marsa" when it is not running.
    pub power: Power,
}

/// The status dot with its word: the sidebar's status, in the same words.
#[derive(Debug, PartialEq)]
pub struct StatusLine {
    pub health: Health,
    pub title: &'static str,
    pub caption: &'static str,
}

/// The sites, at most [`MAX_SITES`].
#[derive(Debug, PartialEq)]
pub struct SitesView {
    /// In the order of the window's list; the first has the key 1.
    pub rows: Vec<SiteLine>,
    /// How many sites there are past the ninth.
    pub more: usize,
}

#[derive(Debug, PartialEq)]
pub struct SiteLine {
    /// 1 to 9: the key that opens it.
    pub key: usize,
    pub name: String,
    /// `PHP 8.4`, or a dash for a site with no PHP.
    pub php: String,
    /// What a screen reader says: "Open shop".
    pub label: String,
}

#[derive(Debug, PartialEq)]
pub struct XdebugLine {
    pub on: bool,
    /// Why the switch cannot be pressed now.
    pub off: Option<&'static str>,
    /// Why the last read or change failed.
    pub error: Option<String>,
}

/// The first link of the footer.
#[derive(Debug, PartialEq)]
pub struct Power {
    pub label: &'static str,
    /// What the click asks of the tray.
    pub exit: PanelExit,
    /// Why it cannot be pressed now.
    pub off: Option<&'static str>,
}

/// Decides what the panel says. Pure: the same model gives the same view.
pub fn view(model: &Model) -> PanelView {
    let overall = model.overall();
    let (title, caption) = strings::overall_text(overall);
    let connected = model.is_connected();
    PanelView {
        status: StatusLine {
            health: sidebar::health(overall),
            title,
            caption,
        },
        sites: connected.then(|| sites(model)),
        php: connected
            .then(|| home::default_php(model))
            .filter(|php| !php.choices.is_empty()),
        xdebug: connected.then(|| xdebug(model)),
        power: power(model),
    }
}

fn sites(model: &Model) -> SitesView {
    let all = &model.session.sites.sites;
    let rows = all
        .iter()
        .take(MAX_SITES)
        .enumerate()
        .map(|(index, site)| SiteLine {
            key: index + 1,
            name: site.name.clone(),
            php: site.php.as_ref().map_or_else(
                || strings::NO_PHP.to_owned(),
                |version| strings::panel_php(&home::minor(version)),
            ),
            label: strings::panel_open_site(&site.name),
        })
        .collect();
    SitesView {
        rows,
        more: all.len().saturating_sub(MAX_SITES),
    }
}

fn xdebug(model: &Model) -> XdebugLine {
    let off = if model.session.xdebug.is_none() {
        Some(strings::XDEBUG_UNREAD)
    } else if model.xdebug_pending().is_some() {
        Some(strings::XDEBUG_BUSY)
    } else {
        None
    };
    XdebugLine {
        on: model.xdebug_on().unwrap_or(false),
        off,
        error: model
            .session
            .notices
            .code(NoticeSource::Xdebug)
            .map(strings::error_text),
    }
}

fn power(model: &Model) -> Power {
    let stop = |off| Power {
        label: strings::TRAY_STOP,
        exit: PanelExit::StopMarsa,
        off,
    };
    match &model.session.connection {
        Connection::Running(_) => stop(None),
        Connection::Connecting | Connection::Reconnecting => {
            stop(Some(strings::PANEL_NOT_CONNECTED))
        }
        Connection::ShuttingDown => stop(Some(strings::PANEL_ALREADY_STOPPING)),
        // No daemon: there is nothing to stop, and starting it is the one
        // thing to offer.
        Connection::Failed(_) => Power {
            label: strings::TRAY_START,
            exit: PanelExit::StartMarsa,
            off: None,
        },
    }
}

/// What the keys do: Esc closes the panel, and 1 to 9 open the site with that
/// key. A key with a modifier is the system's.
pub fn keys(view: &PanelView, pressed: impl Fn(Key) -> bool, plain: bool) -> Vec<PanelAction> {
    let mut actions = Vec::new();
    if pressed(Key::Escape) {
        actions.push(PanelAction::Exit(PanelExit::Closed));
    }
    if !plain {
        return actions;
    }
    let rows = view.sites.as_ref().map_or(&[][..], |sites| &sites.rows[..]);
    for (key, row) in DIGITS.into_iter().zip(rows) {
        if pressed(key) {
            actions.push(PanelAction::Do(Action::OpenSite(row.name.clone())));
        }
    }
    actions
}

/// Draws `view` in a window that is at most `limit` high: when the panel
/// needs more, the sites and the controls scroll between the header and the
/// footer, which stay. Returns what the user did.
pub fn show_limited(ui: &mut Ui, view: &PanelView, theme: &Theme, limit: f32) -> Vec<PanelAction> {
    let mut actions =
        ui.input(|input| keys(view, |key| input.key_pressed(key), !input.modifiers.any()));
    frame(ui, |ui| {
        header(ui, theme, &view.status, &mut actions);
        // What is left for the body: the limit, less the border of the frame
        // on both sides, what the header took, and the footer with its line.
        let border = theme.size.border;
        let taken = border + border + ui.min_rect().height() + border + theme.size.panel_footer;
        let room = (limit - taken).max(theme.size.panel_row);
        ScrollArea::vertical()
            .id_salt("panel_body")
            .max_height(room)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if let Some(sites) = &view.sites {
                    site_list(ui, theme, sites, &mut actions);
                }
                if view.php.is_some() || view.xdebug.is_some() {
                    divider(ui);
                    controls_section(ui, theme, view, &mut actions);
                }
            });
        divider(ui);
        footer(ui, theme, &view.power, &mut actions);
    });
    actions
}

/// The status, and "Open Marsa" at the end of the line.
fn header(ui: &mut Ui, theme: &Theme, status: &StatusLine, actions: &mut Vec<PanelAction>) {
    let side = theme.space.s2;
    let margin = Margin::symmetric(whole(side), whole(side));
    eframe::egui::Frame::new()
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let dot = StatusDot::new(status.health, status.title, status.caption);
            let shown = ui.add(dot);
            // A click on the status opens the window, where it says more.
            if shown.clicked() {
                actions.push(PanelAction::Exit(PanelExit::OpenWindow));
            }
            // "Open Marsa" at the end of the status's line, centered on it.
            let open = Button::quiet(strings::TRAY_OPEN);
            let line = Rect::from_x_y_ranges(ui.max_rect().x_range(), shown.rect.y_range());
            let size = vec2(open.width(ui), theme.size.control);
            let at = Align2::RIGHT_CENTER.align_size_within_rect(size, line);
            let link = ui.put(at, open);
            if link.clicked() {
                actions.push(PanelAction::Exit(PanelExit::OpenWindow));
            }
        });
}

/// The sites, a row each with its key; or what there is to say instead.
fn site_list(ui: &mut Ui, theme: &Theme, sites: &SitesView, actions: &mut Vec<PanelAction>) {
    let (side, bottom) = (whole(theme.space.s2), whole(theme.space.s2));
    let margin = Margin {
        left: side,
        right: side,
        top: 0,
        bottom,
    };
    eframe::egui::Frame::new()
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if sites.rows.is_empty() {
                line(ui, theme, strings::PANEL_NO_SITES);
                return;
            }
            for row in &sites.rows {
                let key = row.key.to_string();
                let shown = SiteRow::new(&key, &row.name, &row.php, &row.label);
                if ui.add(shown).clicked() {
                    actions.push(PanelAction::Do(Action::OpenSite(row.name.clone())));
                }
            }
            if sites.more > 0 {
                // A quiet link: the rest of the sites are in the window.
                let more = strings::panel_more_sites(sites.more);
                let shown = ui.horizontal(|ui| {
                    ui.add_space(theme.space.s1);
                    ui.add(Button::quiet(&more))
                });
                if shown.inner.clicked() {
                    actions.push(PanelAction::Exit(PanelExit::OpenSites));
                }
            }
        });
}

/// A line of `caption` text under the same padding as the controls.
fn line(ui: &mut Ui, theme: &Theme, text: &str) {
    let pad = theme.space.s2 + theme.space.s1;
    ui.add_space(theme.space.s1);
    ui.horizontal(|ui| {
        ui.add_space(pad);
        mute(ui, theme, text);
    });
    ui.add_space(theme.space.s1);
}

/// The default PHP, and Xdebug under it.
fn controls_section(ui: &mut Ui, theme: &Theme, view: &PanelView, actions: &mut Vec<PanelAction>) {
    let margin = Margin::symmetric(whole(theme.space.s4), whole(theme.space.s4));
    eframe::egui::Frame::new()
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = theme.space.group_gap_max;
            if let Some(php) = &view.php {
                let label = RichText::new(strings::DEFAULT_PHP)
                    .font(theme.text.body_sm.font_id())
                    .color(theme.color.ink2);
                ui.add(Label::new(label).selectable(false));
                default_php(ui, php, actions);
            }
            if let Some(xdebug) = &view.xdebug {
                let flipped =
                    SwitchRow::new(xdebug.on, strings::XDEBUG, strings::PANEL_XDEBUG_CAPTION)
                        .disabled_if(xdebug.off.is_some(), xdebug.off.unwrap_or_default())
                        .show(ui);
                if flipped {
                    actions.push(PanelAction::Do(Action::ToggleXdebug));
                }
                if let Some(error) = &xdebug.error {
                    status_line(ui, theme, Mark::Error, error, |_| {});
                }
            }
        });
}

/// The medium version buttons. Every installed version shows, on as many
/// rows as the width needs; there is no "+": installing is the window's.
fn default_php(ui: &mut Ui, php: &DefaultPhp, actions: &mut Vec<PanelAction>) {
    let versions: Vec<(usize, &str)> = php
        .choices
        .iter()
        .enumerate()
        .map(|(index, choice)| (index, choice.label.as_str()))
        .collect();
    let picker = VersionPicker::new(strings::DEFAULT_PHP, &versions, php.chosen)
        .size(PickerSize::Medium)
        .columns(VERSION_COLUMNS);
    if let Some(VersionPick::Version(index)) = picker.show(ui).inner
        && let Some(choice) = php.choices.get(index)
    {
        actions.push(PanelAction::Do(Action::SetDefault {
            runtime: MAIN_RUNTIME,
            version: choice.version.clone(),
        }));
    }
}

/// "Stop Marsa" (or "Start Marsa") and "Quit", side by side.
fn footer(ui: &mut Ui, theme: &Theme, power: &Power, actions: &mut Vec<PanelAction>) {
    let margin = Margin::symmetric(whole(theme.space.s2), whole(theme.space.s1));
    eframe::egui::Frame::new()
        .inner_margin(margin)
        .show(ui, |ui| {
            let width = ui.available_width();
            // Two links of one width, and the line between them.
            let half = ((width - theme.size.border) / (UNIT + UNIT)).floor();
            ui.spacing_mut().item_spacing.x = 0.0;
            let line = vec2(width, footer_link_height(theme));
            ui.allocate_ui_with_layout(line, Direction::CURRENT.row(), |ui| {
                let stop = FooterLink::new(power.label, half)
                    .disabled_if(power.off.is_some(), power.off.unwrap_or_default());
                if ui.add(stop).clicked() {
                    actions.push(PanelAction::Exit(power.exit));
                }
                footer_divider(ui);
                // Quit closes the icon; the daemon and the sites go on.
                let quit =
                    FooterLink::new(strings::TRAY_QUIT, half).tooltip(strings::PANEL_QUIT_HINT);
                if ui.add(quit).clicked() {
                    actions.push(PanelAction::Exit(PanelExit::Quit));
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use crate::model::Effect;
    use std::path::PathBuf;

    use eframe::egui::{self, Pos2};
    use marsa_core::runtimes::Runtime;
    use marsa_core::types::{
        Health as DaemonHealth, RuntimeList, RuntimeVersions, ServiceState, ServiceStatus, Site,
        SiteKind, SiteList, XdebugState,
    };

    use super::*;
    use crate::model::{Update, WEB_SERVER};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::{Harness, Tree};

    /// The panel with all the room it needs.
    fn show(ui: &mut Ui, view: &PanelView, theme: &Theme) -> Vec<PanelAction> {
        show_limited(ui, view, theme, f32::INFINITY)
    }

    fn connected() -> Model {
        let mut model = Model::default();
        model.update(Update::Connected(DaemonHealth {
            app_version: "0.0.0".into(),
            api_version: 1,
            pid: 7,
        }));
        model
    }

    fn site(name: &str, php: Option<&str>) -> Site {
        let root = PathBuf::from("/code").join(name);
        Site {
            name: name.into(),
            docroot: root.join("public"),
            root,
            kind: SiteKind::Laravel,
            php: php.map(|text| text.parse().expect("a version")),
            php_source: None,
            secure: true,
        }
    }

    fn service(state: ServiceState) -> ServiceStatus {
        ServiceStatus {
            id: WEB_SERVER.into(),
            pid: None,
            state,
            restarts: 0,
            since_ms: None,
        }
    }

    fn xdebug_state(enabled: bool) -> XdebugState {
        XdebugState {
            enabled,
            loaded: Vec::new(),
            missing: Vec::new(),
        }
    }

    /// PHP these versions installed (`8.4` the default), Caddy running, and
    /// `count` sites named `site-01`, `site-02`, and so on (the list is sorted by
    /// name).
    fn working(count: usize, php: &[&str]) -> Model {
        let mut model = connected();
        let row = |runtime, installed: &[&str], default: Option<&str>| RuntimeVersions {
            runtime,
            installed: installed.iter().map(|text| (*text).to_owned()).collect(),
            default: default.map(str::to_owned),
        };
        model.update(Update::Runtimes(RuntimeList {
            runtimes: vec![row(Runtime::Php, php, Some("8.4"))],
        }));
        model.update(Update::Services(vec![service(ServiceState::Running)]));
        let sites = (1..=count)
            .map(|n| site(&format!("site-{n:02}"), Some("8.4.27")))
            .collect();
        model.update(Update::Sites(SiteList {
            sites,
            ..SiteList::default()
        }));
        model.update(Update::Xdebug(xdebug_state(false)));
        model
    }

    #[test]
    fn everything_working_says_so_with_the_sidebars_words_and_lists_the_controls() {
        let view = view(&working(2, &["8.3.27", "8.4.27"]));
        assert_eq!(view.status.health, Health::Ok);
        assert_eq!(view.status.title, "All sites working");
        assert_eq!(view.status.caption, "Caddy is running");
        let sites = view.sites.expect("sites");
        assert_eq!(sites.more, 0);
        let names: Vec<(usize, &str, &str)> = sites
            .rows
            .iter()
            .map(|row| (row.key, row.name.as_str(), row.php.as_str()))
            .collect();
        assert_eq!(
            names,
            [(1, "site-01", "PHP 8.4"), (2, "site-02", "PHP 8.4")]
        );
        assert_eq!(sites.rows[0].label, "Open site-01");
        let php = view.php.expect("PHP is installed");
        let labels: Vec<&str> = php.choices.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["8.3", "8.4"]);
        assert_eq!(php.chosen, Some(1));
        let xdebug = view.xdebug.expect("read");
        assert!(!xdebug.on && xdebug.off.is_none() && xdebug.error.is_none());
        assert_eq!(view.power.label, "Stop Marsa");
        assert_eq!(view.power.exit, PanelExit::StopMarsa);
        assert_eq!(view.power.off, None);
    }

    #[test]
    fn a_site_with_no_php_has_a_dash_and_the_others_their_line() {
        let mut model = working(0, &["8.4.27"]);
        model.update(Update::Sites(SiteList {
            sites: vec![site("plain", None), site("shop", Some("8.3.9"))],
            ..SiteList::default()
        }));
        let rows = view(&model).sites.expect("sites").rows;
        assert_eq!(rows[0].php, "–");
        assert_eq!(rows[1].php, "PHP 8.3");
    }

    #[test]
    fn caddy_stopped_is_an_error_in_words_and_the_controls_stay() {
        let mut model = working(3, &["8.4.27"]);
        model.update(Update::Services(vec![service(ServiceState::Stopped)]));
        let view = view(&model);
        assert_eq!(view.status.health, Health::Error);
        assert_eq!(view.status.title, "Sites not opening");
        assert_eq!(view.status.caption, "Caddy stopped");
        assert_eq!(view.sites.expect("sites").rows.len(), 3);
        assert!(view.php.is_some() && view.xdebug.is_some());
    }

    #[test]
    fn with_no_sites_the_list_is_empty_and_the_rest_stays() {
        let view = view(&working(0, &["8.4.27"]));
        assert_eq!(view.status.title, "Marsa is running");
        assert_eq!(view.status.caption, "No sites yet");
        let sites = view.sites.as_ref().expect("sites");
        assert!(sites.rows.is_empty() && sites.more == 0);
        assert!(view.php.is_some());
        // The list says what to do, once: the status says there is none.
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            show(ui, &view, &LIGHT);
        });
        assert!(tree.node_with_text("Add a site in Marsa.").is_some());
        assert!(tree.node_with_text("No sites yet.").is_none());
    }

    #[test]
    fn the_panel_lists_at_most_nine_sites_with_the_keys_1_to_9_and_counts_the_rest() {
        for (count, shown, more) in [(0, 0, 0), (1, 1, 0), (9, 9, 0), (10, 9, 1), (12, 9, 3)] {
            let sites = view(&working(count, &["8.4.27"])).sites.expect("sites");
            assert_eq!(sites.rows.len(), shown, "{count} sites");
            assert_eq!(sites.more, more, "{count} sites");
            let keys: Vec<usize> = sites.rows.iter().map(|row| row.key).collect();
            assert_eq!(keys, (1..=shown).collect::<Vec<_>>());
        }
        assert_eq!(strings::panel_more_sites(1), "1 more site in Marsa");
        assert_eq!(strings::panel_more_sites(3), "3 more sites in Marsa");
    }

    #[test]
    fn without_a_daemon_only_the_status_and_the_footer_show() {
        let mut model = Model::default();
        let connecting = view(&model);
        assert_eq!(connecting.status.health, Health::Busy);
        assert!(connecting.sites.is_none() && connecting.php.is_none());
        assert!(connecting.xdebug.is_none());
        // Stop waits for a daemon to stop.
        assert_eq!(connecting.power.label, "Stop Marsa");
        assert_eq!(connecting.power.off, Some(strings::PANEL_NOT_CONNECTED));
        model.update(Update::Failed("daemon_not_running".into()));
        let down = view(&model);
        assert_eq!(down.status.health, Health::Error);
        assert_eq!(down.status.caption, "Marsa is not running");
        // Nothing to stop: the one thing to offer is to start it.
        assert_eq!(down.power.label, "Start Marsa");
        assert_eq!(down.power.exit, PanelExit::StartMarsa);
        assert_eq!(down.power.off, None);
        let mut model = working(1, &["8.4.27"]);
        model.update(Update::Event(marsa_core::types::Event::ShuttingDown));
        let stopping = view(&model);
        assert_eq!(stopping.power.off, Some(strings::PANEL_ALREADY_STOPPING));
        assert!(stopping.sites.is_none());
    }

    #[test]
    fn php_that_is_not_installed_hides_the_buttons_and_xdebug_says_why_it_is_off() {
        let none = view(&working(1, &[]));
        assert!(none.php.is_none());
        let mut model = working(1, &["8.4.27"]);
        model.session.xdebug = None;
        let unread = view(&model).xdebug.expect("a line");
        assert_eq!(unread.off, Some(strings::XDEBUG_UNREAD));
        let mut model = working(1, &["8.4.27"]);
        assert!(matches!(
            model.apply_action(Action::ToggleXdebug),
            Effect::Send(crate::bridge::Command::SetXdebug(true))
        ));
        let busy = view(&model).xdebug.expect("a line");
        assert_eq!((busy.on, busy.off), (true, Some(strings::XDEBUG_BUSY)));
        model.update(Update::XdebugAnswered(Err("xdebug_interrupted".into())));
        let failed = view(&model).xdebug.expect("a line");
        assert_eq!(
            failed.error,
            Some(strings::error_text("xdebug_interrupted"))
        );
    }

    // -- What the user does --

    /// The middle of the node a screen reader knows as `label`.
    fn center(tree: &Tree, label: &str) -> Pos2 {
        let node = tree
            .nodes_with_label(label)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("no node {label}"));
        let rect = node.bounds().expect("bounds");
        egui::pos2(
            ((rect.x0 + rect.x1) / 2.0) as f32,
            ((rect.y0 + rect.y1) / 2.0) as f32,
        )
    }

    /// [`center`] for a node that egui gives its text as a value.
    fn center_of_text(tree: &Tree, text: &str) -> Pos2 {
        let node = tree.node_with_text(text).expect("a node");
        let rect = node.bounds().expect("bounds");
        egui::pos2(
            ((rect.x0 + rect.x1) / 2.0) as f32,
            ((rect.y0 + rect.y1) / 2.0) as f32,
        )
    }

    /// One frame of the panel in a window the panel's width.
    fn frame_of(
        harness: &Harness,
        input: egui::RawInput,
        view: &PanelView,
        theme: &Theme,
    ) -> (Tree, Vec<PanelAction>) {
        let mut actions = Vec::new();
        let tree = harness.frame(input, |ui| actions = show(ui, view, theme));
        (tree, actions)
    }

    fn key_input(key: Key) -> egui::RawInput {
        egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..egui::RawInput::default()
        }
    }

    #[test]
    fn the_keys_1_to_9_open_the_site_with_that_key_and_esc_closes() {
        let harness = Harness::new(egui::Theme::Light);
        let view = view(&working(12, &["8.4.27"]));
        for (index, key) in DIGITS.into_iter().enumerate() {
            let (_, actions) = frame_of(&harness, key_input(key), &view, &LIGHT);
            let name = format!("site-{:02}", index + 1);
            assert_eq!(
                actions,
                [PanelAction::Do(Action::OpenSite(name))],
                "{key:?}"
            );
        }
        let (_, actions) = frame_of(&harness, key_input(Key::Escape), &view, &LIGHT);
        assert_eq!(actions, [PanelAction::Exit(PanelExit::Closed)]);
        // No key, no action; a letter is not a key of the panel.
        let (_, actions) = frame_of(&harness, egui::RawInput::default(), &view, &LIGHT);
        assert!(actions.is_empty());
        let (_, actions) = frame_of(&harness, key_input(Key::A), &view, &LIGHT);
        assert!(actions.is_empty());
    }

    #[test]
    fn a_key_with_no_site_does_nothing_and_a_key_with_a_modifier_is_not_ours() {
        let harness = Harness::new(egui::Theme::Light);
        let few = view(&working(2, &["8.4.27"]));
        let (_, actions) = frame_of(&harness, key_input(Key::Num5), &few, &LIGHT);
        assert!(actions.is_empty(), "no fifth site");
        let (_, actions) = frame_of(&harness, key_input(Key::Num1), &view_of_none(), &LIGHT);
        assert!(actions.is_empty(), "no daemon, no sites");
        // Cmd or Ctrl with a number belongs to the system.
        let mut input = key_input(Key::Num1);
        input
            .events
            .insert(0, egui::Event::ModifiersChanged(egui::Modifiers::COMMAND));
        let (_, actions) = frame_of(&harness, input, &few, &LIGHT);
        assert!(actions.is_empty());
        // Esc still closes with a modifier held.
        let found = keys(&few, |key| key == Key::Escape, false);
        assert_eq!(found, [PanelAction::Exit(PanelExit::Closed)]);
    }

    fn view_of_none() -> PanelView {
        view(&Model::default())
    }

    #[test]
    fn a_click_on_a_site_opens_it_and_a_click_on_a_version_saves_it() {
        let harness = Harness::new(egui::Theme::Light);
        let view = view(&working(3, &["8.2.30", "8.3.27", "8.4.27"]));
        let labels: Vec<&str> = view
            .php
            .iter()
            .flat_map(|php| &php.choices)
            .map(|c| c.label.as_str())
            .collect();
        assert_eq!(labels, ["8.2", "8.3", "8.4"]);
        let tree = harness.accessibility(|ui| {
            show(ui, &view, &LIGHT);
        });
        let at = center(&tree, "Open site-02");
        let mut actions = Vec::new();
        harness.click(at, |ui| actions = show(ui, &view, &LIGHT));
        assert_eq!(
            actions,
            [PanelAction::Do(Action::OpenSite("site-02".into()))]
        );
        // The button of 8.3.27.
        let at = center(&tree, "8.3");
        harness.click(at, |ui| actions = show(ui, &view, &LIGHT));
        let [PanelAction::Do(Action::SetDefault { runtime, version })] = &actions[..] else {
            panic!("expected SetDefault, got {actions:?}");
        };
        assert_eq!(*runtime, Runtime::Php);
        assert_eq!(version.to_string(), "8.3.27");
        // The chosen one reports nothing.
        let at = center(&tree, "8.4");
        harness.click(at, |ui| actions = show(ui, &view, &LIGHT));
        assert!(actions.is_empty());
    }

    #[test]
    fn the_xdebug_switch_and_the_footer_and_the_status_ask_what_they_say() {
        let harness = Harness::new(egui::Theme::Light);
        let view = view(&working(1, &["8.4.27"]));
        let tree = harness.accessibility(|ui| {
            show(ui, &view, &LIGHT);
        });
        let mut actions = Vec::new();
        for (label, want) in [
            ("Xdebug", PanelAction::Do(Action::ToggleXdebug)),
            ("Stop Marsa", PanelAction::Exit(PanelExit::StopMarsa)),
            ("Quit", PanelAction::Exit(PanelExit::Quit)),
            ("Open Marsa", PanelAction::Exit(PanelExit::OpenWindow)),
        ] {
            let at = center(&tree, label);
            harness.click(at, |ui| actions = show(ui, &view, &LIGHT));
            assert_eq!(actions, [want], "{label}");
        }
        // The status dot opens the window too.
        let at = center(&tree, "All sites working, Caddy is running");
        harness.click(at, |ui| actions = show(ui, &view, &LIGHT));
        assert_eq!(actions, [PanelAction::Exit(PanelExit::OpenWindow)]);
    }

    #[test]
    fn the_more_sites_line_is_a_link_that_opens_marsa_on_the_sites_screen() {
        let harness = Harness::new(egui::Theme::Light);
        let view = view(&working(12, &["8.4.27"]));
        assert_eq!(view.sites.as_ref().map(|sites| sites.more), Some(3));
        let tree = harness.accessibility(|ui| {
            show(ui, &view, &LIGHT);
        });
        // A button a screen reader can press: it has a name and bounds.
        let node = tree.node_with_text("3 more sites in Marsa");
        assert!(node.is_some(), "{:?}", tree.labels_starting_with(""));
        let at = center_of_text(&tree, "3 more sites in Marsa");
        let mut actions = Vec::new();
        harness.click(at, |ui| actions = show(ui, &view, &LIGHT));
        assert_eq!(actions, [PanelAction::Exit(PanelExit::OpenSites)]);
        // Nine sites or fewer: no such line.
        let nine = self::view(&working(9, &["8.4.27"]));
        let tree = harness.accessibility(|ui| {
            show(ui, &nine, &LIGHT);
        });
        assert!(tree.labels_starting_with("1 more").is_empty());
        assert!(tree.labels_starting_with("0 more").is_empty());
    }

    #[test]
    fn with_no_daemon_the_footer_starts_it_and_stop_says_why_it_waits() {
        let harness = Harness::new(egui::Theme::Light);
        let mut down = Model::default();
        down.update(Update::Failed("daemon_not_running".into()));
        let down = view(&down);
        let tree = harness.accessibility(|ui| {
            show(ui, &down, &LIGHT);
        });
        let mut actions = Vec::new();
        let at = center(&tree, "Start Marsa");
        harness.click(at, |ui| actions = show(ui, &down, &LIGHT));
        assert_eq!(actions, [PanelAction::Exit(PanelExit::StartMarsa)]);
        // A disabled Stop does nothing.
        let waiting = view_of_none();
        let tree = harness.accessibility(|ui| {
            show(ui, &waiting, &LIGHT);
        });
        let at = center(&tree, "Stop Marsa");
        harness.click(at, |ui| actions = show(ui, &waiting, &LIGHT));
        assert!(actions.is_empty());
    }

    #[test]
    fn a_panel_taller_than_the_room_scrolls_its_body_between_the_header_and_the_footer() {
        let harness = Harness::new(egui::Theme::Light);
        let nine = view(&working(9, &["8.4.27"]));
        let height = |limit: f32| {
            let mut rect = egui::Rect::NOTHING;
            harness.run(|ui| {
                let before = ui.cursor().min;
                show_limited(ui, &nine, &LIGHT, limit);
                rect = egui::Rect::from_min_max(before, ui.min_rect().max);
            });
            rect.height()
        };
        let whole = height(f32::INFINITY);
        // Room enough: the panel is what it is.
        assert_eq!(height(whole + 100.0), whole);
        // Less room: the panel is as high as the room, never more.
        for limit in [300.0, 420.0, whole - 1.0] {
            let shown = height(limit);
            assert!((shown - limit).abs() < 1.0, "{limit}: {shown}");
        }
        // The footer is still there, and its links work, in a short panel.
        let tree = harness.accessibility(|ui| {
            show_limited(ui, &nine, &LIGHT, 300.0);
        });
        assert!(tree.node_with_label("Quit").is_some());
        assert!(tree.node_with_label("Open Marsa").is_some());
    }

    #[test]
    fn the_panel_is_360_wide_and_as_high_as_its_content_in_both_themes() {
        for (theme, mode) in [(&LIGHT, egui::Theme::Light), (&DARK, egui::Theme::Dark)] {
            let harness = Harness::new(mode);
            let height = |view: &PanelView| {
                let mut rect = egui::Rect::NOTHING;
                harness.run(|ui| {
                    let before = ui.cursor().min;
                    show(ui, view, theme);
                    rect = egui::Rect::from_min_max(before, ui.min_rect().max);
                });
                assert_eq!(rect.width(), theme.size.panel_width, "{mode:?}");
                rect.height()
            };
            let few = height(&view(&working(2, &["8.4.27"])));
            let nine = height(&view(&working(9, &["8.4.27"])));
            let twelve = height(&view(&working(12, &["8.4.27"])));
            // Seven more rows, 42 each; a tenth to twelfth site adds one
            // line that says how many more.
            assert_eq!(nine - few, 7.0 * theme.size.panel_row, "{mode:?}");
            assert!(twelve > nine && twelve - nine < theme.size.panel_row);
            // Four versions stand in one row; a fifth needs a second.
            let four = height(&view(&working(2, &["8.1.0", "8.2.0", "8.3.0", "8.4.27"])));
            assert_eq!(four, few);
            let two = height(&view(&working(
                2,
                &["8.1.0", "8.2.0", "8.3.0", "8.4.27", "8.5.0"],
            )));
            assert_eq!(two - few, theme.size.version_md + theme.space.group_gap);
        }
    }
}
