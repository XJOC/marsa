//! The frame of the window (layout.md, Shell): the sidebar, the main area
//! with the screen of the [`Route`], the last notice at the bottom of the
//! main area, and the confirmation dialog over everything. [Laravel: the
//! layout file; the `match` in `screen` is `routes/web.php`.]

pub mod sidebar;

use std::hash::Hash;

use eframe::egui::{self, CentralPanel, Frame, Margin, Panel, ScrollArea, Ui};

use crate::bridge::Command;
use crate::model::{Action, Model, Removal, Route};
use crate::screens;
use crate::strings;
use crate::theme::page::CONFIRM_WIDTH;
use crate::theme::{Direction, Theme};
use crate::widgets::whole;
use sidebar::Item;

/// What the user did in one frame.
#[derive(Debug, Default)]
pub struct Output {
    /// For [`Model::apply_action`].
    pub actions: Vec<Action>,
    /// From the screens that still call the model's methods themselves.
    pub commands: Vec<Command>,
}

/// Draws the whole window. `picking`: a folder dialog is open.
pub fn show(ui: &mut Ui, model: &mut Model, picking: bool) -> Output {
    let theme = Theme::of(ui.ctx());
    let mut out = Output::default();
    let narrow = sidebar::is_narrow(theme, ui.max_rect().width());
    let width = if narrow || model.sidebar_folded {
        theme.size.sidebar_folded
    } else {
        theme.size.sidebar
    };
    let side = Frame::new()
        .fill(theme.color.side)
        .inner_margin(Margin::symmetric(
            whole(theme.size.sidebar_inset()),
            whole(theme.space.s3),
        ));
    Direction::CURRENT
        .start_panel("sidebar")
        .exact_size(width)
        .resizable(false)
        .frame(side)
        .show(ui, |ui| {
            out.actions.extend(sidebar::show(ui, model, narrow))
        });
    let (top, sides) = (
        whole(theme.space.screen_top),
        whole(theme.space.screen_side),
    );
    let main = Frame::new().fill(theme.color.bg).inner_margin(Margin {
        left: sides,
        right: sides,
        top,
        bottom: top,
    });
    CentralPanel::default().frame(main).show(ui, |ui| {
        if let Some(code) = &model.notice {
            // TODO(M5-D9): a `widgets::NoticeBar` that closes.
            let gap = Margin {
                top: whole(theme.space.s3),
                ..Margin::ZERO
            };
            Panel::bottom("notice")
                .frame(Frame::new().inner_margin(gap))
                .show_separator_line(false)
                .show(ui, |ui| {
                    ui.colored_label(ui.visuals().error_fg_color, strings::error_text(code));
                });
        }
        screen(ui, model, picking, &mut out);
    });
    // Opened by an Uninstall, Unlink, or Forget button.
    let ctx = ui.ctx().clone();
    out.commands.extend(confirm(&ctx, model));
    out
}

/// The screen of the route, in the main area. The screens that do not
/// return actions yet still call the model's methods; each one's own task
/// converts it.
fn screen(ui: &mut Ui, model: &mut Model, picking: bool, out: &mut Output) {
    let item = Item::of(&model.route);
    match item {
        // First run replaces Home until the first site can open.
        Item::Home => scrolled(ui, item, |ui| {
            let theme = Theme::of(ui.ctx());
            if model.needs_first_run() {
                let view = screens::first_run::view(model);
                out.actions
                    .extend(screens::first_run::show(ui, &view, theme));
            } else {
                let view = screens::home::view(model);
                out.actions.extend(screens::home::show(ui, &view, theme));
            }
        }),
        // A site's page is a screen of its own under the Sites item.
        Item::Sites => match &model.route {
            Route::Site(name) => scrolled(ui, ("site", name), |ui| {
                let theme = Theme::of(ui.ctx());
                let view = screens::site::view(model, name);
                out.actions.extend(screens::site::show(ui, &view, theme));
            }),
            _ => scrolled(ui, item, |ui| {
                let theme = Theme::of(ui.ctx());
                let view = screens::sites::view(model, picking);
                out.actions.extend(screens::sites::show(ui, &view, theme));
            }),
        },
        // The log fills the height and scrolls itself.
        Item::Logs => screens::logs::show(ui, model),
        Item::Runtimes => scrolled(ui, item, |ui| {
            let theme = Theme::of(ui.ctx());
            let view = screens::runtimes::view(model);
            out.actions
                .extend(screens::runtimes::show(ui, &view, theme));
        }),
        Item::Services => scrolled(ui, item, |ui| {
            out.actions.extend(screens::services::show(ui, model));
        }),
        Item::Settings => scrolled(ui, item, |ui| {
            out.commands.extend(screens::settings::show(ui, model));
        }),
    }
}

/// A screen that grows with its content scrolls when the window is short.
/// Each screen keeps its own scroll position, and so does each site's page:
/// `key` names it.
fn scrolled(ui: &mut Ui, key: impl Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    ScrollArea::vertical()
        .id_salt(("screen", key))
        .auto_shrink(false)
        .show(ui, add);
}

/// The confirmation of an Uninstall, Unlink, or Forget, over the whole
/// window. Escape or a click beside it cancels. Returns the removal to send
/// once the user confirms.
// TODO(M5-D9): the design's `widgets::ConfirmDialog` over the scrim.
fn confirm(ctx: &egui::Context, model: &mut Model) -> Option<Command> {
    let removal = model.confirm.clone()?;
    let text = match &removal {
        Removal::Unlink { site } => strings::confirm_text(&removal, model.site_root(site), 0),
        Removal::Forget { folder } => strings::confirm_text(&removal, None, model.sites_in(folder)),
        Removal::Uninstall { .. } => strings::confirm_text(&removal, None, 0),
    };
    let mut confirmed = false;
    let mut cancelled = false;
    let modal = egui::Modal::new(egui::Id::unique("confirm_removal")).show(ctx, |ui| {
        ui.set_max_width(CONFIRM_WIDTH);
        ui.strong(strings::confirm_title(&removal));
        ui.label(text);
        ui.horizontal(|ui| {
            confirmed = ui.button(strings::removal_button(&removal)).clicked();
            cancelled = ui.button(strings::CANCEL).clicked();
        });
    });
    if confirmed {
        return model.confirm_removal().map(Command::Remove);
    }
    if cancelled || modal.should_close() {
        model.cancel_removal();
    }
    None
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use marsa_core::types::{Health, Site, SiteKind, SiteList};

    use super::*;
    use crate::model::Update;
    use crate::widgets::harness::Harness;

    fn model_with_shop() -> Model {
        let mut model = Model::default();
        model.update(Update::Connected(Health {
            app_version: "0.0.0".into(),
            api_version: 1,
            pid: 7,
        }));
        let root = PathBuf::from("/code/shop");
        model.update(Update::Sites(SiteList {
            sites: vec![Site {
                name: "shop".into(),
                docroot: root.join("public"),
                root,
                kind: SiteKind::Laravel,
                php: None,
                php_source: None,
                secure: true,
            }],
            ..SiteList::default()
        }));
        model
    }

    /// The tree of one frame of the whole window on `route`.
    fn tree_on(route: Route) -> crate::widgets::harness::Tree {
        let mut model = model_with_shop();
        model.route = route;
        Harness::new(egui::Theme::Light).accessibility(|ui| {
            show(ui, &mut model, false);
        })
    }

    #[test]
    fn a_site_route_draws_the_page_of_that_site_under_the_sites_item() {
        let tree = tree_on(Route::Site("shop".into()));
        // The page has its own buttons; the Sites screen has none of them.
        assert!(tree.node_with_label(strings::OPEN_FOLDER).is_some());
        assert!(tree.node_with_label(strings::ADD_SITE_BUTTON).is_none());
        let sites = tree_on(Route::Sites);
        assert!(sites.node_with_label(strings::ADD_SITE_BUTTON).is_some());
        assert!(sites.node_with_label(strings::OPEN_FOLDER).is_none());
    }

    #[test]
    fn a_site_that_is_gone_shows_a_message_and_the_way_back_and_does_not_panic() {
        let tree = tree_on(Route::Site("gone".into()));
        assert!(
            tree.node_with_text(&strings::site_missing("gone"))
                .is_some()
        );
        assert!(tree.node_with_label(strings::OPEN_FOLDER).is_none());
    }
}
