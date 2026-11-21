//! Removals: uninstall, unlink, and forget, each after a confirmation.

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
            Removal::Unlink { .. } | Removal::Forget { .. } => self.can_remove(),
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
                self.session.uninstall_notice = None;
                self.session.runtimes_notice = None;
            }
            Removal::Unlink { .. } | Removal::Forget { .. } => self.session.sites_notice = None,
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
                self.session.uninstall_notice = Some(UninstallNotice::Done { runtime, version });
            }
            // The new list comes as `sites_changed`.
            Removal::Unlink { site } => {
                // The page of the site is gone: back to the list, where the
                // line about it shows.
                if self.route == Route::Site(site.clone()) {
                    self.route = Route::Sites;
                }
                self.session.sites_notice = Some(SitesNotice::Unlinked(site));
            }
            Removal::Forget { folder } => {
                self.session.sites_notice = Some(SitesNotice::Forgot(folder))
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
                self.session.uninstall_notice = Some(match in_use {
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
                self.session.sites_notice = Some(SitesNotice::SiteIsParked { site, folder });
            }
            Removal::Unlink { .. } | Removal::Forget { .. } => {
                self.session.sites_notice = Some(SitesNotice::Failed(code));
            }
        }
    }
}
