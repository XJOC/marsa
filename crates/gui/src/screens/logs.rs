//! Logs (layout.md, Screens): the output of one program at a
//! time, as a list of lines that fills the window. The source is the control
//! at the end of the header (Caddy, and each PHP pool the daemon lists); under
//! it, a field that filters the lines, a chip for each level, and Follow.
//! An error line has a tinted row. The lines are the model's own copy of what
//! the daemon sent (at most 5000 for each program), so nothing here asks the
//! network.
//!
//! Like Home and Sites it is two functions. [`view`] reads the [`Model`] and
//! decides what the screen says: pure, so the tests assert the filters
//! without a window. [`show`] draws that [`LogsView`] with the widgets and
//! returns what the user did as [`Action`]s (code.md, State). Only the rows
//! in view are drawn (`ScrollArea::show_rows`), so 5000 lines cost what a
//! screenful does.
//!
//! Follow keeps the newest line in view while lines come. Scrolling up
//! stops it, and the Follow chip says so; pressing the chip brings the user
//! back to the end and follows again.

use std::collections::VecDeque;

use eframe::egui::{ScrollArea, Ui, Vec2, vec2};
use marsa_core::types::{LogStream, ServiceId};

use super::lines::message;
use crate::logline::{self, LogLevel};
use crate::model::{self, Action, LogRow, LogSource, Model, Route};
use crate::strings;
use crate::theme::{Direction, Section, Theme, UNIT};
use crate::widgets::log_entry::width_for;
use crate::widgets::table::card;
use crate::widgets::{Button, Chip, Field, LogEntry, Menu, MenuItem, Segmented, level_color};

/// The most logs the segmented control holds (components.md): more go in a
/// menu.
const SEGMENTS: usize = 4;

/// What Logs says.
#[derive(Debug, PartialEq)]
pub(crate) struct LogsView<'a> {
    /// The log of each service the daemon lists, and the one on show.
    pub sources: Vec<SourceView>,
    /// The index in [`LogsView::sources`] of the log on show.
    pub current: usize,
    /// The three level chips, in order.
    pub levels: [LevelChip; 3],
    /// The text of the filter field, as typed.
    pub filter: &'a str,
    /// Follow is on: the newest line stays in view.
    pub follow: bool,
    pub body: Body<'a>,
}

/// One log the user may pick.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SourceView {
    pub source: LogSource,
    pub name: String,
}

/// A level chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LevelChip {
    pub level: LogLevel,
    /// Its lines show.
    pub on: bool,
}

/// The list, or what stands in its place.
#[derive(Debug, PartialEq)]
pub(crate) enum Body<'a> {
    /// No daemon, and nothing kept of this log.
    Offline,
    /// The program has written nothing yet: its name.
    Empty(String),
    /// The filters leave no line.
    NoMatch,
    Lines(Lines<'a>),
}

/// The lines the filters let through, oldest first.
#[derive(Debug, PartialEq)]
pub(crate) struct Lines<'a> {
    rows: &'a VecDeque<LogRow>,
    shown: Shown,
}

/// Which of the rows show. Nothing is filtered out most of the time, and
/// then no list of indexes is made.
#[derive(Debug, PartialEq, Eq)]
enum Shown {
    All(usize),
    Some(Vec<usize>),
}

impl<'a> Lines<'a> {
    pub(crate) fn len(&self) -> usize {
        match &self.shown {
            Shown::All(count) => *count,
            Shown::Some(indexes) => indexes.len(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The most columns of the mono face any line that shows takes.
    pub(crate) fn widest(&self) -> usize {
        let columns = |row: &LogRow| row.columns;
        match &self.shown {
            Shown::All(_) => self.rows.iter().map(columns).max().unwrap_or(0),
            Shown::Some(indexes) => indexes
                .iter()
                .map(|index| columns(&self.rows[*index]))
                .max()
                .unwrap_or(0),
        }
    }

    /// The `n`th line that shows. `n` is below [`Lines::len`].
    pub(crate) fn get(&self, n: usize) -> &'a LogRow {
        match &self.shown {
            Shown::All(_) => &self.rows[n],
            Shown::Some(indexes) => &self.rows[indexes[n]],
        }
    }
}

/// Decides what Logs says about the log of `source`. Pure: the same model
/// gives the same view.
pub(crate) fn view<'a>(model: &'a Model, source: &LogSource) -> LogsView<'a> {
    // Caddy's log first: the window always reads it, so it is there while
    // the daemon's list is not (a connection that dropped keeps the log).
    let others = model
        .session
        .services
        .iter()
        .map(|service| service.id)
        .filter(|id| *id != ServiceId::Caddy);
    let mut sources: Vec<SourceView> = std::iter::once(ServiceId::Caddy)
        .chain(others)
        .map(|id| SourceView {
            source: LogSource::of(id),
            name: model::service_name(id),
        })
        .collect();
    // The log the user is on stays pickable if its service went away.
    if !sources.iter().any(|view| view.source == *source) {
        sources.push(SourceView {
            source: source.clone(),
            name: source_name(source),
        });
    }
    let current = sources
        .iter()
        .position(|view| view.source == *source)
        .unwrap_or_default();
    LogsView {
        sources,
        current,
        levels: LogLevel::ALL.map(|level| LevelChip {
            level,
            on: !model.log_hidden.hides(level),
        }),
        filter: &model.log_filter,
        follow: !model.log_paused,
        body: body(model, source),
    }
}

/// What the source list calls the log of `source`.
fn source_name(source: &LogSource) -> String {
    match source {
        LogSource::Caddy => model::service_name(ServiceId::Caddy),
        LogSource::Service(id) => model::service_name(*id),
        LogSource::Site(site) => strings::site_log_source(site),
    }
}

fn body<'a>(model: &'a Model, source: &LogSource) -> Body<'a> {
    let rows = model.source_rows(source);
    if rows.is_empty() {
        return if model.is_connected() {
            Body::Empty(source_name(source))
        } else {
            Body::Offline
        };
    }
    let search = model.log_filter.trim();
    let shown = if model.log_hidden.is_empty() && search.is_empty() {
        Shown::All(rows.len())
    } else {
        let indexes = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| !model.log_hidden.hides(row.level) && holds(&row.text, search))
            .map(|(index, _)| index)
            .collect();
        Shown::Some(indexes)
    };
    let lines = Lines { rows, shown };
    if lines.is_empty() {
        Body::NoMatch
    } else {
        Body::Lines(lines)
    }
}

/// Whether `text` holds `search`, ignoring case. Compares bytes, which is
/// exact for ASCII and allocates nothing: it runs on every line, on every
/// frame while the user types.
fn holds(text: &str, search: &str) -> bool {
    let (text, search) = (text.as_bytes(), search.as_bytes());
    search.is_empty()
        || text
            .windows(search.len())
            .any(|window| window.eq_ignore_ascii_case(search))
}

/// Draws `view`. Returns what the user did.
pub(crate) fn show(ui: &mut Ui, view: &LogsView<'_>, theme: &Theme) -> Vec<Action> {
    let mut actions = Vec::new();
    ui.spacing_mut().item_spacing.y = theme.space.section_gap;
    super::header(ui, theme, Section::Logs, strings::LOGS, |ui| {
        sources(ui, view, &mut actions);
    });
    tools(ui, theme, view, &mut actions);
    match &view.body {
        Body::Offline => message(ui, theme, strings::LOGS_OFFLINE),
        Body::Empty(name) => message(ui, theme, &strings::log_empty(name)),
        Body::NoMatch => message(ui, theme, strings::LOG_NO_MATCH),
        Body::Lines(lines) => list(ui, theme, view, lines, &mut actions),
    }
    actions
}

/// The control that picks whose log shows: nothing for one log, a segmented
/// control for a few, and a menu for more.
fn sources(ui: &mut Ui, view: &LogsView<'_>, actions: &mut Vec<Action>) {
    let pick = |index: usize| {
        let source = view.sources[index].source.clone();
        Action::Navigate(Route::Logs(source))
    };
    match view.sources.len() {
        0 | 1 => {}
        2..=SEGMENTS => {
            let options: Vec<(usize, &str)> = view
                .sources
                .iter()
                .enumerate()
                .map(|(index, source)| (index, source.name.as_str()))
                .collect();
            let chosen = Segmented::new(strings::LOG_SOURCE, &options, view.current)
                .show(ui)
                .inner;
            actions.extend(chosen.map(pick));
        }
        _ => {
            let label = strings::log_source_menu(&view.sources[view.current].name);
            let button = ui.add(Button::secondary(&label));
            let items: Vec<MenuItem<'_, usize>> = view
                .sources
                .iter()
                .enumerate()
                .map(|(index, source)| MenuItem::new(index, &source.name))
                .collect();
            let chosen = Menu::new(&items).from_end().show(&button);
            actions.extend(chosen.filter(|index| *index != view.current).map(pick));
        }
    }
}

/// The filter field, a chip for each level, and Follow: one line, the field
/// taking what the chips leave.
fn tools(ui: &mut Ui, theme: &Theme, view: &LogsView<'_>, actions: &mut Vec<Action>) {
    let line = vec2(ui.available_width(), theme.size.control);
    ui.allocate_ui_with_layout(line, Direction::CURRENT.row_from_end(), |ui| {
        ui.spacing_mut().item_spacing.x = theme.space.s2;
        let follow = Chip::new(strings::FOLLOW, view.follow)
            .check()
            .hint(strings::FOLLOW_HINT);
        if ui.add(follow).clicked() {
            actions.push(Action::SetLogFollow(!view.follow));
        }
        // From the end, so the chips are added the other way round.
        for chip in view.levels.iter().rev() {
            let name = strings::log_level_name(chip.level);
            let widget = Chip::new(name, chip.on).square(level_color(theme, chip.level));
            if ui.add(widget).clicked() {
                actions.push(Action::ToggleLogLevel(chip.level));
            }
        }
        let width = ui.available_width();
        let mut text = view.filter.to_owned();
        let field = Field::search(&mut text, strings::FILTER_LINES)
            .hint(strings::FILTER_LINES)
            .width(width);
        let size = vec2(width, theme.size.control);
        let changed = ui.allocate_ui(size, |ui| ui.add(field).changed()).inner;
        if changed {
            actions.push(Action::SetLogFilter(text));
            // The list filters on the next frame: ask for it, since typing
            // is the only input there is.
            ui.ctx().request_repaint();
        }
    });
}

/// The lines in a card that fills the height that is left. Only the rows in
/// view are drawn, and the list holds a whole number of rows: at the end of
/// the log no row is cut at the top, and the space left over goes above and
/// under the rows. A line wider than the card is not cut: the list scrolls
/// sideways.
fn list(
    ui: &mut Ui,
    theme: &Theme,
    view: &LogsView<'_>,
    lines: &Lines<'_>,
    actions: &mut Vec<Action>,
) {
    let row = theme.size.log_row;
    card(ui, |ui| {
        // The rows touch.
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let room = ui.available_height();
        let fit = ((room - theme.space.s2 - theme.space.s2) / row)
            .floor()
            .max(UNIT);
        let height = fit * row;
        // Half of what is left over above the rows, half under them.
        let spare = 0.0_f32.midpoint(room - height);
        ui.add_space(spare);
        // A row is as wide as the card, or as the widest line if that is wider.
        let width = ui.available_width().max(width_for(ui, lines.widest()));
        let id = ui.make_persistent_id("log");
        // What the last frame saw: whether Follow was on, and how many
        // lines there were.
        let (was_following, last_count) = ui.data(|data| {
            data.get_temp::<(bool, usize)>(id)
                .unwrap_or((view.follow, lines.len()))
        });
        let mut area = ScrollArea::both()
            .id_salt(id)
            .auto_shrink(false)
            .max_height(height)
            .stick_to_bottom(view.follow);
        // Follow just came back: go to the end, once. Every frame would not
        // let the user scroll.
        if view.follow && !was_following {
            area = area.vertical_scroll_offset(lines.len() as f32 * row);
        }
        let out = area.show_rows(ui, row, lines.len(), |ui, range| {
            for n in range {
                entry(ui, lines.get(n), width);
            }
        });
        // The user scrolled away from the end: Follow is off now, and its
        // chip must say so.
        let at_end = out.state.clamped_offset().y + out.inner_rect.height()
            >= out.content_size.y - theme.size.border;
        if view.follow && !at_end {
            actions.push(Action::SetLogFollow(false));
        }
        // Lines came while following: the scroll area puts the end in view
        // after it drew, so draw once more.
        if view.follow && lines.len() != last_count {
            ui.ctx().request_repaint();
        }
        ui.data_mut(|data| data.insert_temp(id, (view.follow, lines.len())));
        ui.add_space(spare);
    });
}

/// One row, `width` wide: its level, and its message in the words of the
/// program. A line the program wrote to its standard output is drawn quiet.
fn entry(ui: &mut Ui, row: &LogRow, width: f32) {
    let message = logline::message(&row.text);
    let shown = logline::clipped(&message);
    let quiet = row.stream == LogStream::Stdout;
    ui.add(LogEntry::new(row.level, &shown, &row.text, width).quiet(quiet));
}

#[cfg(test)]
mod tests;
