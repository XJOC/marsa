//! The screens of the window (code.md, Target layout): one file per
//! screen. [Laravel: the views.] The sections of the old one-page window
//! moved here without a change in behavior (task M5-D4); each screen's own
//! task redesigns it (M5-D5 to M5-D10).

pub mod first_run;
pub mod home;
pub mod logs;
pub mod panel;
pub mod runtimes;
pub mod services;
pub mod settings;
pub mod site;
pub mod sites;

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{Label, RichText, Ui, vec2};

use crate::theme::{Direction, Section, TextToken, Theme};
use crate::widgets::{SectionTile, TileSize};

/// A heading of a section, with a hint in `caption` after it. One line of
/// text, so the two sizes share a baseline.
pub fn heading(ui: &mut Ui, theme: &Theme, title: &str, hint: Option<&str>) {
    let format = |token: TextToken, color| TextFormat {
        font_id: token.font_id(),
        color,
        ..TextFormat::default()
    };
    let mut job = LayoutJob::default();
    job.append(title, 0.0, format(theme.text.heading, theme.color.ink));
    if let Some(hint) = hint {
        let lead = theme.space.s3;
        job.append(hint, lead, format(theme.text.caption, theme.color.mute));
    }
    ui.add(Label::new(job).selectable(false));
}

/// The header of a screen (layout.md, Shell): the tile of its section, the
/// title, then the actions pushed to the end. `actions` adds them from the
/// end: the primary button first, then the search field before it.
pub fn header(
    ui: &mut Ui,
    theme: &Theme,
    section: Section,
    title: &str,
    actions: impl FnOnce(&mut Ui),
) {
    header_with_hint(ui, theme, section, title, None, actions);
}

/// [`header`] with a hint in `caption` after the title: what the screen is
/// for ("Programs Marsa runs for your sites").
pub fn header_with_hint(
    ui: &mut Ui,
    theme: &Theme,
    section: Section,
    title: &str,
    hint: Option<&str>,
    actions: impl FnOnce(&mut Ui),
) {
    let line = vec2(ui.available_width(), theme.size.tile_header);
    ui.allocate_ui_with_layout(line, Direction::CURRENT.row(), |ui| {
        ui.spacing_mut().item_spacing.x = theme.space.s3;
        // The title names the screen: the tile is a picture beside it.
        ui.add(
            SectionTile::new(section)
                .size(TileSize::Header)
                .decorative(),
        );
        let text = RichText::new(title)
            .font(theme.text.title.font_id())
            .color(theme.color.ink);
        ui.add(Label::new(text).selectable(false));
        if let Some(hint) = hint {
            let text = RichText::new(hint)
                .font(theme.text.body_sm.font_id())
                .color(theme.color.mute);
            ui.add(Label::new(text).truncate().selectable(false));
        }
        ui.with_layout(Direction::CURRENT.row_from_end(), actions);
    });
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Rect};

    use super::*;
    use crate::theme::LIGHT;
    use crate::widgets::Button;
    use crate::widgets::harness::Harness;

    #[test]
    fn a_header_is_a_tile_and_a_title_with_the_actions_at_the_end() {
        let harness = Harness::new(egui::Theme::Light);
        let (mut action, mut shown, mut width) = (Rect::NOTHING, Rect::NOTHING, 0.0);
        let tree = harness.accessibility(|ui| {
            width = ui.available_width();
            let start = ui.min_rect().left();
            header(ui, &LIGHT, Section::Sites, "Sites", |ui| {
                action = ui.add(Button::primary("Add site…")).rect;
            });
            shown = Rect::from_min_max(
                egui::pos2(start, ui.min_rect().top()),
                ui.min_rect().right_bottom(),
            );
        });
        // The action ends the line; the line is as high as the tile.
        assert!((action.right() - shown.right()).abs() < 0.5);
        assert!((shown.height() - LIGHT.size.tile_header).abs() < 0.5);
        assert!(tree.node_with_text("Sites").is_some());
        assert!(width > 0.0);
    }

    #[test]
    fn a_header_may_say_what_the_screen_is_for_after_its_title() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            header_with_hint(
                ui,
                &LIGHT,
                Section::Services,
                "Services",
                Some("Programs Marsa runs for your sites"),
                |_| {},
            );
        });
        assert!(tree.node_with_text("Services").is_some());
        assert!(
            tree.node_with_text("Programs Marsa runs for your sites")
                .is_some()
        );
    }

    #[test]
    fn a_heading_names_the_section_and_a_hint_follows_it() {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            heading(
                ui,
                &LIGHT,
                "Parked folders",
                Some("every sub-folder is a site"),
            );
        });
        // One label holds both texts, so a screen reader reads them together.
        assert!(
            tree.node_with_text("Parked foldersevery sub-folder is a site")
                .is_some()
        );
    }
}
