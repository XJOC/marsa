//! Runtimes (layout.md, Screens): one screen for every runtime.
//! A list of runtimes on the start side, each with its tile; the details of
//! the chosen one: its default version on the large version buttons, the
//! Installed table with the sites that run each version and a menu (⋯) for
//! Uninstall…, "Install a version…" that opens a dialog, and the extra
//! sections only some runtimes have (Xdebug for PHP).
//!
//! The screen draws [`Descriptor`]s, never a runtime by name. [`descriptor`]
//! is the one place that knows what PHP has more than the others, and
//! [`Descriptor::plain`] is all a runtime needs that core adds later (Python,
//! Go): the list comes from [`Runtime::ALL`], so a new variant shows up with
//! its name, two letters, and an install button, with no change here. A
//! database or cache server (an [`Kind::Engine`]: Postgres, MySQL, Valkey) is
//! left out of the list until the screens for them exist; they are named in
//! [`kind_for`], so they stay out when core adds them.
//!
//! Like Home and Sites it is two functions. [`view`] reads the [`Model`] and
//! decides what the screen says: pure, so the tests assert it without a
//! window. [`show`] draws that [`RuntimesView`] with the widgets and returns
//! what the user did as [`Action`]s (code.md, State). Nothing here reaches the
//! network: an install is a job in the model's queue, and the daemon's
//! refusals come back as notices in the shell's bar.

use eframe::egui::{Context, Id, Key, Label, RichText, Ui, vec2};
use marsa_core::runtimes::Runtime;
use semver::Version;

use super::home::{InstallBar, install_bar};
use super::sites::{Mark, message, mute, status_line};
use crate::model::{self, Action, Model, Removal, Route};
use crate::strings;
use crate::theme::page::{RUNTIMES_LIST_WIDTH, RUNTIMES_TAG_COLUMN, RUNTIMES_VERSION_COLUMN};
use crate::theme::{Direction, Face, Section, TextToken, Theme};
use crate::widgets::table::{empty_state_with, padded_card};
use crate::widgets::{
    Answer, Button, ConfirmDialog, Field, Icon, ListItem, Menu, MenuItem, Progress, RowHeight,
    Switch, Table, TableColumn, VersionPick, VersionPicker,
};

/// What the screen knows about one runtime, in one place. The list and the
/// details are drawn from descriptors, never from a `match` on [`Runtime`]:
/// [`Descriptor::plain`] describes any runtime, and only one with a default
/// version, site counts, or an extra section has an entry in [`descriptor`].
/// [Laravel: a config array the view loops over.]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    /// The key of the runtime in the model and in the daemon's commands.
    pub runtime: Runtime,
    /// What it is: only languages and tools are listed here.
    pub kind: Kind,
    pub name: &'static str,
    /// The letters of its tile.
    pub monogram: String,
    /// The colors of its tile.
    pub section: Section,
    /// It has a default version the user picks (`model::has_default`);
    /// otherwise Marsa uses the newest installed version.
    pub default: bool,
    /// Sites run on its versions, so the Installed table counts them.
    pub sites: bool,
    /// The sections after the Installed table.
    pub extras: &'static [Extra],
    /// A version to show as an example in the install dialog.
    pub example: &'static str,
}

/// What a runtime is. The Runtimes screen lists languages and tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A language Marsa runs your code with: PHP, Node.js, later Python, Go.
    Language,
    /// A program that serves or builds: Composer, Caddy.
    Tool,
    /// A database or cache server (Postgres, MySQL, Valkey). It belongs
    /// under Databases and Services, so it is not listed here.
    Engine,
}

impl Kind {
    /// Whether the Runtimes screen lists it.
    pub const fn listed(self) -> bool {
        matches!(self, Self::Language | Self::Tool)
    }
}

/// The kind of the runtime in the folder `folder`. The Phase 2 engines are
/// named now, so they stay out of the list when core adds them; any other
/// name, known or not, is listed: a new language is the likeliest.
pub fn kind_for(folder: &str) -> Kind {
    match folder {
        "composer" | "caddy" => Kind::Tool,
        "postgres" | "mysql" | "valkey" => Kind::Engine,
        _ => Kind::Language,
    }
}

/// A section of a runtime's details that only some runtimes have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extra {
    /// The Xdebug switch (PHP).
    Xdebug,
}

impl Descriptor {
    /// What any runtime is until it asks for more: the name core gives it
    /// (its folder `folder`, as `strings::runtime_name_for` has it), a kind
    /// from [`kind_for`], the first two letters of that name on its tile, the
    /// Runtimes colors, no default, no site counts, and no extra section.
    pub fn plain(runtime: Runtime, folder: &'static str) -> Self {
        let name = strings::runtime_name_for(folder);
        Self {
            runtime,
            kind: kind_for(folder),
            name,
            monogram: strings::monogram_for(folder, name),
            section: Section::Runtimes,
            default: false,
            sites: false,
            extras: &[],
            example: strings::example_version_for(folder),
        }
    }
}

/// The descriptor of `runtime`: the plain one, with what the runtime has more.
pub fn descriptor(runtime: Runtime) -> Descriptor {
    let plain = Descriptor {
        default: model::has_default(runtime),
        ..Descriptor::plain(runtime, runtime.dir_name())
    };
    match runtime {
        Runtime::Php => Descriptor {
            sites: true,
            extras: &[Extra::Xdebug],
            ..plain
        },
        // Node, Composer, Caddy, and every runtime core adds later: the plain
        // descriptor. A `_` arm, so a new variant builds without a change.
        _ => plain,
    }
}

/// The runtimes of [`Runtime::ALL`], in its order, that the screen lists:
/// the languages and the tools.
pub fn descriptors() -> Vec<Descriptor> {
    listed(Runtime::ALL.into_iter().map(descriptor))
}

/// `all` without the engines.
pub fn listed(all: impl IntoIterator<Item = Descriptor>) -> Vec<Descriptor> {
    all.into_iter().filter(|d| d.kind.listed()).collect()
}

/// What the screen says.
#[derive(Debug, PartialEq)]
pub enum RuntimesView {
    /// The runtimes were not read: no daemon, or no answer yet.
    Unread,
    /// Boxed: a screen is much larger than a message.
    Read(Box<Read>),
}

/// The screen once the daemon has listed the runtimes.
#[derive(Debug, PartialEq)]
pub struct Read {
    /// One for each descriptor, in order.
    pub entries: Vec<Entry>,
    /// The index of the chosen entry.
    pub chosen: usize,
    pub details: Details,
    pub status: Status,
}

/// One runtime in the list.
#[derive(Debug, PartialEq)]
pub struct Entry {
    pub runtime: Runtime,
    pub name: &'static str,
    pub monogram: String,
    pub section: Section,
    /// The line under the name: its default and how many versions.
    pub summary: String,
}

/// The chosen runtime.
#[derive(Debug, PartialEq)]
pub struct Details {
    pub runtime: Runtime,
    pub name: &'static str,
    pub version: VersionSection,
    /// The installed versions, newest first.
    pub rows: Vec<VersionRow>,
    /// The table has a "Used by" column.
    pub sites_column: bool,
    /// Why "Install a version…" cannot be pressed now.
    pub install_off: Option<&'static str>,
    /// The extra section of Xdebug, for a runtime that has it.
    pub xdebug: Option<XdebugView>,
}

/// The part above the Installed table.
#[derive(Debug, PartialEq)]
pub enum VersionSection {
    /// Nothing is installed: there is nothing to pick.
    Empty,
    /// The large version buttons: the default.
    Default(DefaultPicker),
    /// A runtime with no default: one sentence, and no buttons.
    Newest { hint: String },
}

#[derive(Debug, PartialEq)]
pub struct DefaultPicker {
    /// "Default PHP": the heading, and the name of the group for a screen
    /// reader.
    pub heading: String,
    /// The installed versions, oldest first.
    pub choices: Vec<VersionChoice>,
    /// The index of the version the default picks now.
    pub chosen: Option<usize>,
    /// The name of the dashed "+": "Install another PHP version".
    pub install: String,
}

#[derive(Debug, PartialEq)]
pub struct VersionChoice {
    pub version: Version,
    /// `8.4` for the newest patch of a line, the whole version for an older
    /// one: the text the daemon would save.
    pub label: String,
}

/// One installed version in the table.
#[derive(Debug, PartialEq)]
pub struct VersionRow {
    pub version: Version,
    /// The whole version: `8.4.27`.
    pub text: String,
    /// The sites that run it (runtimes with site counts only).
    pub used: Option<String>,
    /// "Default", or "In use" for the newest version of a runtime with no
    /// default.
    pub tag: Option<&'static str>,
    /// The name of its menu button.
    pub more: String,
    /// Why "Uninstall…" is off in its menu: the default, sites set to it, or
    /// another job that runs. The daemon has the last word after the
    /// confirmation.
    pub uninstall_off: Option<String>,
}

#[derive(Debug, PartialEq)]
pub struct XdebugView {
    pub on: bool,
    /// Why the switch cannot be pressed now.
    pub off: Option<&'static str>,
    /// What it does now, and why the last read or change failed.
    pub lines: Vec<String>,
    pub error: Option<String>,
}

/// What an install that runs and an uninstall that waits say, above the
/// sections. How they end is a notice in the shell's bar (`shell::notices`).
#[derive(Debug, PartialEq)]
pub struct Status {
    /// An install runs.
    pub progress: Option<InstallBar>,
    /// "Uninstalling PHP 8.4.27…", while the daemon removes it.
    pub removing: Option<String>,
}

#[derive(Debug, PartialEq)]
pub struct InstallDialog {
    pub runtime: Runtime,
    pub title: String,
    /// What to type: a version such as 8.4, or nothing.
    pub body: String,
    /// The field's text, as typed.
    pub text: String,
    /// Why the text is wrong.
    pub error: Option<String>,
}

/// Decides what Runtimes says. Pure: the same model gives the same view.
pub fn view(model: &Model) -> RuntimesView {
    let table = descriptors();
    let chosen = table
        .iter()
        .position(|d| d.runtime == model.chosen_runtime())
        .unwrap_or(0);
    build(model, &table, chosen)
}

/// [`view`] over the descriptors `table`, with the entry `chosen` shown.
/// The tests give it a runtime the daemon does not have yet.
pub fn build(model: &Model, table: &[Descriptor], chosen: usize) -> RuntimesView {
    let chosen = chosen.min(table.len().saturating_sub(1));
    let Some(shown) = table.get(chosen) else {
        return RuntimesView::Unread;
    };
    if model.runtimes.is_empty() {
        return RuntimesView::Unread;
    }
    RuntimesView::Read(Box::new(Read {
        entries: table.iter().map(|d| entry(model, d)).collect(),
        chosen,
        details: details(model, shown),
        status: status(model),
    }))
}

fn entry(model: &Model, d: &Descriptor) -> Entry {
    Entry {
        runtime: d.runtime,
        name: d.name,
        monogram: d.monogram.clone(),
        section: d.section,
        summary: summary(model, d),
    }
}

/// "Default 8.4, 4 installed", or "2.8.4, newest is used".
fn summary(model: &Model, d: &Descriptor) -> String {
    let installed = model.installed(d.runtime);
    let Some(newest) = installed.last() else {
        return strings::NONE_INSTALLED.to_owned();
    };
    if !d.default {
        return strings::summary_newest(newest);
    }
    match model.default_choice(d.runtime) {
        Some(default) => {
            let text = model::version_text(d.runtime, &default, installed);
            strings::summary_default(&text, installed.len())
        }
        None => strings::summary_no_default(installed.len()),
    }
}

fn details(model: &Model, d: &Descriptor) -> Details {
    let installed = model.installed(d.runtime);
    Details {
        runtime: d.runtime,
        name: d.name,
        version: version_section(model, d, installed),
        rows: installed
            .iter()
            .rev()
            .map(|version| row(model, d, version, installed))
            .collect(),
        sites_column: d.sites,
        install_off: (!model.can_install()).then_some(strings::INSTALL_BUSY_HINT),
        xdebug: d.extras.contains(&Extra::Xdebug).then(|| xdebug(model)),
    }
}

fn version_section(model: &Model, d: &Descriptor, installed: &[Version]) -> VersionSection {
    if installed.is_empty() {
        return VersionSection::Empty;
    }
    if !d.default {
        return VersionSection::Newest {
            hint: strings::newest_is_used(d.name),
        };
    }
    let choices = installed
        .iter()
        .map(|version| VersionChoice {
            version: version.clone(),
            label: model::version_text(d.runtime, version, installed),
        })
        .collect();
    let chosen = model
        .default_choice(d.runtime)
        .and_then(|default| installed.iter().position(|version| *version == default));
    VersionSection::Default(DefaultPicker {
        heading: strings::default_heading(d.name),
        choices,
        chosen,
        install: strings::install_another_label(d.name),
    })
}

fn row(model: &Model, d: &Descriptor, version: &Version, installed: &[Version]) -> VersionRow {
    let tag = if d.default {
        (model.default_choice(d.runtime).as_ref() == Some(version)).then_some(strings::TAG_DEFAULT)
    } else {
        (installed.last() == Some(version)).then_some(strings::TAG_IN_USE)
    };
    VersionRow {
        version: version.clone(),
        text: version.to_string(),
        used: d
            .sites
            .then(|| strings::sites_text(&model.sites_running(d.runtime, version))),
        tag,
        more: strings::more_for(d.name, version),
        uninstall_off: uninstall_off(model, d, version, installed),
    }
}

/// Why the version cannot be uninstalled now, as far as the window can tell:
/// the daemon refuses the default (unless it is the only version) and a
/// version some site is set to. Another job that runs comes last: it ends.
fn uninstall_off(
    model: &Model,
    d: &Descriptor,
    version: &Version,
    installed: &[Version],
) -> Option<String> {
    let is_default = d.default && model.default_choice(d.runtime).as_ref() == Some(version);
    if is_default && installed.len() > 1 {
        return Some(strings::uninstall_off_default(d.name));
    }
    let sites = model.sites_set_to(d.runtime, version);
    if !sites.is_empty() {
        return Some(strings::uninstall_off_sites(d.name, &sites));
    }
    (!model.can_uninstall()).then(|| strings::UNINSTALL_BUSY_HINT.to_owned())
}

fn xdebug(model: &Model) -> XdebugView {
    let lines = model.xdebug.as_ref().map_or_else(Vec::new, |state| {
        strings::xdebug_lines(state, model.xdebug_pending())
    });
    let off = if model.xdebug.is_none() {
        Some(strings::XDEBUG_UNREAD)
    } else if model.xdebug_pending().is_some() {
        Some(strings::XDEBUG_BUSY)
    } else {
        None
    };
    XdebugView {
        on: model.xdebug_on().unwrap_or(false),
        off,
        lines,
        error: model.xdebug_notice.as_deref().map(strings::error_text),
    }
}

fn status(model: &Model) -> Status {
    // An install that ended is a notice, not a line of this screen.
    let progress = model
        .install_view()
        .filter(|install| !install.step.is_over())
        .map(install_bar);
    Status {
        progress,
        removing: match model.removing() {
            Some(removal @ Removal::Uninstall { .. }) => Some(strings::removing(removal)),
            _ => None,
        },
    }
}

/// The install dialog, when it is open. The shell draws it over any screen
/// ([`show_install_dialog`]), because Home and a site's page open it too, so
/// it is not part of [`RuntimesView`].
pub fn install_dialog_view(model: &Model) -> Option<InstallDialog> {
    let runtime = model.install_dialog?;
    Some(dialog(model, &descriptor(runtime)))
}

fn dialog(model: &Model, d: &Descriptor) -> InstallDialog {
    InstallDialog {
        runtime: d.runtime,
        title: strings::install_dialog_title(d.name),
        body: strings::version_field_hint(d.name, d.example),
        text: model.install_text(d.runtime).to_owned(),
        error: model.runtimes_notice.as_deref().map(strings::error_text),
    }
}

/// Draws `view`. Returns what the user did.
pub fn show(ui: &mut Ui, view: &RuntimesView, theme: &Theme) -> Vec<Action> {
    let mut actions = Vec::new();
    ui.spacing_mut().item_spacing.y = theme.space.section_gap;
    super::header(ui, theme, Section::Runtimes, strings::RUNTIMES, |_| {});
    match view {
        RuntimesView::Unread => message(ui, theme, strings::NO_RUNTIMES),
        RuntimesView::Read(read) => {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = theme.space.section_gap;
                list(ui, theme, read, &mut actions);
                ui.vertical(|ui| details_of(ui, theme, read, &mut actions));
            });
        }
    }
    actions
}

/// The runtimes on the start side. A click on one shows its details.
fn list(ui: &mut Ui, theme: &Theme, read: &Read, actions: &mut Vec<Action>) {
    ui.vertical(|ui| {
        ui.set_width(RUNTIMES_LIST_WIDTH);
        ui.spacing_mut().item_spacing.y = theme.space.s1;
        for (index, entry) in read.entries.iter().enumerate() {
            let item = ListItem::new(&entry.monogram, entry.name, &entry.summary)
                .section(entry.section)
                .selected(index == read.chosen);
            if ui.add(item).clicked() && index != read.chosen {
                actions.push(Action::Navigate(Route::Runtimes(entry.runtime)));
            }
        }
    });
}

/// What the chosen runtime has: the progress, the default, the Installed
/// table, and its extras.
fn details_of(ui: &mut Ui, theme: &Theme, read: &Read, actions: &mut Vec<Action>) {
    let details = &read.details;
    ui.spacing_mut().item_spacing.y = theme.space.section_gap;
    status_block(ui, theme, &read.status);
    match &details.version {
        VersionSection::Empty => {}
        VersionSection::Default(picker) => default_picker(ui, theme, details, picker, actions),
        VersionSection::Newest { hint } => {
            super::heading(ui, theme, strings::IN_USE_HEADING, Some(hint));
        }
    }
    installed(ui, theme, details, actions);
    if let Some(x) = &details.xdebug {
        xdebug_card(ui, theme, x, actions);
    }
}

/// The progress of an install, and the wait for an uninstall.
fn status_block(ui: &mut Ui, theme: &Theme, status: &Status) {
    if let Some(bar) = &status.progress {
        padded_card(ui, |ui| {
            ui.add(Progress::new(&bar.words, bar.fraction));
        });
    }
    if let Some(text) = &status.removing {
        mute(ui, theme, text);
    }
}

/// "Default PHP", and the large version buttons of the versions installed.
fn default_picker(
    ui: &mut Ui,
    theme: &Theme,
    details: &Details,
    picker: &DefaultPicker,
    actions: &mut Vec<Action>,
) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = theme.space.group_gap_max;
        super::heading(
            ui,
            theme,
            &picker.heading,
            Some(strings::DEFAULT_RUNTIME_HINT),
        );
        let versions: Vec<(usize, &str)> = picker
            .choices
            .iter()
            .enumerate()
            .map(|(index, choice)| (index, choice.label.as_str()))
            .collect();
        let mut shown =
            VersionPicker::new(&picker.heading, &versions, picker.chosen).install(&picker.install);
        if let Some(reason) = details.install_off {
            shown = shown.install_disabled(reason);
        }
        let shown = shown.show(ui);
        match shown.inner {
            Some(VersionPick::Version(index)) => {
                if let Some(choice) = picker.choices.get(index) {
                    actions.push(Action::SetDefault {
                        runtime: details.runtime,
                        version: choice.version.clone(),
                    });
                }
            }
            Some(VersionPick::Install) => actions.push(Action::AskInstall(details.runtime)),
            None => {}
        }
    });
}

/// What a row's menu (⋯) can do.
#[derive(Clone, Copy)]
enum RowMenu {
    Uninstall,
}

/// "Installed" with "Install a version…" at its end, and the table of
/// versions; or, with nothing installed, the empty state.
fn installed(ui: &mut Ui, theme: &Theme, details: &Details, actions: &mut Vec<Action>) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = theme.space.group_gap_max;
        let off = details.install_off;
        // The heading and the button share one line as high as a control.
        let line = vec2(ui.available_width(), theme.size.control);
        ui.allocate_ui_with_layout(line, Direction::CURRENT.row(), |ui| {
            super::heading(ui, theme, strings::INSTALLED, None);
            if details.rows.is_empty() {
                return;
            }
            ui.with_layout(Direction::CURRENT.row_from_end(), |ui| {
                let button = Button::secondary(strings::INSTALL_A_VERSION)
                    .disabled_if(off.is_some(), off.unwrap_or_default());
                let response = ui.add(button);
                if response.clicked() {
                    actions.push(Action::AskInstall(details.runtime));
                }
                if off.is_none() {
                    response.on_hover_text(strings::INSTALL_HINT);
                }
            });
        });
        if details.rows.is_empty() {
            let sentence = strings::no_runtime_installed(details.name);
            let label = strings::install_runtime(details.name);
            let button =
                Button::primary(&label).disabled_if(off.is_some(), off.unwrap_or_default());
            if empty_state_with(ui, &sentence, button).clicked() {
                actions.push(Action::AskInstall(details.runtime));
            }
            return;
        }
        versions_table(ui, theme, details, actions);
    });
}

/// The versions, newest first: the version, the sites that run it, its tag,
/// and the menu with Uninstall….
fn versions_table(ui: &mut Ui, theme: &Theme, details: &Details, actions: &mut Vec<Action>) {
    let version = if details.sites_column {
        TableColumn::fixed(strings::COLUMN_VERSION, RUNTIMES_VERSION_COLUMN)
    } else {
        TableColumn::fill(strings::COLUMN_VERSION)
    };
    let used = TableColumn::fill(strings::COLUMN_USED_BY);
    let status = TableColumn::fixed(strings::COLUMN_STATUS, RUNTIMES_TAG_COLUMN);
    let more = TableColumn::fixed(strings::COLUMN_MORE, theme.size.icon_button);
    let columns: Vec<TableColumn<'_>> = if details.sites_column {
        vec![version, used, status, more]
    } else {
        vec![version, status, more]
    };
    // Each runtime has its own table state: its columns differ.
    let table = Table::new(details.runtime.dir_name(), strings::INSTALLED, &columns)
        .rows(RowHeight::OneLine)
        .headerless();
    table.show(ui, details.rows.len(), |row| {
        let version = &details.rows[row.index()];
        row.cell(|ui| {
            let text = RichText::new(&version.text)
                .font(theme.text.body_strong.font_id())
                .color(theme.color.ink);
            ui.add(Label::new(text).selectable(false));
        });
        if let Some(used) = &version.used {
            row.cell(|ui| {
                let text = RichText::new(used)
                    .font(theme.text.body_sm.font_id())
                    .color(theme.color.ink2);
                ui.add(Label::new(text).truncate().selectable(false))
                    .on_hover_text(used);
            });
        }
        row.cell(|ui| {
            if let Some(tag) = version.tag {
                let token = TextToken {
                    size: theme.text.caption.size,
                    face: Face::Medium,
                };
                let text = RichText::new(tag)
                    .font(token.font_id())
                    .color(theme.color.accent);
                ui.add(Label::new(text).selectable(false));
            }
        });
        row.cell(|ui| {
            let button = ui.add(Button::icon(Icon::More, &version.more));
            let items = [MenuItem::new(RowMenu::Uninstall, strings::UNINSTALL_BUTTON)
                .danger()
                .hint(strings::UNINSTALL_HINT)
                .disabled_if(version.uninstall_off.as_deref())];
            if Menu::new(&items).from_end().show(&button).is_some() {
                actions.push(Action::AskUninstall {
                    runtime: details.runtime,
                    version: version.version.clone(),
                });
            }
        });
    });
}

/// The Xdebug switch, with what it does under it.
fn xdebug_card(ui: &mut Ui, theme: &Theme, x: &XdebugView, actions: &mut Vec<Action>) {
    padded_card(ui, |ui| {
        ui.spacing_mut().item_spacing.y = theme.space.s2;
        let switch = Switch::new(x.on, strings::XDEBUG)
            .disabled_if(x.off.is_some(), x.off.unwrap_or_default());
        if ui.add(switch).clicked() {
            actions.push(Action::ToggleXdebug);
        }
        mute(ui, theme, strings::XDEBUG_CAPTION);
        for line in &x.lines {
            mute(ui, theme, line);
        }
        if let Some(error) = &x.error {
            status_line(ui, theme, Mark::Error, error, |_| {});
        }
    });
}

/// The install dialog over the window: a version field and Install. Enter in
/// the field installs; Escape and Cancel close the dialog. The shell draws it
/// after the screen, so it covers any screen.
pub fn show_install_dialog(ctx: &Context, dialog: &InstallDialog) -> Vec<Action> {
    let mut actions = Vec::new();
    let id = Id::unique(("install_version", dialog.runtime.dir_name()));
    let mut text = dialog.text.clone();
    let mut entered = false;
    let asked = ConfirmDialog::new(
        "install_version",
        &dialog.title,
        &dialog.body,
        strings::INSTALL_BUTTON,
    )
    .field_first()
    .show_with(ctx, |ui, new| {
        let field = Field::new(&mut text, strings::VERSION_FIELD_LABEL)
            .hint(strings::VERSION_FIELD_EMPTY)
            .error_if(dialog.error.as_deref())
            .id(id);
        let response = ui.add(field);
        if new {
            response.request_focus();
        }
        entered = response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
    });
    if text != dialog.text {
        actions.push(Action::SetInstallVersion {
            runtime: dialog.runtime,
            text,
        });
        // The next frame shows the error, or none, for the new text.
        ctx.request_repaint();
    }
    match asked {
        Some(Answer::Confirm) => actions.push(Action::SubmitInstall(dialog.runtime)),
        Some(Answer::Cancel) => actions.push(Action::CancelInstall),
        None if entered => actions.push(Action::SubmitInstall(dialog.runtime)),
        None => {}
    }
    actions
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use eframe::egui::{self, Pos2, pos2};
    use marsa_core::types::{
        Event, Health as DaemonHealth, InstallDone, InstallEvent, InstallProgress, InstallUpdate,
        RuntimeList, RuntimeVersions, Site, SiteKind, SiteList, Source, VersionInUse, XdebugState,
    };

    use super::*;
    use crate::bridge::Command;
    use crate::model::{Change, InstallJob, UninstallNotice, Update, apply};
    use crate::theme::LIGHT;
    use crate::widgets::harness::{Harness, Tree};

    fn v(text: &str) -> Version {
        text.parse().expect("a version")
    }

    fn connected() -> Model {
        let mut model = Model::default();
        model.update(Update::Connected(DaemonHealth {
            app_version: "0.0.0".into(),
            api_version: 1,
            pid: 7,
        }));
        model
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
            .filter(|r| kind_for(r.dir_name()).listed())
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
            Some(Command::Save(Change::Default { runtime: Runtime::Php, ref version })) if version == "8.3"
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
    fn the_only_version_can_be_uninstalled_though_it_is_the_default_and_the_daemon_judges_the_rest()
    {
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
        assert!(matches!(sent, Some(Command::SetXdebug(false))));
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
        assert!(
            model
                .apply_action(Action::SubmitInstall(Runtime::Php))
                .is_none()
        );
        assert_eq!(install_dialog_view(&model), None, "the dialog closes");
        assert_eq!(model.next_install(), Some(job(Runtime::Php, None)));

        let mut model = self::model();
        model.apply_action(Action::AskInstall(Runtime::Php));
        model.apply_action(Action::SetInstallVersion {
            runtime: Runtime::Php,
            text: " 8.5 ".into(),
        });
        model.apply_action(Action::SubmitInstall(Runtime::Php));
        assert_eq!(model.next_install(), Some(job(Runtime::Php, Some("8.5"))));
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
            model.confirm,
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
        model.uninstall_notice = Some(UninstallNotice::Done {
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
        // The engines are named before core has them: they are not listed.
        for folder in ["postgres", "mysql", "valkey"] {
            let engine = Descriptor::plain(Runtime::Caddy, folder);
            assert_eq!(engine.kind, Kind::Engine, "{folder}");
            assert!(!engine.kind.listed());
        }
        // A name nobody knows is shown, as a language.
        let unknown = Descriptor::plain(Runtime::Caddy, "python");
        assert_eq!(unknown.kind, Kind::Language);
        let all = [
            descriptor(Runtime::Php),
            Descriptor::plain(Runtime::Caddy, "postgres"),
            Descriptor::plain(Runtime::Caddy, "python"),
            Descriptor::plain(Runtime::Caddy, "mysql"),
            Descriptor::plain(Runtime::Caddy, "valkey"),
        ];
        let shown: Vec<&str> = listed(all).iter().map(|d| d.name).collect();
        assert_eq!(shown, ["PHP", "python"]);
    }

    // -- a runtime core does not have yet --

    /// a runtime that core adds later (Python, Go)
    /// is in the list, with its details and its install dialog, and no line
    /// of this screen changes. `Runtime::Composer` and `Runtime::Node` stand
    /// in for the keys of variants that do not exist yet, so the fakes have
    /// versions to show.
    #[test]
    fn a_runtime_core_adds_later_is_listed_and_detailed_with_no_screen_code() {
        let mut table = descriptors();
        table.push(Descriptor::plain(Runtime::Composer, "python"));
        table.push(Descriptor {
            default: true,
            ..Descriptor::plain(Runtime::Node, "go")
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
        asked.install_dialog = Some(Runtime::Composer);
        let dialog = dialog(&asked, &Descriptor::plain(Runtime::Composer, "python"));
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
        assert!(
            strings::XDEBUG_CAPTION.contains("PHP runs two to four times slower while it is on.")
        );
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
}
