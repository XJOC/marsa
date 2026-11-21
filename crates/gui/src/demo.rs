//! Debug builds only: fixed fake data for the design review.
//! `MARSA_GUI_DEMO=ok|caddy-stopped|many-sites|first-run-1|first-run-2|sites-search|sites-empty|sites-extras|site-parked|runtimes|runtimes-installing|runtimes-install|settings|settings-setup|services|logs|logs-filtered|logs-no-match|logs-long|confirm-unlink|confirm-uninstall|notice-success|notice-error`
//! fills the model with a few sites, PHP and Caddy (or nothing, for the
//! first run), and starts no bridge and no daemon: the window draws a state the review needs without a
//! machine to break. Pair it with
//! `MARSA_GUI_SCREENSHOT` and `MARSA_GUI_THEME` (see `screenshot`).
//!
//! Nothing here reaches a real data folder: the names and folders are made
//! up, and a button pressed in the demo sends its command to a bridge that
//! is not there.

use std::path::PathBuf;

use marsa_core::helper::{OpResult, OpStatus};
use marsa_core::runtimes::Runtime;
use marsa_core::types::{
    Event, Health, InstallEvent, InstallProgress, InstallUpdate, LogLine, LogStream, RuntimeList,
    RuntimeVersions, ServiceState, ServiceStatus, SetupReport, Site, SiteKind, SiteList,
    SkippedFolder, Source, XdebugState,
};

use crate::model::{
    Action, Autostart, InstallJob, LinkForm, LogLevel, Model, Removal, SitesNotice, Update,
    WEB_SERVER, apply,
};

/// The variable that names the state.
const VAR: &str = "MARSA_GUI_DEMO";

/// A state to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Demo {
    /// PHP, Caddy, and six sites, all working.
    Ok,
    /// The same, with Caddy stopped.
    CaddyStopped,
    /// The same as `Ok`, with twelve sites and four PHP versions: the
    /// menu-bar panel lists nine, says how many more there are, and puts the
    /// version buttons on two rows.
    ManySites,
    /// A new install: nothing installed, and PHP 8.4 downloading, 18 of
    /// 31 MB (the first step of the first-run panel).
    FirstRun1,
    /// PHP 8.4 and Caddy installed, no sites yet: the first step is done and
    /// setup is the one to do.
    FirstRun2,
    /// The same as `Ok`, with "li" in the search field of Sites.
    SitesSearch,
    /// The same as `Ok`, with no site: the empty state of Sites.
    SitesEmpty,
    /// The same as `Ok`, with the link form open, a line about the last
    /// action, two parked folders, and two skipped ones.
    SitesExtras,
    /// The same as `Ok`, with `~/Code` parked: the sites in it come from that
    /// folder, so their pages cannot unlink them.
    SiteParked,
    /// The same as `Ok`, with Node.js and Composer listed too, and the Xdebug
    /// state read: the Runtimes screen.
    Runtimes,
    /// `Runtimes`, with PHP 8.5 downloading, 18 of 31 MB.
    RuntimesInstalling,
    /// `Runtimes`, with the install dialog of PHP open.
    RuntimesInstall,
    /// The same as `Ok`, with Start at login on and nothing known of setup:
    /// the Settings screen. Its data folder is a made-up one,
    /// under the demo's home.
    Settings,
    /// `Settings` after a setup of this session: the certificate and the
    /// `.test` names are done.
    SettingsSetup,
    /// The same as `Ok`, with Caddy running and three PHP pools: one running,
    /// one waiting, one failed with its reason: the Services screen.
    Services,
    /// The same as `Ok`, with Caddy running, the pools of PHP 8.3 (waiting)
    /// and 8.4, and a log for each: Logs following Caddy's, with a warning and
    /// an error among the lines.
    Logs,
    /// `Logs` with the Info level turned off: only warnings and errors.
    LogsFiltered,
    /// `Logs` with a filter that no line holds: "No lines match.", and the
    /// chips still there.
    LogsNoMatch,
    /// `Logs` whose PHP 8.4 pool wrote a line wider than the window: open the
    /// pool's log and scroll sideways to read it.
    LogsLong,
    /// The same as `Ok`, with the confirmation of "Unlink shop.test?" open.
    ConfirmUnlink,
    /// `Runtimes`, with the confirmation of uninstalling PHP 8.3.27 open.
    ConfirmUninstall,
    /// The same as `Ok`, with the success notice of an Unlink.
    NoticeSuccess,
    /// The same as `Ok`, with the error notice of a refused Unlink and its
    /// "Forget ~/Code…" action.
    NoticeError,
}

/// The state `MARSA_GUI_DEMO` asks for, if it names one.
pub fn requested() -> Option<Demo> {
    parse(&std::env::var(VAR).ok()?)
}

fn parse(name: &str) -> Option<Demo> {
    Some(match name {
        "ok" => Demo::Ok,
        "caddy-stopped" => Demo::CaddyStopped,
        "many-sites" => Demo::ManySites,
        "first-run-1" => Demo::FirstRun1,
        "first-run-2" => Demo::FirstRun2,
        "sites-search" => Demo::SitesSearch,
        "sites-empty" => Demo::SitesEmpty,
        "sites-extras" => Demo::SitesExtras,
        "site-parked" => Demo::SiteParked,
        "runtimes" => Demo::Runtimes,
        "runtimes-installing" => Demo::RuntimesInstalling,
        "runtimes-install" => Demo::RuntimesInstall,
        "settings" => Demo::Settings,
        "settings-setup" => Demo::SettingsSetup,
        "services" => Demo::Services,
        "logs" => Demo::Logs,
        "logs-filtered" => Demo::LogsFiltered,
        "logs-no-match" => Demo::LogsNoMatch,
        "logs-long" => Demo::LogsLong,
        "confirm-unlink" => Demo::ConfirmUnlink,
        "confirm-uninstall" => Demo::ConfirmUninstall,
        "notice-success" => Demo::NoticeSuccess,
        "notice-error" => Demo::NoticeError,
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
    let php: &[&str] = if demo == Demo::ManySites {
        &["8.2.30", "8.3.27", "8.4.27", "8.5.0"]
    } else {
        &["8.3.27", "8.4.27"]
    };
    model.update(Update::Runtimes(runtimes(php)));
    let caddy = if demo == Demo::CaddyStopped {
        ServiceState::Stopped
    } else {
        ServiceState::Running
    };
    model.update(Update::Services(vec![
        service(WEB_SERVER, caddy),
        service("php-fpm-8.4", ServiceState::Running),
    ]));
    let list = match demo {
        Demo::SitesEmpty => SiteList::default(),
        Demo::ManySites => many_sites(),
        _ => sites(),
    };
    model.update(Update::Sites(list));
    // The menu-bar panel's Xdebug switch is read (the Runtimes demos read it
    // on their own, with the same answer).
    model.update(Update::Xdebug(XdebugState {
        enabled: false,
        loaded: Vec::new(),
        missing: Vec::new(),
    }));
    if demo == Demo::SitesSearch {
        model.sites_search = "li".into();
    }
    if demo == Demo::SitesExtras {
        sites_extras(model);
    }
    if demo == Demo::SiteParked {
        model.session.sites.parked = vec![PathBuf::from("/demo/Code")];
    }
    if matches!(
        demo,
        Demo::Runtimes | Demo::RuntimesInstalling | Demo::RuntimesInstall | Demo::ConfirmUninstall
    ) {
        runtimes_screen(model, demo);
    }
    dialogs_and_notices(model, demo);
    if matches!(demo, Demo::Settings | Demo::SettingsSetup) {
        settings_screen(model, demo);
    }
    if demo == Demo::Services {
        services_screen(model);
    }
    if matches!(
        demo,
        Demo::Logs | Demo::LogsFiltered | Demo::LogsNoMatch | Demo::LogsLong
    ) {
        logs_screen(model, demo);
    }
}

/// What the Logs screen shows beyond `Ok`: the services that have a log,
/// and the lines of Caddy and of the pool of PHP 8.4 (Caddy's as it writes
/// them: one JSON object a line).
fn logs_screen(model: &mut Model, demo: Demo) {
    model.update(Update::Services(vec![
        service(WEB_SERVER, ServiceState::Running),
        service("php-fpm-8.3", ServiceState::Stopped),
        service("php-fpm-8.4", ServiceState::Running),
    ]));
    let line = |service: &str, seq: u64, text: String| LogLine {
        service: service.into(),
        seq,
        at_ms: 0,
        stream: LogStream::Stderr,
        text,
    };
    let caddy = CADDY_LOG.iter().enumerate().map(|(seq, (level, msg))| {
        let text = format!(
            r#"{{"level":"{level}","ts":1763476328.{seq},"logger":"http.log.access","msg":"{msg}"}}"#
        );
        line(WEB_SERVER, seq as u64, text)
    });
    model.update(Update::Log(caddy.collect()));
    let pool = POOL_LOG.iter().enumerate().map(|(seq, (level, msg))| {
        let text = format!("[18-Nov-2026 14:30:0{seq}] {level}: {msg}");
        line("php-fpm-8.4", seq as u64, text)
    });
    model.update(Update::Log(pool.collect()));
    if demo == Demo::LogsFiltered {
        model.apply_action(Action::ToggleLogLevel(LogLevel::Info));
    }
    if demo == Demo::LogsNoMatch {
        model.apply_action(Action::SetLogFilter("no line says this".into()));
    }
    if demo == Demo::LogsLong {
        // A PHP warning with a file name and a line: php-fpm puts it on one
        // line, wider than the window.
        let text = r#"[18-Nov-2026 14:30:09] WARNING: [pool www] child 4261 said into stderr: "NOTICE: PHP message: PHP Warning:  Undefined array key \"price\" in /demo/Code/shop/app/Http/Controllers/CartController.php on line 128""#.to_owned();
        model.update(Update::Log(vec![LogLine {
            service: "php-fpm-8.4".into(),
            seq: POOL_LOG.len() as u64,
            at_ms: 0,
            stream: LogStream::Stderr,
            text,
        }]));
    }
}

/// The lines of Caddy's log in the demo: the level, and the message.
const CADDY_LOG: [(&str, &str); 22] = [
    (
        "info",
        "maxprocs: Leaving GOMAXPROCS=8: CPU quota undefined",
    ),
    ("info", "serving initial configuration"),
    ("info", "started background certificate maintenance"),
    ("info", "GET https://shop.test/ 200 38ms"),
    ("info", "GET https://shop.test/build/app.js 200 4ms"),
    ("info", "GET https://billing-api.test/health 200 9ms"),
    ("info", "POST https://shop.test/cart 200 52ms"),
    ("info", "GET https://wiki.test/ 200 71ms"),
    (
        "warn",
        "tls: certificate for docs.test not requested: site uses HTTP",
    ),
    ("info", "GET https://docs.test/ 200 12ms"),
    ("info", "GET https://shop.test/cart 200 41ms"),
    ("info", "GET https://shop.test/build/app.css 200 3ms"),
    ("info", "POST https://shop.test/login 302 88ms"),
    ("info", "GET https://billing-api.test/invoices 200 57ms"),
    ("error", "POST https://billing-api.test/invoices 500 120ms"),
    ("info", "GET https://wiki.test/ 200 64ms"),
    ("info", "GET https://client-portal.test/dashboard 200 92ms"),
    ("info", "GET https://shop.test/products?page=2 200 57ms"),
    (
        "warn",
        "reverse proxy: slow answer from legacy-crm.test (1.4 s)",
    ),
    ("info", "GET https://shop.test/checkout 200 77ms"),
    ("info", "GET https://docs.test/ 200 12ms"),
    ("info", "reload: config applied, 6 sites"),
];

/// The lines of the log of the PHP 8.4 pool in the demo, as php-fpm writes them.
const POOL_LOG: [(&str, &str); 4] = [
    ("NOTICE", "fpm is running, pid 4251"),
    ("NOTICE", "ready to handle connections"),
    (
        "WARNING",
        "[pool www] child 4260 said into stderr: \"PHP Deprecated: Return type of Foo::bar() should be compatible\"",
    ),
    (
        "ERROR",
        "[pool www] child 4262 exited on signal 11 (SIGSEGV) after 31.4 seconds from start",
    ),
];

/// What the Services screen shows beyond `Ok`: Caddy with its process, the
/// pool of PHP 8.4 running, the one of 8.3 waiting for a site, and the one
/// of 8.2 failed, with a long reason and a log to show.
fn services_screen(model: &mut Model) {
    let mut caddy = service(WEB_SERVER, ServiceState::Running);
    caddy.pid = Some(4242);
    let mut running = service("php-fpm-8.4", ServiceState::Running);
    running.pid = Some(4251);
    model.update(Update::Services(vec![
        caddy,
        running,
        service("php-fpm-8.3", ServiceState::Stopped),
        service(
            "php-fpm-8.2",
            ServiceState::Failed {
                code: "php_fpm_orphan_stuck".into(),
            },
        ),
    ]));
    // The failed pool wrote a line before it stopped, so its row can open it.
    model.update(Update::Log(vec![LogLine {
        service: "php-fpm-8.2".into(),
        seq: 0,
        at_ms: 0,
        stream: LogStream::Stderr,
        text: "[18-Nov-2026 14:29:58] ERROR: unable to bind listening socket".into(),
    }]));
}

/// The confirmations and the notices of the design review.
fn dialogs_and_notices(model: &mut Model, demo: Demo) {
    match demo {
        Demo::ConfirmUnlink => {
            model.ask_removal(Removal::Unlink {
                site: "shop".into(),
            });
        }
        Demo::ConfirmUninstall => {
            model.ask_removal(Removal::Uninstall {
                runtime: Runtime::Php,
                version: "8.3.27".parse().expect("a version"),
            });
        }
        Demo::NoticeSuccess => {
            model
                .session
                .notices
                .set_sites(SitesNotice::Unlinked("old-blog".into()));
        }
        Demo::NoticeError => {
            let folder = PathBuf::from("/demo/Code");
            model.session.sites.parked = vec![folder.clone()];
            model.session.notices.set_sites(SitesNotice::SiteIsParked {
                site: "shop".into(),
                folder: Some(folder),
            });
        }
        _ => {}
    }
}

/// What Settings shows beyond `Ok`: Start at login read, a data folder under
/// the demo's home, and, for `SettingsSetup`, a setup that ran.
fn settings_screen(model: &mut Model, demo: Demo) {
    model.update(Update::Autostart(Autostart::On));
    model.data_dir = PathBuf::from("/demo/Library/Application Support/Marsa");
    if demo == Demo::SettingsSetup {
        let done = |op: &str, status| OpResult {
            op: op.into(),
            status,
            code: None,
            message: None,
        };
        model.update(Update::SetupDone(SetupReport {
            results: vec![
                done("resolver_install", OpStatus::Done),
                done("pf_install", OpStatus::Done),
                done("trust_install", OpStatus::Unchanged),
            ],
            ..SetupReport::default()
        }));
    }
}

/// What the Runtimes screen shows beyond `Ok`: Node.js and Composer, the
/// Xdebug state, and an install or its dialog.
fn runtimes_screen(model: &mut Model, demo: Demo) {
    let row = |runtime, installed: &[&str], default: Option<&str>| RuntimeVersions {
        runtime,
        installed: installed.iter().map(|text| (*text).to_owned()).collect(),
        default: default.map(str::to_owned),
    };
    model.update(Update::Runtimes(RuntimeList {
        runtimes: vec![
            row(Runtime::Php, &["8.3.27", "8.4.27"], Some("8.4")),
            row(Runtime::Node, &["20.18.0", "22.11.0"], Some("22")),
            row(Runtime::Composer, &["2.8.4"], None),
            row(Runtime::Caddy, &["2.11.7"], None),
        ],
    }));
    model.update(Update::Xdebug(XdebugState {
        enabled: false,
        loaded: Vec::new(),
        missing: Vec::new(),
    }));
    match demo {
        Demo::RuntimesInstalling => {
            let job = InstallJob {
                runtime: Runtime::Php,
                version: Some("8.5".to_owned()),
            };
            if model.queue_install(job) {
                downloading(model, Runtime::Php, "8.5.0");
            }
        }
        Demo::RuntimesInstall => {
            model.open_install(Runtime::Php);
        }
        _ => {}
    }
}

/// The parts of Sites the plain demo leaves out.
fn sites_extras(model: &mut Model) {
    model.session.sites.parked = vec![
        PathBuf::from("/demo/Code"),
        PathBuf::from("/demo/Work/acme"),
    ];
    model.session.sites.skipped = vec![
        SkippedFolder {
            path: PathBuf::from("/demo/Code/My Old Site"),
            code: "site_name_invalid".into(),
        },
        SkippedFolder {
            path: PathBuf::from("/demo/Work/acme/portal-copy"),
            code: "site_name_taken".into(),
        },
    ];
    model
        .session
        .notices
        .set_sites(SitesNotice::Unlinked("old-blog".into()));
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
    downloading(model, Runtime::Php, "8.4.27");
}

/// The next queued install, accepted by the daemon and 18 of 31 MB of the
/// download of `version` done.
fn downloading(model: &mut Model, runtime: Runtime, version: &str) {
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
        runtime,
        version: version.into(),
    };
    let download = InstallEvent::Downloading {
        artifact: format!("{}-{version}-macos-arm64.tar.gz", runtime.dir_name()),
        done: 18_000_000,
        total: 31_000_000,
    };
    apply(model, progress(selected));
    apply(model, progress(download));
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

/// The six sites of `sites`, and six more.
fn many_sites() -> SiteList {
    let mut list = sites();
    let more = [
        ("api-gateway", "Code/api-gateway", "8.5.0"),
        ("blog", "Sites/blog", "8.2.30"),
        ("cms", "Work/acme/cms", "8.4.27"),
        ("forum", "Sites/forum", "8.3.27"),
        ("intranet", "Work/acme/intranet", "8.4.27"),
        ("status-page", "Code/status-page", "8.5.0"),
    ];
    for (name, folder, php) in more {
        let root = PathBuf::from("/demo").join(folder);
        list.sites.push(Site {
            name: name.into(),
            docroot: root.join("public"),
            root,
            kind: SiteKind::Laravel,
            php: php.parse().ok(),
            php_source: Some(Source::GlobalDefault),
            secure: true,
        });
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_many_sites_demo_has_twelve_sites_and_four_php_versions() {
        assert_eq!(parse("many-sites"), Some(Demo::ManySites));
        let mut model = Model::default();
        fill(&mut model, Demo::ManySites);
        assert_eq!(model.session.sites.sites.len(), 12);
        assert_eq!(model.php_versions().len(), 4);
        // The panel's switch is read, and nothing starts a daemon.
        assert!(model.session.xdebug.is_some());
        let mut plain = Model::default();
        fill(&mut plain, Demo::Ok);
        assert_eq!(plain.session.sites.sites.len(), 6);
    }

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
        assert_eq!(parse("runtimes"), Some(Demo::Runtimes));
        assert_eq!(parse("runtimes-installing"), Some(Demo::RuntimesInstalling));
        assert_eq!(parse("runtimes-install"), Some(Demo::RuntimesInstall));
        assert_eq!(parse("settings"), Some(Demo::Settings));
        assert_eq!(parse("settings-setup"), Some(Demo::SettingsSetup));
        assert_eq!(parse("confirm-unlink"), Some(Demo::ConfirmUnlink));
        assert_eq!(parse("confirm-uninstall"), Some(Demo::ConfirmUninstall));
        assert_eq!(parse("notice-success"), Some(Demo::NoticeSuccess));
        assert_eq!(parse("notice-error"), Some(Demo::NoticeError));
        assert_eq!(parse("services"), Some(Demo::Services));
        assert_eq!(parse("logs"), Some(Demo::Logs));
        assert_eq!(parse("logs-filtered"), Some(Demo::LogsFiltered));
        assert_eq!(parse("logs-no-match"), Some(Demo::LogsNoMatch));
        assert_eq!(parse("logs-long"), Some(Demo::LogsLong));
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
        assert!(model.session.sites.sites.is_empty());
        assert_eq!(model.php_versions().len(), 1);
    }

    #[test]
    fn the_sites_demos_search_by_name_or_have_no_site_at_all() {
        let mut model = Model::default();
        fill(&mut model, Demo::SitesSearch);
        assert_eq!(model.sites_search, "li");
        assert_eq!(model.session.sites.sites.len(), 6);
        let mut model = Model::default();
        fill(&mut model, Demo::SitesEmpty);
        assert!(model.session.sites.sites.is_empty() && model.is_connected());
    }

    #[test]
    fn the_extras_demo_opens_the_link_form_and_lists_the_folders() {
        let mut model = Model::default();
        fill(&mut model, Demo::SitesExtras);
        assert!(model.link.is_some() && model.session.notices.sites().is_some());
        assert_eq!(model.session.sites.parked.len(), 2);
        assert_eq!(model.session.sites.skipped.len(), 2);
    }

    #[test]
    fn the_parked_demo_gives_the_sites_of_its_folder_a_parked_page() {
        let mut model = Model::default();
        fill(&mut model, Demo::SiteParked);
        let by_name = |name: &str| {
            model
                .session
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
    fn the_runtimes_demos_list_four_runtimes_read_xdebug_and_start_no_daemon() {
        let mut model = Model::default();
        fill(&mut model, Demo::Runtimes);
        assert_eq!(model.session.runtimes.len(), 4);
        assert!(model.session.xdebug.is_some() && !model.installing());
        assert_eq!(model.session.install_dialog, None);
        let mut installing = Model::default();
        fill(&mut installing, Demo::RuntimesInstalling);
        let words = crate::strings::install_status(installing.install_view().expect("an install"));
        assert!(words.starts_with("Downloading PHP 8.5.0"), "{words}");
        let mut asking = Model::default();
        fill(&mut asking, Demo::RuntimesInstall);
        assert_eq!(asking.session.install_dialog, Some(Runtime::Php));
    }

    #[test]
    fn the_settings_demos_read_start_at_login_and_may_have_run_setup() {
        use crate::screens::settings::{Check, view};
        let mut model = Model::default();
        fill(&mut model, Demo::Settings);
        let shown = view(&model);
        assert!(shown.general.autostart.on && shown.general.autostart.off.is_none());
        assert_eq!(
            (shown.setup.cert, shown.setup.names),
            (Check::NotChecked, Check::NotChecked)
        );
        // Under the demo's home the folder shows as `~/…` (a path that starts
        // with `/` is not absolute on Windows, which never shortens it).
        let text = crate::strings::home_path(shown.about.folder, shown.about.home);
        if shown.about.home.is_some_and(std::path::Path::is_absolute) {
            assert!(text.starts_with('~') && text.ends_with("Marsa"), "{text}");
        }
        let mut model = Model::default();
        fill(&mut model, Demo::SettingsSetup);
        let shown = view(&model).setup;
        assert_eq!((shown.cert, shown.names), (Check::Done, Check::Done));
    }

    #[test]
    fn the_services_demo_has_a_running_a_waiting_and_a_failed_pool_beside_caddy() {
        use crate::screens::services::{ServicesView, view};
        use crate::widgets::PillState;
        let mut model = Model::default();
        fill(&mut model, Demo::Services);
        let ServicesView::Rows { rows, pools } = view(&model) else {
            panic!("rows");
        };
        assert!(pools);
        let states: Vec<_> = rows
            .iter()
            .map(|row| (row.name.as_str(), row.state))
            .collect();
        assert_eq!(
            states,
            [
                ("Caddy", PillState::Running),
                ("PHP-FPM 8.2", PillState::Failed),
                ("PHP-FPM 8.3", PillState::Waiting),
                ("PHP-FPM 8.4", PillState::Running),
            ]
        );
        assert!(rows[1].failure.is_some());
        // The failed pool wrote a log: its row can show it. The others cannot.
        assert_eq!(
            rows[1].actions,
            [crate::screens::services::RowButton::ShowLog]
        );
        assert!(rows.iter().enumerate().all(|(n, row)| {
            n == 1
                || !row
                    .actions
                    .contains(&crate::screens::services::RowButton::ShowLog)
        }));
    }

    #[test]
    fn the_logs_demos_have_a_log_for_caddy_and_a_pool_and_the_filtered_one_hides_info() {
        use crate::model::LogSource;
        use crate::screens::logs::{Body, view};
        let mut model = Model::default();
        fill(&mut model, Demo::Logs);
        let all = view(&model, &LogSource::Caddy);
        assert_eq!(all.sources.len(), 3);
        let Body::Lines(lines) = &all.body else {
            panic!("lines");
        };
        assert_eq!(lines.len(), CADDY_LOG.len());
        let errors = (0..lines.len())
            .filter(|n| lines.get(*n).level == LogLevel::Error)
            .count();
        assert_eq!(errors, 1);
        // The pool's log is php-fpm's: the levels come from its own words.
        let pool = view(&model, &LogSource::of("php-fpm-8.4"));
        let Body::Lines(pool) = &pool.body else {
            panic!("lines");
        };
        let levels: Vec<_> = (0..pool.len()).map(|n| pool.get(n).level).collect();
        assert_eq!(
            levels,
            [
                LogLevel::Info,
                LogLevel::Info,
                LogLevel::Warning,
                LogLevel::Error
            ]
        );
        // A filter no line holds: no lines, and the screen says so.
        let mut none = Model::default();
        fill(&mut none, Demo::LogsNoMatch);
        assert_eq!(view(&none, &LogSource::Caddy).body, Body::NoMatch);
        // The long line is wider than the window can hold at 1120.
        let mut long = Model::default();
        fill(&mut long, Demo::LogsLong);
        let long_view = view(&long, &LogSource::of("php-fpm-8.4"));
        let Body::Lines(long_lines) = &long_view.body else {
            panic!("lines");
        };
        assert!(long_lines.widest() > 150, "{}", long_lines.widest());
        let mut filtered = Model::default();
        fill(&mut filtered, Demo::LogsFiltered);
        let view = view(&filtered, &LogSource::Caddy);
        let Body::Lines(lines) = &view.body else {
            panic!("lines");
        };
        assert_eq!(lines.len(), 3);
        assert!(filtered.log_hidden.hides(LogLevel::Info));
    }

    #[test]
    fn the_dialog_and_notice_demos_open_a_confirmation_or_hold_a_result() {
        use crate::shell::{dialogs, notices};
        let mut model = Model::default();
        fill(&mut model, Demo::ConfirmUnlink);
        let view = dialogs::confirm_view(&model).expect("a question");
        assert_eq!(view.title, "Unlink shop.test?");
        let mut model = Model::default();
        fill(&mut model, Demo::ConfirmUninstall);
        let view = dialogs::confirm_view(&model).expect("a question");
        assert_eq!(view.title, "Uninstall PHP 8.3.27?");
        let mut model = Model::default();
        fill(&mut model, Demo::NoticeSuccess);
        let bar = notices::current(&model).expect("a notice");
        assert_eq!(bar.notice.kind, crate::widgets::NoticeKind::Success);
        let mut model = Model::default();
        fill(&mut model, Demo::NoticeError);
        let bar = notices::current(&model).expect("a notice");
        assert_eq!(bar.notice.kind, crate::widgets::NoticeKind::Error);
        assert!(bar.fix.is_some() && bar.notice.action.is_some());
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
            assert_eq!(model.session.sites.sites.len(), 6);
            assert_eq!(model.php_versions().len(), 2);
            assert_eq!(model.session.services.len(), 2);
        }
    }
}
