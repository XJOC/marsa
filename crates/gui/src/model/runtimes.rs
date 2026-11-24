//! The runtimes: what is installed, the default of each, and the PHP version of each site.

use super::*;

/// The runtime the Runtimes screen opens on: the main one.
pub const MAIN_RUNTIME: Runtime = Runtime::Php;

/// One runtime in the Runtimes section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRow {
    pub runtime: Runtime,
    /// Oldest first.
    pub installed: Vec<Version>,
    /// As saved in `global.toml` (`8.4`). Always `None` for Composer and
    /// Caddy.
    pub default: Option<String>,
}

/// One entry of a PHP or Node list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// A site with no setting of its own: its version file, its
    /// `composer.json`, or the default picks the version. Sites only.
    Auto,
    /// This installed version.
    Version(Version),
}

/// A setting the window changes through the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// `PUT /v1/runtimes/{tool}/default` with this text (`8.4`).
    Default { runtime: Runtime, version: String },
    /// `PATCH /v1/sites/{name}` with this PHP text. `None` clears the site's
    /// own setting.
    SitePhp { site: String, php: Option<String> },
}

impl Change {
    fn setting(&self) -> Setting<'_> {
        match self {
            Change::Default { runtime, .. } => Setting::Default(*runtime),
            Change::SitePhp { site, .. } => Setting::SitePhp(site),
        }
    }
}

/// Which setting a [`Change`] is about. Borrows the site name, so finding
/// the pending change of a row copies nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Setting<'a> {
    Default(Runtime),
    SitePhp(&'a str),
}

/// A change sent to the daemon and not answered yet, with the entry its
/// list shows meanwhile. A refusal shows the daemon's value again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Pending {
    change: Change,
    shown: Choice,
}

/// Whether `runtime` has no installed version in the list `rows`.
pub fn runtime_missing(rows: &[RuntimeRow], runtime: Runtime) -> bool {
    rows.iter()
        .find(|row| row.runtime == runtime)
        .is_none_or(|row| row.installed.is_empty())
}

/// What a service is called: `caddy` is Caddy, `php-fpm-8.4` is PHP-FPM 8.4.
/// A database server and a site's worker (`queue-shop`) show under their id.
pub fn service_name(id: ServiceId) -> String {
    match id {
        ServiceId::Caddy => strings::SERVICE_CADDY.to_owned(),
        ServiceId::Dns => strings::SERVICE_DNS.to_owned(),
        ServiceId::Pool(PoolKind::Fpm, minor) => strings::service_php_fpm(&minor.to_string()),
        ServiceId::Pool(PoolKind::Cgi, minor) => strings::service_php_cgi(&minor.to_string()),
        ServiceId::Database(_) | ServiceId::Worker(..) => id.to_string(),
    }
}

/// What a service does, in one line under its name. `waiting`: a pool that
/// no site needs now. `sites`: how many sites run the PHP line of a pool.
pub fn service_description(id: ServiceId, waiting: bool, sites: usize) -> String {
    match id {
        ServiceId::Caddy => strings::DESCRIPTION_CADDY.to_owned(),
        ServiceId::Dns => strings::DESCRIPTION_DNS.to_owned(),
        ServiceId::Database(_) | ServiceId::Worker(..) => strings::DESCRIPTION_OTHER.to_owned(),
        ServiceId::Pool(..) if waiting => strings::DESCRIPTION_POOL_WAITING.to_owned(),
        ServiceId::Pool(_, minor) => strings::description_pool(&minor.to_string(), sites),
    }
}

/// Whether `runtime` has a default version. Composer and Caddy have none: the
/// newest installed version is used, and so has any runtime core adds later,
/// until it is listed here (the `matches!` is false for it, so it builds).
pub fn has_default(runtime: Runtime) -> bool {
    matches!(runtime, Runtime::Php | Runtime::Node)
}

/// The text to save for `chosen`: its line (`8.4` for PHP, `22` for Node)
/// when the line picks `chosen` now, so later patch releases follow, as with
/// `marsa use 8.4` and the default set by the first install. Otherwise the
/// full version, so an older patch release stays the one used.
pub fn version_text(runtime: Runtime, chosen: &Version, installed: &[Version]) -> String {
    let line = match runtime {
        Runtime::Node => chosen.major.to_string(),
        _ => format!("{}.{}", chosen.major, chosen.minor),
    };
    match resolver::best_match(&line, installed) {
        Ok(Some(best)) if best == *chosen => line,
        _ => chosen.to_string(),
    }
}

impl Model {
    /// The installed PHP versions, oldest first.
    pub fn php_versions(&self) -> &[Version] {
        self.row(Runtime::Php)
            .map_or(&[], |row| row.installed.as_slice())
    }

    /// The entry a site's PHP list shows: the one the user just picked, the
    /// version of the site's own setting, or [`Choice::Auto`].
    pub fn site_choice(&self, site: &Site) -> Choice {
        if let Some(shown) = self.pending_choice(Setting::SitePhp(&site.name)) {
            return shown.clone();
        }
        match (&site.php_source, &site.php) {
            (Some(Source::ProjectSetting), Some(version)) => Choice::Version(version.clone()),
            _ => Choice::Auto,
        }
    }

    /// The user picked `choice` in the PHP list of the site `name`. Returns
    /// the change to send, or `None` if it is the entry shown already. The
    /// list shows `choice` until the daemon answers.
    pub fn choose_site_php(&mut self, name: &str, choice: Choice) -> Option<Change> {
        let site = self
            .session
            .sites
            .sites
            .iter()
            .find(|site| site.name == name)?;
        if self.site_choice(site) == choice {
            return None;
        }
        let php = match &choice {
            Choice::Auto => None,
            Choice::Version(version) => {
                Some(version_text(Runtime::Php, version, self.php_versions()))
            }
        };
        let change = Change::SitePhp {
            site: name.to_owned(),
            php,
        };
        // The notice was about the last action.
        self.session.notices.clear(NoticeSource::Sites);
        self.session.pending.push(Pending {
            change: change.clone(),
            shown: choice,
        });
        Some(change)
    }

    /// The installed version the default of `runtime` picks now, or the one
    /// the user just picked. `None` when there is no default, or it matches
    /// no installed version.
    pub fn default_choice(&self, runtime: Runtime) -> Option<Version> {
        if let Some(Choice::Version(version)) = self.pending_choice(Setting::Default(runtime)) {
            return Some(version.clone());
        }
        let row = self.row(runtime)?;
        let text = row.default.as_deref()?;
        resolver::best_match(text, &row.installed).ok().flatten()
    }

    /// The user picked `version` as the default of `runtime`. Returns the
    /// change to send, or `None` if it is the default already.
    pub fn choose_default(&mut self, runtime: Runtime, version: Version) -> Option<Change> {
        if !has_default(runtime) || self.default_choice(runtime).as_ref() == Some(&version) {
            return None;
        }
        let installed = self.row(runtime).map_or(&[][..], |row| &row.installed);
        let change = Change::Default {
            runtime,
            version: version_text(runtime, &version, installed),
        };
        self.session.notices.clear(NoticeSource::Runtimes);
        self.session.pending.push(Pending {
            change: change.clone(),
            shown: Choice::Version(version),
        });
        Some(change)
    }

    pub(super) fn row(&self, runtime: Runtime) -> Option<&RuntimeRow> {
        self.session
            .runtimes
            .iter()
            .find(|row| row.runtime == runtime)
    }

    /// The installed versions of `runtime`, oldest first. Empty until the
    /// daemon says.
    pub fn installed(&self, runtime: Runtime) -> &[Version] {
        self.row(runtime)
            .map_or(&[], |row| row.installed.as_slice())
    }

    /// The runtime the Runtimes screen shows: the one of its route, else the
    /// one it showed last, else the main one.
    pub fn chosen_runtime(&self) -> Runtime {
        match &self.route {
            Route::Runtimes(runtime) => *runtime,
            _ => self.runtime_chosen.unwrap_or(MAIN_RUNTIME),
        }
    }

    /// The sites that run `version` of `runtime` now, by name, in the order
    /// of the list: the version the daemon says, or the one the user just
    /// picked for the site. Only PHP has a version per site in the list
    /// (`Site::php`), so any other runtime has none.
    pub fn sites_running(&self, runtime: Runtime, version: &Version) -> Vec<&str> {
        if runtime != Runtime::Php {
            return Vec::new();
        }
        let runs = |site: &Site| match self.site_choice(site) {
            Choice::Version(own) => own == *version,
            Choice::Auto => site.php.as_ref() == Some(version),
        };
        let sites = &self.session.sites.sites;
        sites
            .iter()
            .filter(|site| runs(site))
            .map(|site| site.name.as_str())
            .collect()
    }

    /// How many sites run a PHP version of `line` (`8.4`): what a pool of that
    /// line serves. A line that is not `major.minor` has none.
    pub fn sites_on_line(&self, line: &str) -> usize {
        let Some((major, minor)) = line.split_once('.') else {
            return 0;
        };
        let (Ok(major), Ok(minor)) = (major.parse::<u64>(), minor.parse::<u64>()) else {
            return 0;
        };
        self.session
            .sites
            .sites
            .iter()
            .filter_map(|site| site.php.as_ref())
            .filter(|php| php.major == major && php.minor == minor)
            .count()
    }

    /// The sites whose own setting picks `version` of `runtime`: the ones the
    /// daemon refuses to uninstall it for (`version_in_use`). PHP only, as
    /// [`Model::sites_running`].
    pub fn sites_set_to(&self, runtime: Runtime, version: &Version) -> Vec<&str> {
        if runtime != Runtime::Php {
            return Vec::new();
        }
        let sites = &self.session.sites.sites;
        sites
            .iter()
            .filter(|site| self.site_choice(site) == Choice::Version(version.clone()))
            .map(|site| site.name.as_str())
            .collect()
    }

    /// What the newest unanswered change of `setting` shows, if any.
    pub(super) fn pending_choice(&self, setting: Setting<'_>) -> Option<&Choice> {
        // Newest first: the user may pick again before the first answer.
        self.session
            .pending
            .iter()
            .rev()
            .find(|pending| pending.change.setting() == setting)
            .map(|pending| &pending.shown)
    }

    /// The daemon answered `change`: its list shows the daemon's value again.
    pub(super) fn settle(&mut self, change: &Change) {
        if let Some(index) = self
            .session
            .pending
            .iter()
            .position(|p| p.change == *change)
        {
            self.session.pending.remove(index);
        }
    }

    /// Versions that are not semver are left out: the daemon lists only
    /// folders named by a version. Opens or closes the first-run panel.
    pub(super) fn set_runtimes(&mut self, list: RuntimeList) {
        self.session.runtimes = list
            .runtimes
            .into_iter()
            .map(|runtime| {
                let mut installed: Vec<Version> = runtime
                    .installed
                    .iter()
                    .filter_map(|text| text.parse().ok())
                    .collect();
                installed.sort();
                RuntimeRow {
                    runtime: runtime.runtime,
                    installed,
                    default: runtime.default,
                }
            })
            .collect();
        let needed = first_run_needed(&self.session.runtimes);
        match self.session.first_run {
            None if needed && !self.first_run_closed => {
                self.session.first_run = Some(FirstRun::Offer)
            }
            // PHP or Caddy came some other way (`marsa install`).
            Some(FirstRun::Offer) if !needed => self.session.first_run = None,
            _ => {}
        }
    }
}
