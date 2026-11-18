//! The step card (task M5-D5, the first-run panel): a numbered circle, a
//! title, a line of explanation, and whatever the screen adds under them,
//! such as a progress bar. Three looks tell where the user is: the step
//! to do now has the `surface` fill and an `accent` border, a step done has
//! a check, and a step to come is quiet. The number or the check and the
//! word for a screen reader say it too, so color is never alone (R8).

use eframe::egui::{
    Align2, Color32, CornerRadius, Frame, InnerResponse, Label, Margin, Rect, RichText, Role,
    Sense, Stroke, Ui, Vec2, WidgetInfo, pos2, vec2,
};

use super::icons::Icon;
use super::{layout, whole};
use crate::strings;
use crate::theme::{Face, TextToken, Theme};

/// Where a step is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Finished: a check.
    Done,
    /// The one to do now.
    Current,
    /// Comes after the current one.
    Next,
}

/// What a card is made of at one stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    /// `None`: no fill, the page shows through.
    pub fill: Option<Color32>,
    pub border: Color32,
    pub circle: Color32,
    /// The number, or the check.
    pub mark: Color32,
    pub title: Color32,
}

pub fn look(theme: &Theme, stage: Stage) -> Look {
    let c = &theme.color;
    match stage {
        Stage::Current => Look {
            fill: Some(c.surface),
            border: c.accent,
            circle: c.accent,
            mark: c.on_accent,
            title: c.ink,
        },
        Stage::Done => Look {
            fill: None,
            border: c.line,
            circle: c.ok_tint,
            mark: c.ok,
            title: c.ink2,
        },
        Stage::Next => Look {
            fill: None,
            border: c.line,
            circle: c.selected,
            mark: c.ink2,
            title: c.ink2,
        },
    }
}

/// A step of a list of steps. [Laravel: a Blade component with a slot,
/// [`StepCard::show_with`].]
#[derive(Clone, Copy, Debug)]
#[must_use = "a step card is drawn only when it is shown"]
pub struct StepCard<'a> {
    number: u8,
    title: &'a str,
    body: &'a str,
    stage: Stage,
}

impl<'a> StepCard<'a> {
    pub const fn new(number: u8, title: &'a str, body: &'a str, stage: Stage) -> Self {
        Self {
            number,
            title,
            body,
            stage,
        }
    }

    /// Draws the card across the width it is given, with `below` added under
    /// the explanation.
    pub fn show_with<R>(self, ui: &mut Ui, below: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
        let theme = Theme::of(ui.ctx());
        let look = look(theme, self.stage);
        let mut frame = Frame::new()
            .stroke(Stroke::new(theme.size.border, look.border))
            .corner_radius(CornerRadius::same(theme.radius.lg))
            .inner_margin(Margin::same(whole(theme.space.s4)));
        if let Some(fill) = look.fill {
            frame = frame.fill(fill);
        }
        let shown = frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(theme.space.s3, theme.space.s1);
            ui.horizontal_top(|ui| {
                let side = Vec2::splat(theme.size.step_circle);
                let (circle, _) = ui.allocate_exact_size(side, Sense::hover());
                self.paint_mark(ui, theme, look, circle);
                ui.vertical(|ui| {
                    let title = RichText::new(self.title)
                        .font(theme.text.body_strong.font_id())
                        .color(look.title);
                    ui.add(Label::new(title).selectable(false));
                    let body = RichText::new(self.body)
                        .font(theme.text.body_sm.font_id())
                        .color(theme.color.mute);
                    ui.add(Label::new(body).selectable(false));
                    below(ui)
                })
                .inner
            })
            .inner
        });
        let name = strings::step_label(self.number, self.title, self.stage);
        shown
            .response
            .widget_info(|| WidgetInfo::labeled(Role::ListItem, true, &name));
        shown
    }

    /// The number in its circle, or the check once the step is done.
    fn paint_mark(&self, ui: &Ui, theme: &Theme, look: Look, circle: Rect) {
        let painter = ui.painter();
        painter.rect_filled(circle, CornerRadius::same(theme.radius.full), look.circle);
        match self.stage {
            Stage::Done => {
                let icon = Vec2::splat(theme.size.icon_inline);
                let spot = Rect::from_center_size(circle.center(), icon);
                Icon::Check.paint(painter, theme, spot, look.mark);
            }
            Stage::Current | Stage::Next => {
                let token = TextToken {
                    size: theme.text.body_sm.size,
                    face: Face::SemiBold,
                };
                let galley = layout(painter, &self.number.to_string(), token, look.mark);
                let at = Align2::CENTER_CENTER.align_size_within_rect(galley.size(), circle);
                painter.galley(pos2(at.min.x, at.min.y), galley, look.mark);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui;

    use super::*;
    use crate::theme::tests::{TEXT, UI, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::Harness;

    #[test]
    fn the_step_to_do_now_is_the_one_with_a_fill_and_an_accent_border() {
        for theme in [&LIGHT, &DARK] {
            let now = look(theme, Stage::Current);
            assert_eq!(now.fill, Some(theme.color.surface));
            assert_eq!(now.border, theme.color.accent);
            for stage in [Stage::Done, Stage::Next] {
                let other = look(theme, stage);
                assert_eq!(other.fill, None, "{stage:?}");
                assert_eq!(other.border, theme.color.line, "{stage:?}");
            }
        }
    }

    #[test]
    fn the_number_the_check_and_the_title_read_in_every_stage_and_theme() {
        for theme in [&LIGHT, &DARK] {
            for stage in [Stage::Current, Stage::Done, Stage::Next] {
                let look = look(theme, stage);
                // The check is a drawing (3:1); a number is text (4.5:1).
                let least = if stage == Stage::Done { UI } else { TEXT };
                let circle = contrast(look.mark, look.circle);
                assert!(circle >= least, "{stage:?}: the mark is {circle:.2}:1");
                // The title sits on the card's fill, or on the page.
                let behind = look.fill.unwrap_or(theme.color.bg);
                let title = contrast(look.title, behind);
                assert!(title >= TEXT, "{stage:?}: the title is {title:.2}:1");
            }
        }
    }

    #[test]
    fn the_card_fills_the_width_and_holds_what_is_added_under_it() {
        let harness = Harness::new(egui::Theme::Light);
        let (mut width, mut short, mut tall) = (0.0, Rect::NOTHING, Rect::NOTHING);
        harness.run(|ui| {
            width = ui.available_width();
            short = StepCard::new(1, "Install", "Why.", Stage::Current)
                .show_with(ui, |_| {})
                .response
                .rect;
            tall = StepCard::new(2, "Trust", "Why.", Stage::Next)
                .show_with(ui, |ui| {
                    ui.label("under");
                })
                .response
                .rect;
        });
        assert_eq!(short.width(), width);
        assert_eq!(tall.width(), width);
        assert!(tall.height() > short.height());
        // The circle, and the card's margin around it.
        let least = LIGHT.size.step_circle + 2.0 * LIGHT.space.s4;
        assert!(short.height() >= least, "{}", short.height());
    }

    #[test]
    fn a_screen_reader_hears_the_number_the_title_and_the_stage() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            for (number, stage) in [(1, Stage::Done), (2, Stage::Current), (3, Stage::Next)] {
                StepCard::new(number, "Title", "Body.", stage).show_with(ui, |_| {});
            }
        });
        for label in [
            "Step 1: Title, done",
            "Step 2: Title, to do now",
            "Step 3: Title, comes next",
        ] {
            let node = tree.node_with_label(label).expect(label);
            assert_eq!(node.role(), egui::Role::ListItem, "{label}");
        }
    }
}
