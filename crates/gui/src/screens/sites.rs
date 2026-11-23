//! Sites (layout.md, Screens): the table of sites with the PHP version and
//! why it is that one, the connection, and Open; the search field; and the
//! "Add site…" menu. Under the table, the parked folders and the folders that
//! did not become a site.
//!
//! The screen is two functions, like Home. [`view`] reads the [`Model`] and
//! decides what Sites says: pure, so the tests assert it without a window.
//! [`show`] draws that [`SitesView`] with the widgets and returns what the
//! user did as [`Action`]s (code.md, State). [Laravel: `view` is the
//! controller that builds the data; `show` is the Blade template.]
//!
//! A click on a row opens the site's page (`site`), where HTTPS or HTTP and
//! Unlink are.

use std::path::{Path, PathBuf};

use eframe::egui::os::OperatingSystem;
use eframe::egui::{Id, Key, KeyboardShortcut, Modifiers, Response, Role, Ui, WidgetInfo, vec2};
use marsa_core::types::{Site, SkippedFolder, Source};

use crate::model::{Action, Choice, HostsSync, Model, Pick, Removal, Route, Setup};
use crate::screens::home::{PhpChoice, php_choices};
use crate::screens::lines::{Mark, message, mute, path, status_line};
use crate::strings;
use crate::theme::page::{
    NAME_FIELD_WIDTH, SITES_CONNECTION_COLUMN, SITES_FOLDER_ROWS, SITES_FORGET_COLUMN,
    SITES_OPEN_COLUMN, SITES_PHP_COLUMN, SITES_SEARCH_WIDTH, SITES_WHY_COLUMN,
};
use crate::theme::{Section, Theme};
use crate::widgets::table::{empty_state_button, padded_card};
use crate::widgets::{
    Button, Field, Glyph, Icon, Menu, MenuItem, RowHeight, Setting, Table, TableColumn, Text,
    VersionMenu,
};

/// What Sites says.
#[derive(Debug, PartialEq)]
pub(crate) struct SitesView<'a> {
    /// The text of the search field, as typed.
    pub search: &'a str,
    /// Why "Add site…" cannot be pressed now.
    pub add_off: Option<&'static str>,
    /// The form that names a folder picked to link.
    pub link: Option<LinkView<'a>>,
    /// Linux: the sites the hosts file lacks.
    pub hosts: Option<HostsView>,
    /// "Unlinking shop.test…", while the daemon removes it.
    pub removing: Option<String>,
    pub list: List<'a>,
    /// Where the rows are built from: a row is made when the table asks for
    /// it ([`SitesView::row`]), never for every site.
    model: ReadFrom<'a>,
    /// The installed PHP versions, oldest first: the entries of every
    /// version menu after the first row.
    pub php: Vec<PhpChoice>,
    pub parked: &'a [PathBuf],
    /// The user's home folder: a folder under it is shown as `~/…`, with the
    /// full path in a tooltip. Display only.
    pub home: Option<&'a Path>,
    /// Why a parked folder cannot be forgotten now.
    pub forget_off: Option<&'static str>,
    pub skipped: &'a [SkippedFolder],
}

/// The model a view builds its rows from. A model is not `PartialEq`, so
/// two views are equal when they read the same model.
#[derive(Clone, Copy)]
struct ReadFrom<'a>(&'a Model);

impl PartialEq for ReadFrom<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl std::fmt::Debug for ReadFrom<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the model")
    }
}

/// The table, or what stands in its place.
#[derive(Debug, PartialEq)]
pub(crate) enum List<'a> {
    /// No daemon: there is nothing to list.
    Offline,
    /// No site at all: one sentence and the button that adds one.
    Empty,
    /// The search matches none of the sites.
    NoMatch(&'a str),
    /// The sites the search lets through. No row exists yet: the table asks
    /// for the rows in view (R16), and 500 sites cost what 12 do.
    Rows(Matches),
}

/// Which of the model's sites the search lets through, in order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Matches {
    /// Their indexes in the model's list; none while the search is empty
    /// and every site shows.
    found: Option<Vec<usize>>,
    count: usize,
}

impl Matches {
    pub(crate) const fn len(&self) -> usize {
        self.count
    }

    /// The index in the model's list of the `n`th site that shows.
    fn site(&self, n: usize) -> Option<usize> {
        match &self.found {
            Some(found) => found.get(n).copied(),
            None => (n < self.count).then_some(n),
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct SiteRow<'a> {
    pub name: &'a str,
    pub root: &'a Path,
    pub php: PhpCell<'a>,
    /// Served over HTTPS.
    pub https: bool,
}

/// The version menu of a row.
#[derive(Debug, PartialEq)]
pub(crate) struct PhpCell<'a> {
    /// What the button shows: `8.4`, or a dash for a site with no PHP.
    pub shown: String,
    /// The index (in [`SitesView::php`]) of the version in force, if it is
    /// installed: the one a large picker chooses.
    pub at: Option<usize>,
    /// The entry the menu checks: the site's own setting, or its files.
    pub setting: Setting<usize>,
    pub reason: Option<Reason<'a>>,
    /// Why the menu cannot open.
    pub off: Option<&'static str>,
}

/// Why a site has the PHP version its row shows (components.md, Version
/// menu): where `Site::php_source` says it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason<'a> {
    /// The site's own setting, saved by Marsa.
    Site,
    /// `.php-version` or `composer.json`: the file's path.
    File(&'a Path),
    /// The global default.
    Default,
    /// The user cleared the site's own setting and the daemon has not
    /// answered: the files decide, and the window does not know the version.
    Files,
    /// No PHP is installed.
    NoPhpInstalled,
    /// PHP is installed, and no version fits what the site asks for.
    NoMatch,
}

impl Reason<'_> {
    /// The sentence after the heading of a site's page: the version
    /// `shown` and why the site has it (components.md, Version menu).
    pub(crate) fn hint(&self, shown: &str) -> String {
        match self {
            Self::Site => strings::php_hint_site(shown),
            Self::File(path) => strings::php_hint_file(shown, path),
            Self::Default => strings::php_hint_default(shown),
            Self::Files => strings::PHP_HINT_FILES.to_owned(),
            Self::NoPhpInstalled => strings::NO_PHP_YET.to_owned(),
            Self::NoMatch => strings::PHP_HINT_NO_MATCH.to_owned(),
        }
    }

    pub(crate) fn text(&self) -> String {
        match self {
            Self::Site => strings::REASON_SITE.to_owned(),
            Self::File(path) => strings::reason_file(path),
            Self::Default => strings::REASON_DEFAULT.to_owned(),
            Self::Files => strings::REASON_FILES.to_owned(),
            Self::NoPhpInstalled => strings::REASON_NO_PHP.to_owned(),
            Self::NoMatch => strings::REASON_NO_MATCH.to_owned(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct LinkView<'a> {
    pub folder: &'a Path,
    pub name: &'a str,
    /// The site the name would replace, if the name is taken.
    pub replaces: Option<&'a Site>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct HostsView {
    /// The sentence that names the sites the hosts file lacks.
    pub waiting: Option<String>,
    /// Why "Add to /etc/hosts" cannot be pressed now.
    pub off: Option<&'static str>,
    /// The last try failed: its error.
    pub failed: Option<String>,
}

/// Decides what Sites says. Pure: the same model gives the same view.
/// `picking`: a folder dialog is open.
pub(crate) fn view(model: &Model, picking: bool) -> SitesView<'_> {
    let php = php_choices(model);
    let list = list(model);
    SitesView {
        search: &model.sites_search,
        add_off: add_off(model, picking),
        link: link(model),
        hosts: hosts(model),
        removing: removing_line(model),
        list,
        model: ReadFrom(model),
        php,
        parked: &model.session.sites.parked,
        home: model.home.as_deref(),
        forget_off: (model.is_connected() && !model.can_remove())
            .then_some(strings::REMOVE_BUSY_HINT),
        skipped: &model.session.sites.skipped,
    }
}

/// "Unlinking shop.test…", while the daemon removes a site or a folder.
pub(super) fn removing_line(model: &Model) -> Option<String> {
    match model.removing() {
        Some(removal @ (Removal::Unlink { .. } | Removal::Forget { .. })) => {
            Some(removal.progress())
        }
        _ => None,
    }
}

fn add_off(model: &Model, picking: bool) -> Option<&'static str> {
    if !model.is_connected() {
        Some(strings::ADD_SITE_OFFLINE)
    } else if picking {
        Some(strings::PICKING_HINT)
    } else {
        None
    }
}

fn list(model: &Model) -> List<'_> {
    if !model.is_connected() {
        return List::Offline;
    }
    let all = &model.session.sites.sites;
    if all.is_empty() {
        return List::Empty;
    }
    let search = model.sites_search.trim();
    // Only the indexes: a row is built when the table draws it.
    let found: Option<Vec<usize>> = (!search.is_empty()).then(|| {
        all.iter()
            .enumerate()
            .filter(|(_, site)| matches(&site.name, search))
            .map(|(index, _)| index)
            .collect()
    });
    let count = found.as_ref().map_or(all.len(), Vec::len);
    if count == 0 {
        List::NoMatch(search)
    } else {
        List::Rows(Matches { found, count })
    }
}

/// Whether `name` holds `search`, ignoring case. A site name is ASCII
/// (`a-z`, `0-9`, and `-`), so comparing bytes is exact and allocates
/// nothing, which matters with hundreds of sites on every frame.
fn matches(name: &str, search: &str) -> bool {
    let (name, search) = (name.as_bytes(), search.as_bytes());
    search.is_empty()
        || name
            .windows(search.len())
            .any(|window| window.eq_ignore_ascii_case(search))
}

impl<'a> SitesView<'a> {
    /// The `n`th row of the table, built now. `None` past the last one, and
    /// when the screen has no table.
    pub(crate) fn row(&self, n: usize) -> Option<SiteRow<'a>> {
        let List::Rows(matches) = &self.list else {
            return None;
        };
        let model = self.model.0;
        let site = model.session.sites.sites.get(matches.site(n)?)?;
        #[cfg(test)]
        BUILT.with(|built| built.set(built.get() + 1));
        Some(SiteRow {
            name: &site.name,
            root: &site.root,
            php: php_cell(model, site, &self.php),
            https: site.secure,
        })
    }
}

// Tests only: how many rows were built, so a test can tell that a screen of
// 2000 sites built a screenful and not 2000. A counter in a release build
// would be a cost with no use.
#[cfg(test)]
thread_local! {
    static BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The version the row shows, the entry its menu checks, and why. The entry
/// the user just picked shows until the daemon answers, as the model keeps it
/// ([`Model::site_choice`]).
pub(super) fn php_cell<'a>(model: &Model, site: &'a Site, php: &[PhpChoice]) -> PhpCell<'a> {
    let (version, reason) = match model.site_choice(site) {
        Choice::Version(version) => (Some(version), Some(Reason::Site)),
        Choice::Auto => (site.php.clone(), auto_reason(site, php.is_empty())),
    };
    let at = version
        .as_ref()
        .and_then(|version| php.iter().position(|choice| choice.version == *version));
    let shown = match (&version, at) {
        (Some(_), Some(at)) => php[at].label.clone(),
        (Some(version), None) => version.to_string(),
        (None, _) => strings::NO_PHP.to_owned(),
    };
    // The site's own setting is checked; with no setting of its own, the
    // first row is: its files decide.
    let setting = match (model.site_choice(site), at) {
        (Choice::Version(_), Some(at)) => Setting::Version(at),
        _ => Setting::SiteFiles,
    };
    PhpCell {
        shown,
        at,
        setting,
        reason,
        off: php.is_empty().then_some(strings::NO_PHP_YET),
    }
}

/// Why a site with no setting of its own has the version it has.
fn auto_reason(site: &Site, no_php: bool) -> Option<Reason<'_>> {
    match (&site.php_source, &site.php) {
        // After a click on "Use the site's files", before the answer.
        (Some(Source::ProjectSetting), _) => Some(Reason::Files),
        (Some(Source::VersionFile(path) | Source::Constraint(path)), _) => Some(Reason::File(path)),
        (Some(Source::GlobalDefault), _) => Some(Reason::Default),
        (None, None) if no_php => Some(Reason::NoPhpInstalled),
        (None, None) => Some(Reason::NoMatch),
        // A daemon older than `php_source`: nothing to say.
        (None, Some(_)) => None,
    }
}

fn link(model: &Model) -> Option<LinkView<'_>> {
    let form = model.link.as_ref()?;
    Some(LinkView {
        folder: &form.folder,
        name: &form.name,
        replaces: model.link_replaces(),
    })
}

fn hosts(model: &Model) -> Option<HostsView> {
    let failed = match &model.session.hosts_sync {
        HostsSync::Failed(code) => Some(strings::error_text(code)),
        HostsSync::Idle | HostsSync::Running => None,
    };
    let waiting = (model.session.hosts.used && !model.session.hosts.missing.is_empty())
        .then(|| strings::hosts_waiting(&model.session.hosts.missing));
    if waiting.is_none() && failed.is_none() {
        return None;
    }
    let busy =
        model.session.hosts_sync == HostsSync::Running || model.session.setup == Setup::Running;
    Some(HostsView {
        waiting,
        off: busy.then_some(strings::HOSTS_ADDING),
        failed,
    })
}

/// The id of the search field, so Cmd or Ctrl K can move the focus to it.
fn search_id() -> Id {
    Id::unique("sites_search")
}

/// Cmd K on macOS, Ctrl K elsewhere: focus the search field.
const SEARCH_KEY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::K);

/// Draws `view`. Returns what the user did.
pub(crate) fn show(ui: &mut Ui, view: &SitesView<'_>, theme: &Theme) -> Vec<Action> {
    let mut actions = Vec::new();
    ui.spacing_mut().item_spacing.y = theme.space.section_gap;
    let tools = matches!(view.list, List::Rows(_) | List::NoMatch(_));
    if tools && ui.input_mut(|input| input.consume_shortcut(&SEARCH_KEY)) {
        ui.memory_mut(|memory| memory.request_focus(search_id()));
    }
    super::header(ui, theme, Section::Sites, strings::SITES, |ui| {
        if tools {
            // From the end: the button, then the search before it.
            add_site(ui, view, &mut actions);
            search(ui, theme, view, &mut actions);
        }
    });
    if let Some(link) = &view.link {
        link_form(ui, theme, link, view.home, &mut actions);
    }
    if let Some(hosts) = &view.hosts {
        hosts_line(ui, theme, hosts, &mut actions);
    }
    if let Some(text) = &view.removing {
        mute(ui, theme, text);
    }
    match &view.list {
        List::Offline => message(ui, theme, strings::SITES_OFFLINE),
        List::Empty => {
            let button = empty_state_button(ui, strings::SITES_EMPTY, strings::ADD_SITE_BUTTON);
            add_menu(&button, &mut actions);
        }
        List::NoMatch(search) => message(ui, theme, &strings::no_site_matches(search)),
        List::Rows(matches) => table(ui, theme, view, matches.len(), &mut actions),
    }
    parked(ui, theme, view, &mut actions);
    skipped(ui, theme, view.skipped, view.home);
    actions
}

/// The primary button of the header, with its menu.
fn add_site(ui: &mut Ui, view: &SitesView<'_>, actions: &mut Vec<Action>) {
    let button = Button::primary(strings::ADD_SITE_BUTTON)
        .disabled_if(view.add_off.is_some(), view.add_off.unwrap_or_default());
    let response = ui.add(button);
    add_menu(&response, actions);
}

/// "Park a folder…" and "Link a folder…" under the button `response`.
fn add_menu(response: &Response, actions: &mut Vec<Action>) {
    let items = [
        MenuItem::new(Pick::Park, strings::PARK_BUTTON).hint(strings::PARK_BUTTON_HINT),
        MenuItem::new(Pick::Link, strings::LINK_BUTTON).hint(strings::LINK_BUTTON_HINT),
    ];
    if let Some(pick) = Menu::new(&items).from_end().show(response) {
        actions.push(Action::PickFolder(pick));
    }
}

/// The search field. The text is the model's: an edit becomes an action, and
/// the next frame filters by it.
fn search(ui: &mut Ui, theme: &Theme, view: &SitesView<'_>, actions: &mut Vec<Action>) {
    let mut text = view.search.to_owned();
    let key = strings::search_key_hint(ui.ctx().os() == OperatingSystem::Mac);
    let field = Field::search(&mut text, strings::FIND_SITE)
        .hint(strings::FIND_SITE)
        .key_hint(key)
        .width(SITES_SEARCH_WIDTH)
        .id(search_id());
    // A field in a row that runs from the end sits at the start of the space
    // it is given: give it exactly its own width.
    let size = vec2(SITES_SEARCH_WIDTH, theme.size.control);
    let changed = ui.allocate_ui(size, |ui| ui.add(field).changed()).inner;
    if changed {
        actions.push(Action::SearchSites(text));
        // The table filters on the next frame: ask for it, since typing is
        // the only input there is.
        ui.ctx().request_repaint();
    }
}

/// Linux on the hosts way: the sites the hosts file lacks, with the button
/// that adds them. The click is the only way the window asks for the
/// password for this.
fn hosts_line(ui: &mut Ui, theme: &Theme, hosts: &HostsView, actions: &mut Vec<Action>) {
    if let Some(waiting) = &hosts.waiting {
        status_line(ui, theme, Mark::Warning, waiting, |ui| {
            let button = Button::secondary(strings::ADD_TO_HOSTS)
                .in_row()
                .disabled_if(hosts.off.is_some(), hosts.off.unwrap_or_default());
            let response = ui.add(button);
            if response.clicked() {
                actions.push(Action::SyncHosts);
            }
            if hosts.off.is_none() {
                response.on_hover_text(strings::ADD_TO_HOSTS_HINT);
            }
        });
    }
    if let Some(failed) = &hosts.failed {
        status_line(ui, theme, Mark::Error, failed, |_| {});
    }
}

/// The form of a folder picked to link: the name field with `.test` after
/// it, Link, and Cancel.
fn link_form(
    ui: &mut Ui,
    theme: &Theme,
    link: &LinkView<'_>,
    home: Option<&Path>,
    actions: &mut Vec<Action>,
) {
    padded_card(ui, |ui| {
        ui.spacing_mut().item_spacing.y = theme.space.s2;
        // The folder shows as `~/…` under the home folder; the tooltip and a
        // screen reader have the whole path.
        let full = link.folder.display().to_string();
        let shown = strings::link_folder(&strings::home_path(link.folder, home));
        let title = ui.add(Text::new(&shown, theme.text.body_sm, theme.color.ink));
        let spoken = strings::link_folder(&full);
        title.widget_info(|| WidgetInfo::labeled(Role::Label, true, &spoken));
        title.on_hover_text(full);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = theme.space.s2;
            let mut name = link.name.to_owned();
            let field = Field::new(&mut name, strings::LINK_NAME)
                .hint(strings::LINK_NAME_HINT)
                .width(NAME_FIELD_WIDTH);
            let field = ui.add(field);
            let entered = field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            if field.changed() {
                actions.push(Action::SetLinkName(name));
                ui.ctx().request_repaint();
            }
            mute(ui, theme, &strings::site_suffix());
            // The one main action of the form.
            if ui.add(Button::primary(strings::LINK_CONFIRM)).clicked() || entered {
                actions.push(Action::SubmitLink);
            }
            if ui.add(Button::secondary(strings::CANCEL)).clicked() {
                actions.push(Action::CancelLink);
            }
        });
        if let Some(site) = link.replaces {
            mute(ui, theme, &strings::link_replaces(site));
        }
    });
}

/// The sites table. Only the rows in view are drawn (`Table` is over
/// `TableBuilder`): 500 sites cost what 12 do.
fn table(
    ui: &mut Ui,
    theme: &Theme,
    view: &SitesView<'_>,
    count: usize,
    actions: &mut Vec<Action>,
) {
    let columns = [
        TableColumn::fill(strings::COLUMN_SITE),
        TableColumn::fixed(strings::COLUMN_PHP, SITES_PHP_COLUMN),
        TableColumn::fixed(strings::COLUMN_CONNECTION, SITES_CONNECTION_COLUMN),
        TableColumn::fixed("", SITES_OPEN_COLUMN),
    ];
    // The menus list the same versions on every row.
    let versions: Vec<(usize, &str)> = view
        .php
        .iter()
        .enumerate()
        .map(|(index, choice)| (index, choice.label.as_str()))
        .collect();
    let table = Table::new("sites", strings::SITES, &columns).rows(RowHeight::TwoLine);
    let shown = table.show(ui, count, |row| {
        // Built here, for the rows in view only.
        let Some(site) = view.row(row.index()) else {
            return;
        };
        let full = site.root.display().to_string();
        let shown = strings::home_path(site.root, view.home);
        row.two_lines_hover(site.name, &shown, &full);
        row.cell(|ui| php_menu(ui, view, &site, &versions, actions));
        row.cell(|ui| connection(ui, theme, site.https));
        row.cell(|ui| {
            if ui.add(Button::secondary(strings::OPEN).in_row()).clicked() {
                actions.push(Action::OpenSite(site.name.to_owned()));
            }
        });
    });
    if let Some(site) = shown.clicked.and_then(|index| view.row(index)) {
        actions.push(Action::Navigate(Route::Site(site.name.to_owned())));
    }
}

/// The version button of a row, with the reason after it.
fn php_menu(
    ui: &mut Ui,
    view: &SitesView<'_>,
    site: &SiteRow<'_>,
    versions: &[(usize, &str)],
    actions: &mut Vec<Action>,
) {
    let label = strings::php_for_site(site.name);
    let reason = site.php.reason.map(|reason| reason.text());
    let mut menu = VersionMenu::new(
        &label,
        &site.php.shown,
        strings::USE_SITE_FILES,
        versions,
        site.php.setting,
    );
    if let Some(reason) = &reason {
        menu = menu.reason(reason);
    }
    if let Some(off) = site.php.off {
        menu = menu.disabled(off);
    }
    let choice = match menu.show(ui).inner {
        Some(Setting::SiteFiles) => Choice::Auto,
        Some(Setting::Version(index)) => match view.php.get(index) {
            Some(choice) => Choice::Version(choice.version.clone()),
            None => return,
        },
        None => return,
    };
    actions.push(Action::SitePhp {
        site: site.name.to_owned(),
        choice,
    });
}

/// The lock and the word: HTTPS, or HTTP in `mute`.
fn connection(ui: &mut Ui, theme: &Theme, https: bool) {
    let (icon, word, color) = if https {
        (Icon::Lock, strings::HTTPS, theme.color.ink2)
    } else {
        (Icon::LockOpen, strings::HTTP, theme.color.mute)
    };
    ui.add(Glyph::new(icon).size(theme.size.icon_inline).color(color));
    ui.add(Text::new(word, theme.text.body_sm, color));
}

/// A list of folders under the table: a heading, and a card of dense rows
/// that scrolls inside itself past a few.
fn folders(
    ui: &mut Ui,
    theme: &Theme,
    title: (&str, &str),
    table: Table<'_>,
    height: RowHeight,
    rows: usize,
    add_row: impl FnMut(&mut crate::widgets::table::Row<'_, '_>),
) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = theme.space.group_gap_max;
        super::heading(ui, theme, title.0, Some(title.1));
        let scrolls = height.points(theme) * SITES_FOLDER_ROWS;
        table
            .rows(height)
            .scrolls_past(scrolls)
            .show(ui, rows, add_row);
    });
}

/// The parked folders, each with its Forget button.
fn parked(ui: &mut Ui, theme: &Theme, view: &SitesView<'_>, actions: &mut Vec<Action>) {
    if view.parked.is_empty() {
        return;
    }
    let columns = [
        TableColumn::fill(strings::COLUMN_FOLDER),
        TableColumn::fixed("", SITES_FORGET_COLUMN),
    ];
    let table = Table::new("parked", strings::PARKED_TITLE, &columns);
    let title = (strings::PARKED_TITLE, strings::PARKED_HINT);
    folders(
        ui,
        theme,
        title,
        table,
        RowHeight::OneLine,
        view.parked.len(),
        |row| {
            let folder = &view.parked[row.index()];
            row.cell(|ui| path(ui, theme, folder, view.home));
            row.cell(|ui| {
                let button = Button::secondary(strings::FORGET_BUTTON)
                    .in_row()
                    .disabled_if(
                        view.forget_off.is_some(),
                        view.forget_off.unwrap_or_default(),
                    );
                let response = ui.add(button);
                if response.clicked() {
                    actions.push(Action::AskForget(folder.clone()));
                }
                if view.forget_off.is_none() {
                    response.on_hover_text(strings::FORGET_HINT);
                }
            });
        },
    );
}

/// The folders that did not become a site, each with the reason.
fn skipped(ui: &mut Ui, theme: &Theme, folders_skipped: &[SkippedFolder], home: Option<&Path>) {
    if folders_skipped.is_empty() {
        return;
    }
    let columns = [
        TableColumn::fill(strings::COLUMN_FOLDER),
        TableColumn::fixed(strings::COLUMN_WHY, SITES_WHY_COLUMN),
    ];
    let table = Table::new("skipped", strings::SKIPPED_TITLE, &columns);
    let title = (strings::SKIPPED_TITLE, strings::SKIPPED_HINT);
    folders(
        ui,
        theme,
        title,
        table,
        RowHeight::Dense,
        folders_skipped.len(),
        |row| {
            let folder = &folders_skipped[row.index()];
            row.cell(|ui| path(ui, theme, &folder.path, home));
            row.cell(|ui| {
                let reason = strings::skip_reason(&folder.code);
                ui.add(Text::small(theme, &reason).truncate())
                    .on_hover_text(&reason);
            });
        },
    );
}

#[cfg(test)]
mod tests;
