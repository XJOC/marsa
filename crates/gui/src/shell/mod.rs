//! The frame of the window (layout.md, Shell): the sidebar, the main area
//! with the screen of the [`Route`], the notice bar at the bottom of the
//! main area, and the dialogs (the confirmation, the install dialog) over
//! everything. [Laravel: the
//! layout file; the `match` in `screen` is `routes/web.php`.]

pub(crate) mod dialogs;
pub(crate) mod notices;
pub(crate) mod sidebar;

use std::hash::Hash;
use std::time::Instant;

use eframe::egui::{CentralPanel, Frame, Margin, ScrollArea, Ui};

use crate::model::{Action, LogSource, Model, Route};
use crate::screens;
use crate::theme::{Direction, Theme};
use crate::widgets::whole;
use sidebar::Item;

/// What the user did in one frame.
#[derive(Debug, Default)]
pub(crate) struct Output {
    /// For [`Model::apply_action`].
    pub actions: Vec<Action>,
}

/// Draws the whole window. `picking`: a folder dialog is open.
pub(crate) fn show(ui: &mut Ui, model: &Model, picking: bool) -> Output {
    show_at(ui, model, picking, Instant::now())
}

/// [`show`] at the time `now`: the notice bar closes a success by the clock,
/// so a test moves the clock by hand.
pub(crate) fn show_at(ui: &mut Ui, model: &Model, picking: bool, now: Instant) -> Output {
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
    let main_area = CentralPanel::default()
        .frame(main)
        .show(ui, |ui| screen(ui, model, picking, &mut out))
        .response
        .rect;
    // The result of the last action, over the bottom of the main area: on any
    // screen, and under the dialogs.
    out.actions
        .extend(notices::show(ui.ctx(), main_area, model, now));
    // The confirmation of an Uninstall, Unlink, or Forget, or the install
    // dialog ("Install a version…" or a dashed "+" on any screen): the shell
    // draws them, not a screen.
    out.actions.extend(dialogs::show(ui.ctx(), model));
    out
}

/// The screen of the route, in the main area. Every screen returns what the
/// user did as actions.
fn screen(ui: &mut Ui, model: &Model, picking: bool, out: &mut Output) {
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
        Item::Logs => {
            let theme = Theme::of(ui.ctx());
            let source = match &model.route {
                Route::Logs(source) => source.clone(),
                _ => LogSource::default(),
            };
            let view = screens::logs::view(model, &source);
            out.actions.extend(screens::logs::show(ui, &view, theme));
        }
        // The list and the text fill the height and scroll themselves.
        Item::Mail => {
            let theme = Theme::of(ui.ctx());
            let view = screens::mail::view(model, screens::mail::now_ms());
            out.actions.extend(screens::mail::show(ui, &view, theme));
        }
        Item::Runtimes => scrolled(ui, item, |ui| {
            let theme = Theme::of(ui.ctx());
            let view = screens::runtimes::view(model);
            out.actions
                .extend(screens::runtimes::show(ui, &view, theme));
        }),
        Item::Services => scrolled(ui, item, |ui| {
            let theme = Theme::of(ui.ctx());
            let view = screens::services::view(model);
            out.actions
                .extend(screens::services::show(ui, &view, theme));
        }),
        Item::Settings => scrolled(ui, item, |ui| {
            let theme = Theme::of(ui.ctx());
            let view = screens::settings::view(model);
            out.actions
                .extend(screens::settings::show(ui, &view, theme));
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use eframe::egui;
    use marsa_core::types::{Health, Site, SiteKind, SiteList};

    use super::*;
    use crate::model::NoticeSource;
    use crate::model::Update;
    use crate::strings;
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
            show(ui, &model, false);
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

    /// The tree of the window on `route` after `frames` frames, with the
    /// install dialog of PHP open.
    fn tree_with_install_dialog(route: Route) -> crate::widgets::harness::Tree {
        let mut model = model_with_shop();
        model.route = route;
        model.update(Update::Runtimes(marsa_core::types::RuntimeList {
            runtimes: vec![marsa_core::types::RuntimeVersions {
                runtime: marsa_core::runtimes::Runtime::Php,
                installed: vec!["8.4.27".into()],
                default: Some("8.4".into()),
            }],
        }));
        model.apply_action(Action::AskInstall(marsa_core::runtimes::Runtime::Php));
        assert_eq!(
            model.session.install_dialog,
            Some(marsa_core::runtimes::Runtime::Php)
        );
        let harness = Harness::new(egui::Theme::Light);
        for _ in 0..20 {
            harness.run(|ui| {
                show(ui, &model, false);
            });
        }
        harness.accessibility(|ui| {
            show(ui, &model, false);
        })
    }

    #[test]
    fn the_install_dialog_covers_any_screen_not_only_runtimes() {
        for route in [
            Route::Home,
            Route::Site("shop".into()),
            Route::Sites,
            Route::Runtimes(marsa_core::runtimes::Runtime::Node),
        ] {
            let tree = tree_with_install_dialog(route.clone());
            assert!(
                tree.node_with_label("Install a PHP version").is_some(),
                "{route:?}"
            );
            assert!(tree.node_with_label("Version").is_some(), "{route:?}");
        }
        // Closed, it is on no screen.
        assert!(
            tree_on(Route::Home)
                .node_with_label("Install a PHP version")
                .is_none()
        );
    }

    /// The tree of the window on `route` after frames enough for the bar to
    /// size itself, with `model` as it is.
    fn tree_of(model: &mut Model) -> crate::widgets::harness::Tree {
        let harness = Harness::new(egui::Theme::Light);
        for _ in 0..20 {
            harness.run(|ui| {
                show(ui, model, false);
            });
        }
        harness.accessibility(|ui| {
            show(ui, model, false);
        })
    }

    #[test]
    fn a_result_shows_in_the_bar_whatever_screen_the_user_is_on() {
        use crate::model::SitesNotice;
        let text = strings::unlinked("shop");
        for route in [
            Route::Home,
            Route::Sites,
            Route::Site("shop".into()),
            Route::Runtimes(marsa_core::runtimes::Runtime::Node),
            Route::Services,
            Route::Settings,
        ] {
            let mut model = model_with_shop();
            model.route = route.clone();
            model
                .session
                .notices
                .set_sites(SitesNotice::Unlinked("shop".into()));
            let tree = tree_of(&mut model);
            assert!(tree.node_with_text(&text).is_some(), "{route:?}");
        }
        // No result, no bar.
        let mut model = model_with_shop();
        assert!(tree_of(&mut model).node_with_text(&text).is_none());
    }

    #[test]
    fn a_failed_call_is_an_error_notice_that_stays_and_closes_with_its_button() {
        let mut model = model_with_shop();
        model
            .session
            .notices
            .set_code(NoticeSource::Call, "service_busy".into());
        let tree = tree_of(&mut model);
        let error = strings::error_text("service_busy");
        let bar = tree.node_with_text(&error).expect("the bar");
        assert_eq!(bar.role(), egui::Role::Alert);
        assert!(tree.node_with_label(strings::CLOSE).is_some());
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
