//! Sites: the table, the parked folders, the link form, and the addresses.

use super::*;

/// The daemon's answer to unlinking a site that comes from a parked folder.
pub(super) const SITE_IS_PARKED: &str = "site_is_parked";

/// The line under the sites buttons: what the last sites action did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SitesNotice {
    /// The daemon accepted this folder to park.
    Parked(PathBuf),
    /// The daemon linked `folder` as the site `name`.
    Linked { name: String, folder: PathBuf },
    /// The daemon unlinked this site.
    Unlinked(String),
    /// The daemon forgot this parked folder.
    Forgot(PathBuf),
    /// The daemon refused to unlink `site`: it comes from a parked folder.
    /// `folder` is that folder when the list names it, so the window can
    /// offer to forget it instead.
    SiteIsParked {
        site: String,
        folder: Option<PathBuf>,
    },
    /// A sites action failed. A client error code.
    Failed(String),
}

/// The form under the sites buttons once a folder to link was picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkForm {
    pub folder: PathBuf,
    /// The site name the user types. Made from the folder's name at first.
    pub name: String,
}

/// What a folder dialog was opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Park,
    Link,
}

impl Pick {
    /// The title of the folder dialog.
    pub const fn title(self) -> &'static str {
        match self {
            Pick::Park => strings::PICK_PARK_TITLE,
            Pick::Link => strings::PICK_LINK_TITLE,
        }
    }
}

/// The address of a site in the browser: `https://shop.test`, or
/// `http://shop.test` when HTTPS is off.
pub fn url(site: &Site) -> String {
    let scheme = if site.secure { "https" } else { "http" };
    format!("{scheme}://{}", site.host())
}

/// A picked folder as the daemon wants it: absolute, with `.` parts,
/// doubled separators, and a trailing separator removed, as `marsa park`
/// does.
///
/// # Errors
///
/// `site_path_relative` for a relative or empty path. The daemon would
/// answer the same, so no request is sent.
pub fn folder_path(folder: &Path) -> Result<PathBuf, &'static str> {
    if !folder.is_absolute() {
        return Err("site_path_relative");
    }
    // Not `canonicalize`: it reads the disk, follows links, and gives `\\?\`
    // paths on Windows.
    Ok(folder.components().collect())
}

impl Model {
    /// The user picked `folder` to park. Returns the folder to send, or
    /// `None` if it is not an absolute path; the notice line then says why.
    /// Clears the last notice either way: it was about the last action.
    pub fn park_request(&mut self, folder: &Path) -> Option<PathBuf> {
        match folder_path(folder) {
            Ok(path) => {
                self.session.notices.clear(NoticeSource::Sites);
                Some(path)
            }
            Err(code) => {
                self.session
                    .notices
                    .set_sites(SitesNotice::Failed(code.to_owned()));
                None
            }
        }
    }

    /// The user picked `folder` to link: the form opens, with a site name
    /// made from the folder's name when one can be made (`My_App` gives
    /// `my-app`), as `marsa link` does.
    pub fn pick_link_folder(&mut self, folder: &Path) {
        match folder_path(folder) {
            Ok(folder) => {
                let name = folder
                    .file_name()
                    .and_then(marsa_core::sites::name_from_folder)
                    .unwrap_or_default();
                self.session.notices.clear(NoticeSource::Sites);
                self.link = Some(LinkForm { folder, name });
                // The form is drawn on Sites; the first-run panel has none.
                self.route = Route::Sites;
            }
            Err(code) => self
                .session
                .notices
                .set_sites(SitesNotice::Failed(code.to_owned())),
        }
    }

    /// The site name and folder of the link form, to send. `None` when the
    /// name is not valid: the notice line says why, and no request is sent
    /// (the daemon would answer `site_name_invalid` too).
    pub fn link_request(&mut self) -> Option<(String, PathBuf)> {
        let form = self.link.as_ref()?;
        let name = form.name.trim();
        if let Err(err) = marsa_core::sites::validate_name(name) {
            self.session
                .notices
                .set_sites(SitesNotice::Failed(err.code().to_owned()));
            return None;
        }
        let request = (name.to_owned(), form.folder.clone());
        self.session.notices.clear(NoticeSource::Sites);
        Some(request)
    }

    /// The site the link form would replace: one with the same name and
    /// another folder. The daemon links the new folder in its place.
    pub fn link_replaces(&self) -> Option<&Site> {
        let form = self.link.as_ref()?;
        let name = form.name.trim();
        self.session
            .sites
            .sites
            .iter()
            .find(|site| site.name == name && site.root != form.folder)
    }

    /// The parked folder whose scan gives `site`: the folder holds the
    /// site's folder, and the site is named after it, as the daemon names
    /// the sites of a parked folder. `None` for a linked site, which Unlink
    /// removes; the daemon has the last word (`site_is_parked`).
    pub fn parked_folder(&self, site: &Site) -> Option<&Path> {
        let named = site
            .root
            .file_name()
            .and_then(marsa_core::sites::name_from_folder);
        if named.as_deref() != Some(site.name.as_str()) {
            return None;
        }
        self.parked_parent(site)
    }

    /// The parked folder that holds the folder of the site `name`, after the
    /// daemon said it comes from one.
    pub(super) fn holder(&self, name: &str) -> Option<&Path> {
        let site = self
            .session
            .sites
            .sites
            .iter()
            .find(|site| site.name == name)?;
        self.parked_parent(site)
    }

    /// The parked folder that is the parent of the folder of `site`.
    fn parked_parent(&self, site: &Site) -> Option<&Path> {
        let parent = site.root.parent()?;
        self.session
            .sites
            .parked
            .iter()
            .find(|dir| dir.as_path() == parent)
            .map(PathBuf::as_path)
    }

    /// How many sites the parked folder `folder` gives now.
    pub fn sites_in(&self, folder: &Path) -> usize {
        self.session
            .sites
            .sites
            .iter()
            .filter(|site| self.parked_folder(site) == Some(folder))
            .count()
    }

    /// The folder of the site `name`, if it is in the list.
    pub fn site_root(&self, name: &str) -> Option<&Path> {
        self.session
            .sites
            .sites
            .iter()
            .find(|site| site.name == name)
            .map(|site| site.root.as_path())
    }

    /// The address of the site `name`, for the browser.
    pub fn site_url(&self, name: &str) -> Option<String> {
        self.session
            .sites
            .sites
            .iter()
            .find(|site| site.name == name)
            .map(url)
    }

    /// The daemon sends the sites sorted by name already; sorting here keeps
    /// the table right even if a daemon does not.
    pub(super) fn set_sites(&mut self, mut list: SiteList) {
        list.sites.sort_by(|a, b| a.name.cmp(&b.name));
        self.session.sites = list;
        self.leave_gone_site_log();
        self.settle_workers();
    }
}
