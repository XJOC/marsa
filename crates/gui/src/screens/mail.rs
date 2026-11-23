//! Mail (layout.md, Mail screen): the mail Mailpit caught. The header has
//! "Open Mailpit" and "Delete all…", and a caption that says where the mail
//! comes from. Under it, the list of the newest mails on the start side and
//! the chosen mail on the end side: its subject, From, To, and Sent, its
//! plain text, and "Open in browser" for its HTML. Instead of the list: "No
//! mail yet." with how to send some, or "Mailpit is not running." with
//! "Start Mailpit".
//!
//! Like Logs it fills the height and scrolls inside: only the rows in view
//! are drawn (`ScrollArea::show_rows`), and the chosen mail's text is laid
//! out once for each mail, not on every frame.
//!
//! Like the other screens it is two functions. [`view`] reads the [`Model`]
//! and decides what the screen says: pure, so the tests assert it without a
//! window. [`show`] draws that [`MailView`] with the widgets and returns what
//! the user did as [`Action`]s (code.md, State). Nothing here asks the
//! network: the bridge reads the mail and the model keeps it.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::text::{Galley, LayoutJob, TextFormat};
use eframe::egui::{Align, Id, Label, ScrollArea, Sense, Ui, UiBuilder, Vec2, vec2};
use marsa_core::types::{MailList, MailSummary, ServiceAction, ServiceState};

use super::lines::message;
use crate::model::{Action, MAILPIT, MailState, Model, Removal};
use crate::strings;
use crate::theme::page::{MAIL_FACT_LABEL, MAIL_LIST_WIDTH, MAIL_STATE_WIDTH};
use crate::theme::{Direction, Section, Theme};
use crate::widgets::panel::divider;
use crate::widgets::table::{card, padded_card};
use crate::widgets::{Button, MailRow, Text};

/// What Mail says.
#[derive(Debug, PartialEq)]
pub(crate) struct MailView<'a> {
    /// Why "Open Mailpit" cannot be pressed now.
    pub open_off: Option<&'static str>,
    /// Why "Delete all…" cannot be pressed now.
    pub delete_off: Option<&'static str>,
    pub body: Body<'a>,
}

/// The list, or what stands in its place.
#[derive(Debug, PartialEq)]
pub(crate) enum Body<'a> {
    /// No daemon to ask.
    Offline,
    /// Connected, and no answer yet.
    Unread,
    /// The last read failed, and there is no list to show: why, in words.
    Failed(String),
    /// Mailpit does not run. `start_off`: why "Start Mailpit" cannot be
    /// pressed now.
    NotRunning {
        start_off: Option<&'static str>,
    },
    /// Mailpit runs and has no mail.
    Empty,
    List(Listed<'a>),
}

/// The list and the chosen mail.
#[derive(Debug, PartialEq)]
pub(crate) struct Listed<'a> {
    /// Newest first. Each row's words are made when it is drawn, so a long
    /// list costs only the rows in view.
    pub messages: &'a [MailSummary],
    /// The index of the chosen mail.
    pub selected: Option<usize>,
    /// Under the last row when Mailpit keeps more mail than the list holds.
    pub more: Option<String>,
    /// `None`: nothing is chosen yet.
    pub detail: Option<Detail<'a>>,
    /// The time the ages count to, in milliseconds since the Unix epoch.
    pub now: u64,
}

/// The chosen mail.
#[derive(Debug, PartialEq)]
pub(crate) struct Detail<'a> {
    pub id: &'a str,
    pub subject: &'a str,
    pub from: &'a str,
    pub to: String,
    /// How long ago it was sent: "4 min ago".
    pub sent: String,
    /// The plain text, `None` while it is read. Empty for a mail with HTML
    /// only.
    pub text: Option<&'a str>,
}

/// The time now, in milliseconds since the Unix epoch, for the ages. A
/// clock before 1970 counts as the epoch.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Decides what Mail says at the time `now`. Pure: the same model and time
/// give the same view.
pub(crate) fn view(model: &Model, now: u64) -> MailView<'_> {
    let body = body(model, now);
    let open_off = match &body {
        Body::Offline | Body::NotRunning { .. } => Some(strings::MAILPIT_NOT_RUNNING_HINT),
        _ => None,
    };
    let delete_off = match &body {
        Body::List(_) if model.removing() == Some(&Removal::Mail) => Some(strings::DELETING_MAIL),
        Body::List(_) if !model.can_remove() => Some(strings::DELETE_MAIL_BUSY),
        Body::List(_) => None,
        _ => Some(strings::NO_MAIL_TO_DELETE),
    };
    MailView {
        open_off,
        delete_off,
        body,
    }
}

fn body(model: &Model, now: u64) -> Body<'_> {
    if !model.is_connected() {
        return Body::Offline;
    }
    let mail = &model.session.mail;
    let service = model.mailpit_state();
    let start_off = match service {
        Some(ServiceState::Starting) => Some(strings::MAILPIT_STARTING),
        Some(ServiceState::Stopping) => Some(strings::MAILPIT_STOPPING),
        _ => None,
    };
    if mail.not_running() {
        return Body::NotRunning { start_off };
    }
    match &mail.list {
        Some(list) if list.messages.is_empty() => Body::Empty,
        Some(list) => Body::List(listed(mail, list, now)),
        // Not read yet: the services table may already tell that Mailpit
        // does not run.
        None => match (service, &mail.error) {
            (Some(state), _) if *state != ServiceState::Running => Body::NotRunning { start_off },
            (_, Some(code)) => Body::Failed(strings::error_text(code)),
            _ => Body::Unread,
        },
    }
}

fn listed<'a>(mail: &'a MailState, list: &'a MailList, now: u64) -> Listed<'a> {
    let selected = mail
        .selected
        .as_deref()
        .and_then(|id| list.messages.iter().position(|m| m.id == id));
    let listed = list.messages.len();
    let more = (list.total > listed as u64).then(|| strings::mail_more(listed, list.total));
    Listed {
        messages: &list.messages,
        selected,
        more,
        detail: selected.map(|index| detail(mail, &list.messages[index], now)),
        now,
    }
}

fn detail<'a>(mail: &'a MailState, summary: &'a MailSummary, now: u64) -> Detail<'a> {
    let text = mail.text.as_ref().filter(|text| text.id == summary.id);
    // The date the mail gives, else when Mailpit took it.
    let sent = text
        .map(|text| text.date)
        .filter(|date| *date > 0)
        .unwrap_or(summary.received);
    Detail {
        id: &summary.id,
        subject: subject(&summary.subject),
        from: &summary.from,
        to: summary.to.join(", "),
        sent: strings::mail_age(sent, now),
        text: text.map(|text| text.text.as_str()),
    }
}

/// A subject to show: an empty one says so.
fn subject(text: &str) -> &str {
    if text.trim().is_empty() {
        strings::NO_SUBJECT
    } else {
        text
    }
}

/// Draws `view`. Returns what the user did.
pub(crate) fn show(ui: &mut Ui, view: &MailView<'_>, theme: &Theme) -> Vec<Action> {
    let mut actions = Vec::new();
    ui.spacing_mut().item_spacing.y = theme.space.section_gap;
    ui.vertical(|ui| {
        // The caption belongs to the header: closer to it than a section.
        ui.spacing_mut().item_spacing.y = theme.space.s2;
        let title = strings::section_name(Section::Mail);
        super::header(ui, theme, Section::Mail, title, |ui| {
            // From the end: "Delete all…" ends the line.
            let off = view.delete_off;
            let delete = Button::secondary(strings::DELETE_ALL_MAIL)
                .disabled_if(off.is_some(), off.unwrap_or_default());
            if ui.add(delete).clicked() {
                actions.push(Action::AskDeleteMail);
            }
            let off = view.open_off;
            let open = Button::secondary(strings::OPEN_MAILPIT)
                .disabled_if(off.is_some(), off.unwrap_or_default());
            if ui.add(open).clicked() {
                actions.push(Action::OpenMailpit);
            }
        });
        ui.add(Text::new(
            strings::MAIL_LEAD,
            theme.text.body_sm,
            theme.color.mute,
        ));
    });
    match &view.body {
        Body::Offline => message(ui, theme, strings::MAIL_OFFLINE),
        Body::Unread => {}
        Body::Failed(text) => message(ui, theme, text),
        Body::NotRunning { start_off } => {
            let button = Button::primary(strings::START_MAILPIT)
                .disabled_if(start_off.is_some(), start_off.unwrap_or_default());
            let how = strings::MAILPIT_STOPPED_HOW;
            if state(ui, theme, strings::MAILPIT_STOPPED, how, Some(button)) {
                actions.push(Action::Service {
                    service: MAILPIT,
                    action: ServiceAction::Start,
                });
            }
        }
        // No button: the fix is in the project.
        Body::Empty => {
            state(ui, theme, strings::NO_MAIL, strings::NO_MAIL_HOW, None);
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
    card(ui, |ui| {
        let room = ui.available_height();
        ui.set_min_height(room);
        let width = ui.available_width().min(MAIL_STATE_WIDTH);
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

/// The list on the start side, the chosen mail on the end side: both as high
/// as the room left.
fn split(ui: &mut Ui, theme: &Theme, listed: &Listed<'_>, actions: &mut Vec<Action>) {
    let size = ui.available_size();
    ui.allocate_ui_with_layout(size, Direction::CURRENT.row(), |ui| {
        ui.spacing_mut().item_spacing.x = theme.space.section_gap;
        let height = ui.available_height();
        let column = Direction::CURRENT.column();
        ui.allocate_ui_with_layout(vec2(MAIL_LIST_WIDTH, height), column, |ui| {
            list(ui, theme, listed, actions);
        });
        let rest = vec2(ui.available_width(), height);
        ui.allocate_ui_with_layout(rest, column, |ui| {
            detail_card(ui, theme, listed, actions);
        });
    });
}

/// The mails in a card. Only the rows in view are drawn; a click, or Enter
/// on the focused row, opens a mail.
fn list(ui: &mut Ui, theme: &Theme, listed: &Listed<'_>, actions: &mut Vec<Action>) {
    let row = theme.size.row_max;
    card(ui, |ui| {
        // The rows touch: the line under each divides them.
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let height = ui.available_height();
        ui.set_min_height(height);
        // The caption of a cut list is one more row, after the last mail.
        let rows = listed.messages.len() + usize::from(listed.more.is_some());
        ScrollArea::vertical()
            .id_salt("mail_list")
            .auto_shrink(false)
            .max_height(height)
            .show_rows(ui, row, rows, |ui, range| {
                for index in range {
                    match (listed.messages.get(index), &listed.more) {
                        (Some(mail), _) => mail_row(ui, listed, index, mail, actions),
                        (None, Some(more)) => more_line(ui, theme, more),
                        (None, None) => {}
                    }
                }
            });
    });
}

fn mail_row(
    ui: &mut Ui,
    listed: &Listed<'_>,
    index: usize,
    mail: &MailSummary,
    actions: &mut Vec<Action>,
) {
    let age = strings::mail_age(mail.received, listed.now);
    let row = MailRow::new(&mail.from, subject(&mail.subject), &age)
        .new_mail(!mail.read)
        .selected(listed.selected == Some(index));
    let response = ui.add(row);
    // The arrows move the focus from row to row: keep the focused one in
    // view, so the next one is drawn too.
    if response.gained_focus() {
        response.scroll_to_me(None);
    }
    if response.clicked() {
        actions.push(Action::ChooseMail(mail.id.clone()));
    }
}

/// "The newest 200 of 214. Older mail is in Mailpit.", in a row of its own.
fn more_line(ui: &mut Ui, theme: &Theme, more: &str) {
    let size = vec2(ui.available_width(), theme.size.row_max);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let inner = rect.shrink2(vec2(theme.space.s4, 0.0));
    let layout = Direction::CURRENT.row();
    let mut line = ui.new_child(UiBuilder::new().max_rect(inner).layout(layout));
    line.add(Text::caption(theme, more));
}

/// The chosen mail, or a line that says to choose one.
fn detail_card(ui: &mut Ui, theme: &Theme, listed: &Listed<'_>, actions: &mut Vec<Action>) {
    padded_card(ui, |ui| {
        let height = ui.available_height();
        ui.set_min_height(height);
        let Some(mail) = &listed.detail else {
            ui.centered_and_justified(|ui| ui.add(Text::caption(theme, strings::CHOOSE_MAIL)));
            return;
        };
        ui.spacing_mut().item_spacing.y = theme.space.s3;
        ui.add(Text::new(mail.subject, theme.text.heading, theme.color.ink));
        facts(ui, theme, mail);
        divider(ui);
        // The text takes what the button under it leaves, and scrolls.
        let button = theme.size.control + theme.space.s3;
        let room = (ui.available_height() - button).max(theme.size.row_max);
        ScrollArea::vertical()
            .id_salt(("mail_text", mail.id))
            .auto_shrink(false)
            .max_height(room)
            .show(ui, |ui| plain_text(ui, theme, mail));
        if ui.add(Button::primary(strings::OPEN_IN_BROWSER)).clicked() {
            actions.push(Action::OpenMailPage);
        }
    });
}

/// From, To, and Sent: a label column, then the value, cut short at the end
/// of the card.
fn facts(ui: &mut Ui, theme: &Theme, mail: &Detail<'_>) {
    let rows = [
        (strings::MAIL_FROM, mail.from),
        (strings::MAIL_TO, mail.to.as_str()),
        (strings::MAIL_SENT, mail.sent.as_str()),
    ];
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = theme.space.s1;
        for (label, value) in rows {
            let line = vec2(ui.available_width(), theme.text.body_sm.size);
            ui.allocate_ui_with_layout(line, Direction::CURRENT.row(), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let cell = vec2(MAIL_FACT_LABEL, line.y);
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

/// The mail's plain text, or what stands in its place. Nothing while it is
/// read.
fn plain_text(ui: &mut Ui, theme: &Theme, mail: &Detail<'_>) {
    let Some(text) = mail.text else {
        return;
    };
    if text.trim().is_empty() {
        ui.add(Text::new(
            strings::MAIL_NO_TEXT,
            theme.text.body,
            theme.color.mute,
        ));
        return;
    }
    let galley = laid_out(ui, theme, mail.id, text);
    // The one text of the window the user may select: a link to copy.
    ui.add(Label::new(galley).selectable(true));
}

/// A mail's text as laid out last, kept between frames.
#[derive(Clone)]
struct Laid {
    id: String,
    width: f32,
    dark: bool,
    galley: Arc<Galley>,
}

/// The text of the mail `id`, laid out to the width there is. Laid out
/// again only for another mail, width, or theme: a long mail costs one
/// layout, not one a frame (rules.md 10).
fn laid_out(ui: &Ui, theme: &Theme, id: &str, text: &str) -> Arc<Galley> {
    let key = Id::unique("mail_text_layout");
    let width = ui.available_width();
    let kept = ui.data(|data| data.get_temp::<Laid>(key)).filter(|laid| {
        laid.id == id && laid.dark == theme.dark && laid.width.to_bits() == width.to_bits()
    });
    if let Some(laid) = kept {
        return laid.galley;
    }
    // A mail's lines end in CR LF; egui would draw the CR.
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let format = TextFormat {
        font_id: theme.text.body.font_id(),
        color: theme.color.ink,
        line_height: Some(theme.text.body.size * theme.text.line_height_body),
        ..TextFormat::default()
    };
    let mut job = LayoutJob::single_section(text, format);
    job.wrap.max_width = width;
    let galley = ui.painter().layout_job(job);
    let laid = Laid {
        id: id.to_owned(),
        width,
        dark: theme.dark,
        galley: Arc::clone(&galley),
    };
    ui.data_mut(|data| data.insert_temp(key, laid));
    galley
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Pos2, RawInput, Rect, Role, pos2};
    use marsa_core::codes::MAIL_NOT_RUNNING;
    use marsa_core::runtimes::Runtime;
    use marsa_core::types::{MailText, RuntimeList, RuntimeVersions, ServiceStatus};

    use super::*;
    use crate::fixtures::connected;
    use crate::model::Update;
    use crate::theme::LIGHT;
    use crate::widgets::harness::{Harness, Tree};

    /// The time the tests count the ages to.
    const NOW: u64 = 1_800_000_000_000;

    fn summary(id: &str, minutes: u64, read: bool) -> MailSummary {
        MailSummary {
            id: id.into(),
            from: format!("App {id} <app@example.test>"),
            to: vec!["omar@example.test".into(), "team@example.test".into()],
            subject: format!("Subject {id}"),
            received: NOW - minutes * 60_000,
            size: 1024,
            read,
            snippet: String::new(),
            attachments: 0,
        }
    }

    fn mailpit(state: ServiceState) -> Update {
        Update::Services(vec![ServiceStatus {
            id: MAILPIT,
            state,
            pid: None,
            restarts: 0,
            since_ms: None,
        }])
    }

    /// Connected, Mailpit installed and running, and these mails.
    fn with(messages: Vec<MailSummary>, total: u64) -> Model {
        let mut model = connected();
        model.update(Update::Runtimes(RuntimeList {
            runtimes: vec![RuntimeVersions {
                runtime: Runtime::Mailpit,
                installed: vec!["1.27.10".into()],
                default: None,
            }],
        }));
        model.update(mailpit(ServiceState::Running));
        let unread = messages.iter().filter(|mail| !mail.read).count() as u64;
        model.update(Update::Mail(MailList {
            total,
            unread,
            messages,
        }));
        model
    }

    /// Three mails, the first two new.
    fn three() -> Model {
        with(
            vec![
                summary("a1", 0, false),
                summary("b2", 4, false),
                summary("c3", 120, true),
            ],
            3,
        )
    }

    fn text(id: &str, body: &str) -> Update {
        Update::MailText(MailText {
            id: id.into(),
            from: format!("App {id} <app@example.test>"),
            to: vec!["omar@example.test".into()],
            subject: format!("Subject {id}"),
            date: NOW - 60_000,
            text: body.into(),
            web_url: marsa_core::mail::web_url(id),
        })
    }

    /// `model` with the mail `id` opened and its text read.
    fn opened(mut model: Model, id: &str, body: &str) -> Model {
        model.apply_action(Action::ChooseMail(id.into()));
        model.update(text(id, body));
        model
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

    fn press(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn the_list_shows_each_mail_and_the_chosen_one_with_its_facts_and_text() {
        let model = opened(three(), "b2", "Hello!\r\nYour order is on its way.");
        let (_, tree) = frame_of(&model);
        assert!(tree.node_with_text(strings::MAIL_LEAD).is_some());
        for id in ["a1", "b2", "c3"] {
            let row = tree.node_with_label(&format!("App {id} <app@example.test>"));
            assert_eq!(row.map(|row| row.role()), Some(Role::ListBoxOption), "{id}");
        }
        assert!(tree.node_with_text("Subject b2").is_some());
        for label in [strings::MAIL_FROM, strings::MAIL_TO, strings::MAIL_SENT] {
            assert!(tree.node_with_text(label).is_some(), "{label}");
        }
        assert!(
            tree.node_with_text("omar@example.test, team@example.test")
                .is_some()
        );
        // Sent from the mail's own date: a minute before now.
        assert!(tree.node_with_text("1 min ago").is_some());
        // The text without its CRs.
        assert!(
            tree.node_with_text("Hello!\nYour order is on its way.")
                .is_some()
        );
        assert!(tree.node_with_label(strings::OPEN_IN_BROWSER).is_some());
        assert!(tree.node_with_text(strings::CHOOSE_MAIL).is_none());
    }

    #[test]
    fn a_new_mail_says_new_in_words_and_an_opened_one_does_not() {
        let model = opened(three(), "b2", "Hello!");
        let (_, tree) = frame_of(&model);
        let row = |id: &str| {
            tree.node_with_label(&format!("App {id} <app@example.test>"))
                .and_then(|node| node.description().map(str::to_owned))
                .expect(id)
        };
        assert_eq!(row("a1"), "New, Subject a1, just now");
        // Opened: no longer new, and the chosen one.
        assert_eq!(row("b2"), "Subject b2, 4 min ago");
        assert_eq!(row("c3"), "Subject c3, 2 h ago");
        let b2 = tree
            .node_with_label("App b2 <app@example.test>")
            .expect("b2");
        assert_eq!(b2.is_selected(), Some(true));
    }

    #[test]
    fn a_click_on_a_row_opens_that_mail() {
        let model = three();
        assert_eq!(
            click(&model, "App c3 <app@example.test>"),
            [Action::ChooseMail("c3".into())]
        );
        // Nothing chosen yet: the detail says to choose one.
        let (_, tree) = frame_of(&model);
        assert!(tree.node_with_text(strings::CHOOSE_MAIL).is_some());
        assert!(tree.node_with_label(strings::OPEN_IN_BROWSER).is_none());
    }

    #[test]
    fn a_mail_with_html_only_says_so_and_keeps_its_button() {
        let model = opened(three(), "a1", "  ");
        let (_, tree) = frame_of(&model);
        assert!(tree.node_with_text(strings::MAIL_NO_TEXT).is_some());
        assert_eq!(
            click(&model, strings::OPEN_IN_BROWSER),
            [Action::OpenMailPage]
        );
        // While the text is read, neither the text nor the line shows.
        let mut reading = three();
        reading.apply_action(Action::ChooseMail("a1".into()));
        let (_, shown) = frame_of(&reading);
        assert!(shown.node_with_text(strings::MAIL_NO_TEXT).is_none());
        assert!(shown.node_with_label(strings::OPEN_IN_BROWSER).is_some());
    }

    #[test]
    fn no_mail_yet_says_how_to_send_some_and_has_no_button() {
        let model = with(Vec::new(), 0);
        assert_eq!(view(&model, NOW).body, Body::Empty);
        let (_, tree) = frame_of(&model);
        assert!(tree.node_with_text(strings::NO_MAIL).is_some());
        assert!(tree.node_with_text(strings::NO_MAIL_HOW).is_some());
        assert!(tree.node_with_label(strings::START_MAILPIT).is_none());
        // Nothing to delete.
        let delete = tree
            .node_with_label(strings::DELETE_ALL_MAIL)
            .expect("Delete all");
        assert!(delete.is_disabled());
        assert!(click(&model, strings::DELETE_ALL_MAIL).is_empty());
    }

    #[test]
    fn mailpit_not_running_offers_to_start_it() {
        let mut model = three();
        model.update(Update::MailFailed(MAIL_NOT_RUNNING.into()));
        model.update(mailpit(ServiceState::Stopped));
        let (_, tree) = frame_of(&model);
        assert!(tree.node_with_text(strings::MAILPIT_STOPPED).is_some());
        assert!(tree.node_with_text(strings::MAILPIT_STOPPED_HOW).is_some());
        assert_eq!(
            click(&model, strings::START_MAILPIT),
            [Action::Service {
                service: MAILPIT,
                action: ServiceAction::Start
            }]
        );
        let shown = view(&model, NOW);
        assert_eq!(shown.delete_off, Some(strings::NO_MAIL_TO_DELETE));
        assert_eq!(shown.open_off, Some(strings::MAILPIT_NOT_RUNNING_HINT));
        // While it starts, the button waits.
        model.update(mailpit(ServiceState::Starting));
        assert_eq!(
            view(&model, NOW).body,
            Body::NotRunning {
                start_off: Some(strings::MAILPIT_STARTING)
            }
        );
    }

    #[test]
    fn before_the_first_read_the_services_table_tells_mailpit_stopped() {
        let mut model = connected();
        assert_eq!(view(&model, NOW).body, Body::Unread);
        model.update(mailpit(ServiceState::Stopped));
        assert!(matches!(view(&model, NOW).body, Body::NotRunning { .. }));
        model.update(mailpit(ServiceState::Running));
        assert_eq!(view(&model, NOW).body, Body::Unread);
        // A read that failed for another reason says why in place of the list.
        model.update(Update::MailFailed("mail_unavailable".into()));
        assert_eq!(
            view(&model, NOW).body,
            Body::Failed(strings::error_text("mail_unavailable"))
        );
        let offline = Model::default();
        assert_eq!(view(&offline, NOW).body, Body::Offline);
        let (_, tree) = frame_of(&offline);
        assert!(tree.node_with_text(strings::MAIL_OFFLINE).is_some());
    }

    #[test]
    fn delete_all_and_open_mailpit_work_while_there_is_mail() {
        let model = three();
        let shown = view(&model, NOW);
        assert_eq!((shown.delete_off, shown.open_off), (None, None));
        assert_eq!(
            click(&model, strings::DELETE_ALL_MAIL),
            [Action::AskDeleteMail]
        );
        assert_eq!(click(&model, strings::OPEN_MAILPIT), [Action::OpenMailpit]);
        // Pressed and confirmed: it waits for the daemon.
        let mut deleting = three();
        deleting.apply_action(Action::AskDeleteMail);
        deleting.apply_action(Action::ConfirmRemoval);
        let shown = view(&deleting, NOW);
        assert_eq!(shown.delete_off, Some(strings::DELETING_MAIL));
    }

    #[test]
    fn a_cut_list_says_so_and_only_the_rows_in_view_are_drawn() {
        let messages: Vec<MailSummary> = (0..200)
            .map(|n| summary(&format!("m{n}"), n, true))
            .collect();
        let model = with(messages, 214);
        let Body::List(listed) = view(&model, NOW).body else {
            panic!("a list");
        };
        assert_eq!(
            listed.more.as_deref(),
            Some(strings::mail_more(200, 214).as_str())
        );
        let (_, tree) = frame_of(&model);
        let rows = tree.labels_starting_with("App m").len();
        // 720 high: about a dozen rows of 54, never the 200.
        assert!((5..30).contains(&rows), "{rows} rows drawn");
        // The whole list fits: no caption.
        let few = three();
        let Body::List(short) = view(&few, NOW).body else {
            panic!("a list");
        };
        assert_eq!(short.more, None);
    }

    #[test]
    fn the_text_is_laid_out_once_for_each_mail() {
        let model = opened(three(), "a1", "Hello!");
        let harness = Harness::new(egui::Theme::Light);
        let mut first = None;
        for _ in 0..3 {
            harness.frame(input(), |ui| {
                draw(ui, &model);
            });
            let kept = harness
                .ctx
                .data(|data| data.get_temp::<Laid>(Id::unique("mail_text_layout")))
                .expect("a layout kept");
            let galley = first.get_or_insert_with(|| Arc::clone(&kept.galley));
            assert!(Arc::ptr_eq(galley, &kept.galley), "laid out again");
        }
        // Another mail: a new layout.
        let other = opened(three(), "b2", "Bye!");
        harness.frame(input(), |ui| {
            draw(ui, &other);
        });
        let kept = harness
            .ctx
            .data(|data| data.get_temp::<Laid>(Id::unique("mail_text_layout")))
            .expect("a layout kept");
        assert_eq!(kept.id, "b2");
    }

    #[test]
    fn the_list_is_as_wide_as_its_token_and_the_detail_fits_the_height() {
        let model = opened(three(), "a1", "Hello!");
        let (_, tree) = frame_of(&model);
        let bounds = |label: &str| {
            tree.node_with_label(label)
                .and_then(|node| node.bounds())
                .expect(label)
        };
        let row = bounds("App a1 <app@example.test>");
        let inner = f64::from(MAIL_LIST_WIDTH - LIGHT.size.border - LIGHT.size.border);
        assert!((row.width() - inner).abs() < 0.5, "{row:?}");
        assert!((row.height() - f64::from(LIGHT.size.row_max)).abs() < 0.5);
        // The detail is after the list, and its button inside the 720 of
        // the frame.
        let open = bounds(strings::OPEN_IN_BROWSER);
        assert!(open.x0 > row.x1, "{open:?}");
        assert!(open.y1 <= 720.0, "{open:?}");
    }

    #[test]
    fn the_keyboard_reaches_the_rows_and_the_arrows_move_between_them() {
        use egui::{Event, Key, Modifiers};
        let model = three();
        let harness = Harness::new(egui::Theme::Light);
        let press = |key: Key| RawInput {
            events: vec![Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            ..input()
        };
        harness.frame(input(), |ui| {
            draw(ui, &model);
        });
        // "Delete all…", "Open Mailpit", then the first row; down to the
        // second.
        for key in [Key::Tab, Key::Tab, Key::Tab, Key::ArrowDown] {
            harness.frame(press(key), |ui| {
                draw(ui, &model);
            });
        }
        let mut actions = Vec::new();
        harness.frame(press(Key::Enter), |ui| {
            actions.extend(draw(ui, &model));
        });
        assert_eq!(actions, [Action::ChooseMail("b2".into())]);
    }
}
