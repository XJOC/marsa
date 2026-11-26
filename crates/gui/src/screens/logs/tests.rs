//! Tests of the Logs screen: the view with no window, then the drawing.

use eframe::egui::{
    self, Event, Modifiers, MouseWheelSource, MouseWheelUnit, Pos2, RawInput, Rect, Shape,
    TouchPhase, pos2,
};
use marsa_core::types::{LogLine, ServiceState, ServiceStatus, Site, SiteList, SiteLogLine};

use super::*;
use crate::fixtures::{connected, site};
use crate::model::Update;
use crate::theme::{DARK, LIGHT};
use crate::widgets::harness::{Harness, Tree};

const INFO: &str = r#"{"level":"info","msg":"serving shop.test"}"#;
const WARN: &str = r#"{"level":"warn","msg":"certificate for docs.test is not requested"}"#;
const ERROR: &str = r#"{"level":"error","msg":"dial tcp 127.0.0.1:9000: connection refused"}"#;

fn service(id: ServiceId) -> ServiceStatus {
    ServiceStatus {
        id,
        state: ServiceState::Running,
        pid: None,
        restarts: 0,
        since_ms: None,
    }
}

fn line(service: ServiceId, seq: u64, text: &str) -> LogLine {
    LogLine {
        service,
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
        .map(|(seq, text)| line(ServiceId::Caddy, seq as u64, text))
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
        service(ServiceId::Caddy),
        service("php-fpm-8.3".parse::<ServiceId>().unwrap()),
        service("php-fpm-8.4".parse::<ServiceId>().unwrap()),
    ]));
    let view = view(
        &model,
        &LogSource::of("php-fpm-8.4".parse::<ServiceId>().unwrap()),
    );
    let names: Vec<_> = view.sources.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Caddy", "PHP-FPM 8.3", "PHP-FPM 8.4"]);
    assert_eq!(view.current, 2);
    assert_eq!(view.sources[0].source, LogSource::Caddy);
    assert_eq!(
        view.sources[2].source,
        LogSource::Service("php-fpm-8.4".parse::<ServiceId>().unwrap())
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
    let gone = super::view(
        &model,
        &LogSource::of("php-fpm-9.9".parse::<ServiceId>().unwrap()),
    );
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
        .map(|n| line(ServiceId::Caddy, n, &format!("line {n}")))
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
        service(ServiceId::Caddy),
        service("php-fpm-8.4".parse::<ServiceId>().unwrap()),
    ]));
    let (_, two) = settle(&Harness::new(egui::Theme::Light), &model, 3);
    assert!(two.node_with_label(strings::LOG_SOURCE).is_some());
    assert_eq!(
        click(&model, "PHP-FPM 8.4"),
        [Action::Navigate(Route::Logs(LogSource::Service(
            "php-fpm-8.4".parse::<ServiceId>().unwrap()
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
        ids[..4]
            .iter()
            .map(|id| service(id.parse().unwrap()))
            .collect(),
    ));
    let (_, four) = settle(&Harness::new(egui::Theme::Light), &model, 3);
    assert!(four.node_with_label(strings::LOG_SOURCE).is_some());
    model.update(Update::Services(
        ids.iter().map(|id| service(id.parse().unwrap())).collect(),
    ));
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
            "php-fpm-8.3".parse::<ServiceId>().unwrap()
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
                .any(
                    |shape| matches!(shape, Shape::Rect(rect) if rect.fill == theme.color.error_bg),
                )
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
                Shape::Rect(rect) if rect.fill == LIGHT.color.error_bg => Some((*clip, rect.rect)),
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
        ..line(ServiceId::Caddy, 0, "from stdout")
    };
    model.update(Update::Log(vec![out, line(ServiceId::Caddy, 1, ERROR)]));
    let (_, tree) = settle(&Harness::new(egui::Theme::Light), &model, 3);
    // The message has no JSON around it, and starts with its level.
    let label = strings::log_entry_label(
        LogLevel::Error,
        "dial tcp 127.0.0.1:9000: connection refused",
    );
    assert!(tree.node_with_text(&label).is_some(), "{label}");
    assert!(tree.node_with_text("Info: from stdout").is_some());
}

// -- a Laravel site's own log --

const ENTRY_INFO: &str = "[2026-11-24 10:15:01] local.INFO: started";
const ENTRY_ERROR: &str =
    r#"[2026-11-24 10:15:03] local.ERROR: boom {"exception":"[object] (Error(code: 0): boom)"#;
const TRACE: [&str; 3] = ["[stacktrace]", "#0 /app/Foo.php(12): bar()", r#""}"#];
const ENTRY_WARN: &str = "[2026-11-24 10:15:04] local.WARNING: slow";

fn shop() -> LogSource {
    LogSource::Site("shop".into())
}

/// Connected, with Caddy and one PHP pool, the Laravel site `shop` and the
/// plain PHP site `docs`, on `shop`'s log with these lines.
fn site_log(texts: &[&str]) -> Model {
    let mut model = connected();
    model.update(Update::Services(vec![
        service(ServiceId::Caddy),
        service("php-fpm-8.4".parse::<ServiceId>().unwrap()),
    ]));
    let docs = Site {
        kind: SiteKind::Php,
        ..site("docs", None, None)
    };
    model.update(Update::Sites(SiteList {
        sites: vec![site("shop", None, None), docs],
        ..SiteList::default()
    }));
    model.apply_action(Action::Navigate(Route::Logs(shop())));
    let lines = texts
        .iter()
        .zip(0..)
        .map(|(text, seq)| SiteLogLine {
            seq,
            text: (*text).into(),
        })
        .collect();
    model.update(Update::SiteLog {
        site: "shop".into(),
        lines,
    });
    model
}

/// An info entry, an error with its stack trace, and a warning.
fn laravel_log() -> Model {
    site_log(&[
        ENTRY_INFO,
        ENTRY_ERROR,
        TRACE[0],
        TRACE[1],
        TRACE[2],
        ENTRY_WARN,
    ])
}

/// Frames of `shop`'s log with no input, so the layout settles.
fn settle_on_shop(harness: &Harness, model: &Model) -> Tree {
    let mut tree = None;
    for _ in 0..3 {
        tree = Some(harness.frame(input(Vec::new()), |ui| {
            draw(ui, model, &shop());
        }));
    }
    tree.expect("a frame")
}

#[test]
fn each_laravel_site_has_its_own_log_after_the_services() {
    let model = laravel_log();
    let view = view(&model, &shop());
    let names: Vec<_> = view.sources.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Caddy", "PHP-FPM 8.4", "Laravel · shop"]);
    assert_eq!((view.current, &view.sources[2].source), (2, &shop()));
    assert_eq!(texts(lines(&view)).len(), 6);
    // Picked in the switch, it opens.
    assert_eq!(
        click(&model, "Laravel · shop"),
        [Action::Navigate(Route::Logs(shop()))]
    );
}

#[test]
fn a_sites_log_with_no_lines_says_so_and_a_refused_one_says_why() {
    let empty = site_log(&[]);
    assert_eq!(view(&empty, &shop()).body, Body::NoSiteLog);
    let tree = settle_on_shop(&Harness::new(egui::Theme::Light), &empty);
    assert!(tree.node_with_text(strings::SITE_LOG_EMPTY).is_some());
    assert!(tree.node_with_label("Follow").is_some());
    let mut refused = site_log(&[ENTRY_INFO]);
    refused.update(Update::SiteLogFailed {
        site: "shop".into(),
        code: "site_log_unsafe".into(),
    });
    assert_eq!(
        view(&refused, &shop()).body,
        Body::Refused("site_log_unsafe")
    );
    let tree = settle_on_shop(&Harness::new(egui::Theme::Light), &refused);
    let why = strings::error_text("site_log_unsafe");
    assert!(tree.node_with_text(&why).is_some(), "{why}");
    // With no daemon the lines kept show, and the sidebar says the rest.
    refused.update(Update::Reconnecting);
    assert!(matches!(view(&refused, &shop()).body, Body::Lines(_)));
    let mut offline = site_log(&[]);
    offline.update(Update::Reconnecting);
    assert_eq!(view(&offline, &shop()).body, Body::Offline);
}

#[test]
fn the_error_filter_keeps_an_errors_stack_trace_with_it() {
    let mut model = laravel_log();
    model.apply_action(Action::ToggleLogLevel(LogLevel::Info));
    model.apply_action(Action::ToggleLogLevel(LogLevel::Warning));
    let view = view(&model, &shop());
    assert_eq!(
        texts(lines(&view)),
        [ENTRY_ERROR, TRACE[0], TRACE[1], TRACE[2]]
    );
}

#[test]
fn an_entry_shows_its_stamp_and_message_and_its_trace_has_no_word_and_no_tint() {
    let model = laravel_log();
    let harness = Harness::new(egui::Theme::Light);
    let tree = settle_on_shop(&harness, &model);
    let message = r#"boom {"exception":"[object] (Error(code: 0): boom)"#;
    let label = strings::log_entry_label(LogLevel::Error, message);
    assert!(tree.node_with_text(&label).is_some(), "{label}");
    // A line of the trace is heard as itself, with no level.
    assert!(tree.node_with_text(TRACE[1]).is_some());
    let wrong = strings::log_entry_label(LogLevel::Error, TRACE[1]);
    assert!(tree.node_with_text(&wrong).is_none());
    let shapes = harness.shapes(|ui| {
        show(ui, &view(&model, &shop()), &LIGHT);
    });
    // One tinted row: the error's own, not its trace's.
    let tints = shapes
        .iter()
        .filter(|shape| matches!(shape, Shape::Rect(rect) if rect.fill == LIGHT.color.error_bg))
        .count();
    assert_eq!(tints, 1);
    // Each entry's stamp is in the time column, and in no message.
    let drawn: Vec<String> = shapes
        .iter()
        .filter_map(|shape| match shape {
            Shape::Text(text) => Some(text.galley.text().to_owned()),
            _ => None,
        })
        .collect();
    let stamps = drawn
        .iter()
        .filter(|text| text.starts_with("2026-11-24"))
        .count();
    assert_eq!(stamps, 3, "{drawn:?}");
    assert!(drawn.iter().any(|text| text == "started"), "{drawn:?}");
}
