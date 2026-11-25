//! A line of a log (components.md, Log line): 26 high, the level as a word in
//! the mono face, then the message on one line, never wrapped and never cut:
//! the row is as wide as the message needs ([`width_for`]) and the list scrolls
//! sideways. An error line has a tinted row (`error.bg`), not only a red
//! word; a warning is amber and information is `mute`, and the word says
//! which, so the color is never the only clue (R8). The tooltip has the
//! whole text of a line shown in other words than the program wrote.
//!
//! A log whose lines carry their own stamp (a Laravel site's) has a time
//! column before the level, and a line that goes on the entry above it (a
//! stack trace) is drawn under that entry's message, `mute`, with no word
//! and no tint of its own.
//!
//! The Logs screen adds only the rows in view, so a log of thousands of lines
//! costs what a screenful does (R16).

use eframe::egui::{
    Align2, Color32, CornerRadius, Painter, Response, Role, Sense, Ui, Widget, WidgetInfo, vec2,
};

use super::slot;
use super::spot;
use crate::logline::{self, LogLevel, STAMP_COLUMNS};
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

/// How wide the level's word is, whatever the level: the widest word sets
/// the column, so the messages line up.
fn word_column(painter: &Painter, theme: &Theme) -> f32 {
    painter
        .layout_no_wrap(
            strings::log_level_word(LogLevel::Error).to_owned(),
            theme.text.mono_sm.font_id(),
            Color32::PLACEHOLDER,
        )
        .size()
        .x
}

/// How many characters are measured to find the width of a column.
const SAMPLE: usize = 64;

/// How wide one column of the mono face is: every character has the same
/// advance, so a message of `columns` characters is `columns` of these wide.
/// A long run is measured, not one glyph: the advances are kept to the
/// pixel, and a run of them is a little wider than the same count of one.
fn advance(painter: &Painter, theme: &Theme) -> f32 {
    let run = painter.layout_no_wrap(
        "0".repeat(SAMPLE),
        theme.text.mono_sm.font_id(),
        Color32::PLACEHOLDER,
    );
    run.size().x / SAMPLE as f32
}

/// How wide the time column and the gap after it are, in a log that has
/// one.
fn stamp_column(painter: &Painter, theme: &Theme, stamped: bool) -> f32 {
    if stamped {
        STAMP_COLUMNS as f32 * advance(painter, theme) + theme.space.s3
    } else {
        0.0
    }
}

/// How wide a row must be to show a message of `columns` columns whole: the
/// padding, the time column if the log has one (`stamped`), the level's
/// word, the gap, the message, and the padding.
pub fn width_for(ui: &Ui, columns: usize, stamped: bool) -> f32 {
    let theme = Theme::of(ui.ctx());
    let painter = ui.painter();
    theme.space.s4
        + stamp_column(painter, theme, stamped)
        + word_column(painter, theme)
        + theme.space.s3
        + columns as f32 * advance(painter, theme)
        + theme.space.s4
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
    /// How wide the row is: the window, or the message if it is wider.
    width: f32,
    quiet: bool,
    /// The log has a time column: this row's stamp, empty for a row that
    /// has none.
    stamp: Option<&'a str>,
    /// The line goes on the entry above it.
    continuation: bool,
}

impl<'a> LogEntry<'a> {
    /// A row `width` wide. The caller makes it wide enough for its message
    /// ([`width_for`]): the message is drawn whole, never cut.
    pub const fn new(level: LogLevel, message: &'a str, raw: &'a str, width: f32) -> Self {
        Self {
            level,
            message,
            raw,
            width,
            quiet: false,
            stamp: None,
            continuation: false,
        }
    }

    /// The message in `mute`.
    pub const fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }

    /// The log has a time column, and this is the row's stamp, as the line
    /// has it (empty for none). It is cut at [`STAMP_COLUMNS`].
    pub const fn stamp(mut self, stamp: &'a str) -> Self {
        self.stamp = Some(stamp);
        self
    }

    /// The line goes on the entry above it: no word and no tint, and the
    /// text in `mute`, where the messages start.
    pub const fn continuation(mut self, continuation: bool) -> Self {
        self.continuation = continuation;
        self
    }
}

impl Widget for LogEntry<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let theme = Theme::of(ui.ctx());
        let direction = Direction::CURRENT;
        let size = vec2(self.width, theme.size.log_row);
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
        let word = strings::log_level_word(self.level);
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let fill = row_fill(theme, self.level).filter(|_| !self.continuation);
            if let Some(fill) = fill {
                painter.rect_filled(rect, CornerRadius::ZERO, fill);
            }
            let pad = theme.space.s4;
            let spot = |offset: f32, width: f32| {
                spot(
                    slot(direction, rect, pad + offset, width),
                    rect.top(),
                    width,
                    rect.height(),
                )
            };
            let mono = |text: &str| {
                painter.layout_no_wrap(
                    text.to_owned(),
                    theme.text.mono_sm.font_id(),
                    Color32::PLACEHOLDER,
                )
            };
            let stamp = stamp_column(painter, theme, self.stamp.is_some());
            if let Some(text) = self.stamp.filter(|text| !text.is_empty()) {
                let galley = mono(logline::stamp_clipped(text));
                let at = Align2::LEFT_CENTER
                    .align_size_within_rect(galley.size(), spot(0.0, galley.size().x));
                painter.galley(at.min, galley, theme.color.mute);
            }
            let column = word_column(painter, theme);
            if !self.continuation {
                let level = mono(word);
                let at =
                    Align2::LEFT_CENTER.align_size_within_rect(level.size(), spot(stamp, column));
                painter.galley(at.min, level, level_color(theme, self.level));
            }
            let start = stamp + column + theme.space.s3;
            let message = mono(self.message);
            let room = message.size().x;
            let at = Align2::LEFT_CENTER.align_size_within_rect(message.size(), spot(start, room));
            let quiet = self.quiet || self.continuation;
            painter.galley(at.min, message, message_color(theme, quiet));
        }
        // A line that goes on an entry says no level: it has none of its own.
        let label = if self.continuation {
            self.message.to_owned()
        } else {
            strings::log_entry_label(self.level, self.message)
        };
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &label));
        // The whole line, when the row shows other words than the program wrote.
        if self.message != self.raw {
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
    fn a_row_is_a_log_row_high_and_as_wide_as_it_is_given() {
        let harness = Harness::new(egui::Theme::Light);
        let mut rect = egui::Rect::NOTHING;
        harness.run(|ui| {
            rect = ui
                .add(LogEntry::new(LogLevel::Info, "ready", "ready", 400.0))
                .rect;
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
                        ui.add(LogEntry::new(level, "message", "message", 300.0));
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
            ui.add(LogEntry::new(
                LogLevel::Error,
                "dial tcp: refused",
                "{}",
                300.0,
            ));
        });
        let label = strings::log_entry_label(LogLevel::Error, "dial tcp: refused");
        assert!(tree.node_with_text(&label).is_some(), "{label}");
        let words = harness.shapes(|ui| {
            ui.add(LogEntry::new(LogLevel::Warning, "slow", "slow", 300.0));
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
    fn a_message_is_never_cut_and_width_for_is_the_room_it_needs() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let long = ["a very long message that the window cannot hold in three hundred points"; 4]
            .join(" ");
        let columns = long.chars().count();
        let (mut needed, mut one) = (0.0, 0.0);
        let mut drawn = Vec::new();
        harness.run(|ui| {
            needed = width_for(ui, columns, false);
            one = width_for(ui, 1, false) - width_for(ui, 0, false);
        });
        // The message is as wide as its columns: the row is that wide, and the
        // whole message is drawn, with no ellipsis.
        for shape in harness.shapes(|ui| {
            ui.add(LogEntry::new(LogLevel::Info, &long, &long, needed));
        }) {
            if let Shape::Text(text) = shape
                && text.galley.text() != "INFO"
            {
                drawn.push((
                    text.galley.elided,
                    text.galley.text().to_owned(),
                    text.galley.size().x,
                ));
            }
        }
        assert_eq!(drawn.len(), 1);
        let (cut, text, width) = &drawn[0];
        assert!(!cut && *text == long);
        // The row has the padding, the word, and the gap before the message.
        let before = theme.space.s4 + theme.space.s4 + theme.space.s3;
        assert!(needed > before + *width - 1.0, "{needed} for {width}");
        assert!(needed < before + *width + 60.0, "{needed} for {width}");
        // Each column is the advance of the mono face.
        assert!(
            (one * columns as f32 - *width).abs() < 1.0,
            "{one} a column, {columns} columns, drawn {width}"
        );
    }

    /// The texts a row draws, with their color and where they start.
    fn texts(harness: &Harness, entry: LogEntry<'_>) -> Vec<(String, Color32, f32)> {
        harness
            .shapes(|ui| {
                ui.add(entry);
            })
            .into_iter()
            .filter_map(|shape| match shape {
                Shape::Text(text) => Some((
                    text.galley.text().to_owned(),
                    text.fallback_color,
                    text.pos.x,
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_stamped_log_has_a_time_column_and_a_continuation_sits_under_the_message() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let stamp = "2026-11-24 10:15:03";
        let head = texts(
            &harness,
            LogEntry::new(LogLevel::Error, "boom", "raw", 600.0).stamp(stamp),
        );
        let words: Vec<&str> = head.iter().map(|(text, _, _)| text.as_str()).collect();
        assert_eq!(words, [stamp, "ERROR", "boom"]);
        // The stamp is `mute`, before the word.
        assert_eq!(head[0].1, theme.color.mute);
        assert!(head[0].2 < head[1].2 && head[1].2 < head[2].2);
        // A longer stamp is cut to the column.
        let iso = texts(
            &harness,
            LogEntry::new(LogLevel::Info, "hi", "raw", 600.0)
                .stamp("2026-11-24T10:15:03.123+00:00"),
        );
        assert_eq!(iso[0].0, "2026-11-24T10:15:03");
        // A line of the trace: no stamp, no word, `mute`, where the message starts.
        let trace = texts(
            &harness,
            LogEntry::new(LogLevel::Error, "#0 /app/Foo.php(12)", "#0", 600.0)
                .stamp("")
                .continuation(true),
        );
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0].0, "#0 /app/Foo.php(12)");
        assert_eq!(trace[0].1, theme.color.mute);
        assert!((trace[0].2 - head[2].2).abs() < 0.5, "{trace:?} {head:?}");
        // No tint of its own, though its level is Error.
        let tinted = harness
            .shapes(|ui| {
                ui.add(LogEntry::new(LogLevel::Error, "#1", "#1", 300.0).continuation(true));
            })
            .into_iter()
            .any(|shape| matches!(shape, Shape::Rect(rect) if rect.fill == theme.color.error_bg));
        assert!(!tinted);
        // A screen reader hears the line, with no level it does not have.
        let tree = harness.accessibility(|ui| {
            ui.add(LogEntry::new(LogLevel::Error, "#2 x", "#2 x", 300.0).continuation(true));
        });
        assert!(tree.node_with_text("#2 x").is_some());
        // The time column makes the row that much wider.
        let (mut plain, mut stamped) = (0.0, 0.0);
        harness.run(|ui| {
            plain = width_for(ui, 10, false);
            stamped = width_for(ui, 10, true);
        });
        let column = head[1].2 - head[0].2;
        assert!(
            (stamped - plain - column).abs() < 1.0,
            "{plain} {stamped} {column}"
        );
    }

    #[test]
    fn a_row_wider_than_the_window_is_one_row_and_its_tint_spans_it() {
        let harness = Harness::new(egui::Theme::Light);
        let theme = &LIGHT;
        let mut rect = egui::Rect::NOTHING;
        let tint = harness.shapes(|ui| {
            rect = ui
                .add(LogEntry::new(LogLevel::Error, "boom", "boom", 5000.0))
                .rect;
        });
        assert_eq!(rect.width(), 5000.0);
        let wide = tint.iter().any(|shape| matches!(shape, Shape::Rect(r) if r.fill == theme.color.error_bg && r.rect.width() == 5000.0));
        assert!(wide, "the tint is as wide as the row");
    }
}
