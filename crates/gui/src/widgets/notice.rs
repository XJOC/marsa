//! Notices and progress (components.md, Notices and Progress).
//!
//! A notice is a 40 high bar at the bottom of the main area: an icon, one
//! sentence that repeats the verb of the action ("Unlinked shop."), and at
//! most one action ("Undo", "Show log"). A success closes by itself after 4
//! seconds; an error stays until the user closes it. A sentence too long for
//! one line wraps, and the bar grows with it, so no message is cut.
//!
//! The window keeps its results in the model and decides which one shows
//! (`shell::notices`). [`NoticeBar`] draws one notice, [`show_bar`] puts it at
//! the bottom of the main area, and [`Progress`] is the bar of a long task,
//! filled with `accent`.

use std::time::Duration;

use eframe::egui::{
    Align2, Area, Color32, Context, Frame, Id, Margin, Order, Pos2, Rect, Response, Role, Sense,
    Stroke, Ui, Vec2, Widget, WidgetInfo, pos2, vec2,
};

use super::icons::{Glyph, Icon};
use super::{Button, layout, slot, spot, whole};
use crate::strings;
use crate::theme::{Direction, PERCENT, TextToken, Theme, UNIT};

/// How long a success stays: 4 seconds.
pub const SUCCESS_LIFETIME: Duration = Duration::from_secs(4);

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    /// A check in `ok`. Closes by itself.
    Success,
    /// A cross in `error`. Stays until closed.
    Error,
}

impl NoticeKind {
    /// How long a notice of this kind stays; `None` until closed.
    pub const fn lifetime(self) -> Option<Duration> {
        match self {
            Self::Success => Some(SUCCESS_LIFETIME),
            Self::Error => None,
        }
    }
}

/// One notice: what happened, in one sentence, and an action if there is
/// one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub text: String,
    /// The label of the one action: "Undo".
    pub action: Option<String>,
    /// Why the action cannot be pressed now (another job runs), if it
    /// cannot: the button is drawn off, and the reason is its tooltip.
    pub action_off: Option<String>,
    /// What the action does, in full, over the button while the pointer
    /// rests on it: the label may show only the end of a long path.
    pub action_hint: Option<String>,
}

impl Notice {
    pub fn success(text: impl Into<String>) -> Self {
        Self {
            kind: NoticeKind::Success,
            text: text.into(),
            action: None,
            action_off: None,
            action_hint: None,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            kind: NoticeKind::Error,
            text: text.into(),
            action: None,
            action_off: None,
            action_hint: None,
        }
    }

    pub fn with_action(mut self, label: impl Into<String>) -> Self {
        self.action = Some(label.into());
        self
    }

    /// The tooltip of the action while it is on.
    pub fn with_action_hint(mut self, hint: impl Into<String>) -> Self {
        self.action_hint = Some(hint.into());
        self
    }

    /// The action is off while `reason` holds (with no reason, it is on).
    pub fn with_action_off(mut self, reason: Option<&str>) -> Self {
        self.action_off = reason.map(str::to_owned);
        self
    }
}

/// Draws `notice` over the bottom of `main_area`, in its middle, and returns
/// what the user pressed. A window whose notices live in its own state (the
/// shell) draws the one it holds with this.
pub fn show_bar(ctx: &Context, main_area: Rect, notice: &Notice) -> Option<Reaction> {
    let theme = Theme::of(ctx);
    let anchor = main_area.center_bottom() - vec2(0.0, theme.space.s4);
    let widest = (main_area.width() - theme.space.screen_side - theme.space.screen_side)
        .min(theme.size.notice_max);
    Area::new(Id::unique("notice_bar"))
        .order(Order::Foreground)
        .pivot(Align2::CENTER_BOTTOM)
        .fixed_pos(anchor)
        .show(ctx, |ui| {
            ui.set_max_width(widest);
            NoticeBar::new(notice).show(ui)
        })
        .inner
}

/// What the user did to a notice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reaction {
    /// Pressed the close button.
    Closed,
    /// Pressed the notice's action.
    Action,
}

/// The icon and its color for a kind of notice.
pub fn mark(theme: &Theme, kind: NoticeKind) -> (Icon, Color32) {
    match kind {
        NoticeKind::Success => (Icon::Check, theme.color.ok),
        NoticeKind::Error => (Icon::Cross, theme.color.error),
    }
}

/// One notice as a bar: 40 high, `surface`, `elevation.1`. A sentence that
/// does not fit on one line wraps, and the bar grows by whole lines, with the
/// icon and the buttons level with the first line.
#[derive(Clone, Copy, Debug)]
#[must_use = "a notice is drawn only when it is shown"]
pub struct NoticeBar<'a> {
    notice: &'a Notice,
}

/// The action and the close button of `notice`, if it has them.
fn controls(notice: &Notice) -> (Option<Button<'_>>, Option<Button<'static>>) {
    let action = notice.action.as_deref().map(|label| {
        let off = notice.action_off.as_deref();
        Button::quiet(label)
            .in_row()
            .disabled_if(off.is_some(), off.unwrap_or_default())
    });
    let close =
        (notice.kind == NoticeKind::Error).then(|| Button::icon(Icon::Cross, strings::CLOSE));
    (action, close)
}

/// What the icon and the buttons take of a bar `available` wide, each with
/// the gap before it, and what is left for the sentence.
fn room(
    theme: &Theme,
    available: f32,
    action_width: Option<f32>,
    close_width: Option<f32>,
) -> (f32, f32) {
    let (gap, side) = (theme.space.s2, theme.size.icon_inline);
    let reserved = side
        + [action_width, close_width]
            .into_iter()
            .flatten()
            .map(|width| gap + width)
            .sum::<f32>();
    // The bar's width less its margins and what is not the sentence.
    let margin = theme.space.s3 + theme.size.border;
    (
        reserved,
        (available - margin - margin - reserved - gap).max(0.0),
    )
}

impl<'a> NoticeBar<'a> {
    pub const fn new(notice: &'a Notice) -> Self {
        Self { notice }
    }

    /// How many lines the sentence takes in a bar `available` wide. A notice
    /// is at most two: the words that explain more belong to
    /// the screen.
    pub fn lines(&self, ui: &Ui, available: f32) -> usize {
        let theme = Theme::of(ui.ctx());
        let (action, close) = controls(self.notice);
        let room = room(
            theme,
            available,
            action.as_ref().map(|button| button.width(ui)),
            close.as_ref().map(|button| button.width(ui)),
        )
        .1;
        ui.painter()
            .layout(
                self.notice.text.clone(),
                theme.text.body_sm.font_id(),
                theme.color.ink,
                room,
            )
            .rows
            .len()
    }

    /// Draws the bar in `ui`. `Some` when the user pressed a button in it.
    /// An error has a close button; a success closes by itself.
    pub fn show(self, ui: &mut Ui) -> Option<Reaction> {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let notice = self.notice;
        let (icon, color) = mark(theme, notice.kind);
        let border = theme.size.border;
        let frame = Frame::new()
            .fill(theme.color.surface)
            .stroke(Stroke::new(border, theme.color.line))
            .corner_radius(theme.radius.md)
            .shadow(theme.elevation.menu)
            .inner_margin(Margin::symmetric(whole(theme.space.s3), 0));
        // The height of one line of content: the bar less its borders.
        let zone = theme.size.notice - border - border;
        let (gap, side) = (theme.space.s2, theme.size.icon_inline);
        let (action, close) = controls(notice);
        let action_width = action.as_ref().map(|button| button.width(ui));
        let close_width = close.as_ref().map(|button| button.width(ui));
        let (reserved, room) = room(theme, ui.available_width(), action_width, close_width);
        let font = theme.text.body_sm.font_id();
        let text = notice.text.clone();
        let one_line = ui
            .painter()
            .layout_no_wrap(text.clone(), font.clone(), theme.color.ink);
        let galley = if one_line.size().x <= room {
            one_line.clone()
        } else {
            ui.painter().layout(text, font, theme.color.ink, room)
        };
        // The first line sits level with the icon and the buttons: the space
        // above it is what is left of the zone's height, shared.
        let line_zone = Rect::from_min_size(Pos2::ZERO, vec2(one_line.size().x, zone));
        let first_line = Align2::LEFT_CENTER.align_size_within_rect(one_line.size(), line_zone);
        let pad = first_line.top().max(0.0);
        let height = (galley.size().y + pad + pad).max(zone);
        let width = reserved + gap + galley.size().x;
        let mut reaction = None;
        let shown = frame.show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
            // `size` at `offset` from the start, level with the first line.
            let centered = |size: Vec2, offset: f32| {
                let x = slot(direction, rect, offset, size.x);
                let line = spot(x, rect.top(), size.x, zone);
                Align2::LEFT_CENTER.align_size_within_rect(size, line)
            };
            ui.put(
                centered(vec2(side, side), 0.0),
                Glyph::new(icon).size(side).color(color),
            );
            let x = slot(direction, rect, side + gap, galley.size().x);
            ui.painter()
                .galley(pos2(x, rect.top() + pad), galley, theme.color.ink);
            // From the end: the close button, then the action before it.
            let mut offset = width;
            let mut place = |width: f32, height: f32| {
                offset -= width;
                let at = centered(vec2(width, height), offset);
                offset -= gap;
                at
            };
            let close_at = close_width.map(|w| place(w, theme.size.icon_button));
            let action_at = action_width.map(|w| place(w, theme.size.control_in_row));
            // The action comes first in the Tab order.
            if let (Some(button), Some(at)) = (action, action_at) {
                let response = ui.put(at, button);
                // An action that is off says why instead.
                if let (Some(hint), None) = (&notice.action_hint, &notice.action_off) {
                    response.clone().on_hover_text(hint);
                }
                if response.clicked() {
                    reaction = Some(Reaction::Action);
                }
            }
            if let (Some(button), Some(at)) = (close, close_at)
                && ui.put(at, button).clicked()
            {
                reaction = Some(Reaction::Closed);
            }
        });
        // An error is an alert; a success is a status a screen reader reads
        // when it appears.
        let role = match notice.kind {
            NoticeKind::Error => Role::Alert,
            NoticeKind::Success => Role::Status,
        };
        let text = notice.text.as_str();
        shown
            .response
            .widget_info(|| WidgetInfo::labeled(role, true, text));
        reaction
    }
}

/// The part of a bar the share `fraction` fills, from the start of the line
/// (the progress bar fills from the right in a right-to-left language).
pub fn fill_rect(direction: Direction, track: Rect, fraction: f32) -> Rect {
    let width = track.width() * share(fraction);
    let x = slot(direction, track, 0.0, width);
    spot(x, track.top(), width, track.height())
}

/// `fraction` kept between nothing and a whole; not-a-number is nothing.
pub fn share(fraction: f32) -> f32 {
    if fraction.is_nan() {
        0.0
    } else {
        fraction.clamp(0.0, UNIT)
    }
}

/// The progress of a task that takes over a second: the words ("Downloading
/// PHP 8.4.27: 18 of 31 MB") over a 6 high bar filled with `accent`. With no
/// fraction the task has no measure yet: a spinner and the words.
#[derive(Clone, Copy, Debug)]
#[must_use = "a progress bar is drawn only when it is added to a Ui"]
pub struct Progress<'a> {
    words: &'a str,
    fraction: Option<f32>,
}

impl<'a> Progress<'a> {
    pub const fn new(words: &'a str, fraction: Option<f32>) -> Self {
        Self { words, fraction }
    }
}

impl Widget for Progress<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let token = TextToken {
            size: theme.text.body_sm.size,
            face: theme.text.body_sm.face,
        };
        let galley = layout(ui.painter(), self.words, token, Color32::PLACEHOLDER);
        let width = ui.available_width();
        let bar = theme.size.progress;
        let gap = theme.space.s2;
        let icon = theme.size.icon_inline;
        let height = match self.fraction {
            Some(_) => galley.size().y + gap + bar,
            None => galley.size().y.max(icon),
        };
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let line = |offset: f32, size: f32| {
                let x = slot(direction, rect, offset, size);
                spot(x, rect.top(), size, galley.size().y)
            };
            match self.fraction {
                Some(fraction) => {
                    let words = line(0.0, galley.size().x);
                    painter.galley(words.min, galley, theme.color.ink2);
                    let track = spot(rect.left(), rect.bottom() - bar, width, bar);
                    let corner = theme.radius.full;
                    painter.rect_filled(track, corner, theme.color.selected);
                    let fill = fill_rect(direction, track, fraction);
                    painter.rect_filled(fill, corner, theme.color.accent);
                }
                None => {
                    let spot = spot(slot(direction, rect, 0.0, icon), rect.top(), icon, height);
                    Icon::Spinner.paint(painter, theme, spot, theme.color.accent);
                    let words = line(icon + gap, galley.size().x);
                    let at = Align2::LEFT_CENTER.align_size_within_rect(
                        galley.size(),
                        Rect::from_min_size(words.min, vec2(galley.size().x, height)),
                    );
                    painter.galley(at.min, galley, theme.color.ink2);
                }
            }
        }
        let words = self.words;
        let value = self
            .fraction
            .map(|fraction| f64::from(share(fraction) * PERCENT));
        response.widget_info(|| {
            let mut info = WidgetInfo::labeled(Role::ProgressIndicator, true, words);
            info.value = value;
            info
        });
        response
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::epaint::ColorMode;
    use eframe::egui::{self, Shape, pos2};

    use super::*;
    use crate::theme::tests::{TEXT, UI, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::Harness;

    const THEMES: [(&str, &Theme, egui::Theme); 2] = [
        ("light", &LIGHT, egui::Theme::Light),
        ("dark", &DARK, egui::Theme::Dark),
    ];

    #[test]
    fn only_a_success_has_a_lifetime_and_it_is_four_seconds() {
        assert_eq!(SUCCESS_LIFETIME, Duration::from_secs(4));
        assert_eq!(NoticeKind::Success.lifetime(), Some(SUCCESS_LIFETIME));
        assert_eq!(NoticeKind::Error.lifetime(), None);
    }

    /// The node of a notice bar, and its action and close buttons.
    fn bar_tree(harness: &Harness, notice: &Notice) -> crate::widgets::harness::Tree {
        harness.accessibility(|ui| {
            NoticeBar::new(notice).show(ui);
        })
    }

    #[test]
    fn a_bar_is_40_high_with_or_without_an_action() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        for notice in [
            Notice::success("Unlinked shop."),
            Notice::success("Unlinked shop.").with_action("Undo"),
            Notice::error("Caddy stopped.").with_action("Show log"),
        ] {
            let tree = bar_tree(&harness, &notice);
            let bar = tree.node_with_label(&notice.text).expect("the bar");
            let bounds = bar.bounds().expect("bounds");
            let height = (bounds.y1 - bounds.y0) as f32;
            assert!(
                (height - theme.size.notice).abs() < 1.5,
                "{} is {height} high",
                notice.text
            );
        }
        assert_eq!(theme.size.notice, 40.0);
    }

    #[test]
    fn an_error_has_a_close_button_a_success_has_none_and_an_action_is_a_quiet_button() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = bar_tree(
            &harness,
            &Notice::success("Unlinked shop.").with_action("Undo"),
        );
        assert!(tree.node_with_label("Close").is_none());
        assert_eq!(
            tree.node_with_label("Undo").expect("action").role(),
            egui::Role::Button
        );
        let tree = bar_tree(
            &harness,
            &Notice::error("Caddy stopped.").with_action("Show log"),
        );
        assert_eq!(
            tree.node_with_label("Close").expect("close").role(),
            egui::Role::Button
        );
        assert!(tree.node_with_label("Show log").is_some());
        let tree = bar_tree(&harness, &Notice::error("Caddy stopped."));
        assert!(tree.node_with_label("Close").is_some());
    }

    #[test]
    fn a_screen_reader_hears_an_error_as_an_alert_and_a_success_as_a_status() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = bar_tree(&harness, &Notice::error("Caddy stopped."));
        assert_eq!(
            tree.node_with_label("Caddy stopped.")
                .expect("error")
                .role(),
            egui::Role::Alert
        );
        let tree = bar_tree(&harness, &Notice::success("Unlinked shop."));
        assert_eq!(
            tree.node_with_label("Unlinked shop.")
                .expect("success")
                .role(),
            egui::Role::Status
        );
    }

    #[test]
    fn each_kind_has_its_own_icon_and_color_and_they_read_on_the_bar() {
        for (name, theme, _) in THEMES {
            let (ok_icon, ok) = mark(theme, NoticeKind::Success);
            let (error_icon, error) = mark(theme, NoticeKind::Error);
            assert_ne!(ok_icon, error_icon, "R8: not by color alone");
            assert_eq!((ok, error), (theme.color.ok, theme.color.error));
            for color in [ok, error] {
                assert!(contrast(color, theme.color.surface) >= UI, "{name} icon");
            }
            let c = &theme.color;
            assert!(contrast(c.ink, c.surface) >= TEXT, "{name} sentence");
            assert!(contrast(c.accent, c.surface) >= TEXT, "{name} action");
            assert!(contrast(c.mute, c.surface) >= UI, "{name} close");
        }
    }

    /// One frame of the bar over a main area, with a click at `at` if there
    /// is one. What the user did; a notice the user answered is gone.
    fn show(
        harness: &Harness,
        notice: &mut Option<Notice>,
        area: Rect,
        at: Option<egui::Pos2>,
    ) -> Option<Reaction> {
        let mut reaction = None;
        let mut draw = |ui: &mut egui::Ui| {
            if let Some(shown) = notice.as_ref() {
                reaction = show_bar(ui.ctx(), area, shown).or(reaction);
            }
        };
        match at {
            Some(at) => harness.click(at, &mut draw),
            None => harness.run(&mut draw),
        }
        if reaction.is_some() {
            *notice = None;
        }
        reaction
    }

    #[test]
    fn the_action_and_the_close_button_report_and_close_the_notice() {
        let area = Rect::from_min_size(pos2(200.0, 0.0), egui::vec2(800.0, 600.0));
        for (label, notice, expected) in [
            (
                "Undo",
                Notice::success("Unlinked shop.").with_action("Undo"),
                Reaction::Action,
            ),
            ("Close", Notice::error("Caddy stopped."), Reaction::Closed),
        ] {
            let harness = Harness::new(egui::Theme::Light);
            let mut notices = Some(notice);
            // The first frames size the bar and fade it in.
            for _ in 0..20 {
                show(&harness, &mut notices, area, None);
            }
            let tree = harness.accessibility(|ui| {
                show_bar(ui.ctx(), area, notices.as_ref().expect("a notice"));
            });
            let bounds = tree
                .node_with_label(label)
                .and_then(|n| n.bounds())
                .expect(label);
            let at = pos2(
                ((bounds.x0 + bounds.x1) / 2.0) as f32,
                ((bounds.y0 + bounds.y1) / 2.0) as f32,
            );
            let reaction = show(&harness, &mut notices, area, Some(at));
            assert_eq!(reaction, Some(expected), "{label}");
            assert!(notices.is_none(), "{label} closes the notice");
        }
    }

    #[test]
    fn the_bar_sits_at_the_bottom_and_the_middle_of_the_main_area() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let area = Rect::from_min_size(pos2(224.0, 0.0), egui::vec2(896.0, 720.0));
        let mut notices = Some(Notice::success("Unlinked shop."));
        for _ in 0..20 {
            show(&harness, &mut notices, area, None);
        }
        let tree = harness.accessibility(|ui| {
            show_bar(ui.ctx(), area, notices.as_ref().expect("a notice"));
        });
        let bar = tree
            .node_with_label("Unlinked shop.")
            .and_then(|n| n.bounds())
            .expect("bar");
        let middle = ((bar.x0 + bar.x1) / 2.0) as f32;
        assert!((middle - area.center().x).abs() < 1.5, "centered: {middle}");
        let bottom = bar.y1 as f32;
        assert!(
            (bottom - (area.bottom() - theme.space.s4)).abs() < 3.0,
            "{bottom}"
        );
        assert!((bar.x1 - bar.x0) as f32 <= theme.size.notice_max + 4.0);
    }

    /// The bounds of a node, as a rectangle.
    fn rect_of(tree: &crate::widgets::harness::Tree, label: &str) -> Rect {
        let bounds = tree
            .node_with_label(label)
            .and_then(|node| node.bounds())
            .unwrap_or_else(|| panic!("no {label}"));
        Rect::from_min_max(
            pos2(bounds.x0 as f32, bounds.y0 as f32),
            pos2(bounds.x1 as f32, bounds.y1 as f32),
        )
    }

    #[test]
    fn a_sentence_too_long_for_one_line_wraps_and_the_bar_grows_with_the_buttons_in_it() {
        let theme = &LIGHT;
        let long = "shop.test comes from the parked folder /Users/someone/Code, so it cannot \
                    be unlinked alone. Forget that folder to stop serving it and the folder's \
                    other sites.";
        let area = Rect::from_min_size(pos2(224.0, 0.0), egui::vec2(896.0, 720.0));
        let harness = Harness::new(egui::Theme::Light);
        let mut notices = Some(Notice::error(long).with_action("Forget /Users/someone/Code…"));
        for _ in 0..20 {
            show(&harness, &mut notices, area, None);
        }
        let tree = harness.accessibility(|ui| {
            show_bar(ui.ctx(), area, notices.as_ref().expect("a notice"));
        });
        let bar = rect_of(&tree, long);
        // Not cut: the whole sentence is the bar's name, on more than one line.
        assert!(bar.height() > theme.size.notice + 10.0, "{}", bar.height());
        assert!(
            bar.width() <= theme.size.notice_max + 4.0,
            "{}",
            bar.width()
        );
        // The bar stays at the bottom, growing up.
        assert!((bar.bottom() - (area.bottom() - theme.space.s4)).abs() < 3.0);
        // The buttons are inside the bar, level with its first line.
        for label in ["Forget /Users/someone/Code…", "Close"] {
            let button = rect_of(&tree, label);
            assert!(bar.contains_rect(button), "{label} is inside the bar");
            assert!(
                button.center().y < bar.top() + theme.size.notice,
                "{label} is level with the first line"
            );
        }
        // And they still work.
        let at = rect_of(&tree, "Close").center();
        assert_eq!(
            show(&harness, &mut notices, area, Some(at)),
            Some(Reaction::Closed)
        );
    }

    #[test]
    fn an_action_that_is_off_says_why_and_does_nothing() {
        let area = Rect::from_min_size(pos2(224.0, 0.0), egui::vec2(896.0, 720.0));
        let harness = Harness::new(egui::Theme::Light);
        let notice = Notice::error("shop.test cannot be unlinked alone.")
            .with_action("Forget /code…")
            .with_action_off(Some("Another removal runs."));
        assert_eq!(notice.action_off.as_deref(), Some("Another removal runs."));
        let mut notices = Some(notice);
        for _ in 0..20 {
            show(&harness, &mut notices, area, None);
        }
        let tree = harness.accessibility(|ui| {
            show_bar(ui.ctx(), area, notices.as_ref().expect("a notice"));
        });
        let button = tree.node_with_label("Forget /code…").expect("the action");
        assert!(button.is_disabled(), "drawn off");
        let at = rect_of(&tree, "Forget /code…").center();
        assert_eq!(show(&harness, &mut notices, area, Some(at)), None);
        assert!(notices.is_some(), "the notice stays");
        // With no reason the action is on.
        let on = Notice::error("x")
            .with_action("Forget")
            .with_action_off(None);
        assert_eq!(on.action_off, None);
    }

    #[test]
    fn the_fill_starts_at_the_start_of_the_bar_and_grows_with_the_share() {
        let track = Rect::from_min_size(pos2(10.0, 50.0), egui::vec2(200.0, 6.0));
        let ltr = fill_rect(Direction::Ltr, track, 0.25);
        assert_eq!((ltr.left(), ltr.width(), ltr.height()), (10.0, 50.0, 6.0));
        // Right to left: it fills from the right (rtl.md).
        let rtl = fill_rect(Direction::Rtl, track, 0.25);
        assert_eq!((rtl.right(), rtl.width()), (210.0, 50.0));
        assert_eq!(fill_rect(Direction::Ltr, track, 1.0), track);
        assert_eq!(fill_rect(Direction::Ltr, track, 0.0).width(), 0.0);
    }

    #[test]
    fn a_share_is_kept_between_nothing_and_a_whole() {
        assert_eq!(share(-1.0), 0.0);
        assert_eq!(share(f32::NAN), 0.0);
        assert_eq!(share(0.4), 0.4);
        assert_eq!(share(3.0), 1.0);
        assert_eq!(share(f32::INFINITY), 1.0);
    }

    fn progress_shapes(theme: egui::Theme, fraction: Option<f32>) -> (Rect, Vec<Shape>) {
        let harness = Harness::new(theme);
        let mut rect = Rect::NOTHING;
        let shapes = harness.shapes(|ui| {
            rect = ui
                .add(Progress::new(
                    "Downloading PHP 8.4.27: 18 of 31 MB",
                    fraction,
                ))
                .rect;
        });
        (rect, shapes)
    }

    #[test]
    fn the_fill_is_accent_on_a_selected_track_6_high_and_as_wide_as_the_share() {
        for (name, theme, active) in THEMES {
            let (rect, shapes) = progress_shapes(active, Some(0.4));
            let rects: Vec<_> = shapes
                .iter()
                .filter_map(|shape| match shape {
                    Shape::Rect(r) => Some(r.clone()),
                    _ => None,
                })
                .collect();
            let fill = rects
                .iter()
                .find(|r| r.fill == theme.color.accent)
                .unwrap_or_else(|| panic!("{name}: the fill"));
            let track = rects
                .iter()
                .find(|r| r.fill == theme.color.selected)
                .unwrap_or_else(|| panic!("{name}: the track"));
            assert_eq!(track.rect.height(), theme.size.progress);
            assert_eq!(theme.size.progress, 6.0);
            assert_eq!(track.rect.width(), rect.width());
            assert!((fill.rect.width() - 0.4 * track.rect.width()).abs() < 0.01);
            assert_eq!(fill.rect.left(), track.rect.left());
            assert_eq!(
                fill.corner_radius,
                egui::CornerRadius::same(theme.radius.full)
            );
            // The words are above the bar.
            assert!(rect.height() > theme.size.progress);
            // The fill stands out from the track and from the card under it.
            assert!(
                contrast(theme.color.accent, theme.color.selected) >= UI,
                "{name}"
            );
            assert!(
                contrast(theme.color.accent, theme.color.surface) >= UI,
                "{name}"
            );
            assert!(
                contrast(theme.color.ink2, theme.color.surface) >= TEXT,
                "{name}"
            );
        }
    }

    #[test]
    fn a_task_with_no_measure_shows_a_spinner_and_its_words_and_no_bar() {
        let (rect, shapes) = progress_shapes(egui::Theme::Light, None);
        let bar = shapes.iter().any(|shape| matches!(shape, Shape::Rect(r) if r.fill == LIGHT.color.accent || r.fill == LIGHT.color.selected));
        assert!(!bar);
        assert!(rect.height() < 2.0 * LIGHT.size.icon_inline + LIGHT.size.progress);
        // The spinner is an arc in the accent color.
        let accent = ColorMode::Solid(LIGHT.color.accent);
        let spinner = shapes
            .iter()
            .any(|shape| matches!(shape, Shape::Path(path) if path.stroke.color == accent));
        assert!(spinner);
    }

    #[test]
    fn a_screen_reader_hears_the_words_and_the_percent() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            ui.add(Progress::new(
                "Downloading PHP 8.4.27: 18 of 31 MB",
                Some(0.58),
            ));
            ui.add(Progress::new("Starting Caddy", None));
        });
        let node = tree
            .node_with_label("Downloading PHP 8.4.27: 18 of 31 MB")
            .expect("progress");
        assert_eq!(node.role(), egui::Role::ProgressIndicator);
        assert_eq!(node.numeric_value().map(f64::round), Some(58.0));
        let spinner = tree.node_with_label("Starting Caddy").expect("no measure");
        assert_eq!(spinner.numeric_value(), None);
    }
}
