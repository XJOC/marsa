//! `marsa queue on|off|status [<site>]` and `marsa schedule on|off|status
//! [<site>]`: a Laravel site's `php artisan queue:work` and `php artisan
//! schedule:work`. One command for both, given its [`WorkerKind`].
//!
//! `on` and `off` are one `PATCH /v1/sites/{name}` each, and return once the
//! setting is saved: `marsad` starts or stops the worker after that.
//! `status` reads the sites and the services. A worker is the service
//! `queue-<site>`, so `marsa logs queue-shop` and `marsa services restart
//! queue-shop` reach it too.

use std::path::Path;
use std::process::ExitCode;

use marsa_core::client::{ClientError, DaemonClient};
use marsa_core::codes;
use marsa_core::config::ConfigPaths;
use marsa_core::types::{ServiceId, ServiceStatus, Site, SiteKey, SiteKind, SiteList, WorkerKind};

use super::Failure;
use super::daemon::{connect, failure};
use super::services::failed_lines;
use super::sites::{checked, local};
use super::strings::{self, Details};
use super::text::columns;

/// The arguments of `marsa queue` and `marsa schedule`.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct WorkerArgs {
    #[command(subcommand)]
    pub command: WorkerCommand,
}

/// The commands of `marsa queue` and `marsa schedule`.
#[derive(Debug, Clone, PartialEq, Eq, clap::Subcommand)]
pub enum WorkerCommand {
    /// Run it for a Laravel site. Marsa restarts it if it crashes, and starts
    /// it again when Marsa starts.
    On {
        /// The site name.
        site: String,
    },
    /// Stop it for a site.
    Off {
        /// The site name.
        site: String,
    },
    /// Show it for every Laravel site, or for one.
    Status {
        /// The site name. Default: every Laravel site.
        site: Option<String>,
    },
}

/// What a command asks the daemon, with the site name checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Switch { site: SiteKey, on: bool },
    Status { site: Option<SiteKey> },
}

impl WorkerCommand {
    /// The request of this command.
    ///
    /// # Errors
    ///
    /// `site_name_invalid` for a name no site can have, before any call.
    pub fn request(&self) -> Result<Request, Failure> {
        Ok(match self {
            WorkerCommand::On { site } => Request::Switch {
                site: key(site)?,
                on: true,
            },
            WorkerCommand::Off { site } => Request::Switch {
                site: key(site)?,
                on: false,
            },
            WorkerCommand::Status { site } => Request::Status {
                site: site.as_deref().map(key).transpose()?,
            },
        })
    }
}

/// Starts the daemon if needed, sends the command, and prints the answer.
/// Exit code 1 if the arguments or a call fail.
pub async fn run(
    kind: WorkerKind,
    args: &WorkerArgs,
    data_dir: &Path,
) -> Result<ExitCode, Failure> {
    let request = args.command.request()?;
    let client = connect(data_dir).await?;
    let text = match request {
        Request::Switch { site, on } => switch(&client, kind, site, on, data_dir).await?,
        Request::Status { site } => {
            let list = client.sites().await;
            let list = list.map_err(|err| failure(&err, data_dir, None))?;
            let services = client.services().await;
            let services = services.map_err(|err| failure(&err, data_dir, None))?;
            render_status(kind, site, &list, &services, data_dir)?
        }
    };
    print!("{text}");
    Ok(ExitCode::SUCCESS)
}

async fn switch(
    client: &DaemonClient,
    kind: WorkerKind,
    site: SiteKey,
    on: bool,
    data_dir: &Path,
) -> Result<String, Failure> {
    let id = ServiceId::Worker(kind, site);
    let list = client.set_worker(site.as_str(), kind, on).await;
    let list = list.map_err(|err| worker_failure(&err, data_dir, id))?;
    // One read, not a wait: the daemon may not have listed the worker yet,
    // and then it is starting.
    let services = if on {
        client.services().await.unwrap_or_else(|err| {
            tracing::debug!(code = err.code(), error = %err, "no services after the change");
            Vec::new()
        })
    } else {
        Vec::new()
    };
    render_switch(kind, site, &list, &services, data_dir)
}

/// The line for the daemon's answer to `on` or `off`, from the site's
/// setting in `list`. For `on`, the worker's state too, and why it failed if
/// it did.
///
/// # Errors
///
/// `site_not_found` when `list` has no site `site`.
pub fn render_switch(
    kind: WorkerKind,
    site: SiteKey,
    list: &SiteList,
    services: &[ServiceStatus],
    data_dir: &Path,
) -> Result<String, Failure> {
    let found = find_site(list, site)?;
    if !found.wants(kind) {
        return Ok(format!(
            "{}\n",
            strings::worker_turned_off(kind, site.as_str())
        ));
    }
    let status = find_status(services, kind, site);
    let state = state(kind, found, status);
    let mut text = strings::worker_turned_on(kind, site.as_str(), state);
    text.push('\n');
    text.push_str(&failed_lines(status, data_dir));
    Ok(text)
}

/// The table of the workers of `kind`: of every Laravel site, or of `site`.
/// Under it, why each failed worker failed.
///
/// # Errors
///
/// `site_not_found` when `list` has no site `site`, and `site_not_laravel`
/// when that site is not Laravel and its setting is off.
pub fn render_status(
    kind: WorkerKind,
    site: Option<SiteKey>,
    list: &SiteList,
    services: &[ServiceStatus],
    data_dir: &Path,
) -> Result<String, Failure> {
    let shown: Vec<&Site> = match site {
        Some(site) => {
            let found = find_site(list, site)?;
            if found.kind != SiteKind::Laravel && !found.wants(kind) {
                return Err(site_failure(codes::SITE_NOT_LARAVEL, site));
            }
            vec![found]
        }
        // A site that lost its `artisan` file keeps its setting: it is shown,
        // with no worker.
        None => list
            .sites
            .iter()
            .filter(|site| site.kind == SiteKind::Laravel || site.wants(kind))
            .collect(),
    };
    if shown.is_empty() {
        return Ok(format!("{}\n", strings::no_laravel_sites(kind)));
    }
    let mut rows = vec![strings::WORKERS_HEADER.map(str::to_owned)];
    let mut statuses = Vec::new();
    for site in shown {
        let status = SiteKey::new(&site.name).and_then(|key| find_status(services, kind, key));
        let setting = if site.wants(kind) {
            strings::WORKER_SETTING_ON
        } else {
            strings::WORKER_SETTING_OFF
        };
        let none = || strings::NO_WORKER.to_owned();
        rows.push([
            site.name.clone(),
            setting.to_owned(),
            state(kind, site, status).to_owned(),
            status
                .and_then(|s| s.pid)
                .map_or_else(none, |pid| pid.to_string()),
            status.map_or_else(none, |s| s.restarts.to_string()),
        ]);
        statuses.extend(status);
    }
    let mut text = columns(&rows);
    text.push_str(&failed_lines(statuses, data_dir));
    Ok(text)
}

/// The state column: the service's state; `starting` for a Laravel site
/// that asks for the worker before the daemon lists it; else `-`.
fn state(kind: WorkerKind, site: &Site, status: Option<&ServiceStatus>) -> &'static str {
    match status {
        Some(status) => strings::state_name(&status.state),
        None if site.wants(kind) && site.kind == SiteKind::Laravel => strings::WORKER_STARTING,
        None => strings::NO_WORKER,
    }
}

fn find_site(list: &SiteList, site: SiteKey) -> Result<&Site, Failure> {
    let found = list.sites.iter().find(|found| found.name == site.as_str());
    found.ok_or_else(|| site_failure(codes::SITE_NOT_FOUND, site))
}

fn find_status(
    services: &[ServiceStatus],
    kind: WorkerKind,
    site: SiteKey,
) -> Option<&ServiceStatus> {
    let id = ServiceId::Worker(kind, site);
    services.iter().find(|status| status.id == id)
}

/// `name` as a site key.
fn key(name: &str) -> Result<SiteKey, Failure> {
    let name = checked(name)?;
    Ok(SiteKey::new(&name).expect("a checked site name is a valid site key"))
}

/// A failure found here about `site`.
fn site_failure(code: &str, site: SiteKey) -> Failure {
    let details = Details {
        site: Some(site.as_str()),
        ..Details::default()
    };
    local(code, &details)
}

/// The text and code the user sees for `err` from turning worker `id` on or
/// off. A site code names the site, and a `config_*` code names
/// `sites.toml`, where the daemon keeps the setting.
pub fn worker_failure(err: &ClientError, data_dir: &Path, id: ServiceId) -> Failure {
    let name = id.to_string();
    let mut failure = failure(err, data_dir, Some(&name));
    let ServiceId::Worker(_, site) = id else {
        return failure;
    };
    let sites_toml = ConfigPaths::in_dir(data_dir).sites;
    let path = match &*failure.code {
        codes::SITE_NOT_FOUND | codes::SITE_NOT_LARAVEL => None,
        codes::CONFIG_READ | codes::CONFIG_INVALID | codes::CONFIG_WRITE => {
            Some(sites_toml.as_path())
        }
        _ => return failure,
    };
    let details = Details {
        service: Some(&name),
        site: Some(site.as_str()),
        path,
        ..Details::default()
    };
    failure.message = strings::message(&failure.code, &details);
    failure
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::types::ServiceState;
    use std::path::PathBuf;

    fn site(name: &str, kind: SiteKind, queue: bool) -> Site {
        Site {
            name: name.into(),
            root: PathBuf::from(format!("/code/{name}")),
            docroot: PathBuf::from(format!("/code/{name}/public")),
            kind,
            php: None,
            php_source: None,
            secure: true,
            queue,
            schedule: false,
        }
    }

    fn list(sites: Vec<Site>) -> SiteList {
        SiteList {
            sites,
            ..SiteList::default()
        }
    }

    fn key(name: &str) -> SiteKey {
        SiteKey::new(name).unwrap()
    }

    fn worker(name: &str, state: ServiceState, pid: Option<u32>, restarts: u32) -> ServiceStatus {
        ServiceStatus {
            id: ServiceId::Worker(WorkerKind::Queue, key(name)),
            state,
            pid,
            restarts,
            since_ms: None,
        }
    }

    fn data() -> &'static Path {
        Path::new("/data")
    }

    #[test]
    fn each_command_gives_its_request() {
        let shop = || "shop".to_owned();
        let on = WorkerCommand::On { site: shop() };
        let off = WorkerCommand::Off { site: shop() };
        let switch = |on| Request::Switch {
            site: key("shop"),
            on,
        };
        assert_eq!(on.request().unwrap(), switch(true));
        assert_eq!(off.request().unwrap(), switch(false));
        let all = WorkerCommand::Status { site: None };
        assert_eq!(all.request().unwrap(), Request::Status { site: None });
        let one = WorkerCommand::Status { site: Some(shop()) };
        let one_site = Some(key("shop"));
        assert_eq!(one.request().unwrap(), Request::Status { site: one_site });
        // No site has this name, so the daemon is not asked.
        let bad = WorkerCommand::On {
            site: "Shop!".into(),
        };
        let failure = bad.request().unwrap_err();
        assert_eq!(failure.code, "site_name_invalid");
        assert!(
            failure.message.starts_with("\"Shop!\" is not"),
            "{failure:?}"
        );
    }

    #[test]
    fn status_lists_every_laravel_site_and_why_a_worker_failed() {
        let sites = list(vec![
            site("api", SiteKind::Laravel, true),
            site("blog", SiteKind::Php, false),
            site("docs", SiteKind::Laravel, false),
            site("old", SiteKind::Php, true),
            site("shop", SiteKind::Laravel, true),
            site("web", SiteKind::Laravel, true),
        ]);
        let failed = ServiceState::Failed {
            code: "worker_failed".into(),
        };
        let services = [
            worker("shop", ServiceState::Running, Some(4242), 1),
            worker("web", failed, None, 8),
            ServiceStatus {
                id: ServiceId::Caddy,
                state: ServiceState::Running,
                pid: Some(7),
                restarts: 0,
                since_ms: None,
            },
        ];
        let text = render_status(WorkerKind::Queue, None, &sites, &services, data());
        assert_eq!(
            text.unwrap(),
            "SITE  SETTING  STATE     PID   RESTARTS\n\
             api   on       starting  -     -\n\
             docs  off      -         -     -\n\
             old   on       -         -     -\n\
             shop  on       running   4242  1\n\
             web   on       failed    -     8\n\
             queue-web failed: queue-web stopped after it crashed again and again. Run \
             \"marsa logs queue-web\" to see why, fix it, then run \"marsa services restart \
             queue-web\". [worker_failed]\n"
        );
        // The scheduler of the same sites: each is off.
        let text = render_status(WorkerKind::Schedule, None, &sites, &services, data());
        assert!(text.unwrap().contains("shop  off      -"));
    }

    #[test]
    fn status_of_one_site_is_one_row_or_says_why_not() {
        let sites = list(vec![
            site("blog", SiteKind::Php, false),
            site("shop", SiteKind::Laravel, true),
        ]);
        let services = [worker("shop", ServiceState::Starting, None, 0)];
        let status = |name| {
            render_status(
                WorkerKind::Queue,
                Some(key(name)),
                &sites,
                &services,
                data(),
            )
        };
        assert_eq!(
            status("shop").unwrap(),
            "SITE  SETTING  STATE     PID  RESTARTS\nshop  on       starting  -    0\n"
        );
        let missing = status("gone").unwrap_err();
        assert_eq!(
            missing.line(),
            "marsa: There is no site named \"gone\". Run \"marsa sites\" to see them. \
             [site_not_found]"
        );
        let blog = status("blog").unwrap_err();
        assert_eq!(blog.code, "site_not_laravel");
        assert!(
            blog.message
                .starts_with("\"blog\" is not a Laravel project")
        );
    }

    #[test]
    fn no_laravel_site_is_a_sentence() {
        let sites = list(vec![site("blog", SiteKind::Static, false)]);
        let text = render_status(WorkerKind::Schedule, None, &sites, &[], data());
        assert_eq!(
            text.unwrap(),
            "No Laravel sites yet. The scheduler runs for a Laravel project only.\n"
        );
    }

    #[test]
    fn on_and_off_say_the_setting_and_the_state() {
        let shop = |queue| list(vec![site("shop", SiteKind::Laravel, queue)]);
        let switch = |list: &SiteList, services: &[ServiceStatus]| {
            render_switch(WorkerKind::Queue, key("shop"), list, services, data())
        };
        let on = "Queue worker is on for shop: starting. Run \"marsa queue status shop\" to see \
                  it.\n";
        assert_eq!(switch(&shop(true), &[]).unwrap(), on);
        let running = [worker("shop", ServiceState::Running, Some(9), 0)];
        assert!(
            switch(&shop(true), &running)
                .unwrap()
                .contains(": running.")
        );
        let no_php = ServiceState::Failed {
            code: "php_not_installed".into(),
        };
        let text = switch(&shop(true), &[worker("shop", no_php, None, 0)]).unwrap();
        assert!(text.contains(": failed. Run"), "{text}");
        assert!(text.ends_with("[php_not_installed]\n"), "{text}");
        assert_eq!(
            switch(&shop(false), &[]).unwrap(),
            "Queue worker is off for shop.\n"
        );
        let scheduler = render_switch(WorkerKind::Schedule, key("shop"), &shop(true), &[], data());
        assert_eq!(scheduler.unwrap(), "Scheduler is off for shop.\n");
        let gone = switch(&list(Vec::new()), &[]).unwrap_err();
        assert_eq!(gone.code, "site_not_found");
    }

    #[test]
    fn a_refused_switch_names_the_site() {
        let id = ServiceId::Worker(WorkerKind::Schedule, key("blog"));
        let api = |status, code: &str| ClientError::Api {
            status,
            code: code.into(),
            message: "x".into(),
        };
        let blog = worker_failure(&api(409, "site_not_laravel"), data(), id);
        assert_eq!(
            blog.line(),
            "marsa: \"blog\" is not a Laravel project (it has no artisan file), so it has no \
             Laravel log or workers. [site_not_laravel]"
        );
        let gone = worker_failure(&api(404, "site_not_found"), data(), id);
        assert!(gone.message.contains("no site named \"blog\""), "{gone:?}");
        let config = worker_failure(&api(500, "config_write"), data(), id);
        let sites_toml = ConfigPaths::in_dir(data()).sites;
        let named = sites_toml.display().to_string();
        assert!(config.message.contains(&named), "{config:?}");
        // Any other code keeps the general text.
        let down = worker_failure(&ClientError::NotRunning, data(), id);
        assert_eq!(down.code, "daemon_not_running");
    }
}
