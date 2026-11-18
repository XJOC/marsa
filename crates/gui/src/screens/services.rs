//! Services (layout.md, Screens): one row per program the daemon runs, with
//! what it does, its state as an icon, a word, and a color, and the actions
//! that make sense in that state.
//!
//! The screen is generic over what the daemon lists: Caddy, each PHP pool,
//! and any program a later release adds. Only [`strings::service_name`] and
//! [`strings::service_description`] know a program by name, and a program
//! they do not know shows under its id with a plain line. Like Home and
//! Sites it is two functions. [`view`] reads the [`Model`] and decides what
//! the screen says: pure, so the tests assert it without a window. [`show`]
//! draws that [`ServicesView`] with the widgets and returns what the user
//! did as [`Action`]s (code.md, State).

use eframe::egui::{Role, Ui, WidgetInfo};
use marsa_core::types::{ServiceAction, ServiceState, ServiceStatus};

use super::sites::{message, mute};
use crate::model::{self, Action, Model};
use crate::strings;
use crate::theme::page::{SERVICES_ACTIONS_COLUMN, SERVICES_STATE_COLUMN};
use crate::theme::{Direction, Section, Theme};
use crate::widgets::{Button, LineTone, PillState, RowHeight, StatePill, Table, TableColumn};

/// The buttons a program may have, in the order they are drawn.
const ACTIONS: [ServiceAction; 3] = [
    ServiceAction::Start,
    ServiceAction::Restart,
    ServiceAction::Stop,
];

/// What Services says.
#[derive(Debug, PartialEq)]
pub enum ServicesView<'a> {
    /// No daemon: there is nothing to list.
    Offline,
    /// The daemon runs and lists no program.
    Empty,
    Rows {
        rows: Vec<ServiceRow<'a>>,
        /// A PHP pool is among them: a line under the table says why its
        /// buttons are off.
        pools: bool,
    },
}

/// One program.
#[derive(Debug, PartialEq)]
pub struct ServiceRow<'a> {
    /// What an action names the program by.
    pub id: &'a str,
    pub name: String,
    /// What it does, under its name.
    pub description: String,
    pub state: PillState,
    /// Why it failed, when it did: it takes the place of the description,
    /// in `error`.
    pub failure: Option<String>,
    /// The process id, while it runs.
    pub pid: Option<u32>,
    /// Only the buttons that make sense in the state.
    pub actions: Vec<ServiceButton>,
}

/// A button of a row.
#[derive(Debug, PartialEq, Eq)]
pub struct ServiceButton {
    pub action: ServiceAction,
    /// Why it cannot be pressed now: it is shown, and disabled.
    pub off: Option<&'static str>,
}

/// Decides what Services says. Pure: the same model gives the same view.
pub fn view(model: &Model) -> ServicesView<'_> {
    if !model.is_connected() {
        return ServicesView::Offline;
    }
    if model.services.is_empty() {
        return ServicesView::Empty;
    }
    let rows: Vec<_> = model
        .services
        .iter()
        .map(|service| row(model, service))
        .collect();
    let pools = model.services.iter().any(|s| model::is_pool(&s.id));
    ServicesView::Rows { rows, pools }
}

fn row<'a>(model: &Model, service: &'a ServiceStatus) -> ServiceRow<'a> {
    let pool = model::pool_of(&service.id);
    let state = pill(pool.is_some(), &service.state);
    let sites = pool.map_or(0, |(_, line)| model.sites_on_line(line));
    let failure = match &service.state {
        ServiceState::Failed { code } => Some(strings::error_text(code)),
        _ => None,
    };
    // A pool starts and stops with the sites that need it: the daemon
    // refuses every action on it, so each button is off, with that reason.
    let off = pool.map(|_| strings::MANAGED_BY_SITES_HINT);
    ServiceRow {
        id: &service.id,
        name: strings::service_name(&service.id),
        description: strings::service_description(&service.id, state == PillState::Waiting, sites),
        state,
        failure,
        pid: service.pid,
        actions: ACTIONS
            .into_iter()
            .filter(|action| model::can(&service.state, *action))
            .map(|action| ServiceButton { action, off })
            .collect(),
    }
}

/// The pill of a state. A pool that is stopped is not broken: no site needs
/// it now, so it waits (components.md, Status).
fn pill(pool: bool, state: &ServiceState) -> PillState {
    match state {
        ServiceState::Running => PillState::Running,
        ServiceState::Starting => PillState::Starting,
        ServiceState::Stopping => PillState::Stopping,
        ServiceState::Stopped if pool => PillState::Waiting,
        ServiceState::Stopped => PillState::Stopped,
        ServiceState::Failed { .. } => PillState::Failed,
    }
}

/// Draws `view`. Returns what the user did.
pub fn show(ui: &mut Ui, view: &ServicesView<'_>, theme: &Theme) -> Vec<Action> {
    let mut actions = Vec::new();
    ui.spacing_mut().item_spacing.y = theme.space.section_gap;
    super::header_with_hint(
        ui,
        theme,
        Section::Services,
        strings::SERVICES,
        Some(strings::SERVICES_HINT),
        |_| {},
    );
    match view {
        ServicesView::Offline => message(ui, theme, strings::SERVICES_OFFLINE),
        ServicesView::Empty => message(ui, theme, strings::NO_SERVICES),
        ServicesView::Rows { rows, pools } => {
            table(ui, rows, &mut actions);
            if *pools {
                mute(ui, theme, strings::SERVICES_POOLS_NOTE);
            }
        }
    }
    actions
}

/// The rows: the name over its line, the state, and the buttons at the end.
fn table(ui: &mut Ui, rows: &[ServiceRow<'_>], actions: &mut Vec<Action>) {
    let columns = [
        TableColumn::fill(strings::COLUMN_SERVICE),
        TableColumn::fixed(strings::COLUMN_STATE, SERVICES_STATE_COLUMN),
        TableColumn::fixed(strings::COLUMN_ACTIONS, SERVICES_ACTIONS_COLUMN),
    ];
    // The rows describe themselves: no header row.
    let table = Table::new("services", strings::SERVICES, &columns)
        .headerless()
        .rows(RowHeight::TwoLine);
    table.show(ui, rows.len(), |row| {
        let service = &rows[row.index()];
        match &service.failure {
            Some(reason) => row.two_lines_toned(&service.name, reason, reason, LineTone::Error),
            None => row.two_lines(&service.name, &service.description),
        }
        row.cell(|ui| {
            let state = ui.add(StatePill::new(service.state));
            if let Some(pid) = service.pid {
                state.on_hover_text(strings::service_pid(pid));
            }
        });
        row.cell(|ui| {
            // From the end, so Stop is the last thing in the row: the
            // buttons are added the other way round to read in order.
            ui.with_layout(Direction::CURRENT.row_from_end(), |ui| {
                for button in service.actions.iter().rev() {
                    if press(ui, service, button) {
                        actions.push(Action::Service {
                            service: service.id.to_owned(),
                            action: button.action,
                        });
                    }
                }
            });
        });
    });
}

/// One button. Whether it was pressed. A screen reader hears which program
/// it is for ("Restart Caddy").
fn press(ui: &mut Ui, service: &ServiceRow<'_>, button: &ServiceButton) -> bool {
    let widget = Button::secondary(strings::action_text(button.action))
        .in_row()
        .disabled_if(button.off.is_some(), button.off.unwrap_or_default());
    let response = ui.add(widget);
    let (enabled, label) = (
        response.enabled(),
        strings::service_action_label(button.action, &service.name),
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, &label));
    response.clicked()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use eframe::egui::{self, Pos2, pos2};
    use marsa_core::types::{Health as DaemonHealth, Site, SiteKind, SiteList};

    use super::*;
    use crate::model::Update;
    use crate::theme::LIGHT;
    use crate::widgets::harness::{Harness, Tree};

    fn connected() -> Model {
        let mut model = Model::default();
        model.update(Update::Connected(DaemonHealth {
            app_version: "0.0.0".into(),
            api_version: 1,
            pid: 7,
        }));
        model
    }

    fn service(id: &str, state: ServiceState) -> ServiceStatus {
        ServiceStatus {
            id: id.into(),
            pid: (state == ServiceState::Running).then_some(4242),
            state,
            restarts: 0,
            since_ms: None,
        }
    }

    fn model(services: Vec<ServiceStatus>) -> Model {
        let mut model = connected();
        model.update(Update::Services(services));
        model
    }

    fn site(name: &str, php: &str) -> Site {
        let root = PathBuf::from("/code").join(name);
        Site {
            name: name.into(),
            docroot: root.join("public"),
            root,
            kind: SiteKind::Laravel,
            php: php.parse().ok(),
            php_source: None,
            secure: true,
        }
    }

    fn rows<'a, 'b>(view: &'b ServicesView<'a>) -> &'b [ServiceRow<'a>] {
        match view {
            ServicesView::Rows { rows, .. } => rows,
            other => panic!("expected rows, got {other:?}"),
        }
    }

    /// The one row of a model with one service.
    fn only(id: &str, state: ServiceState) -> ServiceRowCopy {
        let model = model(vec![service(id, state)]);
        let view = view(&model);
        let row = &rows(&view)[0];
        ServiceRowCopy {
            state: row.state,
            actions: row.actions.iter().map(|b| (b.action, b.off)).collect(),
            failure: row.failure.clone(),
        }
    }

    /// What a test reads of a row, so the view can go out of scope.
    #[derive(Debug)]
    struct ServiceRowCopy {
        state: PillState,
        actions: Vec<(ServiceAction, Option<&'static str>)>,
        failure: Option<String>,
    }

    // -- view --

    #[test]
    fn every_state_of_caddy_has_its_word_and_only_the_actions_that_make_sense() {
        use ServiceAction::{Restart, Start, Stop};
        let failed = ServiceState::Failed {
            code: "port_in_use".into(),
        };
        let cases = [
            (
                ServiceState::Running,
                PillState::Running,
                vec![Restart, Stop],
            ),
            (
                ServiceState::Starting,
                PillState::Starting,
                vec![Restart, Stop],
            ),
            // On its way down: nothing to press until it is down.
            (ServiceState::Stopping, PillState::Stopping, vec![]),
            (ServiceState::Stopped, PillState::Stopped, vec![Start]),
            (failed, PillState::Failed, vec![Start]),
        ];
        for (state, pill, want) in cases {
            let row = only("caddy", state.clone());
            assert_eq!(row.state, pill, "{state:?}");
            let actions: Vec<_> = row.actions.iter().map(|(action, _)| *action).collect();
            assert_eq!(actions, want, "{state:?}");
            // Caddy's buttons all work: none is off.
            assert!(
                row.actions.iter().all(|(_, off)| off.is_none()),
                "{state:?}"
            );
            // A button is shown only where the model says it makes sense.
            for (action, _) in &row.actions {
                assert!(model::can(&state, *action), "{state:?} {action:?}");
            }
        }
    }

    #[test]
    fn a_failed_service_says_why_in_words_from_its_code() {
        let row = only(
            "caddy",
            ServiceState::Failed {
                code: "port_in_use".into(),
            },
        );
        assert_eq!(row.failure, Some(strings::error_text("port_in_use")));
        // A service that runs, waits, or stopped by itself has no failure.
        for state in [
            ServiceState::Running,
            ServiceState::Stopped,
            ServiceState::Starting,
            ServiceState::Stopping,
        ] {
            assert_eq!(only("caddy", state).failure, None);
        }
        // A code the window does not know still says something.
        let odd = only(
            "caddy",
            ServiceState::Failed {
                code: "brand_new".into(),
            },
        );
        assert!(odd.failure.is_some_and(|text| text.contains("brand_new")));
    }

    #[test]
    fn a_pool_that_is_stopped_waits_and_every_button_of_a_pool_is_off_with_the_reason() {
        use ServiceAction::{Restart, Start, Stop};
        let why = Some(strings::MANAGED_BY_SITES_HINT);
        for id in ["php-fpm-8.4", "php-cgi-8.3"] {
            let running = only(id, ServiceState::Running);
            assert_eq!(running.state, PillState::Running);
            assert_eq!(running.actions, [(Restart, why), (Stop, why)], "{id}");
            // Not broken: no site needs it now, so it waits for one.
            let stopped = only(id, ServiceState::Stopped);
            assert_eq!(stopped.state, PillState::Waiting, "{id}");
            assert_eq!(stopped.actions, [(Start, why)], "{id}");
            // A pool that failed is broken, whatever starts it.
            let failed = only(
                id,
                ServiceState::Failed {
                    code: "php_fpm_orphan_stuck".into(),
                },
            );
            assert_eq!(failed.state, PillState::Failed, "{id}");
            assert!(failed.failure.is_some());
            assert_eq!(failed.actions, [(Start, why)], "{id}");
        }
    }

    #[test]
    fn a_program_the_window_does_not_know_shows_under_its_id_with_a_plain_line() {
        let model = model(vec![service("mailpit", ServiceState::Running)]);
        let view = view(&model);
        let row = &rows(&view)[0];
        assert_eq!((row.id, row.name.as_str()), ("mailpit", "mailpit"));
        assert_eq!(row.description, "A program Marsa runs for your sites.");
        // It is not a pool: its buttons work.
        assert!(row.actions.iter().all(|button| button.off.is_none()));
        assert_eq!(row.actions.len(), 2);
        // No line under the table about pools.
        assert!(matches!(view, ServicesView::Rows { pools: false, .. }));
    }

    #[test]
    fn each_program_is_named_and_described_and_a_pool_counts_its_sites() {
        let mut model = model(vec![
            service("php-fpm-8.4", ServiceState::Running),
            service("php-fpm-8.3", ServiceState::Stopped),
            service("php-fpm-8.2", ServiceState::Running),
            service("caddy", ServiceState::Running),
        ]);
        model.update(Update::Sites(SiteList {
            sites: vec![
                site("shop", "8.4.27"),
                site("blog", "8.4.1"),
                site("wiki", "8.3.27"),
                site("legacy", "8.40.0"),
            ],
            ..SiteList::default()
        }));
        let view = view(&model);
        let text: Vec<_> = rows(&view)
            .iter()
            .map(|row| (row.name.as_str(), row.description.as_str()))
            .collect();
        // Sorted by id: Caddy first.
        assert_eq!(
            text,
            [
                ("Caddy", "The web server for every .test site."),
                ("PHP-FPM 8.2", "Runs PHP 8.2 for your sites."),
                (
                    "PHP-FPM 8.3",
                    "No site needs PHP 8.3 now. It starts when one does."
                ),
                ("PHP-FPM 8.4", "Runs PHP 8.4 for 2 sites."),
            ]
        );
        assert!(matches!(view, ServicesView::Rows { pools: true, .. }));
        // One site is a singular.
        model.update(Update::Sites(SiteList {
            sites: vec![site("shop", "8.4.27")],
            ..SiteList::default()
        }));
        let view = super::view(&model);
        assert_eq!(rows(&view)[3].description, "Runs PHP 8.4 for 1 site.");
        assert_eq!(strings::service_name("php-cgi-8.4"), "PHP-CGI 8.4");
    }

    #[test]
    fn no_daemon_and_no_service_are_not_a_table() {
        let none = Model::default();
        assert_eq!(view(&none), ServicesView::Offline);
        let empty = connected();
        assert_eq!(view(&empty), ServicesView::Empty);
        // A window that lost its daemon lists nothing.
        let mut lost = model(vec![service("caddy", ServiceState::Running)]);
        lost.update(Update::Reconnecting);
        assert_eq!(view(&lost), ServicesView::Offline);
    }

    // -- show --

    fn bounds_center(tree: &Tree, label: &str) -> Pos2 {
        let node = tree
            .node_with_label(label)
            .unwrap_or_else(|| panic!("no widget named {label}"));
        let b = node.bounds().expect("a node with bounds");
        pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32)
    }

    fn draw(ui: &mut egui::Ui, model: &Model) -> Vec<Action> {
        show(ui, &view(model), &LIGHT)
    }

    fn start(model: &Model) -> (Harness, Tree) {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            draw(ui, model);
        });
        (harness, tree)
    }

    fn click(model: &Model, label: &str) -> Vec<Action> {
        let (harness, tree) = start(model);
        let mut actions = Vec::new();
        harness.click(bounds_center(&tree, label), |ui| {
            actions.extend(draw(ui, model));
        });
        actions
    }

    fn two_services() -> Model {
        model(vec![
            service("caddy", ServiceState::Running),
            service("php-fpm-8.4", ServiceState::Running),
        ])
    }

    #[test]
    fn the_screen_names_each_program_its_state_and_each_button_by_its_program() {
        let (_, tree) = start(&two_services());
        assert!(tree.node_with_text("Services").is_some());
        assert!(
            tree.node_with_text("Programs Marsa runs for your sites")
                .is_some()
        );
        assert!(
            tree.node_with_text("Caddy, The web server for every .test site.")
                .is_some()
        );
        // The state is a word, read as a status.
        assert_eq!(tree.nodes_with_label("Running").len(), 2);
        // Many buttons say "Restart": each says which program it is for.
        for label in [
            "Restart Caddy",
            "Stop Caddy",
            "Restart PHP-FPM 8.4",
            "Stop PHP-FPM 8.4",
        ] {
            assert!(tree.node_with_label(label).is_some(), "{label}");
        }
        // The line about pools is there while a pool is.
        assert!(tree.node_with_text(strings::SERVICES_POOLS_NOTE).is_some());
        let (_, caddy_only) = start(&model(vec![service("caddy", ServiceState::Running)]));
        assert!(
            caddy_only
                .node_with_text(strings::SERVICES_POOLS_NOTE)
                .is_none()
        );
    }

    #[test]
    fn a_button_of_caddy_returns_its_action_and_a_button_of_a_pool_does_nothing() {
        let model = two_services();
        assert_eq!(
            click(&model, "Restart Caddy"),
            [Action::Service {
                service: "caddy".into(),
                action: ServiceAction::Restart,
            }]
        );
        assert_eq!(
            click(&model, "Stop Caddy"),
            [Action::Service {
                service: "caddy".into(),
                action: ServiceAction::Stop,
            }]
        );
        // The pool's buttons are there, and off.
        assert!(click(&model, "Restart PHP-FPM 8.4").is_empty());
        assert!(click(&model, "Stop PHP-FPM 8.4").is_empty());
        let stopped = model_with_stopped_caddy();
        assert_eq!(
            click(&stopped, "Start Caddy"),
            [Action::Service {
                service: "caddy".into(),
                action: ServiceAction::Start,
            }]
        );
    }

    fn model_with_stopped_caddy() -> Model {
        model(vec![service("caddy", ServiceState::Stopped)])
    }

    #[test]
    fn the_buttons_follow_the_state_and_the_pid_is_in_the_tooltip_of_the_state() {
        let (_, tree) = start(&model_with_stopped_caddy());
        assert!(tree.node_with_label("Start Caddy").is_some());
        assert!(tree.node_with_label("Stop Caddy").is_none());
        assert!(tree.node_with_label("Restart Caddy").is_none());
        // A stopped program has no process.
        let stopped = only("caddy", ServiceState::Stopped);
        assert_eq!(stopped.state, PillState::Stopped);
        assert_eq!(strings::service_pid(4242), "Process 4242");
    }

    #[test]
    fn a_failed_service_shows_its_reason_in_place_of_its_line() {
        let model = model(vec![service(
            "caddy",
            ServiceState::Failed {
                code: "port_in_use".into(),
            },
        )]);
        let (_, tree) = start(&model);
        let reason = strings::error_text("port_in_use");
        assert!(
            tree.node_with_text(&format!("Caddy, {reason}")).is_some(),
            "the name and the reason are one cell"
        );
        assert!(tree.node_with_label("Failed").is_some());
        assert!(tree.node_with_label("Start Caddy").is_some());
    }

    #[test]
    fn offline_and_empty_say_so_and_still_have_the_header() {
        for (model, text) in [
            (Model::default(), strings::SERVICES_OFFLINE),
            (connected(), strings::NO_SERVICES),
        ] {
            let (_, tree) = start(&model);
            assert!(tree.node_with_text("Services").is_some());
            assert!(tree.node_with_text(text).is_some(), "{text}");
        }
    }
}
