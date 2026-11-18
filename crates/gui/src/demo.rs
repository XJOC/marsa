//! Debug builds only: fixed fake data for the design review (task M5-D5).
//! `MARSA_GUI_DEMO=ok|caddy-stopped|first-run-1` fills the model with a few
//! sites, PHP and Caddy (or nothing, for the first run), and starts no bridge
//! and no daemon: the window draws a state the review needs without a
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
    ServiceState, ServiceStatus, Site, SiteKind, SiteList,
};

use crate::model::{Model, Update, WEB_SERVER, apply};

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
    if demo == Demo::FirstRun1 {
        return first_run_downloading(model);
    }
    model.update(Update::Runtimes(runtimes(&["8.3.27", "8.4.27"])));
    let caddy = match demo {
        Demo::CaddyStopped => ServiceState::Stopped,
        Demo::Ok | Demo::FirstRun1 => ServiceState::Running,
    };
    model.update(Update::Services(vec![
        service(WEB_SERVER, caddy),
        service("php-fpm-8.4", ServiceState::Running),
    ]));
    model.update(Update::Sites(sites()));
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

fn sites() -> SiteList {
    let site = |name: &str, php: &str| {
        let root = PathBuf::from("/demo/Code").join(name);
        Site {
            name: name.into(),
            docroot: root.join("public"),
            root,
            kind: SiteKind::Laravel,
            php: php.parse().ok(),
            php_source: None,
            secure: true,
        }
    };
    SiteList {
        sites: vec![
            site("shop", "8.4.27"),
            site("billing-api", "8.4.27"),
            site("legacy-crm", "8.3.27"),
            site("docs", "8.4.27"),
            site("client-portal", "8.4.27"),
            site("wiki", "8.3.27"),
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
    fn the_demo_connects_and_fills_the_lists_with_no_daemon() {
        for demo in [Demo::Ok, Demo::CaddyStopped] {
            let mut model = Model::default();
            fill(&mut model, demo);
            assert!(model.is_connected());
            assert_eq!(model.sites.sites.len(), 6);
            assert_eq!(model.php_versions().len(), 2);
            assert_eq!(model.services.len(), 2);
        }
    }
}
