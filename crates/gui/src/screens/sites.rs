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
use eframe::egui::{
    Id, Key, KeyboardShortcut, Label, Modifiers, Response, RichText, Role, Ui, WidgetInfo, vec2,
};
use marsa_core::types::{Site, SkippedFolder, Source};

use crate::model::{Action, Choice, HostsSync, Model, Pick, Removal, Route, Setup};
use crate::screens::home::{PhpChoice, php_choices};
use crate::strings;
use crate::theme::page::{
    NAME_FIELD_WIDTH, SITES_CONNECTION_COLUMN, SITES_FOLDER_ROWS, SITES_FORGET_COLUMN,
    SITES_OPEN_COLUMN, SITES_PHP_COLUMN, SITES_SEARCH_WIDTH, SITES_WHY_COLUMN,
};
use crate::theme::{Section, Theme};
use crate::widgets::table::{empty_state_button, padded_card};
use crate::widgets::{
    Button, Field, Glyph, Icon, Menu, MenuItem, NoticeKind, RowHeight, Setting, Table, TableColumn,
    VersionMenu,
};

/// What Sites says.
#[derive(Debug, PartialEq)]
pub struct SitesView<'a> {
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
pub enum List<'a> {
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
pub struct Matches {
    /// Their indexes in the model's list; none while the search is empty
    /// and every site shows.
    found: Option<Vec<usize>>,
    count: usize,
}

impl Matches {
    pub const fn len(&self) -> usize {
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
pub struct SiteRow<'a> {
    pub name: &'a str,
    pub root: &'a Path,
    pub php: PhpCell<'a>,
    /// Served over HTTPS.
    pub https: bool,
}

/// The version menu of a row.
#[derive(Debug, PartialEq)]
pub struct PhpCell<'a> {
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
pub enum Reason<'a> {
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
    pub fn hint(&self, shown: &str) -> String {
        match self {
            Self::Site => strings::php_hint_site(shown),
            Self::File(path) => strings::php_hint_file(shown, path),
            Self::Default => strings::php_hint_default(shown),
            Self::Files => strings::PHP_HINT_FILES.to_owned(),
            Self::NoPhpInstalled => strings::NO_PHP_YET.to_owned(),
            Self::NoMatch => strings::PHP_HINT_NO_MATCH.to_owned(),
        }
    }

    pub fn text(&self) -> String {
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
pub struct LinkView<'a> {
    pub folder: &'a Path,
    pub name: &'a str,
    /// The site the name would replace, if the name is taken.
    pub replaces: Option<&'a Site>,
}

#[derive(Debug, PartialEq)]
pub struct HostsView {
    /// The sentence that names the sites the hosts file lacks.
    pub waiting: Option<String>,
    /// Why "Add to /etc/hosts" cannot be pressed now.
    pub off: Option<&'static str>,
    /// The last try failed: its error.
    pub failed: Option<String>,
}

/// Decides what Sites says. Pure: the same model gives the same view.
/// `picking`: a folder dialog is open.
pub fn view(model: &Model, picking: bool) -> SitesView<'_> {
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
        parked: &model.sites.parked,
        home: model.home.as_deref(),
        forget_off: (model.is_connected() && !model.can_remove())
            .then_some(strings::REMOVE_BUSY_HINT),
        skipped: &model.sites.skipped,
    }
}

/// "Unlinking shop.test…", while the daemon removes a site or a folder.
pub(super) fn removing_line(model: &Model) -> Option<String> {
    match model.removing() {
        Some(removal @ (Removal::Unlink { .. } | Removal::Forget { .. })) => {
            Some(strings::removing(removal))
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
    let all = &model.sites.sites;
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
    pub fn row(&self, n: usize) -> Option<SiteRow<'a>> {
        let List::Rows(matches) = &self.list else {
            return None;
        };
        let model = self.model.0;
        let site = model.sites.sites.get(matches.site(n)?)?;
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
    let failed = match &model.hosts_sync {
        HostsSync::Failed(code) => Some(strings::error_text(code)),
        HostsSync::Idle | HostsSync::Running => None,
    };
    let waiting = (model.hosts.used && !model.hosts.missing.is_empty())
        .then(|| strings::hosts_waiting(&model.hosts.missing));
    if waiting.is_none() && failed.is_none() {
        return None;
    }
    let busy = model.hosts_sync == HostsSync::Running || model.setup == Setup::Running;
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
pub fn show(ui: &mut Ui, view: &SitesView<'_>, theme: &Theme) -> Vec<Action> {
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

/// A sentence in a card, in place of a table.
pub(super) fn message(ui: &mut Ui, theme: &Theme, text: &str) {
    padded_card(ui, |ui| {
        ui.vertical_centered(|ui| {
            let text = RichText::new(text)
                .font(theme.text.body.font_id())
                .color(theme.color.ink2);
            ui.add(Label::new(text).selectable(false));
        });
    });
}

/// A line of `caption` text in `mute`.
pub(super) fn mute(ui: &mut Ui, theme: &Theme, text: &str) {
    let text = RichText::new(text)
        .font(theme.text.caption.font_id())
        .color(theme.color.mute);
    ui.add(Label::new(text).selectable(false));
}

/// How a line of status says what kind it is, besides its words (R8).
#[derive(Clone, Copy)]
pub(super) enum Mark {
    Error,
    /// Amber (tokens.md, Color: states).
    Warning,
}

/// An icon and a sentence that wraps, then whatever `add` puts after them.
pub(super) fn status_line(
    ui: &mut Ui,
    theme: &Theme,
    mark: Mark,
    text: &str,
    add: impl FnOnce(&mut Ui),
) {
    let (icon, color) = match mark {
        Mark::Error => crate::widgets::notice::mark(theme, NoticeKind::Error),
        Mark::Warning => (Icon::Alert, theme.section(Section::Dumps).fg),
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = theme.space.s2;
        ui.add(Glyph::new(icon).size(theme.size.icon_inline).color(color));
        let words = RichText::new(text)
            .font(theme.text.body_sm.font_id())
            .color(theme.color.ink);
        ui.add(Label::new(words).selectable(false));
        add(ui);
    });
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
        let title = RichText::new(strings::link_folder(&strings::home_path(link.folder, home)))
            .font(theme.text.body_sm.font_id())
            .color(theme.color.ink);
        let title = ui.add(Label::new(title).selectable(false));
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
    let text = RichText::new(word)
        .font(theme.text.body_sm.font_id())
        .color(color);
    ui.add(Label::new(text).selectable(false));
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
                let text = RichText::new(&reason)
                    .font(theme.text.body_sm.font_id())
                    .color(theme.color.ink2);
                ui.add(Label::new(text).truncate().selectable(false))
                    .on_hover_text(&reason);
            });
        },
    );
}

/// A folder's path in the mono face, as `~/…` under the home folder, cut
/// short at the end of the column. The tooltip has the full path.
pub(super) fn path(ui: &mut Ui, theme: &Theme, folder: &Path, home: Option<&Path>) {
    let shown = strings::home_path(folder, home);
    let text = RichText::new(&shown)
        .font(theme.text.mono_sm.font_id())
        .color(theme.color.ink);
    let response = ui.add(Label::new(text).truncate().selectable(false));
    // Shortened for the eye, or cut short by the column: the whole path is
    // one hover away.
    response.on_hover_text(folder.display().to_string());
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use eframe::egui::{self, Event, Pos2, RawInput, pos2, vec2};
    use marsa_core::runtimes::Runtime;
    use marsa_core::types::{
        Health as DaemonHealth, HostsStatus, RuntimeList, RuntimeVersions, SiteKind, SiteList,
    };
    use semver::Version;

    use super::*;
    use crate::model::{Update, apply};
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

    /// `~` and the parts after it, with the separator of the system.
    fn tilde(parts: &[&str]) -> String {
        let sep = std::path::MAIN_SEPARATOR;
        parts
            .iter()
            .fold(String::from("~"), |text, part| format!("{text}{sep}{part}"))
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
        model.sites.parked = vec![PathBuf::from("/code")];
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
        model.hosts_sync = HostsSync::Failed("hosts_failed".into());
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
        model.sites.parked = vec![PathBuf::from("/code"), PathBuf::from("/work")];
        model.sites.skipped = vec![SkippedFolder {
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
        model.sites.parked = vec![home.join("Code"), abs("/srv/www")];
        model.sites.skipped = vec![SkippedFolder {
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
        model.sites.parked = vec![PathBuf::from("/code"), PathBuf::from("/work")];
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
                .filter(|shape| matches!(shape, egui::Shape::Rect(rect) if rect.fill == LIGHT.color.accent))
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
}
