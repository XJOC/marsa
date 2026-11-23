//! Removals: uninstall, unlink, forget, and deleting the caught mail, each
//! after a confirmation.

use super::*;

/// Something the window removes once the user confirms it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// `DELETE /v1/runtimes/{tool}/{version}`, with the full version.
    Uninstall { runtime: Runtime, version: Version },
    /// `DELETE /v1/sites/{name}`: a linked site.
    Unlink { site: String },
    /// `DELETE /v1/sites/park`: a parked folder, as the list names it.
    Forget { folder: PathBuf },
    /// `DELETE /v1/mail`: every mail Mailpit caught.
    Mail,
}

impl Removal {
    /// The title of the confirmation: the exact question. A folder under
    /// `home` shows as `~/…`.
    pub fn title(&self, home: Option<&Path>) -> String {
        match self {
            Removal::Uninstall { runtime, version } => strings::uninstall_title(*runtime, version),
            Removal::Unlink { site } => strings::unlink_title(site),
            Removal::Forget { folder } => strings::forget_title(folder, home),
            Removal::Mail => strings::DELETE_MAIL_TITLE.to_owned(),
        }
    }

    /// What the confirmed removal does, and what stays. `site_root`: the
    /// folder of the site to unlink; `sites`: how many sites the folder to
    /// forget gives now.
    pub fn text(&self, site_root: Option<&Path>, sites: usize, home: Option<&Path>) -> String {
        match self {
            Removal::Uninstall { runtime, .. } => strings::uninstall_text(*runtime),
            Removal::Unlink { .. } => strings::unlink_text(site_root, home),
            Removal::Forget { .. } => strings::forget_text(sites),
            Removal::Mail => strings::DELETE_MAIL_TEXT.to_owned(),
        }
    }

    /// The button that confirms.
    pub const fn button(&self) -> &'static str {
        match self {
            Removal::Uninstall { .. } => strings::UNINSTALL_CONFIRM,
            Removal::Unlink { .. } => strings::UNLINK_CONFIRM,
            Removal::Forget { .. } => strings::FORGET_CONFIRM,
            Removal::Mail => strings::DELETE_MAIL_CONFIRM,
        }
    }

    /// While the daemon removes it.
    pub fn progress(&self) -> String {
        match self {
            Removal::Uninstall { runtime, version } => strings::uninstalling(*runtime, version),
            Removal::Unlink { site } => strings::unlinking(site),
            Removal::Forget { folder } => strings::forgetting(folder),
            Removal::Mail => strings::DELETING_MAIL.to_owned(),
        }
    }
}

/// The lines under the Runtimes table about the last uninstall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UninstallNotice {
    /// The daemon deleted this version.
    Done { runtime: Runtime, version: Version },
    /// The daemon refused: the default, a site, or Caddy uses the version.
    InUse {
        runtime: Runtime,
        in_use: VersionInUse,
    },
    /// The daemon refused or failed for another reason. A client error code.
    Failed {
        runtime: Runtime,
        version: Version,
        code: String,
    },
}

impl UninstallNotice {
    /// The lines under the Runtimes table about the last uninstall. A refusal
    /// for `version_in_use` or `sites_lose_php` names each thing that uses
    /// the version, and what to do about it.
    pub fn lines(&self) -> Vec<ReportLine> {
        let problem = |text: String| ReportLine {
            text,
            problem: true,
        };
        match self {
            UninstallNotice::Done { runtime, version } => vec![ReportLine {
                text: strings::uninstalled(*runtime, version),
                problem: false,
            }],
            UninstallNotice::Failed {
                runtime,
                version,
                code,
            } => vec![problem(strings::uninstall_failed(*runtime, version, code))],
            UninstallNotice::InUse { runtime, in_use } => {
                let mut lines = vec![problem(strings::uninstall_in_use_with_reasons(
                    *runtime,
                    &in_use.version,
                ))];
                lines.extend(
                    strings::in_use_reasons(*runtime, in_use)
                        .into_iter()
                        .map(problem),
                );
                lines
            }
        }
    }

    /// The notice about the last uninstall: one sentence. A refusal for a
    /// version that is in use does not list what uses it: the Runtimes screen
    /// says that beside the version's menu ([`UninstallNotice::lines`] has the
    /// whole list).
    pub fn line(&self) -> ReportLine {
        let (text, problem) = match self {
            UninstallNotice::Done { runtime, version } => {
                (strings::uninstalled(*runtime, version), false)
            }
            UninstallNotice::Failed {
                runtime,
                version,
                code,
            } => (strings::uninstall_failed(*runtime, version, code), true),
            UninstallNotice::InUse { runtime, in_use } => {
                (strings::uninstall_in_use(*runtime, &in_use.version), true)
            }
        };
        ReportLine { text, problem }
    }
}

impl Model {
    /// Whether a removal can be asked for now: connected, and no other
    /// removal waits for its answer.
    pub fn can_remove(&self) -> bool {
        self.is_connected() && self.session.removing.is_none()
    }

    /// [`Model::can_remove`], and no job of this window installs: the daemon
    /// runs one install or uninstall at a time.
    pub fn can_uninstall(&self) -> bool {
        self.can_remove() && !self.installing()
    }

    /// The removal sent and not answered yet.
    pub fn removing(&self) -> Option<&Removal> {
        self.session.removing.as_ref()
    }

    /// Whether an uninstall waits for its answer. The install buttons wait
    /// for it.
    pub fn uninstalling(&self) -> bool {
        matches!(self.session.removing, Some(Removal::Uninstall { .. }))
    }

    pub(super) fn may_remove(&self, removal: &Removal) -> bool {
        match removal {
            Removal::Uninstall { .. } => self.can_uninstall(),
            Removal::Unlink { .. } | Removal::Forget { .. } | Removal::Mail => self.can_remove(),
        }
    }

    /// The user pressed Uninstall, Unlink, or Forget: the confirmation
    /// opens. `false` when it cannot be removed now.
    pub fn ask_removal(&mut self, removal: Removal) -> bool {
        if !self.may_remove(&removal) {
            return false;
        }
        self.session.confirm = Some(removal);
        true
    }

    /// "Cancel", Escape, or a click beside the confirmation.
    pub fn cancel_removal(&mut self) {
        self.session.confirm = None;
    }

    /// The user confirmed: the removal to send, or `None` if it cannot be
    /// sent now. The last notice of its section goes: it was about the last
    /// action.
    pub fn confirm_removal(&mut self) -> Option<Removal> {
        let removal = self.session.confirm.take()?;
        if !self.may_remove(&removal) {
            return None;
        }
        match removal {
            Removal::Uninstall { .. } => {
                self.session.notices.clear(NoticeSource::Uninstall);
                self.session.notices.clear(NoticeSource::Runtimes);
            }
            Removal::Unlink { .. } | Removal::Forget { .. } => {
                self.session.notices.clear(NoticeSource::Sites)
            }
            Removal::Mail => self.session.notices.clear(NoticeSource::Call),
        }
        self.session.removing = Some(removal.clone());
        Some(removal)
    }

    /// Whether `removal` is the one waiting for its answer, which is then no
    /// longer waited for. An answer from a lost session finds none.
    pub(super) fn settle_removal(&mut self, removal: &Removal) -> bool {
        let ours = self.session.removing.as_ref() == Some(removal);
        if ours {
            self.session.removing = None;
        }
        ours
    }

    pub(super) fn removed(&mut self, removal: Removal) {
        if !self.settle_removal(&removal) {
            return;
        }
        match removal {
            Removal::Uninstall { runtime, version } => {
                // The bridge reads the list again; until it comes, the
                // version is gone from the table already. The last version
                // took the default that picked it, as in the daemon.
                if let Some(row) = self
                    .session
                    .runtimes
                    .iter_mut()
                    .find(|r| r.runtime == runtime)
                {
                    let picks = |text: &str| resolver::best_match(text, &row.installed);
                    let picked = row.default.as_deref().and_then(|t| picks(t).ok().flatten());
                    row.installed.retain(|installed| *installed != version);
                    if row.installed.is_empty() && picked.as_ref() == Some(&version) {
                        row.default = None;
                    }
                }
                self.session
                    .notices
                    .set_uninstall(UninstallNotice::Done { runtime, version });
            }
            // The new list comes as `sites_changed`.
            Removal::Unlink { site } => {
                // The page of the site is gone: back to the list, where the
                // line about it shows.
                if self.route == Route::Site(site.clone()) {
                    self.route = Route::Sites;
                }
                self.session.notices.set_sites(SitesNotice::Unlinked(site));
            }
            Removal::Forget { folder } => {
                self.session.notices.set_sites(SitesNotice::Forgot(folder))
            }
            // The list after the delete follows from the bridge; the screen
            // shows it, so no notice repeats it.
            Removal::Mail => {
                self.session.mail.selected = None;
                self.session.mail.text = None;
            }
        }
    }

    pub(super) fn not_removed(
        &mut self,
        removal: Removal,
        code: String,
        in_use: Option<VersionInUse>,
    ) {
        if !self.settle_removal(&removal) {
            return;
        }
        match removal {
            Removal::Uninstall { runtime, version } => {
                self.session.notices.set_uninstall(match in_use {
                    Some(in_use) => UninstallNotice::InUse { runtime, in_use },
                    None => UninstallNotice::Failed {
                        runtime,
                        version,
                        code,
                    },
                });
            }
            Removal::Unlink { site } if code == SITE_IS_PARKED => {
                let folder = self.holder(&site).map(Path::to_path_buf);
                self.session
                    .notices
                    .set_sites(SitesNotice::SiteIsParked { site, folder });
            }
            Removal::Unlink { .. } | Removal::Forget { .. } => {
                self.session.notices.set_sites(SitesNotice::Failed(code));
            }
            Removal::Mail => self.mail_call_failed(code),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use marsa_core::types::VersionInUse;

    use super::*;
    use crate::strings::tests::{full, texts, tilde, v};
    use crate::strings::{error_text, home_path};

    fn in_use(default: Option<&str>, sites: &[&str], caddy: bool) -> UninstallNotice {
        UninstallNotice::InUse {
            runtime: Runtime::Php,
            in_use: VersionInUse {
                version: "8.3.27".into(),
                default: default.map(str::to_owned),
                sites: sites.iter().map(|site| (*site).to_owned()).collect(),
                caddy,
                ..VersionInUse::default()
            },
        }
    }

    /// The bar says an uninstall in one sentence; the list of what uses the
    /// version stays in [`UninstallNotice::lines`].
    #[test]
    fn the_uninstall_notice_is_one_sentence() {
        let notice = in_use(Some("8.3"), &["shop", "blog"], true).line();
        assert_eq!(
            (notice.text.as_str(), notice.problem),
            ("PHP 8.3.27 is in use, so it was not uninstalled.", true)
        );
        let version: Version = "8.2.28".parse().expect("a version");
        let done = UninstallNotice::Done {
            runtime: Runtime::Php,
            version: version.clone(),
        }
        .line();
        assert_eq!(
            (done.text.as_str(), done.problem),
            ("Uninstalled PHP 8.2.28.", false)
        );
        let failed = UninstallNotice::Failed {
            runtime: Runtime::Php,
            version,
            code: "uninstall_failed".into(),
        }
        .line();
        assert!(failed.problem && failed.text.starts_with("Cannot uninstall PHP 8.2.28. "));
    }

    #[test]
    fn a_version_in_use_names_what_uses_it() {
        let title = ("PHP 8.3.27 is in use, so it was not uninstalled:", true);
        let lines = in_use(Some("8.3"), &["shop"], false).lines();
        assert_eq!(
            texts(&lines),
            [
                title,
                (
                    "It is the default PHP (set to 8.3). Pick another default above first.",
                    true
                ),
                (
                    "The site shop.test is set to it. Pick another PHP version for it first.",
                    true
                ),
            ]
        );
        let lines = in_use(None, &["shop", "blog"], false).lines();
        assert_eq!(
            texts(&lines)[1],
            (
                "The sites shop.test, blog.test are set to it. Pick another PHP version for \
                 them first.",
                true
            )
        );
        let caddy = UninstallNotice::InUse {
            runtime: Runtime::Caddy,
            in_use: VersionInUse {
                version: "2.11.7".into(),
                caddy: true,
                ..VersionInUse::default()
            },
        };
        assert_eq!(
            texts(&caddy.lines()),
            [
                ("Caddy 2.11.7 is in use, so it was not uninstalled:", true),
                (
                    "Caddy is running from it. Stop Caddy under Services first.",
                    true
                ),
            ]
        );
        // No fact at all: the general text.
        let lines = in_use(None, &[], false).lines();
        let general = error_text("version_in_use");
        assert_eq!(texts(&lines), [title, (general.as_str(), true)]);

        // The only PHP, with sites that would have no PHP after it.
        let mut last = in_use(None, &[], false);
        let one = "The site blog.test would have no PHP: PHP 8.3.27 is the only PHP installed. \
                   Install another PHP version and pick it as the default above first.";
        let many = "The sites blog.test, shop.test would have no PHP: PHP 8.3.27 is the only PHP \
                    installed. Install another PHP version and pick it as the default above \
                    first.";
        for (sites, want) in [(&["blog"][..], one), (&["blog", "shop"][..], many)] {
            if let UninstallNotice::InUse { in_use, .. } = &mut last {
                in_use.sites_left_without_php =
                    sites.iter().map(|site| (*site).to_owned()).collect();
            }
            assert_eq!(texts(&last.lines()), [title, (want, true)]);
        }
    }

    #[test]
    fn an_uninstall_says_how_it_ended() {
        let version: Version = v("8.3.27");
        let done = UninstallNotice::Done {
            runtime: Runtime::Node,
            version: version.clone(),
        };
        assert_eq!(
            texts(&done.lines()),
            [("Uninstalled Node.js 8.3.27.", false)]
        );
        let failed = UninstallNotice::Failed {
            runtime: Runtime::Php,
            version,
            code: "uninstall_io".into(),
        };
        let want = format!(
            "Cannot uninstall PHP 8.3.27. {}",
            error_text("uninstall_io")
        );
        assert_eq!(texts(&failed.lines()), [(want.as_str(), true)]);
    }

    #[test]
    fn each_removal_has_its_confirmation() {
        let uninstall = Removal::Uninstall {
            runtime: Runtime::Php,
            version: v("8.3.27"),
        };
        let unlink = Removal::Unlink { site: "api".into() };
        let forget = Removal::Forget {
            folder: PathBuf::from("/code"),
        };
        assert_eq!(uninstall.title(None), "Uninstall PHP 8.3.27?");
        assert_eq!(unlink.title(None), "Unlink api.test?");
        assert_eq!(forget.title(None), "Forget /code?");
        assert_eq!(
            [&uninstall, &unlink, &forget].map(|removal| removal.button()),
            ["Uninstall", "Unlink", "Forget"]
        );
        assert_eq!(uninstall.progress(), "Uninstalling PHP 8.3.27…");
        assert_eq!(unlink.progress(), "Unlinking api.test…");
        assert_eq!(forget.progress(), "Forgetting /code…");

        let text = unlink.text(Some(Path::new("/work/api")), 0, None);
        assert!(
            text.ends_with("The folder /work/api and its files stay."),
            "{text}"
        );
        assert!(
            unlink
                .text(None, 0, None)
                .ends_with("Its folder and files stay.")
        );
        // Under the home folder, a folder shows as `~/…`, in the title too.
        let (work, api, code_dir) = (full("/work"), full("/work/api"), full("/work/code"));
        let home = Some(work.as_path());
        let shown = home_path(&api, home);
        assert_eq!(shown, tilde(&["api"]));
        let text = unlink.text(Some(&api), 0, home);
        assert!(
            text.ends_with(&format!("The folder {shown} and its files stay.")),
            "{text}"
        );
        let code = Removal::Forget { folder: code_dir };
        assert_eq!(code.title(home), format!("Forget {}?", tilde(&["code"])));
        for (sites, start) in [
            (0, "Marsa stops watching this folder."),
            (1, "Marsa stops serving its site,"),
            (3, "Marsa stops serving its 3 sites,"),
        ] {
            let text = forget.text(None, sites, None);
            assert!(text.starts_with(start), "{text}");
            assert!(text.ends_with("The folder and its files stay."), "{text}");
        }
        assert!(uninstall.text(None, 0, None).contains("install PHP again"));
    }
}
