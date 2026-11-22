//! Settings (layout.md, Screens): three groups in cards of rows.
//! General: Start at login, the Appearance, and the language. Setup: what
//! the window knows of the HTTPS certificate and the `.test` names, and "Run
//! setup" with its report. About: the version (with the daemon's under it)
//! and the data folder.
//!
//! Like Home and Sites it is two functions. [`view`] reads the [`Model`] and
//! decides what the screen says: pure, so the tests assert it without a
//! window. [`show`] draws that [`SettingsView`] with the widgets and returns
//! what the user did as [`Action`]s (code.md, State). [Laravel: `view` is the
//! controller; `show` is the Blade template.]
//!
//! Nothing here claims what the window cannot know. It learns that the
//! certificate is trusted and the `.test` names reach Marsa only from a setup
//! report of this session (or, on Linux, from the hosts file the daemon
//! reads); before that, and after a setup that did not reach the step, the
//! row says "Not checked".

use std::path::Path;

use eframe::egui::Ui;
use marsa_core::helper::OpStatus;
use marsa_core::recipes::Platform;

use super::lines::{Mark, mute, status_line};
use crate::model::{self, Action, Appearance, Autostart, Model, Setup};
use crate::strings::{self, ReportLine};
use crate::theme::page::SETTINGS_WIDTH;
use crate::theme::{Section, Theme};
use crate::widgets::{Button, Glyph, Icon, Segmented, Switch, Text, setting_card};

/// What the window knows of one step of setup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// The last setup of this session did it, or it was already done.
    Done,
    /// The last setup of this session tried and failed.
    Failed,
    /// The last setup of this session left the step out.
    Skipped,
    /// No setup of this session reached it: the window cannot tell.
    NotChecked,
}

impl Check {
    /// How the check stands, by its word.
    pub const fn word(self) -> &'static str {
        match self {
            Check::Done => strings::CHECK_DONE,
            Check::Failed => strings::CHECK_FAILED,
            Check::Skipped => strings::CHECK_SKIPPED,
            Check::NotChecked => strings::CHECK_NOT_CHECKED,
        }
    }
}

/// What Settings says.
#[derive(Debug, PartialEq)]
pub struct SettingsView<'a> {
    pub general: General,
    pub setup: SetupGroup,
    pub about: About<'a>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct General {
    pub autostart: AutostartRow,
    pub appearance: Appearance,
}

/// The Start at login switch.
#[derive(Debug, PartialEq, Eq)]
pub struct AutostartRow {
    pub on: bool,
    /// Why the switch cannot be pressed now.
    pub off: Option<&'static str>,
    /// The line under the name: what it does, or what is wrong with the
    /// login file.
    pub hint: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SetupGroup {
    pub cert: Check,
    pub names: Check,
    /// What the `.test` row says under its name.
    pub names_hint: String,
    /// What setup does, and the prompt it brings, on this system.
    pub explanation: &'static str,
    /// Why "Run setup" cannot be pressed now.
    pub run_off: Option<&'static str>,
    /// While it runs: what to do when the system asks.
    pub running: Option<&'static str>,
    /// The last report, one line a step.
    pub report: Vec<ReportLine>,
    /// Setup did not run at all: the text of why.
    pub failed: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct About<'a> {
    pub version: &'static str,
    /// Under the version: the daemon's version, or that there is no daemon.
    pub daemon: String,
    pub folder: &'a Path,
    /// The user's home folder: a folder under it is shown as `~/…`, with
    /// the full path in a tooltip. Display only.
    pub home: Option<&'a Path>,
}

/// The helper operations that trust Marsa's certificates, and those that make
/// `.test` names reach Marsa, by the names the reports use.
const TRUST_OPS: [&str; 1] = ["trust_install"];
const NAMES_OPS: [&str; 3] = ["resolver_install", "resolved_install", "hosts_sync"];

/// Decides what Settings says. Pure: the same model gives the same view.
pub fn view(model: &Model) -> SettingsView<'_> {
    SettingsView {
        general: General {
            autostart: autostart(model),
            appearance: model.appearance,
        },
        setup: setup(model, Platform::current()),
        about: About {
            version: model::APP_VERSION,
            daemon: strings::daemon_hint(model.session.connection.daemon_version()),
            folder: &model.data_dir,
            home: model.home.as_deref(),
        },
    }
}

fn autostart(model: &Model) -> AutostartRow {
    let off = if !model.is_connected() {
        Some(strings::NEEDS_DAEMON_HINT)
    } else {
        match model.session.autostart {
            Autostart::Unknown => Some(strings::AUTOSTART_READING),
            Autostart::Unsupported => Some(strings::AUTOSTART_UNSUPPORTED),
            Autostart::Off | Autostart::On | Autostart::Stale => None,
        }
    };
    AutostartRow {
        on: model.session.autostart == Autostart::On,
        off,
        hint: model
            .session
            .autostart
            .note()
            .unwrap_or(strings::AUTOSTART_HINT),
    }
}

fn setup(model: &Model, platform: Option<Platform>) -> SetupGroup {
    let running = model.session.setup == Setup::Running;
    let (mut names, mut names_hint) = (
        check(model, &NAMES_OPS),
        strings::TEST_NAMES_HINT.to_owned(),
    );
    // Linux on the hosts way: the daemon reads the hosts file, so the window
    // knows without a setup run.
    if model.session.hosts.used {
        if model.session.hosts.missing.is_empty() {
            names = Check::Done;
        } else {
            names = Check::Failed;
            names_hint = strings::hosts_waiting(&model.session.hosts.missing);
        }
    }
    let run_off = if !model.is_connected() {
        Some(strings::NEEDS_DAEMON_HINT)
    } else if running {
        Some(strings::SETUP_IS_RUNNING)
    } else {
        None
    };
    let (report, failed) = match &model.session.setup {
        Setup::Finished(report) => (strings::setup_lines(report), None),
        Setup::Failed(code) => (Vec::new(), Some(strings::error_text(code))),
        Setup::Idle | Setup::Running => (Vec::new(), None),
    };
    SetupGroup {
        cert: check(model, &TRUST_OPS),
        names,
        names_hint,
        explanation: strings::setup_explanation(platform),
        run_off,
        running: running.then(|| strings::setup_running(platform)),
        report,
        failed,
    }
}

/// How the last setup of this session did the step of one of `ops`: the
/// newest result of the report, else whether it left the step out.
fn check(model: &Model, ops: &[&str]) -> Check {
    let Setup::Finished(report) = &model.session.setup else {
        return Check::NotChecked;
    };
    let ours = |op: &str| ops.contains(&op);
    if let Some(result) = report.results.iter().rev().find(|r| ours(&r.op)) {
        return match result.status {
            OpStatus::Done | OpStatus::Unchanged => Check::Done,
            OpStatus::Failed => Check::Failed,
        };
    }
    if report.skipped.iter().any(|skipped| ours(&skipped.op)) {
        Check::Skipped
    } else {
        Check::NotChecked
    }
}

/// Draws `view`. Returns what the user did.
pub fn show(ui: &mut Ui, view: &SettingsView<'_>, theme: &Theme) -> Vec<Action> {
    let mut actions = Vec::new();
    ui.spacing_mut().item_spacing.y = theme.space.section_gap;
    // The column keeps its width in a wide window, at the start, and takes
    // the whole window when that is narrower.
    ui.set_max_width(ui.available_width().min(SETTINGS_WIDTH));
    ui.add(Text::new(
        strings::SETTINGS,
        theme.text.title,
        theme.color.ink,
    ));
    group(ui, theme, strings::SETTINGS_GENERAL, |ui| {
        general(ui, theme, &view.general, &mut actions);
    });
    group(ui, theme, strings::SETTINGS_SETUP, |ui| {
        setup_group(ui, theme, &view.setup, &mut actions);
    });
    group(ui, theme, strings::SETTINGS_ABOUT, |ui| {
        about(ui, theme, &view.about, &mut actions);
    });
    actions
}

/// A heading over a card.
fn group(ui: &mut Ui, theme: &Theme, title: &str, add: impl FnOnce(&mut Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = theme.space.group_gap;
        ui.add(Text::new(title, theme.text.heading, theme.color.ink));
        add(ui);
    });
}

fn general(ui: &mut Ui, theme: &Theme, general: &General, actions: &mut Vec<Action>) {
    let login = &general.autostart;
    let options = [Appearance::System, Appearance::Light, Appearance::Dark]
        .map(|appearance| (appearance, strings::appearance_name(appearance)));
    setting_card(ui, |rows| {
        let mut flipped = false;
        let row = rows.row(strings::AUTOSTART, Some(login.hint), |ui| {
            let switch = Switch::new(login.on, strings::AUTOSTART)
                .bare()
                .disabled_if(login.off.is_some(), login.off.unwrap_or_default());
            flipped = ui.add(switch).clicked();
        });
        // The name is part of the click target, as for any switch.
        if (flipped || row.clicked()) && login.off.is_none() {
            actions.push(Action::ToggleAutostart);
        }
        rows.row(strings::APPEARANCE, Some(strings::APPEARANCE_HINT), |ui| {
            let control = Segmented::new(strings::APPEARANCE, &options, general.appearance);
            if let Some(chosen) = control.show(ui).inner {
                actions.push(Action::SetAppearance(chosen));
            }
        });
        // A fixed value, not a control that does nothing.
        rows.row(strings::LANGUAGE, Some(strings::LANGUAGE_HINT), |ui| {
            value(ui, theme, strings::LANGUAGE_NAME);
        });
    });
}

fn setup_group(ui: &mut Ui, theme: &Theme, setup: &SetupGroup, actions: &mut Vec<Action>) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = theme.space.group_gap;
        setting_card(ui, |rows| {
            rows.row(strings::HTTPS_CERT, Some(strings::HTTPS_CERT_HINT), |ui| {
                check_value(ui, theme, setup.cert);
            });
            rows.row(strings::TEST_NAMES, Some(&setup.names_hint), |ui| {
                check_value(ui, theme, setup.names);
            });
            rows.row(strings::SETUP_ROW, Some(setup.explanation), |ui| {
                let button = Button::secondary(strings::RUN_SETUP)
                    .disabled_if(setup.run_off.is_some(), setup.run_off.unwrap_or_default());
                let response = ui.add(button);
                if response.clicked() {
                    actions.push(Action::RunSetup);
                }
                if setup.run_off.is_none() {
                    response.on_hover_text(strings::RUN_SETUP_HINT);
                }
            });
        });
        if let Some(words) = setup.running {
            mute(ui, theme, words);
        }
        if let Some(failed) = &setup.failed {
            status_line(ui, theme, Mark::Error, failed, |_| {});
        }
        for line in &setup.report {
            if line.problem {
                status_line(ui, theme, Mark::Error, &line.text, |_| {});
            } else {
                mute(ui, theme, &line.text);
            }
        }
    });
}

fn about(ui: &mut Ui, theme: &Theme, about: &About<'_>, actions: &mut Vec<Action>) {
    setting_card(ui, |rows| {
        rows.row(strings::APP_VERSION, Some(&about.daemon), |ui| {
            value(ui, theme, about.version);
        });
        rows.row(
            strings::DATA_FOLDER,
            Some(strings::DATA_FOLDER_HINT),
            |ui| {
                // From the end: the button, then the path before it.
                if ui.add(Button::secondary(strings::OPEN_FOLDER)).clicked() {
                    actions.push(Action::OpenDataFolder);
                }
                super::lines::path(ui, theme, about.folder, about.home);
            },
        );
    });
}

/// A value at the end of a row: the words in `ink2`.
fn value(ui: &mut Ui, theme: &Theme, text: &str) {
    ui.add(Text::small(theme, text));
}

/// How a step stands: an icon, a color, and the word, never the color alone.
fn check_value(ui: &mut Ui, theme: &Theme, check: Check) {
    let (icon, color) = match check {
        Check::Done => (Icon::Check, theme.color.ok),
        Check::Failed => (Icon::Cross, theme.color.error),
        Check::Skipped => (Icon::Alert, theme.section(Section::Dumps).fg),
        Check::NotChecked => (Icon::Dash, theme.color.mute),
    };
    // From the end: the word, then the icon before it.
    value(ui, theme, check.word());
    ui.add(Glyph::new(icon).size(theme.size.icon_inline).color(color));
}

#[cfg(test)]
mod tests {
    use crate::model::Effect;
    use std::path::PathBuf;

    use eframe::egui::{self, Rect};
    use marsa_core::types::{Health, HostsStatus, OpResult, SetupReport, SetupSkip};

    use super::*;
    use crate::bridge::Command;
    use crate::model::{Connection, Update};
    use crate::theme::LIGHT;
    use crate::widgets::harness::{Harness, Tree};

    /// An absolute path on any system: a drive on Windows, where a path that
    /// starts with `/` is not absolute and `~` is never used.
    fn abs(unix: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{}", unix.replace('/', "\\")))
        } else {
            PathBuf::from(unix)
        }
    }

    fn connected() -> Model {
        let mut model = Model::default();
        model.update(Update::Connected(Health {
            app_version: "0.3.1".into(),
            api_version: 1,
            pid: 4242,
        }));
        model.data_dir = abs("/home/me/Marsa");
        model.home = Some(abs("/home/me"));
        model
    }

    fn result(op: &str, status: OpStatus) -> OpResult {
        OpResult {
            op: op.into(),
            status,
            code: (status == OpStatus::Failed).then(|| "resolver_foreign".to_owned()),
            message: None,
        }
    }

    fn report(results: Vec<OpResult>, skipped: &[&str]) -> SetupReport {
        SetupReport {
            results,
            skipped: skipped
                .iter()
                .map(|op| SetupSkip {
                    op: (*op).into(),
                    code: "redirect_port_held".into(),
                })
                .collect(),
            ..SetupReport::default()
        }
    }

    // -- view --

    #[test]
    fn the_about_group_has_the_version_the_daemon_and_the_data_folder() {
        let model = connected();
        let about = view(&model).about;
        assert_eq!(about.version, model::APP_VERSION);
        assert_eq!(about.daemon, "Daemon version 0.3.1");
        assert_eq!(about.folder, model.data_dir.as_path());
        assert_eq!(about.home, model.home.as_deref());
        // No daemon: the row says so.
        let down = Model::default();
        assert_eq!(view(&down).about.daemon, strings::DAEMON_NOT_CONNECTED);
        assert_eq!(down.session.connection, Connection::Connecting);
    }

    #[test]
    fn start_at_login_follows_the_daemons_state_and_says_why_it_is_off() {
        let mut model = connected();
        let login = |model: &Model| view(model).general.autostart;
        // Not read yet.
        assert_eq!(login(&model).off, Some(strings::AUTOSTART_READING));
        for (state, on, off, hint) in [
            (Autostart::Off, false, None, strings::AUTOSTART_HINT),
            (Autostart::On, true, None, strings::AUTOSTART_HINT),
            (
                Autostart::Stale,
                false,
                None,
                Autostart::Stale.note().expect("a note"),
            ),
            (
                Autostart::Unsupported,
                false,
                Some(strings::AUTOSTART_UNSUPPORTED),
                Autostart::Unsupported.note().expect("a note"),
            ),
        ] {
            model.update(Update::Autostart(state));
            let row = login(&model);
            assert_eq!((row.on, row.off, row.hint), (on, off, hint), "{state:?}");
        }
        // Without the daemon nothing can change.
        let mut down = Model::default();
        down.session.autostart = Autostart::On;
        assert_eq!(login(&down).off, Some(strings::NEEDS_DAEMON_HINT));
    }

    #[test]
    fn the_appearance_is_the_models() {
        let mut model = connected();
        assert_eq!(view(&model).general.appearance, Appearance::System);
        model.apply_action(Action::SetAppearance(Appearance::Dark));
        assert_eq!(view(&model).general.appearance, Appearance::Dark);
    }

    #[test]
    fn the_window_claims_nothing_before_a_setup_ran() {
        let mut model = connected();
        for setup in [
            Setup::Idle,
            Setup::Running,
            Setup::Failed("setup_cancelled".into()),
        ] {
            model.session.setup = setup;
            let group = view(&model).setup;
            assert_eq!(
                (group.cert, group.names),
                (Check::NotChecked, Check::NotChecked)
            );
        }
        // A report that holds neither step is no answer either.
        model.session.setup = Setup::Finished(report(Vec::new(), &[]));
        let group = view(&model).setup;
        assert_eq!(
            (group.cert, group.names),
            (Check::NotChecked, Check::NotChecked)
        );
        assert_eq!(group.names_hint, strings::TEST_NAMES_HINT);
    }

    #[test]
    fn a_setup_report_of_this_session_says_each_step_by_its_own_result() {
        let mut model = connected();
        model.session.setup = Setup::Finished(report(
            vec![
                result("resolver_install", OpStatus::Done),
                result("pf_install", OpStatus::Done),
                result("trust_install", OpStatus::Unchanged),
            ],
            &[],
        ));
        let group = view(&model).setup;
        assert_eq!((group.cert, group.names), (Check::Done, Check::Done));

        // The step that failed, and the one the helper left out.
        model.session.setup = Setup::Finished(report(
            vec![result("resolver_install", OpStatus::Failed)],
            &["trust_install"],
        ));
        let group = view(&model).setup;
        assert_eq!((group.cert, group.names), (Check::Skipped, Check::Failed));
        // The other systems name their steps differently; the newest result
        // of a step wins (Linux runs a second request).
        model.session.setup = Setup::Finished(report(
            vec![
                result("hosts_sync", OpStatus::Failed),
                result("hosts_sync", OpStatus::Done),
                result("trust_install", OpStatus::Done),
            ],
            &[],
        ));
        let group = view(&model).setup;
        assert_eq!((group.cert, group.names), (Check::Done, Check::Done));
    }

    #[test]
    fn on_linux_the_hosts_file_tells_the_names_without_a_setup_run() {
        let mut model = connected();
        model.update(Update::Hosts(HostsStatus {
            used: true,
            missing: Vec::new(),
        }));
        assert_eq!(view(&model).setup.names, Check::Done);
        model.update(Update::Hosts(HostsStatus {
            used: true,
            missing: vec!["shop.test".into()],
        }));
        let group = view(&model).setup;
        assert_eq!(group.names, Check::Failed);
        assert_eq!(
            group.names_hint,
            strings::hosts_waiting(&["shop.test".into()])
        );
        // The certificate is still not known.
        assert_eq!(group.cert, Check::NotChecked);
    }

    #[test]
    fn run_setup_keeps_its_explanation_its_wait_and_its_report() {
        let mut model = connected();
        let group = view(&model).setup;
        assert_eq!(
            group.explanation,
            strings::setup_explanation(Platform::current())
        );
        assert_eq!((group.run_off, group.running), (None, None));
        assert!(group.report.is_empty() && group.failed.is_none());

        model.start_setup();
        let group = view(&model).setup;
        assert_eq!(group.run_off, Some(strings::SETUP_IS_RUNNING));
        assert_eq!(
            group.running,
            Some(strings::setup_running(Platform::current()))
        );

        let done = report(vec![result("trust_install", OpStatus::Done)], &[]);
        model.update(Update::SetupDone(done.clone()));
        let group = view(&model).setup;
        assert_eq!(group.report, strings::setup_lines(&done));
        assert_eq!(group.run_off, None);

        model.update(Update::SetupFailed("setup_not_supported".into()));
        let group = view(&model).setup;
        assert_eq!(
            group.failed,
            Some(strings::error_text("setup_not_supported"))
        );
        assert!(group.report.is_empty());

        // No daemon: no setup.
        assert_eq!(
            view(&Model::default()).setup.run_off,
            Some(strings::NEEDS_DAEMON_HINT)
        );
    }

    // -- show --

    fn draw(ui: &mut Ui, model: &Model) -> Vec<Action> {
        show(ui, &view(model), &LIGHT)
    }

    fn start(model: &Model) -> Tree {
        let harness = Harness::new(egui::Theme::Light);
        for _ in 0..3 {
            harness.run(|ui| {
                draw(ui, model);
            });
        }
        harness.accessibility(|ui| {
            draw(ui, model);
        })
    }

    fn middle(tree: &Tree, label: &str) -> egui::Pos2 {
        let node = tree
            .node_with_label(label)
            .or_else(|| tree.node_with_text(label))
            .unwrap_or_else(|| panic!("no widget named {label}"));
        let b = node.bounds().expect("a node with bounds");
        egui::pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32)
    }

    /// The actions of the frame where `label` is clicked.
    fn click(model: &Model, label: &str) -> Vec<Action> {
        let tree = start(model);
        let at = middle(&tree, label);
        click_at(model, at)
    }

    fn click_at(model: &Model, at: egui::Pos2) -> Vec<Action> {
        let harness = Harness::new(egui::Theme::Light);
        for _ in 0..3 {
            harness.run(|ui| {
                draw(ui, model);
            });
        }
        let mut actions = Vec::new();
        harness.click(at, |ui| actions.extend(draw(ui, model)));
        actions
    }

    #[test]
    fn the_screen_has_a_title_three_groups_and_every_row() {
        let mut model = connected();
        model.update(Update::Autostart(Autostart::Off));
        let tree = start(&model);
        for words in [
            strings::SETTINGS,
            strings::SETTINGS_GENERAL,
            strings::SETTINGS_SETUP,
            strings::SETTINGS_ABOUT,
            strings::AUTOSTART,
            strings::APPEARANCE,
            strings::LANGUAGE,
            strings::LANGUAGE_NAME,
            strings::HTTPS_CERT,
            strings::TEST_NAMES,
            strings::SETUP_ROW,
            strings::RUN_SETUP,
            strings::APP_VERSION,
            strings::DATA_FOLDER,
            strings::OPEN_FOLDER,
            "Daemon version 0.3.1",
            strings::setup_explanation(Platform::current()),
        ] {
            assert!(tree.node_with_text(words).is_some(), "{words}");
        }
        // Appearance is a segmented control with three options.
        for option in ["System", "Light", "Dark"] {
            assert_eq!(
                tree.node_with_label(option).map(|node| node.role()),
                Some(egui::Role::RadioButton),
                "{option}"
            );
        }
        // Language is a fixed value: no control to change it.
        assert!(tree.node_with_label(strings::LANGUAGE).is_none());
        // Nothing is claimed before setup ran.
        assert!(tree.node_with_text(Check::NotChecked.word()).is_some());
        assert!(tree.node_with_text(Check::Done.word()).is_none());
    }

    #[test]
    fn the_data_folder_shows_as_tilde_and_its_full_path_is_one_hover_away() {
        let model = connected();
        let tree = start(&model);
        let shown = strings::home_path(&model.data_dir, model.home.as_deref());
        assert!(shown.starts_with('~'), "{shown}");
        assert!(tree.node_with_text(&shown).is_some(), "{shown}");
        assert!(
            tree.node_with_text(&model.data_dir.display().to_string())
                .is_none(),
            "the full path is the tooltip, not the text"
        );
    }

    #[test]
    fn the_switch_and_its_name_flip_start_at_login_and_a_switch_that_is_off_does_not() {
        let mut model = connected();
        model.update(Update::Autostart(Autostart::Off));
        assert_eq!(
            click(&model, strings::AUTOSTART),
            [Action::ToggleAutostart],
            "the switch"
        );
        // The name beside it is part of the target.
        let tree = start(&model);
        let name = tree
            .node_with_text(strings::AUTOSTART_HINT)
            .and_then(|node| node.bounds())
            .expect("the hint");
        let at = egui::pos2(name.x0 as f32 + 4.0, ((name.y0 + name.y1) / 2.0) as f32);
        assert_eq!(click_at(&model, at), [Action::ToggleAutostart]);

        // Not read yet: nothing to flip.
        let mut model = connected();
        assert!(click(&model, strings::AUTOSTART).is_empty());
        model.update(Update::Autostart(Autostart::Unsupported));
        assert!(click(&model, strings::AUTOSTART).is_empty());
    }

    #[test]
    fn the_switch_action_reaches_the_daemon_and_the_switch_shows_the_new_state_at_once() {
        let mut model = connected();
        model.update(Update::Autostart(Autostart::Off));
        let command = model.apply_action(Action::ToggleAutostart);
        assert!(
            matches!(command, Effect::Send(Command::SetAutostart(true))),
            "{command:?}"
        );
        assert!(view(&model).general.autostart.on);
    }

    #[test]
    fn an_option_of_the_appearance_control_is_an_action_and_the_chosen_one_is_not() {
        let model = connected();
        assert_eq!(
            click(&model, "Dark"),
            [Action::SetAppearance(Appearance::Dark)]
        );
        assert_eq!(
            click(&model, "Light"),
            [Action::SetAppearance(Appearance::Light)]
        );
        assert!(click(&model, "System").is_empty());
    }

    #[test]
    fn run_setup_is_an_action_and_off_while_it_runs_or_with_no_daemon() {
        let mut model = connected();
        assert_eq!(click(&model, strings::RUN_SETUP), [Action::RunSetup]);
        model.start_setup();
        assert!(click(&model, strings::RUN_SETUP).is_empty());
        assert!(
            start(&model)
                .node_with_label(strings::RUN_SETUP)
                .expect("the button")
                .is_disabled()
        );
        // The wait is said under the card.
        assert!(
            start(&model)
                .node_with_text(strings::setup_running(Platform::current()))
                .is_some()
        );
        let down = Model::default();
        assert!(click(&down, strings::RUN_SETUP).is_empty());
    }

    #[test]
    fn a_report_shows_a_line_a_step_and_a_failure_with_its_mark() {
        let mut model = connected();
        let done = report(
            vec![
                result("trust_install", OpStatus::Done),
                result("resolver_install", OpStatus::Failed),
            ],
            &[],
        );
        model.update(Update::SetupDone(done.clone()));
        let tree = start(&model);
        for line in strings::setup_lines(&done) {
            assert!(tree.node_with_text(&line.text).is_some(), "{}", line.text);
        }
        // The steps read by their own result.
        assert!(tree.node_with_text(Check::Done.word()).is_some());
        assert!(tree.node_with_text(Check::Failed.word()).is_some());
        model.update(Update::SetupFailed("setup_not_supported".into()));
        let tree = start(&model);
        assert!(
            tree.node_with_text(&strings::error_text("setup_not_supported"))
                .is_some()
        );
    }

    #[test]
    fn open_folder_is_a_local_action_for_the_data_folder() {
        let model = connected();
        assert_eq!(
            click(&model, strings::OPEN_FOLDER),
            [Action::OpenDataFolder]
        );
        // The model only says which folder: `App` opens it.
        let mut model = connected();
        let effect = model.apply_action(Action::OpenDataFolder);
        assert!(matches!(effect, Effect::OpenFolder(folder) if folder == model.data_dir));
    }

    #[test]
    fn the_rows_fit_the_smallest_window() {
        let model = connected();
        let harness = Harness::new(egui::Theme::Light);
        let min = crate::theme::SIZE.window_min;
        // The main area of the smallest window: the folded sidebar and the
        // margins are gone.
        let width =
            min.x - LIGHT.size.sidebar_folded - LIGHT.space.screen_side - LIGHT.space.screen_side;
        let mut used = Rect::NOTHING;
        for _ in 0..3 {
            harness.run(|ui| {
                ui.set_max_width(width);
                draw(ui, &model);
                used = ui.min_rect();
            });
        }
        assert!(used.width() <= width + 0.5, "{} of {width}", used.width());
    }
}
