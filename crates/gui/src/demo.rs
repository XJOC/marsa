//! Debug builds only: fixed fake data for the design review (task M5-D5).
//! `MARSA_GUI_DEMO=ok|caddy-stopped|first-run-1|first-run-2|sites-search|sites-empty|sites-extras|site-parked`
//! fills the model with a few sites, PHP and Caddy (or nothing, for the
//! first run), and starts no bridge and no daemon: the window draws a state the review needs without a
//! machine to break. Pair it with
//! `MARSA_GUI_SCREENSHOT` and `MARSA_GUI_THEME` (see `screenshot`).
//!
//! Nothing here reaches a real data folder: the names and folders are made
//! up, and a button pressed in the demo sends its command to a bridge that
//! is not there.

use std::path::PathBuf;

use marsa_core::runtimes::Runtime;
use marsa_core::types::{
    Event, Health, InstallEvent, InstallProgress, InstallUpdate, RuntimeList, RuntimeVersions,
    ServiceState, ServiceStatus, Site, SiteKind, SiteList, SkippedFolder, Source,
};

use crate::model::{LinkForm, Model, SitesNotice, Update, WEB_SERVER, apply};

/// The variable that names the state.
const VAR: &str = "MARSA_GUI_DEMO";

/// A state to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Demo {
    /// PHP, Caddy, and six sites, all working.
    Ok,
    /// The same, with Caddy stopped.
    CaddyStopped,
    /// A new install: nothing installed, and PHP 8.4 downloading, 18 of
    /// 31 MB (the first step of the first-run panel).
    FirstRun1,
    /// PHP 8.4 and Caddy installed, no sites yet: the first step is done and
    /// setup is the one to do.
    FirstRun2,
    /// The same as `Ok`, with "li" in the search field of Sites (task
    /// M5-D6).
    SitesSearch,
    /// The same as `Ok`, with no site: the empty state of Sites.
    SitesEmpty,
    /// The same as `Ok`, with the link form open, a line about the last
    /// action, two parked folders, and two skipped ones.
    SitesExtras,
    /// The same as `Ok`, with `~/Code` parked: the sites in it come from that
    /// folder, so their pages cannot unlink them (task M5-D7).
    SiteParked,
}

/// The state `MARSA_GUI_DEMO` asks for, if it names one.
pub fn requested() -> Option<Demo> {
    parse(&std::env::var(VAR).ok()?)
}

fn parse(name: &str) -> Option<Demo> {
    Some(match name {
        "ok" => Demo::Ok,
        "caddy-stopped" => Demo::CaddyStopped,
        "first-run-1" => Demo::FirstRun1,
        "first-run-2" => Demo::FirstRun2,
        "sites-search" => Demo::SitesSearch,
        "sites-empty" => Demo::SitesEmpty,
        "sites-extras" => Demo::SitesExtras,
        "site-parked" => Demo::SiteParked,
        _ => return None,
    })
}

/// Puts the demo's data in `model`, as a daemon would have sent it.
pub fn fill(model: &mut Model, demo: Demo) {
    model.update(Update::Connected(Health {
        app_version: "0.0.0".into(),
        api_version: 1,
        pid: 1,
    }));
    // The made-up folders are under this "home", so they show as `~/…`.
    model.home = Some(PathBuf::from("/demo"));
    if demo == Demo::FirstRun1 {
        return first_run_downloading(model);
    }
    if demo == Demo::FirstRun2 {
        model.update(Update::Runtimes(runtimes(&["8.4.27"])));
        model.update(Update::Services(vec![service(
            WEB_SERVER,
            ServiceState::Running,
        )]));
        model.update(Update::Sites(SiteList::default()));
        return;
    }
    model.update(Update::Runtimes(runtimes(&["8.3.27", "8.4.27"])));
    let caddy = if demo == Demo::CaddyStopped {
        ServiceState::Stopped
    } else {
        ServiceState::Running
    };
    model.update(Update::Services(vec![
        service(WEB_SERVER, caddy),
        service("php-fpm-8.4", ServiceState::Running),
    ]));
    let list = if demo == Demo::SitesEmpty {
        SiteList::default()
    } else {
        sites()
    };
    model.update(Update::Sites(list));
    if demo == Demo::SitesSearch {
        model.sites_search = "li".into();
    }
    if demo == Demo::SitesExtras {
        sites_extras(model);
    }
    if demo == Demo::SiteParked {
        model.sites.parked = vec![PathBuf::from("/demo/Code")];
    }
}

/// The parts of Sites the plain demo leaves out.
fn sites_extras(model: &mut Model) {
    model.sites.parked = vec![
        PathBuf::from("/demo/Code"),
        PathBuf::from("/demo/Work/acme"),
    ];
    model.sites.skipped = vec![
        SkippedFolder {
            path: PathBuf::from("/demo/Code/My Old Site"),
            code: "site_name_invalid".into(),
        },
        SkippedFolder {
            path: PathBuf::from("/demo/Work/acme/portal-copy"),
            code: "site_name_taken".into(),
        },
    ];
    model.sites_notice = Some(SitesNotice::Unlinked("old-blog".into()));
    model.link = Some(LinkForm {
        folder: PathBuf::from("/demo/Work/blog"),
        name: "blog".into(),
    });
}

/// Nothing installed, and the first-run install under way: what the panel
/// shows once the user pressed its button.
fn first_run_downloading(model: &mut Model) {
    model.update(Update::Runtimes(RuntimeList {
        runtimes: vec![
            RuntimeVersions {
                runtime: Runtime::Php,
                installed: Vec::new(),
                default: None,
            },
            RuntimeVersions {
                runtime: Runtime::Caddy,
                installed: Vec::new(),
                default: None,
            },
        ],
    }));
    model.start_first_run();
    let Some(job) = model.next_install() else {
        return;
    };
    model.update(Update::InstallAccepted { job, id: 1 });
    let progress = |step| {
        Event::Install(InstallProgress {
            id: 1,
            update: InstallUpdate::Running(step),
        })
    };
    let selected = InstallEvent::Selected {
        runtime: Runtime::Php,
        version: "8.4.27".into(),
    };
    let downloading = InstallEvent::Downloading {
        artifact: "php-8.4.27-macos-arm64.tar.gz".into(),
        done: 18_000_000,
        total: 31_000_000,
    };
    apply(model, progress(selected));
    apply(model, progress(downloading));
}

fn runtimes(php: &[&str]) -> RuntimeList {
    let row = |runtime, installed: &[&str], default: Option<&str>| RuntimeVersions {
        runtime,
        installed: installed.iter().map(|text| (*text).to_owned()).collect(),
        default: default.map(str::to_owned),
    };
    RuntimeList {
        runtimes: vec![
            row(Runtime::Php, php, Some("8.4")),
            row(Runtime::Caddy, &["2.11.7"], None),
        ],
    }
}

fn service(id: &str, state: ServiceState) -> ServiceStatus {
    ServiceStatus {
        id: id.into(),
        pid: None,
        state,
        restarts: 0,
        since_ms: None,
    }
}

/// Six sites as the mockup lists them: each version has its own reason.
fn sites() -> SiteList {
    let site = |name: &str, folder: &str, php: &str, source: Source, secure: bool| {
        let root = PathBuf::from("/demo").join(folder);
        Site {
            name: name.into(),
            docroot: root.join("public"),
            root,
            kind: SiteKind::Laravel,
            php: php.parse().ok(),
            php_source: Some(source),
            secure,
        }
    };
    let file = |folder: &str, name: &str| PathBuf::from("/demo").join(folder).join(name);
    SiteList {
        sites: vec![
            site(
                "shop",
                "Code/shop",
                "8.4.27",
                Source::VersionFile(file("Code/shop", ".php-version")),
                true,
            ),
            site(
                "billing-api",
                "Code/billing-api",
                "8.4.27",
                Source::GlobalDefault,
                true,
            ),
            site(
                "legacy-crm",
                "Code/legacy-crm",
                "8.3.27",
                Source::ProjectSetting,
                true,
            ),
            site(
                "docs",
                "Code/docs",
                "8.4.27",
                Source::Constraint(file("Code/docs", "composer.json")),
                false,
            ),
            site(
                "client-portal",
                "Work/acme/portal",
                "8.4.27",
                Source::GlobalDefault,
                true,
            ),
            site(
                "wiki",
                "Sites/wiki",
                "8.3.27",
                Source::VersionFile(file("Sites/wiki", ".php-version")),
                true,
            ),
        ],
        ..SiteList::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_demo_is_named_by_its_state() {
        assert_eq!(parse("ok"), Some(Demo::Ok));
        assert_eq!(parse("caddy-stopped"), Some(Demo::CaddyStopped));
        assert_eq!(parse("first-run-1"), Some(Demo::FirstRun1));
        assert_eq!(parse("first-run-2"), Some(Demo::FirstRun2));
        assert_eq!(parse("sites-search"), Some(Demo::SitesSearch));
        assert_eq!(parse("sites-empty"), Some(Demo::SitesEmpty));
        assert_eq!(parse("sites-extras"), Some(Demo::SitesExtras));
        assert_eq!(parse("site-parked"), Some(Demo::SiteParked));
        assert_eq!(parse("on fire"), None);
    }

    #[test]
    fn the_first_run_demo_has_nothing_installed_and_the_php_download_running() {
        let mut model = Model::default();
        fill(&mut model, Demo::FirstRun1);
        assert!(model.needs_first_run());
        assert!(model.installing());
        let words = crate::strings::install_status(model.install_view().expect("an install"));
        assert!(words.starts_with("Downloading PHP 8.4.27"), "{words}");
    }

    #[test]
    fn the_second_first_run_demo_has_php_and_caddy_and_no_site() {
        let mut model = Model::default();
        fill(&mut model, Demo::FirstRun2);
        assert!(model.needs_first_run());
        assert!(!model.installing());
        assert!(model.sites.sites.is_empty());
        assert_eq!(model.php_versions().len(), 1);
    }

    #[test]
    fn the_sites_demos_search_by_name_or_have_no_site_at_all() {
        let mut model = Model::default();
        fill(&mut model, Demo::SitesSearch);
        assert_eq!(model.sites_search, "li");
        assert_eq!(model.sites.sites.len(), 6);
        let mut model = Model::default();
        fill(&mut model, Demo::SitesEmpty);
        assert!(model.sites.sites.is_empty() && model.is_connected());
    }

    #[test]
    fn the_extras_demo_opens_the_link_form_and_lists_the_folders() {
        let mut model = Model::default();
        fill(&mut model, Demo::SitesExtras);
        assert!(model.link.is_some() && model.sites_notice.is_some());
        assert_eq!(model.sites.parked.len(), 2);
        assert_eq!(model.sites.skipped.len(), 2);
    }

    #[test]
    fn the_parked_demo_gives_the_sites_of_its_folder_a_parked_page() {
        let mut model = Model::default();
        fill(&mut model, Demo::SiteParked);
        let by_name = |name: &str| {
            model
                .sites
                .sites
                .iter()
                .find(|site| site.name == name)
                .expect("a demo site")
        };
        // `shop` is in `~/Code`; `client-portal` is elsewhere, so it is linked.
        assert!(model.parked_folder(by_name("shop")).is_some());
        assert!(model.parked_folder(by_name("client-portal")).is_none());
    }

    #[test]
    fn the_demo_sites_show_every_kind_of_reason_and_both_connections() {
        let list = sites();
        let sources: Vec<_> = list
            .sites
            .iter()
            .map(|site| site.php_source.clone().expect("a source"))
            .collect();
        assert!(sources.contains(&Source::GlobalDefault));
        assert!(sources.contains(&Source::ProjectSetting));
        assert!(sources.iter().any(|s| matches!(s, Source::VersionFile(_))));
        assert!(sources.iter().any(|s| matches!(s, Source::Constraint(_))));
        assert!(list.sites.iter().any(|site| !site.secure));
    }

    #[test]
    fn the_demo_connects_and_fills_the_lists_with_no_daemon() {
        for demo in [Demo::Ok, Demo::CaddyStopped] {
            let mut model = Model::default();
            fill(&mut model, demo);
            assert!(model.is_connected());
            assert_eq!(model.home.as_deref(), Some(std::path::Path::new("/demo")));
            assert_eq!(model.sites.sites.len(), 6);
            assert_eq!(model.php_versions().len(), 2);
            assert_eq!(model.services.len(), 2);
        }
    }
}
