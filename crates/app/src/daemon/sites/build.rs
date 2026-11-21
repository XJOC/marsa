//! The site list: what `sites.toml`, the folders, and the installed PHP
//! versions say the sites are now.

use std::path::Path;

use marsa_core::config::{self, ConfigPaths, GlobalConfig, SitesConfig};
use marsa_core::resolver::{self, Tool};
use marsa_core::runtimes::{Runtime, RuntimesDir};
use marsa_core::sites;
use marsa_core::types::{SiteList, SkippedFolder};

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
            Err(err) => skipped.push(SkippedFolder::new(&table.root, err.code())),
        }
    }
    let mut scans = Vec::new();
    for dir in &sites_toml.parked {
        match sites::scan_parked(dir) {
            Ok(scan) => scans.push(scan),
            Err(err) => skipped.push(SkippedFolder::new(dir, err.code())),
        }
    }
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
    let default = global.php.default.as_deref();
    sites::list(sites_toml, linked, scans, skipped, |found, setting| {
        // No PHP at all: skip the version files, the answer is `None`. The
        // resolution keeps where the version came from, for the window.
        if installed.is_empty() {
            return None;
        }
        match resolver::resolve(Tool::Php, &found.root, setting, &installed, default) {
            Ok(resolution) => Some(resolution),
            Err(err) => {
                let code = err.code();
                tracing::debug!(site = %found.name, code, error = %err, "no PHP version");
                None
            }
        }
    })
}
