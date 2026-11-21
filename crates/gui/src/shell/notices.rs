//! The notice bar of the shell (components.md, Notices): one bar at the
//! bottom of the main area for the result of what the user just did (the last
//! sites action, an install, an uninstall, a failed call). It is one bar for
//! the whole window, so a result shows on whatever screen the user is on.
//!
//! The results stay where they were, in the [`Model`]. [`current`] reads
//! them and decides which one the bar shows: pure, so the tests assert it
//! without a window. An error comes before a success, so a success waits
//! behind an error. A [`Clock`] knows how long the notice has been on screen:
//! a success closes after 4 seconds, an error stays until the user closes it.
//! Either way closing is an [`Action::DismissNotice`] that clears the result
//! in the model, so it does not come back. [Laravel: `current` is the flash
//! message the layout shows; closing it is forgetting the flash.]

use std::time::{Duration, Instant};

use eframe::egui::{Context, Id, Rect};

use crate::model::{Action, Model, NoticeSource, SitesNotice};
use crate::strings;
use crate::widgets::notice::{Notice, NoticeKind, Reaction, show_bar};

/// What the bar shows, and what it does for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bar {
    /// The result of the model that it tells about: closing it clears that.
    pub source: NoticeSource,
    pub notice: Notice,
    /// What the notice's action does: "Forget /code…" opens the
    /// confirmation. `notice.action` holds its label.
    pub fix: Option<Action>,
}

impl Bar {
    fn new(source: NoticeSource, kind: NoticeKind, text: String) -> Self {
        let notice = match kind {
            NoticeKind::Success => Notice::success(text),
            NoticeKind::Error => Notice::error(text),
        };
        Self {
            source,
            notice,
            fix: None,
        }
    }

    fn error(source: NoticeSource, text: String) -> Self {
        Self::new(source, NoticeKind::Error, text)
    }

    fn success(source: NoticeSource, text: String) -> Self {
        Self::new(source, NoticeKind::Success, text)
    }

    fn with_fix(mut self, label: &str, action: Action, off: Option<&str>) -> Self {
        self.notice = self.notice.with_action(label).with_action_off(off);
        self.fix = Some(action);
        self
    }
}

/// Decides what the bar shows now: the first error, else the first success,
/// of the results the model holds. Pure: the same model gives the same bar.
pub fn current(model: &Model) -> Option<Bar> {
    let mut bars: Vec<Bar> = [
        call(model),
        sites(model),
        install(model),
        uninstall(model),
        runtimes(model),
        autostart(model),
    ]
    .into_iter()
    .flatten()
    .collect();
    let first_error = bars
        .iter()
        .position(|bar| bar.notice.kind == NoticeKind::Error);
    let at = first_error.unwrap_or(0);
    (at < bars.len()).then(|| bars.swap_remove(at))
}

/// A call to the daemon failed: the code of the error.
fn call(model: &Model) -> Option<Bar> {
    let code = model.session.notices.code(NoticeSource::Call)?;
    Some(Bar::error(NoticeSource::Call, strings::error_text(code)))
}

/// The last sites action: what worked, or why not. A refused Unlink of a site
/// from a parked folder offers to forget the folder instead. Not while the
/// first-run panel is on screen.
fn sites(model: &Model) -> Option<Bar> {
    // The first-run panel says what the last park or link did in its own
    // step, and keeps saying it: the bar would say it twice.
    if model.first_run_shown() {
        return None;
    }
    let (source, home) = (NoticeSource::Sites, model.home.as_deref());
    Some(match model.session.notices.sites()? {
        SitesNotice::Failed(code) => Bar::error(source, strings::error_text(code)),
        SitesNotice::Parked(folder) => Bar::success(source, strings::parked(folder, home)),
        SitesNotice::Linked { name, folder } => {
            Bar::success(source, strings::linked(name, folder, home))
        }
        SitesNotice::Unlinked(site) => Bar::success(source, strings::unlinked(site)),
        SitesNotice::Forgot(folder) => Bar::success(source, strings::forgot(folder, home)),
        SitesNotice::SiteIsParked { site, folder } => {
            let text = strings::site_is_parked(site, folder.as_deref(), home);
            let bar = Bar::error(source, text);
            match folder {
                Some(folder) => {
                    let off = (!model.can_remove()).then_some(strings::REMOVE_BUSY_HINT);
                    let mut bar = bar.with_fix(
                        &strings::forget_folder(folder, home),
                        Action::AskForget(folder.clone()),
                        off,
                    );
                    // The label keeps the end of a long path: the tooltip
                    // has the whole one.
                    bar.notice = bar
                        .notice
                        .with_action_hint(strings::forget_folder_hint(folder));
                    bar
                }
                None => bar,
            }
        }
    })
}

/// How the install shown ended.
fn install(model: &Model) -> Option<Bar> {
    use crate::model::InstallStep;
    let view = model.install_result()?;
    let text = strings::install_status(view);
    Some(match view.step {
        InstallStep::Failed(_) => Bar::error(NoticeSource::Install, text),
        _ => Bar::success(NoticeSource::Install, text),
    })
}

/// The last uninstall, in one sentence. A refusal does not list what uses the
/// version: the Runtimes screen says it beside the version's menu.
fn uninstall(model: &Model) -> Option<Bar> {
    let line = strings::uninstall_notice(model.session.notices.uninstall()?);
    Some(if line.problem {
        Bar::error(NoticeSource::Uninstall, line.text)
    } else {
        Bar::success(NoticeSource::Uninstall, line.text)
    })
}

/// The runtimes could not be read, or a default or a version was not
/// accepted. While the install dialog is open the error is its field's, under
/// the field.
fn runtimes(model: &Model) -> Option<Bar> {
    if model.session.install_dialog.is_some() {
        return None;
    }
    let code = model.session.notices.code(NoticeSource::Runtimes)?;
    Some(Bar::error(
        NoticeSource::Runtimes,
        strings::error_text(code),
    ))
}

/// Start at login could not be read or changed.
fn autostart(model: &Model) -> Option<Bar> {
    let code = model.session.notices.code(NoticeSource::Autostart)?;
    Some(Bar::error(
        NoticeSource::Autostart,
        strings::error_text(code),
    ))
}

/// What the clock says about the notice on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    /// No notice.
    Nothing,
    /// The notice stays. A success has this long to go; an error has no end.
    Show { closes_in: Option<Duration> },
    /// A success has been there for 4 seconds: close it.
    Expired,
}

/// How long the notice on screen has been there. It takes the time from the
/// caller, so a test moves the clock by hand.
#[derive(Clone, Debug, Default)]
pub struct Clock {
    /// The notice, and when it came on screen.
    shown: Option<(Bar, Instant)>,
}

impl Clock {
    /// Looks at the notice the model wants on screen at `now`. A different
    /// notice starts its own time: the next one after a close does not
    /// inherit the one before it.
    pub fn look(&mut self, wanted: Option<&Bar>, now: Instant) -> Look {
        let Some(wanted) = wanted else {
            self.shown = None;
            return Look::Nothing;
        };
        let since = match &self.shown {
            Some((bar, since)) if bar == wanted => *since,
            _ => {
                self.shown = Some((wanted.clone(), now));
                now
            }
        };
        let Some(lifetime) = wanted.notice.kind.lifetime() else {
            return Look::Show { closes_in: None };
        };
        let shown_for = now.saturating_duration_since(since);
        if shown_for >= lifetime {
            Look::Expired
        } else {
            Look::Show {
                closes_in: Some(lifetime - shown_for),
            }
        }
    }
}

/// Draws the bar over the bottom of `main_area` and returns what the user
/// did: close it, or press its action. A success that has run out closes
/// itself, and the window asks for one repaint when the next one will.
pub fn show(ctx: &Context, main_area: Rect, model: &Model, now: Instant) -> Vec<Action> {
    let bar = current(model);
    let look = ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Clock>(Id::unique("notice_clock"))
            .look(bar.as_ref(), now)
    });
    let Some(bar) = bar else {
        return Vec::new();
    };
    match look {
        Look::Nothing => Vec::new(),
        // The clock's own repaint: the model has not changed, and no input
        // comes, so nothing else would draw the frame that closes it.
        Look::Expired => {
            ctx.request_repaint();
            vec![Action::DismissNotice(bar.source)]
        }
        Look::Show { closes_in } => {
            if let Some(wait) = closes_in {
                ctx.request_repaint_after(wait);
            }
            match show_bar(ctx, main_area, &bar.notice) {
                Some(Reaction::Closed) => vec![Action::DismissNotice(bar.source)],
                // The notice stays: the confirmation it opens may be
                // cancelled, and the result is still true.
                Some(Reaction::Action) => bar.fix.into_iter().collect(),
                None => Vec::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use eframe::egui;
    use marsa_core::runtimes::Runtime;
    use marsa_core::types::{Event, InstallDone, InstallProgress, InstallUpdate};
    use marsa_core::types::{Health, SiteList};
    use semver::Version;

    use super::*;
    use crate::model::{InstallJob, Route, UninstallNotice, Update, apply};
    use crate::theme::LIGHT;
    use crate::widgets::NoticeBar;
    use crate::widgets::harness::Harness;

    fn connected() -> Model {
        let mut model = Model::default();
        model.update(Update::Connected(Health {
            app_version: "0.0.0".into(),
            api_version: 1,
            pid: 7,
        }));
        model
    }

    fn seconds(count: u64) -> Duration {
        Duration::from_secs(count)
    }

    /// The text and kind of the bar.
    fn says(model: &Model) -> Option<(NoticeKind, String)> {
        current(model).map(|bar| (bar.notice.kind, bar.notice.text))
    }

    #[test]
    fn no_result_no_bar() {
        assert_eq!(current(&connected()), None);
        assert_eq!(current(&Model::default()), None);
    }

    #[test]
    fn each_result_the_lines_had_is_a_notice_with_the_same_words() {
        let mut model = connected();
        model
            .session
            .notices
            .set_code(NoticeSource::Call, "service_busy".into());
        assert_eq!(
            says(&model),
            Some((NoticeKind::Error, strings::error_text("service_busy")))
        );
        model.session.notices.clear(NoticeSource::Call);

        let folder = PathBuf::from("/code");
        for (notice, kind, text) in [
            (
                SitesNotice::Unlinked("shop".into()),
                NoticeKind::Success,
                strings::unlinked("shop"),
            ),
            (
                SitesNotice::Parked(folder.clone()),
                NoticeKind::Success,
                strings::parked(&folder, None),
            ),
            (
                SitesNotice::Linked {
                    name: "shop".into(),
                    folder: folder.clone(),
                },
                NoticeKind::Success,
                strings::linked("shop", &folder, None),
            ),
            (
                SitesNotice::Forgot(folder.clone()),
                NoticeKind::Success,
                strings::forgot(&folder, None),
            ),
            (
                SitesNotice::Failed("site_name_invalid".into()),
                NoticeKind::Error,
                strings::error_text("site_name_invalid"),
            ),
        ] {
            model.session.notices.set_sites(notice);
            assert_eq!(says(&model), Some((kind, text)));
            assert_eq!(current(&model).and_then(|bar| bar.fix), None);
        }
        model.session.notices.clear(NoticeSource::Sites);

        let version: Version = "8.3.27".parse().expect("a version");
        model.session.notices.set_uninstall(UninstallNotice::Done {
            runtime: Runtime::Php,
            version: version.clone(),
        });
        assert_eq!(
            says(&model),
            Some((NoticeKind::Success, "Uninstalled PHP 8.3.27.".to_owned()))
        );
        model
            .session
            .notices
            .set_uninstall(UninstallNotice::Failed {
                runtime: Runtime::Php,
                version,
                code: "uninstall_failed".into(),
            });
        let (kind, text) = says(&model).expect("a notice");
        assert_eq!(kind, NoticeKind::Error);
        assert!(text.starts_with("Cannot uninstall PHP 8.3.27"), "{text}");
        model.session.notices.clear(NoticeSource::Uninstall);

        model
            .session
            .notices
            .set_code(NoticeSource::Runtimes, "runtimes_read".into());
        assert_eq!(
            says(&model),
            Some((NoticeKind::Error, strings::error_text("runtimes_read")))
        );
        // While the install dialog is open the error is its field's.
        model.update(Update::Runtimes(marsa_core::types::RuntimeList {
            runtimes: vec![marsa_core::types::RuntimeVersions {
                runtime: Runtime::Php,
                installed: vec!["8.4.27".into()],
                default: Some("8.4".into()),
            }],
        }));
        model
            .session
            .notices
            .set_code(NoticeSource::Runtimes, "invalid_version".into());
        model.session.install_dialog = Some(Runtime::Php);
        assert_eq!(says(&model), None);
        model.session.install_dialog = None;
        model.session.notices.clear(NoticeSource::Runtimes);

        model
            .session
            .notices
            .set_code(NoticeSource::Autostart, "autostart_write".into());
        assert_eq!(
            says(&model),
            Some((NoticeKind::Error, strings::error_text("autostart_write")))
        );
    }

    #[test]
    fn a_refused_unlink_offers_to_forget_the_folder_and_the_offer_waits_for_a_busy_daemon() {
        let mut model = connected();
        let folder = PathBuf::from("/code");
        model.session.notices.set_sites(SitesNotice::SiteIsParked {
            site: "shop".into(),
            folder: Some(folder.clone()),
        });
        let bar = current(&model).expect("a notice");
        assert_eq!(bar.notice.kind, NoticeKind::Error);
        assert_eq!(
            bar.notice.text,
            strings::site_is_parked("shop", Some(&folder), None)
        );
        assert_eq!(bar.notice.action.as_deref(), Some("Forget /code…"));
        assert_eq!(bar.notice.action_off, None);
        assert_eq!(bar.fix, Some(Action::AskForget(folder.clone())));
        assert_eq!(bar.source, NoticeSource::Sites);

        // Another removal runs: the offer is there and off, with its reason.
        model.update(Update::Sites(SiteList {
            sites: Vec::new(),
            parked: vec![folder.clone()],
            ..SiteList::default()
        }));
        model.apply_action(Action::AskForget(folder.clone()));
        assert!(model.confirm_removal().is_some());
        // Confirming cleared the old result: the refusal comes after it.
        model.session.notices.set_sites(SitesNotice::SiteIsParked {
            site: "shop".into(),
            folder: Some(folder.clone()),
        });
        let bar = current(&model).expect("a notice");
        assert_eq!(
            bar.notice.action_off.as_deref(),
            Some(strings::REMOVE_BUSY_HINT)
        );

        // Without the folder there is nothing to offer.
        model.session.notices.set_sites(SitesNotice::SiteIsParked {
            site: "shop".into(),
            folder: None,
        });
        let bar = current(&model).expect("a notice");
        assert_eq!((bar.notice.action, bar.fix), (None, None));
    }

    #[test]
    fn the_first_run_panel_keeps_the_last_sites_result_and_the_bar_does_not_say_it_twice() {
        let mut model = connected();
        model.update(Update::Runtimes(marsa_core::types::RuntimeList {
            runtimes: vec![
                marsa_core::types::RuntimeVersions {
                    runtime: Runtime::Php,
                    installed: vec!["8.4.27".into()],
                    default: Some("8.4".into()),
                },
                marsa_core::types::RuntimeVersions {
                    runtime: Runtime::Caddy,
                    installed: vec!["2.11.7".into()],
                    default: None,
                },
            ],
        }));
        model.update(Update::SitesFailed("site_path_relative".into()));
        assert!(model.first_run_shown());
        assert_eq!(current(&model), None, "the step says it");
        // Anywhere else the bar says it.
        model.route = Route::Sites;
        assert!(!model.first_run_shown());
        assert_eq!(
            says(&model),
            Some((NoticeKind::Error, strings::error_text("site_path_relative")))
        );
        // Other results are not the step's.
        model.route = Route::Home;
        model
            .session
            .notices
            .set_code(NoticeSource::Call, "service_busy".into());
        assert_eq!(current(&model).map(|b| b.source), Some(NoticeSource::Call));
    }

    /// An absolute path on any system: a drive on Windows, where a path that
    /// starts with `/` is not absolute and `~` is never used.
    fn abs(unix: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{}", unix.replace('/', "\\")))
        } else {
            PathBuf::from(unix)
        }
    }

    /// How many lines each notice takes in a bar of the widest size (a
    /// window 1120 wide has a 560 bar), measured as the bar lays it out.
    fn lines_of(bars: &[(String, Bar)]) -> Vec<(usize, String)> {
        let harness = Harness::new(egui::Theme::Light);
        let mut counts = Vec::new();
        harness.run(|ui| {
            for (name, bar) in bars {
                let width = LIGHT.size.notice_max;
                let lines = NoticeBar::new(&bar.notice).lines(ui, width);
                counts.push((lines, format!("{name}: {}", bar.notice.text)));
            }
        });
        counts
    }

    #[test]
    fn the_refused_unlink_is_one_short_sentence_with_the_folder_under_a_tilde_and_fits_two_lines() {
        let mut model = connected();
        model.home = Some(abs("/Users/omar"));
        let folder = abs("/Users/omar/Code");
        model.session.notices.set_sites(SitesNotice::SiteIsParked {
            site: "shop".into(),
            folder: Some(folder.clone()),
        });
        let bar = current(&model).expect("a notice");
        let tilde = strings::home_path(&folder, model.home.as_deref());
        assert!(tilde.starts_with('~'), "{tilde}");
        assert_eq!(
            bar.notice.text,
            format!("shop.test is part of the parked folder {tilde}.")
        );
        assert_eq!(
            bar.notice.action.as_deref(),
            Some(format!("Forget {tilde}…").as_str())
        );
        // The action still asks about the whole folder, not the shortened text.
        assert_eq!(bar.fix, Some(Action::AskForget(folder)));
        let lines = lines_of(&[("refused unlink".to_owned(), bar)]);
        assert!(lines[0].0 <= 2, "{lines:?}");
    }

    #[test]
    fn the_forget_action_of_a_long_folder_shows_its_end_and_the_tooltip_has_the_whole_path() {
        let mut model = connected();
        model.home = Some(abs("/Users/omar"));
        let folder = abs("/Users/omar/Code/clients/acme/projects");
        model.session.notices.set_sites(SitesNotice::SiteIsParked {
            site: "shop".into(),
            folder: Some(folder.clone()),
        });
        let bar = current(&model).expect("a notice");
        // The sentence names no long path; the button keeps the last parts.
        assert_eq!(bar.notice.text, "shop.test is part of a parked folder.");
        let sep = std::path::MAIN_SEPARATOR;
        let label = format!("Forget …{sep}clients{sep}acme{sep}projects…");
        assert_eq!(bar.notice.action.as_deref(), Some(label.as_str()));
        // The tooltip has the whole path, not the shortened one and not `~`.
        let hint = bar.notice.action_hint.as_deref().expect("a tooltip");
        assert!(hint.contains(&folder.display().to_string()), "{hint}");
        assert!(!hint.contains('~') && !hint.contains('…'), "{hint}");
        assert_eq!(bar.fix, Some(Action::AskForget(folder)));
        // Over the button, a screen and a pointer both get that text.
        let start = Instant::now();
        let harness = Harness::new(egui::Theme::Light);
        let area = Rect::from_min_size(egui::pos2(224.0, 0.0), egui::vec2(896.0, 720.0));
        // The bar sizes itself over a few frames.
        for _ in 0..20 {
            harness.run(|ui| {
                show(ui.ctx(), area, &model, start);
            });
        }
        let tree = harness.accessibility(|ui| {
            show(ui.ctx(), area, &model, start);
        });
        let bounds = tree
            .node_with_label(&label)
            .and_then(|node| node.bounds())
            .expect("the action");
        let over = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        // No wait before a tooltip: the test has no clock.
        harness
            .ctx
            .all_styles_mut(|style| style.interaction.tooltip_delay = 0.0);
        let mut tip = None;
        for _ in 0..30 {
            harness.hover(over, |ui| {
                show(ui.ctx(), area, &model, start);
            });
            let tree = harness.accessibility(|ui| {
                show(ui.ctx(), area, &model, start);
            });
            if tree.node_with_text(hint).is_some() {
                tip = Some(hint);
                break;
            }
        }
        assert_eq!(tip, Some(hint), "the tooltip shows over the action");
    }

    /// Every result the sites actions say, with a long folder under home.
    fn sites_bars() -> Vec<(String, Bar)> {
        let mut model = connected();
        model.home = Some(abs("/Users/omar"));
        let folder = abs("/Users/omar/Code/clients/acme/projects");
        let notices = [
            SitesNotice::Parked(folder.clone()),
            SitesNotice::Linked {
                name: "acme-portal".into(),
                folder: folder.clone(),
            },
            SitesNotice::Unlinked("acme-portal".into()),
            SitesNotice::Forgot(folder.clone()),
            SitesNotice::SiteIsParked {
                site: "acme-portal".into(),
                folder: Some(folder),
            },
            SitesNotice::SiteIsParked {
                site: "acme-portal".into(),
                folder: None,
            },
        ];
        notices
            .into_iter()
            .map(|notice| {
                model.session.notices.set_sites(notice.clone());
                let bar = current(&model).expect("a notice");
                (format!("{notice:?}"), bar)
            })
            .collect()
    }

    /// The codes `strings::error_text` has a sentence for, read from its
    /// source: a code is a quoted name before `|` or `=>` in `error_message`.
    fn error_codes() -> Vec<String> {
        let source = include_str!("../strings.rs");
        let start = source
            .find("fn error_message(code: &str)")
            .expect("the table of error texts");
        let body = &source[start..];
        let body = &body[..body.find("\n}\n").expect("its end")];
        let mut codes = Vec::new();
        let mut rest = body;
        while let Some(open) = rest.find('"') {
            let after = &rest[open + 1..];
            let mut end = 0;
            let bytes = after.as_bytes();
            while end < bytes.len() && bytes[end] != b'"' {
                end += if bytes[end] == b'\\' { 2 } else { 1 };
            }
            let literal = &after[..end.min(after.len())];
            rest = &after[(end + 1).min(after.len())..];
            let next = rest.trim_start();
            let name = !literal.is_empty()
                && literal
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if name && (next.starts_with('|') || next.starts_with("=>")) {
                codes.push(literal.to_owned());
            }
        }
        codes
    }

    #[test]
    fn the_codes_of_the_error_table_are_read_from_its_source() {
        let codes = error_codes();
        assert!(codes.len() > 100, "{}", codes.len());
        for code in ["daemon_not_running", "site_is_parked", "install_os_too_old"] {
            assert!(codes.iter().any(|c| c == code), "{code}");
        }
        // Texts are not codes.
        assert!(!codes.iter().any(|c| c.contains(' ')));
    }

    #[test]
    fn no_notice_takes_more_than_two_lines_in_the_widest_bar() {
        let mut bars = sites_bars();
        // A failed call, a failed sites action, a failed read, a failed
        // Start at login: any error code, with its close button.
        // Not the codes only a setup says: Settings and the first-run panel
        // show those in full, in their own report.
        let setup = [
            "setup_",
            "hosts_",
            "resolved_",
            "resolver_",
            "trust_",
            "pf_",
            "ports_",
            "redirect_",
            "ca_",
        ];
        let codes: Vec<String> = error_codes()
            .into_iter()
            .filter(|code| !setup.iter().any(|start| code.starts_with(start)))
            .collect();
        assert!(codes.len() > 90, "{}", codes.len());
        for code in &codes {
            bars.push((
                code.clone(),
                Bar::error(NoticeSource::Call, strings::error_text(code)),
            ));
        }
        // An install that failed, with the facts the daemon sends.
        for code in codes {
            let failure = marsa_core::types::InstallFailure {
                code: code.clone(),
                requested: Some("8.4".into()),
                available: (1..=9).map(|n| format!("8.{n}.0")).collect(),
                output: Some("dyld: Library not loaded: @rpath/libxml2.2.dylib".into()),
                os: Some(Box::new(marsa_core::types::OsMismatch {
                    needs: "macOS 15.0".into(),
                    found: "macOS 14.6".into(),
                })),
                ..marsa_core::types::InstallFailure::default()
            };
            let view = crate::model::InstallView {
                runtime: Some(Runtime::Php),
                version: Some("8.4.27".into()),
                step: crate::model::InstallStep::Failed(failure),
            };
            if code.starts_with("install_")
                || ["download_", "checksum_", "manifest_", "extract_", "recipe_"]
                    .iter()
                    .any(|start| code.starts_with(start))
            {
                bars.push((
                    format!("install {code}"),
                    Bar::error(NoticeSource::Install, strings::install_status(&view)),
                ));
            }
        }
        // An uninstall: every kind.
        let version: Version = "8.3.27".parse().expect("a version");
        let in_use = marsa_core::types::VersionInUse {
            version: "8.3.27".into(),
            default: Some("8.3".into()),
            sites: vec!["shop".into(), "blog".into(), "docs".into()],
            caddy: true,
            ..marsa_core::types::VersionInUse::default()
        };
        for notice in [
            UninstallNotice::Done {
                runtime: Runtime::Php,
                version: version.clone(),
            },
            UninstallNotice::InUse {
                runtime: Runtime::Php,
                in_use,
            },
            UninstallNotice::Failed {
                runtime: Runtime::Php,
                version,
                code: "uninstall_failed".into(),
            },
        ] {
            let line = strings::uninstall_notice(&notice);
            let bar = if line.problem {
                Bar::error(NoticeSource::Uninstall, line.text)
            } else {
                Bar::success(NoticeSource::Uninstall, line.text)
            };
            bars.push((format!("{notice:?}"), bar));
        }
        let wrapping: Vec<_> = lines_of(&bars)
            .into_iter()
            .filter(|(lines, _)| *lines > 2)
            .collect();
        assert!(wrapping.is_empty(), "{wrapping:#?}");
    }

    /// An install of PHP that ended with `update`.
    fn installed(model: &mut Model, id: u64, update: InstallUpdate) {
        let job = InstallJob {
            runtime: Runtime::Php,
            version: Some("8.4".into()),
        };
        assert!(model.queue_install(job));
        let job = model.next_install().expect("sent");
        model.update(Update::InstallAccepted { job, id });
        apply(model, Event::Install(InstallProgress { id, update }));
    }

    #[test]
    fn an_install_that_ended_is_a_notice_until_it_is_closed() {
        let mut model = connected();
        let done = InstallDone {
            runtime: Runtime::Php,
            version: "8.4.27".into(),
            dir: PathBuf::from("/data/runtimes/php/8.4.27"),
            already: false,
        };
        installed(&mut model, 1, InstallUpdate::Done(done));
        assert_eq!(
            says(&model),
            Some((NoticeKind::Success, "Installed PHP 8.4.27.".to_owned()))
        );
        let source = current(&model).expect("a notice").source;
        assert_eq!(source, NoticeSource::Install);
        model.apply_action(Action::DismissNotice(source));
        assert_eq!(current(&model), None, "it does not come back");
        // The next install is a new notice.
        installed(
            &mut model,
            2,
            InstallUpdate::Failed(marsa_core::types::InstallFailure {
                code: "install_download".into(),
                ..marsa_core::types::InstallFailure::default()
            }),
        );
        let (kind, text) = says(&model).expect("a notice");
        assert_eq!(kind, NoticeKind::Error);
        assert!(text.starts_with("Cannot install PHP"), "{text}");
    }

    #[test]
    fn an_error_comes_before_a_success_and_a_closed_one_lets_the_next_show() {
        let mut model = connected();
        let source = |model: &Model| current(model).map(|bar| bar.source);
        model
            .session
            .notices
            .set_sites(SitesNotice::Unlinked("shop".into()));
        model.session.notices.set_uninstall(UninstallNotice::Done {
            runtime: Runtime::Php,
            version: "8.3.27".parse().expect("a version"),
        });
        // Two successes: the sites one first; the other waits behind it.
        assert_eq!(source(&model), Some(NoticeSource::Sites));
        // An error that comes meanwhile takes the bar: a success waits.
        model
            .session
            .notices
            .set_code(NoticeSource::Call, "service_busy".into());
        assert_eq!(source(&model), Some(NoticeSource::Call));
        // Closing the error brings the success back, with its result intact.
        model.apply_action(Action::DismissNotice(NoticeSource::Call));
        assert_eq!(source(&model), Some(NoticeSource::Sites));
        model.apply_action(Action::DismissNotice(NoticeSource::Sites));
        assert_eq!(source(&model), Some(NoticeSource::Uninstall));
        model.apply_action(Action::DismissNotice(NoticeSource::Uninstall));
        assert_eq!(source(&model), None);
    }

    #[test]
    fn a_success_closes_after_four_seconds_and_an_error_stays() {
        let start = Instant::now();
        let mut model = connected();
        let mut clock = Clock::default();
        assert_eq!(clock.look(current(&model).as_ref(), start), Look::Nothing);

        model
            .session
            .notices
            .set_sites(SitesNotice::Unlinked("shop".into()));
        let wanted = current(&model);
        assert_eq!(
            clock.look(wanted.as_ref(), start),
            Look::Show {
                closes_in: Some(seconds(4))
            }
        );
        assert_eq!(
            clock.look(wanted.as_ref(), start + seconds(3)),
            Look::Show {
                closes_in: Some(seconds(1))
            }
        );
        assert_eq!(
            clock.look(wanted.as_ref(), start + seconds(4)),
            Look::Expired,
            "closed at 4 seconds"
        );

        // An error is still there after an hour, and has no end to wait for.
        model
            .session
            .notices
            .set_sites(SitesNotice::Failed("site_name_invalid".into()));
        let wanted = current(&model);
        let later = start + seconds(10);
        assert_eq!(
            clock.look(wanted.as_ref(), later),
            Look::Show { closes_in: None }
        );
        assert_eq!(
            clock.look(wanted.as_ref(), later + seconds(3600)),
            Look::Show { closes_in: None }
        );
    }

    #[test]
    fn the_next_notice_starts_its_own_four_seconds() {
        let start = Instant::now();
        let mut model = connected();
        let mut clock = Clock::default();
        model
            .session
            .notices
            .set_sites(SitesNotice::Unlinked("shop".into()));
        model.session.notices.set_uninstall(UninstallNotice::Done {
            runtime: Runtime::Php,
            version: "8.3.27".parse().expect("a version"),
        });
        let first = current(&model);
        assert_eq!(
            clock.look(first.as_ref(), start + seconds(4)),
            Look::Show {
                closes_in: Some(seconds(4))
            },
            "the clock starts when the notice is first seen"
        );
        assert_eq!(
            clock.look(first.as_ref(), start + seconds(8)),
            Look::Expired
        );
        // It closed: the next one shows, and has its whole time.
        model.apply_action(Action::DismissNotice(NoticeSource::Sites));
        let second = current(&model);
        assert_eq!(
            clock.look(second.as_ref(), start + seconds(8)),
            Look::Show {
                closes_in: Some(seconds(4))
            }
        );
        // An error replaces a success on screen, with no time to run out.
        model
            .session
            .notices
            .set_code(NoticeSource::Call, "service_busy".into());
        let error = current(&model);
        assert_eq!(
            clock.look(error.as_ref(), start + seconds(9)),
            Look::Show { closes_in: None }
        );
    }

    /// The tree of the bar over a 1120 by 720 window, after the frames it
    /// needs to size itself.
    fn frames(model: &Model, harness: &Harness, now: Instant) -> Vec<Action> {
        let area = Rect::from_min_size(egui::pos2(224.0, 0.0), egui::vec2(896.0, 720.0));
        let mut done = Vec::new();
        for _ in 0..20 {
            harness.run(|ui| done = show(ui.ctx(), area, model, now));
        }
        done
    }

    #[test]
    fn the_shell_draws_the_bar_and_a_success_closes_it_by_the_clock() {
        let start = Instant::now();
        let mut model = connected();
        model
            .session
            .notices
            .set_sites(SitesNotice::Unlinked("shop".into()));
        let harness = Harness::new(egui::Theme::Light);
        let text = strings::unlinked("shop");
        // Drawn, with no action asked yet.
        assert!(frames(&model, &harness, start).is_empty());
        let area = Rect::from_min_size(egui::pos2(224.0, 0.0), egui::vec2(896.0, 720.0));
        let tree = harness.accessibility(|ui| {
            show(ui.ctx(), area, &model, start);
        });
        let node = tree.node_with_label(&text).expect("the bar");
        assert_eq!(node.role(), egui::Role::Status);
        assert!(tree.node_with_label(strings::CLOSE).is_none(), "no close");
        // Four seconds later the shell asks to dismiss it, and the model
        // clears it.
        let actions = frames(&model, &harness, start + seconds(4));
        assert_eq!(actions, [Action::DismissNotice(NoticeSource::Sites)]);
        for action in actions {
            model.apply_action(action);
        }
        assert!(model.session.notices.sites().is_none());
        let actions = frames(&model, &harness, start + seconds(5));
        assert!(actions.is_empty());
    }

    #[test]
    fn the_shell_keeps_an_error_with_its_close_button_and_its_action() {
        let start = Instant::now();
        let mut model = connected();
        let folder = PathBuf::from("/code");
        model.session.notices.set_sites(SitesNotice::SiteIsParked {
            site: "shop".into(),
            folder: Some(folder.clone()),
        });
        let harness = Harness::new(egui::Theme::Light);
        // An hour later: still there, nothing asked.
        assert!(frames(&model, &harness, start).is_empty());
        assert!(frames(&model, &harness, start + seconds(3600)).is_empty());
        let area = Rect::from_min_size(egui::pos2(224.0, 0.0), egui::vec2(896.0, 720.0));
        let now = start + seconds(3600);
        let tree = harness.accessibility(|ui| {
            show(ui.ctx(), area, &model, now);
        });
        let alert = tree
            .node_with_text(&strings::site_is_parked("shop", Some(&folder), None))
            .expect("the bar");
        assert_eq!(alert.role(), egui::Role::Alert);
        let middle = |label: &str| {
            let bounds = tree
                .node_with_label(label)
                .and_then(|node| node.bounds())
                .expect(label);
            egui::pos2(
                ((bounds.x0 + bounds.x1) / 2.0) as f32,
                ((bounds.y0 + bounds.y1) / 2.0) as f32,
            )
        };
        // The action opens the confirmation to forget the folder.
        let mut done = Vec::new();
        harness.click(middle("Forget /code…"), |ui| {
            done.extend(show(ui.ctx(), area, &model, now));
        });
        assert_eq!(done, [Action::AskForget(folder)]);
        // The close button dismisses the result.
        let mut done = Vec::new();
        harness.click(middle(strings::CLOSE), |ui| {
            done.extend(show(ui.ctx(), area, &model, now));
        });
        assert_eq!(done, [Action::DismissNotice(NoticeSource::Sites)]);
    }
}
