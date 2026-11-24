//! The dialogs the shell draws over the whole window, after the screen
//! (components.md, Dialogs): the confirmation of a removal (Unlink, Forget,
//! Uninstall, Delete all mail) and the install dialog. Every one is the design's
//! [`ConfirmDialog`] over the scrim, so no screen draws a dialog of its own
//! and any screen can open one.
//!
//! Like a screen, the host is two functions. [`confirm_view`] reads the
//! [`Model`] and decides what the confirmation says: pure, so the tests assert
//! it without a window. [`show`] draws it and returns what the user did as
//! [`Action`]s (code.md, State). [Laravel: `confirm_view` is the controller
//! that builds the data; `show` is the Blade template of the modal.]
//!
//! A removal takes the confirmation of a destructive choice: its button is the
//! danger style, Cancel has the focus when the dialog opens (so Enter alone
//! never removes anything), Tab moves on to the action, and Escape cancels. A
//! click on the scrim answers nothing.

use eframe::egui::Context;

use crate::model::{Action, Model, Removal};
use crate::screens::runtimes;
use crate::widgets::{Answer, ConfirmDialog};

/// What a confirmation says.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ConfirmView {
    /// Tells one kind of removal from another: the dialog takes the focus
    /// again when a different one replaces it.
    pub id: &'static str,
    /// The exact question: "Unlink shop.test?".
    pub title: String,
    /// What happens, and what stays.
    pub body: String,
    /// The action, named by its verb: "Unlink".
    pub confirm: &'static str,
}

/// Decides what the confirmation of the model's pending removal says, if
/// there is one. Pure: the same model gives the same view.
pub(crate) fn confirm_view(model: &Model) -> Option<ConfirmView> {
    let removal = model.session.confirm.as_ref()?;
    let home = model.home.as_deref();
    let (id, body) = match removal {
        Removal::Unlink { site } => ("unlink", removal.text(model.site_root(site), 0, home)),
        Removal::Forget { folder } => ("forget", removal.text(None, model.sites_in(folder), home)),
        Removal::Uninstall { .. } => ("uninstall", removal.text(None, 0, home)),
        Removal::Mail => ("delete_mail", removal.text(None, 0, home)),
    };
    Some(ConfirmView {
        id,
        title: removal.title(home),
        body,
        confirm: removal.button(),
    })
}

/// Draws the dialog the model asks for: the confirmation of a removal, else
/// the install dialog. One dialog at a time. Returns what the user did.
pub(crate) fn show(ctx: &Context, model: &Model) -> Vec<Action> {
    if let Some(view) = confirm_view(model) {
        return show_confirm(ctx, &view);
    }
    // Opened by "Install a version…" or a dashed "+" on any screen.
    match runtimes::install_dialog_view(model) {
        Some(dialog) => runtimes::show_install_dialog(ctx, &dialog),
        None => Vec::new(),
    }
}

/// Every removal destroys something: its button is the danger style.
fn show_confirm(ctx: &Context, view: &ConfirmView) -> Vec<Action> {
    let dialog = ConfirmDialog::new(view.id, &view.title, &view.body, view.confirm).danger();
    match dialog.show(ctx) {
        Some(Answer::Confirm) => vec![Action::ConfirmRemoval],
        Some(Answer::Cancel) => vec![Action::CancelRemoval],
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use crate::model::Effect;
    use std::path::PathBuf;

    use eframe::egui::{self, Key, Role};
    use marsa_core::runtimes::Runtime;
    use marsa_core::types::{Health, Site, SiteKind, SiteList};
    use semver::Version;

    use super::*;
    use crate::bridge::Command;
    use crate::fixtures::abs;
    use crate::model::Update;
    use crate::widgets::harness::{Harness, Tree};

    /// A window with one linked site `shop`, a parked folder `/code` with a
    /// site `api` in it, and PHP 8.3.27 installed, under the home folder
    /// `/home`.
    fn model() -> Model {
        let mut model = Model::default();
        model.update(Update::Connected(Health {
            app_version: "0.0.0".into(),
            api_version: 1,
            pid: 7,
        }));
        let site = |name: &str, root: PathBuf| Site {
            name: name.into(),
            docroot: root.join("public"),
            root,
            kind: SiteKind::Laravel,
            php: None,
            php_source: None,
            secure: true,
        };
        model.update(Update::Sites(SiteList {
            sites: vec![
                site("shop", abs("/home/Code/shop")),
                site("api", PathBuf::from("/code/api")),
            ],
            parked: vec![PathBuf::from("/code")],
            ..SiteList::default()
        }));
        model.home = Some(abs("/home"));
        model
    }

    fn version() -> Version {
        "8.3.27".parse().expect("a version")
    }

    fn asked(model: &mut Model, action: Action) {
        model.apply_action(action);
    }

    /// Frames enough for the dialog to size itself and fade in.
    fn settle(harness: &Harness, model: &Model) {
        for _ in 0..20 {
            harness.run(|ui| {
                show(ui.ctx(), model);
            });
        }
    }

    /// One frame with `key` pressed: what the user did.
    fn press(harness: &Harness, model: &Model, key: Key) -> Vec<Action> {
        let mut done = Vec::new();
        harness.key(key, |ui| done = show(ui.ctx(), model));
        done
    }

    fn tree(harness: &Harness, model: &Model) -> Tree {
        harness.accessibility(|ui| {
            show(ui.ctx(), model);
        })
    }

    #[test]
    fn each_removal_asks_the_exact_question_and_says_what_stays() {
        let mut model = model();
        assert_eq!(confirm_view(&model), None, "nothing asked");

        asked(&mut model, Action::AskUnlink("shop".into()));
        let view = confirm_view(&model).expect("the unlink question");
        assert_eq!(view.title, "Unlink shop.test?");
        assert_eq!(view.confirm, "Unlink");
        // The folder is under the home folder: `~/…`, with the separator of
        // the system.
        let shown = crate::strings::home_path(&abs("/home/Code/shop"), model.home.as_deref());
        assert!(shown.starts_with('~'), "{shown}");
        assert!(
            view.body
                .ends_with(&format!("The folder {shown} and its files stay.")),
            "{}",
            view.body
        );
        model.cancel_removal();

        asked(&mut model, Action::AskForget(PathBuf::from("/code")));
        let view = confirm_view(&model).expect("the forget question");
        assert_eq!(view.title, "Forget /code?");
        assert_eq!(view.confirm, "Forget");
        // One site comes from the folder: the body counts it.
        assert!(
            view.body.starts_with("Marsa stops serving its site,"),
            "{}",
            view.body
        );
        assert!(view.body.ends_with("The folder and its files stay."));
        model.cancel_removal();

        model.update(Update::Runtimes(marsa_core::types::RuntimeList {
            runtimes: vec![marsa_core::types::RuntimeVersions {
                runtime: Runtime::Php,
                installed: vec!["8.3.27".into(), "8.4.27".into()],
                default: Some("8.4".into()),
            }],
        }));
        asked(
            &mut model,
            Action::AskUninstall {
                runtime: Runtime::Php,
                version: version(),
            },
        );
        let view = confirm_view(&model).expect("the uninstall question");
        assert_eq!(view.title, "Uninstall PHP 8.3.27?");
        assert_eq!(view.confirm, "Uninstall");
        assert!(view.body.contains("install PHP again"), "{}", view.body);
        // Three kinds, three ids.
        let ids = ["unlink", "forget", "uninstall"];
        assert!(ids.contains(&view.id));
    }

    #[test]
    fn a_removal_is_a_danger_dialog_over_the_window_with_a_title_and_two_buttons() {
        let mut model = model();
        asked(&mut model, Action::AskUnlink("shop".into()));
        let harness = Harness::new(egui::Theme::Light);
        settle(&harness, &model);
        let seen = tree(&harness, &model);
        // A screen reader hears an alert dialog named by the question.
        let dialog = seen
            .node_with_label("Unlink shop.test?")
            .expect("the dialog is named by its question");
        assert_eq!(dialog.role(), Role::AlertDialog);
        assert_eq!(
            seen.node_with_label("Cancel").map(|n| n.role()),
            Some(Role::Button)
        );
        assert_eq!(
            seen.node_with_label("Unlink").map(|n| n.role()),
            Some(Role::Button)
        );
        // Nothing is asked, nothing is drawn.
        let none = Model::default();
        let harness = Harness::new(egui::Theme::Light);
        settle(&harness, &none);
        assert!(tree(&harness, &none).node_with_label("Cancel").is_none());
    }

    #[test]
    fn escape_and_enter_cancel_and_only_tab_then_enter_removes() {
        let mut model = model();
        asked(&mut model, Action::AskUnlink("shop".into()));
        // Escape cancels, from the first frame on.
        let harness = Harness::new(egui::Theme::Light);
        settle(&harness, &model);
        assert_eq!(
            press(&harness, &model, Key::Escape),
            [Action::CancelRemoval]
        );
        // Enter alone cancels: Cancel has the focus when the dialog opens.
        let harness = Harness::new(egui::Theme::Light);
        settle(&harness, &model);
        assert_eq!(press(&harness, &model, Key::Enter), [Action::CancelRemoval]);
        // Tab moves to the danger button, and Enter on it confirms.
        let harness = Harness::new(egui::Theme::Light);
        settle(&harness, &model);
        assert!(press(&harness, &model, Key::Tab).is_empty());
        settle(&harness, &model);
        assert_eq!(
            press(&harness, &model, Key::Enter),
            [Action::ConfirmRemoval]
        );
    }

    #[test]
    fn the_answers_reach_the_model_and_a_confirmed_removal_becomes_a_command() {
        let mut model = model();
        asked(&mut model, Action::AskUnlink("shop".into()));
        let harness = Harness::new(egui::Theme::Light);
        settle(&harness, &model);
        for action in press(&harness, &model, Key::Escape) {
            assert!(model.apply_action(action).is_none());
        }
        assert!(model.session.confirm.is_none(), "cancelled");

        asked(&mut model, Action::AskUnlink("shop".into()));
        let command = model.apply_action(Action::ConfirmRemoval);
        assert!(
            matches!(
                command,
                Effect::Send(Command::Remove(Removal::Unlink { ref site })) if site == "shop"
            ),
            "{command:?}"
        );
    }

    #[test]
    fn a_click_beside_the_dialog_answers_nothing() {
        let mut model = model();
        asked(&mut model, Action::AskUnlink("shop".into()));
        let harness = Harness::new(egui::Theme::Light);
        settle(&harness, &model);
        let mut done = Vec::new();
        // The top left corner is over the scrim, far from the card.
        harness.click(egui::pos2(2.0, 2.0), |ui| done = show(ui.ctx(), &model));
        assert!(done.is_empty(), "{done:?}");
    }

    #[test]
    fn the_install_dialog_shows_when_no_removal_is_asked_and_gives_way_to_one() {
        let mut model = model();
        model.update(Update::Runtimes(marsa_core::types::RuntimeList {
            runtimes: vec![marsa_core::types::RuntimeVersions {
                runtime: Runtime::Php,
                installed: vec!["8.3.27".into()],
                default: Some("8.3".into()),
            }],
        }));
        model.apply_action(Action::AskInstall(Runtime::Php));
        let harness = Harness::new(egui::Theme::Light);
        settle(&harness, &model);
        assert!(
            tree(&harness, &model)
                .node_with_label("Install a PHP version")
                .is_some()
        );
        // A removal asked meanwhile: one dialog at a time.
        asked(&mut model, Action::AskUnlink("shop".into()));
        let harness = Harness::new(egui::Theme::Light);
        settle(&harness, &model);
        let seen = tree(&harness, &model);
        assert!(seen.node_with_label("Unlink shop.test?").is_some());
        assert!(seen.node_with_label("Install a PHP version").is_none());
    }
}
