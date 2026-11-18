//! The sidebar (layout.md, Shell; components.md, Sidebar): the name and the
//! fold button, the items in their groups, then the status and Settings at
//! the bottom. Only the sections that ship appear (rules.md 8): Phase 2 adds
//! Databases, Mail, Dumps, and Queues to these groups.

use eframe::egui::{Layout, RichText, Ui, vec2};
use marsa_core::runtimes::Runtime;

use crate::model::{Action, LogSource, Model, Overall, Route};
use crate::strings;
use crate::theme::{Direction, Face, Section, TextToken, Theme};
use crate::widgets::{Button, Health, Icon, NavItem, StatusDot};

/// The runtime the Runtimes item opens: the main one.
const MAIN_RUNTIME: Runtime = Runtime::Php;

/// An item of the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Item {
    Home,
    Sites,
    Logs,
    Runtimes,
    Services,
    /// At the bottom, under the status.
    Settings,
}

/// The items above the status, in their groups, in order.
pub const GROUPS: [&[Item]; 4] = [
    &[Item::Home],
    &[Item::Sites],
    &[Item::Logs],
    &[Item::Runtimes, Item::Services],
];

impl Item {
    /// The route a click on the item opens.
    pub fn route(self) -> Route {
        match self {
            Self::Home => Route::Home,
            Self::Sites => Route::Sites,
            Self::Logs => Route::Logs(LogSource::default()),
            Self::Runtimes => Route::Runtimes(MAIN_RUNTIME),
            Self::Services => Route::Services,
            Self::Settings => Route::Settings,
        }
    }

    /// The item a route belongs to: a site's page is under Sites.
    pub fn of(route: &Route) -> Self {
        match route {
            Route::Home => Self::Home,
            Route::Sites | Route::Site(_) => Self::Sites,
            Route::Logs(_) => Self::Logs,
            Route::Runtimes(_) => Self::Runtimes,
            Route::Services => Self::Services,
            Route::Settings => Self::Settings,
        }
    }

    /// The section whose tile the item shows. Settings has a plain icon.
    pub const fn section(self) -> Option<Section> {
        match self {
            Self::Home => Some(Section::Home),
            Self::Sites => Some(Section::Sites),
            Self::Logs => Some(Section::Logs),
            Self::Runtimes => Some(Section::Runtimes),
            Self::Services => Some(Section::Services),
            Self::Settings => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self.section() {
            Some(section) => strings::section_name(section),
            None => strings::SETTINGS,
        }
    }
}

/// A window narrower than this folds the sidebar, whatever the user chose.
pub fn is_narrow(theme: &Theme, window_width: f32) -> bool {
    window_width < theme.size.sidebar_fold_below
}

/// The mark of the status at the bottom: an error when no site opens.
pub const fn health(overall: Overall) -> Health {
    match overall {
        Overall::Connecting | Overall::Stopping | Overall::Starting => Health::Busy,
        Overall::DaemonDown | Overall::WebServerDown => Health::Error,
        Overall::NoSites | Overall::Working => Health::Ok,
    }
}

/// Draws the sidebar inside its panel. `narrow`: the window is too narrow
/// for labels. Returns what the user did.
pub fn show(ui: &mut Ui, model: &Model, narrow: bool) -> Vec<Action> {
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
        for &item in *group {
            if ui.add(nav(model, item, current, folded)).clicked() {
                actions.push(Action::Navigate(item.route()));
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
        let (title, caption) = strings::overall_text(overall);
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

/// The widget of `item`: Sites counts the sites.
fn nav(model: &Model, item: Item, current: Item, folded: bool) -> NavItem<'static> {
    let shown = match item.section() {
        Some(section) => NavItem::section(section),
        None => NavItem::plain(Icon::Settings, item.label()),
    };
    let shown = shown.selected(item == current).folded(folded);
    let sites = model.sites.sites.len();
    if item == Item::Sites && sites > 0 {
        return shown.count(u32::try_from(sites).unwrap_or(u32::MAX), false);
    }
    shown
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Pos2, pos2};
    use marsa_core::types::Health as DaemonHealth;

    use super::*;
    use crate::model::Update;
    use crate::theme::LIGHT;
    use crate::widgets::harness::{Harness, Tree};

    /// Every item: the groups, then Settings.
    const ALL: [Item; 6] = [
        Item::Home,
        Item::Sites,
        Item::Logs,
        Item::Runtimes,
        Item::Services,
        Item::Settings,
    ];

    fn connected() -> Model {
        let mut model = Model::default();
        model.update(Update::Connected(DaemonHealth {
            app_version: "0.0.0".into(),
            api_version: 1,
            pid: 7,
        }));
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
    fn the_sidebar_shows_the_phase_one_sections_in_their_order() {
        let order: Vec<Item> = GROUPS
            .iter()
            .flat_map(|group| group.iter().copied())
            .collect();
        assert_eq!(
            order,
            [
                Item::Home,
                Item::Sites,
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
        for section in [
            Section::Databases,
            Section::Mail,
            Section::Dumps,
            Section::Queues,
        ] {
            let name = strings::section_name(section);
            assert!(tree.node_with_label(name).is_none(), "{name} ships later");
        }
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
                    assert!(model.apply_action(action).is_none());
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
    fn the_status_opens_home_and_the_fold_button_folds() {
        let harness = Harness::new(egui::Theme::Light);
        let mut model = connected();
        model.route = Route::Settings;
        let (title, caption) = strings::overall_text(Overall::NoSites);
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
            strings::overall_text(model.overall()),
            ("Sites not opening", "Marsa is not running")
        );
        assert_eq!(health(connected().overall()), Health::Ok);
        assert_eq!(health(Overall::Connecting), Health::Busy);
        assert_eq!(health(Overall::WebServerDown), Health::Error);
        assert_eq!(health(Overall::Working), Health::Ok);
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
