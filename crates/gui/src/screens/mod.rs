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

use eframe::egui;

use crate::model::{InstallStep, Model};
use crate::strings::{self, ReportLine};
use crate::theme::page::PROGRESS_WIDTH;

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
