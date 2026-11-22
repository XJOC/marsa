//! The dialogs, notices, and progress page of the gallery.
use std::time::Instant;

use eframe::egui::{self, Rect, Sense, Ui, UiBuilder, pos2, vec2};

use crate::theme::Theme;
use crate::widgets::button::Button;
use crate::widgets::dialog::ConfirmDialog;
use crate::widgets::notice::{Notice, NoticeBar, Progress, show_bar};
use crate::widgets::spot;
use crate::widgets::step::{Stage, StepCard};
use crate::widgets::table::{Table, TableColumn};

/// Set to `1` to show the gallery.
use super::{Live, caption, heading, label, row};

/// How much of `total` is `done`: "18 of 31 MB".
fn share_of(done: u8, total: u8) -> f32 {
    f32::from(done) / f32::from(total)
}

/// The question of the sample dialogs.
fn unlink() -> ConfirmDialog<'static> {
    ConfirmDialog::new(
        "gallery_unlink",
        "Unlink shop?",
        "Marsa stops serving https://shop.test. The folder ~/Code/shop and its files stay where they are.",
        "Unlink",
    )
    .danger()
}

pub(super) fn dialogs_page(ui: &mut Ui, theme: &Theme) {
    let id = ui.make_persistent_id("gallery_dialogs");
    let mut live = ui
        .ctx()
        .data(|data| data.get_temp::<Live>(id))
        .unwrap_or_default();

    heading(ui, theme, label::DIALOG);
    // A window behind a scrim, with the dialog on it, and a calm dialog
    // beside it.
    let height = theme.size.sidebar + theme.space.s6;
    let (region, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let mut page = ui.new_child(UiBuilder::new().max_rect(region));
    let columns = [
        TableColumn::fill("Site"),
        TableColumn::fixed("PHP", theme.size.version_lg.x),
    ];
    Table::new("gallery_behind", "Sites", &columns).show(&mut page, 3, |row| {
        let name = ["shop", "blog", "docs"][row.index()];
        row.cell(|ui| {
            ui.label(name);
        });
        row.cell(|ui| {
            ui.label("8.4");
        });
    });
    ui.painter()
        .rect_filled(region, theme.radius.lg, theme.color.scrim);
    let at = pos2(
        region.left() + theme.space.s6,
        region.top() + theme.space.s6,
    );
    let card = Rect::from_min_size(at, vec2(theme.size.dialog_width, height));
    unlink().show_card(&mut ui.new_child(UiBuilder::new().max_rect(card)));
    let calm = spot(
        card.right() + theme.space.s6,
        at.y,
        theme.size.dialog_width,
        height,
    );
    let dialog = ConfirmDialog::new(
        "gallery_install",
        "Install PHP 8.5?",
        "Marsa downloads 31 MB and keeps PHP 8.4 as the default.",
        "Install",
    );
    dialog.show_card(&mut ui.new_child(UiBuilder::new().max_rect(calm)));

    heading(ui, theme, label::NOTICES);
    row(ui, |ui| {
        NoticeBar::new(&Notice::success("Unlinked shop.").with_action("Undo")).show(ui);
        NoticeBar::new(&Notice::error("Caddy stopped.").with_action("Show log")).show(ui);
    });

    heading(ui, theme, label::PROGRESS);
    row(ui, |ui| {
        ui.set_width(theme.size.dialog_width);
        ui.add(Progress::new(
            "Downloading PHP 8.4.27: 18 of 31 MB",
            Some(share_of(18, 31)),
        ));
    });
    row(ui, |ui| {
        ui.set_width(theme.size.dialog_width);
        ui.add(Progress::new("Starting Caddy…", None));
    });

    heading(ui, theme, label::STEPS);
    ui.scope(|ui| {
        ui.set_width(theme.size.dialog_width);
        ui.spacing_mut().item_spacing.y = theme.space.group_gap_max;
        let steps = [
            (1, "Install PHP 8.4 and Caddy", Stage::Done),
            (2, "Set up HTTPS for .test sites", Stage::Current),
            (3, "Add your first site", Stage::Next),
        ];
        for (number, title, stage) in steps {
            let card = StepCard::new(number, title, "One line that says what this does.", stage);
            card.show_with(ui, |ui| {
                if stage == Stage::Current {
                    ui.add(Progress::new("Setting up: 2 of 3", Some(share_of(2, 3))));
                }
            });
        }
    });

    heading(ui, theme, label::DIALOG_LIVE);
    row(ui, |ui| {
        live.dialog |= ui.add(Button::secondary("Unlink shop…")).clicked();
        if ui.add(Button::secondary("Show a success")).clicked() {
            let notice = Notice::success("Unlinked shop.").with_action("Undo");
            live.notice = Some((notice, Instant::now()));
        }
        if ui.add(Button::secondary("Show an error")).clicked() {
            let notice = Notice::error("Caddy stopped.").with_action("Show log");
            live.notice = Some((notice, Instant::now()));
        }
        if let Some(answer) = live.answer {
            caption(ui, theme, &format!("Answered: {answer:?}"));
        }
    });
    if live.dialog
        && let Some(answer) = unlink().show(ui.ctx())
    {
        live.dialog = false;
        live.answer = Some(answer);
    }
    show_notice(ui.ctx(), &mut live.notice);
    ui.ctx().data_mut(|data| data.insert_temp(id, live));
}

/// Draws the notice at the bottom of the window: a success closes after its
/// lifetime, and either kind closes when the user presses a button.
fn show_notice(ctx: &egui::Context, notice: &mut Option<(Notice, Instant)>) {
    let Some((shown, since)) = notice.as_ref() else {
        return;
    };
    if let Some(lifetime) = shown.kind.lifetime() {
        let left = lifetime.saturating_sub(since.elapsed());
        if left.is_zero() {
            *notice = None;
            return;
        }
        ctx.request_repaint_after(left);
    }
    if show_bar(ctx, ctx.content_rect(), shown).is_some() {
        *notice = None;
    }
}
