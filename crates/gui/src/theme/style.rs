//! The tokens as egui styles. egui keeps one style per egui theme and draws
//! with the one that matches the system setting (or the user's choice), so
//! both are built once, at start, and nothing is built per frame.

use std::collections::BTreeMap;

use eframe::egui::style::{Selection, WidgetVisuals, Widgets};
use eframe::egui::{self, Color32, CornerRadius, FontId, Stroke, TextStyle, Visuals};

use super::{DARK, LIGHT, Theme, fonts};

/// Installs the fonts and the light and dark styles. egui then follows the
/// system's setting by itself.
pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts::definitions());
    ctx.set_style_of(egui::Theme::Light, style(&LIGHT));
    ctx.set_style_of(egui::Theme::Dark, style(&DARK));
}

/// egui's style for `theme`. Spacing stays egui's: the screens keep their
/// layout until the widgets of M5-D2 draw with the size tokens.
pub fn style(theme: &Theme) -> egui::Style {
    egui::Style {
        text_styles: text_styles(theme),
        visuals: visuals(theme),
        ..egui::Style::default()
    }
}

/// egui's text styles: captions, body, buttons, headings, and code.
fn text_styles(theme: &Theme) -> BTreeMap<TextStyle, FontId> {
    let text = &theme.text;
    BTreeMap::from([
        (TextStyle::Small, text.caption.font_id()),
        (TextStyle::Body, text.body.font_id()),
        (TextStyle::Button, text.body_sm.font_id()),
        (TextStyle::Heading, text.heading.font_id()),
        (TextStyle::Monospace, text.mono.font_id()),
    ])
}

fn visuals(theme: &Theme) -> Visuals {
    let c = &theme.color;
    let border = theme.size.border;
    let focus = Stroke::new(theme.focus.width, c.accent);
    // egui's own light or dark visuals give the text rendering of each mode;
    // every color is replaced below.
    let base = if theme.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let mut ime = base.ime_composition;
    ime.active_underline_stroke = focus;
    ime.inactive_underline_stroke = Stroke::new(border, c.mute);
    Visuals {
        dark_mode: theme.dark,
        override_text_color: None,
        weak_text_color: Some(c.mute),
        widgets: widgets(theme),
        // The selected entry of a list, selected text, and the border of
        // the text field that has the focus.
        selection: Selection {
            bg_fill: c.selected,
            stroke: focus,
        },
        ime_composition: ime,
        hyperlink_color: c.accent,
        // Striped rows of the tables.
        faint_bg_color: c.side,
        // Text fields, progress bars.
        extreme_bg_color: c.surface,
        text_edit_bg_color: Some(c.surface),
        code_bg_color: c.side,
        warn_fg_color: theme.sections.dumps.fg,
        error_fg_color: c.error,
        // Dialogs and other windows.
        window_corner_radius: CornerRadius::same(theme.radius.xl),
        window_shadow: theme.elevation.dialog,
        window_fill: c.surface,
        window_stroke: Stroke::new(border, c.line),
        // Menus, lists, and tooltips.
        menu_corner_radius: CornerRadius::same(theme.radius.md),
        popup_shadow: theme.elevation.menu,
        panel_fill: c.bg,
        text_cursor: egui::style::TextCursorStyle {
            stroke: focus,
            ..base.text_cursor
        },
        disabled_alpha: theme.state.disabled_opacity,
        ..base
    }
}

/// The look of controls at rest, hovered, pressed or focused, and open.
/// At rest a control is a secondary button: `surface` fill, `control`
/// border, `ink` text. The pressed or focused one gets the focus ring.
fn widgets(theme: &Theme) -> Widgets {
    let c = &theme.color;
    let border = theme.size.border;
    let radius = CornerRadius::same(theme.radius.md);
    let control = |fill: Color32, stroke: Stroke| WidgetVisuals {
        bg_fill: fill,
        weak_bg_fill: fill,
        bg_stroke: stroke,
        corner_radius: radius,
        fg_stroke: Stroke::new(border, c.ink),
        expansion: 0.0,
    };
    let rest = Stroke::new(border, c.control);
    Widgets {
        // Labels, separators, and frames: text in `ink`, lines in `line`.
        noninteractive: WidgetVisuals {
            bg_stroke: Stroke::new(border, c.line),
            ..control(c.bg, rest)
        },
        inactive: control(c.surface, rest),
        hovered: control(tone(theme, c.surface, theme.state.hover), rest),
        active: control(
            tone(theme, c.surface, theme.state.pressed),
            Stroke::new(theme.focus.width, c.accent),
        ),
        open: control(c.selected, rest),
    }
}

/// `color` moved `amount` toward `ink`: darker in the light theme, lighter
/// in the dark one (components.md, hover and pressed).
fn tone(theme: &Theme, color: Color32, amount: f32) -> Color32 {
    color.lerp_to_gamma(theme.color.ink, amount)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{check, contrast};
    use super::*;

    const TEXT: f64 = 4.5;
    const UI: f64 = 3.0;

    /// The pairs egui really draws with the styles: every text on the fill
    /// it sits on, and every control border on the window.
    #[test]
    fn egui_draws_every_text_and_control_with_enough_contrast() {
        for (name, theme) in [("light", &LIGHT), ("dark", &DARK)] {
            let v = style(theme).visuals;
            let w = &v.widgets;
            let mut pairs = Vec::new();
            for (bg_name, bg) in [
                ("panel", v.panel_fill),
                ("window", v.window_fill),
                ("striped row", v.faint_bg_color),
                ("text field", v.text_edit_bg_color()),
            ] {
                for (fg_name, fg) in [
                    ("text", v.text_color()),
                    ("weak text", v.weak_text_color()),
                    ("strong text", v.strong_text_color()),
                    ("link", v.hyperlink_color),
                    ("error", v.error_fg_color),
                    ("warning", v.warn_fg_color),
                ] {
                    pairs.push((format!("{fg_name} on {bg_name}"), fg, bg, TEXT));
                }
                pairs.push((
                    format!("control border on {bg_name}"),
                    w.inactive.bg_stroke.color,
                    bg,
                    UI,
                ));
                pairs.push((
                    format!("focus ring on {bg_name}"),
                    w.active.bg_stroke.color,
                    bg,
                    UI,
                ));
                pairs.push((
                    format!("field focus on {bg_name}"),
                    v.selection.stroke.color,
                    bg,
                    UI,
                ));
            }
            for (state, visuals) in [
                ("at rest", &w.inactive),
                ("hovered", &w.hovered),
                ("pressed", &w.active),
                ("open", &w.open),
            ] {
                let pair = format!("button text {state}");
                pairs.push((pair, visuals.fg_stroke.color, visuals.weak_bg_fill, TEXT));
            }
            pairs.extend([
                (
                    "selected entry".into(),
                    v.selection.stroke.color,
                    v.selection.bg_fill,
                    TEXT,
                ),
                (
                    "selected text".into(),
                    v.text_color(),
                    v.selection.bg_fill,
                    TEXT,
                ),
                (
                    "progress text".into(),
                    v.selection.stroke.color,
                    v.extreme_bg_color,
                    TEXT,
                ),
            ]);
            let pairs: Vec<_> = pairs
                .iter()
                .map(|(pair, fg, bg, min)| (pair.as_str(), *fg, *bg, *min))
                .collect();
            check(name, &pairs);
        }
    }

    #[test]
    fn hover_and_press_move_toward_ink() {
        for theme in [&LIGHT, &DARK] {
            let w = style(theme).visuals.widgets;
            let surface = contrast(theme.color.surface, theme.color.ink);
            let hovered = contrast(w.hovered.weak_bg_fill, theme.color.ink);
            let pressed = contrast(w.active.weak_bg_fill, theme.color.ink);
            assert!(surface > hovered && hovered > pressed);
        }
    }

    #[test]
    fn apply_sets_both_styles_and_the_fonts() {
        let ctx = egui::Context::default();
        apply(&ctx);
        assert_eq!(
            ctx.style_of(egui::Theme::Light).visuals.panel_fill,
            LIGHT.color.bg
        );
        assert_eq!(
            ctx.style_of(egui::Theme::Dark).visuals.panel_fill,
            DARK.color.bg
        );
        assert!(!ctx.style_of(egui::Theme::Light).visuals.dark_mode);
        assert!(ctx.style_of(egui::Theme::Dark).visuals.dark_mode);
        let body = &ctx.style_of(egui::Theme::Dark).text_styles[&TextStyle::Body];
        assert_eq!(body.size, DARK.text.body.size);
        // Theme::of follows egui's theme.
        ctx.set_theme(egui::Theme::Light);
        assert_eq!(Theme::of(&ctx), &LIGHT);
        ctx.set_theme(egui::Theme::Dark);
        assert_eq!(Theme::of(&ctx), &DARK);
    }
}
