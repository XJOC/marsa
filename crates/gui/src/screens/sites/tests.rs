use std::path::PathBuf;

use eframe::egui::{self, Event, Pos2, RawInput, pos2, vec2};
use marsa_core::runtimes::Runtime;
use marsa_core::types::{HostsStatus, RuntimeList, RuntimeVersions, SiteKind, SiteList};
use semver::Version;

use super::*;
use crate::fixtures::{abs, connected};
use crate::model::{Update, apply};
use crate::theme::LIGHT;
use crate::widgets::harness::{Harness, Tree};

fn v(text: &str) -> Version {
    text.parse().expect("a version")
}

/// `~` and the parts after it, with the separator of the system.
fn tilde(parts: &[&str]) -> String {
    let sep = std::path::MAIN_SEPARATOR;
    parts
        .iter()
        .fold(String::from("~"), |text, part| format!("{text}{sep}{part}"))
}

fn runtimes(php: &[&str]) -> RuntimeList {
    RuntimeList {
        runtimes: vec![RuntimeVersions {
            runtime: Runtime::Php,
            installed: php.iter().map(|text| (*text).to_owned()).collect(),
            default: Some("8.4".into()),
        }],
    }
}

/// PHP 8.3 and 8.4 installed, and these sites with PHP 8.4.27 from the
/// global default.
fn model(names: &[&str]) -> Model {
    let mut model = connected();
    model.update(Update::Runtimes(runtimes(&["8.3.27", "8.4.27"])));
    let sites = names
        .iter()
        .map(|name| site(name, Some("8.4.27"), Some(Source::GlobalDefault)))
        .collect();
    model.update(Update::Sites(SiteList {
        sites,
        ..SiteList::default()
    }));
    model
}

fn site(name: &str, php: Option<&str>, source: Option<Source>) -> Site {
    let root = PathBuf::from("/code").join(name);
    Site {
        name: name.into(),
        docroot: root.join("public"),
        root,
        kind: SiteKind::Laravel,
        php: php.map(v),
        php_source: source,
        secure: true,
    }
}

fn with_sites(model: &mut Model, sites: Vec<Site>) {
    model.update(Update::Sites(SiteList {
        sites,
        ..SiteList::default()
    }));
}

/// Every row of the table, built (a screen builds only the ones in view).
fn rows<'a>(view: &SitesView<'a>) -> Vec<SiteRow<'a>> {
    match &view.list {
        List::Rows(matches) => (0..matches.len())
            .map(|n| view.row(n).expect("a row"))
            .collect(),
        other => panic!("expected rows, got {other:?}"),
    }
}

/// The `n`th row.
fn row_of<'a>(view: &SitesView<'a>, n: usize) -> SiteRow<'a> {
    view.row(n).expect("a row")
}

fn names<'a>(view: &SitesView<'a>) -> Vec<&'a str> {
    rows(view).iter().map(|row| row.name).collect()
}

// -- view --

#[test]
fn the_search_filters_by_name_ignoring_case_and_the_spaces_around_it() {
    let mut model = model(&["shop", "billing-api", "legacy-crm", "wiki"]);
    assert_eq!(
        names(&view(&model, false)),
        ["billing-api", "legacy-crm", "shop", "wiki"]
    );
    for (search, want) in [
        ("sh", &["shop"][..]),
        ("SH", &["shop"]),
        ("  Api ", &["billing-api"]),
        ("l", &["billing-api", "legacy-crm"]),
        ("-", &["billing-api", "legacy-crm"]),
        ("", &["billing-api", "legacy-crm", "shop", "wiki"]),
        ("   ", &["billing-api", "legacy-crm", "shop", "wiki"]),
    ] {
        model.sites_search = search.to_owned();
        assert_eq!(names(&view(&model, false)), want, "{search:?}");
    }
    // The folder is not searched, only the name.
    model.sites_search = "code".into();
    assert!(matches!(view(&model, false).list, List::NoMatch("code")));
    // The text of the field is what the user typed, spaces and all.
    model.sites_search = "  Api ".into();
    assert_eq!(view(&model, false).search, "  Api ");
}

#[test]
fn a_search_that_matches_nothing_says_so_and_is_not_the_empty_state() {
    let mut model = model(&["shop"]);
    model.sites_search = " nope ".into();
    let view = view(&model, false);
    assert_eq!(view.list, List::NoMatch("nope"));
    assert_eq!(strings::no_site_matches("nope"), "No site matches “nope”.");
    // The same search over no sites at all is the empty state.
    let mut none = model;
    with_sites(&mut none, Vec::new());
    assert_eq!(super::view(&none, false).list, List::Empty);
}

#[test]
fn matching_works_on_bytes_and_survives_odd_input() {
    assert!(matches("shop", "OP"));
    assert!(matches("shop", ""));
    assert!(!matches("shop", "shops"));
    assert!(!matches("", "a"));
    // Not ASCII: no site name has it, so nothing matches.
    assert!(!matches("shop", "é"));
    assert!(matches("a-b", "-"));
}

#[test]
fn the_reason_of_each_source_is_shown_after_the_version() {
    let dir = PathBuf::from("/code/shop");
    let mut model = model(&[]);
    let cases = [
        (
            Some(Source::ProjectSetting),
            Some(Reason::Site),
            "set for this site",
        ),
        (
            Some(Source::VersionFile(dir.join(".php-version"))),
            Some(Reason::File(Path::new("/code/shop/.php-version"))),
            "from .php-version",
        ),
        (
            Some(Source::Constraint(dir.join("composer.json"))),
            Some(Reason::File(Path::new("/code/shop/composer.json"))),
            "from composer.json",
        ),
        (
            Some(Source::GlobalDefault),
            Some(Reason::Default),
            "default",
        ),
    ];
    for (source, reason, text) in cases {
        with_sites(&mut model, vec![site("shop", Some("8.4.27"), source)]);
        let view = view(&model, false);
        let cell = row_of(&view, 0).php;
        assert_eq!(cell.reason, reason, "{text}");
        assert_eq!(
            cell.reason.map(|reason| reason.text()).as_deref(),
            Some(text)
        );
        assert_eq!(cell.shown, "8.4");
    }
}

#[test]
fn a_site_with_no_php_says_why_and_a_daemon_with_no_reason_says_nothing() {
    let mut model = model(&[]);
    with_sites(&mut model, vec![site("shop", None, None)]);
    let view_ = view(&model, false);
    let cell = row_of(&view_, 0).php;
    assert_eq!(cell.shown, strings::NO_PHP);
    assert_eq!(cell.reason, Some(Reason::NoMatch));
    assert_eq!(
        cell.reason.map(|r| r.text()).as_deref(),
        Some("no installed version fits")
    );
    assert_eq!(cell.off, None);

    // No PHP at all: the menu is off, and says what to do.
    model.update(Update::Runtimes(runtimes(&[])));
    let view_ = view(&model, false);
    let cell = row_of(&view_, 0).php;
    assert_eq!(cell.reason, Some(Reason::NoPhpInstalled));
    assert_eq!(cell.off, Some(strings::NO_PHP_YET));
    assert_eq!(strings::REASON_NO_PHP, "no PHP installed");

    // A daemon from before `php_source`.
    with_sites(&mut model, vec![site("old", Some("8.4.27"), None)]);
    model.update(Update::Runtimes(runtimes(&["8.4.27"])));
    let view_ = view(&model, false);
    assert_eq!(rows(&view_)[0].php.reason, None);
}

#[test]
fn the_menu_checks_the_site_setting_or_the_site_files() {
    let mut model = model(&[]);
    with_sites(
        &mut model,
        vec![
            site("pinned", Some("8.3.27"), Some(Source::ProjectSetting)),
            site("files", Some("8.4.27"), Some(Source::GlobalDefault)),
        ],
    );
    let view_ = view(&model, false);
    let rows = rows(&view_);
    // Sorted by name: `files`, then `pinned`.
    assert_eq!(rows[0].name, "files");
    assert_eq!(rows[0].php.setting, Setting::SiteFiles);
    assert_eq!(rows[1].php.setting, Setting::Version(0));
    assert_eq!(rows[1].php.shown, "8.3");
    let labels: Vec<_> = view_.php.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["8.3", "8.4"]);
}

#[test]
fn a_version_just_picked_shows_until_the_daemon_answers() {
    let mut model = model(&["shop"]);
    model
        .choose_site_php("shop", Choice::Version(v("8.3.27")))
        .expect("a change");
    let view_ = view(&model, false);
    let cell = row_of(&view_, 0).php;
    assert_eq!(
        (cell.shown.as_str(), cell.reason),
        ("8.3", Some(Reason::Site))
    );
    assert_eq!(cell.setting, Setting::Version(0));

    // Clearing the setting of a pinned site: the files decide, version
    // unknown until the answer.
    let mut model = model_with_pin();
    model
        .choose_site_php("shop", Choice::Auto)
        .expect("a change");
    let view_ = view(&model, false);
    let cell = row_of(&view_, 0).php;
    assert_eq!(cell.reason, Some(Reason::Files));
    assert_eq!(cell.setting, Setting::SiteFiles);
    assert_eq!(strings::REASON_FILES, "from the site's files");
}

fn model_with_pin() -> Model {
    let mut model = model(&[]);
    with_sites(
        &mut model,
        vec![site("shop", Some("8.3.27"), Some(Source::ProjectSetting))],
    );
    model
}

#[test]
fn the_connection_comes_from_secure_and_the_row_carries_its_folder() {
    let mut model = model(&[]);
    let mut plain = site("docs", Some("8.4.27"), Some(Source::GlobalDefault));
    plain.secure = false;
    with_sites(
        &mut model,
        vec![
            plain,
            site("shop", Some("8.4.27"), Some(Source::GlobalDefault)),
        ],
    );
    let view_ = view(&model, false);
    let rows = rows(&view_);
    assert_eq!((rows[0].name, rows[0].https), ("docs", false));
    assert_eq!((rows[1].name, rows[1].https), ("shop", true));
    assert_eq!(rows[1].root, Path::new("/code/shop"));
}

#[test]
fn no_daemon_no_sites_and_no_match_each_have_their_own_body() {
    let model = Model::default();
    let offline = view(&model, false);
    assert_eq!(offline.list, List::Offline);
    assert_eq!(offline.add_off, Some(strings::ADD_SITE_OFFLINE));

    let no_sites = model_without_sites();
    let empty = view(&no_sites, false);
    assert_eq!(empty.list, List::Empty);
    assert_eq!(empty.add_off, None);
}

fn model_without_sites() -> Model {
    model(&[])
}

#[test]
fn add_site_waits_for_a_folder_dialog_that_is_open() {
    let model = model(&["shop"]);
    assert_eq!(view(&model, false).add_off, None);
    assert_eq!(view(&model, true).add_off, Some(strings::PICKING_HINT));
}

#[test]
fn while_a_removal_runs_the_screen_says_so_and_forget_waits() {
    let mut model = model(&["shop", "wiki"]);
    model.session.sites.parked = vec![PathBuf::from("/code")];
    let view_ = view(&model, false);
    assert_eq!(view_.removing, None);
    assert_eq!(view_.forget_off, None);
    assert!(model.ask_removal(Removal::Unlink {
        site: "shop".into()
    }));
    model.confirm_removal().expect("sent");
    let view_ = view(&model, false);
    assert_eq!(view_.removing.as_deref(), Some("Unlinking shop.test…"));
    assert_eq!(view_.forget_off, Some(strings::REMOVE_BUSY_HINT));
}

#[test]
fn the_hosts_line_shows_only_with_names_waiting_or_a_failure() {
    let mut model = model(&["shop"]);
    assert!(view(&model, false).hosts.is_none());
    model.update(Update::Hosts(HostsStatus {
        used: true,
        missing: vec!["shop.test".into()],
    }));
    let hosts = view(&model, false).hosts.expect("a line");
    assert_eq!(
        hosts.waiting,
        Some(strings::hosts_waiting(&["shop.test".into()]))
    );
    assert_eq!(hosts.off, None);
    assert!(model.start_hosts_sync());
    assert_eq!(
        view(&model, false).hosts.expect("a line").off,
        Some(strings::HOSTS_ADDING)
    );
    // Names gone in, but the last try failed: the error stays.
    model.update(Update::Hosts(HostsStatus {
        used: true,
        missing: Vec::new(),
    }));
    model.session.hosts_sync = HostsSync::Failed("hosts_failed".into());
    let hosts = view(&model, false).hosts.expect("a line");
    assert_eq!(hosts.waiting, None);
    assert_eq!(hosts.failed, Some(strings::error_text("hosts_failed")));
}

#[test]
fn the_link_form_shows_the_folder_the_name_and_the_site_it_would_replace() {
    let mut model = model(&["shop"]);
    assert!(view(&model, false).link.is_none());
    model.pick_link_folder(&abs("/work/Shop"));
    let view_ = view(&model, false);
    let link = view_.link.as_ref().expect("the form");
    assert_eq!(
        (link.folder, link.name),
        (abs("/work/Shop").as_path(), "shop")
    );
    assert_eq!(link.replaces.map(|site| site.name.as_str()), Some("shop"));
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

fn draw(ui: &mut Ui, model: &Model) -> Vec<Action> {
    show(ui, &view(model, false), &LIGHT)
}

/// A harness that has drawn `model` once, and its accessibility tree.
fn start(model: &Model) -> (Harness, Tree) {
    let harness = Harness::new(egui::Theme::Light);
    let tree = harness.accessibility(|ui| {
        draw(ui, model);
    });
    (harness, tree)
}

/// One click on the widget named `label`; what Sites returned.
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
    assert!(
        egui::Popup::is_any_open(&harness.ctx),
        "{button} opens a menu"
    );
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
fn open_opens_the_site_and_a_click_on_the_row_opens_its_page() {
    let model = model(&["shop"]);
    assert_eq!(
        click(&model, "Open"),
        [Action::OpenSite("shop".into())],
        "the button is not also a row click"
    );
    // The name and the folder are one cell, named by both. The folder is
    // written the system's way (`\` on Windows).
    let label = format!("shop, {}", Path::new("/code").join("shop").display());
    assert_eq!(
        click(&model, &label),
        [Action::Navigate(Route::Site("shop".into()))]
    );
}

#[test]
fn add_site_opens_a_menu_with_park_and_link() {
    let model = model(&["shop"]);
    assert_eq!(
        choose(&model, strings::ADD_SITE_BUTTON, strings::PARK_BUTTON),
        [Action::PickFolder(Pick::Park)]
    );
    assert_eq!(
        choose(&model, strings::ADD_SITE_BUTTON, strings::LINK_BUTTON),
        [Action::PickFolder(Pick::Link)]
    );
}

#[test]
fn the_empty_state_is_one_sentence_and_the_same_button_with_the_same_menu() {
    let model = model_without_sites();
    let (_, tree) = start(&model);
    assert!(tree.node_with_text(strings::SITES_EMPTY).is_some());
    // One Add site button, not two: the header keeps the title only.
    assert_eq!(
        choose(&model, strings::ADD_SITE_BUTTON, strings::LINK_BUTTON),
        [Action::PickFolder(Pick::Link)]
    );
    assert!(tree.node_with_label(strings::FIND_SITE).is_none());
}

#[test]
fn add_site_does_nothing_while_a_dialog_is_open_or_with_no_daemon() {
    let model = model(&["shop"]);
    let harness = Harness::new(egui::Theme::Light);
    let tree = harness.accessibility(|ui| {
        show(ui, &view(&model, true), &LIGHT);
    });
    harness.click(bounds_center(&tree, strings::ADD_SITE_BUTTON), |ui| {
        show(ui, &view(&model, true), &LIGHT);
    });
    assert!(!egui::Popup::is_any_open(&harness.ctx));
}

#[test]
fn the_version_menu_sets_a_version_or_clears_the_site_setting() {
    let model = model(&["shop"]);
    let button = "PHP version for shop, 8.4";
    let set = Action::SitePhp {
        site: "shop".into(),
        choice: Choice::Version(v("8.3.27")),
    };
    assert_eq!(choose(&model, button, "8.3"), [set]);

    let model = model_with_pin();
    let clear = Action::SitePhp {
        site: "shop".into(),
        choice: Choice::Auto,
    };
    assert_eq!(
        choose(&model, "PHP version for shop, 8.3", strings::USE_SITE_FILES),
        [clear]
    );
}

#[test]
fn the_reason_is_in_the_tree_beside_the_version() {
    let (_, tree) = start(&model(&["shop"]));
    assert!(tree.node_with_text("default").is_some());
}

#[test]
fn a_row_has_no_menu_any_more_the_page_of_the_site_has_what_it_had() {
    let (_, tree) = start(&model(&["alpha", "shop"]));
    for site in ["alpha", "shop"] {
        assert!(
            tree.node_with_label(&format!("More actions for {site}"))
                .is_none()
        );
    }
    // The other cells of the row stay: the version menu and Open.
    assert!(tree.node_with_label("PHP version for shop, 8.4").is_some());
    assert_eq!(tree.nodes_with_label(strings::OPEN).len(), 2);
}

#[test]
fn the_open_button_ends_the_row_at_the_card_padding() {
    let model = model(&["shop"]);
    let harness = Harness::new(egui::Theme::Light);
    let mut right = 0.0;
    let tree = harness.accessibility(|ui| {
        right = ui.max_rect().right();
        draw(ui, &model);
    });
    let open = tree.node_with_label(strings::OPEN).expect("Open");
    let edge = f64::from(right - LIGHT.space.s4);
    let gap = (open.bounds().expect("a place").x1 - edge).abs();
    assert!(gap < 1.5, "{gap}");
}

#[test]
fn typing_in_the_search_field_returns_the_text_and_cmd_k_focuses_it() {
    let model = model(&["shop", "wiki"]);
    let harness = Harness::new(egui::Theme::Light);
    harness.run(|ui| {
        draw(ui, &model);
    });
    assert!(!harness.ctx.memory(|memory| memory.has_focus(search_id())));
    harness.key_with(Key::K, Modifiers::COMMAND, |ui| {
        draw(ui, &model);
    });
    assert!(harness.ctx.memory(|memory| memory.has_focus(search_id())));

    let input = RawInput {
        events: vec![Event::Text("wi".into())],
        ..RawInput::default()
    };
    let mut actions = Vec::new();
    harness.frame(input, |ui| actions.extend(draw(ui, &model)));
    assert_eq!(actions, [Action::SearchSites("wi".into())]);
}

#[test]
fn the_search_field_is_a_named_search_input_with_the_key_hint() {
    let (_, tree) = start(&model(&["shop"]));
    let field = tree.node_with_label(strings::FIND_SITE).expect("the field");
    assert_eq!(field.role(), egui::Role::SearchInput);
}

#[test]
fn only_the_rows_in_view_are_drawn() {
    let names: Vec<String> = (0..2000).map(|i| format!("site-{i:04}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let model = model(&refs);
    let harness = Harness::new(egui::Theme::Light);
    let screen = egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 600.0));
    let input = RawInput {
        screen_rect: Some(screen),
        ..RawInput::default()
    };
    let mut count = 0;
    let (mut after_view, mut after_frame) = (0, 0);
    BUILT.with(|built| built.set(0));
    let tree = harness.frame(input, |ui| {
        let shown = view(&model, false);
        count = match &shown.list {
            List::Rows(rows) => rows.len(),
            _ => 0,
        };
        after_view = BUILT.with(std::cell::Cell::get);
        show(ui, &shown, &LIGHT);
        after_frame = BUILT.with(std::cell::Cell::get);
    });
    assert_eq!(count, 2000);
    // The view holds the 2000 matches and builds no row; the table builds
    // the rows in view, and only those (R16).
    assert_eq!(after_view, 0, "the view builds no row");
    assert!(
        (1..=20).contains(&after_frame),
        "{after_frame} rows were built for 2000 sites"
    );
    let drawn = (0..2000)
        .filter(|i| {
            tree.node_with_label(&format!("PHP version for site-{i:04}, 8.4"))
                .is_some()
        })
        .count();
    assert!((1..=20).contains(&drawn), "{drawn} rows were drawn");
}

#[test]
fn the_link_form_returns_its_edits_and_sends_with_link_or_enter() {
    let mut model = model(&[]);
    model.pick_link_folder(&abs("/work/blog"));
    assert_eq!(click(&model, strings::LINK_CONFIRM), [Action::SubmitLink]);
    assert_eq!(click(&model, strings::CANCEL), [Action::CancelLink]);

    // Typing in the name field.
    let harness = Harness::new(egui::Theme::Light);
    let tree = harness.accessibility(|ui| {
        draw(ui, &model);
    });
    harness.click(bounds_center(&tree, strings::LINK_NAME), |ui| {
        draw(ui, &model);
    });
    let input = RawInput {
        events: vec![Event::Text("x".into())],
        ..RawInput::default()
    };
    let mut actions = Vec::new();
    harness.frame(input, |ui| actions.extend(draw(ui, &model)));
    assert_eq!(actions, [Action::SetLinkName("blogx".into())]);
    // Enter in the field sends the link.
    let mut actions = Vec::new();
    harness.key(Key::Enter, |ui| actions.extend(draw(ui, &model)));
    assert_eq!(actions, [Action::SubmitLink]);
}

#[test]
fn the_hosts_button_returns_sync_hosts_and_is_off_while_it_waits() {
    let mut model = model(&["shop"]);
    model.update(Update::Hosts(HostsStatus {
        used: true,
        missing: vec!["shop.test".into()],
    }));
    assert_eq!(click(&model, strings::ADD_TO_HOSTS), [Action::SyncHosts]);
    assert!(model.start_hosts_sync());
    assert!(click(&model, strings::ADD_TO_HOSTS).is_empty());
}

#[test]
fn the_parked_folders_forget_and_the_skipped_ones_say_why() {
    let mut model = model(&["shop"]);
    model.session.sites.parked = vec![PathBuf::from("/code"), PathBuf::from("/work")];
    model.session.sites.skipped = vec![SkippedFolder {
        path: PathBuf::from("/code/My Site"),
        code: "site_name_invalid".into(),
    }];
    // Either row's button asks about its own folder.
    let asked = click(&model, strings::FORGET_BUTTON);
    assert!(
        asked == [Action::AskForget(PathBuf::from("/code"))]
            || asked == [Action::AskForget(PathBuf::from("/work"))],
        "{asked:?}"
    );
    let (_, tree) = start(&model);
    assert!(tree.node_with_text("/code/My Site").is_some());
    assert!(
        tree.node_with_text(&strings::skip_reason("site_name_invalid"))
            .is_some()
    );
    // While a removal runs, no folder can be forgotten.
    assert!(model.ask_removal(Removal::Unlink {
        site: "shop".into()
    }));
    model.confirm_removal().expect("sent");
    assert!(click(&model, strings::FORGET_BUTTON).is_empty());
}

#[test]
fn the_folders_under_home_show_with_a_tilde_and_a_screen_reader_hears_the_full_path() {
    let home = abs("/Users/omar");
    let mut model = model(&[]);
    let places = [
        ("docs", abs("/Users/omarx/Code/docs")),
        ("shop", home.join("Code").join("shop")),
        ("wiki", abs("/srv/www/wiki")),
    ];
    let sites = places
        .iter()
        .map(|(name, root)| {
            let mut site = site(name, Some("8.4.27"), Some(Source::GlobalDefault));
            site.root = root.clone();
            site
        })
        .collect();
    with_sites(&mut model, sites);
    model.home = Some(home.clone());
    assert_eq!(view(&model, false).home, Some(home.as_path()));

    let harness = Harness::new(egui::Theme::Light);
    let mut painted = Vec::new();
    let shapes = harness.shapes(|ui| {
        draw(ui, &model);
    });
    for shape in shapes {
        if let egui::Shape::Text(text) = shape {
            painted.push(text.galley.text().to_owned());
        }
    }
    // Under home: shortened. Beside home, and elsewhere: in full.
    assert!(painted.contains(&tilde(&["Code", "shop"])), "{painted:?}");
    assert!(
        painted.contains(&places[0].1.display().to_string()),
        "{painted:?}"
    );
    assert!(
        painted.contains(&places[2].1.display().to_string()),
        "{painted:?}"
    );
    assert!(
        !painted
            .iter()
            .any(|text| text.contains("Users") && text.starts_with('~')),
        "{painted:?}"
    );
    // The tree has the whole path, so the tilde is never all there is.
    let (_, tree) = start(&model);
    let label = format!("shop, {}", places[1].1.display());
    assert!(tree.node_with_text(&label).is_some(), "{label}");
}

/// The texts one frame of Sites paints.
fn painted(model: &Model) -> Vec<String> {
    let harness = Harness::new(egui::Theme::Light);
    let shapes = harness.shapes(|ui| {
        draw(ui, model);
    });
    shapes
        .into_iter()
        .filter_map(|shape| match shape {
            egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
            _ => None,
        })
        .collect()
}

#[test]
fn the_link_form_names_the_folder_with_a_tilde_and_a_screen_reader_hears_the_whole_path() {
    let home = abs("/Users/omar");
    let folder = home.join("Work").join("blog");
    let mut model = model(&[]);
    model.home = Some(home.clone());
    model.pick_link_folder(&folder);
    let shown = strings::link_folder(&strings::home_path(&folder, Some(&home)));
    assert!(shown.starts_with("Link ~"), "{shown}");
    assert_eq!(shown, format!("Link {} as:", tilde(&["Work", "blog"])));
    let whole = strings::link_folder(&folder.display().to_string());
    let painted = painted(&model);
    assert!(painted.contains(&shown), "{painted:?}");
    assert!(!painted.contains(&whole), "{painted:?}");
    let (_, tree) = start(&model);
    assert!(tree.node_with_text(&whole).is_some(), "{whole}");

    // A folder outside home, or no home: shown in full.
    let elsewhere = abs("/srv/blog");
    model.pick_link_folder(&elsewhere);
    let want = strings::link_folder(&elsewhere.display().to_string());
    assert!(painted_has(&model, &want));
    model.home = None;
    model.pick_link_folder(&folder);
    assert!(painted_has(&model, &whole));
}

fn painted_has(model: &Model, text: &str) -> bool {
    painted(model).iter().any(|painted| painted == text)
}

#[test]
fn the_parked_and_skipped_lists_show_the_home_folder_as_a_tilde_too() {
    let home = abs("/Users/omar");
    let mut model = model(&["shop"]);
    model.home = Some(home.clone());
    model.session.sites.parked = vec![home.join("Code"), abs("/srv/www")];
    model.session.sites.skipped = vec![SkippedFolder {
        path: home.join("Code").join("My Site"),
        code: "site_name_invalid".into(),
    }];
    let (_, tree) = start(&model);
    assert!(tree.node_with_text(&tilde(&["Code"])).is_some());
    assert!(tree.node_with_text(&tilde(&["Code", "My Site"])).is_some());
    assert!(
        tree.node_with_text(&abs("/srv/www").display().to_string())
            .is_some()
    );
    // With no home, the same folders are shown in full.
    model.home = None;
    let (_, tree) = start(&model);
    let full = home.join("Code").display().to_string();
    assert!(tree.node_with_text(&full).is_some(), "{full}");
    assert!(tree.node_with_text(&tilde(&["Code"])).is_none());
}

#[test]
fn every_cell_of_a_row_is_centered_on_the_middle_of_the_row() {
    let model = model(&["shop"]);
    let (_, tree) = start(&model);
    let middle = |node: &eframe::egui::accesskit::Node| {
        let bounds = node.bounds().expect("a place");
        (bounds.y0 + bounds.y1) / 2.0
    };
    let by_label = |label: &str| middle(tree.node_with_label(label).expect(label));
    let open = by_label("Open");
    assert!((by_label("PHP version for shop, 8.4") - open).abs() < 0.5);
    // The reason and the lock and word of the connection share it too.
    let reason = middle(tree.node_with_text("default").expect("the reason"));
    assert!((reason - open).abs() < 1.0, "{reason} {open}");
    let word = middle(tree.node_with_text(strings::HTTPS).expect("the word"));
    assert!((word - open).abs() < 1.0, "{word} {open}");
}

#[test]
fn the_forget_buttons_are_30_high_and_sit_inside_rows_of_at_least_46() {
    let mut model = model(&["shop"]);
    model.session.sites.parked = vec![PathBuf::from("/code"), PathBuf::from("/work")];
    let (_, tree) = start(&model);
    let mut buttons: Vec<_> = tree
        .nodes_with_label(strings::FORGET_BUTTON)
        .into_iter()
        .map(|node| node.bounds().expect("a place"))
        .collect();
    assert_eq!(buttons.len(), 2);
    buttons.sort_by(|a, b| a.y0.total_cmp(&b.y0));
    let height = buttons[0].y1 - buttons[0].y0;
    assert_eq!(height, f64::from(LIGHT.size.control_in_row));
    // One row down is one row's height.
    let row = buttons[1].y0 - buttons[0].y0;
    assert!(row >= 46.0, "{row}");
    assert_eq!(row, f64::from(RowHeight::OneLine.points(&LIGHT)));
    // The divider of a row is 1 px: the button clears it on both sides.
    assert!(
        row - height >= 2.0 * f64::from(LIGHT.size.border),
        "{row} {height}"
    );
}

#[test]
fn link_is_the_primary_button_of_the_form_and_cancel_is_not() {
    let accent = |model: &Model| {
        let harness = Harness::new(egui::Theme::Light);
        let shapes = harness.shapes(|ui| {
            draw(ui, model);
        });
        shapes
            .iter()
            .filter(
                |shape| matches!(shape, egui::Shape::Rect(rect) if rect.fill == LIGHT.color.accent),
            )
            .count()
    };
    let mut model = model(&["shop"]);
    // "Add site…" is the only accent button.
    assert_eq!(accent(&model), 1);
    model.pick_link_folder(&abs("/work/blog"));
    // Link is the second; Cancel stays secondary (a `surface` fill).
    assert_eq!(accent(&model), 2);
}

#[test]
fn a_site_row_is_as_high_as_a_two_line_row() {
    let model = model(&["alpha", "shop"]);
    let (_, tree) = start(&model);
    let top = |label: &str| {
        let node = tree.node_with_label(label).expect(label);
        node.bounds().expect("a place").y0
    };
    let step = top("PHP version for shop, 8.4") - top("PHP version for alpha, 8.4");
    assert!((step - f64::from(LIGHT.size.row_max)).abs() < 0.5, "{step}");
}

#[test]
fn the_screen_after_a_site_came_in_through_the_daemon_events() {
    // The table follows `sites_changed`: a new site shows on the next view.
    let mut model = model(&["shop"]);
    apply(
        &mut model,
        marsa_core::types::Event::SitesChanged(SiteList {
            sites: vec![
                site("blog", Some("8.4.27"), Some(Source::GlobalDefault)),
                site("shop", Some("8.4.27"), Some(Source::GlobalDefault)),
            ],
            ..SiteList::default()
        }),
    );
    assert_eq!(names(&view(&model, false)), ["blog", "shop"]);
}
