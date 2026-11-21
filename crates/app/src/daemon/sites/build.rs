//! The site list: what `sites.toml`, the folders, and the installed PHP
//! versions say the sites are now.

use std::path::{Path, PathBuf};

use marsa_core::config::{self, ConfigPaths, GlobalConfig, SitesConfig};
use marsa_core::resolver::{self, Tool};
use marsa_core::runtimes::{Runtime, RuntimesDir};
use marsa_core::sites::{self, Found, SkipReason, Skipped};
use marsa_core::types::{Site, SiteKind, SiteList, SkippedFolder};

/// The site list for `sites_toml`: each linked table, then each parked
/// folder, joined by [`sites::merge`], with the PHP version and the settings
/// of each site.
///
/// Reads the disk (the folders, version files, `global.toml`, and
/// `runtimes/php`), so it runs on a blocking thread. Never fails: a folder
/// that cannot be used is in `skipped` with its code, and a `global.toml` or
/// `runtimes/php` that cannot be read only leaves no PHP version to pick.
pub fn build(data_dir: &Path, sites_toml: &SitesConfig) -> SiteList {
    let mut skipped = Vec::new();
    let mut linked = Vec::new();
    for (name, table) in &sites_toml.sites {
        match sites::link(name, &table.root) {
            Ok(found) => linked.push(found),
            Err(err) => skipped.push(skipped_folder(&table.root, err.code())),
        }
    }
    let mut scans = Vec::new();
    for dir in &sites_toml.parked {
        match sites::scan_parked(dir) {
            Ok(scan) => scans.push(scan),
            Err(err) => skipped.push(skipped_folder(dir, err.code())),
        }
    }
    let merged = sites::merge(linked, scans);
    // A parked folder with its own table is found twice under one name. That
    // is one site, not a clash.
    let itself =
        |s: &Skipped| matches!(&s.reason, SkipReason::NameTaken { by, .. } if *by == s.path);
    let others = merged.skipped.iter().filter(|s| !itself(s));
    skipped.extend(others.map(|s| skipped_folder(&s.path, s.reason.code())));

    let installed = RuntimesDir::in_dir(data_dir)
        .installed(Runtime::Php)
        .unwrap_or_else(|err| {
            tracing::warn!(code = err.code(), error = %err, "cannot list the PHP versions");
            Vec::new()
        });
    let global = config::load_global(&ConfigPaths::in_dir(data_dir).global).unwrap_or_else(|err| {
        tracing::warn!(code = err.code(), error = %err, "global.toml");
        GlobalConfig::default()
    });
    let site = |found: Found| -> Site {
        let own = sites_toml
            .sites
            .get(&found.name)
            .filter(|table| table.root == found.root);
        // The shim's rule (`site_for`: the nearest table at or above the
        // folder), so a terminal in the root runs the same PHP. The site's own
        // table first, for two names on one folder.
        let setting = own.or_else(|| sites_toml.site_for(&found.root).map(|(_, table)| table));
        let setting = setting.and_then(|table| table.php.as_deref());
        let default = global.php.default.as_deref();
        // No PHP at all: skip the version files, the answer is `None`. The
        // resolution keeps where the version came from, for the window.
        let php = if installed.is_empty() {
            None
        } else {
            match resolver::resolve(Tool::Php, &found.root, setting, &installed, default) {
                Ok(resolution) => Some(resolution),
                Err(err) => {
                    let code = err.code();
                    tracing::debug!(site = %found.name, code, error = %err, "no PHP version");
                    None
                }
            }
        };
        let proxy = own.and_then(|table| table.proxy);
        let mut site = found.into_site(php, own.is_none_or(|table| table.secure));
        if let Some(port) = proxy {
            site.kind = SiteKind::NodeProxy { port };
        }
        site
    };
    SiteList {
        parked: sites_toml.parked.clone(),
        sites: merged.found.into_iter().map(site).collect(),
        skipped,
    }
}

fn skipped_folder(path: &Path, code: &str) -> SkippedFolder {
    SkippedFolder {
        // JSON text must be UTF-8, and the path is only shown.
        path: PathBuf::from(path.to_string_lossy().into_owned()),
        code: code.to_owned(),
    }
}
