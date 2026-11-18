//! A site's own page (layout.md, Screens: Site page): the name and its
//! address with "Open folder" and "Open in browser", the site's PHP on the
//! large version buttons with the reason, the facts (the HTTPS switch, the
//! folder, the kind), and "Unlink this site…" at the end.
//!
//! It is two functions, like Home and Sites. [`view`] reads the [`Model`]
//! and decides what the page says: pure, so the tests assert it without a
//! window. [`show`] draws that [`SiteView`] with the widgets and returns what
//! the user did as [`Action`]s (code.md, State). [Laravel: `view` is the
//! controller that builds the data; `show` is the Blade template.]
//!
//! There is no tab bar yet: with one part (the overview) a bar would look like
//! navigation that goes nowhere. It comes back with the second part, drawn by
//! [`crate::widgets::Tabs`], when Mail, Dumps, Database, or Logs ship
//! (rules.md 8). The window has one Caddy log, not a log per site, so there is
//! no Logs part yet.

use std::path::Path;

use eframe::egui::{CursorIcon, Label, RichText, Sense, Ui, vec2};
use marsa_core::runtimes::Runtime;
use marsa_core::types::Site;

use super::sites::{self, NoticeLine, Reason};
use crate::model::{Action, Choice, Model, Route};
use crate::screens::home::{PhpChoice, php_choices};
use crate::strings;
use crate::theme::page::SITE_FACT_LABEL;
use crate::theme::{Direction, Theme};
use crate::widgets::{Button, Icon, QuietTone, Switch, VersionPick, VersionPicker, fact_card};

/// What the page says.
#[derive(Debug, PartialEq)]
pub enum SiteView<'a> {
    /// No site has this name: it was unlinked meanwhile (from the CLI, or
    /// from another window), or the window has no daemon to ask.
    Missing(Missing<'a>),
    /// Boxed: a page is much larger than a message.
    Found(Box<Found<'a>>),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Missing<'a> {
    pub name: &'a str,
    /// The window has no daemon: the list is empty because it is not asked.
    pub offline: bool,
}

/// The page of a site that is in the list.
#[derive(Debug, PartialEq)]
pub struct Found<'a> {
    pub name: &'a str,
    /// What the browser opens: `https://shop.test`.
    pub url: String,
    /// The line about the last sites action: a failed change shows here, as
    /// it shows on Sites.
    pub line: Option<NoticeLine>,
    /// "Unlinking shop.test…", while the daemon removes it.
    pub removing: Option<String>,
    pub php: PhpView<'a>,
    pub facts: Facts<'a>,
    pub unlink: Unlink<'a>,
    /// Why "Forget…" of the parked folder cannot be pressed now.
    pub forget_off: Option<&'static str>,
    /// The user's home folder: a folder under it is shown as `~/…`, with
    /// the full path in a tooltip. Display only.
    pub home: Option<&'a Path>,
}

/// The large version picker of the site.
#[derive(Debug, PartialEq)]
pub struct PhpView<'a> {
    /// The installed versions, oldest first.
    pub choices: Vec<PhpChoice>,
    /// The index of the version in force, if it is installed.
    pub chosen: Option<usize>,
    pub reason: Option<Reason<'a>>,
    /// The sentence after the heading: which version, and why.
    pub hint: Option<String>,
    /// The site has a setting of its own: "Use the site's files" shows.
    pub own: bool,
    /// Why the dashed "+" cannot open the install dialog now.
    pub install_off: Option<&'static str>,
}

#[derive(Debug, PartialEq)]
pub struct Facts<'a> {
    /// Served over HTTPS.
    pub https: bool,
    pub root: &'a Path,
    /// Where the web server starts, when it is not the project folder
    /// (`public` for Laravel).
    pub docroot: Option<&'a Path>,
    pub kind: String,
    pub added_as: AddedAs<'a>,
}

/// How the site came to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddedAs<'a> {
    /// A sub-folder of this parked folder.
    Parked(&'a Path),
    Linked,
}

/// Whether the page can unlink the site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unlink<'a> {
    Allowed,
    /// Another removal runs.
    Busy,
    /// The site comes from this parked folder: forget the folder instead.
    Parked(&'a Path),
}

/// Decides what the page of the site `name` says. Pure: the same model gives
/// the same view.
pub fn view<'a>(model: &'a Model, name: &'a str) -> SiteView<'a> {
    let Some(site) = model.sites.sites.iter().find(|site| site.name == name) else {
        return SiteView::Missing(Missing {
            name,
            offline: !model.is_connected(),
        });
    };
    SiteView::Found(Box::new(Found {
        name: &site.name,
        url: crate::model::url(site),
        line: sites::line(model),
        removing: sites::removing_line(model),
        php: php(model, site),
        facts: facts(model, site),
        unlink: unlink_of(model, site),
        forget_off: (model.is_connected() && !model.can_remove())
            .then_some(strings::REMOVE_BUSY_HINT),
        home: model.home.as_deref(),
    }))
}

/// Whether `site` can be unlinked now: not when it comes from a parked
/// folder (the daemon refuses it too), and not while another removal runs.
fn unlink_of<'a>(model: &'a Model, site: &Site) -> Unlink<'a> {
    match model.parked_folder(site) {
        Some(folder) => Unlink::Parked(folder),
        None if model.can_remove() => Unlink::Allowed,
        None => Unlink::Busy,
    }
}

fn php<'a>(model: &Model, site: &'a Site) -> PhpView<'a> {
    let choices = php_choices(model);
    let cell = sites::php_cell(model, site, &choices);
    PhpView {
        chosen: cell.at,
        hint: cell.reason.map(|reason| reason.hint(&cell.shown)),
        reason: cell.reason,
        own: matches!(model.site_choice(site), Choice::Version(_)),
        install_off: (!model.can_install()).then_some(strings::INSTALL_BUSY_HINT),
        choices,
    }
}

fn facts<'a>(model: &'a Model, site: &'a Site) -> Facts<'a> {
    Facts {
        https: site.secure,
        root: &site.root,
        docroot: (site.docroot != site.root).then_some(site.docroot.as_path()),
        kind: strings::kind_text(site.kind),
        added_as: model
            .parked_folder(site)
            .map_or(AddedAs::Linked, AddedAs::Parked),
    }
}

/// Draws `view`. Returns what the user did.
pub fn show(ui: &mut Ui, view: &SiteView<'_>, theme: &Theme) -> Vec<Action> {
    let mut actions = Vec::new();
    ui.spacing_mut().item_spacing.y = theme.space.section_gap;
    // The way back: an arrow and "Sites", as the mockup has it.
    if ui
        .add(Button::quiet(strings::SITES).leading(Icon::Back))
        .clicked()
    {
        actions.push(Action::Navigate(Route::Sites));
    }
    match view {
        SiteView::Missing(missing) => {
            let text = if missing.offline {
                strings::SITE_OFFLINE.to_owned()
            } else {
                strings::site_missing(missing.name)
            };
            sites::message(ui, theme, &text);
        }
        SiteView::Found(page) => found(ui, theme, page, &mut actions),
    }
    actions
}

fn found(ui: &mut Ui, theme: &Theme, page: &Found<'_>, actions: &mut Vec<Action>) {
    header(ui, theme, page, actions);
    if let Some(line) = &page.line {
        sites::notice(ui, theme, line, actions);
    }
    if let Some(text) = &page.removing {
        sites::mute(ui, theme, text);
    }
    php_section(ui, theme, page, actions);
    facts_card(ui, theme, page, actions);
    unlink(ui, theme, page, actions);
}

/// The name and the address, and the two ways to open the site at the end.
fn header(ui: &mut Ui, theme: &Theme, page: &Found<'_>, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme.space.s3;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = theme.space.s1;
            let name = RichText::new(page.name)
                .font(theme.text.display.font_id())
                .color(theme.color.ink);
            ui.add(Label::new(name).selectable(false));
            // The address is a link for the pointer; the keyboard has "Open
            // in browser" (`Sense::CLICK` takes no Tab stop).
            let url = RichText::new(&page.url)
                .font(theme.text.body.font_id())
                .color(theme.color.accent);
            let link = ui.add(Label::new(url).selectable(false).sense(Sense::CLICK));
            if link.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                actions.push(Action::OpenSite(page.name.to_owned()));
            }
        });
        ui.with_layout(
            Direction::CURRENT
                .row_from_end()
                .with_cross_align(eframe::egui::Align::Max),
            |ui| {
                // From the end: the primary button, then the one before it.
                if ui.add(Button::primary(strings::OPEN_IN_BROWSER)).clicked() {
                    actions.push(Action::OpenSite(page.name.to_owned()));
                }
                if ui.add(Button::secondary(strings::OPEN_FOLDER)).clicked() {
                    actions.push(Action::OpenFolder(page.name.to_owned()));
                }
            },
        );
    });
}

/// "PHP for this site": the large version buttons, the reason after the
/// heading, and after the reason the way back to the site's files.
fn php_section(ui: &mut Ui, theme: &Theme, page: &Found<'_>, actions: &mut Vec<Action>) {
    let php = &page.php;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = theme.space.group_gap_max;
        // The heading, the reason, and the link share one line as high as a
        // control, whether the link is there or not. The link shows only
        // with a setting of the site's own, whose reason is short, so the
        // line fits the narrowest window.
        let line = vec2(ui.available_width(), theme.size.control);
        ui.allocate_ui_with_layout(line, Direction::CURRENT.row(), |ui| {
            ui.spacing_mut().item_spacing.x = theme.space.s1;
            super::heading(ui, theme, strings::PHP_FOR_THIS_SITE, php.hint.as_deref());
            if php.own {
                // "8.3 is set for this site · Use the site's files".
                // A middle dot is small: the heavier face and `ink2` keep it
                // seen (it is decoration, the link names itself).
                // The link has padding of its own after the dot, so the dot
                // gets some before it.
                ui.add_space(theme.space.s2);
                let dot = RichText::new(strings::HINT_SEPARATOR)
                    .font(theme.text.body_strong.font_id())
                    .color(theme.color.ink2);
                ui.add(Label::new(dot).selectable(false));
                if ui.add(Button::quiet(strings::USE_SITE_FILES)).clicked() {
                    actions.push(Action::SitePhp {
                        site: page.name.to_owned(),
                        choice: Choice::Auto,
                    });
                }
            }
        });
        let versions: Vec<(usize, &str)> = php
            .choices
            .iter()
            .enumerate()
            .map(|(index, choice)| (index, choice.label.as_str()))
            .collect();
        let install = strings::install_another_label(strings::runtime_name(Runtime::Php));
        let mut picker =
            VersionPicker::new(strings::PHP_FOR_THIS_SITE, &versions, php.chosen).install(&install);
        if let Some(reason) = php.install_off {
            picker = picker.install_disabled(reason);
        }
        match picker.show(ui).inner {
            Some(VersionPick::Version(index)) => {
                if let Some(choice) = php.choices.get(index) {
                    actions.push(Action::SitePhp {
                        site: page.name.to_owned(),
                        choice: Choice::Version(choice.version.clone()),
                    });
                }
            }
            Some(VersionPick::Install) => actions.push(Action::AskInstall(Runtime::Php)),
            None => {}
        }
    });
}

/// The facts: the HTTPS switch, the folder, the kind, and where the site
/// came from.
fn facts_card(ui: &mut Ui, theme: &Theme, page: &Found<'_>, actions: &mut Vec<Action>) {
    let facts = &page.facts;
    fact_card(ui, SITE_FACT_LABEL, |rows| {
        rows.row(strings::FACT_CONNECTION, |ui| {
            let switch = Switch::new(facts.https, strings::SERVE_OVER_HTTPS);
            if ui.add(switch).clicked() {
                actions.push(Action::SetSecure {
                    site: page.name.to_owned(),
                    secure: !facts.https,
                });
            }
        });
        rows.row(strings::FACT_FOLDER, |ui| {
            sites::path(ui, theme, facts.root, page.home);
        });
        if let Some(docroot) = facts.docroot {
            rows.row(strings::FACT_DOCROOT, |ui| {
                sites::path(ui, theme, docroot, page.home);
            });
        }
        rows.row(strings::FACT_KIND, |ui| {
            text(ui, theme, &facts.kind);
        });
        rows.row(strings::FACT_ADDED_AS, |ui| match facts.added_as {
            AddedAs::Parked(folder) => {
                let shown = strings::home_path(folder, page.home);
                let response = text(ui, theme, &strings::added_as_parked(&shown));
                // Shortened for the eye: the whole path is one hover away.
                response.on_hover_text(folder.display().to_string());
            }
            AddedAs::Linked => {
                text(ui, theme, strings::ADDED_AS_LINKED);
            }
        });
    });
}

fn text(ui: &mut Ui, theme: &Theme, text: &str) -> eframe::egui::Response {
    let text = RichText::new(text)
        .font(theme.text.body.font_id())
        .color(theme.color.ink);
    ui.add(Label::new(text).selectable(false))
}

/// "Unlink this site…", a quiet button at the end that opens the
/// confirmation. A site from a parked folder goes with its folder only: the
/// button is off, says why, and the folder's own Forget is offered.
fn unlink(ui: &mut Ui, theme: &Theme, page: &Found<'_>, actions: &mut Vec<Action>) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = theme.space.s2;
        let parked = match page.unlink {
            Unlink::Parked(folder) => Some(folder),
            Unlink::Allowed | Unlink::Busy => None,
        };
        let why = parked.map(strings::unlink_parked_hint);
        let off = match page.unlink {
            Unlink::Allowed => None,
            Unlink::Busy => Some(strings::REMOVE_BUSY_HINT),
            Unlink::Parked(_) => why.as_deref(),
        };
        let button = Button::quiet(strings::UNLINK_THIS_SITE)
            .tone(QuietTone::Error)
            .disabled_if(off.is_some(), off.unwrap_or_default());
        let response = ui.add(button);
        if response.clicked() {
            actions.push(Action::AskUnlink(page.name.to_owned()));
        }
        if off.is_none() {
            response.on_hover_text(strings::UNLINK_HINT);
        }
        let Some(folder) = parked else {
            return;
        };
        let shown = strings::home_path(folder, page.home);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = theme.space.s3;
            // The words line up with the text of the button above, which has
            // its own padding.
            ui.add_space(theme.space.s3);
            sites::mute(ui, theme, &strings::site_from_parked(&shown));
            let label = strings::forget_folder_shown(&shown);
            let button = Button::secondary(&label).disabled_if(
                page.forget_off.is_some(),
                page.forget_off.unwrap_or_default(),
            );
            let response = ui.add(button);
            if response.clicked() {
                actions.push(Action::AskForget(folder.to_path_buf()));
            }
            if page.forget_off.is_none() {
                response.on_hover_text(folder.display().to_string());
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use eframe::egui::{self, Pos2, pos2};
    use marsa_core::types::{
        Health as DaemonHealth, RuntimeList, RuntimeVersions, SiteKind, SiteList, Source,
    };
    use semver::Version;

    use super::*;
    use crate::bridge::Command;
    use crate::model::{Change, Removal, SitesNotice, Update};
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

    fn v(text: &str) -> Version {
        text.parse().expect("a version")
    }

    /// An absolute path on this system, from Unix-style parts (a Windows
    /// path needs a drive to be absolute).
    fn abs(unix: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{}", unix.replace('/', "\\")))
        } else {
            PathBuf::from(unix)
        }
    }

    fn site(name: &str, php: Option<&str>, source: Option<Source>) -> Site {
        let root = abs("/code").join(name);
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

    /// PHP 8.3 and 8.4 installed, and `shop` with PHP 8.4.27 from the global
    /// default.
    fn model() -> Model {
        let mut model = connected();
        model.update(Update::Runtimes(RuntimeList {
            runtimes: vec![RuntimeVersions {
                runtime: Runtime::Php,
                installed: vec!["8.3.27".into(), "8.4.27".into()],
                default: Some("8.4".into()),
            }],
        }));
        let shop = site("shop", Some("8.4.27"), Some(Source::GlobalDefault));
        with_sites(&mut model, vec![shop]);
        model
    }

    /// The page of `shop` for a site with this PHP and source.
    fn model_with(php: &str, source: Source) -> Model {
        let mut model = model();
        with_sites(&mut model, vec![site("shop", Some(php), Some(source))]);
        model
    }

    fn found<'a>(view: &'a SiteView<'a>) -> &'a Found<'a> {
        match view {
            SiteView::Found(page) => page,
            other => panic!("expected a page, got {other:?}"),
        }
    }

    // -- view --

    #[test]
    fn a_name_that_is_not_in_the_list_is_a_message_and_not_a_panic() {
        let model = model();
        assert_eq!(
            view(&model, "gone"),
            SiteView::Missing(Missing {
                name: "gone",
                offline: false
            })
        );
        // No daemon: the list is empty because it is not asked.
        let offline = Model::default();
        assert_eq!(
            view(&offline, "shop"),
            SiteView::Missing(Missing {
                name: "shop",
                offline: true
            })
        );
        assert_eq!(
            strings::site_missing("gone"),
            "gone.test is not in your sites. It may have been unlinked."
        );
    }

    #[test]
    fn the_page_has_the_name_and_the_address_of_the_connection() {
        let mut model = model();
        let mut plain = site("docs", Some("8.4.27"), Some(Source::GlobalDefault));
        plain.secure = false;
        let shop = site("shop", Some("8.4.27"), Some(Source::GlobalDefault));
        with_sites(&mut model, vec![plain, shop]);
        let (docs, shop) = (view(&model, "docs"), view(&model, "shop"));
        assert_eq!(found(&docs).url, "http://docs.test");
        assert!(!found(&docs).facts.https);
        assert_eq!(found(&shop).url, "https://shop.test");
        assert_eq!(found(&shop).name, "shop");
        assert!(found(&shop).facts.https);
    }

    #[test]
    fn the_version_in_force_is_chosen_and_the_hint_says_why() {
        let dir = abs("/code/shop");
        let cases = [
            (
                Source::VersionFile(dir.join(".php-version")),
                false,
                "8.4 comes from the .php-version file in this folder",
            ),
            (
                Source::Constraint(dir.join("composer.json")),
                false,
                "8.4 comes from the composer.json file in this folder",
            ),
            (
                Source::GlobalDefault,
                false,
                "8.4 is the default: the site's files ask for no version",
            ),
            (Source::ProjectSetting, true, "8.4 is set for this site"),
        ];
        for (source, own, hint) in cases {
            let model = model_with("8.4.27", source);
            let view = view(&model, "shop");
            let php = &found(&view).php;
            let labels: Vec<_> = php.choices.iter().map(|c| c.label.as_str()).collect();
            assert_eq!(labels, ["8.3", "8.4"]);
            assert_eq!(php.chosen, Some(1), "{hint}");
            assert_eq!(php.own, own, "{hint}");
            assert_eq!(php.hint.as_deref(), Some(hint));
        }
    }

    #[test]
    fn a_site_with_no_version_in_force_has_none_chosen_and_says_why() {
        let mut model = model();
        with_sites(&mut model, vec![site("shop", None, None)]);
        let view_ = view(&model, "shop");
        let php = &found(&view_).php;
        assert_eq!(php.chosen, None);
        assert_eq!(php.reason, Some(Reason::NoMatch));
        assert_eq!(php.hint.as_deref(), Some(strings::PHP_HINT_NO_MATCH));
        // No PHP at all: the buttons are only the "+".
        model.update(Update::Runtimes(RuntimeList {
            runtimes: Vec::new(),
        }));
        let view_ = view(&model, "shop");
        let php = &found(&view_).php;
        assert!(php.choices.is_empty());
        assert_eq!(php.hint.as_deref(), Some(strings::NO_PHP_YET));
    }

    #[test]
    fn the_facts_are_the_folder_the_kind_and_a_docroot_only_when_it_differs() {
        let mut model = model();
        let mut plain = site("docs", Some("8.4.27"), Some(Source::GlobalDefault));
        plain.kind = SiteKind::Static;
        plain.docroot = plain.root.clone();
        let shop = site("shop", Some("8.4.27"), Some(Source::GlobalDefault));
        with_sites(&mut model, vec![plain, shop]);
        let docs = view(&model, "docs");
        let facts = &found(&docs).facts;
        assert_eq!(facts.kind, "Static");
        assert_eq!(facts.docroot, None);
        assert_eq!(facts.root, abs("/code").join("docs"));
        let shop = view(&model, "shop");
        let facts = &found(&shop).facts;
        assert_eq!(facts.kind, "Laravel");
        assert_eq!(
            facts.docroot,
            Some(abs("/code").join("shop").join("public").as_path())
        );
    }

    #[test]
    fn a_site_is_added_as_part_of_its_parked_folder_or_as_a_linked_folder() {
        let mut model = model();
        assert_eq!(found(&view(&model, "shop")).facts.added_as, AddedAs::Linked);
        model.sites.parked = vec![abs("/code")];
        let parked = abs("/code");
        assert_eq!(
            found(&view(&model, "shop")).facts.added_as,
            AddedAs::Parked(parked.as_path())
        );
        assert_eq!(
            strings::added_as_parked("~/Code"),
            "Part of parked folder ~/Code"
        );
        assert_eq!(strings::ADDED_AS_LINKED, "Linked folder");
        assert_eq!(strings::FACT_ADDED_AS, "Added as");
    }

    #[test]
    fn unlink_is_allowed_unless_the_site_is_parked_or_a_removal_runs() {
        let mut model = model();
        assert_eq!(found(&view(&model, "shop")).unlink, Unlink::Allowed);
        assert_eq!(found(&view(&model, "shop")).forget_off, None);
        assert!(model.ask_removal(Removal::Unlink {
            site: "shop".into()
        }));
        model.confirm_removal().expect("sent");
        let view_ = view(&model, "shop");
        assert_eq!(found(&view_).unlink, Unlink::Busy);
        assert_eq!(found(&view_).forget_off, Some(strings::REMOVE_BUSY_HINT));
        assert_eq!(
            found(&view_).removing.as_deref(),
            Some("Unlinking shop.test…")
        );
        model.sites.parked = vec![abs("/code")];
        model.update(Update::NotRemoved {
            removal: Removal::Unlink {
                site: "shop".into(),
            },
            code: "config_write".into(),
            in_use: None,
        });
        let parked = abs("/code");
        assert_eq!(
            found(&view(&model, "shop")).unlink,
            Unlink::Parked(parked.as_path())
        );
    }

    #[test]
    fn a_failed_change_shows_as_the_line_the_sites_screen_shows() {
        let mut model = model();
        assert!(found(&view(&model, "shop")).line.is_none());
        model.sites_notice = Some(SitesNotice::Failed("site_not_found".into()));
        let view_ = view(&model, "shop");
        let line = found(&view_).line.as_ref().expect("a line");
        assert_eq!(line.text, strings::error_text("site_not_found"));
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

    fn draw(ui: &mut Ui, model: &Model, name: &str) -> Vec<Action> {
        show(ui, &view(model, name), &LIGHT)
    }

    /// A harness that has drawn the page of `name` once, and its tree.
    fn start(model: &Model, name: &str) -> (Harness, Tree) {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            draw(ui, model, name);
        });
        (harness, tree)
    }

    /// One click on the widget named `label` of the page of `shop`; what the
    /// page returned.
    fn click(model: &Model, label: &str) -> Vec<Action> {
        let (harness, tree) = start(model, "shop");
        let mut actions = Vec::new();
        harness.click(bounds_center(&tree, label), |ui| {
            actions.extend(draw(ui, model, "shop"));
        });
        actions
    }

    #[test]
    fn open_folder_and_open_in_browser_are_actions_for_the_app_to_do() {
        let model = model();
        assert_eq!(
            click(&model, strings::OPEN_FOLDER),
            [Action::OpenFolder("shop".into())]
        );
        assert_eq!(
            click(&model, strings::OPEN_IN_BROWSER),
            [Action::OpenSite("shop".into())]
        );
        // The address is a link too.
        assert_eq!(
            click(&model, "https://shop.test"),
            [Action::OpenSite("shop".into())]
        );
        // The model does nothing with either: `App` opens them.
        let mut model = model;
        for action in [
            Action::OpenFolder("shop".into()),
            Action::OpenSite("shop".into()),
        ] {
            assert!(model.apply_action(action).is_none());
        }
    }

    #[test]
    fn the_way_back_goes_to_sites_from_a_page_and_from_the_message() {
        let model = model();
        assert_eq!(
            click(&model, strings::SITES),
            [Action::Navigate(Route::Sites)]
        );
        let (harness, tree) = start(&model, "gone");
        assert!(
            tree.node_with_text(&strings::site_missing("gone"))
                .is_some()
        );
        let mut actions = Vec::new();
        harness.click(bounds_center(&tree, strings::SITES), |ui| {
            actions.extend(draw(ui, &model, "gone"));
        });
        assert_eq!(actions, [Action::Navigate(Route::Sites)]);
        // With no daemon the message says so.
        let (_, tree) = start(&Model::default(), "shop");
        assert!(tree.node_with_text(strings::SITE_OFFLINE).is_some());
    }

    /// A click on `label`, and what the model sends for what the page
    /// returned.
    fn sent(model: &mut Model, label: &str) -> Vec<Command> {
        let actions = click(model, label);
        actions
            .into_iter()
            .filter_map(|action| model.apply_action(action))
            .collect()
    }

    #[test]
    fn choosing_a_version_saves_it_for_the_site() {
        let mut model = model();
        let actions = click(&model, "8.3");
        assert_eq!(
            actions,
            [Action::SitePhp {
                site: "shop".into(),
                choice: Choice::Version(v("8.3.27")),
            }]
        );
        let commands = sent(&mut model, "8.3");
        let [Command::Save(change)] = &commands[..] else {
            panic!("one Save, got {commands:?}");
        };
        assert_eq!(
            *change,
            Change::SitePhp {
                site: "shop".into(),
                php: Some("8.3".into()),
            }
        );
        // The page shows the choice until the daemon answers.
        let view_ = view(&model, "shop");
        assert_eq!(found(&view_).php.chosen, Some(0));
        assert!(found(&view_).php.own);
        // The version in force is already chosen: nothing to send.
        assert!(click(&model_with("8.4.27", Source::GlobalDefault), "8.4").is_empty());
    }

    #[test]
    fn use_the_sites_files_shows_with_a_setting_of_its_own_and_clears_it() {
        // No setting of its own: nothing to clear.
        let (_, tree) = start(&model(), "shop");
        assert!(tree.node_with_label(strings::USE_SITE_FILES).is_none());

        let mut model = model_with("8.3.27", Source::ProjectSetting);
        assert_eq!(
            click(&model, strings::USE_SITE_FILES),
            [Action::SitePhp {
                site: "shop".into(),
                choice: Choice::Auto,
            }]
        );
        let commands = sent(&mut model, strings::USE_SITE_FILES);
        let [Command::Save(change)] = &commands[..] else {
            panic!("one Save, got {commands:?}");
        };
        assert_eq!(
            *change,
            Change::SitePhp {
                site: "shop".into(),
                php: None,
            }
        );
        // Until the daemon answers, the files decide and the button is gone.
        let view_ = view(&model, "shop");
        assert!(!found(&view_).php.own);
        assert_eq!(found(&view_).php.reason, Some(Reason::Files));
    }

    #[test]
    fn the_plus_after_the_versions_opens_the_install_dialog_of_php_on_this_page() {
        let label = strings::install_another_label(strings::runtime_name(Runtime::Php));
        assert_eq!(click(&model(), &label), [Action::AskInstall(Runtime::Php)]);
        let mut model = model();
        model.route = Route::Site("shop".into());
        assert!(
            model
                .apply_action(Action::AskInstall(Runtime::Php))
                .is_none()
        );
        assert_eq!(model.install_dialog, Some(Runtime::Php));
        assert_eq!(model.route, Route::Site("shop".into()));
        // While an install runs, the plus is off with its reason.
        let mut busy = self::model();
        assert!(busy.queue_install(crate::model::InstallJob {
            runtime: Runtime::Caddy,
            version: None,
        }));
        assert_eq!(
            found(&view(&busy, "shop")).php.install_off,
            Some(strings::INSTALL_BUSY_HINT)
        );
        assert!(click(&busy, &label).is_empty());
    }

    #[test]
    fn the_https_switch_sends_the_other_connection() {
        let mut model = model();
        assert_eq!(
            click(&model, strings::SERVE_OVER_HTTPS),
            [Action::SetSecure {
                site: "shop".into(),
                secure: false,
            }]
        );
        let commands = sent(&mut model, strings::SERVE_OVER_HTTPS);
        assert!(
            matches!(&commands[..], [Command::Secure { site, secure: false }] if site == "shop"),
            "{commands:?}"
        );
        // A site on plain HTTP: the switch turns HTTPS on.
        let mut plain = site("shop", Some("8.4.27"), Some(Source::GlobalDefault));
        plain.secure = false;
        with_sites(&mut model, vec![plain]);
        assert_eq!(
            click(&model, strings::SERVE_OVER_HTTPS),
            [Action::SetSecure {
                site: "shop".into(),
                secure: true,
            }]
        );
        // The switch says what it is for a screen reader.
        let (_, tree) = start(&model, "shop");
        let node = tree
            .node_with_label(strings::SERVE_OVER_HTTPS)
            .expect("the switch");
        assert_eq!(node.role(), egui::Role::Switch);
        assert_eq!(node.toggled(), Some(egui::accesskit::Toggled::False));
    }

    #[test]
    fn unlink_opens_the_confirmation_and_sends_nothing() {
        let mut model = model();
        assert_eq!(
            click(&model, strings::UNLINK_THIS_SITE),
            [Action::AskUnlink("shop".into())]
        );
        let commands = sent(&mut model, strings::UNLINK_THIS_SITE);
        assert!(commands.is_empty(), "{commands:?}");
        assert_eq!(
            model.confirm,
            Some(Removal::Unlink {
                site: "shop".into()
            })
        );
        // Only the confirmation sends the removal.
        assert!(matches!(
            model.confirm_removal(),
            Some(Removal::Unlink { .. })
        ));
    }

    #[test]
    fn a_site_from_a_parked_folder_cannot_be_unlinked_and_offers_to_forget_its_folder() {
        let mut model = model();
        model.sites.parked = vec![abs("/code")];
        model.home = Some(abs("/code"));
        // The button is off: a click returns nothing.
        assert!(click(&model, strings::UNLINK_THIS_SITE).is_empty());
        let shown = strings::home_path(&abs("/code"), model.home.as_deref());
        let (_, tree) = start(&model, "shop");
        assert!(
            tree.node_with_text(&strings::site_from_parked(&shown))
                .is_some()
        );
        // The offer names the folder and asks about the whole folder.
        assert_eq!(
            click(&model, &strings::forget_folder_shown(&shown)),
            [Action::AskForget(abs("/code"))]
        );
        // While a removal runs, the offer waits too.
        model.sites.parked.clear();
        assert!(model.ask_removal(Removal::Unlink {
            site: "shop".into()
        }));
        model.confirm_removal().expect("sent");
        assert!(click(&model, strings::UNLINK_THIS_SITE).is_empty());
    }

    #[test]
    fn a_linked_site_has_no_forget_offer_and_no_reason_line() {
        let mut model = model();
        let shown = strings::home_path(&abs("/code"), None);
        let offer = strings::forget_folder_shown(&shown);
        let reason = strings::site_from_parked(&shown);
        let (_, tree) = start(&model, "shop");
        assert!(tree.node_with_label(&offer).is_none());
        assert!(tree.node_with_text(&reason).is_none());
        // The same site, once its folder is parked: both are there.
        model.sites.parked = vec![abs("/code")];
        let (_, tree) = start(&model, "shop");
        assert!(tree.node_with_label(&offer).is_some());
        assert!(tree.node_with_text(&reason).is_some());
    }

    #[test]
    fn the_folders_show_with_a_tilde_and_the_kind_and_the_origin_are_words() {
        let home = abs("/Users/omar");
        let mut model = model();
        let mut shop = site("shop", Some("8.4.27"), Some(Source::GlobalDefault));
        shop.root = home.join("Code").join("shop");
        shop.docroot = shop.root.join("public");
        with_sites(&mut model, vec![shop.clone()]);
        model.home = Some(home.clone());
        model.sites.parked = vec![home.join("Code")];
        let (_, tree) = start(&model, "shop");
        let tilde = |path: &Path| strings::home_path(path, Some(&home));
        for path in [shop.root.clone(), shop.docroot.clone(), home.join("Code")] {
            let shown = tilde(&path);
            assert!(shown.starts_with('~'), "{shown}");
            let wanted = if path == home.join("Code") {
                strings::added_as_parked(&shown)
            } else {
                shown
            };
            assert!(tree.node_with_text(&wanted).is_some(), "{wanted}");
        }
        assert!(tree.node_with_text("Laravel").is_some());
        // The tilde is display only: the action carries no path at all.
        assert_eq!(
            click(&model, strings::OPEN_FOLDER),
            [Action::OpenFolder("shop".into())]
        );
        assert_eq!(model.site_root("shop"), Some(shop.root.as_path()));
    }

    #[test]
    fn the_page_has_no_tab_bar_while_it_has_one_part_and_no_part_of_a_feature_that_is_not_there() {
        let (_, tree) = start(&model(), "shop");
        assert!(tree.node_with_role(egui::Role::TabList).is_none());
        assert!(tree.node_with_role(egui::Role::Tab).is_none());
        for absent in ["Overview", "Mail", "Dumps", "Logs", "Database"] {
            assert!(tree.node_with_label(absent).is_none(), "{absent}");
        }
    }

    #[test]
    fn the_way_back_to_the_files_follows_the_reason_on_the_same_line() {
        let model = model_with("8.3.27", Source::ProjectSetting);
        let (_, tree) = start(&model, "shop");
        let bounds = |node: &egui::accesskit::Node| node.bounds().expect("a place");
        let hint = tree
            .node_with_text("PHP for this site8.3 is set for this site")
            .expect("the heading and the reason");
        let separator = tree
            .node_with_text(strings::HINT_SEPARATOR)
            .expect("the separator");
        let link = tree
            .node_with_label(strings::USE_SITE_FILES)
            .expect("the link");
        // One line: the reason, then the separator, then the link.
        assert!(bounds(hint).x1 <= bounds(separator).x0);
        assert!(bounds(separator).x1 <= bounds(link).x0);
        let middle = |node| (bounds(node).y0 + bounds(node).y1) / 2.0;
        assert!((middle(hint) - middle(link)).abs() < 2.0);
        assert!((middle(separator) - middle(link)).abs() < 2.0);
        // Without a setting of its own there is no link and no separator.
        let (_, tree) = start(&self::model(), "shop");
        assert!(tree.node_with_label(strings::USE_SITE_FILES).is_none());
        assert!(tree.node_with_text(strings::HINT_SEPARATOR).is_none());
    }

    #[test]
    fn the_link_to_the_sites_files_is_a_tab_stop_that_shows_the_focus_ring() {
        let model = model_with("8.3.27", Source::ProjectSetting);
        let (harness, tree) = start(&model, "shop");
        let b = tree
            .node_with_label(strings::USE_SITE_FILES)
            .expect("the link")
            .bounds()
            .expect("a place");
        let ring = LIGHT.focus.offset;
        let link = egui::Rect::from_min_max(
            pos2(b.x0 as f32 - ring, b.y0 as f32 - ring),
            pos2(b.x1 as f32 + ring, b.y1 as f32 + ring),
        );
        let ring_on_link = |harness: &Harness| {
            harness
                .shapes(|ui| {
                    draw(ui, &model, "shop");
                })
                .iter()
                .any(|shape| {
                    matches!(shape, egui::Shape::Rect(rect)
                        if rect.stroke.color == LIGHT.color.accent
                            && rect.stroke.width == LIGHT.focus.width
                            && rect.rect.min.distance(link.min) < 1.0
                            && rect.rect.max.distance(link.max) < 1.0)
                })
        };
        assert!(!ring_on_link(&harness), "no ring before the keyboard comes");
        // Tab walks the page: the link is one of its stops.
        let reached = (0..30).any(|_| {
            harness.key(egui::Key::Tab, |ui| {
                draw(ui, &model, "shop");
            });
            ring_on_link(&harness)
        });
        assert!(reached, "Tab reaches the link and the ring shows");
    }

    #[test]
    fn a_failed_change_and_its_close_button_show_on_the_page() {
        let mut model = model();
        model.sites_notice = Some(SitesNotice::Failed("config_write".into()));
        let (_, tree) = start(&model, "shop");
        assert!(
            tree.node_with_text(&strings::error_text("config_write"))
                .is_some()
        );
        assert_eq!(click(&model, strings::CLOSE), [Action::DismissSitesNotice]);
    }
}
