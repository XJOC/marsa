use super::*;

fn site(root: &Path, kind: SiteKind, php: Option<&str>, queue: bool, schedule: bool) -> Site {
    Site {
        name: "shop".to_owned(),
        root: root.to_owned(),
        docroot: root.join("public"),
        kind,
        php: php.map(|version| semver::Version::parse(version).unwrap()),
        php_source: None,
        secure: true,
        queue,
        schedule,
    }
}

#[test]
fn only_laravel_sites_that_ask_get_workers() {
    let data = Path::new("/data");
    let root = Path::new("/code/shop");
    let platform = Some(Platform::LinuxX64);
    let both = site(root, SiteKind::Laravel, Some("8.4.1"), true, true);
    let ids: Vec<String> = wanted(data, platform, &[both])
        .keys()
        .map(ToString::to_string)
        .collect();
    assert_eq!(ids, ["queue-shop", "schedule-shop"]);
    for other in [
        SiteKind::Php,
        SiteKind::Static,
        SiteKind::NodeProxy { port: 5173 },
    ] {
        let asks = site(root, other, Some("8.4.1"), true, true);
        assert!(wanted(data, platform, &[asks]).is_empty());
    }
    let off = site(root, SiteKind::Laravel, Some("8.4.1"), false, false);
    assert!(wanted(data, platform, &[off]).is_empty());
}

#[test]
fn a_worker_runs_the_sites_php_cli_with_its_php_ini() {
    let data = Path::new("/data");
    let root = Path::new("/code/shop");
    let shop = site(root, SiteKind::Laravel, Some("8.4.1"), true, false);
    let key = SiteKey::new("shop").unwrap();
    let plan = plan(
        data,
        Some(Platform::LinuxX64),
        &shop,
        WorkerKind::Queue,
        key,
    )
    .unwrap();
    assert_eq!(plan.spec.program, data.join("runtimes/php/8.4.1/bin/php"));
    // Built as the daemon builds it: Windows joins with `\`.
    let version = semver::Version::new(8, 4, 1);
    let ini = PhpFiles::new(data, &version).ini_file;
    assert_eq!(plan.spec.args[1], ini.to_string_lossy());
    assert_eq!(plan.spec.cwd.as_deref(), Some(root));
    let windows = super::plan(
        data,
        Some(Platform::WindowsX64),
        &shop,
        WorkerKind::Queue,
        key,
    );
    assert_eq!(
        windows.unwrap().spec.program.file_name().unwrap(),
        "php.exe"
    );
}

#[test]
fn a_site_without_php_or_a_system_without_builds_cannot_run_a_worker() {
    let data = Path::new("/data");
    let root = Path::new("/code/shop");
    let key = SiteKey::new("shop").unwrap();
    let none = site(root, SiteKind::Laravel, None, true, false);
    let refused = plan(
        data,
        Some(Platform::LinuxX64),
        &none,
        WorkerKind::Queue,
        key,
    );
    assert_eq!(refused, Err(codes::PHP_NOT_INSTALLED));
    let shop = site(root, SiteKind::Laravel, Some("8.4.1"), true, false);
    let refused = plan(data, None, &shop, WorkerKind::Queue, key);
    assert_eq!(refused, Err(codes::PHP_NOT_INSTALLED));
}

// The fake PHP is a shell script, and the tests send signals: Unix only.
// Windows runs the same manager; its stop is the supervisor's kill.
#[cfg(unix)]
mod real {
    use super::*;
    use std::time::Duration;

    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::Arc;
    use tempfile::TempDir;

    use marsa_core::services::RestartPolicy;

    /// A fake PHP CLI: prints how it was run, then fails while the site has a
    /// file `fail`, or runs until stopped.
    pub(super) const FAKE_PHP: &str = "#!/bin/sh\n\
        echo \"fake $0 $*\"\n\
        if [ -f fail ]; then exit 1; fi\n\
        exec sleep 600\n";

    const WAIT: Duration = Duration::from_secs(30);

    struct Setup {
        _temp: TempDir,
        data: PathBuf,
        root: PathBuf,
        daemon: Arc<Daemon>,
        manager: WorkerManager,
    }

    fn setup() -> Setup {
        let temp = TempDir::new().unwrap();
        let data = temp.path().join("data");
        let root = temp.path().join("shop");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("artisan"), "<?php\n").unwrap();
        let daemon = Arc::new(Daemon::new(data.clone()));
        let mut manager = WorkerManager::new(&data, Platform::current());
        manager.restart = RestartPolicy::Always {
            max_retries: 2,
            initial: Duration::from_millis(20),
            max: Duration::from_millis(50),
        };
        Setup {
            _temp: temp,
            data,
            root,
            daemon,
            manager,
        }
    }

    pub(super) fn install_php(data: &Path, version: &str) {
        let bin = data.join("runtimes/php").join(version).join("bin");
        fs::create_dir_all(&bin).unwrap();
        let php = bin.join("php");
        fs::write(&php, FAKE_PHP).unwrap();
        fs::set_permissions(&php, fs::Permissions::from_mode(0o755)).unwrap();
        let minor = version.rsplit_once('.').unwrap().0;
        let ini = data.join("php").join(minor);
        fs::create_dir_all(&ini).unwrap();
        fs::write(ini.join("php.ini"), format!("; php {version}\n")).unwrap();
    }

    fn queue() -> ServiceId {
        ServiceId::Worker(WorkerKind::Queue, SiteKey::new("shop").unwrap())
    }

    /// Waits until the status of the queue worker passes `ok`.
    async fn wait_for(daemon: &Daemon, ok: impl Fn(&ServiceStatus) -> bool) -> ServiceStatus {
        let wait = async {
            loop {
                let found = daemon.workers().find(queue());
                let status = found.map(|runtime| lock(&runtime).status.clone());
                if let Some(status) = status.filter(|status| ok(status)) {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(WAIT, wait)
            .await
            .expect("the status in time")
    }

    fn running(status: &ServiceStatus) -> bool {
        status.state == ServiceState::Running && status.pid.is_some()
    }

    fn alive(pid: u32) -> bool {
        std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    fn kill(pid: u32) {
        let killed = std::process::Command::new("/bin/kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
    }

    fn lines(daemon: &Daemon) -> String {
        let runtime = daemon.workers().find(queue()).unwrap();
        let lines = lock(&runtime).logs.recent();
        lines
            .iter()
            .map(|line| format!("{}\n", line.text))
            .collect()
    }

    /// Waits until the worker's lines hold `needle`: a line comes a moment
    /// after the `running` state.
    async fn wait_line(daemon: &Daemon, needle: &str) {
        let wait = async {
            while !lines(daemon).contains(needle) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let found = tokio::time::timeout(WAIT, wait).await;
        assert!(found.is_ok(), "no {needle:?} in {}", lines(daemon));
    }

    /// The process is gone soon after its stop: `kill -0` may still see it
    /// for an instant while the system reaps it.
    async fn gone(pid: u32) {
        let wait = async {
            while alive(pid) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(WAIT, wait)
            .await
            .expect("the process ended");
    }

    #[tokio::test]
    async fn a_worker_runs_restarts_after_a_crash_and_stops_when_turned_off() {
        let Setup {
            _temp,
            data,
            root,
            daemon,
            mut manager,
        } = setup();
        install_php(&data, "8.4.1");
        let on = site(&root, SiteKind::Laravel, Some("8.4.1"), true, false);
        manager.sync(&daemon, std::slice::from_ref(&on)).await;
        let first = wait_for(&daemon, running).await;
        let php = data.join("runtimes/php/8.4.1/bin/php");
        let ini = data.join("php/8.4/php.ini");
        let ran = format!(
            "fake {} -c {} artisan queue:work",
            php.display(),
            ini.display()
        );
        wait_line(&daemon, &ran).await;
        assert_eq!(daemon.workers().statuses().len(), 1);

        let pid = first.pid.unwrap();
        kill(pid);
        let again = wait_for(&daemon, |s| running(s) && s.pid != Some(pid)).await;
        assert_eq!(again.restarts, 1);
        wait_line(&daemon, "marsad: signal: 9").await;

        // A pass with nothing changed leaves the running worker alone.
        manager.sync(&daemon, std::slice::from_ref(&on)).await;
        assert_eq!(wait_for(&daemon, running).await.pid, again.pid);

        let off = site(&root, SiteKind::Laravel, Some("8.4.1"), false, false);
        manager.sync(&daemon, &[off]).await;
        assert!(daemon.workers().find(queue()).is_none());
        assert!(daemon.workers().statuses().is_empty());
        assert!(!daemon.proc_alive(queue()));
        gone(again.pid.unwrap()).await;
    }

    #[tokio::test]
    async fn a_worker_that_keeps_failing_stays_failed_until_a_restart() {
        let Setup {
            _temp,
            data,
            root,
            daemon,
            mut manager,
        } = setup();
        install_php(&data, "8.4.1");
        fs::write(root.join("fail"), "").unwrap();
        let on = site(&root, SiteKind::Laravel, Some("8.4.1"), true, false);
        manager.sync(&daemon, std::slice::from_ref(&on)).await;
        let worker_failed = |s: &ServiceStatus| matches!(&s.state, ServiceState::Failed { code } if code == codes::WORKER_FAILED);
        let failed = wait_for(&daemon, worker_failed).await;
        assert_eq!(failed.restarts, 2);
        // The task ends a moment after its last state: the record of the
        // run is taken down first.
        let ended = async {
            while daemon.proc_alive(queue()) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(WAIT, ended)
            .await
            .expect("the task ended");

        // A pass does not start it again.
        fs::remove_file(root.join("fail")).unwrap();
        manager.sync(&daemon, std::slice::from_ref(&on)).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(worker_failed(&wait_for(&daemon, |_| true).await));
        assert!(!daemon.proc_alive(queue()));

        // A restart asked for does.
        daemon.workers().ask_restart(queue());
        manager.sync(&daemon, std::slice::from_ref(&on)).await;
        let back = wait_for(&daemon, running).await;
        assert_eq!(back.restarts, 0);
        daemon.stop_procs(WAIT).await;
        gone(back.pid.unwrap()).await;
    }

    #[tokio::test]
    async fn a_restart_asked_for_or_a_new_php_ini_starts_a_running_worker_again() {
        let Setup {
            _temp,
            data,
            root,
            daemon,
            mut manager,
        } = setup();
        install_php(&data, "8.4.1");
        let on = site(&root, SiteKind::Laravel, Some("8.4.1"), true, false);
        manager.sync(&daemon, std::slice::from_ref(&on)).await;
        let first = wait_for(&daemon, running).await;

        daemon.workers().ask_restart(queue());
        manager.sync(&daemon, std::slice::from_ref(&on)).await;
        let second = wait_for(&daemon, |s| running(s) && s.pid != first.pid).await;
        gone(first.pid.unwrap()).await;

        // Xdebug turned on, for example: `php.ini` changed.
        fs::write(data.join("php/8.4/php.ini"), "zend_extension=xdebug\n").unwrap();
        manager.sync(&daemon, std::slice::from_ref(&on)).await;
        let third = wait_for(&daemon, |s| running(s) && s.pid != second.pid).await;
        gone(second.pid.unwrap()).await;
        daemon.stop_procs(WAIT).await;
        gone(third.pid.unwrap()).await;
    }

    #[tokio::test]
    async fn a_running_worker_whose_site_loses_php_stops_and_says_why() {
        let Setup {
            _temp,
            data,
            root,
            daemon,
            mut manager,
        } = setup();
        install_php(&data, "8.4.1");
        let both = site(&root, SiteKind::Laravel, Some("8.4.1"), true, true);
        manager.sync(&daemon, &[both]).await;
        let first = wait_for(&daemon, running).await;
        let none = site(&root, SiteKind::Laravel, None, true, true);
        manager.sync(&daemon, &[none]).await;
        let failed = ServiceState::Failed {
            code: codes::PHP_NOT_INSTALLED.to_owned(),
        };
        assert_eq!(wait_for(&daemon, |_| true).await.state, failed);
        assert!(!daemon.proc_alive(queue()));
        gone(first.pid.unwrap()).await;
        assert_eq!(daemon.workers().statuses().len(), 2);
    }

    #[tokio::test]
    async fn a_worker_turned_off_and_on_goes_on_with_its_lines() {
        let Setup {
            _temp,
            data,
            root,
            daemon,
            mut manager,
        } = setup();
        install_php(&data, "8.4.1");
        let on = site(&root, SiteKind::Laravel, Some("8.4.1"), true, true);
        let off = site(&root, SiteKind::Laravel, Some("8.4.1"), false, false);
        manager.sync(&daemon, std::slice::from_ref(&on)).await;
        wait_line(&daemon, "artisan queue:work").await;
        let before = lock(&daemon.workers().find(queue()).unwrap()).logs.recent();
        let last = before.last().unwrap().seq;
        // Both at once: their stops overlap.
        manager.sync(&daemon, std::slice::from_ref(&off)).await;
        assert!(daemon.workers().statuses().is_empty());
        manager.sync(&daemon, &[on]).await;
        let back = wait_for(&daemon, running).await;
        let after = lock(&daemon.workers().find(queue()).unwrap()).logs.recent();
        assert!(after.iter().any(|line| line.seq > last));
        assert!(after.windows(2).all(|pair| pair[0].seq < pair[1].seq));
        daemon.stop_procs(WAIT).await;
        gone(back.pid.unwrap()).await;
    }

    #[tokio::test]
    async fn a_site_without_php_gets_a_failed_worker_and_no_process() {
        let Setup {
            _temp,
            root,
            daemon,
            mut manager,
            ..
        } = setup();
        let on = site(&root, SiteKind::Laravel, None, true, false);
        manager.sync(&daemon, std::slice::from_ref(&on)).await;
        let status = wait_for(&daemon, |_| true).await;
        let code = codes::PHP_NOT_INSTALLED.to_owned();
        assert_eq!(status.state, ServiceState::Failed { code });
        assert!(!daemon.proc_alive(queue()));
        let note = format!(
            "marsad: the worker cannot run: {}\n",
            codes::PHP_NOT_INSTALLED
        );
        assert_eq!(lines(&daemon), note);
        // Once: a second pass adds no line.
        manager.sync(&daemon, &[on]).await;
        assert_eq!(lines(&daemon), note);
    }

    #[tokio::test]
    async fn another_php_version_replaces_the_process() {
        let Setup {
            _temp,
            data,
            root,
            daemon,
            mut manager,
        } = setup();
        install_php(&data, "8.4.1");
        install_php(&data, "8.3.2");
        let new = site(&root, SiteKind::Laravel, Some("8.4.1"), true, false);
        manager.sync(&daemon, &[new]).await;
        let first = wait_for(&daemon, running).await;
        let old = site(&root, SiteKind::Laravel, Some("8.3.2"), true, false);
        manager.sync(&daemon, &[old]).await;
        let second = wait_for(&daemon, |s| running(s) && s.pid != first.pid).await;
        gone(first.pid.unwrap()).await;
        wait_line(&daemon, "runtimes/php/8.3.2/bin/php -c").await;
        daemon.stop_procs(WAIT).await;
        gone(second.pid.unwrap()).await;
    }
}

/// The routes of a worker, through the real router and the real sync task,
/// with no PHP installed: the worker is listed `php_not_installed` and no
/// process ever runs, so this runs on every OS.
mod routes {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::http::StatusCode;
    use marsa_core::config::ConfigPaths;
    use marsa_core::types::{ErrorBody, SiteList};
    use serde_json::{Value, json};

    use crate::daemon::http::test_router::TestRouter;
    use crate::daemon::sites::sync;
    use tempfile::TempDir;

    struct App {
        temp: TempDir,
        daemon: Arc<Daemon>,
        router: TestRouter,
    }

    impl App {
        /// A daemon with `shop` (Laravel) and `blog` (plain PHP) linked.
        async fn new() -> Self {
            let temp = TempDir::new().unwrap();
            let daemon = Arc::new(Daemon::new(temp.path().join("data")));
            let app = Self {
                router: TestRouter::new(Arc::clone(&daemon)),
                daemon,
                temp,
            };
            for (name, laravel) in [("shop", true), ("blog", false)] {
                let root = app.temp.path().join(name);
                fs::create_dir_all(root.join("public")).unwrap();
                fs::write(root.join("public/index.php"), "<?php\n").unwrap();
                if laravel {
                    fs::write(root.join("artisan"), "<?php\n").unwrap();
                }
                let body = json!({ "name": name, "path": root });
                let (status, json) = app.answer("POST", "/v1/sites/link", Some(body)).await;
                assert_eq!(status, StatusCode::OK, "{json}");
            }
            app
        }

        async fn answer(
            &self,
            method: &str,
            uri: &str,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            self.router.answer(method, uri, body).await
        }

        async fn refused(
            &self,
            method: &str,
            uri: &str,
            body: Option<Value>,
        ) -> (StatusCode, String) {
            let (status, json) = self.answer(method, uri, body).await;
            let error: ErrorBody = serde_json::from_value(json).unwrap();
            (status, error.error.code)
        }

        async fn patch(&self, name: &str, body: Value) -> SiteList {
            let uri = format!("/v1/sites/{name}");
            let (status, json) = self.answer("PATCH", &uri, Some(body)).await;
            assert_eq!(status, StatusCode::OK, "{json}");
            serde_json::from_value(json).unwrap()
        }

        async fn services(&self) -> Vec<ServiceStatus> {
            let (status, json) = self.answer("GET", "/v1/services", None).await;
            assert_eq!(status, StatusCode::OK, "{json}");
            serde_json::from_value(json).unwrap()
        }

        /// Waits until the list of services passes `ok`.
        async fn wait_services(&self, ok: impl Fn(&[ServiceStatus]) -> bool) -> Vec<ServiceStatus> {
            let wait = async {
                loop {
                    let services = self.services().await;
                    if ok(&services) {
                        return services;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            tokio::time::timeout(Duration::from_secs(30), wait)
                .await
                .expect("the services in time")
        }
    }

    fn worker(services: &[ServiceStatus]) -> Option<&ServiceStatus> {
        services
            .iter()
            .find(|status| status.id.to_string() == "queue-shop")
    }

    #[tokio::test]
    async fn a_worker_follows_its_sites_setting_and_takes_only_restart() {
        let app = App::new().await;
        let sync = sync::start(&app.daemon);

        let list = app.patch("shop", json!({ "queue": true })).await;
        let shop = list.sites.iter().find(|site| site.name == "shop").unwrap();
        assert!(shop.queue && !shop.schedule);
        let sites_toml =
            fs::read_to_string(ConfigPaths::in_dir(app.daemon.data_dir()).sites).unwrap();
        assert!(sites_toml.contains("queue = true"), "{sites_toml}");

        let failed = ServiceState::Failed {
            code: codes::PHP_NOT_INSTALLED.to_owned(),
        };
        let services = app
            .wait_services(|services| worker(services).is_some_and(|s| s.state == failed))
            .await;
        assert_eq!(worker(&services).unwrap().pid, None);
        let (status, json) = app.answer("GET", "/v1/logs/queue-shop", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(json.to_string().contains("php_not_installed"), "{json}");

        for action in ["start", "stop"] {
            let refused = (StatusCode::CONFLICT, codes::WORKER_FOLLOWS_SITE.to_owned());
            let uri = format!("/v1/services/queue-shop/{action}");
            assert_eq!(app.refused("POST", &uri, None).await, refused);
            let uri = format!("/v1/jobs/services/queue-shop/{action}");
            assert_eq!(app.refused("POST", &uri, None).await, refused);
        }
        // A restart that cannot start it says why, on both routes.
        let no_php = (StatusCode::CONFLICT, codes::PHP_NOT_INSTALLED.to_owned());
        let restart = "/v1/services/queue-shop/restart";
        assert_eq!(app.refused("POST", restart, None).await, no_php);
        let (status, json) = app
            .answer("POST", "/v1/jobs/services/queue-shop/restart", None)
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{json}");
        let job = format!("/v1/jobs/{}", json["id"]);
        let ended = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let (_, json) = app.answer("GET", &job, None).await;
                if json["status"] != "running" {
                    return json;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        let ended = ended.await.expect("the job ended in time");
        assert_eq!(ended["status"], "failed", "{ended}");
        assert_eq!(ended["code"], codes::PHP_NOT_INSTALLED, "{ended}");

        // No worker for another site, nor for a name that is not a site name.
        let not_found = (StatusCode::NOT_FOUND, codes::SERVICE_NOT_FOUND.to_owned());
        for id in ["queue-blog", "schedule-shop", "queue-Shop"] {
            let uri = format!("/v1/services/{id}/restart");
            assert_eq!(app.refused("POST", &uri, None).await, not_found, "{id}");
        }

        app.patch("shop", json!({ "queue": false })).await;
        app.wait_services(|services| worker(services).is_none())
            .await;
        let sites_toml =
            fs::read_to_string(ConfigPaths::in_dir(app.daemon.data_dir()).sites).unwrap();
        assert!(!sites_toml.contains("queue"), "{sites_toml}");

        app.daemon.shutdown().cancel();
        sync.stop().await;
    }

    #[tokio::test]
    async fn only_a_laravel_site_turns_a_worker_on() {
        let app = App::new().await;
        let not_laravel = (StatusCode::CONFLICT, codes::SITE_NOT_LARAVEL.to_owned());
        for body in [json!({ "queue": true }), json!({ "schedule": true })] {
            let got = app.refused("PATCH", "/v1/sites/blog", Some(body)).await;
            assert_eq!(got, not_laravel);
        }
        // A Node proxy is not served by Laravel, even with `artisan`, and a
        // site with a worker does not become one.
        let body = json!({ "proxy": 5173, "queue": true });
        let got = app.refused("PATCH", "/v1/sites/shop", Some(body)).await;
        assert_eq!(got, not_laravel);
        let list = app.patch("shop", json!({ "schedule": true })).await;
        let body = json!({ "proxy": 5173 });
        let got = app.refused("PATCH", "/v1/sites/shop", Some(body)).await;
        assert_eq!(got, not_laravel);
        // Off is always fine, also once the site lost `artisan`.
        fs::remove_file(app.temp.path().join("shop/artisan")).unwrap();
        let off = app.patch("shop", json!({ "schedule": false })).await;
        assert!(off.sites.iter().all(|site| !site.schedule));
        assert!(
            list.sites
                .iter()
                .any(|site| site.name == "shop" && site.schedule)
        );
    }

    #[tokio::test]
    async fn a_restart_without_a_sync_pass_is_an_internal_error() {
        let app = App::new().await;
        let id = ServiceId::Worker(WorkerKind::Queue, SiteKey::new("shop").unwrap());
        app.daemon.workers().runtime(id);
        let (status, json) = app
            .answer("POST", "/v1/services/queue-shop/restart", None)
            .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{json}");
        assert_eq!(json["error"]["code"], codes::INTERNAL_ERROR);
    }

    // The fake PHP is a shell script: Unix only, as `real`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_restart_on_the_route_starts_a_running_worker_again() {
        let app = App::new().await;
        super::real::install_php(app.daemon.data_dir(), "8.4.1");
        let sync = sync::start(&app.daemon);
        // The patch builds the list again, which now finds the PHP.
        app.patch("shop", json!({ "php": "8.4", "queue": true }))
            .await;
        let running = |s: &ServiceStatus| s.state == ServiceState::Running && s.pid.is_some();
        let first = app
            .wait_services(|services| worker(services).is_some_and(running))
            .await;
        let first = worker(&first).unwrap().clone();
        let (status, json) = app
            .answer("POST", "/v1/services/queue-shop/restart", None)
            .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        let again = app
            .wait_services(|services| {
                worker(services).is_some_and(|s| running(s) && s.pid != first.pid)
            })
            .await;
        assert_eq!(worker(&again).unwrap().restarts, 0);
        app.daemon.shutdown().cancel();
        sync.stop().await;
        app.daemon.stop_procs(Duration::from_secs(30)).await;
    }
}
