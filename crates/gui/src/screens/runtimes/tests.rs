use crate::model::Effect;
use std::path::PathBuf;

use eframe::egui::{self, Pos2, pos2};
use marsa_core::types::{
    Event, InstallDone, InstallEvent, InstallProgress, InstallUpdate, RuntimeList, RuntimeVersions,
    Site, SiteKind, SiteList, Source, VersionInUse, XdebugState,
};

use super::*;
use crate::bridge::Command;
use crate::fixtures::connected;
use crate::model::{Change, InstallJob, UninstallNotice, Update, apply};
use crate::theme::LIGHT;
use crate::widgets::harness::{Harness, Tree};

fn v(text: &str) -> Version {
    text.parse().expect("a version")
}

fn site(name: &str, php: &str, source: Source) -> Site {
    let root = PathBuf::from("/code").join(name);
    Site {
        name: name.into(),
        docroot: root.join("public"),
        root,
        kind: SiteKind::Laravel,
        php: Some(v(php)),
        php_source: Some(source),
        secure: true,
        queue: false,
        schedule: false,
    }
}

/// PHP 8.2.28, 8.3.27, and 8.4.23 (default `8.4`), Node 22.11.0
/// (default `22`), Composer 2.8.3, and no Caddy; three sites: `shop`
/// runs the default, `pinned` is set to 8.3.27, and `wiki` has a version
/// file for 8.3.27.
fn model() -> Model {
    let mut model = connected();
    let row = |runtime, installed: &[&str], default: Option<&str>| RuntimeVersions {
        runtime,
        installed: installed.iter().map(|text| (*text).to_owned()).collect(),
        default: default.map(str::to_owned),
    };
    model.update(Update::Runtimes(RuntimeList {
        runtimes: vec![
            row(Runtime::Php, &["8.2.28", "8.3.27", "8.4.23"], Some("8.4")),
            row(Runtime::Node, &["22.11.0"], Some("22")),
            row(Runtime::Composer, &["2.8.3"], None),
            row(Runtime::Caddy, &[], None),
        ],
    }));
    let file = PathBuf::from("/code/wiki/.php-version");
    model.update(Update::Sites(SiteList {
        sites: vec![
            site("shop", "8.4.23", Source::GlobalDefault),
            site("pinned", "8.3.27", Source::ProjectSetting),
            site("wiki", "8.3.27", Source::VersionFile(file)),
        ],
        ..SiteList::default()
    }));
    model.route = Route::Runtimes(Runtime::Php);
    model
}

fn with_route(mut model: Model, runtime: Runtime) -> Model {
    model.route = Route::Runtimes(runtime);
    model
}

fn read(model: &Model) -> Box<Read> {
    match view(model) {
        RuntimesView::Read(read) => read,
        RuntimesView::Unread => panic!("the runtimes were read"),
    }
}

fn job(runtime: Runtime, version: Option<&str>) -> InstallJob {
    InstallJob {
        runtime,
        version: version.map(str::to_owned),
    }
}

// -- the descriptors --

fn by_runtime(runtime: Runtime) -> Descriptor {
    descriptors()
        .into_iter()
        .find(|d| d.runtime == runtime)
        .expect("a descriptor for every runtime")
}

#[test]
fn the_list_is_every_runtime_core_has_in_its_order() {
    let listed: Vec<Runtime> = descriptors().iter().map(|d| d.runtime).collect();
    let want: Vec<Runtime> = Runtime::ALL
        .into_iter()
        .filter(|r| kind_for(*r).listed())
        .collect();
    assert_eq!(listed, want);
    // The engines belong under Databases and Services.
    assert!(!listed.contains(&Runtime::Postgres));
}

#[test]
fn only_php_has_site_counts_and_the_xdebug_extra() {
    for d in descriptors() {
        let php = d.runtime == Runtime::Php;
        assert_eq!(d.sites, php, "{}", d.name);
        assert_eq!(d.extras == [Extra::Xdebug], php, "{}", d.name);
        assert_eq!(d.section, Section::Runtimes);
    }
}

#[test]
fn php_and_node_have_a_default_and_composer_and_caddy_use_the_newest() {
    let has: Vec<(&str, bool)> = descriptors().iter().map(|d| (d.name, d.default)).collect();
    assert_eq!(
        has,
        [
            ("PHP", true),
            ("Node.js", true),
            ("Composer", false),
            ("Caddy", false)
        ]
    );
}

#[test]
fn names_and_letters_come_from_the_strings_and_every_tile_has_its_own_letters() {
    let letters: Vec<String> = descriptors().into_iter().map(|d| d.monogram).collect();
    assert_eq!(letters, ["PHP", "JS", "Co", "Ca"]);
    assert_eq!(by_runtime(Runtime::Node).example, "22");
}

// -- view --

#[test]
fn the_list_has_every_runtime_with_its_default_and_how_many_versions() {
    let read = read(&model());
    let lines: Vec<(&str, &str, &str)> = read
        .entries
        .iter()
        .map(|e| (e.name, e.monogram.as_str(), e.summary.as_str()))
        .collect();
    assert_eq!(
        lines,
        [
            ("PHP", "PHP", "Default 8.4, 3 installed"),
            ("Node.js", "JS", "Default 22, 1 installed"),
            ("Composer", "Co", "2.8.3, newest is used"),
            ("Caddy", "Ca", "None installed"),
        ]
    );
    assert_eq!(read.chosen, 0);
}

#[test]
fn the_route_chooses_the_runtime_and_the_last_one_is_kept_off_the_screen() {
    let mut model = model();
    for (runtime, at) in [(Runtime::Node, 1), (Runtime::Caddy, 3), (Runtime::Php, 0)] {
        model.route = Route::Runtimes(runtime);
        let read = read(&model);
        assert_eq!(read.chosen, at, "{runtime:?}");
        assert_eq!(read.details.runtime, runtime);
    }
    // On another screen, the one it showed last.
    model.apply_action(Action::Navigate(Route::Runtimes(Runtime::Node)));
    model.apply_action(Action::Navigate(Route::Home));
    assert_eq!(read(&model).details.runtime, Runtime::Node);
}

#[test]
fn no_runtimes_read_says_so_and_draws_no_list() {
    assert_eq!(view(&Model::default()), RuntimesView::Unread);
    assert_eq!(view(&connected()), RuntimesView::Unread);
}

#[test]
fn the_default_is_on_the_large_buttons_and_a_pick_is_the_set_default_command() {
    let mut model = model();
    let first = read(&model);
    let VersionSection::Default(picker) = &first.details.version else {
        panic!("PHP has a default");
    };
    assert_eq!(picker.heading, "Default PHP");
    let labels: Vec<&str> = picker.choices.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["8.2", "8.3", "8.4"]);
    assert_eq!(picker.chosen, Some(2));
    assert_eq!(picker.install, "Install another PHP version");

    // The action is the model's existing command.
    let command = model.apply_action(Action::SetDefault {
        runtime: Runtime::Php,
        version: v("8.3.27"),
    });
    assert!(matches!(
        command,
        Effect::Send(Command::Save(Change::Default { runtime: Runtime::Php, ref version })) if version == "8.3"
    ));
    // The button shows the pick until the daemon answers.
    let VersionSection::Default(picker) = read(&model).details.version else {
        panic!("PHP has a default");
    };
    assert_eq!(picker.chosen, Some(1));
}

#[test]
fn a_runtime_with_no_default_has_a_sentence_and_the_newest_version_in_use() {
    let read = read(&with_route(model(), Runtime::Composer));
    assert_eq!(
        read.details.version,
        VersionSection::Newest {
            hint: strings::newest_is_used("Composer")
        }
    );
    let tags: Vec<_> = read.details.rows.iter().map(|r| r.tag).collect();
    assert_eq!(tags, [Some("In use")]);
    // Nothing installed: nothing to say about a version.
    let none = self::read(&with_route(model(), Runtime::Caddy));
    assert_eq!(none.details.version, VersionSection::Empty);
    assert!(none.details.rows.is_empty());
}

#[test]
fn the_installed_table_lists_newest_first_with_the_sites_that_run_each_version() {
    let read = read(&model());
    let rows: Vec<(&str, Option<&str>, Option<&str>)> = read
        .details
        .rows
        .iter()
        .map(|r| (r.text.as_str(), r.used.as_deref(), r.tag))
        .collect();
    assert_eq!(
        rows,
        [
            ("8.4.23", Some("1 site: shop"), Some("Default")),
            ("8.3.27", Some("2 sites: pinned, wiki"), None),
            ("8.2.28", Some("No sites"), None),
        ]
    );
    assert!(read.details.sites_column);
    // Another runtime has no such column, and no count.
    let node = self::read(&with_route(model(), Runtime::Node));
    assert!(!node.details.sites_column);
    assert!(node.details.rows.iter().all(|row| row.used.is_none()));
    assert_eq!(node.details.rows[0].tag, Some("Default"));
}

#[test]
fn a_site_the_user_just_moved_counts_at_its_new_version_at_once() {
    let mut model = model();
    model.choose_site_php("shop", crate::model::Choice::Version(v("8.2.28")));
    let used: Vec<Option<String>> = read(&model)
        .details
        .rows
        .iter()
        .map(|r| r.used.clone())
        .collect();
    assert_eq!(
        used,
        [
            Some("No sites".to_owned()),
            Some("2 sites: pinned, wiki".to_owned()),
            Some("1 site: shop".to_owned()),
        ]
    );
}

#[test]
fn uninstall_is_refused_in_the_row_for_the_default_and_for_a_version_a_site_is_set_to() {
    let rows = read(&model()).details.rows;
    let off = |at: usize| rows[at].uninstall_off.as_deref();
    assert_eq!(
        off(0),
        Some("It is the default PHP. Pick another default first.")
    );
    // `pinned` has its own setting; `wiki` only a version file, which
    // the daemon does not count.
    assert_eq!(
        off(1),
        Some("The site pinned.test is set to it. Pick another PHP version for it first.")
    );
    assert_eq!(off(2), None, "no site, not the default");
}

#[test]
fn the_only_version_can_be_uninstalled_though_it_is_the_default_and_the_daemon_judges_the_rest() {
    let read = read(&with_route(model(), Runtime::Node));
    assert_eq!(read.details.rows[0].uninstall_off, None);
}

#[test]
fn while_a_job_runs_uninstall_and_install_are_off_with_the_busy_reason() {
    let mut model = model();
    assert!(model.queue_install(job(Runtime::Caddy, None)));
    let read = read(&model);
    assert_eq!(read.details.install_off, Some(strings::INSTALL_BUSY_HINT));
    assert_eq!(
        read.details.rows[2].uninstall_off.as_deref(),
        Some(strings::UNINSTALL_BUSY_HINT)
    );
    // A refusal about the default stays the reason: it does not end.
    assert!(
        read.details.rows[0]
            .uninstall_off
            .as_deref()
            .is_some_and(|text| text.starts_with("It is the default"))
    );
    // Idle: nothing is off.
    assert_eq!(self::read(&self::model()).details.install_off, None);
}

#[test]
fn xdebug_is_a_section_of_php_only() {
    let mut model = model();
    for (runtime, has) in [
        (Runtime::Php, true),
        (Runtime::Node, false),
        (Runtime::Composer, false),
        (Runtime::Caddy, false),
    ] {
        model.route = Route::Runtimes(runtime);
        assert_eq!(read(&model).details.xdebug.is_some(), has, "{runtime:?}");
    }
}

#[test]
fn the_xdebug_switch_says_what_it_does_and_why_it_is_off() {
    let mut model = model();
    let xdebug = |model: &Model| read(model).details.xdebug.expect("PHP has Xdebug");
    // Not read yet: off, with the reason.
    let unread = xdebug(&model);
    assert!(!unread.on && unread.lines.is_empty());
    assert_eq!(unread.off, Some(strings::XDEBUG_UNREAD));
    model.update(Update::Xdebug(XdebugState {
        enabled: true,
        loaded: vec!["8.3.27".into(), "8.4.23".into()],
        missing: Vec::new(),
    }));
    let on = xdebug(&model);
    assert!(on.on && on.off.is_none());
    // The slow-PHP note and where it is loaded.
    assert_eq!(on.lines[0], strings::XDEBUG_TURN_OFF);
    assert_eq!(on.lines[1], "Loaded in PHP 8.3.27, 8.4.23.");
    // The switch asks for the other state, and shows it at once.
    let sent = model.apply_action(Action::ToggleXdebug);
    assert!(matches!(sent, Effect::Send(Command::SetXdebug(false))));
    let asked = xdebug(&model);
    assert!(!asked.on);
    assert_eq!(asked.off, Some(strings::XDEBUG_BUSY));
    assert_eq!(asked.lines, ["Turning Xdebug off…"]);
    model.update(Update::XdebugAnswered(Err("xdebug_interrupted".into())));
    assert_eq!(
        xdebug(&model).error,
        Some(strings::error_text("xdebug_interrupted"))
    );
}

#[test]
fn the_dialog_is_closed_until_asked_and_then_says_what_to_type_for_that_runtime() {
    let mut model = model();
    assert_eq!(install_dialog_view(&model), None);
    model.apply_action(Action::AskInstall(Runtime::Node));
    let dialog = install_dialog_view(&model).expect("the dialog is open");
    assert_eq!(dialog.runtime, Runtime::Node);
    assert_eq!(dialog.title, "Install a Node.js version");
    assert_eq!(
        dialog.body,
        "A version such as 22: the newest Node.js that matches it. Empty: the newest stable version."
    );
    assert_eq!((dialog.text.as_str(), dialog.error), ("", None));
    model.apply_action(Action::SetInstallVersion {
        runtime: Runtime::Node,
        text: "lts/*".into(),
    });
    model.apply_action(Action::SubmitInstall(Runtime::Node));
    let dialog = install_dialog_view(&model).expect("the dialog stays");
    assert_eq!(dialog.text, "lts/*");
    assert_eq!(dialog.error, Some(strings::error_text("unsupported_alias")));
    // The error is the field's, not a notice of the bar.
    assert!(crate::shell::notices::current(&model).is_none());
}

#[test]
fn install_sends_the_job_for_the_text_and_an_empty_field_asks_for_the_newest() {
    let mut model = model();
    model.apply_action(Action::AskInstall(Runtime::Php));
    let sent = model
        .apply_action(Action::SubmitInstall(Runtime::Php))
        .command();
    assert!(matches!(sent, Some(Command::Install(ref sent)) if *sent == job(Runtime::Php, None)));
    assert_eq!(install_dialog_view(&model), None, "the dialog closes");

    let mut model = self::model();
    model.apply_action(Action::AskInstall(Runtime::Php));
    model.apply_action(Action::SetInstallVersion {
        runtime: Runtime::Php,
        text: " 8.5 ".into(),
    });
    let sent = model
        .apply_action(Action::SubmitInstall(Runtime::Php))
        .command();
    assert!(
        matches!(sent, Some(Command::Install(ref sent)) if *sent == job(Runtime::Php, Some("8.5")))
    );
}

#[test]
fn uninstall_opens_the_confirmation_and_never_sends_a_command() {
    let mut model = model();
    let command = model.apply_action(Action::AskUninstall {
        runtime: Runtime::Php,
        version: v("8.2.28"),
    });
    assert!(command.is_none());
    assert_eq!(
        model.session.confirm,
        Some(Removal::Uninstall {
            runtime: Runtime::Php,
            version: v("8.2.28"),
        })
    );
    assert!(
        model.removing().is_none(),
        "nothing is sent until confirmed"
    );
}

#[test]
fn an_install_that_runs_shows_its_bar_and_one_that_ended_its_line() {
    let mut model = model();
    assert!(model.queue_install(job(Runtime::Php, Some("8.4"))));
    let sent = model.next_install().expect("the job");
    model.update(Update::InstallAccepted { job: sent, id: 5 });
    let progress = |event| {
        Event::Install(InstallProgress {
            id: 5,
            update: InstallUpdate::Running(event),
        })
    };
    let selected = InstallEvent::Selected {
        runtime: Runtime::Php,
        version: "8.4.27".into(),
    };
    let downloading = InstallEvent::Downloading {
        artifact: "php.tar.gz".into(),
        done: 18_000_000,
        total: 31_000_000,
    };
    apply(&mut model, progress(selected));
    apply(&mut model, progress(downloading));
    let status = read(&model).status;
    let bar = status.progress.expect("a bar");
    assert_eq!(bar.words, "Downloading PHP 8.4.27: 18.0 of 31.0 MB");
    assert!(bar.fraction.is_some());
    // Running, it is a bar on the screen and no notice.
    assert!(crate::shell::notices::current(&model).is_none());

    let done = InstallUpdate::Done(InstallDone {
        runtime: Runtime::Php,
        version: "8.4.27".into(),
        dir: PathBuf::from("/d/runtimes/php/8.4.27"),
        already: false,
    });
    apply(
        &mut model,
        Event::Install(InstallProgress {
            id: 5,
            update: done,
        }),
    );
    let status = read(&model).status;
    assert!(status.progress.is_none());
    // How it ended is a notice of the bar.
    let notice = crate::shell::notices::current(&model).expect("a notice");
    assert_eq!(notice.notice.text, "Installed PHP 8.4.27.");
}

#[test]
fn a_refused_uninstall_is_one_sentence_in_the_bar() {
    let mut model = model();
    let removal = Removal::Uninstall {
        runtime: Runtime::Php,
        version: v("8.3.27"),
    };
    assert!(model.ask_removal(removal.clone()));
    assert_eq!(model.confirm_removal(), Some(removal.clone()));
    assert_eq!(
        read(&model).status.removing.as_deref(),
        Some("Uninstalling PHP 8.3.27…")
    );
    let in_use = VersionInUse {
        version: "8.3.27".into(),
        sites: vec!["pinned".into()],
        ..VersionInUse::default()
    };
    model.update(Update::NotRemoved {
        removal,
        code: "version_in_use".into(),
        in_use: Some(in_use),
    });
    let status = read(&model).status;
    assert_eq!(status.removing, None);
    // The bar says the refusal in one sentence; what uses the version is
    // said by the screen, beside the version's menu.
    let notice = crate::shell::notices::current(&model).expect("a notice");
    assert_eq!(
        notice.notice.text,
        "PHP 8.3.27 is in use, so it was not uninstalled."
    );
    // A done uninstall is one sentence.
    model.session.notices.set_uninstall(UninstallNotice::Done {
        runtime: Runtime::Php,
        version: v("8.2.28"),
    });
    let notice = crate::shell::notices::current(&model).expect("a notice");
    assert_eq!(notice.notice.text, "Uninstalled PHP 8.2.28.");
}

#[test]
fn a_failed_runtimes_read_is_a_notice_of_the_bar_and_not_a_line_of_the_screen() {
    let mut model = model();
    model.update(Update::RuntimesFailed("runtimes_read".into()));
    let notice = crate::shell::notices::current(&model).expect("a notice");
    assert_eq!(notice.notice.text, strings::error_text("runtimes_read"));
}

#[test]
fn languages_and_tools_are_listed_and_the_phase_two_engines_are_left_out() {
    let kinds: Vec<(&str, Kind)> = descriptors().iter().map(|d| (d.name, d.kind)).collect();
    assert_eq!(
        kinds,
        [
            ("PHP", Kind::Language),
            ("Node.js", Kind::Language),
            ("Composer", Kind::Tool),
            ("Caddy", Kind::Tool),
        ]
    );
    // The engines are not listed.
    for runtime in [Runtime::Postgres, Runtime::Mysql, Runtime::Valkey] {
        let engine = Descriptor::plain(runtime);
        assert_eq!(engine.kind, Kind::Engine, "{runtime:?}");
        assert!(!engine.kind.listed());
    }
    let all = [
        descriptor(Runtime::Php),
        Descriptor::plain(Runtime::Postgres),
        fake(Runtime::Composer, "python", "Py"),
        Descriptor::plain(Runtime::Mysql),
        Descriptor::plain(Runtime::Valkey),
    ];
    let shown: Vec<&str> = listed(all).iter().map(|d| d.name).collect();
    assert_eq!(shown, ["PHP", "python"]);
}

// -- a runtime core does not have yet --

/// A descriptor for a runtime that core does not have yet: `runtime`
/// only stands in for its key, so the fakes have versions to show.
fn fake(runtime: Runtime, name: &'static str, monogram: &str) -> Descriptor {
    Descriptor {
        kind: Kind::Language,
        name,
        monogram: monogram.to_owned(),
        example: "1.0",
        ..Descriptor::plain(runtime)
    }
}

/// a runtime that core adds later (Python, Go)
/// is in the list, with its details and its install dialog, and no line
/// of this screen changes. `Runtime::Composer` and `Runtime::Node` stand
/// in for the keys of variants that do not exist yet, so the fakes have
/// versions to show.
#[test]
fn a_runtime_core_adds_later_is_listed_and_detailed_with_no_screen_code() {
    let mut table = descriptors();
    table.push(fake(Runtime::Composer, "python", "Py"));
    table.push(Descriptor {
        default: true,
        ..fake(Runtime::Node, "go", "Go")
    });
    let model = model();

    // The fifth: listed with core's name, two letters, and a line.
    let RuntimesView::Read(plain) = build(&model, &table, 4) else {
        panic!("read");
    };
    let fifth = &plain.entries[4];
    assert_eq!(
        (fifth.name, fifth.monogram.as_str(), fifth.section),
        ("python", "Py", Section::Runtimes)
    );
    assert_eq!(fifth.summary, "2.8.3, newest is used");
    assert_eq!(plain.chosen, 4);
    // Details: no default to pick, no site counts, and no extra.
    assert_eq!(plain.details.name, "python");
    assert!(matches!(
        plain.details.version,
        VersionSection::Newest { .. }
    ));
    assert!(!plain.details.sites_column && plain.details.xdebug.is_none());
    assert_eq!(plain.details.rows[0].more, "More for python 2.8.3");
    assert_eq!(plain.details.rows[0].tag, Some("In use"));

    // The sixth has a default version: the large buttons, and the tag.
    let RuntimesView::Read(sixth) = build(&model, &table, 5) else {
        panic!("read");
    };
    let VersionSection::Default(picker) = &sixth.details.version else {
        panic!("a default picker");
    };
    assert_eq!(picker.heading, "Default go");
    assert_eq!(picker.install, "Install another go version");
    assert_eq!(sixth.details.rows[0].tag, Some("Default"));

    // Drawn: the list and the details have the new runtimes with no
    // code of their own, and a click on one chooses it.
    let draw_read = |read: Box<Read>| {
        let view = RuntimesView::Read(read);
        let harness = Harness::new(egui::Theme::Light);
        let mut actions = Vec::new();
        let tree = harness.accessibility(|ui| actions = show(ui, &view, &LIGHT));
        (harness, tree, view)
    };
    let RuntimesView::Read(read) = build(&model, &table, 4) else {
        panic!("read");
    };
    let (harness, tree, view) = draw_read(read);
    assert!(tree.node_with_label("python").is_some());
    assert!(tree.node_with_label("go").is_some());
    assert!(tree.node_with_label("Install a version…").is_some());
    assert!(tree.node_with_label("Xdebug").is_none());
    let mut actions = Vec::new();
    harness.click(bounds_center(&tree, "go"), |ui| {
        actions = show(ui, &view, &LIGHT);
    });
    assert_eq!(actions, [Action::Navigate(Route::Runtimes(Runtime::Node))]);
    // The dialog of a new runtime says what to type for it.
    let mut asked = self::model();
    asked.session.install_dialog = Some(Runtime::Composer);
    let dialog = dialog(&asked, &fake(Runtime::Composer, "python", "Py"));
    assert_eq!(dialog.title, "Install a python version");
    assert_eq!(
        dialog.body,
        "A version such as 1.0: the newest python that matches it. Empty: the newest stable version."
    );
}

// -- show --

fn bounds_center(tree: &Tree, label: &str) -> Pos2 {
    let node = tree
        .node_with_label(label)
        .or_else(|| tree.node_with_text(label))
        .unwrap_or_else(|| panic!("no widget named {label}"));
    let b = node.bounds().expect("a node with bounds");
    pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32)
}

/// The screen, then the dialog over it, as the shell draws them.
fn draw(ui: &mut Ui, model: &Model) -> Vec<Action> {
    let mut actions = show(ui, &view(model), &LIGHT);
    if let Some(dialog) = install_dialog_view(model) {
        actions.extend(show_install_dialog(ui.ctx(), &dialog));
    }
    actions
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

/// A click on `button`, which opens a menu, then one on its row `row`.
fn choose(model: &Model, button: &str, row: &str) -> Vec<Action> {
    let (harness, tree) = start(model);
    harness.click(bounds_center(&tree, button), |ui| {
        draw(ui, model);
    });
    assert!(egui::Popup::is_any_open(&harness.ctx), "{button} opens");
    let tree = harness.accessibility(|ui| {
        draw(ui, model);
    });
    let mut actions = Vec::new();
    harness.click(bounds_center(&tree, row), |ui| {
        actions.extend(draw(ui, model));
    });
    actions
}

#[test]
fn the_screen_has_the_list_the_default_the_table_and_xdebug_for_php() {
    let (_, tree) = start(&model());
    for label in [
        "Runtimes",
        "PHP",
        "Node.js",
        "Composer",
        "Caddy",
        "Default PHP",
        "Install another PHP version",
        "Install a version…",
        "More for PHP 8.4.23",
        "More for PHP 8.3.27",
        "More for PHP 8.2.28",
        "Xdebug",
    ] {
        assert!(
            tree.node_with_label(label)
                .or_else(|| tree.node_with_text(label))
                .is_some(),
            "{label}"
        );
    }
    // The list is options, one of them selected.
    let php = tree.node_with_label("PHP").expect("PHP");
    assert_eq!(php.role(), egui::Role::ListBoxOption);
    assert_eq!(php.is_selected(), Some(true));
    assert_eq!(
        tree.node_with_label("Node.js")
            .and_then(|n| n.is_selected()),
        Some(false)
    );
    // The card says what Xdebug does and what it costs.
    assert!(tree.node_with_text(strings::XDEBUG_CAPTION).is_some());
    assert!(strings::XDEBUG_CAPTION.contains("PHP runs two to four times slower while it is on."));
    // Node has no Xdebug.
    let (_, node) = start(&with_route(model(), Runtime::Node));
    assert!(node.node_with_label("Xdebug").is_none());
    assert!(node.node_with_label("Default Node.js").is_some());
}

#[test]
fn a_click_on_a_runtime_in_the_list_chooses_it() {
    let actions = click(&model(), "Node.js");
    assert_eq!(actions, [Action::Navigate(Route::Runtimes(Runtime::Node))]);
    // The one chosen already is not a navigation.
    assert!(click(&model(), "PHP").is_empty());
}

#[test]
fn a_large_button_sets_the_default_and_the_dashed_plus_and_the_button_open_the_dialog() {
    assert_eq!(
        click(&model(), "8.3"),
        [Action::SetDefault {
            runtime: Runtime::Php,
            version: v("8.3.27"),
        }]
    );
    let ask = [Action::AskInstall(Runtime::Php)];
    assert_eq!(click(&model(), "Install another PHP version"), ask);
    assert_eq!(click(&model(), "Install a version…"), ask);
}

#[test]
fn a_row_menu_asks_to_uninstall_and_a_version_that_cannot_go_has_its_row_off() {
    let model = model();
    assert_eq!(
        choose(&model, "More for PHP 8.2.28", strings::UNINSTALL_BUTTON),
        [Action::AskUninstall {
            runtime: Runtime::Php,
            version: v("8.2.28"),
        }]
    );
    // The default: the row is there, off, and a click does nothing.
    assert!(
        choose(&model, "More for PHP 8.4.23", strings::UNINSTALL_BUTTON).is_empty(),
        "the default stays"
    );
    let (harness, tree) = start(&model);
    harness.click(bounds_center(&tree, "More for PHP 8.4.23"), |ui| {
        draw(ui, &model);
    });
    let tree = harness.accessibility(|ui| {
        draw(ui, &model);
    });
    let row = tree
        .node_with_label(strings::UNINSTALL_BUTTON)
        .expect("the row stays");
    assert!(row.is_disabled());
}

#[test]
fn while_a_job_runs_the_plus_and_the_install_button_do_nothing_and_the_versions_still_pick() {
    let mut model = model();
    assert!(model.queue_install(job(Runtime::Caddy, None)));
    assert!(click(&model, "Install another PHP version").is_empty());
    assert!(click(&model, "Install a version…").is_empty());
    assert_eq!(
        click(&model, "8.3"),
        [Action::SetDefault {
            runtime: Runtime::Php,
            version: v("8.3.27"),
        }]
    );
    // Asked anyway (a stale click), the model does not open the dialog.
    model.apply_action(Action::AskInstall(Runtime::Php));
    assert_eq!(install_dialog_view(&model), None);
}

#[test]
fn with_nothing_installed_the_empty_state_has_the_button_that_installs() {
    let model = with_route(model(), Runtime::Caddy);
    let (_, tree) = start(&model);
    assert!(tree.node_with_text("No Caddy installed yet.").is_some());
    assert!(tree.node_with_label("Install a version…").is_none());
    assert_eq!(
        click(&model, "Install Caddy…"),
        [Action::AskInstall(Runtime::Caddy)]
    );
}

#[test]
fn the_xdebug_switch_toggles() {
    let mut model = model();
    model.update(Update::Xdebug(XdebugState {
        enabled: false,
        loaded: Vec::new(),
        missing: Vec::new(),
    }));
    assert_eq!(click(&model, "Xdebug"), [Action::ToggleXdebug]);
    // Not read yet: the switch is off and does nothing.
    assert!(click(&self::model(), "Xdebug").is_empty());
}

/// Frames enough for the dialog to size itself and fade in.
fn settled(model: &Model) -> (Harness, Tree) {
    let harness = Harness::new(egui::Theme::Light);
    for _ in 0..20 {
        harness.run(|ui| {
            draw(ui, model);
        });
    }
    let tree = harness.accessibility(|ui| {
        draw(ui, model);
    });
    (harness, tree)
}

fn asking(runtime: Runtime) -> Model {
    let mut model = model();
    model.apply_action(Action::AskInstall(runtime));
    model
}

#[test]
fn the_install_dialog_has_the_field_in_focus_and_install_and_cancel_answer() {
    let model = asking(Runtime::Php);
    let (harness, tree) = settled(&model);
    for label in ["Install a PHP version", "Version", "Cancel", "Install"] {
        assert!(tree.node_with_label(label).is_some(), "{label}");
    }
    let field = tree.node_with_label("Version").expect("the field");
    assert_eq!(field.role(), egui::Role::TextInput);
    let mut actions = Vec::new();
    harness.click(bounds_center(&tree, "Install"), |ui| {
        actions.extend(draw(ui, &model));
    });
    assert_eq!(actions, [Action::SubmitInstall(Runtime::Php)]);

    let (harness, tree) = settled(&model);
    let mut actions = Vec::new();
    harness.click(bounds_center(&tree, "Cancel"), |ui| {
        actions.extend(draw(ui, &model));
    });
    assert_eq!(actions, [Action::CancelInstall]);

    let (harness, _) = settled(&model);
    let mut actions = Vec::new();
    harness.key(egui::Key::Escape, |ui| actions.extend(draw(ui, &model)));
    assert_eq!(actions, [Action::CancelInstall]);
}

#[test]
fn typing_in_the_field_is_an_action_and_enter_installs() {
    let model = asking(Runtime::Node);
    let (harness, _) = settled(&model);
    let typed = egui::RawInput {
        events: vec![egui::Event::Text("22".into())],
        ..egui::RawInput::default()
    };
    let mut actions = Vec::new();
    harness.frame(typed, |ui| actions.extend(draw(ui, &model)));
    assert_eq!(
        actions,
        [Action::SetInstallVersion {
            runtime: Runtime::Node,
            text: "22".into(),
        }]
    );
    let mut actions = Vec::new();
    harness.key(egui::Key::Enter, |ui| actions.extend(draw(ui, &model)));
    assert_eq!(actions, [Action::SubmitInstall(Runtime::Node)]);
}

#[test]
fn a_wrong_version_shows_under_the_field() {
    let mut model = asking(Runtime::Php);
    model.apply_action(Action::SetInstallVersion {
        runtime: Runtime::Php,
        text: "latest".into(),
    });
    model.apply_action(Action::SubmitInstall(Runtime::Php));
    let (_, tree) = settled(&model);
    let error = strings::error_text("unsupported_alias");
    assert!(tree.node_with_text(&error).is_some(), "{error}");
}

#[test]
fn no_daemon_says_so_with_no_list() {
    let harness = Harness::new(egui::Theme::Light);
    let tree = harness.accessibility(|ui| {
        show(ui, &view(&Model::default()), &LIGHT);
    });
    assert!(tree.node_with_text(strings::NO_RUNTIMES).is_some());
    assert!(tree.node_with_label("PHP").is_none());
}
