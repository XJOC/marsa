//! The sidebar (layout.md, Shell; components.md, Sidebar): the name and the
//! fold button, the items in their groups, then the status and Settings at
//! the bottom. Only the sections that ship appear (rules.md 8): Mail, while
//! Mailpit is installed; Phase 2 adds Databases and Queues to these groups.

use eframe::egui::{Layout, RichText, Ui, vec2};

use crate::model::{Action, LogSource, MAIN_RUNTIME, Model, Overall, Route};
use crate::strings;
use crate::theme::{Direction, Face, Section, TextToken, Theme};
use crate::widgets::{Button, Health, Icon, NavItem, StatusDot};

/// An item of the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Item {
    Home,
    Sites,
    /// Only while Mailpit is installed ([`Item::shown`]).
    Mail,
    Dumps,
    Logs,
    Runtimes,
    Services,
    /// At the bottom, under the status.
    Settings,
}

/// The items above the status, in their groups, in order.
pub(crate) const GROUPS: [&[Item]; 4] = [
    &[Item::Home],
    &[Item::Sites],
    &[Item::Mail, Item::Dumps, Item::Logs],
    &[Item::Runtimes, Item::Services],
];

impl Item {
    /// The route a click on the item opens. The Runtimes item opens the main
    /// runtime here; a click opens the one the screen showed last
    /// ([`Model::chosen_runtime`], see [`Item::route_in`]).
    pub(crate) fn route(self) -> Route {
        match self {
            Self::Home => Route::Home,
            Self::Sites => Route::Sites,
            Self::Mail => Route::Mail,
            Self::Dumps => Route::Dumps,
            Self::Logs => Route::Logs(LogSource::default()),
            Self::Runtimes => Route::Runtimes(MAIN_RUNTIME),
            Self::Services => Route::Services,
            Self::Settings => Route::Settings,
        }
    }

    /// [`Item::route`] for a window that remembers where it was: the Runtimes
    /// item opens the runtime the user chose last.
    pub(crate) fn route_in(self, model: &Model) -> Route {
        match self {
            Self::Runtimes => Route::Runtimes(model.chosen_runtime()),
            _ => self.route(),
        }
    }

    /// The item a route belongs to: a site's page is under Sites.
    pub(crate) fn of(route: &Route) -> Self {
        match route {
            Route::Home => Self::Home,
            Route::Sites | Route::Site(_) => Self::Sites,
            Route::Mail => Self::Mail,
            Route::Dumps => Self::Dumps,
            Route::Logs(_) => Self::Logs,
            Route::Runtimes(_) => Self::Runtimes,
            Route::Services => Self::Services,
            Route::Settings => Self::Settings,
        }
    }

    /// The section whose tile the item shows. Settings has a plain icon.
    pub(crate) const fn section(self) -> Option<Section> {
        match self {
            Self::Home => Some(Section::Home),
            Self::Sites => Some(Section::Sites),
            Self::Mail => Some(Section::Mail),
            Self::Dumps => Some(Section::Dumps),
            Self::Logs => Some(Section::Logs),
            Self::Runtimes => Some(Section::Runtimes),
            Self::Services => Some(Section::Services),
            Self::Settings => None,
        }
    }

    /// Whether the sidebar lists the item: Mail only while Mailpit is
    /// installed, so the window shows only what the user has.
    pub(crate) fn shown(self, model: &Model) -> bool {
        match self {
            Self::Mail => model.has_mailpit(),
            _ => true,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self.section() {
            Some(section) => strings::section_name(section),
            None => strings::SETTINGS,
        }
    }
}

/// A window narrower than this folds the sidebar, whatever the user chose.
pub(crate) fn is_narrow(theme: &Theme, window_width: f32) -> bool {
    window_width < theme.size.sidebar_fold_below
}

/// The mark of the status at the bottom: an error when no site opens.
pub(crate) const fn health(overall: Overall) -> Health {
    match overall {
        Overall::Connecting | Overall::Stopping | Overall::Starting => Health::Busy,
        Overall::DaemonDown | Overall::WebServerDown => Health::Error,
        Overall::NoSites | Overall::Working => Health::Ok,
    }
}

/// Draws the sidebar inside its panel. `narrow`: the window is too narrow
/// for labels. Returns what the user did.
pub(crate) fn show(ui: &mut Ui, model: &Model, narrow: bool) -> Vec<Action> {
    let theme = Theme::of(ui.ctx());
    let folded = narrow || model.sidebar_folded;
    let current = Item::of(&model.route);
    let mut actions = Vec::new();
    ui.spacing_mut().item_spacing.y = theme.space.s1;
    let label = if folded {
        strings::SHOW_LABELS
    } else {
        strings::HIDE_LABELS
    };
    let fold = Button::icon(Icon::Fold, label).disabled_if(narrow, strings::FOLD_NARROW_HINT);
    let header = vec2(ui.available_width(), theme.size.control);
    let folds = ui
        .allocate_ui_with_layout(header, Direction::CURRENT.row(), |ui| {
            if folded {
                // The button alone, in the middle of the column.
                return ui.centered_and_justified(|ui| ui.add(fold)).inner;
            }
            // The name lines up with the tiles under it.
            ui.add_space(theme.size.nav_inset());
            let name = TextToken {
                size: theme.text.heading.size,
                face: Face::SemiBold,
            };
            let name = RichText::new(strings::APP_NAME)
                .font(name.font_id())
                .color(theme.color.ink);
            ui.label(name);
            ui.with_layout(Direction::CURRENT.row_from_end(), |ui| ui.add(fold))
                .inner
        })
        .inner;
    if folds.clicked() {
        actions.push(Action::FoldSidebar);
    }
    ui.add_space(theme.space.s2);
    for (index, group) in GROUPS.iter().enumerate() {
        if index > 0 {
            ui.add_space(theme.space.s3);
        }
        for &item in group.iter().filter(|item| item.shown(model)) {
            if ui.add(nav(model, item, current, folded)).clicked() {
                actions.push(Action::Navigate(item.route_in(model)));
            }
        }
    }
    // The status and Settings sit at the bottom, Settings last.
    ui.with_layout(Layout::bottom_up(Direction::CURRENT.start()), |ui| {
        let settings = nav(model, Item::Settings, current, folded);
        if ui.add(settings).clicked() {
            actions.push(Action::Navigate(Item::Settings.route()));
        }
        let overall = model.overall();
        let (title, caption) = overall.text();
        let status = StatusDot::new(health(overall), title, caption)
            .width(ui.available_width())
            .folded(folded);
        // Home tells more about the status.
        if ui.add(status).clicked() {
            actions.push(Action::Navigate(Route::Home));
        }
    });
    actions
}

/// The widget of `item`: Sites counts the sites, Mail the mail not opened
/// yet, and Dumps the dumps that came while its screen was shut, as new.
fn nav(model: &Model, item: Item, current: Item, folded: bool) -> NavItem<'static> {
    let shown = match item.section() {
        Some(section) => NavItem::section(section),
        None => NavItem::plain(Icon::Settings, item.label()),
    };
    let shown = shown.selected(item == current).folded(folded);
    let count = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
    let sites = model.session.sites.sites.len() as u64;
    let unread = model.session.mail.unread();
    let unseen = model.session.dumps.unseen();
    match item {
        Item::Sites if sites > 0 => shown.count(count(sites), false),
        Item::Mail if unread > 0 => shown.count(count(unread), true),
        Item::Dumps if unseen > 0 => shown.count(count(unseen), true),
        _ => shown,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use eframe::egui::{self, Pos2, pos2};
    use marsa_core::types::{Event, ServiceState, ServiceStatus, Site, SiteKind, SiteList};

    use marsa_core::runtimes::Runtime;

    use super::*;
    use crate::fixtures::connected;
    use crate::model::Update;
    use crate::theme::LIGHT;
    use crate::widgets::harness::{Harness, Tree};
    use marsa_core::types::ServiceId;

    /// Every item a window without Mailpit shows: the groups, then
    /// Settings.
    const ALL: [Item; 7] = [
        Item::Home,
        Item::Sites,
        Item::Dumps,
        Item::Logs,
        Item::Runtimes,
        Item::Services,
        Item::Settings,
    ];

    /// A model whose `overall()` is `overall`.
    fn model_in(overall: Overall) -> Model {
        let mut model = connected();
        let web = |model: &mut Model, state| {
            let root = PathBuf::from("/code/shop");
            let sites = SiteList {
                sites: vec![Site {
                    name: "shop".into(),
                    docroot: root.join("public"),
                    root,
                    kind: SiteKind::Laravel,
                    php: None,
                    php_source: None,
                    secure: true,
                    queue: false,
                    schedule: false,
                }],
                ..SiteList::default()
            };
            model.update(Update::Sites(sites));
            model.update(Update::Services(vec![ServiceStatus {
                id: ServiceId::Caddy,
                state,
                pid: None,
                restarts: 0,
                since_ms: None,
            }]));
        };
        match overall {
            Overall::Connecting => model = Model::default(),
            Overall::Stopping => model.update(Update::Event(Event::ShuttingDown)),
            Overall::DaemonDown => model.update(Update::Failed("daemon_start_failed".into())),
            Overall::NoSites => {}
            Overall::Starting => web(&mut model, ServiceState::Starting),
            Overall::WebServerDown => web(&mut model, ServiceState::Stopped),
            Overall::Working => web(&mut model, ServiceState::Running),
        }
        model
    }

    /// The middle of the widget a screen reader knows as `label`.
    fn center(tree: &Tree, label: &str) -> Pos2 {
        let node = tree.node_with_label(label).expect(label);
        let bounds = node.bounds().expect("a node with bounds");
        pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        )
    }

    /// One click on the widget named `label`; what the sidebar returned.
    fn click(harness: &Harness, model: &Model, narrow: bool, label: &str) -> Vec<Action> {
        let tree = harness.accessibility(|ui| {
            show(ui, model, narrow);
        });
        let mut actions = Vec::new();
        harness.click(center(&tree, label), |ui| {
            actions.extend(show(ui, model, narrow));
        });
        actions
    }

    #[test]
    fn the_sidebar_shows_the_sections_that_ship_in_their_order() {
        let order: Vec<Item> = GROUPS
            .iter()
            .flat_map(|group| group.iter().copied())
            .collect();
        assert_eq!(
            order,
            [
                Item::Home,
                Item::Sites,
                Item::Mail,
                Item::Dumps,
                Item::Logs,
                Item::Runtimes,
                Item::Services
            ]
        );
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            show(ui, &connected(), false);
        });
        for item in ALL {
            assert!(tree.node_with_label(item.label()).is_some(), "{item:?}");
        }
        for section in [Section::Databases, Section::Queues] {
            let name = strings::section_name(section);
            assert!(tree.node_with_label(name).is_none(), "{name} ships later");
        }
        // No Mailpit, no Mail.
        let mail = strings::section_name(Section::Mail);
        assert!(tree.node_with_label(mail).is_none());
    }

    /// A connected window with Mailpit installed, and these many mails not
    /// opened yet.
    fn with_mailpit(unread: u64) -> Model {
        use marsa_core::types::{MailList, RuntimeList, RuntimeVersions};
        let mut model = connected();
        model.update(Update::Runtimes(RuntimeList {
            runtimes: vec![RuntimeVersions {
                runtime: Runtime::Mailpit,
                installed: vec!["1.27.10".into()],
                default: None,
            }],
        }));
        model.update(Update::Mail(MailList {
            total: unread + 2,
            unread,
            messages: Vec::new(),
        }));
        model
    }

    #[test]
    fn mail_shows_once_mailpit_is_installed_and_counts_the_new_mail() {
        let harness = Harness::new(egui::Theme::Light);
        let mail = strings::section_name(Section::Mail);
        let model = with_mailpit(3);
        let tree = harness.accessibility(|ui| {
            show(ui, &model, false);
        });
        let node = tree.node_with_label(mail).expect("the Mail item");
        assert_eq!(node.description(), Some("3 new mails"));
        // Between Sites and Logs.
        let top = |label: &str| {
            let node = tree.node_with_label(label).expect(label);
            node.bounds().expect("a place").y0
        };
        assert!(top(strings::SITES) < top(mail) && top(mail) < top(strings::LOGS));
        // Nothing new: no count.
        let quiet = with_mailpit(0);
        let tree = harness.accessibility(|ui| {
            show(ui, &quiet, false);
        });
        let node = tree.node_with_label(mail).expect("the Mail item");
        assert_eq!(node.description(), None);
        // A click opens Mail, and the screen reads the mail as it opens.
        let mut model = with_mailpit(3);
        let actions = click(&harness, &model, false, mail);
        assert_eq!(actions, [Action::Navigate(Route::Mail)]);
        for action in actions {
            let sent = model.apply_action(action).command();
            assert!(
                matches!(sent, Some(crate::bridge::Command::ReadMail)),
                "{sent:?}"
            );
        }
        assert_eq!(Item::of(&model.route), Item::Mail);
    }

    #[test]
    fn dumps_is_always_there_between_mail_and_logs_and_counts_what_came_while_it_was_shut() {
        use marsa_core::types::{Dump, DumpList, DumpsStatus};
        let harness = Harness::new(egui::Theme::Light);
        let dumps = strings::section_name(Section::Dumps);
        let dump = |id: u64| Dump {
            id,
            received_at: 0,
            source_file: None,
            source_line: None,
            uri: None,
            method: None,
            label: None,
            text: "1".into(),
            truncated: false,
        };
        let mut model = connected();
        model.update(Update::Dumps {
            list: DumpList {
                status: DumpsStatus::Listening,
                latest_id: 2,
                count: 2,
                dumps: vec![dump(2), dump(1)],
            },
            replace: true,
        });
        let tree = harness.accessibility(|ui| {
            show(ui, &model, false);
        });
        let node = tree.node_with_label(dumps).expect("the Dumps item");
        assert_eq!(node.description(), Some("2 dumps"));
        let top = |label: &str| {
            tree.node_with_label(label)
                .expect(label)
                .bounds()
                .unwrap()
                .y0
        };
        assert!(top(strings::SITES) < top(dumps) && top(dumps) < top(strings::LOGS));
        // A click opens Dumps, which counts them seen and reads what is new.
        let actions = click(&harness, &model, false, dumps);
        assert_eq!(actions, [Action::Navigate(Route::Dumps)]);
        for action in actions {
            let sent = model.apply_action(action).command();
            assert!(
                matches!(sent, Some(crate::bridge::Command::ReadDumps)),
                "{sent:?}"
            );
        }
        let tree = harness.accessibility(|ui| {
            show(ui, &model, false);
        });
        let node = tree.node_with_label(dumps).expect("the Dumps item");
        assert_eq!(node.description(), None, "seen");
    }

    #[test]
    fn every_sidebar_item_sets_its_route_open_and_folded() {
        let harness = Harness::new(egui::Theme::Light);
        for folded in [false, true] {
            let mut model = connected();
            model.sidebar_folded = folded;
            for item in ALL {
                let actions = click(&harness, &model, false, item.label());
                assert_eq!(actions, [Action::Navigate(item.route())], "{item:?}");
                for action in actions {
                    // Dumps reads what is new as it opens, like Mail.
                    let sent = model.apply_action(action).command();
                    assert!(sent.is_none() || item == Item::Dumps, "{item:?}: {sent:?}");
                }
                assert_eq!(model.route, item.route());
                assert_eq!(Item::of(&model.route), item);
            }
        }
    }

    #[test]
    fn each_route_selects_its_item() {
        let routes = [
            (Route::Home, Item::Home),
            (Route::Sites, Item::Sites),
            (Route::Site("shop".into()), Item::Sites),
            (Route::Logs(LogSource::Caddy), Item::Logs),
            (Route::Mail, Item::Mail),
            (Route::Dumps, Item::Dumps),
            (Route::Runtimes(Runtime::Node), Item::Runtimes),
            (Route::Services, Item::Services),
            (Route::Settings, Item::Settings),
        ];
        for (route, item) in routes {
            assert_eq!(Item::of(&route), item, "{route:?}");
        }
        assert_eq!(Item::Runtimes.route(), Route::Runtimes(Runtime::Php));
    }

    #[test]
    fn the_runtimes_item_opens_the_runtime_the_screen_showed_last() {
        let harness = Harness::new(egui::Theme::Light);
        let mut model = connected();
        model.apply_action(Action::Navigate(Route::Runtimes(Runtime::Node)));
        model.apply_action(Action::Navigate(Route::Home));
        let actions = click(&harness, &model, false, Item::Runtimes.label());
        assert_eq!(actions, [Action::Navigate(Route::Runtimes(Runtime::Node))]);
    }

    #[test]
    fn the_status_opens_home_and_the_fold_button_folds() {
        let harness = Harness::new(egui::Theme::Light);
        let mut model = connected();
        model.route = Route::Settings;
        let (title, caption) = Overall::NoSites.text();
        let status = format!("{title}, {caption}");
        assert_eq!(
            click(&harness, &model, false, &status),
            [Action::Navigate(Route::Home)]
        );
        assert_eq!(
            click(&harness, &model, false, strings::HIDE_LABELS),
            [Action::FoldSidebar]
        );
        model.sidebar_folded = true;
        assert_eq!(
            click(&harness, &model, false, strings::SHOW_LABELS),
            [Action::FoldSidebar]
        );
    }

    #[test]
    fn a_narrow_window_folds_the_sidebar_and_its_button_does_nothing() {
        assert!(is_narrow(
            &LIGHT,
            LIGHT.size.sidebar_fold_below - LIGHT.size.border
        ));
        assert!(!is_narrow(&LIGHT, LIGHT.size.sidebar_fold_below));
        assert_eq!(LIGHT.size.sidebar_fold_below, 980.0);
        let harness = Harness::new(egui::Theme::Light);
        let model = connected();
        // Folded by the width: the button says what it would do, and is off.
        assert!(click(&harness, &model, true, strings::SHOW_LABELS).is_empty());
        // The items still work, by their tiles.
        assert_eq!(
            click(&harness, &model, true, strings::SITES),
            [Action::Navigate(Route::Sites)]
        );
    }

    #[test]
    fn the_status_is_an_error_only_when_no_site_opens() {
        let mut model = Model::default();
        model.update(Update::Failed("daemon_start_failed".into()));
        assert_eq!(health(model.overall()), Health::Error);
        assert_eq!(
            model.overall().text(),
            ("Sites not opening", "Marsa is not running")
        );
        assert_eq!(health(connected().overall()), Health::Ok);
        assert_eq!(health(Overall::Connecting), Health::Busy);
        assert_eq!(health(Overall::WebServerDown), Health::Error);
        assert_eq!(health(Overall::Working), Health::Ok);
    }

    #[test]
    fn the_name_stays_in_place_whatever_the_status_says() {
        let all = [
            Overall::Connecting,
            Overall::Stopping,
            Overall::DaemonDown,
            Overall::NoSites,
            Overall::Starting,
            Overall::WebServerDown,
            Overall::Working,
        ];
        let harness = Harness::new(egui::Theme::Light);
        let mut places = Vec::new();
        for overall in all {
            let mut model = model_in(overall);
            assert_eq!(model.overall(), overall);
            let tree = harness.accessibility(|ui| {
                show(ui, &model, false);
            });
            let name = tree.node_with_text(strings::APP_NAME).expect("the name");
            let name = name.bounds().expect("the name's place");
            // It starts where the tiles under it start.
            let home = tree.node_with_label(Item::Home.label()).expect("Home");
            let home = home.bounds().expect("Home's place");
            let tile = home.x0 + f64::from(LIGHT.size.nav_inset());
            assert!((name.x0 - tile).abs() < 0.01, "{overall:?}: {name:?}");
            places.push(name);
            // Folded, the sidebar shows no name, in any state.
            model.sidebar_folded = true;
            let tree = harness.accessibility(|ui| {
                show(ui, &model, false);
            });
            assert!(
                tree.node_with_text(strings::APP_NAME).is_none(),
                "{overall:?}"
            );
        }
        assert!(places.iter().all(|place| *place == places[0]), "{places:?}");
    }

    #[test]
    fn the_sidebar_insets_make_the_folded_sidebar_one_column_of_square_items() {
        let size = &LIGHT.size;
        assert_eq!(
            size.sidebar_inset() + size.nav_item + size.sidebar_inset(),
            size.sidebar_folded
        );
        assert_eq!(
            size.nav_inset() + size.nav_tile + size.nav_inset(),
            size.nav_item
        );
    }
}
