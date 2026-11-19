//! Home (layout.md, Screens): the status sentence with the request path and
//! its fix when something breaks, the default PHP as large version buttons,
//! and the first sites with Open. "Recent" waits for the events of phase 2.
//!
//! The screen is two functions. [`view`] reads the [`Model`] and decides what
//! Home says: pure, so the tests assert it without a window. [`show`] draws
//! that [`HomeView`] with the widgets and returns what the user did as
//! [`Action`]s (code.md, State). [Laravel: `view` is the controller that
//! builds the data; `show` is the Blade template.]
//!
//! What the window knows. Caddy and the PHP pools: the services table, and
//! the runtimes list for "not installed". DNS: only the hosts file on Linux
//! (`Model::hosts`). On macOS and Windows the window cannot ask the system
//! whether `.test` resolves, so the DNS step shows as not checked and Home
//! never claims it works.

use eframe::egui::{Label, RichText, Ui, vec2};
use marsa_core::runtimes::Runtime;
use marsa_core::types::{ServiceAction, ServiceState};
use semver::Version;

use crate::model::{
    self, Action, Connection, FIRST_PHP, HostsSync, InstallJob, InstallStep, InstallView,
    LogSource, Model, Route, Setup, WEB_SERVER,
};
use crate::strings;
use crate::theme::page::{HOME_OPEN_COLUMN, HOME_PHP_COLUMN};
use crate::theme::{Direction, Theme};
use crate::widgets::table::empty_state;
use crate::widgets::{
    Button, Health, PathStep, Progress, RequestPath, RowHeight, StatusSentence, StepState, Table,
    TableColumn, VersionPick, VersionPicker,
};

/// How many sites Home lists.
const HOME_SITES: usize = 4;

/// What Home says.
#[derive(Debug, PartialEq)]
pub struct HomeView {
    pub status: StatusView,
    /// `None` while there is no daemon to ask.
    pub default_php: Option<DefaultPhp>,
    pub sites: Option<SitesView>,
}

/// The status sentence, and what sits under it when something is broken.
#[derive(Debug, PartialEq)]
pub struct StatusView {
    pub health: Health,
    pub sentence: String,
    pub explanation: String,
    /// The request path, in the order a request takes. Empty unless
    /// something is broken (components.md, Status).
    pub path: Vec<PathPill>,
    pub fixes: Vec<Fix>,
    /// The install a fix started, while it runs.
    pub progress: Option<InstallBar>,
}

/// One step of the request path.
#[derive(Debug, PartialEq)]
pub struct PathPill {
    pub label: String,
    pub state: StepState,
}

/// A button of the error card.
#[derive(Debug, PartialEq)]
pub struct Fix {
    pub label: String,
    pub action: Action,
    /// The fix is the red button; the rest are secondary.
    pub danger: bool,
    /// Why it cannot be pressed now.
    pub disabled: Option<&'static str>,
}

/// The words and the share of the install that runs.
#[derive(Debug, PartialEq)]
pub struct InstallBar {
    pub words: String,
    /// `None` until the daemon reports something to measure.
    pub fraction: Option<f32>,
}

/// The large version picker of the main runtime.
#[derive(Debug, PartialEq)]
pub struct DefaultPhp {
    /// The installed versions, oldest first.
    pub choices: Vec<PhpChoice>,
    /// The index of the version the default picks now.
    pub chosen: Option<usize>,
    /// Why the dashed "+" cannot open the install dialog now.
    pub install_off: Option<&'static str>,
}

#[derive(Debug, PartialEq)]
pub struct PhpChoice {
    pub version: Version,
    /// `8.4` for the newest patch of a line, the whole version for an older
    /// one: the text the daemon would save.
    pub label: String,
}

/// The first sites.
#[derive(Debug, PartialEq)]
pub struct SitesView {
    /// How many sites there are in all.
    pub total: usize,
    pub rows: Vec<SiteRow>,
}

#[derive(Debug, PartialEq)]
pub struct SiteRow {
    pub name: String,
    /// `PHP 8.4`, or a dash for a site with no PHP.
    pub php: String,
}

/// What the window knows of DNS: the hosts file, where Marsa keeps it (Linux).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dns {
    Working,
    /// A site is not in the hosts file.
    Missing,
    NotChecked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Caddy {
    NotInstalled,
    Stopped,
    Failed(String),
    Starting,
    Stopping,
    Running,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Php {
    NotInstalled,
    /// A pool that failed, by its line (`8.3`).
    Failed {
        minor: String,
        code: String,
    },
    /// Installed; `minor` is the line the default picks, once the list is read.
    Ready(Option<String>),
}

impl Php {
    fn minor(&self) -> Option<&str> {
        match self {
            Php::Ready(minor) => minor.as_deref(),
            Php::Failed { minor, .. } => Some(minor),
            Php::NotInstalled => None,
        }
    }
}

/// Decides what Home says. Pure: the same model gives the same view.
pub fn view(model: &Model) -> HomeView {
    let connected = model.is_connected();
    HomeView {
        status: status(model),
        default_php: connected.then(|| default_php(model)),
        sites: connected.then(|| sites(model)),
    }
}

fn status(model: &Model) -> StatusView {
    match &model.connection {
        Connection::Connecting | Connection::Reconnecting => {
            plain(Health::Busy, strings::HOME_CONNECTING)
        }
        Connection::ShuttingDown => plain(Health::Busy, strings::HOME_STOPPING),
        Connection::Failed(_) => {
            let explanation = strings::status_line(&model.connection);
            plain(Health::Error, (strings::HOME_DAEMON_DOWN, &explanation))
        }
        Connection::Running(_) => running(model),
    }
}

/// A sentence with no path and no fix.
fn plain(health: Health, (sentence, explanation): (&str, &str)) -> StatusView {
    StatusView {
        health,
        sentence: sentence.to_owned(),
        explanation: explanation.to_owned(),
        path: Vec::new(),
        fixes: Vec::new(),
        progress: None,
    }
}

/// The daemon runs: the first broken step of the path (DNS, Caddy, the
/// runtime) is the sentence; none broken, the sites work.
fn running(model: &Model) -> StatusView {
    let (dns, caddy, php) = (dns(model), caddy(model), php(model));
    let broken = |(sentence, explanation): (&str, &str), fixes, progress| StatusView {
        health: Health::Error,
        sentence: sentence.to_owned(),
        explanation: explanation.to_owned(),
        path: request_path(model, dns, &caddy, &php),
        fixes,
        progress,
    };
    if dns == Dns::Missing {
        let mut explanation = strings::hosts_waiting(&model.hosts.missing);
        if let HostsSync::Failed(code) = &model.hosts_sync {
            explanation = format!("{explanation} {}", strings::error_text(code));
        }
        let busy = model.hosts_sync == HostsSync::Running || model.setup == Setup::Running;
        let fix = Fix {
            label: strings::ADD_TO_HOSTS.to_owned(),
            action: Action::SyncHosts,
            danger: true,
            disabled: busy.then_some(strings::HOSTS_ADDING),
        };
        return broken((strings::HOME_DNS_MISSING, &explanation), vec![fix], None);
    }
    match &caddy {
        Caddy::NotInstalled => {
            let job = InstallJob {
                runtime: Runtime::Caddy,
                version: None,
            };
            let (fixes, progress, failed) = install_fix(model, strings::INSTALL_CADDY, job);
            let (sentence, explanation) = strings::HOME_CADDY_NOT_INSTALLED;
            let explanation = failed.unwrap_or_else(|| explanation.to_owned());
            return broken((sentence, &explanation), fixes, progress);
        }
        Caddy::Stopped => return broken(strings::HOME_CADDY_STOPPED, caddy_fixes(), None),
        Caddy::Failed(code) => {
            let explanation = strings::error_text(code);
            return broken(
                (strings::HOME_CADDY_FAILED, &explanation),
                caddy_fixes(),
                None,
            );
        }
        Caddy::Starting => return plain(Health::Busy, strings::HOME_CADDY_STARTING),
        Caddy::Stopping => return plain(Health::Busy, strings::HOME_CADDY_STOPPING),
        Caddy::Running => {}
    }
    match &php {
        Php::NotInstalled => {
            let job = InstallJob {
                runtime: Runtime::Php,
                version: Some(FIRST_PHP.to_owned()),
            };
            let (fixes, progress, failed) = install_fix(model, strings::INSTALL_PHP, job);
            let (sentence, explanation) = strings::HOME_PHP_NOT_INSTALLED;
            let explanation = failed.unwrap_or_else(|| explanation.to_owned());
            return broken((sentence, &explanation), fixes, progress);
        }
        // The sites start and stop a pool: no button restarts it.
        Php::Failed { minor, code } => {
            let sentence = strings::home_php_failed(minor);
            return broken((&sentence, &strings::error_text(code)), Vec::new(), None);
        }
        Php::Ready(_) => {}
    }
    let sites = model.sites.sites.len();
    if sites == 0 {
        let explanation = strings::home_no_sites(php.minor());
        return plain(Health::Ok, (strings::HOME_NO_SITES, &explanation));
    }
    let sentence = strings::home_working(sites);
    plain(Health::Ok, (&sentence, &strings::home_running(php.minor())))
}

/// The hosts file is the one DNS state the window reads.
fn dns(model: &Model) -> Dns {
    match (model.hosts.used, model.hosts.missing.is_empty()) {
        (false, _) => Dns::NotChecked,
        (true, true) => Dns::Working,
        (true, false) => Dns::Missing,
    }
}

fn caddy(model: &Model) -> Caddy {
    let service = model
        .services
        .iter()
        .find(|service| service.id == WEB_SERVER)
        .map(|service| &service.state);
    let installed = model
        .runtimes
        .iter()
        .find(|row| row.runtime == Runtime::Caddy)
        .map(|row| !row.installed.is_empty());
    match service {
        Some(ServiceState::Running) => Caddy::Running,
        Some(ServiceState::Starting) => Caddy::Starting,
        // The list is read and holds no Caddy: there is nothing to start.
        _ if installed == Some(false) => Caddy::NotInstalled,
        Some(ServiceState::Stopping) => Caddy::Stopping,
        Some(ServiceState::Failed { code }) => Caddy::Failed(code.clone()),
        Some(ServiceState::Stopped) | None => Caddy::Stopped,
    }
}

/// PHP is broken when a pool failed, or when no version is installed. A pool
/// that is not in the table starts with the first site that needs it.
fn php(model: &Model) -> Php {
    let failed = model.services.iter().find_map(|service| {
        let ServiceState::Failed { code } = &service.state else {
            return None;
        };
        // `php-fpm-8.3`, or `php-cgi-8.3` on Windows: the line is the end.
        let minor = service.id.rsplit('-').next()?;
        model::is_pool(&service.id).then(|| Php::Failed {
            minor: minor.to_owned(),
            code: code.clone(),
        })
    });
    if let Some(failed) = failed {
        return failed;
    }
    let read = model.runtimes.iter().any(|row| row.runtime == Runtime::Php);
    if read && model.php_versions().is_empty() {
        return Php::NotInstalled;
    }
    let shown = model
        .default_choice(Runtime::Php)
        .or_else(|| model.php_versions().last().cloned());
    Php::Ready(shown.as_ref().map(minor))
}

/// `8.4` of `8.4.27`.
fn minor(version: &Version) -> String {
    format!("{}.{}", version.major, version.minor)
}

/// DNS, Caddy, the runtime, and the first site. A step the window cannot
/// know is not claimed, and the site is never reached while another step
/// breaks.
fn request_path(model: &Model, dns: Dns, caddy: &Caddy, php: &Php) -> Vec<PathPill> {
    let pill = |label: String, state| PathPill { label, state };
    let mut path = vec![
        pill(
            strings::PATH_DNS.to_owned(),
            match dns {
                Dns::Working => StepState::Working,
                Dns::Missing => StepState::Broken,
                Dns::NotChecked => StepState::NotChecked,
            },
        ),
        pill(
            strings::PATH_CADDY.to_owned(),
            match caddy {
                Caddy::Running => StepState::Working,
                Caddy::NotInstalled | Caddy::Stopped | Caddy::Failed(_) => StepState::Broken,
                Caddy::Starting | Caddy::Stopping => StepState::Unreached,
            },
        ),
        pill(
            strings::php_line(php.minor()),
            match php {
                Php::Ready(_) => StepState::Working,
                Php::NotInstalled | Php::Failed { .. } => StepState::Broken,
            },
        ),
    ];
    if let Some(site) = model.sites.sites.first() {
        path.push(pill(site.host(), StepState::Unreached));
    }
    path
}

/// "Start Caddy", and the way to see why it stopped.
fn caddy_fixes() -> Vec<Fix> {
    let start = Fix {
        label: strings::START_CADDY.to_owned(),
        action: Action::Service {
            service: WEB_SERVER.to_owned(),
            action: ServiceAction::Start,
        },
        danger: true,
        disabled: None,
    };
    let log = Fix {
        label: strings::SHOW_LOG.to_owned(),
        action: Action::Navigate(Route::Logs(LogSource::Caddy)),
        danger: false,
        disabled: None,
    };
    vec![start, log]
}

/// The fix that installs `job`, the bar while it runs, and why the last
/// install of that program failed.
fn install_fix(
    model: &Model,
    label: &str,
    job: InstallJob,
) -> (Vec<Fix>, Option<InstallBar>, Option<String>) {
    let busy = model.installing();
    let view = model
        .install_view()
        .filter(|view| view.runtime == Some(job.runtime));
    let progress = view.filter(|_| busy).map(install_bar);
    let failed = view
        .filter(|view| matches!(view.step, InstallStep::Failed(_)))
        .map(strings::install_status);
    let fix = Fix {
        label: label.to_owned(),
        action: Action::Install(job),
        danger: true,
        disabled: busy.then_some(strings::INSTALL_BUSY_HINT),
    };
    (vec![fix], progress, failed)
}

/// The words and the share of an install. Also the first-run panel's.
pub(super) fn install_bar(view: &InstallView) -> InstallBar {
    InstallBar {
        words: strings::install_status(view),
        fraction: match view.step {
            InstallStep::Waiting | InstallStep::ListChecked => None,
            _ => Some(view.step.fraction()),
        },
    }
}

fn default_php(model: &Model) -> DefaultPhp {
    let installed = model.php_versions();
    let choices = php_choices(model);
    let chosen = model
        .default_choice(Runtime::Php)
        .and_then(|default| installed.iter().position(|version| *version == default));
    DefaultPhp {
        choices,
        chosen,
        install_off: (!model.can_install()).then_some(strings::INSTALL_BUSY_HINT),
    }
}

/// The installed PHP versions, oldest first, with the text each is saved and
/// shown as. The Sites screen's version menus list them too.
pub(super) fn php_choices(model: &Model) -> Vec<PhpChoice> {
    let installed = model.php_versions();
    installed
        .iter()
        .map(|version| PhpChoice {
            version: version.clone(),
            label: model::version_text(Runtime::Php, version, installed),
        })
        .collect()
}

fn sites(model: &Model) -> SitesView {
    let rows = model
        .sites
        .sites
        .iter()
        .take(HOME_SITES)
        .map(|site| SiteRow {
            name: site.name.clone(),
            // The column header says PHP, so the cell is the version only,
            // as in the Sites table.
            php: site
                .php
                .as_ref()
                .map_or_else(|| strings::NO_PHP.to_owned(), minor),
        })
        .collect();
    SitesView {
        total: model.sites.sites.len(),
        rows,
    }
}

/// Draws `view`. Returns what the user did.
pub fn show(ui: &mut Ui, view: &HomeView, theme: &Theme) -> Vec<Action> {
    let mut actions = Vec::new();
    ui.spacing_mut().item_spacing.y = theme.space.section_gap;
    status_card(ui, theme, &view.status, &mut actions);
    if let Some(php) = &view.default_php {
        default_php_section(ui, theme, php, &mut actions);
    }
    if let Some(sites) = &view.sites {
        sites_section(ui, theme, sites, &mut actions);
    }
    actions
}

fn status_card(ui: &mut Ui, theme: &Theme, status: &StatusView, actions: &mut Vec<Action>) {
    let card = StatusSentence::new(status.health, &status.sentence, &status.explanation);
    if status.path.is_empty() && status.fixes.is_empty() && status.progress.is_none() {
        ui.add(card);
        return;
    }
    card.show_with(ui, |ui| {
        ui.spacing_mut().item_spacing.x = theme.space.group_gap_max;
        ui.horizontal_wrapped(|ui| {
            if !status.path.is_empty() {
                let steps: Vec<PathStep> = status
                    .path
                    .iter()
                    .map(|pill| PathStep {
                        label: &pill.label,
                        state: pill.state,
                    })
                    .collect();
                ui.add(RequestPath::new(strings::REQUEST_PATH, &steps));
            }
            for fix in &status.fixes {
                let button = if fix.danger {
                    Button::danger(&fix.label)
                } else {
                    Button::secondary(&fix.label)
                };
                let button = match fix.disabled {
                    Some(reason) => button.disabled(reason),
                    None => button,
                };
                if ui.add(button).clicked() {
                    actions.push(fix.action.clone());
                }
            }
        });
        if let Some(bar) = &status.progress {
            ui.add(Progress::new(&bar.words, bar.fraction));
        }
    });
}

fn default_php_section(ui: &mut Ui, theme: &Theme, php: &DefaultPhp, actions: &mut Vec<Action>) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = theme.space.group_gap_max;
        let hint = strings::DEFAULT_RUNTIME_HINT;
        super::heading(ui, theme, strings::DEFAULT_PHP, Some(hint));
        if php.choices.is_empty() {
            let text = RichText::new(strings::NO_PHP_YET)
                .font(theme.text.body_sm.font_id())
                .color(theme.color.ink2);
            ui.add(Label::new(text).selectable(false));
        }
        let versions: Vec<(usize, &str)> = php
            .choices
            .iter()
            .enumerate()
            .map(|(index, choice)| (index, choice.label.as_str()))
            .collect();
        let install = strings::install_another_label(strings::runtime_name(Runtime::Php));
        let mut picker =
            VersionPicker::new(strings::DEFAULT_PHP, &versions, php.chosen).install(&install);
        if let Some(reason) = php.install_off {
            picker = picker.install_disabled(reason);
        }
        match picker.show(ui).inner {
            Some(VersionPick::Version(index)) => {
                if let Some(choice) = php.choices.get(index) {
                    actions.push(Action::SetDefault {
                        runtime: Runtime::Php,
                        version: choice.version.clone(),
                    });
                }
            }
            Some(VersionPick::Install) => actions.push(Action::AskInstall(Runtime::Php)),
            None => {}
        }
    });
}

fn sites_section(ui: &mut Ui, theme: &Theme, sites: &SitesView, actions: &mut Vec<Action>) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = theme.space.group_gap_max;
        // The heading and the link share one line as high as a control.
        let line = vec2(ui.available_width(), theme.size.control);
        ui.allocate_ui_with_layout(line, Direction::CURRENT.row(), |ui| {
            super::heading(ui, theme, strings::SITES, None);
            ui.with_layout(Direction::CURRENT.row_from_end(), |ui| {
                if sites.total > 0 {
                    let all = strings::all_sites(sites.total);
                    if ui.add(Button::quiet(&all)).clicked() {
                        actions.push(Action::Navigate(Route::Sites));
                    }
                }
            });
        });
        if sites.rows.is_empty() {
            if empty_state(ui, strings::HOME_SITES_EMPTY, strings::ADD_SITE) {
                actions.push(Action::Navigate(Route::Sites));
            }
            return;
        }
        let columns = [
            TableColumn::fill(strings::COLUMN_NAME),
            TableColumn::fixed(strings::COLUMN_PHP, HOME_PHP_COLUMN),
            TableColumn::fixed("", HOME_OPEN_COLUMN),
        ];
        let table = Table::new("home_sites", strings::SITES, &columns).rows(RowHeight::OneLine);
        let shown = table.show(ui, sites.rows.len(), |row| {
            let site = &sites.rows[row.index()];
            row.cell(|ui| {
                let name = RichText::new(&site.name)
                    .font(theme.text.body_strong.font_id())
                    .color(theme.color.ink);
                ui.add(Label::new(name).truncate().selectable(false));
            });
            row.cell(|ui| {
                let php = RichText::new(&site.php)
                    .font(theme.text.caption.font_id())
                    .color(theme.color.mute);
                ui.add(Label::new(php).selectable(false));
            });
            row.cell(|ui| {
                if ui.add(Button::secondary(strings::OPEN).in_row()).clicked() {
                    actions.push(Action::OpenSite(site.name.clone()));
                }
            });
        });
        if let Some(index) = shown.clicked {
            let name = sites.rows[index].name.clone();
            actions.push(Action::Navigate(Route::Site(name)));
        }
    });
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use eframe::egui::{self, Pos2, pos2};
    use marsa_core::types::{
        Health as DaemonHealth, HostsStatus, RuntimeList, RuntimeVersions, ServiceStatus, Site,
        SiteKind, SiteList,
    };

    use super::*;
    use crate::model::{Update, apply};
    use crate::theme::LIGHT;
    use crate::widgets::harness::{Harness, Tree};

    fn connected() -> Model {
        let mut model = Model::default();
        model.update(Update::Connected(DaemonHealth {
            app_version: "0.0.0".into(),
            api_version: 1,
            pid: 7,
        }));
        model
    }

    /// Runtimes read: these PHP and Caddy versions, `8.4` the PHP default.
    fn runtimes(php: &[&str], caddy: &[&str]) -> RuntimeList {
        let row = |runtime, installed: &[&str], default: Option<&str>| RuntimeVersions {
            runtime,
            installed: installed.iter().map(|text| (*text).to_owned()).collect(),
            default: default.map(str::to_owned),
        };
        RuntimeList {
            runtimes: vec![
                row(Runtime::Php, php, Some("8.4")),
                row(Runtime::Caddy, caddy, None),
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

    fn site(name: &str, php: Option<&str>) -> Site {
        let root = PathBuf::from("/code").join(name);
        Site {
            name: name.into(),
            docroot: root.join("public"),
            root,
            kind: SiteKind::Laravel,
            php: php.map(|text| text.parse().expect("a version")),
            php_source: None,
            secure: true,
        }
    }

    /// PHP 8.4 and Caddy installed, Caddy in `caddy`, and these sites.
    fn model(caddy: ServiceState, sites: &[&str]) -> Model {
        let mut model = connected();
        model.update(Update::Runtimes(runtimes(&["8.4.27"], &["2.11.7"])));
        model.update(Update::Services(vec![service(WEB_SERVER, caddy)]));
        let sites = sites
            .iter()
            .map(|name| site(name, Some("8.4.27")))
            .collect();
        model.update(Update::Sites(SiteList {
            sites,
            ..SiteList::default()
        }));
        model
    }

    fn working(sites: &[&str]) -> Model {
        model(ServiceState::Running, sites)
    }

    fn states(status: &StatusView) -> Vec<(&str, StepState)> {
        let path = status.path.iter();
        path.map(|pill| (pill.label.as_str(), pill.state)).collect()
    }

    fn labels(status: &StatusView) -> Vec<&str> {
        status.fixes.iter().map(|fix| fix.label.as_str()).collect()
    }

    #[test]
    fn everything_working_says_so_and_shows_no_path() {
        let status = view(&working(&["billing", "shop"])).status;
        assert_eq!(status.health, Health::Ok);
        assert_eq!(status.sentence, "All 2 sites are working.");
        assert_eq!(status.explanation, "Caddy is running and PHP 8.4 is ready.");
        assert!(status.path.is_empty() && status.fixes.is_empty());
        assert!(status.progress.is_none());
        assert_eq!(
            view(&working(&["shop"])).status.sentence,
            "Your site is working."
        );
    }

    #[test]
    fn caddy_stopped_names_the_step_and_offers_start_and_the_log() {
        let status = view(&model(ServiceState::Stopped, &["shop"])).status;
        assert_eq!(status.health, Health::Error);
        assert_eq!(status.sentence, "Sites are not opening: Caddy stopped.");
        // The window cannot ask whether `.test` resolves: DNS is not claimed.
        assert_eq!(
            states(&status),
            [
                ("DNS", StepState::NotChecked),
                ("Caddy", StepState::Broken),
                ("PHP 8.4", StepState::Working),
                ("shop.test", StepState::Unreached),
            ]
        );
        assert_eq!(labels(&status), ["Start Caddy", "Show log"]);
        let start = Action::Service {
            service: "caddy".into(),
            action: ServiceAction::Start,
        };
        assert_eq!(status.fixes[0].action, start);
        assert!(status.fixes[0].danger && status.fixes[0].disabled.is_none());
        let log = Action::Navigate(Route::Logs(LogSource::Caddy));
        assert_eq!(status.fixes[1].action, log);
        assert!(!status.fixes[1].danger);
    }

    #[test]
    fn caddy_that_failed_says_why_and_still_offers_start() {
        let state = ServiceState::Failed {
            code: "port_in_use".into(),
        };
        let status = view(&model(state, &["shop"])).status;
        assert_eq!(status.sentence, "Sites are not opening: Caddy failed.");
        assert_eq!(status.explanation, strings::error_text("port_in_use"));
        assert_eq!(labels(&status), ["Start Caddy", "Show log"]);
    }

    #[test]
    fn caddy_starting_or_stopping_is_busy_with_no_error() {
        for (state, health) in [
            (ServiceState::Starting, Health::Busy),
            (ServiceState::Stopping, Health::Busy),
        ] {
            let status = view(&model(state, &["shop"])).status;
            assert_eq!(status.health, health);
            assert!(status.path.is_empty() && status.fixes.is_empty());
        }
    }

    #[test]
    fn dns_that_does_not_answer_is_the_broken_step_on_the_hosts_way() {
        let mut model = working(&["shop"]);
        let hosts = HostsStatus {
            used: true,
            missing: vec!["shop.test".into()],
        };
        model.update(Update::Hosts(hosts));
        let status = view(&model).status;
        assert_eq!(status.health, Health::Error);
        assert_eq!(status.sentence, strings::HOME_DNS_MISSING);
        assert_eq!(
            status.explanation,
            strings::hosts_waiting(&["shop.test".into()])
        );
        assert_eq!(states(&status)[0], ("DNS", StepState::Broken));
        assert_eq!(states(&status)[1], ("Caddy", StepState::Working));
        assert_eq!(labels(&status), ["Add to /etc/hosts"]);
        assert_eq!(status.fixes[0].action, Action::SyncHosts);
        assert!(status.fixes[0].disabled.is_none());

        // While it waits for the password the button is off.
        assert!(model.start_hosts_sync());
        let waiting = view(&model).status;
        assert_eq!(waiting.fixes[0].disabled, Some(strings::HOSTS_ADDING));
        // The names went in: the sites work, and DNS is known to work.
        model.update(Update::Hosts(HostsStatus {
            used: true,
            missing: Vec::new(),
        }));
        assert_eq!(view(&model).status.health, Health::Ok);
    }

    #[test]
    fn working_does_not_claim_dns_and_a_stopped_caddy_shows_it_not_checked() {
        let status = view(&working(&["shop"])).status;
        assert!(
            !status.explanation.contains("DNS"),
            "{}",
            status.explanation
        );
        let status = view(&model(ServiceState::Stopped, &["shop"])).status;
        assert_eq!(states(&status)[0], ("DNS", StepState::NotChecked));
    }

    #[test]
    fn no_php_installed_offers_to_install_it_and_the_picker_offers_the_plus() {
        let mut model = working(&[]);
        model.update(Update::Runtimes(runtimes(&[], &["2.11.7"])));
        let home = view(&model);
        assert_eq!(home.status.health, Health::Error);
        assert_eq!(
            home.status.sentence,
            "Sites are not opening: PHP is not installed."
        );
        assert_eq!(labels(&home.status), ["Install PHP 8.4"]);
        let job = InstallJob {
            runtime: Runtime::Php,
            version: Some("8.4".into()),
        };
        assert_eq!(home.status.fixes[0].action, Action::Install(job));
        assert_eq!(states(&home.status)[2], ("PHP", StepState::Broken));
        let php = home.default_php.expect("the section shows");
        assert!(php.choices.is_empty() && php.chosen.is_none());
    }

    #[test]
    fn an_install_that_runs_shows_its_bar_and_turns_the_fix_off() {
        let mut model = working(&[]);
        model.update(Update::Runtimes(runtimes(&["8.4.27"], &[])));
        model.update(Update::Services(Vec::new()));
        let status = view(&model).status;
        assert_eq!(status.sentence, strings::HOME_CADDY_NOT_INSTALLED.0);
        assert_eq!(labels(&status), ["Install Caddy"]);
        assert!(status.progress.is_none());

        assert!(model.apply_action(status.fixes[0].action.clone()).is_none());
        let job = model.next_install().expect("the fix queued the job");
        assert_eq!(job.runtime, Runtime::Caddy);
        let status = view(&model).status;
        assert_eq!(status.fixes[0].disabled, Some(strings::INSTALL_BUSY_HINT));
        let bar = status.progress.expect("the install shows");
        assert_eq!(bar.words, "Asking the daemon to install Caddy…");
        assert_eq!(bar.fraction, None);

        // A refused install says why, where the sentence explains.
        model.update(Update::InstallRefused {
            job,
            code: "install_busy".into(),
        });
        let status = view(&model).status;
        assert!(status.progress.is_none());
        assert!(status.explanation.starts_with("Cannot install Caddy."));
        assert!(status.fixes[0].disabled.is_none(), "a new try is allowed");
    }

    #[test]
    fn a_failed_pool_names_its_php_and_a_stopped_pool_is_no_fault() {
        let mut model = working(&["shop"]);
        let failed = ServiceState::Failed {
            code: "service_failed".into(),
        };
        model.update(Update::Services(vec![
            service(WEB_SERVER, ServiceState::Running),
            service("php-fpm-8.3", failed),
        ]));
        let status = view(&model).status;
        assert_eq!(
            status.sentence,
            "PHP 8.3 failed: its sites are not opening."
        );
        assert_eq!(states(&status)[2], ("PHP 8.3", StepState::Broken));
        // The sites start and stop a pool: nothing to press.
        assert!(status.fixes.is_empty());

        // On Windows the pool is `php-cgi`.
        let failed = ServiceState::Failed {
            code: "service_failed".into(),
        };
        apply(
            &mut model,
            marsa_core::types::Event::ServiceChanged(service("php-fpm-8.3", ServiceState::Stopped)),
        );
        assert_eq!(view(&model).status.health, Health::Ok);
        apply(
            &mut model,
            marsa_core::types::Event::ServiceChanged(service("php-cgi-8.3", failed)),
        );
        assert_eq!(states(&view(&model).status)[2].0, "PHP 8.3");
    }

    #[test]
    fn without_the_daemon_home_says_why_and_shows_no_sections() {
        let mut model = Model::default();
        let home = view(&model);
        assert_eq!(home.status.health, Health::Busy);
        assert!(home.default_php.is_none() && home.sites.is_none());

        model.update(Update::Failed("daemon_start_failed".into()));
        let home = view(&model);
        assert_eq!(home.status.health, Health::Error);
        assert_eq!(home.status.sentence, strings::HOME_DAEMON_DOWN);
        assert_eq!(
            home.status.explanation,
            strings::status_line(&model.connection)
        );
        assert!(home.status.path.is_empty() && home.status.fixes.is_empty());

        let mut model = working(&["shop"]);
        model.update(Update::Event(marsa_core::types::Event::ShuttingDown));
        assert_eq!(view(&model).status.health, Health::Busy);
    }

    #[test]
    fn no_sites_yet_is_not_an_error() {
        let home = view(&working(&[]));
        assert_eq!(home.status.health, Health::Ok);
        assert_eq!(home.status.sentence, "No sites yet.");
        assert!(home.status.explanation.contains("Park a folder"));
        let sites = home.sites.expect("the section shows");
        assert!(sites.rows.is_empty() && sites.total == 0);
    }

    #[test]
    fn home_lists_the_first_four_sites_with_their_php() {
        let names = ["a", "b", "c", "d", "e", "f"];
        let mut model = working(&names);
        let mut list = model.sites.clone();
        list.sites[1].php = None;
        list.sites[2].php = Some("8.3.27".parse().expect("a version"));
        model.update(Update::Sites(list));
        let sites = view(&model).sites.expect("the section shows");
        assert_eq!(sites.total, 6);
        let rows: Vec<_> = sites
            .rows
            .iter()
            .map(|row| (row.name.as_str(), row.php.as_str()))
            .collect();
        assert_eq!(rows, [("a", "8.4"), ("b", "–"), ("c", "8.3"), ("d", "8.4")]);
    }

    #[test]
    fn the_default_picker_marks_the_default_and_names_an_older_patch_in_full() {
        let mut model = working(&["shop"]);
        model.update(Update::Runtimes(runtimes(
            &["8.3.27", "8.4.23", "8.4.27"],
            &["2.11.7"],
        )));
        let php = view(&model).default_php.expect("the section shows");
        let labels: Vec<_> = php.choices.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["8.3", "8.4.23", "8.4"]);
        assert_eq!(php.chosen, Some(2));
    }

    // -- show --

    /// The middle of the widget a screen reader knows as `label`.
    fn center(tree: &Tree, label: &str) -> Pos2 {
        let node = tree
            .node_with_label(label)
            .or_else(|| tree.node_with_text(label))
            .expect(label);
        let bounds = node.bounds().expect("a node with bounds");
        pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        )
    }

    fn draw(ui: &mut egui::Ui, model: &Model) -> Vec<Action> {
        show(ui, &view(model), &LIGHT)
    }

    /// One click on the widget named `label`; what Home returned.
    fn click(model: &Model, label: &str) -> Vec<Action> {
        let harness = Harness::new(egui::Theme::Light);
        let tree = harness.accessibility(|ui| {
            draw(ui, model);
        });
        let mut actions = Vec::new();
        harness.click(center(&tree, label), |ui| actions.extend(draw(ui, model)));
        actions
    }

    #[test]
    fn the_fix_and_show_log_return_their_actions() {
        let model = model(ServiceState::Stopped, &["shop"]);
        let fixes = view(&model).status.fixes;
        assert_eq!(click(&model, "Start Caddy"), [fixes[0].action.clone()]);
        assert_eq!(click(&model, "Show log"), [fixes[1].action.clone()]);
    }

    #[test]
    fn an_error_is_an_alert_with_its_path_for_a_screen_reader() {
        let harness = Harness::new(egui::Theme::Light);
        let model = model(ServiceState::Stopped, &["shop"]);
        let tree = harness.accessibility(|ui| {
            draw(ui, &model);
        });
        let alert = tree.node_with_label("Sites are not opening: Caddy stopped.");
        assert_eq!(alert.expect("the sentence").role(), egui::Role::Alert);
        assert!(tree.node_with_label("Caddy, not working").is_some());
        assert!(tree.node_with_label("DNS, not checked").is_some());
        let ok = Harness::new(egui::Theme::Light).accessibility(|ui| {
            draw(ui, &working(&["shop"]));
        });
        assert!(ok.node_with_label(strings::REQUEST_PATH).is_none());
    }

    #[test]
    fn the_default_php_hint_is_the_one_the_runtimes_screen_says() {
        let harness = Harness::new(egui::Theme::Light);
        let model = working(&["shop"]);
        let tree = harness.accessibility(|ui| {
            draw(ui, &model);
        });
        // One label holds the heading and its hint (`screens::heading`).
        let words = format!("{}{}", strings::DEFAULT_PHP, strings::DEFAULT_RUNTIME_HINT);
        assert!(tree.node_with_text(&words).is_some(), "{words}");
        assert_eq!(
            strings::DEFAULT_RUNTIME_HINT,
            "used where a project does not ask for a version"
        );
    }

    #[test]
    fn a_version_button_sets_the_default_and_the_plus_opens_install() {
        let mut model = working(&["shop"]);
        model.update(Update::Runtimes(runtimes(
            &["8.3.27", "8.4.27"],
            &["2.11.7"],
        )));
        let set = Action::SetDefault {
            runtime: Runtime::Php,
            version: "8.3.27".parse().expect("a version"),
        };
        assert_eq!(click(&model, "8.3"), [set]);
        let install = strings::install_another_label("PHP");
        let install_php = || Action::AskInstall(Runtime::Php);
        assert_eq!(click(&model, &install), [install_php()]);
        // With no PHP, the dashed button is all there is.
        model.update(Update::Runtimes(runtimes(&[], &["2.11.7"])));
        assert_eq!(click(&model, &install), [install_php()]);
        // The action opens the install dialog of PHP, where the user is.
        assert!(model.apply_action(install_php()).is_none());
        assert_eq!(model.install_dialog, Some(Runtime::Php));
        assert_eq!(model.route, Route::Home);
    }

    #[test]
    fn while_an_install_runs_the_plus_is_off_with_the_reason_and_does_nothing() {
        let mut model = working(&["shop"]);
        assert!(model.queue_install(crate::model::InstallJob {
            runtime: Runtime::Caddy,
            version: None,
        }));
        let view = view(&model);
        let php = view.default_php.as_ref().expect("the picker");
        assert_eq!(php.install_off, Some(strings::INSTALL_BUSY_HINT));
        assert!(click(&model, &strings::install_another_label("PHP")).is_empty());
    }

    #[test]
    fn open_opens_the_site_and_a_row_click_opens_its_page() {
        let model = working(&["billing", "shop"]);
        assert_eq!(
            click(&model, "Open"),
            [Action::OpenSite("billing".into())],
            "the button is not also a row click"
        );
        assert_eq!(
            click(&model, "shop"),
            [Action::Navigate(Route::Site("shop".into()))]
        );
        assert_eq!(
            click(&model, "All 2 sites"),
            [Action::Navigate(Route::Sites)]
        );
    }

    #[test]
    fn with_no_sites_the_list_offers_to_add_one() {
        let model = working(&[]);
        assert_eq!(
            click(&model, strings::ADD_SITE),
            [Action::Navigate(Route::Sites)]
        );
    }

    #[test]
    fn the_rows_are_as_high_as_one_line_rows() {
        let harness = Harness::new(egui::Theme::Light);
        let model = working(&["billing", "shop"]);
        let tree = harness.accessibility(|ui| {
            draw(ui, &model);
        });
        let top = |name: &str| {
            let node = tree.node_with_text(name).expect(name);
            node.bounds().expect("a place").y0
        };
        let step = top("shop") - top("billing");
        assert!((step - f64::from(LIGHT.size.row)).abs() < 0.5, "{step}");
    }
}
