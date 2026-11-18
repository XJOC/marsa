//! The confirm dialog (components.md, Dialogs): 440 wide, `radius.xl`,
//! `elevation.2`, over a `scrim`. The title is the exact question ("Unlink
//! shop?"), the body says what happens and what stays, and the buttons at the
//! end are Cancel and the action named by its verb, in the danger style when
//! it destroys something. Cancel takes the focus first, so Enter alone never
//! destroys anything, and Esc cancels. Only for destructive or blocking
//! choices; everything else is inline.
//!
//! The dialog is stateless: a screen shows it every frame while it holds a
//! pending question, and drops the question when [`ConfirmDialog::show`]
//! returns an answer. Clicking the scrim answers nothing: a click beside a
//! dialog is not a choice.

use eframe::egui::{
    Context, CornerRadius, Frame, Id, Key, Label, Margin, Modal, Modifiers, RichText, Role, Stroke,
    Ui, WidgetInfo, vec2,
};

use super::{Button, Look, whole};
use crate::strings;
use crate::theme::{Direction, Theme};

/// What the user answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Cancel,
    Confirm,
}

/// The order the buttons take the keyboard focus: Cancel is focused when the
/// dialog opens, and Tab moves on to the action.
pub const FOCUS_ORDER: [Answer; 2] = [Answer::Cancel, Answer::Confirm];

/// A question with two answers.
#[derive(Clone, Copy, Debug)]
#[must_use = "a dialog is drawn only when it is shown"]
pub struct ConfirmDialog<'a> {
    /// Tells this dialog from another one that is open in the window.
    id: &'a str,
    /// The exact question: "Unlink shop?".
    title: &'a str,
    /// What happens, and what stays.
    body: &'a str,
    /// The action, named by its verb: "Unlink".
    confirm: &'a str,
    cancel: &'a str,
    destructive: bool,
}

impl<'a> ConfirmDialog<'a> {
    pub const fn new(id: &'a str, title: &'a str, body: &'a str, confirm: &'a str) -> Self {
        Self {
            id,
            title,
            body,
            confirm,
            cancel: strings::CANCEL,
            destructive: false,
        }
    }

    /// The action destroys something: its button is the danger style, and a
    /// screen reader hears an alert dialog.
    pub const fn danger(mut self) -> Self {
        self.destructive = true;
        self
    }

    /// The role a screen reader hears.
    pub const fn role(&self) -> Role {
        if self.destructive {
            Role::AlertDialog
        } else {
            Role::Dialog
        }
    }

    /// The card of the dialog: its fill, border, corner, shadow, and margin.
    fn frame(theme: &Theme) -> Frame {
        Frame::new()
            .fill(theme.color.surface)
            .stroke(Stroke::new(theme.size.border, theme.color.line))
            .corner_radius(CornerRadius::same(theme.radius.xl))
            .shadow(theme.elevation.dialog)
            .inner_margin(Margin::same(whole(theme.space.s6)))
    }

    /// The inside of the card. `new`: the dialog was not there last frame,
    /// so Cancel takes the focus. `preview`: draw Cancel as it looks when it
    /// has the focus, though it has none (the gallery).
    fn contents(&self, ui: &mut Ui, new: bool, preview: bool) -> Option<Answer> {
        let theme = Theme::of(ui.ctx());
        // 440 in all: the margins and the border are inside it.
        let chrome = theme.space.s6 + theme.space.s6 + theme.size.border + theme.size.border;
        ui.set_width(theme.size.dialog_width - chrome);
        ui.spacing_mut().item_spacing = vec2(theme.space.s2, theme.space.s3);
        let title = RichText::new(self.title)
            .font(theme.text.dialog_title.font_id())
            .color(theme.color.ink);
        ui.add(Label::new(title));
        let body = RichText::new(self.body)
            .font(theme.text.body.font_id())
            .color(theme.color.ink2)
            .line_height(Some(theme.text.body.size * theme.text.line_height_body));
        ui.add(Label::new(body));
        ui.add_space(theme.space.s1);
        let mut answer = None;
        let size = vec2(ui.available_width(), theme.size.control);
        // The row runs from the end, so the action is the last on the line
        // and is added first.
        ui.allocate_ui_with_layout(size, Direction::CURRENT.row_from_end(), |ui| {
            let action = if self.destructive {
                Button::danger(self.confirm)
            } else {
                Button::primary(self.confirm)
            };
            if ui.add(action).clicked() {
                answer = Some(Answer::Confirm);
            }
            let cancel = Button::secondary(self.cancel);
            let cancel = ui.add(if preview {
                cancel.looks(Look::focused())
            } else {
                cancel
            });
            if new {
                cancel.request_focus();
            }
            if cancel.clicked() {
                answer = Some(Answer::Cancel);
            }
        });
        let role = self.role();
        let title = self.title;
        ui.response()
            .widget_info(|| WidgetInfo::labeled(role, true, title));
        answer
    }

    /// The card alone, in a screen: the gallery and the tests. No scrim, no
    /// focus change, and Esc is not read. Cancel is drawn with the focus
    /// ring it has when the dialog opens.
    pub fn show_card(self, ui: &mut Ui) -> Option<Answer> {
        let theme = Theme::of(ui.ctx());
        Self::frame(theme)
            .show(ui, |ui| self.contents(ui, false, true))
            .inner
    }

    /// Shows the dialog over the window and a `scrim`. `Some` when the user
    /// answered: a click on a button, Enter or Space on the focused one, or
    /// Esc, which cancels.
    pub fn show(self, ctx: &Context) -> Option<Answer> {
        let theme = Theme::of(ctx);
        let id = Id::unique(("confirm_dialog", self.id));
        let new = is_new(ctx, id);
        let shown = Modal::new(id)
            .backdrop_color(theme.color.scrim)
            .frame(Self::frame(theme))
            .show(ctx, |ui| self.contents(ui, new, false));
        // Esc reads only for the top dialog, and not while a menu in it is
        // open (that Esc closes the menu).
        let escape = shown.is_top_modal
            && !shown.any_popup_open
            && ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape));
        shown.inner.or(escape.then_some(Answer::Cancel))
    }
}

/// Whether the dialog `id` was absent last frame: the first time it shows,
/// and again after a frame without it.
fn is_new(ctx: &Context, id: Id) -> bool {
    let now = ctx.cumulative_frame_nr();
    let key = id.with("last_frame");
    let last = ctx.data_mut(|data| {
        let last = data.get_temp::<u64>(key);
        data.insert_temp(key, now);
        last
    });
    last.is_none_or(|last| now > last.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Color32, Shape, pos2};

    use super::*;
    use crate::theme::tests::{TEXT, UI, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::Harness;

    const THEMES: [(&str, &Theme, egui::Theme); 2] = [
        ("light", &LIGHT, egui::Theme::Light),
        ("dark", &DARK, egui::Theme::Dark),
    ];

    fn unlink() -> ConfirmDialog<'static> {
        ConfirmDialog::new(
            "unlink",
            "Unlink shop?",
            "Marsa stops serving https://shop.test. The folder ~/Code/shop and its files stay where they are.",
            "Unlink",
        )
        .danger()
    }

    /// One frame with the dialog shown, and the key pressed if there is
    /// one. What the dialog answered.
    fn frame(harness: &Harness, key: Option<Key>) -> Option<Answer> {
        let mut answer = None;
        let draw = |ui: &mut Ui| answer = unlink().show(ui.ctx());
        match key {
            Some(key) => harness.key(key, draw),
            None => harness.run(draw),
        }
        answer
    }

    /// Frames enough for a window to size itself and fade in (0.2 s at 60
    /// frames a second), so the next frame paints it as it is.
    fn settle(harness: &Harness) {
        for _ in 0..20 {
            frame(harness, None);
        }
    }

    /// `shapes` with the groups opened: a frame with a shadow is one group.
    fn flatten(shapes: Vec<Shape>, into: &mut Vec<Shape>) {
        for shape in shapes {
            match shape {
                Shape::Vec(inner) => flatten(inner, into),
                shape => into.push(shape),
            }
        }
    }

    /// The middle of a node's bounds, for a click.
    fn middle(harness: &Harness, label: &str) -> egui::Pos2 {
        let tree = harness.accessibility(|ui| {
            unlink().show(ui.ctx());
        });
        let bounds = tree
            .node_with_label(label)
            .and_then(|node| node.bounds())
            .expect(label);
        pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        )
    }

    #[test]
    fn cancel_takes_the_focus_first_so_enter_cancels() {
        assert_eq!(FOCUS_ORDER, [Answer::Cancel, Answer::Confirm]);
        let harness = Harness::new(egui::Theme::Light);
        assert_eq!(frame(&harness, None), None, "opening answers nothing");
        assert_eq!(frame(&harness, Some(Key::Enter)), Some(Answer::Cancel));
    }

    #[test]
    fn tab_moves_to_the_action_and_enter_or_space_confirms() {
        for key in [Key::Enter, Key::Space] {
            let harness = Harness::new(egui::Theme::Light);
            frame(&harness, None);
            assert_eq!(frame(&harness, Some(Key::Tab)), None);
            frame(&harness, None);
            assert_eq!(frame(&harness, Some(key)), Some(Answer::Confirm), "{key:?}");
        }
    }

    #[test]
    fn esc_cancels_whatever_has_the_focus() {
        let harness = Harness::new(egui::Theme::Light);
        frame(&harness, None);
        assert_eq!(frame(&harness, Some(Key::Escape)), Some(Answer::Cancel));
        // On the action too.
        frame(&harness, Some(Key::Tab));
        frame(&harness, None);
        assert_eq!(frame(&harness, Some(Key::Escape)), Some(Answer::Cancel));
    }

    #[test]
    fn the_focus_goes_back_to_cancel_each_time_the_dialog_opens() {
        let harness = Harness::new(egui::Theme::Light);
        frame(&harness, None);
        frame(&harness, Some(Key::Tab));
        frame(&harness, None);
        // The dialog closes for a few frames, then opens again.
        for _ in 0..3 {
            harness.run(|_| {});
        }
        frame(&harness, None);
        assert_eq!(frame(&harness, Some(Key::Enter)), Some(Answer::Cancel));
    }

    #[test]
    fn a_click_on_a_button_answers_and_a_click_on_the_scrim_does_not() {
        let harness = Harness::new(egui::Theme::Light);
        frame(&harness, None);
        for (label, expected) in [
            ("Unlink", Some(Answer::Confirm)),
            ("Cancel", Some(Answer::Cancel)),
        ] {
            let at = middle(&harness, label);
            let mut answer = None;
            harness.click(at, |ui| answer = unlink().show(ui.ctx()));
            assert_eq!(answer, expected, "{label}");
        }
        // Far from the card: the scrim.
        let mut answer = None;
        harness.click(pos2(2.0, 2.0), |ui| answer = unlink().show(ui.ctx()));
        assert_eq!(answer, None);
    }

    #[test]
    fn cancel_is_before_the_action_and_the_action_is_at_the_end() {
        let harness = Harness::new(egui::Theme::Light);
        settle(&harness);
        let tree = harness.accessibility(|ui| {
            unlink().show(ui.ctx());
        });
        let x = |label: &str| {
            tree.node_with_label(label)
                .and_then(|n| n.bounds())
                .expect(label)
        };
        let (cancel, action, card) = (x("Cancel"), x("Unlink"), x("Unlink shop?"));
        assert!(cancel.x1 < action.x0, "Cancel, then the action");
        // The action is the last thing on the line: it ends where the
        // card's inside ends.
        assert!((card.x1 - action.x1).abs() < 1.5, "{card:?} {action:?}");
    }

    #[test]
    fn the_card_is_440_wide_with_a_radius_xl_corner_the_dialog_shadow_and_a_scrim_behind() {
        for (name, theme, active) in THEMES {
            let harness = Harness::new(active);
            settle(&harness);
            let shapes = harness.shapes(|ui| {
                unlink().show(ui.ctx());
            });
            let mut flat = Vec::new();
            flatten(shapes, &mut flat);
            let rects: Vec<_> = flat
                .iter()
                .filter_map(|shape| match shape {
                    Shape::Rect(rect) => Some(rect.clone()),
                    _ => None,
                })
                .collect();
            let card = rects
                .iter()
                .find(|r| {
                    r.fill == theme.color.surface && r.corner_radius == egui::CornerRadius::same(14)
                })
                .unwrap_or_else(|| panic!("{name}: the card"));
            assert_eq!(card.rect.width(), theme.size.dialog_width, "{name}");
            assert_eq!(theme.size.dialog_width, 440.0);
            assert_eq!(theme.radius.xl, 14);
            let shadow = rects
                .iter()
                .find(|r| r.fill == theme.elevation.dialog.color);
            assert!(
                shadow.is_some_and(|s| s.blur_width > 0.0),
                "{name}: the shadow"
            );
            let scrim = rects
                .iter()
                .find(|r| r.fill == theme.color.scrim)
                .unwrap_or_else(|| panic!("{name}: the scrim"));
            assert!(
                scrim.rect.contains_rect(card.rect),
                "{name}: over the whole window"
            );
        }
    }

    #[test]
    fn a_destructive_dialog_has_a_danger_button_and_is_an_alert_dialog() {
        let harness = Harness::new(egui::Theme::Light);
        let fills = |dialog: ConfirmDialog<'static>| -> Vec<Color32> {
            for _ in 0..20 {
                harness.run(|ui| {
                    dialog.show(ui.ctx());
                });
            }
            harness
                .shapes(|ui| {
                    dialog.show(ui.ctx());
                })
                .into_iter()
                .filter_map(|shape| match shape {
                    Shape::Rect(rect) => Some(rect.fill),
                    _ => None,
                })
                .collect()
        };
        let plain = ConfirmDialog::new("plain", "Install PHP 8.4?", "It takes 31 MB.", "Install");
        let danger = fills(unlink());
        let calm = fills(plain);
        assert!(danger.contains(&LIGHT.color.error));
        assert!(!calm.contains(&LIGHT.color.error));
        assert!(calm.contains(&LIGHT.color.accent));
        assert_eq!(unlink().role(), Role::AlertDialog);
        assert_eq!(plain.role(), Role::Dialog);
        frame(&harness, None);
        let tree = harness.accessibility(|ui| {
            unlink().show(ui.ctx());
        });
        let card = tree.node_with_label("Unlink shop?").expect("the question");
        assert_eq!(card.role(), Role::AlertDialog);
        for label in ["Cancel", "Unlink"] {
            assert_eq!(
                tree.node_with_label(label).expect(label).role(),
                Role::Button
            );
        }
    }

    #[test]
    fn the_card_alone_has_no_scrim_and_leaves_the_focus_alone() {
        let harness = Harness::new(egui::Theme::Light);
        let mut answer = Some(Answer::Confirm);
        let shapes = harness.shapes(|ui| answer = unlink().show_card(ui));
        assert_eq!(answer, None);
        let scrim = shapes
            .iter()
            .any(|shape| matches!(shape, Shape::Rect(r) if r.fill == LIGHT.color.scrim));
        assert!(!scrim);
        let mut focused = Some(egui::Id::NULL);
        harness.run(|ui| {
            unlink().show_card(ui);
            focused = ui.memory(|m| m.focused());
        });
        assert_eq!(focused, None, "show_card does not take the focus");
    }

    #[test]
    fn the_words_read_on_the_card_and_the_danger_button_stands_out_from_it() {
        for (name, theme, _) in THEMES {
            let c = &theme.color;
            assert!(contrast(c.ink, c.surface) >= TEXT, "{name} title");
            assert!(contrast(c.ink2, c.surface) >= TEXT, "{name} body");
            assert!(
                contrast(c.on_error, c.error) >= TEXT,
                "{name} danger button"
            );
            assert!(
                contrast(c.error, c.surface) >= UI,
                "{name} danger button edge"
            );
        }
    }
}
