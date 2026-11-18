//! The screens of the window (code.md, Target layout): one file per
//! screen. [Laravel: the views.] The sections of the old one-page window
//! moved here without a change in behavior (task M5-D4); each screen's own
//! task redesigns it (M5-D5 to M5-D10).

pub mod first_run;
pub mod home;
pub mod logs;
pub mod runtimes;
pub mod services;
pub mod settings;
pub mod sites;

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{self, Label, Ui};

use crate::model::{InstallStep, Model};
use crate::strings::{self, ReportLine};
use crate::theme::page::PROGRESS_WIDTH;
use crate::theme::{TextToken, Theme};

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

/// One line of a report: in the error color if it tells of a problem.
pub fn report_line(ui: &mut egui::Ui, line: ReportLine) {
    if line.problem {
        ui.colored_label(ui.visuals().error_fg_color, line.text);
    } else {
        ui.label(line.text);
    }
}

/// The install the model shows: a progress bar while it runs, then one
/// line. No animation: the bar moves with the daemon's events.
pub fn install_progress(ui: &mut egui::Ui, model: &Model) {
    let Some(view) = model.install_view() else {
        return;
    };
    let text = strings::install_status(view);
    match &view.step {
        InstallStep::Failed(_) => {
            ui.colored_label(ui.visuals().error_fg_color, text);
        }
        InstallStep::Done { .. } => {
            ui.label(text);
        }
        step => {
            let bar = egui::ProgressBar::new(step.fraction())
                .desired_width(PROGRESS_WIDTH)
                .text(text);
            ui.add(bar);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::LIGHT;
    use crate::widgets::harness::Harness;

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
