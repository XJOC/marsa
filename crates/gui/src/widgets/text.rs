//! A line of words in one token and one color: the label every screen draws
//! many times. It is never selectable (the window is not a document), so a
//! drag over it moves nothing and the text never joins a tab stop.

use eframe::egui::{Color32, Label, Response, RichText, Sense, Ui, Widget};

use crate::theme::{TextToken, Theme};

/// Words in a text token and a color from the theme. [Laravel: a Blade
/// `<x-text>` component with a few presets.]
pub struct Text<'a> {
    text: &'a str,
    token: TextToken,
    color: Color32,
    truncate: bool,
    sense: Option<Sense>,
}

impl<'a> Text<'a> {
    pub fn new(text: &'a str, token: TextToken, color: Color32) -> Self {
        Self {
            text,
            token,
            color,
            truncate: false,
            sense: None,
        }
    }

    /// `caption` in `mute`: a note under or beside something.
    pub fn caption(theme: &Theme, text: &'a str) -> Self {
        Self::new(text, theme.text.caption, theme.color.mute)
    }

    /// `body` in `ink`: the main words of a screen.
    pub fn body(theme: &Theme, text: &'a str) -> Self {
        Self::new(text, theme.text.body, theme.color.ink)
    }

    /// `body_sm` in `ink2`: a value, or a second line.
    pub fn small(theme: &Theme, text: &'a str) -> Self {
        Self::new(text, theme.text.body_sm, theme.color.ink2)
    }

    pub fn color(mut self, color: Color32) -> Self {
        self.color = color;
        self
    }

    pub fn token(mut self, token: TextToken) -> Self {
        self.token = token;
        self
    }

    /// Cut at the end of the column instead of wrapping.
    pub fn truncate(mut self) -> Self {
        self.truncate = true;
        self
    }

    /// A pointer target (a link). It takes no Tab stop.
    pub fn sense(mut self, sense: Sense) -> Self {
        self.sense = Some(sense);
        self
    }
}

impl Widget for Text<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let rich = RichText::new(self.text)
            .font(self.token.font_id())
            .color(self.color);
        let mut label = Label::new(rich).selectable(false);
        if self.truncate {
            label = label.truncate();
        }
        if let Some(sense) = self.sense {
            label = label.sense(sense);
        }
        ui.add(label)
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui;

    use super::*;
    use crate::theme::LIGHT;
    use crate::widgets::harness::Harness;

    #[test]
    fn a_text_is_one_label_in_its_token_and_color() {
        let harness = Harness::new(egui::Theme::Light);
        let shapes = harness.shapes(|ui| {
            ui.add(Text::caption(&LIGHT, "note"));
        });
        let colors: Vec<Color32> = shapes
            .iter()
            .filter_map(|shape| match shape {
                egui::Shape::Text(text) => Some(
                    text.override_text_color
                        .unwrap_or(text.galley.job.sections[0].format.color),
                ),
                _ => None,
            })
            .collect();
        assert_eq!(colors, [LIGHT.color.mute]);
    }

    #[test]
    fn the_presets_use_the_tokens_of_the_theme() {
        let words = Text::body(&LIGHT, "x");
        assert_eq!(words.color, LIGHT.color.ink);
        assert_eq!(Text::small(&LIGHT, "x").color, LIGHT.color.ink2);
    }
}
