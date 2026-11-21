//! The first-run panel (principles 2, layout.md): three steps that take a
//! new install to its first site over HTTPS. It replaces Home until
//! [`Model::needs_first_run`] says the first site can open: PHP and Caddy are
//! installed, and there is a site. Each step has one clear action, which is
//! the primary button of the step the user is on.
//!
//! Like Home, the screen is a pure [`view`] and a [`show`] that draws it and
//! returns [`Action`]s. The installs, setup, park and link are the model's
//! own logic, reached through those actions.
//!
//! The steps, in order: install what is missing (with progress), set up
//! `.test` and HTTPS (with the system's own explanation), add a site (park a
//! folder of projects, or link one). A step is done when the model can tell:
//! the runtimes list for the installs, a setup report without a failure or
//! the hosts block for setup (the window cannot read whether setup ran in an
//! earlier session), the site list for the last.

use eframe::egui::{Label, RichText, Ui, vec2};
use marsa_core::recipes::Platform;
use marsa_core::runtimes::Runtime;

use super::home::{InstallBar, install_bar};
use crate::model::{self, Action, InstallStep, Model, Pick, Setup, SitesNotice};
use crate::strings::{self, ReportLine};
use crate::theme::page::FIRST_RUN_WIDTH;
use crate::theme::{Direction, Theme};
use crate::widgets::{Button, Progress, Stage, StepCard};

/// What the panel says.
#[derive(Debug, PartialEq)]
pub struct FirstRunView {
    pub steps: [StepView; 3],
    /// The button of the step to do now.
    pub primary: Primary,
    /// A second way to do the last step: "Link a folder…".
    pub also: Option<(&'static str, Action)>,
}

#[derive(Debug, PartialEq)]
pub struct StepView {
    pub number: u8,
    pub title: &'static str,
    pub body: &'static str,
    pub stage: Stage,
    pub detail: Detail,
}

/// What a step shows under its explanation.
#[derive(Debug, PartialEq)]
pub enum Detail {
    None,
    /// An install that runs.
    Progress(InstallBar),
    /// What happened: a failure in the error color, or a report.
    Lines(Vec<ReportLine>),
}

#[derive(Debug, PartialEq)]
pub struct Primary {
    pub label: &'static str,
    pub action: Action,
    /// Why it cannot be pressed now: its step runs.
    pub disabled: Option<&'static str>,
}

/// Decides what the panel says. Pure: the same model gives the same view.
pub fn view(model: &Model) -> FirstRunView {
    let rows = &model.session.runtimes;
    let (php, caddy) = (
        model::runtime_missing(rows, Runtime::Php),
        model::runtime_missing(rows, Runtime::Caddy),
    );
    let platform = Platform::current();
    let done = [
        !php && !caddy,
        setup_done(model),
        !model.session.sites.sites.is_empty(),
    ];
    // The first step not done; the last when all are.
    let current = done.iter().position(|done| !done).unwrap_or(done.len() - 1);
    let stage = |index: usize| match (done[index], index == current) {
        (true, _) => Stage::Done,
        (false, true) => Stage::Current,
        (false, false) => Stage::Next,
    };
    let steps = [
        StepView {
            number: 1,
            title: strings::first_run_install_title(php, caddy),
            body: strings::FIRST_RUN_INSTALL_BODY,
            stage: stage(0),
            detail: install_detail(model, done[0]),
        },
        StepView {
            number: 2,
            title: strings::FIRST_RUN_SETUP,
            body: strings::first_run_setup_body(platform),
            stage: stage(1),
            detail: setup_detail(model, platform),
        },
        StepView {
            number: 3,
            title: strings::FIRST_RUN_SITE,
            body: strings::FIRST_RUN_SITE_BODY,
            stage: stage(2),
            detail: site_detail(model),
        },
    ];
    let primary = match current {
        0 if model.installing() => Primary {
            label: strings::INSTALLING,
            action: Action::StartFirstRun,
            disabled: Some(strings::INSTALLING_HINT),
        },
        0 => Primary {
            label: strings::first_run_install_title(php, caddy),
            action: Action::StartFirstRun,
            disabled: None,
        },
        1 if model.session.setup == Setup::Running => Primary {
            label: strings::SETTING_UP,
            action: Action::RunSetup,
            disabled: Some(strings::setup_running(platform)),
        },
        1 => Primary {
            label: strings::RUN_SETUP,
            action: Action::RunSetup,
            disabled: None,
        },
        _ => Primary {
            label: strings::PARK_BUTTON,
            action: Action::PickFolder(Pick::Park),
            disabled: None,
        },
    };
    let also = (current == 2).then_some((strings::LINK_BUTTON, Action::PickFolder(Pick::Link)));
    FirstRunView {
        steps,
        primary,
        also,
    }
}

/// Setup ran without a failure in this window, or the hosts block (Linux)
/// shows that an earlier one did.
fn setup_done(model: &Model) -> bool {
    match &model.session.setup {
        Setup::Finished(report) => !strings::setup_lines(report).iter().any(|line| line.problem),
        Setup::Idle | Setup::Running | Setup::Failed(_) => model.session.hosts.used,
    }
}

fn problem(text: String) -> Detail {
    Detail::Lines(vec![ReportLine {
        text,
        problem: true,
    }])
}

/// The install that runs, or why the last one of PHP or Caddy failed.
fn install_detail(model: &Model, done: bool) -> Detail {
    let ours = model
        .install_view()
        .filter(|view| matches!(view.runtime, Some(Runtime::Php | Runtime::Caddy)));
    match ours {
        Some(view) if model.installing() => Detail::Progress(install_bar(view)),
        Some(view) if !done && matches!(view.step, InstallStep::Failed(_)) => {
            problem(strings::install_status(view))
        }
        _ => Detail::None,
    }
}

/// The wait for the password, or what setup said when it did not finish.
fn setup_detail(model: &Model, platform: Option<Platform>) -> Detail {
    match &model.session.setup {
        Setup::Running => Detail::Lines(vec![ReportLine {
            text: strings::setup_running(platform).to_owned(),
            problem: false,
        }]),
        Setup::Failed(code) => problem(strings::error_text(code)),
        Setup::Finished(report) if !setup_done(model) => {
            Detail::Lines(strings::setup_lines(report))
        }
        Setup::Finished(_) | Setup::Idle => Detail::None,
    }
}

/// What the last park or link said, while there is no site.
fn site_detail(model: &Model) -> Detail {
    match &model.session.sites_notice {
        Some(SitesNotice::Failed(code)) => problem(strings::error_text(code)),
        Some(SitesNotice::Parked(folder)) if model.session.sites.sites.is_empty() => {
            Detail::Lines(vec![ReportLine {
                text: strings::parked(folder, model.home.as_deref()),
                problem: false,
            }])
        }
        _ => Detail::None,
    }
}

/// Draws `view`, centered in a column. Returns what the user did.
pub fn show(ui: &mut Ui, view: &FirstRunView, theme: &Theme) -> Vec<Action> {
    let mut actions = Vec::new();
    ui.add_space(theme.space.s8);
    let width = ui.available_width().min(FIRST_RUN_WIDTH);
    ui.vertical_centered(|ui| {
        // The column keeps its text at the start; the column itself is
        // centered, and grows with its content.
        ui.allocate_ui_with_layout(vec2(width, 0.0), Direction::CURRENT.column(), |ui| {
            ui.spacing_mut().item_spacing.y = theme.space.section_gap;
            welcome(ui, theme);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = theme.space.group_gap_max;
                for step in &view.steps {
                    let card = StepCard::new(step.number, step.title, step.body, step.stage);
                    card.show_with(ui, |ui| detail(ui, theme, &step.detail));
                }
            });
            buttons(ui, theme, view, &mut actions);
        });
    });
    actions
}

fn welcome(ui: &mut Ui, theme: &Theme) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = theme.space.s1;
        let title = RichText::new(strings::FIRST_RUN_WELCOME)
            .font(theme.text.display.font_id())
            .color(theme.color.ink);
        ui.add(Label::new(title).selectable(false));
        let intro = RichText::new(strings::FIRST_RUN_INTRO)
            .font(theme.text.body.font_id())
            .color(theme.color.ink2);
        ui.add(Label::new(intro).selectable(false));
    });
}

fn detail(ui: &mut Ui, theme: &Theme, detail: &Detail) {
    match detail {
        Detail::None => {}
        Detail::Progress(bar) => {
            ui.add_space(theme.space.s1);
            ui.add(Progress::new(&bar.words, bar.fraction));
        }
        Detail::Lines(lines) => {
            for line in lines {
                let color = if line.problem {
                    theme.color.error
                } else {
                    theme.color.ink2
                };
                let text = RichText::new(&line.text)
                    .font(theme.text.body_sm.font_id())
                    .color(color);
                ui.add(Label::new(text).selectable(false));
            }
        }
    }
}

fn buttons(ui: &mut Ui, theme: &Theme, view: &FirstRunView, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme.space.group_gap_max;
        let primary = &view.primary;
        let button = Button::primary(primary.label);
        let button = match primary.disabled {
            Some(reason) => button.disabled(reason),
            None => button,
        };
        if ui.add(button).clicked() {
            actions.push(primary.action.clone());
        }
        if let Some((label, action)) = &view.also
            && ui.add(Button::secondary(label)).clicked()
        {
            actions.push(action.clone());
        }
        if ui.add(Button::secondary(strings::NOT_NOW)).clicked() {
            actions.push(Action::CloseFirstRun);
        }
    });
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use eframe::egui::{self, Pos2, pos2};
    use marsa_core::helper::{OpResult, OpStatus};
    use marsa_core::types::{
        Event, Health, HostsStatus, InstallEvent, InstallProgress, InstallUpdate, RuntimeList,
        RuntimeVersions, SetupReport, Site, SiteKind, SiteList,
    };

    use super::*;
    use crate::model::{FirstRun, Update, apply};
    use crate::theme::LIGHT;
    use crate::widgets::harness::{Harness, Tree};

    fn connected() -> Model {
        let mut model = Model::default();
        model.update(Update::Connected(Health {
            app_version: "0.0.0".into(),
            api_version: 1,
            pid: 7,
        }));
        model
    }

    fn runtimes(php: &[&str], caddy: &[&str]) -> RuntimeList {
        let row = |runtime, installed: &[&str]| RuntimeVersions {
            runtime,
            installed: installed.iter().map(|text| (*text).to_owned()).collect(),
            default: None,
        };
        RuntimeList {
            runtimes: vec![row(Runtime::Php, php), row(Runtime::Caddy, caddy)],
        }
    }

    /// A fresh install: nothing installed, no site.
    fn fresh() -> Model {
        let mut model = connected();
        model.update(Update::Runtimes(runtimes(&[], &[])));
        model
    }

    /// PHP and Caddy installed, and no site.
    fn installed() -> Model {
        let mut model = connected();
        model.update(Update::Runtimes(runtimes(&["8.4.27"], &["2.11.7"])));
        model
    }

    fn report(op: &str, status: OpStatus) -> SetupReport {
        let failed = status == OpStatus::Failed;
        SetupReport {
            results: vec![OpResult {
                op: op.into(),
                status,
                code: failed.then(|| "resolver_foreign".to_owned()),
                message: None,
            }],
            ..SetupReport::default()
        }
    }

    fn stages(view: &FirstRunView) -> [Stage; 3] {
        [0, 1, 2].map(|index| view.steps[index].stage)
    }

    /// The PHP job accepted, and `done` of `total` bytes of it downloaded.
    fn downloading(model: &mut Model, done: u64, total: u64) {
        assert!(model.apply_action(Action::StartFirstRun).is_none());
        let sent = model.next_install().expect("the first job");
        model.update(Update::InstallAccepted { job: sent, id: 1 });
        let selected = InstallEvent::Selected {
            runtime: Runtime::Php,
            version: "8.4.27".into(),
        };
        let progress = |update| Event::Install(InstallProgress { id: 1, update });
        apply(model, progress(InstallUpdate::Running(selected)));
        let step = InstallEvent::Downloading {
            artifact: "php.tar.gz".into(),
            done,
            total,
        };
        apply(model, progress(InstallUpdate::Running(step)));
    }

    #[test]
    fn a_new_install_starts_with_the_install_step_and_its_button() {
        let view = view(&fresh());
        assert_eq!(stages(&view), [Stage::Current, Stage::Next, Stage::Next]);
        let titles: Vec<_> = view.steps.iter().map(|step| step.title).collect();
        assert_eq!(
            titles,
            [
                "Install PHP 8.4 and Caddy",
                "Set up HTTPS for .test sites",
                "Add your first site"
            ]
        );
        let numbers: Vec<_> = view.steps.iter().map(|step| step.number).collect();
        assert_eq!(numbers, [1, 2, 3]);
        assert_eq!(view.primary.label, "Install PHP 8.4 and Caddy");
        assert_eq!(view.primary.action, Action::StartFirstRun);
        assert!(view.primary.disabled.is_none() && view.also.is_none());
        assert!(view.steps.iter().all(|step| step.detail == Detail::None));
    }

    #[test]
    fn only_what_is_missing_is_named() {
        let mut model = connected();
        model.update(Update::Runtimes(runtimes(&["8.4.27"], &[])));
        let view = view(&model);
        assert_eq!(view.steps[0].title, "Install Caddy");
        assert_eq!(view.primary.label, "Install Caddy");
        model.update(Update::Runtimes(runtimes(&[], &["2.11.7"])));
        assert_eq!(super::view(&model).primary.label, "Install PHP 8.4");
    }

    #[test]
    fn an_install_that_runs_shows_its_progress_and_the_button_waits() {
        let mut model = fresh();
        downloading(&mut model, 18, 31);
        let view = view(&model);
        assert_eq!(stages(&view)[0], Stage::Current);
        let Detail::Progress(bar) = &view.steps[0].detail else {
            panic!("the install shows: {:?}", view.steps[0].detail);
        };
        assert!(
            bar.words.starts_with("Downloading PHP 8.4.27"),
            "{}",
            bar.words
        );
        let share = bar.fraction.expect("a share to show");
        assert!((share - 18.0 / 31.0).abs() < 0.001, "{share}");
        assert_eq!(view.primary.label, "Installing…");
        assert_eq!(view.primary.disabled, Some(strings::INSTALLING_HINT));
    }

    #[test]
    fn a_failed_install_says_why_and_offers_the_button_again() {
        let mut model = fresh();
        assert!(model.apply_action(Action::StartFirstRun).is_none());
        let sent = model.next_install().expect("the first job");
        model.update(Update::InstallRefused {
            job: sent,
            code: "install_busy".into(),
        });
        assert_eq!(model.session.first_run, Some(FirstRun::Offer));
        let view = view(&model);
        let Detail::Lines(lines) = &view.steps[0].detail else {
            panic!("the failure shows: {:?}", view.steps[0].detail);
        };
        assert!(lines[0].problem && lines[0].text.starts_with("Cannot install PHP"));
        assert_eq!(view.primary.label, "Install PHP 8.4 and Caddy");
        assert!(view.primary.disabled.is_none());
    }

    #[test]
    fn once_php_and_caddy_are_there_setup_is_the_step_to_do() {
        let view = view(&installed());
        assert_eq!(stages(&view), [Stage::Done, Stage::Current, Stage::Next]);
        assert_eq!(view.steps[0].title, "PHP and Caddy are installed");
        assert_eq!(
            view.steps[1].body,
            strings::first_run_setup_body(Platform::current())
        );
        assert_eq!(view.primary.label, "Run setup");
        assert_eq!(view.primary.action, Action::RunSetup);
        assert!(view.also.is_none());
    }

    #[test]
    fn setup_waits_for_the_password_fails_with_its_reason_or_moves_on() {
        let mut model = installed();
        assert!(model.start_setup());
        let view = view(&model);
        assert_eq!(view.primary.label, "Setting up…");
        let hint = strings::setup_running(Platform::current());
        assert_eq!(view.primary.disabled, Some(hint));
        assert_eq!(
            view.steps[1].detail,
            Detail::Lines(vec![ReportLine {
                text: hint.to_owned(),
                problem: false
            }])
        );

        // A failed step: its reason shows, and the step stays to do.
        model.update(Update::SetupDone(report(
            "resolver_install",
            OpStatus::Failed,
        )));
        let view = super::view(&model);
        assert_eq!(stages(&view)[1], Stage::Current);
        let Detail::Lines(lines) = &view.steps[1].detail else {
            panic!("the report shows");
        };
        assert!(lines.iter().any(|line| line.problem), "{lines:?}");
        assert_eq!(view.primary.label, "Run setup", "a new try");

        // A request that did not reach the helper.
        model.update(Update::SetupFailed("setup_cancelled".into()));
        let view = super::view(&model);
        let Detail::Lines(lines) = &view.steps[1].detail else {
            panic!("the failure shows");
        };
        assert_eq!(lines[0].text, strings::error_text("setup_cancelled"));
        assert!(lines[0].problem);

        // Done: the last step is the one to do, and the report is not shown.
        model.update(Update::SetupDone(report(
            "resolver_install",
            OpStatus::Done,
        )));
        let view = super::view(&model);
        assert_eq!(stages(&view), [Stage::Done, Stage::Done, Stage::Current]);
        assert_eq!(view.steps[1].detail, Detail::None);
    }

    #[test]
    fn a_hosts_block_shows_that_setup_ran_in_an_earlier_session() {
        let mut model = installed();
        model.update(Update::Hosts(HostsStatus {
            used: true,
            missing: Vec::new(),
        }));
        assert_eq!(
            stages(&view(&model)),
            [Stage::Done, Stage::Done, Stage::Current]
        );
    }

    #[test]
    fn the_last_step_parks_a_folder_or_links_one() {
        let mut model = installed();
        model.update(Update::SetupDone(report(
            "resolver_install",
            OpStatus::Done,
        )));
        let view = view(&model);
        assert_eq!(view.primary.label, "Park a folder…");
        assert_eq!(view.primary.action, Action::PickFolder(Pick::Park));
        let also = view.also.expect("a second way");
        assert_eq!(also, ("Link a folder…", Action::PickFolder(Pick::Link)));

        // A folder with no project in it: the step says so, and stays.
        model.update(Update::Parked(PathBuf::from("/code")));
        let view = super::view(&model);
        let Detail::Lines(lines) = &view.steps[2].detail else {
            panic!("the park shows");
        };
        assert_eq!(lines[0].text, strings::parked(Path::new("/code"), None));
        assert!(!lines[0].problem);
        model.update(Update::SitesFailed("site_path_relative".into()));
        let view = super::view(&model);
        let Detail::Lines(lines) = &view.steps[2].detail else {
            panic!("the failure shows");
        };
        assert!(lines[0].problem);
    }

    #[test]
    fn a_site_before_caddy_leaves_the_last_step_done_and_the_install_to_do() {
        let mut model = connected();
        model.update(Update::Runtimes(runtimes(&["8.4.27"], &[])));
        let root = PathBuf::from("/code/shop");
        let site = Site {
            name: "shop".into(),
            docroot: root.join("public"),
            root,
            kind: SiteKind::Laravel,
            php: None,
            php_source: None,
            secure: true,
        };
        model.update(Update::Sites(SiteList {
            sites: vec![site],
            ..SiteList::default()
        }));
        assert!(model.needs_first_run());
        assert_eq!(
            stages(&view(&model)),
            [Stage::Current, Stage::Next, Stage::Done]
        );
    }

    // -- show --

    fn draw(ui: &mut egui::Ui, view: &FirstRunView) -> Vec<Action> {
        show(ui, view, &LIGHT)
    }

    fn center(tree: &Tree, label: &str) -> Pos2 {
        let node = tree.node_with_label(label).expect(label);
        let bounds = node.bounds().expect("a node with bounds");
        pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        )
    }

    /// One click on the button named `label`; what the panel returned.
    fn click(view: &FirstRunView, label: &str) -> Vec<Action> {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            draw(ui, view);
        });
        let mut actions = Vec::new();
        harness.click(center(&tree, label), |ui| actions.extend(draw(ui, view)));
        actions
    }

    #[test]
    fn each_button_returns_its_action() {
        let mut model = installed();
        assert_eq!(click(&view(&model), "Run setup"), [Action::RunSetup]);
        assert_eq!(
            click(&view(&model), strings::NOT_NOW),
            [Action::CloseFirstRun]
        );
        model.update(Update::SetupDone(report(
            "resolver_install",
            OpStatus::Done,
        )));
        let view = view(&model);
        let park = Action::PickFolder(Pick::Park);
        assert_eq!(click(&view, strings::PARK_BUTTON), [park]);
        let link = Action::PickFolder(Pick::Link);
        assert_eq!(click(&view, strings::LINK_BUTTON), [link]);
        assert_eq!(
            click(&super::view(&fresh()), strings::FIRST_RUN_INSTALL),
            [Action::StartFirstRun]
        );
    }

    #[test]
    fn a_button_that_waits_returns_nothing_and_not_now_still_works() {
        let mut model = fresh();
        downloading(&mut model, 1, 2);
        let view = view(&model);
        assert!(click(&view, strings::INSTALLING).is_empty());
        assert_eq!(
            click(&view, strings::NOT_NOW),
            [Action::CloseFirstRun],
            "the install goes on in the daemon"
        );
    }

    #[test]
    fn the_steps_are_cards_in_a_centered_column_no_wider_than_its_width() {
        let harness = Harness::new(egui::Theme::Light);
        let view = view(&fresh());
        let mut page = 0.0;
        let tree = harness.accessibility(|ui| {
            page = ui.available_width();
            draw(ui, &view);
        });
        let mut places = Vec::new();
        for step in &view.steps {
            let label = strings::step_label(step.number, step.title, step.stage);
            let node = tree.node_with_label(&label).expect("the step");
            places.push(node.bounds().expect("a place"));
        }
        let first = places[0];
        assert!(
            (first.width() - f64::from(FIRST_RUN_WIDTH)).abs() < 0.5,
            "{first:?}"
        );
        // Centered: as much room on one side as on the other.
        let after = f64::from(page) - first.x1;
        assert!((first.x0 - after).abs() < 1.0, "{} and {after}", first.x0);
        // One under the other, the same width and place.
        for pair in places.windows(2) {
            assert!(pair[1].y0 > pair[0].y1);
            assert_eq!((pair[0].x0, pair[0].x1), (pair[1].x0, pair[1].x1));
        }
    }
}
