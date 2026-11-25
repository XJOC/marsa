//! The Workers card of a Laravel site's page (layout.md, Site page:
//! Workers): a row for the queue worker and one for the scheduler, each with
//! its switch, its state as a pill, and "Log"; under the card, why a worker
//! failed, and one line on what the workers run.
//!
//! As the page: [`view`] reads the [`Model`] and decides what the card says,
//! pure; [`show`] draws it and returns what the user did. The switch shows
//! the daemon's setting only, and waits while its change is on its way.

use eframe::egui::Ui;
use marsa_core::types::{ServiceId, ServiceState, Site, SiteKey, SiteKind, WorkerKind};

use crate::model::{Action, LogSource, Model, Route};
use crate::screens::lines::{self, Mark};
use crate::strings;
use crate::theme::page::SITE_FACT_LABEL;
use crate::theme::{Direction, Theme};
use crate::widgets::{Button, PillState, StatePill, Switch, fact_card};

/// What the card says.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct WorkersView {
    /// The queue worker, then the scheduler.
    pub rows: [WorkerRow; 2],
}

/// One worker.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct WorkerRow {
    pub kind: WorkerKind,
    pub site: SiteKey,
    /// The site's setting, as the daemon said last.
    pub on: bool,
    pub state: PillState,
    /// The switch sent a change and waits for the site list: it is off.
    pub saving: bool,
    /// Why the worker failed: a line under the card.
    pub failure: Option<String>,
    /// The daemon lists the worker, so it has a log to open.
    pub log: bool,
    /// The process id while it runs: the pill's tooltip.
    pub pid: Option<u32>,
}

/// The card of `site`: a Laravel site, or one whose setting still asks for
/// a worker (so it can be turned off). `None` for any other site.
pub(crate) fn view(model: &Model, site: &Site) -> Option<WorkersView> {
    let asks = WorkerKind::ALL.iter().any(|kind| site.wants(*kind));
    if site.kind != SiteKind::Laravel && !asks {
        return None;
    }
    let key = SiteKey::new(&site.name)?;
    Some(WorkersView {
        rows: WorkerKind::ALL.map(|kind| row(model, site, key, kind)),
    })
}

fn row(model: &Model, site: &Site, key: SiteKey, kind: WorkerKind) -> WorkerRow {
    let on = site.wants(kind);
    let status = model.worker_status(kind, key);
    let state = status.map(|status| &status.state);
    let pill = match state {
        Some(ServiceState::Running) => PillState::Running,
        Some(ServiceState::Starting) => PillState::Starting,
        Some(ServiceState::Stopping) => PillState::Stopping,
        Some(ServiceState::Stopped) => PillState::Stopped,
        Some(ServiceState::Failed { .. }) => PillState::Failed,
        // The setting is saved: the daemon starts it on its next pass.
        None if on => PillState::Starting,
        None => PillState::Off,
    };
    let failure = match state {
        Some(ServiceState::Failed { code }) => Some(strings::worker_failure(name(kind), code)),
        _ => None,
    };
    WorkerRow {
        kind,
        site: key,
        on,
        state: pill,
        saving: model.worker_asked(kind, key),
        failure,
        log: status.is_some(),
        pid: status.and_then(|status| status.pid),
    }
}

/// The name of worker `kind`: its row's label, and its switch's name.
pub(crate) const fn name(kind: WorkerKind) -> &'static str {
    match kind {
        WorkerKind::Queue => strings::WORKER_QUEUE,
        WorkerKind::Schedule => strings::WORKER_SCHEDULE,
    }
}

/// Draws `view`: the heading, the card, the failures, and the help line.
pub(crate) fn show(ui: &mut Ui, theme: &Theme, view: &WorkersView, actions: &mut Vec<Action>) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = theme.space.group_gap_max;
        super::super::heading(ui, theme, strings::WORKERS, None);
        fact_card(ui, SITE_FACT_LABEL, |rows| {
            for row in &view.rows {
                rows.row(name(row.kind), |ui| draw_row(ui, row, actions));
            }
        });
        for failure in view.rows.iter().filter_map(|row| row.failure.as_deref()) {
            lines::status_line(ui, theme, Mark::Error, failure, |_| {});
        }
        lines::mute(ui, theme, strings::WORKERS_HELP);
    });
}

/// The switch, named by the row's label, the pill, and "Log" at the end.
fn draw_row(ui: &mut Ui, row: &WorkerRow, actions: &mut Vec<Action>) {
    let switch = Switch::new(row.on, name(row.kind))
        .bare()
        .disabled_if(row.saving, strings::WORKER_SAVING_HINT);
    if ui.add(switch).clicked() {
        actions.push(Action::SetWorker {
            kind: row.kind,
            site: row.site,
            on: !row.on,
        });
    }
    let pill = ui.add(StatePill::new(row.state));
    if let Some(pid) = row.pid {
        pill.on_hover_text(strings::service_pid(pid));
    }
    ui.with_layout(Direction::CURRENT.row_from_end(), |ui| {
        let log = Button::secondary(strings::WORKER_LOG)
            .disabled_if(!row.log, strings::WORKER_LOG_OFF_HINT);
        if ui.add(log).clicked() {
            let id = ServiceId::Worker(row.kind, row.site);
            actions.push(Action::Navigate(Route::Logs(LogSource::of(id))));
        }
    });
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Pos2, accesskit, pos2};
    use marsa_core::types::{Event, ServiceStatus, SiteList};

    use super::*;
    use crate::bridge::Command;
    use crate::fixtures::{connected, site};
    use crate::model::{Update, apply};
    use crate::theme::LIGHT;
    use crate::widgets::harness::{Harness, Tree};

    fn key(name: &str) -> SiteKey {
        SiteKey::new(name).unwrap()
    }

    fn status(kind: WorkerKind, state: ServiceState) -> ServiceStatus {
        ServiceStatus {
            id: ServiceId::Worker(kind, key("shop")),
            state,
            pid: None,
            restarts: 0,
            since_ms: None,
        }
    }

    /// `shop` with its queue and scheduler settings, and these workers
    /// listed.
    fn model(queue: bool, schedule: bool, workers: Vec<ServiceStatus>) -> Model {
        let mut model = connected();
        let shop = Site {
            queue,
            schedule,
            ..site("shop", Some("8.4.27"), None)
        };
        apply(
            &mut model,
            Event::SitesChanged(SiteList {
                sites: vec![shop],
                ..SiteList::default()
            }),
        );
        model.update(Update::Services(workers));
        model
    }

    fn card(model: &Model) -> WorkersView {
        let shop = &model.session.sites.sites[0];
        view(model, shop).expect("a card for a Laravel site")
    }

    fn draw(ui: &mut Ui, model: &Model) -> Vec<Action> {
        let mut actions = Vec::new();
        show(ui, &LIGHT, &card(model), &mut actions);
        actions
    }

    fn start(model: &Model) -> (Harness, Tree) {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            draw(ui, model);
        });
        (harness, tree)
    }

    fn center(node: &accesskit::Node) -> Pos2 {
        let b = node.bounds().expect("a node with bounds");
        pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32)
    }

    /// The widgets named `label`, from the top of the card.
    fn named<'a>(tree: &'a Tree, label: &str) -> Vec<&'a accesskit::Node> {
        let mut nodes = tree.nodes_with_label(label);
        nodes.sort_by(|a, b| center(a).y.total_cmp(&center(b).y));
        nodes
    }

    /// One click on the `index`th widget named `label` from the top; what
    /// the card returned.
    fn click(model: &Model, label: &str, index: usize) -> Vec<Action> {
        let (harness, tree) = start(model);
        let at = center(named(&tree, label)[index]);
        let mut actions = Vec::new();
        harness.click(at, |ui| actions.extend(draw(ui, model)));
        actions
    }

    #[test]
    fn each_state_has_its_word_icon_and_color() {
        let failed = ServiceState::Failed {
            code: "worker_failed".into(),
        };
        let cases = [
            (
                model(
                    true,
                    false,
                    vec![status(WorkerKind::Queue, ServiceState::Running)],
                ),
                [PillState::Running, PillState::Off],
            ),
            // On, and not listed yet: the daemon starts it on its next pass.
            (
                model(true, true, Vec::new()),
                [PillState::Starting, PillState::Starting],
            ),
            (
                model(true, false, vec![status(WorkerKind::Queue, failed)]),
                [PillState::Failed, PillState::Off],
            ),
            (
                model(false, false, Vec::new()),
                [PillState::Off, PillState::Off],
            ),
        ];
        for (model, states) in cases {
            let card = card(&model);
            assert_eq!(card.rows.map(|row| row.state), states);
            let (_, tree) = start(&model);
            let words: Vec<_> = tree
                .nodes_with_label(states[0].word())
                .into_iter()
                .filter(|node| node.role() == egui::Role::Status)
                .collect();
            assert!(!words.is_empty(), "{states:?}");
        }
        // The look of each comes from the pill: an icon and a color per state.
        for state in [PillState::Running, PillState::Starting, PillState::Failed] {
            let look = crate::widgets::status::pill_look(&LIGHT, state);
            assert!(look.icon.is_some(), "{state:?}");
        }
        assert_eq!(PillState::Off.word(), "Off");
    }

    #[test]
    fn a_failed_worker_says_why_under_the_card_and_keeps_its_log() {
        let failed = |code: &str| ServiceState::Failed { code: code.into() };
        let model = model(
            true,
            true,
            vec![
                status(WorkerKind::Queue, failed("worker_failed")),
                status(WorkerKind::Schedule, failed("php_not_installed")),
            ],
        );
        let card = card(&model);
        assert_eq!(
            card.rows[0].failure.as_deref(),
            Some(
                "Queue worker: Stopped after repeated crashes. Check the log, then turn it on again."
            )
        );
        assert_eq!(
            card.rows[1].failure,
            Some(format!(
                "Scheduler: {}",
                strings::error_text("php_not_installed")
            ))
        );
        assert!(card.rows.iter().all(|row| row.log));
        let (_, tree) = start(&model);
        for row in &card.rows {
            assert!(
                tree.node_with_text(row.failure.as_deref().unwrap())
                    .is_some()
            );
        }
        assert!(tree.node_with_text(strings::WORKERS_HELP).is_some());
    }

    #[test]
    fn the_switch_sends_the_other_setting_and_is_off_while_it_waits() {
        let mut model = model(false, false, Vec::new());
        let actions = click(&model, strings::WORKER_SCHEDULE, 0);
        let on = Action::SetWorker {
            kind: WorkerKind::Schedule,
            site: key("shop"),
            on: true,
        };
        assert_eq!(actions, std::slice::from_ref(&on));
        let sent = model.apply_action(on).command();
        assert!(
            matches!(sent, Some(Command::SetWorker { on: true, .. })),
            "{sent:?}"
        );
        // The switch still shows the daemon's setting, disabled.
        let card = card(&model);
        assert!(!card.rows[1].on && card.rows[1].saving);
        let (_, tree) = start(&model);
        let switch = tree.nodes_with_label(strings::WORKER_SCHEDULE);
        let switch = switch
            .iter()
            .find(|node| node.role() == egui::Role::Switch)
            .unwrap();
        assert!(switch.is_disabled());
        assert_eq!(switch.toggled(), Some(accesskit::Toggled::False));
        assert!(click(&model, strings::WORKER_SCHEDULE, 0).is_empty());
    }

    #[test]
    fn log_opens_the_workers_own_log_once_the_daemon_lists_it() {
        let running = vec![status(WorkerKind::Queue, ServiceState::Running)];
        let model = model(true, false, running);
        let id = ServiceId::Worker(WorkerKind::Queue, key("shop"));
        assert_eq!(
            click(&model, strings::WORKER_LOG, 0),
            [Action::Navigate(Route::Logs(LogSource::Service(id)))]
        );
        // The scheduler never ran: no log, and the button says so.
        let (_, tree) = start(&model);
        let logs = named(&tree, strings::WORKER_LOG);
        assert!(!logs[0].is_disabled() && logs[1].is_disabled());
        assert!(click(&model, strings::WORKER_LOG, 1).is_empty());
    }

    #[test]
    fn only_a_laravel_site_or_one_that_asks_for_a_worker_has_the_card() {
        let model = model(false, false, Vec::new());
        let plain = Site {
            kind: SiteKind::Php,
            ..site("blog", Some("8.4.27"), None)
        };
        assert_eq!(view(&model, &plain), None);
        let asks = Site {
            schedule: true,
            ..plain
        };
        assert!(view(&model, &asks).is_some());
    }
}
