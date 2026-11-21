//! The results of what the user did, one entry for each place that tells
//! about one. The notice bar (`shell::notices`) reads them, and closing the
//! bar clears the entry it showed ([`Model::dismiss_notice`]).

use std::collections::BTreeMap;

use super::{SitesNotice, UninstallNotice};

/// Which of the model's results a notice in the bar tells about. Closing the
/// notice clears that result (`Model::dismiss_notice`), so the bar shows the
/// next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NoticeSource {
    /// A call to the daemon failed: an error code.
    Call,
    /// The last sites action ([`SitesNotice`]).
    Sites,
    /// How the install shown ended (`Model::install_result`). Not stored
    /// here: the installs hold it.
    Install,
    /// The last uninstall ([`UninstallNotice`]).
    Uninstall,
    /// The runtimes could not be read, or a default not saved: an error code.
    Runtimes,
    /// Start at login failed: an error code.
    Autostart,
    /// Xdebug could not be read or changed: an error code. The Runtimes
    /// screen and the panel show it beside the box, so the bar does not.
    Xdebug,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Noticed {
    Code(String),
    Sites(SitesNotice),
    Uninstall(UninstallNotice),
}

/// The last result of each [`NoticeSource`], if it has one. A new session
/// starts with none.
#[derive(Debug, Default)]
pub struct Notices(BTreeMap<NoticeSource, Noticed>);

impl Notices {
    /// The error code of `source`, if it holds one.
    pub fn code(&self, source: NoticeSource) -> Option<&str> {
        match self.0.get(&source) {
            Some(Noticed::Code(code)) => Some(code),
            _ => None,
        }
    }

    pub fn set_code(&mut self, source: NoticeSource, code: String) {
        self.0.insert(source, Noticed::Code(code));
    }

    pub fn sites(&self) -> Option<&SitesNotice> {
        match self.0.get(&NoticeSource::Sites) {
            Some(Noticed::Sites(notice)) => Some(notice),
            _ => None,
        }
    }

    pub fn set_sites(&mut self, notice: SitesNotice) {
        self.0.insert(NoticeSource::Sites, Noticed::Sites(notice));
    }

    pub fn uninstall(&self) -> Option<&UninstallNotice> {
        match self.0.get(&NoticeSource::Uninstall) {
            Some(Noticed::Uninstall(notice)) => Some(notice),
            _ => None,
        }
    }

    pub fn set_uninstall(&mut self, notice: UninstallNotice) {
        self.0
            .insert(NoticeSource::Uninstall, Noticed::Uninstall(notice));
    }

    pub fn clear(&mut self, source: NoticeSource) {
        self.0.remove(&source);
    }
}
