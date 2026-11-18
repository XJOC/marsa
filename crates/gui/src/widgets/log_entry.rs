//! A line of a log (components.md, Log line): 26 high, the level as a word in
//! the mono face, then the message cut short at the end of the row. An error
//! line has a tinted row (`error.bg`), not only a red word; a warning is
//! amber and information is `mute`, and the word says which, so the color is
//! never the only clue (R8). The tooltip has the whole text of a line that is
//! cut short or shown in other words than the program wrote.
//!
//! The Logs screen adds only the rows in view, so a log of thousands of lines
//! costs what a screenful does (R16).

use eframe::egui::text::{LayoutJob, TextWrapping};
use eframe::egui::{
    Align2, Color32, CornerRadius, Rect, Response, Role, Sense, Ui, Widget, WidgetInfo, pos2, vec2,
};

use super::slot;
use crate::logline::LogLevel;
use crate::strings;
use crate::theme::{Direction, Theme};

/// The color of the word of a level, and of the square of its filter: `mute`,
/// amber (the warning color of the design), and `error`.
pub const fn level_color(theme: &Theme, level: LogLevel) -> Color32 {
    match level {
        LogLevel::Info => theme.color.mute,
        LogLevel::Warning => theme.sections.dumps.fg,
        LogLevel::Error => theme.color.error,
    }
}

/// The fill of the row of a line, if it has one: only an error does.
pub const fn row_fill(theme: &Theme, level: LogLevel) -> Option<Color32> {
    match level {
        LogLevel::Error => Some(theme.color.error_bg),
        LogLevel::Info | LogLevel::Warning => None,
    }
}

/// The color of a message: `ink2`, and `mute` for a line the program wrote
/// to its standard output instead of its error output.
pub const fn message_color(theme: &Theme, quiet: bool) -> Color32 {
    if quiet {
        theme.color.mute
    } else {
        theme.color.ink2
    }
}

/// One line of a log.
#[derive(Clone, Copy, Debug)]
#[must_use = "a log line is drawn only when it is added to a Ui"]
pub struct LogEntry<'a> {
    level: LogLevel,
    /// What the row shows.
    message: &'a str,
    /// The line as the program wrote it, for the tooltip.
    raw: &'a str,
    quiet: bool,
}

impl<'a> LogEntry<'a> {
    pub const fn new(level: LogLevel, message: &'a str, raw: &'a str) -> Self {
        Self {
            level,
            message,
            raw,
            quiet: false,
        }
    }

    /// The message in `mute`.
    pub const fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }
}

impl Widget for LogEntry<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let size = vec2(ui.available_width(), theme.size.log_row);
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
        let word = strings::log_level_word(self.level);
        let mut elided = false;
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            if let Some(fill) = row_fill(theme, self.level) {
                painter.rect_filled(rect, CornerRadius::ZERO, fill);
            }
            let pad = theme.space.s4;
            // The widest word sets the column, so the messages line up.
            let widest = painter.layout_no_wrap(
                strings::log_level_word(LogLevel::Error).to_owned(),
                theme.text.mono_sm.font_id(),
                Color32::PLACEHOLDER,
            );
            let column = widest.size().x;
            let level = painter.layout_no_wrap(
                word.to_owned(),
                theme.text.mono_sm.font_id(),
                Color32::PLACEHOLDER,
            );
            let inside = rect.width() - pad - pad;
            let spot = |offset: f32, width: f32| {
                Rect::from_min_size(
                    pos2(slot(direction, rect, pad + offset, width), rect.top()),
                    vec2(width, rect.height()),
                )
            };
            let at = Align2::LEFT_CENTER.align_size_within_rect(level.size(), spot(0.0, column));
            painter.galley(at.min, level, level_color(theme, self.level));
            let start = column + theme.space.s3;
            let room = (inside - start).max(0.0);
            let mut job = LayoutJob::simple_singleline(
                self.message.to_owned(),
                theme.text.mono_sm.font_id(),
                Color32::PLACEHOLDER,
            );
            job.wrap = TextWrapping::truncate_at_width(room);
            let message = painter.layout_job(job);
            elided = message.elided;
            let at = Align2::LEFT_CENTER.align_size_within_rect(message.size(), spot(start, room));
            painter.galley(at.min, message, message_color(theme, self.quiet));
        }
        let label = strings::log_entry_label(self.level, self.message);
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &label));
        // The whole line, when the row shows less than the program wrote.
        if elided || self.message != self.raw {
            response.on_hover_text(self.raw)
        } else {
            response
        }
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Shape};

    use super::*;
    use crate::theme::tests::{TEXT, UI, contrast};
    use crate::theme::{DARK, LIGHT};
    use crate::widgets::harness::Harness;

    #[test]
    fn only_an_error_has_a_tinted_row_and_each_level_has_its_own_color() {
        for theme in [&LIGHT, &DARK] {
            assert_eq!(row_fill(theme, LogLevel::Error), Some(theme.color.error_bg));
            assert_eq!(row_fill(theme, LogLevel::Warning), None);
            assert_eq!(row_fill(theme, LogLevel::Info), None);
            assert_eq!(level_color(theme, LogLevel::Info), theme.color.mute);
            assert_eq!(
                level_color(theme, LogLevel::Warning),
                theme.sections.dumps.fg
            );
            assert_eq!(level_color(theme, LogLevel::Error), theme.color.error);
            assert_eq!(message_color(theme, false), theme.color.ink2);
            assert_eq!(message_color(theme, true), theme.color.mute);
        }
    }

    #[test]
    fn every_word_and_message_reads_on_its_row_in_both_themes() {
        for (name, theme) in [("light", &LIGHT), ("dark", &DARK)] {
            for level in LogLevel::ALL {
                let fill = row_fill(theme, level).unwrap_or(theme.color.surface);
                let word = contrast(level_color(theme, level), fill);
                assert!(word >= TEXT, "{name} {level:?} word {word:.2}");
                for quiet in [false, true] {
                    let message = contrast(message_color(theme, quiet), fill);
                    assert!(
                        message >= TEXT,
                        "{name} {level:?} quiet={quiet} {message:.2}"
                    );
                }
            }
            // The tint of an error row stands out from the card.
            assert_ne!(theme.color.error_bg, theme.color.surface);
            // The square of a filter reads as a control part on the page.
            for level in LogLevel::ALL {
                assert!(
                    contrast(level_color(theme, level), theme.color.bg) >= UI,
                    "{name} {level:?}"
                );
            }
        }
    }

    #[test]
    fn a_row_is_a_log_row_high_and_as_wide_as_the_space_it_is_given() {
        let harness = Harness::new(egui::Theme::Light);
        let mut rect = egui::Rect::NOTHING;
        harness.run(|ui| {
            ui.set_width(400.0);
            rect = ui.add(LogEntry::new(LogLevel::Info, "ready", "ready")).rect;
        });
        assert_eq!((rect.width(), rect.height()), (400.0, LIGHT.size.log_row));
        assert_eq!(LIGHT.size.log_row, 26.0);
    }

    #[test]
    fn an_error_row_paints_its_tint_and_the_others_paint_none() {
        for (theme, egui_theme) in [(&LIGHT, egui::Theme::Light), (&DARK, egui::Theme::Dark)] {
            let harness = Harness::new(egui_theme);
            let tinted = |level| {
                harness
                    .shapes(|ui| {
                        ui.add(LogEntry::new(level, "message", "message"));
                    })
                    .into_iter()
                    .any(|shape| matches!(shape, Shape::Rect(rect) if rect.fill == theme.color.error_bg))
            };
            assert!(tinted(LogLevel::Error));
            assert!(!tinted(LogLevel::Warning));
            assert!(!tinted(LogLevel::Info));
        }
    }

    #[test]
    fn the_level_is_a_word_before_the_message_and_a_screen_reader_hears_both() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            ui.add(LogEntry::new(LogLevel::Error, "dial tcp: refused", "{}"));
        });
        let label = strings::log_entry_label(LogLevel::Error, "dial tcp: refused");
        assert!(tree.node_with_text(&label).is_some(), "{label}");
        let words = harness.shapes(|ui| {
            ui.add(LogEntry::new(LogLevel::Warning, "slow", "slow"));
        });
        let texts: Vec<String> = words
            .iter()
            .filter_map(|shape| match shape {
                Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["WARN", "slow"]);
    }

    #[test]
    fn a_message_too_long_for_the_row_ends_in_an_ellipsis_and_a_short_one_is_whole() {
        let harness = Harness::new(egui::Theme::Light);
        let long = "a very long message that the row cannot hold in three hundred points of room";
        let shown = |width: f32| {
            harness
                .shapes(|ui| {
                    ui.set_width(width);
                    ui.add(LogEntry::new(LogLevel::Info, long, long));
                })
                .into_iter()
                .filter_map(|shape| match shape {
                    Shape::Text(text) if text.galley.text() != "INFO" => Some((
                        text.galley.elided,
                        text.galley.rows.last().map_or(0.0, |r| r.size.x),
                    )),
                    _ => None,
                })
                .next()
                .expect("the message")
        };
        let (cut, width) = shown(300.0);
        assert!(cut && width <= 300.0, "{cut} {width}");
        let (cut, _) = shown(2000.0);
        assert!(!cut);
    }
}
