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
use marsa_core::types::LogStream;

use super::lines::message;
use crate::logline::{self, LogLevel};
use crate::model::{self, Action, LOG_SERVICE, LogRow, LogSource, Model, Route};
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
pub struct LogsView<'a> {
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
pub struct SourceView {
    pub source: LogSource,
    pub name: String,
}

/// A level chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelChip {
    pub level: LogLevel,
    /// Its lines show.
    pub on: bool,
}

/// The list, or what stands in its place.
#[derive(Debug, PartialEq)]
pub enum Body<'a> {
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
pub struct Lines<'a> {
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
    pub fn len(&self) -> usize {
        match &self.shown {
            Shown::All(count) => *count,
            Shown::Some(indexes) => indexes.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The most columns of the mono face any line that shows takes.
    pub fn widest(&self) -> usize {
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
    pub fn get(&self, n: usize) -> &'a LogRow {
        match &self.shown {
            Shown::All(_) => &self.rows[n],
            Shown::Some(indexes) => &self.rows[indexes[n]],
        }
    }
}

/// Decides what Logs says about the log of `source`. Pure: the same model
/// gives the same view.
pub fn view<'a>(model: &'a Model, source: &LogSource) -> LogsView<'a> {
    // Caddy's log first: the window always reads it, so it is there while
    // the daemon's list is not (a connection that dropped keeps the log).
    let others = model
        .session
        .services
        .iter()
        .map(|service| service.id.as_str())
        .filter(|id| *id != LOG_SERVICE);
    let mut ids: Vec<&str> = std::iter::once(LOG_SERVICE).chain(others).collect();
    // The log the user is on stays pickable if its service went away.
    if !ids.contains(&source.id()) {
        ids.push(source.id());
    }
    let current = ids
        .iter()
        .position(|id| *id == source.id())
        .unwrap_or_default();
    let sources = ids
        .iter()
        .map(|id| SourceView {
            source: LogSource::of(id),
            name: model::service_name(id),
        })
        .collect();
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

fn body<'a>(model: &'a Model, source: &LogSource) -> Body<'a> {
    let rows = model.log_rows(source.id());
    if rows.is_empty() {
        return if model.is_connected() {
            Body::Empty(model::service_name(source.id()))
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
pub fn show(ui: &mut Ui, view: &LogsView<'_>, theme: &Theme) -> Vec<Action> {
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
mod tests {
    use eframe::egui::{
        self, Event, Modifiers, MouseWheelSource, MouseWheelUnit, Pos2, RawInput, Rect, Shape,
        TouchPhase, pos2,
    };
    use marsa_core::types::{LogLine, ServiceState, ServiceStatus};

    use super::*;
    use crate::fixtures::connected;
    use crate::model::Update;
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::{Harness, Tree};

    const INFO: &str = r#"{"level":"info","msg":"serving shop.test"}"#;
    const WARN: &str = r#"{"level":"warn","msg":"certificate for docs.test is not requested"}"#;
    const ERROR: &str = r#"{"level":"error","msg":"dial tcp 127.0.0.1:9000: connection refused"}"#;

    fn service(id: &str) -> ServiceStatus {
        ServiceStatus {
            id: id.into(),
            state: ServiceState::Running,
            pid: None,
            restarts: 0,
            since_ms: None,
        }
    }

    fn line(service: &str, seq: u64, text: &str) -> LogLine {
        LogLine {
            service: service.into(),
            seq,
            at_ms: 0,
            stream: LogStream::Stderr,
            text: text.into(),
        }
    }

    /// A model that holds these lines of Caddy's log, oldest first.
    fn with_lines(texts: &[&str]) -> Model {
        let mut model = connected();
        let lines = texts
            .iter()
            .enumerate()
            .map(|(seq, text)| line("caddy", seq as u64, text))
            .collect();
        model.update(Update::Log(lines));
        model
    }

    /// Three information lines, two warnings, and one error.
    fn mixed() -> Model {
        with_lines(&[INFO, WARN, INFO, ERROR, INFO, WARN])
    }

    fn lines<'a, 'b>(view: &'b LogsView<'a>) -> &'b Lines<'a> {
        match &view.body {
            Body::Lines(lines) => lines,
            other => panic!("expected lines, got {other:?}"),
        }
    }

    /// The text of every line that shows, in order.
    fn texts<'a>(lines: &Lines<'a>) -> Vec<&'a str> {
        (0..lines.len())
            .map(|n| lines.get(n).text.as_str())
            .collect()
    }

    fn levels(view: &LogsView<'_>) -> Vec<LogLevel> {
        let lines = lines(view);
        (0..lines.len()).map(|n| lines.get(n).level).collect()
    }

    // -- view --

    #[test]
    fn every_service_has_its_log_and_the_one_on_show_is_marked() {
        let mut model = with_lines(&[INFO]);
        model.update(Update::Services(vec![
            service("caddy"),
            service("php-fpm-8.3"),
            service("php-fpm-8.4"),
        ]));
        let view = view(&model, &LogSource::of("php-fpm-8.4"));
        let names: Vec<_> = view.sources.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Caddy", "PHP-FPM 8.3", "PHP-FPM 8.4"]);
        assert_eq!(view.current, 2);
        assert_eq!(view.sources[0].source, LogSource::Caddy);
        assert_eq!(
            view.sources[2].source,
            LogSource::Service("php-fpm-8.4".into())
        );
        // The one on show decides the lines: PHP 8.4 wrote none yet.
        assert_eq!(view.body, Body::Empty("PHP-FPM 8.4".into()));
        let caddy = super::view(&model, &LogSource::Caddy);
        assert_eq!(caddy.current, 0);
        assert_eq!(texts(lines(&caddy)), [INFO]);
    }

    #[test]
    fn a_log_that_is_on_show_stays_in_the_list_when_its_service_is_not_listed() {
        // A lost connection clears the table and keeps the log.
        let mut model = with_lines(&[INFO]);
        model.update(Update::Reconnecting);
        let view = view(&model, &LogSource::Caddy);
        assert_eq!(view.sources.len(), 1);
        assert_eq!((view.sources[0].name.as_str(), view.current), ("Caddy", 0));
        assert_eq!(lines(&view).len(), 1);
        // A pool that is gone: its name, and nothing to read.
        let gone = super::view(&model, &LogSource::of("php-fpm-9.9"));
        assert_eq!(gone.sources.len(), 2);
        assert_eq!(gone.current, 1);
        assert_eq!(gone.sources[1].name, "PHP-FPM 9.9");
        assert_eq!(gone.body, Body::Offline);
    }

    #[test]
    fn the_levels_that_are_off_leave_their_lines_out() {
        let mut model = mixed();
        let source = LogSource::Caddy;
        let all = [
            LogLevel::Info,
            LogLevel::Warning,
            LogLevel::Info,
            LogLevel::Error,
            LogLevel::Info,
            LogLevel::Warning,
        ];
        assert_eq!(levels(&view(&model, &source)), all);
        assert!(view(&model, &source).levels.iter().all(|chip| chip.on));
        model.apply_action(Action::ToggleLogLevel(LogLevel::Info));
        let shown = view(&model, &source);
        assert_eq!(
            levels(&shown),
            [LogLevel::Warning, LogLevel::Error, LogLevel::Warning]
        );
        // The chip of a level that is off says so, and the others do not.
        let on: Vec<_> = shown
            .levels
            .iter()
            .map(|chip| (chip.level, chip.on))
            .collect();
        assert_eq!(
            on,
            [
                (LogLevel::Info, false),
                (LogLevel::Warning, true),
                (LogLevel::Error, true)
            ]
        );
        model.apply_action(Action::ToggleLogLevel(LogLevel::Warning));
        assert_eq!(levels(&view(&model, &source)), [LogLevel::Error]);
        // Every level off: no line, and the screen says why.
        model.apply_action(Action::ToggleLogLevel(LogLevel::Error));
        assert_eq!(view(&model, &source).body, Body::NoMatch);
        // Back on: all of them, in their order.
        for level in LogLevel::ALL {
            model.apply_action(Action::ToggleLogLevel(level));
        }
        assert_eq!(levels(&view(&model, &source)), all);
    }

    #[test]
    fn the_filter_text_matches_ignoring_case_and_the_spaces_around_it() {
        let mut model = mixed();
        let texts = |model: &Model| {
            let view = view(model, &LogSource::Caddy);
            match &view.body {
                Body::Lines(lines) => texts(lines)
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            }
        };
        model.log_filter = "  DOCS.test ".into();
        assert_eq!(texts(&model), [WARN, WARN]);
        // The text of the raw line is searched, not only the message.
        model.log_filter = "level\":\"error".into();
        assert_eq!(texts(&model), [ERROR]);
        // With a level that is off, both must agree.
        model.log_filter = "docs".into();
        model.apply_action(Action::ToggleLogLevel(LogLevel::Warning));
        assert_eq!(view(&model, &LogSource::Caddy).body, Body::NoMatch);
        // The field shows what was typed, spaces and all.
        model.log_filter = "  Docs ".into();
        assert_eq!(view(&model, &LogSource::Caddy).filter, "  Docs ");
        // Nothing typed but spaces: no filter.
        model.log_filter = "   ".into();
        model.apply_action(Action::ToggleLogLevel(LogLevel::Warning));
        assert_eq!(texts(&model).len(), 6);
    }

    #[test]
    fn matching_works_on_bytes_and_survives_odd_input() {
        assert!(holds("GET https://shop.test/cart 200", "SHOP"));
        assert!(holds("shop", ""));
        assert!(!holds("shop", "shops"));
        assert!(!holds("", "a"));
        assert!(!holds("shop", "é"));
    }

    #[test]
    fn a_log_with_no_filter_lists_every_line_in_its_order_without_an_index() {
        let model = with_lines(&["a", "b", "c"]);
        let view = view(&model, &LogSource::Caddy);
        assert_eq!(lines(&view).shown, Shown::All(3));
        assert_eq!(texts(lines(&view)), ["a", "b", "c"]);
    }

    #[test]
    fn follow_is_on_unless_the_user_paused_it() {
        let mut model = mixed();
        assert!(view(&model, &LogSource::Caddy).follow);
        model.apply_action(Action::SetLogFollow(false));
        assert!(!view(&model, &LogSource::Caddy).follow);
        model.apply_action(Action::SetLogFollow(true));
        assert!(view(&model, &LogSource::Caddy).follow);
    }

    #[test]
    fn an_empty_log_and_no_daemon_say_so() {
        let empty = connected();
        assert_eq!(
            view(&empty, &LogSource::Caddy).body,
            Body::Empty("Caddy".into())
        );
        assert_eq!(
            strings::log_empty("Caddy"),
            "Caddy has not written anything yet."
        );
        let none = Model::default();
        assert_eq!(view(&none, &LogSource::Caddy).body, Body::Offline);
    }

    // -- show --

    /// A window of 1120 by 720, as the design has it.
    fn input(events: Vec<Event>) -> RawInput {
        RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1120.0, 720.0))),
            events,
            ..RawInput::default()
        }
    }

    fn draw(ui: &mut Ui, model: &Model, source: &LogSource) -> Vec<Action> {
        show(ui, &view(model, source), &LIGHT)
    }

    /// Frames with no input, so the layout settles.
    fn settle(harness: &Harness, model: &Model, frames: usize) -> (Vec<Action>, Tree) {
        let mut actions = Vec::new();
        let mut tree = None;
        for _ in 0..frames {
            actions.clear();
            tree = Some(harness.frame(input(Vec::new()), |ui| {
                actions.extend(draw(ui, model, &LogSource::Caddy));
            }));
        }
        (actions, tree.expect("a frame"))
    }

    fn many(count: usize) -> Model {
        let texts: Vec<String> = (0..count).map(|n| format!("line {n}")).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        with_lines(&refs)
    }

    fn drawn(tree: &Tree) -> Vec<&str> {
        tree.labels_starting_with("Info: ")
    }

    #[test]
    fn only_the_rows_in_view_are_drawn_whatever_the_log_holds() {
        let harness = Harness::new(egui::Theme::Light);
        let model = many(5000);
        let (_, tree) = settle(&harness, &model, 4);
        let rows = drawn(&tree);
        // 720 high with the header and the tools above: about 20 rows fit.
        assert!(
            (10..=40).contains(&rows.len()),
            "{} rows drawn of 5000",
            rows.len()
        );
        // Following: the end is in view, and the start is not.
        assert!(rows.contains(&"Info: line 4999"));
        assert!(!rows.contains(&"Info: line 0"));
    }

    #[test]
    fn follow_shows_the_newest_line_and_a_log_that_is_not_followed_stays_at_its_start() {
        let harness = Harness::new(egui::Theme::Light);
        let mut model = many(300);
        let (actions, tree) = settle(&harness, &model, 4);
        assert!(drawn(&tree).contains(&"Info: line 299"));
        // At the end: nothing to report.
        assert!(actions.is_empty(), "{actions:?}");
        // New lines come while following: the newest is in view again.
        let more = (300..320)
            .map(|n| line("caddy", n, &format!("line {n}")))
            .collect();
        model.update(Update::Log(more));
        let (_, tree) = settle(&harness, &model, 4);
        assert!(drawn(&tree).contains(&"Info: line 319"));
        // A log that is not followed, drawn for the first time, shows its start.
        let other = Harness::new(egui::Theme::Light);
        let mut paused = many(300);
        paused.apply_action(Action::SetLogFollow(false));
        let (_, tree) = settle(&other, &paused, 4);
        assert!(drawn(&tree).contains(&"Info: line 0"));
        assert!(!drawn(&tree).contains(&"Info: line 299"));
    }

    #[test]
    fn follow_that_comes_back_goes_to_the_end_once() {
        let harness = Harness::new(egui::Theme::Light);
        let mut model = many(300);
        model.apply_action(Action::SetLogFollow(false));
        let (_, tree) = settle(&harness, &model, 4);
        assert!(drawn(&tree).contains(&"Info: line 0"));
        // The Follow chip is pressed: the model follows again.
        model.apply_action(Action::SetLogFollow(true));
        let (actions, tree) = settle(&harness, &model, 4);
        assert!(drawn(&tree).contains(&"Info: line 299"));
        assert!(actions.is_empty(), "the end is in view: {actions:?}");
    }

    #[test]
    fn scrolling_up_while_following_turns_follow_off_and_the_chip_says_so() {
        let harness = Harness::new(egui::Theme::Light);
        let mut model = many(300);
        settle(&harness, &model, 3);
        let wheel = Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: vec2(0.0, 600.0),
            phase: TouchPhase::Move,
            modifiers: Modifiers::NONE,
            source: MouseWheelSource::Wheel,
        };
        let mut seen = Vec::new();
        harness.frame(
            input(vec![Event::PointerMoved(pos2(700.0, 400.0)), wheel]),
            |ui| {
                seen.extend(draw(ui, &model, &LogSource::Caddy));
            },
        );
        for _ in 0..30 {
            harness.frame(input(Vec::new()), |ui| {
                seen.extend(draw(ui, &model, &LogSource::Caddy));
            });
        }
        assert!(seen.contains(&Action::SetLogFollow(false)), "{seen:?}");
        // The model takes it, and the chip is off.
        for action in seen {
            model.apply_action(action);
        }
        assert!(model.log_paused);
        let (_, tree) = settle(&harness, &model, 2);
        let chip = tree.node_with_label("Follow").expect("the chip");
        assert_eq!(chip.toggled(), Some(egui::accesskit::Toggled::False));
        // Not at the end now, so the newest line is out of view.
        assert!(!drawn(&tree).contains(&"Info: line 299"));
    }

    fn center(tree: &Tree, label: &str) -> Pos2 {
        let node = tree
            .node_with_label(label)
            .unwrap_or_else(|| panic!("no widget named {label}"));
        let b = node.bounds().expect("a node with bounds");
        pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32)
    }

    /// One click on the widget named `label`; what Logs returned.
    fn click(model: &Model, label: &str) -> Vec<Action> {
        let harness = Harness::new(egui::Theme::Light);
        let (_, tree) = settle(&harness, model, 3);
        let at = center(&tree, label);
        let mut actions = Vec::new();
        for events in [
            vec![
                Event::PointerMoved(at),
                Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
            vec![Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        ] {
            actions.clear();
            harness.frame(input(events), |ui| {
                actions.extend(draw(ui, model, &LogSource::Caddy));
            });
        }
        actions
    }

    #[test]
    fn a_chip_and_follow_return_the_action_that_flips_them() {
        let model = mixed();
        for level in LogLevel::ALL {
            assert_eq!(
                click(&model, strings::log_level_name(level)),
                [Action::ToggleLogLevel(level)],
                "{level:?}"
            );
        }
        assert_eq!(click(&model, "Follow"), [Action::SetLogFollow(false)]);
        let mut paused = mixed();
        paused.apply_action(Action::SetLogFollow(false));
        assert_eq!(click(&paused, "Follow"), [Action::SetLogFollow(true)]);
    }

    #[test]
    fn the_filter_field_returns_what_is_typed() {
        let model = mixed();
        let harness = Harness::new(egui::Theme::Light);
        let (_, tree) = settle(&harness, &model, 3);
        let at = center(&tree, "Filter lines");
        let press = |pressed| Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let mut actions = Vec::new();
        for events in [
            vec![Event::PointerMoved(at), press(true)],
            vec![press(false)],
            vec![Event::Text("docs".into())],
        ] {
            actions.clear();
            harness.frame(input(events), |ui| {
                actions.extend(draw(ui, &model, &LogSource::Caddy));
            });
        }
        assert_eq!(actions, [Action::SetLogFilter("docs".into())]);
    }

    #[test]
    fn the_source_switch_opens_another_log_and_is_absent_for_one() {
        let mut model = with_lines(&[INFO]);
        let (_, one) = settle(&Harness::new(egui::Theme::Light), &model, 3);
        assert!(one.node_with_label(strings::LOG_SOURCE).is_none());
        model.update(Update::Services(vec![
            service("caddy"),
            service("php-fpm-8.4"),
        ]));
        let (_, two) = settle(&Harness::new(egui::Theme::Light), &model, 3);
        assert!(two.node_with_label(strings::LOG_SOURCE).is_some());
        assert_eq!(
            click(&model, "PHP-FPM 8.4"),
            [Action::Navigate(Route::Logs(LogSource::Service(
                "php-fpm-8.4".into()
            )))]
        );
        // The one on show is not an action.
        assert!(click(&model, "Caddy").is_empty());
    }

    #[test]
    fn five_logs_are_a_menu_and_four_are_a_segmented_control() {
        let ids = [
            "caddy",
            "php-fpm-8.1",
            "php-fpm-8.2",
            "php-fpm-8.3",
            "php-fpm-8.4",
        ];
        let mut model = with_lines(&[INFO]);
        model.update(Update::Services(
            ids[..4].iter().map(|id| service(id)).collect(),
        ));
        let (_, four) = settle(&Harness::new(egui::Theme::Light), &model, 3);
        assert!(four.node_with_label(strings::LOG_SOURCE).is_some());
        model.update(Update::Services(ids.iter().map(|id| service(id)).collect()));
        let harness = Harness::new(egui::Theme::Light);
        let (_, five) = settle(&harness, &model, 3);
        assert!(five.node_with_label(strings::LOG_SOURCE).is_none());
        // The button names the log on show, and opens the others.
        let button = strings::log_source_menu("Caddy");
        let at = center(&five, &button);
        let press = |pressed| Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        for events in [
            vec![Event::PointerMoved(at), press(true)],
            vec![press(false)],
        ] {
            harness.frame(input(events), |ui| {
                draw(ui, &model, &LogSource::Caddy);
            });
        }
        assert!(egui::Popup::is_any_open(&harness.ctx));
        let (_, open) = settle(&harness, &model, 2);
        let at = center(&open, "PHP-FPM 8.3");
        let mut actions = Vec::new();
        let press = |pressed| Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        for events in [
            vec![Event::PointerMoved(at), press(true)],
            vec![press(false)],
        ] {
            actions.clear();
            harness.frame(input(events), |ui| {
                actions.extend(draw(ui, &model, &LogSource::Caddy));
            });
        }
        assert_eq!(
            actions,
            [Action::Navigate(Route::Logs(LogSource::Service(
                "php-fpm-8.3".into()
            )))]
        );
    }

    #[test]
    fn an_error_line_has_a_tinted_row_and_a_log_without_one_has_none() {
        for (theme, egui_theme) in [(&LIGHT, egui::Theme::Light), (&DARK, egui::Theme::Dark)] {
            let tinted = |model: &Model| {
                let harness = Harness::new(egui_theme);
                settle(&harness, model, 2);
                harness
                    .shapes(|ui| {
                        show(ui, &view(model, &LogSource::Caddy), theme);
                    })
                    .into_iter()
                    .any(|shape| matches!(shape, Shape::Rect(rect) if rect.fill == theme.color.error_bg))
            };
            assert!(tinted(&mixed()));
            assert!(!tinted(&with_lines(&[INFO, WARN])));
            // Only the rows in view count: with the error level off, no tint.
            let mut hidden = mixed();
            hidden.apply_action(Action::ToggleLogLevel(LogLevel::Error));
            assert!(!tinted(&hidden));
        }
    }

    /// A frame of the window `height` high, 1120 wide.
    fn input_of(height: f32, events: Vec<Event>) -> RawInput {
        RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1120.0, height))),
            events,
            ..RawInput::default()
        }
    }

    #[test]
    fn at_any_window_height_the_list_holds_whole_rows_and_none_is_cut_at_the_top() {
        // A log of errors only: every row has a tint, which is the row's own
        // rectangle, so a row that is cut shows as a tint that sticks out of
        // the area it is drawn in.
        let texts = vec![ERROR; 300];
        let model = with_lines(&texts);
        for height in (520..=760).step_by(7) {
            let height = height as f32;
            let harness = Harness::new(egui::Theme::Light);
            for _ in 0..3 {
                harness.run(|ui| {
                    draw(ui, &model, &LogSource::Caddy);
                });
            }
            let shapes = harness.clipped_shapes(input_of(height, Vec::new()), |ui| {
                draw(ui, &model, &LogSource::Caddy);
            });
            let tints: Vec<_> = shapes
                .iter()
                .filter_map(|(clip, shape)| match shape {
                    Shape::Rect(rect) if rect.fill == LIGHT.color.error_bg => {
                        Some((*clip, rect.rect))
                    }
                    _ => None,
                })
                .collect();
            // `show_rows` also lays out one row just past the end, which the
            // clip rect hides whole: only the rows that show are checked.
            let tints: Vec<_> = tints
                .into_iter()
                .filter(|(clip, rect)| clip.intersect(*rect).height() > 0.5)
                .collect();
            assert!(tints.len() >= 5, "{height}: {} rows", tints.len());
            for (clip, rect) in &tints {
                assert!(
                    clip.expand(0.5).contains_rect(*rect),
                    "{height}: a row {rect:?} is cut by {clip:?}"
                );
            }
            // The rows are as many as fit, one under the other.
            let mut tops: Vec<f32> = tints.iter().map(|(_, rect)| rect.top()).collect();
            tops.sort_by(f32::total_cmp);
            assert!(
                tops.windows(2)
                    .all(|pair| (pair[1] - pair[0] - LIGHT.size.log_row).abs() < 0.5)
            );
        }
    }

    /// The long line of the sideways tests: wider than the window.
    fn long_line() -> String {
        let words = "slow answer from legacy-crm.test while the upstream pool was busy".to_owned();
        format!(
            "{{\"level\":\"warn\",\"msg\":\"{}\"}}",
            [words.as_str(); 6].join(" ")
        )
    }

    #[test]
    fn a_line_wider_than_the_card_is_whole_and_the_list_scrolls_sideways_to_it() {
        let long = long_line();
        let message = logline::message(&long).into_owned();
        let model = with_lines(&[INFO, &long, INFO]);
        let harness = Harness::new(egui::Theme::Light);
        let frame = |events: Vec<Event>| {
            harness.frame(input_of(720.0, events), |ui| {
                draw(ui, &model, &LogSource::Caddy);
            })
        };
        for _ in 0..4 {
            frame(Vec::new());
        }
        let tree = frame(Vec::new());
        let label = strings::log_entry_label(LogLevel::Warning, &message);
        let place = |tree: &Tree, text: &str| {
            tree.node_with_text(text)
                .and_then(|node| node.bounds())
                .map(|b| (b.x0 as f32, b.x1 as f32))
                .expect("a row")
        };
        let (left, right) = place(&tree, &label);
        // The row is as wide as its message: wider than the window, not cut.
        assert!(right - left > 1120.0, "{left}..{right}");
        // Every row is that wide, so a tint would span the whole line.
        let (short_left, short_right) = place(&tree, "Info: serving shop.test");
        assert_eq!((short_left, short_right), (left, right));
        // A wheel to the side moves the rows to the start side.
        let wheel = Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: vec2(-500.0, 0.0),
            phase: TouchPhase::Move,
            modifiers: Modifiers::NONE,
            source: MouseWheelSource::Wheel,
        };
        frame(vec![Event::PointerMoved(pos2(700.0, 300.0)), wheel]);
        let mut scrolled = tree;
        for _ in 0..40 {
            scrolled = frame(Vec::new());
        }
        let (now_left, now_right) = place(&scrolled, &label);
        assert!(now_left < left - 100.0, "{now_left} against {left}");
        // The end of the line comes into view: its right edge is inside the window.
        assert!(
            now_right <= 1120.0 + 1.0 || now_left < left - 400.0,
            "{now_left}..{now_right}"
        );
    }

    #[test]
    fn a_log_with_only_short_lines_has_rows_as_wide_as_the_card_and_no_sideways_scroll() {
        let model = mixed();
        let harness = Harness::new(egui::Theme::Light);
        let mut tree = None;
        for _ in 0..4 {
            tree = Some(harness.frame(input_of(720.0, Vec::new()), |ui| {
                draw(ui, &model, &LogSource::Caddy);
            }));
        }
        let tree = tree.expect("a frame");
        let b = tree
            .node_with_text("Info: serving shop.test")
            .and_then(|node| node.bounds())
            .expect("a row");
        // As wide as the window it is drawn in (the test has no sidebar).
        assert!(b.x1 - b.x0 <= 1120.0, "{}..{}", b.x0, b.x1);
    }

    #[test]
    fn a_log_that_says_nothing_or_matches_nothing_has_a_sentence_and_the_tools() {
        for (model, text) in [
            (connected(), strings::log_empty("Caddy")),
            (Model::default(), strings::LOGS_OFFLINE.to_owned()),
        ] {
            let (_, tree) = settle(&Harness::new(egui::Theme::Light), &model, 2);
            assert!(tree.node_with_text(strings::LOGS).is_some());
            assert!(tree.node_with_text(&text).is_some(), "{text}");
            assert!(tree.node_with_label("Follow").is_some());
        }
        let mut hidden = mixed();
        hidden.log_filter = "nothing like this".into();
        let (_, tree) = settle(&Harness::new(egui::Theme::Light), &hidden, 2);
        assert!(tree.node_with_text(strings::LOG_NO_MATCH).is_some());
        assert_eq!(strings::LOG_NO_MATCH, "No lines match.");
        // The chips and the filter field stay, so the user can undo the filter.
        for label in ["Info", "Warning", "Error", "Follow", strings::FILTER_LINES] {
            assert!(tree.node_with_label(label).is_some(), "{label}");
        }
    }

    #[test]
    fn a_standard_output_line_is_drawn_quiet_and_an_error_line_says_error() {
        let mut model = connected();
        let out = LogLine {
            stream: LogStream::Stdout,
            ..line("caddy", 0, "from stdout")
        };
        model.update(Update::Log(vec![out, line("caddy", 1, ERROR)]));
        let (_, tree) = settle(&Harness::new(egui::Theme::Light), &model, 3);
        // The message has no JSON around it, and starts with its level.
        let label = strings::log_entry_label(
            LogLevel::Error,
            "dial tcp 127.0.0.1:9000: connection refused",
        );
        assert!(tree.node_with_text(&label).is_some(), "{label}");
        assert!(tree.node_with_text("Info: from stdout").is_some());
    }
}
