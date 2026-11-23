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

use eframe::egui::{Context, Id, Key, Ui};
use marsa_core::runtimes::Runtime;
use semver::Version;

use super::home::{InstallBar, install_bar};
use super::lines::{Mark, message, mute, status_line};
use crate::model::NoticeSource;
use crate::model::{self, Action, Model, Removal, Route};
use crate::strings;
use crate::theme::page::{RUNTIMES_LIST_WIDTH, RUNTIMES_TAG_COLUMN, RUNTIMES_VERSION_COLUMN};
use crate::theme::{Face, Section, TextToken, Theme};
use crate::widgets::table::{empty_state_with, padded_card};
use crate::widgets::{
    Answer, Button, ConfirmDialog, Field, Icon, ListItem, Menu, MenuItem, Progress, RowHeight,
    Switch, Table, TableColumn, Text, VersionPick, VersionPicker, runtime_monogram,
};

/// What the screen knows about one runtime, in one place. The list and the
/// details are drawn from descriptors, never from a `match` on [`Runtime`]:
/// [`Descriptor::plain`] describes any runtime, and only one with a default
/// version, site counts, or an extra section has an entry in [`descriptor`].
/// [Laravel: a config array the view loops over.]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Descriptor {
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
pub(crate) enum Kind {
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
    pub(crate) const fn listed(self) -> bool {
        matches!(self, Self::Language | Self::Tool)
    }
}

/// The kind of `runtime`. The Phase 2 engines stay out of the list; a new
/// variant is a compile error here until it is placed.
pub(crate) fn kind_for(runtime: Runtime) -> Kind {
    match runtime {
        Runtime::Php | Runtime::Node => Kind::Language,
        Runtime::Composer | Runtime::Caddy => Kind::Tool,
        Runtime::Postgres | Runtime::Mysql | Runtime::Valkey | Runtime::Mailpit => Kind::Engine,
    }
}

/// A section of a runtime's details that only some runtimes have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Extra {
    /// The Xdebug switch (PHP).
    Xdebug,
}

impl Descriptor {
    /// What any runtime is until it asks for more: the name and the letters
    /// of its tile that `strings` and `widgets` give it, a kind from
    /// [`kind_for`], the Runtimes colors, no default, no site counts, and no
    /// extra section.
    pub(crate) fn plain(runtime: Runtime) -> Self {
        Self {
            runtime,
            kind: kind_for(runtime),
            name: strings::runtime_name(runtime),
            monogram: runtime_monogram(runtime).to_owned(),
            section: Section::Runtimes,
            default: false,
            sites: false,
            extras: &[],
            example: strings::example_version(runtime),
        }
    }
}

/// The descriptor of `runtime`: the plain one, with what the runtime has more.
pub(crate) fn descriptor(runtime: Runtime) -> Descriptor {
    let plain = Descriptor {
        default: model::has_default(runtime),
        ..Descriptor::plain(runtime)
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
pub(crate) fn descriptors() -> Vec<Descriptor> {
    listed(Runtime::ALL.into_iter().map(descriptor))
}

/// `all` without the engines.
pub(crate) fn listed(all: impl IntoIterator<Item = Descriptor>) -> Vec<Descriptor> {
    all.into_iter().filter(|d| d.kind.listed()).collect()
}

/// What the screen says.
#[derive(Debug, PartialEq)]
pub(crate) enum RuntimesView {
    /// The runtimes were not read: no daemon, or no answer yet.
    Unread,
    /// Boxed: a screen is much larger than a message.
    Read(Box<Read>),
}

/// The screen once the daemon has listed the runtimes.
#[derive(Debug, PartialEq)]
pub(crate) struct Read {
    /// One for each descriptor, in order.
    pub entries: Vec<Entry>,
    /// The index of the chosen entry.
    pub chosen: usize,
    pub details: Details,
    pub status: Status,
}

/// One runtime in the list.
#[derive(Debug, PartialEq)]
pub(crate) struct Entry {
    pub runtime: Runtime,
    pub name: &'static str,
    pub monogram: String,
    pub section: Section,
    /// The line under the name: its default and how many versions.
    pub summary: String,
}

/// The chosen runtime.
#[derive(Debug, PartialEq)]
pub(crate) struct Details {
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
pub(crate) enum VersionSection {
    /// Nothing is installed: there is nothing to pick.
    Empty,
    /// The large version buttons: the default.
    Default(DefaultPicker),
    /// A runtime with no default: one sentence, and no buttons.
    Newest { hint: String },
}

#[derive(Debug, PartialEq)]
pub(crate) struct DefaultPicker {
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
pub(crate) struct VersionChoice {
    pub version: Version,
    /// `8.4` for the newest patch of a line, the whole version for an older
    /// one: the text the daemon would save.
    pub label: String,
}

/// One installed version in the table.
#[derive(Debug, PartialEq)]
pub(crate) struct VersionRow {
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
pub(crate) struct XdebugView {
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
pub(crate) struct Status {
    /// An install runs.
    pub progress: Option<InstallBar>,
    /// "Uninstalling PHP 8.4.27…", while the daemon removes it.
    pub removing: Option<String>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct InstallDialog {
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
pub(crate) fn view(model: &Model) -> RuntimesView {
    let table = descriptors();
    let chosen = table
        .iter()
        .position(|d| d.runtime == model.chosen_runtime())
        .unwrap_or(0);
    build(model, &table, chosen)
}

/// [`view`] over the descriptors `table`, with the entry `chosen` shown.
/// The tests give it a runtime the daemon does not have yet.
pub(crate) fn build(model: &Model, table: &[Descriptor], chosen: usize) -> RuntimesView {
    let chosen = chosen.min(table.len().saturating_sub(1));
    let Some(shown) = table.get(chosen) else {
        return RuntimesView::Unread;
    };
    if model.session.runtimes.is_empty() {
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
    let lines = model
        .session
        .xdebug
        .as_ref()
        .map_or_else(Vec::new, |state| {
            strings::xdebug_lines(state, model.xdebug_pending())
        });
    let off = if model.session.xdebug.is_none() {
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
        error: model
            .session
            .notices
            .code(NoticeSource::Xdebug)
            .map(strings::error_text),
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
            Some(removal @ Removal::Uninstall { .. }) => Some(removal.progress()),
            _ => None,
        },
    }
}

/// The install dialog, when it is open. The shell draws it over any screen
/// ([`show_install_dialog`]), because Home and a site's page open it too, so
/// it is not part of [`RuntimesView`].
pub(crate) fn install_dialog_view(model: &Model) -> Option<InstallDialog> {
    let runtime = model.session.install_dialog?;
    Some(dialog(model, &descriptor(runtime)))
}

fn dialog(model: &Model, d: &Descriptor) -> InstallDialog {
    InstallDialog {
        runtime: d.runtime,
        title: strings::install_dialog_title(d.name),
        body: strings::version_field_hint(d.name, d.example),
        text: model.install_text(d.runtime).to_owned(),
        error: model
            .session
            .notices
            .code(NoticeSource::Runtimes)
            .map(strings::error_text),
    }
}

/// Draws `view`. Returns what the user did.
pub(crate) fn show(ui: &mut Ui, view: &RuntimesView, theme: &Theme) -> Vec<Action> {
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
        super::section_header(ui, theme, strings::INSTALLED, None, |ui| {
            if details.rows.is_empty() {
                return;
            }
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
            ui.add(Text::new(
                &version.text,
                theme.text.body_strong,
                theme.color.ink,
            ));
        });
        if let Some(used) = &version.used {
            row.cell(|ui| {
                ui.add(Text::small(theme, used).truncate())
                    .on_hover_text(used);
            });
        }
        row.cell(|ui| {
            if let Some(tag) = version.tag {
                let token = TextToken {
                    size: theme.text.caption.size,
                    face: Face::Medium,
                };
                ui.add(Text::new(tag, token, theme.color.accent));
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
pub(crate) fn show_install_dialog(ctx: &Context, dialog: &InstallDialog) -> Vec<Action> {
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
mod tests;
