//! Dumps (layout.md, Dumps screen): the `dump()` calls of PHP sites that
//! `marsad` caught. The header has the switch "Capture" and "Clear", and a
//! caption that says where the dumps come from. Under it, the list on the
//! start side, newest first, and the shown dump on the end side: its place,
//! its facts, and its text in the mono face. Instead of the list: "No dumps
//! yet.", or why nothing comes (capture off, the port taken).
//!
//! Like Mail it fills the height and scrolls inside, and it draws only what
//! is in view: the rows of the list (`ScrollArea::show_rows`) and the lines
//! of the text (the same, over the lines of one dump, which may have 20 000).
//! The lines are found once for each dump, not on every frame.
//!
//! Like the other screens it is two functions. [`view`] reads the [`Model`]
//! and decides what the screen says: pure, so the tests assert it without a
//! window. [`show`] draws that [`DumpsView`] and returns what the user did as
//! [`Action`]s (code.md, State). Nothing here asks the network.

use std::sync::Arc;

use eframe::egui::{
    Align, Color32, Id, Label, Rect, RichText, ScrollArea, Sense, TextWrapMode, Ui, UiBuilder,
    Vec2, vec2,
};
use marsa_core::types::{Dump, DumpsStatus, MAX_KEPT_DUMPS};

use super::lines::message;
use crate::model::{Action, DumpsState, Model};
use crate::strings;
use crate::theme::page::{DUMPS_FACT_LABEL, DUMPS_LIST_WIDTH, DUMPS_STATE_WIDTH};
use crate::theme::{Direction, Section, Theme};
use crate::widgets::panel::divider;
use crate::widgets::table::{card, padded_card};
use crate::widgets::{Button, MailRow, Switch, Text};

/// What Dumps says.
#[derive(Debug, PartialEq)]
pub(crate) struct DumpsView<'a> {
    /// The switch: whether the dump server is on.
    pub capture: bool,
    /// Why the switch cannot be pressed now.
    pub capture_off: Option<&'static str>,
    /// Why "Clear" cannot be pressed now.
    pub clear_off: Option<&'static str>,
    /// A line under the caption: why nothing new comes, while the list
    /// still shows older dumps.
    pub note: Option<&'static str>,
    pub body: Body<'a>,
}

/// The list, or what stands in its place.
#[derive(Debug, PartialEq)]
pub(crate) enum Body<'a> {
    /// No daemon to ask.
    Offline,
    /// Connected, and no answer yet.
    Unread,
    /// The read failed, and there is nothing to show: why, in words.
    Failed(String),
    /// The capture is off and no dump is left.
    Off,
    /// Another program holds the port and no dump is left.
    Busy,
    /// The capture is on and nothing came yet.
    Empty,
    List(Listed<'a>),
}

/// The list and the shown dump.
#[derive(Debug, PartialEq)]
pub(crate) struct Listed<'a> {
    /// Newest first. Each row's words are made when it is drawn, so a long
    /// list costs only the rows in view.
    pub dumps: &'a [Dump],
    /// Which dumps are new: the model's count of what the user has not seen.
    pub state: &'a DumpsState,
    /// The index of the shown dump.
    pub shown: usize,
    /// Under the last row: the list holds as many as the daemon keeps.
    pub full: bool,
    pub detail: Detail<'a>,
    /// The time the ages count to, in milliseconds since the Unix epoch.
    pub now: u64,
}

/// The shown dump.
#[derive(Debug, PartialEq)]
pub(crate) struct Detail<'a> {
    pub id: u64,
    /// `folder/file.php:line` (the folders cut from the start), or "dump()"
    /// for a dump that names no file.
    pub place: String,
    /// The whole path and the line, for the facts.
    pub file: Option<String>,
    /// "GET http://shop.test/users", for a dump made serving a request.
    pub request: Option<String>,
    pub label: Option<&'a str>,
    /// How long ago: "4 min ago".
    pub sent: String,
    pub text: &'a str,
    /// The text ends where the dump reader cut it.
    pub cut: bool,
}

/// Decides what Dumps says at the time `now`. Pure: the same model and time
/// give the same view.
pub(crate) fn view(model: &Model, now: u64) -> DumpsView<'_> {
    let state = &model.session.dumps;
    let capture_off = (!model.is_connected()).then_some(strings::CAPTURE_OFFLINE_HINT);
    let clear_off = state.dumps.is_empty().then_some(strings::NO_DUMPS_TO_CLEAR);
    let mut note = None;
    let body = if !model.is_connected() {
        Body::Offline
    } else if !state.loaded {
        match &state.error {
            Some(code) => Body::Failed(strings::error_text(code)),
            None => Body::Unread,
        }
    } else if state.dumps.is_empty() {
        match state.status {
            DumpsStatus::Disabled => Body::Off,
            DumpsStatus::PortInUse => Body::Busy,
            DumpsStatus::Listening => Body::Empty,
        }
    } else {
        note = match state.status {
            DumpsStatus::Disabled => Some(strings::DUMPS_OFF),
            DumpsStatus::PortInUse => Some(strings::DUMPS_BUSY),
            DumpsStatus::Listening => None,
        };
        Body::List(listed(state, now))
    };
    DumpsView {
        capture: state.capture_on(),
        capture_off,
        clear_off,
        note,
        body,
    }
}

fn listed(state: &DumpsState, now: u64) -> Listed<'_> {
    let shown = state
        .shown()
        .and_then(|shown| state.dumps.iter().position(|dump| dump.id == shown.id))
        .unwrap_or(0);
    Listed {
        dumps: &state.dumps,
        state,
        shown,
        full: state.dumps.len() >= MAX_KEPT_DUMPS,
        detail: detail(&state.dumps[shown], now),
        now,
    }
}

fn detail(dump: &Dump, now: u64) -> Detail<'_> {
    let file = dump
        .source_file
        .as_ref()
        .map(|file| match dump.source_line {
            Some(line) => format!("{file}:{line}"),
            None => file.clone(),
        });
    Detail {
        id: dump.id,
        place: first_line(dump),
        file,
        request: dump.uri.as_ref().map(|uri| {
            format!("{} {uri}", dump.method.as_deref().unwrap_or(""))
                .trim()
                .to_owned()
        }),
        label: dump.label.as_deref(),
        sent: strings::dump_age(dump.received_at, now),
        text: &dump.text,
        cut: dump.truncated,
    }
}

/// The first line of a row: the file and line with the folders cut from the
/// start, or "dump()".
fn first_line(dump: &Dump) -> String {
    match (&dump.source_file, dump.source_line) {
        (Some(file), Some(line)) => format!("{}:{line}", strings::short_path(file)),
        (Some(file), None) => strings::short_path(file),
        (None, _) => strings::DUMP_NO_PLACE.to_owned(),
    }
}

/// The second line of a row: the request, or the first line of the text.
fn second_line(dump: &Dump) -> String {
    if let Some(uri) = &dump.uri {
        return format!("{} {uri}", dump.method.as_deref().unwrap_or(""))
            .trim()
            .to_owned();
    }
    // A line, not the whole text: a dump may be long.
    dump.text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect()
}

/// Draws `view`. Returns what the user did.
pub(crate) fn show(ui: &mut Ui, view: &DumpsView<'_>, theme: &Theme) -> Vec<Action> {
    let mut actions = Vec::new();
    ui.spacing_mut().item_spacing.y = theme.space.section_gap;
    ui.vertical(|ui| {
        // The caption belongs to the header: closer to it than a section.
        ui.spacing_mut().item_spacing.y = theme.space.s2;
        let title = strings::section_name(Section::Dumps);
        super::header(ui, theme, Section::Dumps, title, |ui| {
            // From the end: "Clear" ends the line, the switch is before it.
            let off = view.clear_off;
            let clear = Button::secondary(strings::CLEAR_DUMPS)
                .disabled_if(off.is_some(), off.unwrap_or_default());
            if ui.add(clear).clicked() {
                actions.push(Action::ClearDumps);
            }
            let off = view.capture_off;
            let capture = Switch::new(view.capture, strings::CAPTURE)
                .disabled_if(off.is_some(), off.unwrap_or_default());
            if ui.add(capture).clicked() {
                actions.push(Action::ToggleDumps);
            }
        });
        ui.add(Text::new(
            strings::DUMPS_LEAD,
            theme.text.body_sm,
            theme.color.mute,
        ));
        if let Some(note) = view.note {
            ui.add(Text::new(note, theme.text.body_sm, theme.color.ink2));
        }
    });
    match &view.body {
        Body::Offline => message(ui, theme, strings::DUMPS_OFFLINE),
        Body::Unread => {}
        Body::Failed(text) => message(ui, theme, text),
        Body::Off => {
            let button = Button::primary(strings::TURN_ON_CAPTURE);
            let how = strings::DUMPS_OFF_HOW;
            if state(ui, theme, strings::DUMPS_OFF, how, Some(button)) {
                actions.push(Action::ToggleDumps);
            }
        }
        // No button: the switch in the header does it.
        Body::Busy => {
            state(
                ui,
                theme,
                strings::DUMPS_BUSY,
                strings::DUMPS_BUSY_HOW,
                None,
            );
        }
        // No button: the fix is in the code.
        Body::Empty => {
            state(ui, theme, strings::NO_DUMPS, strings::NO_DUMPS_HOW, None);
        }
        Body::List(listed) => split(ui, theme, listed, &mut actions),
    }
    actions
}

/// A card that fills the height left, with a sentence, a caption under it,
/// and the button of the fix if there is one, in the middle. Returns
/// whether the button was pressed.
fn state(
    ui: &mut Ui,
    theme: &Theme,
    sentence: &str,
    how: &str,
    button: Option<Button<'_>>,
) -> bool {
    use eframe::egui::text::LayoutJob;
    card(ui, |ui| {
        let room = ui.available_height();
        ui.set_min_height(room);
        let width = ui.available_width().min(DUMPS_STATE_WIDTH);
        // Laid out first, to center the block on the card's height.
        let centered = |text: &str, font, color| {
            let mut job = LayoutJob::simple(text.to_owned(), font, color, width);
            job.halign = Align::Center;
            ui.painter().layout_job(job)
        };
        let first = centered(sentence, theme.text.body.font_id(), theme.color.ink);
        let second = centered(how, theme.text.body_sm.font_id(), theme.color.mute);
        let gap = theme.space.s3;
        let fix = button.as_ref().map_or(0.0, |_| gap + theme.size.control);
        let block = first.size().y + gap + second.size().y + fix;
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = gap;
            ui.add_space(0.0_f32.midpoint(room - block).max(theme.space.s6));
            ui.add(Label::new(first).selectable(false));
            ui.add(Label::new(second).selectable(false));
            button.is_some_and(|button| ui.add(button).clicked())
        })
        .inner
    })
    .inner
}

/// The list on the start side, the shown dump on the end side: both as high
/// as the room left.
fn split(ui: &mut Ui, theme: &Theme, listed: &Listed<'_>, actions: &mut Vec<Action>) {
    let size = ui.available_size();
    ui.allocate_ui_with_layout(size, Direction::CURRENT.row(), |ui| {
        ui.spacing_mut().item_spacing.x = theme.space.section_gap;
        let height = ui.available_height();
        let column = Direction::CURRENT.column();
        ui.allocate_ui_with_layout(vec2(DUMPS_LIST_WIDTH, height), column, |ui| {
            list(ui, theme, listed, actions);
        });
        let rest = vec2(ui.available_width(), height);
        ui.allocate_ui_with_layout(rest, column, |ui| {
            detail_card(ui, theme, listed, actions);
        });
    });
}

/// The dumps in a card. Only the rows in view are drawn; a click, or Enter
/// on the focused row, shows a dump.
fn list(ui: &mut Ui, theme: &Theme, listed: &Listed<'_>, actions: &mut Vec<Action>) {
    let row = theme.size.row_max;
    card(ui, |ui| {
        // The rows touch: the line under each divides them.
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let height = ui.available_height();
        ui.set_min_height(height);
        // The caption of a full list is one more row, after the last dump.
        let rows = listed.dumps.len() + usize::from(listed.full);
        ScrollArea::vertical()
            .id_salt("dumps_list")
            .auto_shrink(false)
            .max_height(height)
            .show_rows(ui, row, rows, |ui, range| {
                for index in range {
                    match listed.dumps.get(index) {
                        Some(dump) => dump_row(ui, listed, index, dump, actions),
                        None => kept_line(ui, theme),
                    }
                }
            });
    });
}

fn dump_row(
    ui: &mut Ui,
    listed: &Listed<'_>,
    index: usize,
    dump: &Dump,
    actions: &mut Vec<Action>,
) {
    let age = strings::dump_age(dump.received_at, listed.now);
    let (first, second) = (first_line(dump), second_line(dump));
    let row = MailRow::new(&first, &second, &age)
        .section(Section::Dumps)
        .new_mail(listed.state.is_new(dump))
        .selected(listed.shown == index);
    let response = ui.add(row);
    // The arrows move the focus from row to row: keep the focused one in
    // view, so the next one is drawn too.
    if response.gained_focus() {
        response.scroll_to_me(None);
    }
    if response.clicked() {
        actions.push(Action::ChooseDump(dump.id));
    }
}

/// "The newest 500 are kept.", in a row of its own.
fn kept_line(ui: &mut Ui, theme: &Theme) {
    let size = vec2(ui.available_width(), theme.size.row_max);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let inner = rect.shrink2(vec2(theme.space.s4, 0.0));
    let layout = Direction::CURRENT.row();
    let mut line = ui.new_child(UiBuilder::new().max_rect(inner).layout(layout));
    line.add(Text::caption(theme, strings::DUMPS_KEPT));
}

/// The shown dump: its place, its facts, "Copy", and its text.
fn detail_card(ui: &mut Ui, theme: &Theme, listed: &Listed<'_>, actions: &mut Vec<Action>) {
    padded_card(ui, |ui| {
        let height = ui.available_height();
        ui.set_min_height(height);
        let dump = &listed.detail;
        ui.spacing_mut().item_spacing.y = theme.space.s3;
        ui.add(Text::new(&dump.place, theme.text.heading, theme.color.ink).truncate());
        facts(ui, theme, dump);
        divider(ui);
        // The text takes what the lines under it leave, and scrolls.
        let cut = if dump.cut {
            // A caption line, and the gap before it.
            theme.text.caption.size * theme.text.line_height_body + theme.space.s3
        } else {
            0.0
        };
        let below = cut + theme.size.control + theme.space.s3;
        let room = (ui.available_height() - below).max(theme.size.row_max);
        ScrollArea::both()
            .id_salt(("dump_text", dump.id))
            .auto_shrink(false)
            .max_height(room)
            .show_viewport(ui, |ui, viewport| text_lines(ui, theme, dump, viewport));
        if dump.cut {
            ui.add(Text::caption(theme, strings::DUMP_CUT));
        }
        if ui.add(Button::secondary(strings::COPY_DUMP)).clicked() {
            actions.push(Action::CopyDump);
        }
    });
}

/// File, URL, Label, and Sent: a label column, then the value, cut short at
/// the end of the card. Only the facts the dump has.
fn facts(ui: &mut Ui, theme: &Theme, dump: &Detail<'_>) {
    let rows = [
        (strings::DUMP_FILE, dump.file.as_deref()),
        (strings::DUMP_URL, dump.request.as_deref()),
        (strings::DUMP_LABEL, dump.label),
        (strings::DUMP_SENT, Some(dump.sent.as_str())),
    ];
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = theme.space.s1;
        for (label, value) in rows {
            let Some(value) = value else { continue };
            let line = vec2(ui.available_width(), theme.text.body_sm.size);
            ui.allocate_ui_with_layout(line, Direction::CURRENT.row(), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let cell = vec2(DUMPS_FACT_LABEL, line.y);
                ui.allocate_ui_with_layout(cell, Direction::CURRENT.row(), |ui| {
                    // The column is as wide as asked, not as its word.
                    ui.set_min_size(cell);
                    ui.add(Text::caption(theme, label));
                });
                ui.add(Text::new(value, theme.text.body_sm, theme.color.ink).truncate());
            });
        }
    });
}

/// Where each line of a dump's text starts and ends, kept between frames.
#[derive(Clone)]
struct Lines {
    id: u64,
    len: usize,
    ranges: Arc<[(u32, u32)]>,
}

/// The lines of `dump`'s text: found once for each dump, not on every
/// frame (rules.md 10).
fn lines_of(ui: &Ui, dump: &Detail<'_>) -> Arc<[(u32, u32)]> {
    let key = Id::unique("dump_lines");
    let kept = ui
        .data(|data| data.get_temp::<Lines>(key))
        .filter(|lines| lines.id == dump.id && lines.len == dump.text.len());
    if let Some(lines) = kept {
        return lines.ranges;
    }
    let mut ranges = Vec::new();
    let mut start = 0_usize;
    for line in dump.text.split('\n') {
        let end = start + line.len();
        // A text over 4 GiB cannot be: a dump is cut at 256 KiB.
        ranges.push((
            u32::try_from(start).unwrap_or(u32::MAX),
            u32::try_from(end).unwrap_or(u32::MAX),
        ));
        start = end + 1;
    }
    let ranges: Arc<[(u32, u32)]> = ranges.into();
    let lines = Lines {
        id: dump.id,
        len: dump.text.len(),
        ranges: Arc::clone(&ranges),
    };
    ui.data_mut(|data| data.insert_temp(key, lines));
    ranges
}

/// The text of the dump, one label for each line in view: monospace, never
/// wrapped, selectable. `viewport` is the part of the content in view, in
/// the content's own coordinates. A dump of 20 000 lines costs the frame
/// what a screenful does.
fn text_lines(ui: &mut Ui, theme: &Theme, dump: &Detail<'_>, viewport: Rect) {
    let ranges = lines_of(ui, dump);
    let font = theme.text.mono_sm.font_id();
    let row = ui
        .painter()
        .layout_no_wrap("Ag".to_owned(), font.clone(), Color32::PLACEHOLDER)
        .size()
        .y;
    // The rows touch: there is no space between lines of a text.
    ui.spacing_mut().item_spacing.y = 0.0;
    let total = ranges.len();
    let first = ((viewport.min.y / row).floor().max(0.0) as usize).min(total);
    let last = (((viewport.max.y / row).ceil().max(0.0) as usize) + 1).min(total);
    // The content is as high as all the lines, so the scroll bar is true.
    ui.set_min_height(total as f32 * row);
    ui.add_space(first as f32 * row);
    for &(from, to) in &ranges[first..last] {
        let line = &dump.text[from as usize..to as usize];
        let text = RichText::new(line)
            .font(font.clone())
            .color(theme.color.ink);
        ui.add(
            Label::new(text)
                .wrap_mode(TextWrapMode::Extend)
                .selectable(true),
        );
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Pos2, RawInput, Rect, Role, pos2};
    use marsa_core::types::DumpList;

    use super::*;
    use crate::fixtures::connected;
    use crate::model::Update;
    use crate::theme::LIGHT;
    use crate::widgets::harness::{Harness, Tree};

    /// The time the tests count the ages to.
    const NOW: u64 = 1_800_000_000_000;

    fn dump(id: u64, minutes: u64, text: &str) -> Dump {
        Dump {
            id,
            received_at: NOW - minutes * 60_000,
            source_file: Some("/code/shop/routes/web.php".into()),
            source_line: Some(id as u32),
            uri: Some("http://shop.test/users".into()),
            method: Some("GET".into()),
            label: None,
            text: text.into(),
            truncated: false,
        }
    }

    /// Connected, with these dumps (newest first) and this status.
    fn with(dumps: Vec<Dump>, status: DumpsStatus) -> Model {
        let mut model = connected();
        model.update(Update::Dumps {
            list: DumpList {
                status,
                latest_id: dumps.first().map_or(0, |dump| dump.id),
                count: dumps.len() as u64,
                dumps,
            },
            replace: true,
        });
        model
    }

    fn three() -> Model {
        with(
            vec![
                dump(3, 0, "array:1 [\n  0 => 1\n]"),
                dump(2, 4, "\"hello\""),
                dump(1, 120, "null"),
            ],
            DumpsStatus::Listening,
        )
    }

    fn input() -> RawInput {
        RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1120.0, 720.0))),
            ..RawInput::default()
        }
    }

    fn draw(ui: &mut Ui, model: &Model) -> Vec<Action> {
        show(ui, &view(model, NOW), &LIGHT)
    }

    fn frame_of(model: &Model) -> (Harness, Tree) {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.frame(input(), |ui| {
            draw(ui, model);
        });
        (harness, tree)
    }

    fn center(tree: &Tree, label: &str) -> Pos2 {
        let node = tree.node_with_label(label).expect(label);
        let bounds = node.bounds().expect("a node with bounds");
        pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        )
    }

    fn press(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn click(model: &Model, label: &str) -> Vec<Action> {
        let (harness, tree) = frame_of(model);
        let mut actions = Vec::new();
        let at = center(&tree, label);
        for (index, events) in [
            vec![egui::Event::PointerMoved(at), press(at, true)],
            vec![press(at, false)],
        ]
        .into_iter()
        .enumerate()
        {
            let frame = RawInput { events, ..input() };
            harness.frame(frame, |ui| {
                let done = draw(ui, model);
                if index == 1 {
                    actions.extend(done);
                }
            });
        }
        actions
    }

    #[test]
    fn the_view_follows_the_model_in_every_state() {
        assert_eq!(view(&Model::default(), NOW).body, Body::Offline);
        assert_eq!(
            view(&Model::default(), NOW).capture_off,
            Some(strings::CAPTURE_OFFLINE_HINT)
        );
        assert_eq!(view(&connected(), NOW).body, Body::Unread);
        let mut failed = connected();
        failed.update(Update::DumpsFailed("not_found".into()));
        assert!(matches!(view(&failed, NOW).body, Body::Failed(_)));
        let listening = with(Vec::new(), DumpsStatus::Listening);
        let disabled = with(Vec::new(), DumpsStatus::Disabled);
        let busy = with(Vec::new(), DumpsStatus::PortInUse);
        assert_eq!(view(&listening, NOW).body, Body::Empty);
        assert_eq!(view(&disabled, NOW).body, Body::Off);
        assert_eq!(view(&busy, NOW).body, Body::Busy);
        assert_eq!(
            view(&listening, NOW).clear_off,
            Some(strings::NO_DUMPS_TO_CLEAR)
        );
        assert!(!view(&disabled, NOW).capture);
        assert!(view(&busy, NOW).capture, "on, with the port taken");
    }

    #[test]
    fn older_dumps_stay_with_a_note_when_nothing_new_can_come() {
        let disabled = with(vec![dump(1, 0, "1")], DumpsStatus::Disabled);
        let off = view(&disabled, NOW);
        assert!(matches!(off.body, Body::List(_)));
        assert_eq!(off.note, Some(strings::DUMPS_OFF));
        let taken = with(vec![dump(1, 0, "1")], DumpsStatus::PortInUse);
        assert_eq!(view(&taken, NOW).note, Some(strings::DUMPS_BUSY));
        assert_eq!(view(&three(), NOW).note, None);
    }

    #[test]
    fn the_newest_is_shown_until_one_is_chosen_and_the_list_says_when_it_is_full() {
        let mut model = three();
        let shown = view(&model, NOW);
        let Body::List(listed) = &shown.body else {
            panic!("a list");
        };
        assert_eq!((listed.shown, listed.detail.id), (0, 3));
        assert_eq!(listed.detail.place, "routes/web.php:3");
        assert_eq!(
            listed.detail.file.as_deref(),
            Some("/code/shop/routes/web.php:3")
        );
        assert_eq!(
            listed.detail.request.as_deref(),
            Some("GET http://shop.test/users")
        );
        assert!(!listed.full);
        model.apply_action(Action::ChooseDump(1));
        let shown = view(&model, NOW);
        let Body::List(listed) = &shown.body else {
            panic!("a list");
        };
        assert_eq!((listed.shown, listed.detail.id), (2, 1));
        assert_eq!(listed.detail.sent, "2 h ago");
        // As many as the daemon keeps: the caption row.
        let many: Vec<Dump> = (1..=MAX_KEPT_DUMPS as u64)
            .rev()
            .map(|id| dump(id, 0, "1"))
            .collect();
        let full = with(many, DumpsStatus::Listening);
        let shown = view(&full, NOW);
        let Body::List(listed) = &shown.body else {
            panic!("a list");
        };
        assert!(listed.full);
    }

    #[test]
    fn a_row_names_its_place_and_says_new_in_words() {
        let mut model = three();
        // Seen: nothing is new.
        model.session.dumps.set_open(true);
        model.session.dumps.set_open(false);
        model.update(Update::Dumps {
            list: DumpList {
                status: DumpsStatus::Listening,
                latest_id: 4,
                count: 4,
                dumps: vec![dump(4, 0, "true")],
            },
            replace: false,
        });
        let (_, tree) = frame_of(&model);
        let row = |place: &str| {
            let node = tree.node_with_label(place).expect(place);
            (node.role(), node.description().map(str::to_owned))
        };
        let (role, description) = row("routes/web.php:4");
        assert_eq!(role, Role::ListBoxOption);
        assert_eq!(
            description.as_deref(),
            Some("New, GET http://shop.test/users, just now")
        );
        assert_eq!(
            row("routes/web.php:2").1.as_deref(),
            Some("GET http://shop.test/users, 4 min ago")
        );
    }

    #[test]
    fn a_click_on_a_row_shows_that_dump_and_the_buttons_say_what_they_do() {
        let model = three();
        assert_eq!(click(&model, "routes/web.php:2"), [Action::ChooseDump(2)]);
        assert_eq!(click(&model, strings::CLEAR_DUMPS), [Action::ClearDumps]);
        assert_eq!(click(&model, strings::COPY_DUMP), [Action::CopyDump]);
        assert_eq!(click(&model, strings::CAPTURE), [Action::ToggleDumps]);
        // Nothing to clear: the button is off and does nothing.
        let empty = with(Vec::new(), DumpsStatus::Listening);
        let (_, tree) = frame_of(&empty);
        assert!(
            tree.node_with_label(strings::CLEAR_DUMPS)
                .expect("Clear")
                .is_disabled()
        );
        assert!(click(&empty, strings::CLEAR_DUMPS).is_empty());
    }

    #[test]
    fn the_detail_shows_the_facts_and_the_lines_of_the_text() {
        let (_, tree) = frame_of(&three());
        assert!(tree.node_with_text(strings::DUMPS_LEAD).is_some());
        for label in [strings::DUMP_FILE, strings::DUMP_URL, strings::DUMP_SENT] {
            assert!(tree.node_with_text(label).is_some(), "{label}");
        }
        // No label on this dump: no row for it.
        assert!(tree.node_with_text(strings::DUMP_LABEL).is_none());
        assert!(tree.node_with_text("/code/shop/routes/web.php:3").is_some());
        for line in ["array:1 [", "  0 => 1", "]"] {
            assert!(tree.node_with_text(line).is_some(), "{line:?}");
        }
    }

    #[test]
    fn the_empty_states_say_why_and_only_capture_off_has_a_button() {
        let (_, tree) = frame_of(&with(Vec::new(), DumpsStatus::Listening));
        assert!(tree.node_with_text(strings::NO_DUMPS).is_some());
        assert!(tree.node_with_text(strings::NO_DUMPS_HOW).is_some());
        assert!(tree.node_with_label(strings::TURN_ON_CAPTURE).is_none());
        let (_, tree) = frame_of(&with(Vec::new(), DumpsStatus::PortInUse));
        assert!(tree.node_with_text(strings::DUMPS_BUSY).is_some());
        assert!(tree.node_with_text(strings::DUMPS_BUSY_HOW).is_some());
        let off = with(Vec::new(), DumpsStatus::Disabled);
        let (_, tree) = frame_of(&off);
        assert!(tree.node_with_text(strings::DUMPS_OFF).is_some());
        assert_eq!(click(&off, strings::TURN_ON_CAPTURE), [Action::ToggleDumps]);
    }

    #[test]
    fn a_dump_of_twenty_thousand_lines_draws_only_the_lines_in_view() {
        let mut text = String::new();
        for n in 0..20_000 {
            text.push_str(&format!("line {n}\n"));
        }
        let model = with(vec![dump(1, 0, &text)], DumpsStatus::Listening);
        let (_, tree) = frame_of(&model);
        let lines = tree.labels_starting_with("line ");
        // 720 high: a few dozen lines of the mono face, never the 20 000.
        assert!(
            (5..120).contains(&lines.len()),
            "{} lines drawn",
            lines.len()
        );
        assert!(lines.contains(&"line 0"), "{lines:?}");
        assert!(!lines.contains(&"line 19999"));
    }

    #[test]
    fn five_hundred_dumps_in_the_list_draw_only_the_rows_in_view() {
        let many: Vec<Dump> = (1..=500).rev().map(|id| dump(id, 0, "1")).collect();
        let model = with(many, DumpsStatus::Listening);
        let (_, tree) = frame_of(&model);
        let rows = tree.labels_starting_with("routes/web.php:").len();
        // 720 high: about a dozen rows of 54, never the 500.
        assert!((5..30).contains(&rows), "{rows} rows drawn");
    }

    #[test]
    fn the_lines_of_a_text_are_found_once_for_each_dump() {
        let model = with(vec![dump(1, 0, "a\nb\nc")], DumpsStatus::Listening);
        let harness = Harness::new(egui::Theme::Light);
        let mut first: Option<Arc<[(u32, u32)]>> = None;
        for _ in 0..3 {
            harness.frame(input(), |ui| {
                draw(ui, &model);
            });
            let kept = harness
                .ctx
                .data(|data| data.get_temp::<Lines>(Id::unique("dump_lines")))
                .expect("lines kept");
            assert_eq!(&*kept.ranges, &[(0, 1), (2, 3), (4, 5)]);
            let ranges = first.get_or_insert_with(|| Arc::clone(&kept.ranges));
            assert!(Arc::ptr_eq(ranges, &kept.ranges), "found again");
        }
        // Another dump: new lines.
        let other = with(vec![dump(2, 0, "x\ny")], DumpsStatus::Listening);
        harness.frame(input(), |ui| {
            draw(ui, &other);
        });
        let kept = harness
            .ctx
            .data(|data| data.get_temp::<Lines>(Id::unique("dump_lines")))
            .expect("lines kept");
        assert_eq!(&*kept.ranges, &[(0, 1), (2, 3)]);
    }

    #[test]
    fn a_cut_dump_ends_with_the_line_that_says_so() {
        let cut = Dump {
            truncated: true,
            ..dump(1, 0, "\"x\"")
        };
        let (_, tree) = frame_of(&with(vec![cut], DumpsStatus::Listening));
        assert!(tree.node_with_text(strings::DUMP_CUT).is_some());
        let (_, whole) = frame_of(&three());
        assert!(whole.node_with_text(strings::DUMP_CUT).is_none());
    }
}
