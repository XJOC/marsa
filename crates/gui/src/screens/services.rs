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

use super::lines::{message, mute};
use crate::model::{self, Action, LogSource, Model, Route};
use crate::strings;
use crate::theme::page::{SERVICES_ACTIONS_COLUMN, SERVICES_STATE_COLUMN};
use crate::theme::{Direction, Section, Theme};
use crate::widgets::table::wrapped_row_height;
use crate::widgets::{Button, LineTone, PillState, StatePill, Table, TableColumn};

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
    /// Only the buttons that make sense in the state, in the order they are
    /// drawn. A pool has none: the sites start and stop it, and the line
    /// under the table says so.
    pub actions: Vec<RowButton>,
}

/// A button of a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowButton {
    /// Start, Restart, or Stop.
    Service(ServiceAction),
    /// A service that failed and wrote a log: the Logs screen opens on it.
    ShowLog,
}

/// Decides what Services says. Pure: the same model gives the same view.
pub fn view(model: &Model) -> ServicesView<'_> {
    if !model.is_connected() {
        return ServicesView::Offline;
    }
    if model.session.services.is_empty() {
        return ServicesView::Empty;
    }
    let rows: Vec<_> = model
        .session
        .services
        .iter()
        .map(|service| row(model, service))
        .collect();
    let pools = model.session.services.iter().any(|s| model::is_pool(&s.id));
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
    ServiceRow {
        id: &service.id,
        name: model::service_name(&service.id),
        description: model::service_description(&service.id, state == PillState::Waiting, sites),
        state,
        failure,
        pid: service.pid,
        actions: buttons(model, service, pool.is_some()),
    }
}

/// The buttons of a row. A pool starts and stops with the sites that need
/// it, and the daemon refuses every action on it, so it has none: a button
/// that cannot work is not shown (the line under the table says why). A
/// service that failed and wrote a log may show that log.
fn buttons(model: &Model, service: &ServiceStatus, pool: bool) -> Vec<RowButton> {
    let mut buttons = Vec::new();
    if matches!(service.state, ServiceState::Failed { .. })
        && !model.log_rows(&service.id).is_empty()
    {
        buttons.push(RowButton::ShowLog);
    }
    if !pool {
        let actions = ACTIONS.into_iter();
        buttons.extend(
            actions
                .filter(|action| model::can(&service.state, *action))
                .map(RowButton::Service),
        );
    }
    buttons
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

impl ServiceRow<'_> {
    /// The second line of the row: why it failed, or what it does.
    fn note(&self) -> &str {
        self.failure.as_deref().unwrap_or(&self.description)
    }
}

/// The rows: the name over its line, the state, and the buttons at the end.
/// The line wraps in at most two lines and is never cut, so a row is as high
/// as its lines need.
fn table(ui: &mut Ui, rows: &[ServiceRow<'_>], actions: &mut Vec<Action>) {
    let columns = [
        TableColumn::fill(strings::COLUMN_SERVICE),
        TableColumn::fixed(strings::COLUMN_STATE, SERVICES_STATE_COLUMN),
        TableColumn::fixed(strings::COLUMN_ACTIONS, SERVICES_ACTIONS_COLUMN),
    ];
    // The rows describe themselves: no header row.
    let base = Table::new("services", strings::SERVICES, &columns).headerless();
    let width = base.fill_width(ui);
    let heights: Vec<f32> = rows
        .iter()
        .map(|service| wrapped_row_height(ui, width, service.note()))
        .collect();
    base.heights(&heights).show(ui, rows.len(), |row| {
        let service = &rows[row.index()];
        let tone = if service.failure.is_some() {
            LineTone::Error
        } else {
            LineTone::Mute
        };
        let note = service.note();
        row.two_lines_wrapped(&service.name, note, note, tone);
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
                    if press(ui, service, *button) {
                        actions.push(match button {
                            RowButton::Service(action) => Action::Service {
                                service: service.id.to_owned(),
                                action: *action,
                            },
                            RowButton::ShowLog => {
                                Action::Navigate(Route::Logs(LogSource::of(service.id)))
                            }
                        });
                    }
                }
            });
        });
    });
}

/// One button. Whether it was pressed. A screen reader hears which program
/// it is for ("Restart Caddy"). The ones that act on the program are
/// secondary; "Show log" is quiet, a way to read more.
fn press(ui: &mut Ui, service: &ServiceRow<'_>, button: RowButton) -> bool {
    let (widget, label) = match button {
        RowButton::Service(action) => (
            Button::secondary(strings::action_text(action)),
            strings::service_action_label(action, &service.name),
        ),
        RowButton::ShowLog => (
            Button::quiet(strings::SHOW_LOG),
            strings::service_show_log_label(&service.name),
        ),
    };
    let response = ui.add(widget.in_row());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
    response.clicked()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use eframe::egui::{self, Pos2, pos2};
    use marsa_core::types::{LogLine, LogStream, Site, SiteKind, SiteList};

    use super::*;
    use crate::fixtures::connected;
    use crate::model::Update;
    use crate::theme::LIGHT;
    use crate::widgets::harness::{Harness, Tree};

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
        only_with(id, state, false)
    }

    /// The one row of a model with one service, whose log has a line if
    /// `logged`.
    fn only_with(id: &str, state: ServiceState, logged: bool) -> ServiceRowCopy {
        let mut model = model(vec![service(id, state)]);
        if logged {
            model.update(Update::Log(vec![LogLine {
                service: id.into(),
                seq: 0,
                at_ms: 0,
                stream: LogStream::Stderr,
                text: "failed".into(),
            }]));
        }
        let view = view(&model);
        let row = &rows(&view)[0];
        ServiceRowCopy {
            state: row.state,
            actions: row.actions.clone(),
            failure: row.failure.clone(),
        }
    }

    /// What a test reads of a row, so the view can go out of scope.
    #[derive(Debug)]
    struct ServiceRowCopy {
        state: PillState,
        actions: Vec<RowButton>,
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
            let want: Vec<_> = want.into_iter().map(RowButton::Service).collect();
            assert_eq!(row.actions, want, "{state:?}");
            // A button is shown only where the model says it makes sense.
            for button in &row.actions {
                let RowButton::Service(action) = button else {
                    panic!("only service buttons here: {button:?}");
                };
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
    fn a_pool_that_is_stopped_waits_and_a_pool_has_no_action_button() {
        for id in ["php-fpm-8.4", "php-cgi-8.3"] {
            let running = only(id, ServiceState::Running);
            assert_eq!(running.state, PillState::Running);
            assert_eq!(running.actions, [], "{id}");
            // Not broken: no site needs it now, so it waits for one.
            let stopped = only(id, ServiceState::Stopped);
            assert_eq!(stopped.state, PillState::Waiting, "{id}");
            assert_eq!(stopped.actions, [], "{id}");
            // A pool that failed is broken, whatever starts it. Its log, if it
            // wrote one, is a button; the sites start it, so there is no Start.
            let failed = ServiceState::Failed {
                code: "php_fpm_orphan_stuck".into(),
            };
            let without = only(id, failed.clone());
            assert_eq!(without.state, PillState::Failed, "{id}");
            assert!(without.failure.is_some());
            assert_eq!(without.actions, [], "{id}");
            let with = only_with(id, failed, true);
            assert_eq!(with.actions, [RowButton::ShowLog], "{id}");
        }
    }

    #[test]
    fn a_failed_service_with_a_log_shows_it_before_its_start_and_no_other_state_does() {
        use ServiceAction::Start;
        let failed = ServiceState::Failed {
            code: "port_in_use".into(),
        };
        assert_eq!(
            only_with("caddy", failed.clone(), true).actions,
            [RowButton::ShowLog, RowButton::Service(Start)]
        );
        // No line written: nothing to show.
        assert_eq!(only("caddy", failed).actions, [RowButton::Service(Start)]);
        // A program that is fine has no use for it, log or not.
        for state in [
            ServiceState::Running,
            ServiceState::Stopped,
            ServiceState::Starting,
            ServiceState::Stopping,
        ] {
            let row = only_with("caddy", state.clone(), true);
            assert!(!row.actions.contains(&RowButton::ShowLog), "{state:?}");
        }
    }

    #[test]
    fn a_program_the_window_does_not_know_shows_under_its_id_with_a_plain_line() {
        let model = model(vec![service("mailpit", ServiceState::Running)]);
        let view = view(&model);
        let row = &rows(&view)[0];
        assert_eq!((row.id, row.name.as_str()), ("mailpit", "mailpit"));
        assert_eq!(row.description, "A program Marsa runs for your sites.");
        // It is not a pool: it has its buttons.
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
                    "No site needs it now. It starts when one does."
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
        assert_eq!(model::service_name("php-cgi-8.4"), "PHP-CGI 8.4");
        assert_eq!(model::service_name("dns"), "DNS");
        assert_eq!(
            model::service_description("dns", false, 0),
            "Finds every .test site on this computer."
        );
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
        for label in ["Restart Caddy", "Stop Caddy"] {
            assert!(tree.node_with_label(label).is_some(), "{label}");
        }
        // A pool has no button: the sites start and stop it.
        for label in [
            "Restart PHP-FPM 8.4",
            "Stop PHP-FPM 8.4",
            "Start PHP-FPM 8.4",
        ] {
            assert!(tree.node_with_label(label).is_none(), "{label}");
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
    fn a_button_of_caddy_returns_its_action() {
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
    fn show_log_opens_the_log_of_that_service_and_is_named_for_it() {
        let mut model = model(vec![
            service("caddy", ServiceState::Running),
            service(
                "php-fpm-8.2",
                ServiceState::Failed {
                    code: "spawn_failed".into(),
                },
            ),
        ]);
        model.update(Update::Log(vec![LogLine {
            service: "php-fpm-8.2".into(),
            seq: 0,
            at_ms: 0,
            stream: LogStream::Stderr,
            text: "[18-Nov-2026 14:30:00] ERROR: cannot bind".into(),
        }]));
        let (_, tree) = start(&model);
        assert!(tree.node_with_label("Show log of PHP-FPM 8.2").is_some());
        // Caddy runs: no log to show.
        assert!(tree.node_with_label("Show log of Caddy").is_none());
        assert_eq!(
            click(&model, "Show log of PHP-FPM 8.2"),
            [Action::Navigate(Route::Logs(LogSource::Service(
                "php-fpm-8.2".into()
            )))]
        );
    }

    /// A frame of the window at `width` wide (the narrowest the window opens
    /// is 760 with the sidebar folded to 64, and 32 on each side: 632).
    fn tree_at(model: &Model, width: f32) -> (Harness, Tree) {
        let harness = Harness::new(egui::Theme::Light);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(width, 900.0),
            )),
            ..egui::RawInput::default()
        };
        let tree = harness.frame(input, |ui| {
            draw(ui, model);
        });
        (harness, tree)
    }

    #[test]
    fn every_line_a_service_can_have_fits_in_two_lines_at_the_narrowest_window() {
        use crate::widgets::table::{NOTE_LINES, note_galley};
        // The codes a program fails with (`ServiceState::Failed`), and every
        // description, at the width the fill column has at 760.
        let codes = [
            "caddy_not_installed",
            "caddy_config_write",
            "caddy_start_timeout",
            "port_in_use",
            "caddy_orphan_stuck",
            "caddy_other_marsa",
            "php_fpm_orphan_stuck",
            "exited",
            marsa_core::codes::DNS_PORT_IN_USE,
            "dns_start_failed",
            "spawn_failed",
            "program_not_absolute",
            "internal_error",
            "brand_new_code",
        ];
        let mut texts: Vec<String> = codes.iter().map(|code| strings::error_text(code)).collect();
        for id in ["caddy", "dns", "php-fpm-8.4", "php-cgi-8.4", "mailpit"] {
            for waiting in [false, true] {
                for sites in [0, 1, 25] {
                    texts.push(model::service_description(id, waiting, sites));
                }
            }
        }
        let width = {
            let model = model(vec![service("caddy", ServiceState::Running)]);
            let harness = Harness::new(egui::Theme::Light);
            let mut width = 0.0;
            harness.run(|ui| {
                ui.allocate_ui(egui::vec2(632.0, 400.0), |ui| {
                    let columns = [
                        TableColumn::fill(strings::COLUMN_SERVICE),
                        TableColumn::fixed(strings::COLUMN_STATE, SERVICES_STATE_COLUMN),
                        TableColumn::fixed(strings::COLUMN_ACTIONS, SERVICES_ACTIONS_COLUMN),
                    ];
                    width = Table::new("services", strings::SERVICES, &columns).fill_width(ui);
                    draw(ui, &model);
                });
            });
            width
        };
        let harness = Harness::new(egui::Theme::Light);
        let mut cut = Vec::new();
        harness.run(|ui| {
            for text in &texts {
                let galley = note_galley(ui.painter(), &LIGHT, text, width);
                if galley.elided || galley.rows.len() > NOTE_LINES {
                    cut.push(text.clone());
                }
            }
        });
        assert!(cut.is_empty(), "cut at {width} wide: {cut:#?}");
    }

    #[test]
    fn a_row_is_as_high_as_its_lines_need_and_the_text_is_whole_for_a_screen_reader() {
        let long = ServiceState::Failed {
            code: "php_fpm_orphan_stuck".into(),
        };
        let short = ServiceState::Failed {
            code: "exited".into(),
        };
        let measure = |state: ServiceState, width: f32| {
            let model = model(vec![service("php-fpm-8.4", state)]);
            let (_, tree) = tree_at(&model, width);
            let reason = match &view(&model) {
                ServicesView::Rows { rows, .. } => rows[0].failure.clone().expect("a reason"),
                other => panic!("{other:?}"),
            };
            let cell = tree
                .node_with_text(&format!("PHP-FPM 8.4, {reason}"))
                .expect("the whole reason is there");
            let b = cell.bounds().expect("a place");
            b.y1 - b.y0
        };
        // A line that wraps makes a taller row than one that does not.
        let (tall, flat) = (measure(long, 632.0), measure(short, 632.0));
        assert!(tall > flat, "{tall} against {flat}");
        // At a wide window the same line fits in one line: the row is the usual one.
        assert!(
            measure(
                ServiceState::Failed {
                    code: "php_fpm_orphan_stuck".into()
                },
                1400.0
            ) <= flat + 1.0
        );
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
