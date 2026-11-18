//! The small widgets of the design (`docs/design/components.md`): icons,
//! tiles, buttons, the switch, the segmented control, badges, the text
//! field, and the sidebar item. They are the only place that draws these; a screen adds them with
//! `ui.add(...)` and never reaches for a raw egui button (rules.md 3).
//! [Laravel: Blade components; each takes its props with a builder and
//! returns what the user did.]
//!
//! Rules every widget here follows:
//!
//! - Colors, sizes, and radii come from [`Theme`] only (rules.md 2). The
//!   active theme is `Theme::of(ui.ctx())`, one `&'static` read, no
//!   allocation.
//! - A widget never reads the model and never sends a command. It returns a
//!   [`Response`], or a small value such as the option a segmented control
//!   chose.
//! - Every widget sets [`Response::widget_info`], so a screen reader hears
//!   its name, its role, and its state.
//! - Every state of components.md exists: rest, hover, pressed, focused (the
//!   ring of `tokens.md`, never removed), and disabled, drawn at 55% opacity
//!   with a tooltip that says why.
//! - Nothing draws a blur or animates: flat fills and strokes (rules.md 10).

pub mod badge;
pub mod button;
pub mod dialog;
pub mod field;
// Debug builds only: the page the design review reads (task M5-D2). A
// release build has no gallery and reads no extra environment variable.
#[cfg(debug_assertions)]
pub mod gallery;
pub mod icons;
pub mod nav;
pub mod notice;
pub mod segmented;
pub mod status;
pub mod switch;
pub mod table;
pub mod tile;
pub mod version;

use std::sync::Arc;

use eframe::egui::text::Galley;
use eframe::egui::{
    Color32, CornerRadius, Id, Painter, Rect, Response, Sense, Stroke, StrokeKind, Ui, accesskit,
};

use crate::theme::{Direction, TextToken, Theme};

pub use badge::{CountBadge, RowBadge};
pub use button::{Button, Kind as ButtonKind, QuietTone};
pub use dialog::{Answer, ConfirmDialog};
pub use field::Field;
pub use icons::{Glyph, Icon};
pub use nav::NavItem;
pub use notice::{Notice, NoticeBar, NoticeKind, Notices, Progress, Reaction};
pub use segmented::Segmented;
pub use status::{
    Health, PathStep, PillState, RequestPath, StatePill, StatusDot, StatusSentence, StepState,
};
pub use switch::Switch;
pub use table::{RowHeight, Table, TableColumn, TableOutput};
pub use tile::{Monogram, SectionTile, TileSize};
pub use version::{PickerSize, Setting, VersionMenu, VersionPick, VersionPicker};

/// How a pointer touches a control now: the states of components.md that
/// change its fill.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Touch {
    #[default]
    Rest,
    Hover,
    /// The pointer button is down on the control.
    Pressed,
}

impl Touch {
    /// A disabled control is always at rest: it has no hover or pressed look.
    pub fn of(response: &Response) -> Self {
        if !response.enabled() {
            Self::Rest
        } else if response.is_pointer_button_down_on() {
            Self::Pressed
        } else if response.hovered() {
            Self::Hover
        } else {
            Self::Rest
        }
    }
}

/// How a control is drawn: the pointer's touch, and whether the keyboard
/// focus ring shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Look {
    pub touch: Touch,
    pub focused: bool,
}

impl Look {
    pub fn of(response: &Response) -> Self {
        Self {
            touch: Touch::of(response),
            focused: response.has_focus(),
        }
    }

    pub const fn rest() -> Self {
        Self {
            touch: Touch::Rest,
            focused: false,
        }
    }

    pub const fn hover() -> Self {
        Self {
            touch: Touch::Hover,
            focused: false,
        }
    }

    pub const fn pressed() -> Self {
        Self {
            touch: Touch::Pressed,
            focused: false,
        }
    }

    pub const fn focused() -> Self {
        Self {
            touch: Touch::Rest,
            focused: true,
        }
    }
}

/// The fill of a control a state moves toward `ink`: 6% on hover, 12% when
/// pressed (components.md, Buttons).
pub fn touched(theme: &Theme, rest: Color32, touch: Touch) -> Color32 {
    match touch {
        Touch::Rest => rest,
        Touch::Hover => theme.shade(rest, theme.state.hover),
        Touch::Pressed => theme.shade(rest, theme.state.pressed),
    }
}

/// The fill of a control that has none at rest, such as a quiet button, an
/// icon button, or a sidebar item: `selected` at 50% under the pointer, and
/// `selected` when pressed. Text on it keeps its contrast, which moving the
/// fill toward `ink` would not.
pub fn quiet_fill(theme: &Theme, touch: Touch) -> Option<Color32> {
    match touch {
        Touch::Rest => None,
        Touch::Hover => Some(theme.color.selected.gamma_multiply(theme.state.row_hover)),
        Touch::Pressed => Some(theme.color.selected),
    }
}

/// A margin in whole points from a space token (the tokens are whole
/// numbers, and egui's margins are `i8`).
pub fn whole(points: f32) -> i8 {
    points as i8
}

/// The focus ring: 2 px of `accent`, 2 px outside the control (tokens.md,
/// Focus). `radius` is the control's own corner.
pub fn paint_focus_ring(painter: &Painter, theme: &Theme, rect: Rect, radius: u8) {
    // The ring follows the corner: its radius grows by the gap.
    let gap = theme.focus.offset;
    let corner = radius.saturating_add(gap.round() as u8);
    painter.rect_stroke(
        rect.expand(gap),
        CornerRadius::same(corner),
        Stroke::new(theme.focus.width, theme.color.accent),
        StrokeKind::Outside,
    );
}

/// The x of the left edge of an item `width` wide that sits `offset` from
/// the start of `within`: on the left in a left-to-right language, on the
/// right in a right-to-left one (rules.md 13).
pub fn slot(direction: Direction, within: Rect, offset: f32, width: f32) -> f32 {
    match direction {
        Direction::Ltr => within.left() + offset,
        Direction::Rtl => within.right() - offset - width,
    }
}

/// `text` laid out on one line in the face and size of `token`.
pub fn layout(painter: &Painter, text: &str, token: TextToken, color: Color32) -> Arc<Galley> {
    painter.layout_no_wrap(text.to_owned(), token.font_id(), color)
}

/// Shows `reason` over a disabled widget, and over nothing else.
pub fn with_disabled_reason(response: Response, reason: Option<&str>) -> Response {
    match reason {
        Some(reason) => response.on_disabled_hover_text(reason),
        None => response,
    }
}

/// The button of a group that is its one Tab stop: the one that has the
/// focus, or else the chosen one, or else the first (the WAI-ARIA radio
/// group, "roving focus"). Tab enters the group there and the next Tab leaves
/// it; the arrow keys move the focus inside. `group` is the id the buttons'
/// ids come from, `chosen` the index of the chosen button.
pub fn tab_stop(ui: &Ui, group: Id, buttons: usize, chosen: Option<usize>) -> usize {
    (0..buttons)
        .find(|index| ui.memory(|memory| memory.has_focus(group.with(index))))
        .or(chosen)
        .unwrap_or(0)
}

/// The button at `index` of a group whose Tab stop is `stop`. egui gives the
/// focus to a widget only when it is focusable, so the others sense clicks
/// and nothing else: the pointer, and the keyboard once the focus is on
/// them, still work. Their id is `group.with(index)`.
pub fn group_button(ui: &Ui, rect: Rect, group: Id, index: usize, stop: usize) -> Response {
    let id = group.with(index);
    if index == stop {
        return ui.interact(rect, id, Sense::click());
    }
    let response = ui.interact(rect, id, Sense::CLICK);
    // A screen reader still moves its focus to any button of the group,
    // as a browser lets it focus a radio that is not the Tab stop. The
    // node is made here, so the buttons keep their order in the tree.
    let focus = accesskit::Action::Focus;
    ui.ctx()
        .accesskit_node_builder(id, |node| node.add_action(focus));
    if ui.input(|input| input.has_accesskit_action_request(id, focus)) {
        response.request_focus();
    }
    response
}

/// A frame for the tests of the widgets: egui runs a frame with no window,
/// so sizes, states, and what a screen reader hears can be read directly.
#[cfg(test)]
pub(crate) mod harness {
    use eframe::egui::{
        self, Color32, Context, Event, Modifiers, PointerButton, Pos2, RawInput, Ui, accesskit,
    };

    use crate::theme;

    /// One egui context with the window's fonts and styles.
    pub struct Harness {
        pub ctx: Context,
    }

    impl Harness {
        pub fn new(theme: egui::Theme) -> Self {
            let ctx = Context::default();
            theme::style::apply(&ctx);
            ctx.set_theme(theme);
            ctx.enable_accesskit();
            Self { ctx }
        }

        /// One frame with this input; the output's accessibility tree.
        pub fn frame(&self, input: RawInput, add: impl FnMut(&mut Ui)) -> Tree {
            let mut output = self.ctx.run_ui(input, add);
            let nodes = output
                .platform_output
                .accesskit_update
                .take()
                .map(|update| update.nodes.into_iter().map(|(_, node)| node).collect())
                .unwrap_or_default();
            output.drop_without_applying_deltas();
            Tree { nodes }
        }

        /// The shapes one frame paints, in order.
        pub fn shapes(&self, add: impl FnMut(&mut Ui)) -> Vec<egui::Shape> {
            let output = self.ctx.run_ui(RawInput::default(), add);
            let shapes = output
                .shapes
                .iter()
                .map(|clipped| clipped.shape.clone())
                .collect();
            output.drop_without_applying_deltas();
            shapes
        }

        /// One frame with no input.
        pub fn run(&self, add: impl FnMut(&mut Ui)) {
            self.frame(RawInput::default(), add);
        }

        /// One frame, then the tree it described.
        pub fn accessibility(&self, add: impl FnMut(&mut Ui)) -> Tree {
            self.frame(RawInput::default(), add)
        }

        /// A frame where the pointer moves to `pos`.
        pub fn hover(&self, pos: Pos2, add: impl FnMut(&mut Ui)) {
            let input = RawInput {
                events: vec![Event::PointerMoved(pos)],
                ..RawInput::default()
            };
            self.frame(input, add);
        }

        /// A frame where the primary button goes down at `pos`, and one
        /// where it goes up: the click.
        pub fn click(&self, pos: Pos2, mut add: impl FnMut(&mut Ui)) {
            let button = |pressed| Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            for events in [
                vec![Event::PointerMoved(pos), button(true)],
                vec![button(false)],
            ] {
                let input = RawInput {
                    events,
                    ..RawInput::default()
                };
                self.frame(input, &mut add);
            }
        }

        /// A frame with the key pressed once.
        pub fn key(&self, key: egui::Key, add: impl FnMut(&mut Ui)) {
            self.key_with(key, Modifiers::NONE, add);
        }

        /// A frame with the key pressed once while `modifiers` are held.
        pub fn key_with(&self, key: egui::Key, modifiers: Modifiers, add: impl FnMut(&mut Ui)) {
            let input = RawInput {
                events: vec![Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
                ..RawInput::default()
            };
            self.frame(input, add);
        }
    }

    /// The nodes of one frame's accessibility tree.
    pub struct Tree {
        nodes: Vec<accesskit::Node>,
    }

    impl Tree {
        pub fn node_with_label(&self, label: &str) -> Option<&accesskit::Node> {
            self.nodes.iter().find(|node| node.label() == Some(label))
        }

        /// A node named `text`, or showing it as its value: egui gives a
        /// text label its words as a value, not a name.
        pub fn node_with_text(&self, text: &str) -> Option<&accesskit::Node> {
            self.nodes
                .iter()
                .find(|node| node.label() == Some(text) || node.value() == Some(text))
        }
    }

    /// `fill` painted over an opaque `page`: what the eye sees under a
    /// fill that is partly see-through.
    pub fn over(fill: Color32, page: Color32) -> Color32 {
        let behind = |page: u8| {
            let kept = u16::from(255 - fill.a());
            (u16::from(page) * kept + 127) / 255
        };
        Color32::from_rgb(
            fill.r() + behind(page.r()) as u8,
            fill.g() + behind(page.g()) as u8,
            fill.b() + behind(page.b()) as u8,
        )
    }
}
