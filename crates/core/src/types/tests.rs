use std::fmt;
use std::num::NonZeroU16;
use std::path::PathBuf;

use super::*;
use crate::databases::ServerKind;
use crate::php::PhpMinor;
use crate::recipes::Os;
use crate::runtimes::Runtime;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// The value serializes to `expected` exactly, and `expected` reads back
/// into the same value.
fn round_trip<T>(value: &T, expected: Value)
where
    T: Serialize + DeserializeOwned + PartialEq + fmt::Debug,
{
    assert_eq!(serde_json::to_value(value).unwrap(), expected);
    let back: T = serde_json::from_value(expected).unwrap();
    assert_eq!(&back, value);
}

fn running_caddy() -> ServiceStatus {
    ServiceStatus {
        id: ServiceId::Caddy,
        state: ServiceState::Running,
        pid: Some(4242),
        restarts: 0,
        since_ms: Some(1_762_000_000_000),
    }
}

#[test]
fn health() {
    let health = Health {
        app_version: "0.1.0".into(),
        api_version: API_VERSION,
        pid: 7,
    };
    round_trip(
        &health,
        json!({ "app_version": "0.1.0", "api_version": 1, "pid": 7 }),
    );
}

#[test]
fn service_status_running() {
    round_trip(
        &running_caddy(),
        json!({
            "id": "caddy",
            "state": "running",
            "pid": 4242,
            "restarts": 0,
            "since_ms": 1_762_000_000_000_u64,
        }),
    );
}

#[test]
fn service_status_failed_carries_the_code() {
    let status = ServiceStatus {
        id: ServiceId::Caddy,
        state: ServiceState::Failed {
            code: "port_in_use".into(),
        },
        pid: None,
        restarts: 3,
        since_ms: None,
    };
    round_trip(
        &status,
        json!({
            "id": "caddy",
            "state": "failed",
            "code": "port_in_use",
            "pid": null,
            "restarts": 3,
            "since_ms": null,
        }),
    );
}

#[test]
fn every_plain_state_is_snake_case() {
    for (state, name) in [
        (ServiceState::Stopped, "stopped"),
        (ServiceState::Starting, "starting"),
        (ServiceState::Running, "running"),
        (ServiceState::Stopping, "stopping"),
    ] {
        round_trip(&state, json!({ "state": name }));
    }
}

/// The text of each id is the one the daemon sent before the type: the
/// JSON, the log file names, and the routes did not change.
#[test]
fn service_ids_are_their_text_and_nothing_else_parses() {
    let line = |major, minor| PhpMinor { major, minor };
    let ids = [
        (ServiceId::Caddy, "caddy"),
        (ServiceId::Dns, "dns"),
        (ServiceId::Pool(PoolKind::Fpm, line(8, 4)), "php-fpm-8.4"),
        (ServiceId::Pool(PoolKind::Cgi, line(8, 10)), "php-cgi-8.10"),
        (ServiceId::Database(ServerKind::Postgres), "postgres"),
        (ServiceId::Database(ServerKind::Mysql), "mysql"),
        (ServiceId::Database(ServerKind::Valkey), "valkey"),
    ];
    for (id, text) in ids {
        assert_eq!(id.to_string(), text);
        assert_eq!(text.parse(), Ok(id));
        assert_eq!(serde_json::to_value(id).unwrap(), json!(text));
        assert_eq!(
            serde_json::from_value::<ServiceId>(json!(text)).unwrap(),
            id
        );
    }
    assert_eq!(
        ServiceId::pool(Os::Windows, line(8, 4)).to_string(),
        "php-cgi-8.4"
    );
    assert_eq!(
        ServiceId::pool(Os::Linux, line(8, 4)).to_string(),
        "php-fpm-8.4"
    );
    for text in [
        "",
        "Caddy",
        "php",
        "php-fpm",
        "php-fpm-8",
        "php-fpm-8.",
        "php-fpm-08.4",
        "php-fpm-8.4.1",
        "php-fpm-+8.4",
        "my-php-fpm-8.4",
        "mailhog",
    ] {
        assert_eq!(text.parse::<ServiceId>(), Err(UnknownService(text.into())));
    }
    assert!(serde_json::from_value::<ServiceId>(json!("mailhog")).is_err());
}

#[test]
fn service_action_parses_and_prints_path_segments() {
    for action in [
        ServiceAction::Start,
        ServiceAction::Stop,
        ServiceAction::Restart,
    ] {
        assert_eq!(action.as_str().parse::<ServiceAction>(), Ok(action));
        assert_eq!(action.to_string(), action.as_str());
        round_trip(&action, json!(action.as_str()));
    }
}

#[test]
fn service_action_rejects_unknown_and_wrong_case() {
    for bad in ["kill", "Start", "", "start "] {
        let err = bad.parse::<ServiceAction>().unwrap_err();
        assert_eq!(err, UnknownAction(bad.to_owned()));
        assert_eq!(err.code(), "unknown_action");
    }
}

#[test]
fn log_line() {
    let line = LogLine {
        service: ServiceId::Caddy,
        seq: 12,
        at_ms: 5,
        stream: LogStream::Stderr,
        text: "serving initial configuration".into(),
    };
    round_trip(
        &line,
        json!({
            "service": "caddy",
            "seq": 12,
            "at_ms": 5,
            "stream": "stderr",
            "text": "serving initial configuration",
        }),
    );
}

#[test]
fn events_are_tagged() {
    round_trip(
        &Event::ServiceChanged(running_caddy()),
        json!({
            "type": "service_changed",
            "data": {
                "id": "caddy",
                "state": "running",
                "pid": 4242,
                "restarts": 0,
                "since_ms": 1_762_000_000_000_u64,
            },
        }),
    );
    round_trip(
        &Event::Log(LogLine {
            service: ServiceId::Caddy,
            seq: 0,
            at_ms: 1,
            stream: LogStream::Stdout,
            text: "hi".into(),
        }),
        json!({
            "type": "log",
            "data": { "service": "caddy", "seq": 0, "at_ms": 1, "stream": "stdout", "text": "hi" },
        }),
    );
    round_trip(
        &Event::DefaultChanged(DefaultVersion {
            runtime: Runtime::Php,
            default: "8.3".into(),
            version: "8.3.27".into(),
        }),
        json!({
            "type": "default_changed",
            "data": { "runtime": "php", "default": "8.3", "version": "8.3.27" },
        }),
    );
    round_trip(&Event::ShuttingDown, json!({ "type": "shutting_down" }));
}

#[test]
fn error_body() {
    round_trip(
        &ErrorBody::new("service_not_found", "no service named php"),
        json!({ "error": { "code": "service_not_found", "message": "no service named php" } }),
    );
}

#[test]
fn install_events() {
    round_trip(
        &InstallEvent::Selected {
            runtime: Runtime::Php,
            version: "8.4.23".into(),
        },
        json!({ "step": "selected", "runtime": "php", "version": "8.4.23" }),
    );
    round_trip(
        &InstallEvent::Downloading {
            artifact: "cli".into(),
            done: 1,
            total: 2,
        },
        json!({ "step": "downloading", "artifact": "cli", "done": 1, "total": 2 }),
    );
    round_trip(&InstallEvent::Checking, json!({ "step": "checking" }));
}

#[test]
fn runtime_list() {
    let list = RuntimeList {
        runtimes: vec![
            RuntimeVersions {
                runtime: Runtime::Php,
                installed: vec!["8.3.27".into(), "8.4.23".into()],
                default: Some("8.4".into()),
            },
            RuntimeVersions {
                runtime: Runtime::Composer,
                installed: vec![],
                default: None,
            },
        ],
    };
    round_trip(
        &list,
        json!({ "runtimes": [
                { "runtime": "php", "installed": ["8.3.27", "8.4.23"], "default": "8.4" },
                { "runtime": "composer", "installed": [] },
            ] }),
    );
}

#[test]
fn install_request_and_accepted() {
    round_trip(
        &InstallRequest {
            version: Some("8.4".into()),
            force: false,
        },
        json!({ "version": "8.4", "force": false }),
    );
    // Both fields may be left out.
    let empty: InstallRequest = serde_json::from_value(json!({})).unwrap();
    assert_eq!(empty, InstallRequest::default());
    round_trip(&InstallAccepted { id: 7 }, json!({ "id": 7 }));
}

#[test]
fn default_request_and_answer() {
    round_trip(
        &DefaultRequest {
            version: "8.3".into(),
        },
        json!({ "version": "8.3" }),
    );
    // The version is required: there is no default for the default.
    assert!(serde_json::from_value::<DefaultRequest>(json!({})).is_err());
    round_trip(
        &DefaultVersion {
            runtime: Runtime::Node,
            default: "22".into(),
            version: "22.11.0".into(),
        },
        json!({ "runtime": "node", "default": "22", "version": "22.11.0" }),
    );
}

#[test]
fn the_sites_left_without_php_have_a_code_of_their_own() {
    let left = VersionInUse {
        version: "8.3.12".into(),
        sites_left_without_php: vec!["blog".into()],
        ..VersionInUse::default()
    };
    assert!(left.any());
    assert_eq!(left.code(), "sites_lose_php");
    round_trip(
        &left,
        json!({ "version": "8.3.12", "sites_left_without_php": ["blog"], "caddy": false }),
    );
    // With any other use, the general code.
    let pinned = VersionInUse {
        sites: vec!["shop".into()],
        ..left.clone()
    };
    assert_eq!(pinned.code(), "version_in_use");
    let running = VersionInUse {
        caddy: true,
        ..VersionInUse::default()
    };
    assert_eq!(running.code(), "version_in_use");
    // PostgreSQL runs from it. Written only when true, so an
    // older client reads the same JSON as before.
    let server = VersionInUse {
        version: "18.7.0".into(),
        server: true,
        ..VersionInUse::default()
    };
    assert!(server.any());
    assert_eq!(server.code(), "version_in_use");
    round_trip(
        &server,
        json!({ "version": "18.7.0", "caddy": false, "server": true }),
    );
    let no_server = serde_json::to_value(VersionInUse::default()).unwrap();
    assert!(no_server.get("server").is_none(), "{no_server}");
    // An answer from an older daemon reads with no such sites.
    let old: VersionInUse =
        serde_json::from_value(json!({ "version": "8.3.12", "default": "8.3" })).unwrap();
    assert!(old.sites_left_without_php.is_empty() && old.any());
    assert!(!VersionInUse::default().any());
}

#[test]
fn job_events_are_flat_objects_in_a_job_event() {
    let event = |update| {
        Event::Job(JobEvent {
            id: 4,
            kind: JobKind::Setup,
            update,
        })
    };
    round_trip(&JobAccepted { id: 4 }, json!({ "id": 4 }));
    round_trip(
        &event(JobUpdate::Running {
            step: JobStep::Started,
        }),
        json!({
            "type": "job",
            "data": { "id": 4, "kind": "setup", "status": "running", "step": "started" },
        }),
    );
    round_trip(
        &event(JobUpdate::Done(JobOutcome::Setup(SetupReport::default()))),
        json!({
            "type": "job",
            "data": {
                "id": 4, "kind": "setup", "status": "done", "result": "setup",
                "data": { "results": [], "skipped": [], "port_holders": [] },
            },
        }),
    );
    round_trip(
        &event(JobUpdate::Failed(JobFailure {
            code: "internal_error".into(),
            message: "the setup job failed".into(),
            http_status: Some(500),
        })),
        json!({
            "type": "job",
            "data": {
                "id": 4, "kind": "setup", "status": "failed",
                "code": "internal_error", "message": "the setup job failed",
                "http_status": 500,
            },
        }),
    );
    // A daemon that sends no status: the field is absent, not `null`.
    round_trip(
        &event(JobUpdate::Failed(JobFailure {
            code: "x".into(),
            message: "y".into(),
            http_status: None,
        })),
        json!({
            "type": "job",
            "data": { "id": 4, "kind": "setup", "status": "failed", "code": "x", "message": "y" },
        }),
    );
    round_trip(
        &Event::Job(JobEvent {
            id: 5,
            kind: JobKind::Uninstall,
            update: JobUpdate::Done(JobOutcome::Uninstall(UninstallReport::default())),
        }),
        json!({
            "type": "job",
            "data": {
                "id": 5, "kind": "uninstall", "status": "done", "result": "uninstall",
                "data": { "results": [], "stopping": false },
            },
        }),
    );
    round_trip(
        &Event::Job(JobEvent {
            id: 6,
            kind: JobKind::HostsSync,
            update: JobUpdate::Done(JobOutcome::HostsSync(SetupReport::default())),
        }),
        json!({
            "type": "job",
            "data": {
                "id": 6, "kind": "hosts_sync", "status": "done", "result": "hosts_sync",
                "data": { "results": [], "skipped": [], "port_holders": [] },
            },
        }),
    );
    round_trip(
        &Event::Job(JobEvent {
            id: 7,
            kind: JobKind::ServiceAction,
            update: JobUpdate::Failed(JobFailure {
                code: "service_busy".into(),
                message: "another action runs".into(),
                http_status: Some(409),
            }),
        }),
        json!({
            "type": "job",
            "data": {
                "id": 7, "kind": "service_action", "status": "failed",
                "code": "service_busy", "message": "another action runs",
                "http_status": 409,
            },
        }),
    );
    round_trip(
        &JobEvent {
            id: 8,
            kind: JobKind::ServiceAction,
            update: JobUpdate::Done(JobOutcome::ServiceAction(ServiceStatus {
                id: ServiceId::Caddy,
                state: ServiceState::Running,
                pid: Some(1),
                restarts: 0,
                since_ms: None,
            })),
        },
        json!({
            "id": 8, "kind": "service_action", "status": "done", "result": "service_action",
            "data": {
                "id": "caddy", "state": "running", "pid": 1, "restarts": 0, "since_ms": null,
            },
        }),
    );
    // A reader that knows only `code` and `message` (an `ApiError`) still
    // reads a failure that carries a status.
    let old: ApiError =
        serde_json::from_value(json!({ "code": "x", "message": "y", "http_status": 409 })).unwrap();
    assert_eq!((old.code.as_str(), old.message.as_str()), ("x", "y"));
    round_trip(
        &Event::Job(JobEvent {
            id: 9,
            kind: JobKind::RuntimeUninstall,
            update: JobUpdate::Done(JobOutcome::RuntimeUninstall(Uninstalled {
                runtime: Runtime::Node,
                version: "22.11.0".into(),
            })),
        }),
        json!({
            "type": "job",
            "data": {
                "id": 9, "kind": "runtime_uninstall", "status": "done",
                "result": "runtime_uninstall",
                "data": { "runtime": "node", "version": "22.11.0" },
            },
        }),
    );
    let running = JobEvent {
        id: 1,
        kind: JobKind::Setup,
        update: JobUpdate::Running {
            step: JobStep::Started,
        },
    };
    assert!(!running.is_last());
    // A kind and a step of a newer daemon are read, not refused.
    let newer: JobEvent = serde_json::from_value(
        json!({ "id": 2, "kind": "from_the_future", "status": "running", "step": "later" }),
    )
    .unwrap();
    assert_eq!(newer.kind, JobKind::Other);
    assert_eq!(
        newer.update,
        JobUpdate::Running {
            step: JobStep::Other
        }
    );
    round_trip(
        &JobList {
            jobs: vec![running],
        },
        json!({ "jobs": [{ "id": 1, "kind": "setup", "status": "running", "step": "started" }] }),
    );
}

#[test]
fn install_progress_is_one_flat_object_in_an_install_event() {
    let step = |update| Event::Install(InstallProgress { id: 3, update });
    round_trip(
        &step(InstallUpdate::Running(InstallEvent::Downloading {
            artifact: "cli".into(),
            done: 1,
            total: 2,
        })),
        json!({
            "type": "install",
            "data": {
                "id": 3, "status": "running",
                "step": "downloading", "artifact": "cli", "done": 1, "total": 2,
            },
        }),
    );
    round_trip(
        &step(InstallUpdate::Running(InstallEvent::Checking)),
        json!({ "type": "install", "data": { "id": 3, "status": "running", "step": "checking" } }),
    );
    round_trip(
        &step(InstallUpdate::Done(InstallDone {
            runtime: Runtime::Node,
            version: "22.11.0".into(),
            dir: PathBuf::from("/d/runtimes/node/22.11.0"),
            already: false,
        })),
        json!({
            "type": "install",
            "data": {
                "id": 3, "status": "done", "runtime": "node", "version": "22.11.0",
                "dir": "/d/runtimes/node/22.11.0", "already": false,
            },
        }),
    );
}

#[test]
fn a_failed_install_carries_its_code_and_only_the_facts_it_has() {
    let failed = |failure| {
        Event::Install(InstallProgress {
            id: 9,
            update: InstallUpdate::Failed(failure),
        })
    };
    round_trip(
        &failed(InstallFailure {
            code: "install_busy".into(),
            message: "another install is running".into(),
            ..InstallFailure::default()
        }),
        json!({
            "type": "install",
            "data": {
                "id": 9, "status": "failed",
                "code": "install_busy", "message": "another install is running",
            },
        }),
    );
    round_trip(
        &failed(InstallFailure {
            code: "install_no_such_version".into(),
            message: "no match".into(),
            requested: Some("8.9".into()),
            available: vec!["8.3.27".into(), "8.4.23".into()],
            ..InstallFailure::default()
        }),
        json!({
            "type": "install",
            "data": {
                "id": 9, "status": "failed",
                "code": "install_no_such_version", "message": "no match",
                "requested": "8.9", "available": ["8.3.27", "8.4.23"],
            },
        }),
    );
}

#[test]
fn site_list_in_a_sites_changed_event() {
    let list = SiteList {
        parked: vec![PathBuf::from("/code")],
        sites: vec![Site {
            name: "shop".into(),
            root: PathBuf::from("/code/shop"),
            docroot: PathBuf::from("/code/shop/public"),
            kind: SiteKind::Laravel,
            php: Some(semver::Version::new(8, 4, 23)),
            php_source: Some(Source::GlobalDefault),
            secure: true,
        }],
        skipped: vec![SkippedFolder {
            path: PathBuf::from("/code/متجر"),
            code: "site_name_invalid".into(),
        }],
    };
    round_trip(
        &Event::SitesChanged(list),
        json!({
            "type": "sites_changed",
            "data": {
                "parked": ["/code"],
                "sites": [{
                    "name": "shop", "root": "/code/shop", "docroot": "/code/shop/public",
                    "kind": { "type": "laravel" }, "php": "8.4.23",
                    "php_source": { "type": "global_default" }, "secure": true,
                }],
                "skipped": [{ "path": "/code/متجر", "code": "site_name_invalid" }],
            },
        }),
    );
}

#[test]
fn park_and_link_requests() {
    round_trip(
        &ParkRequest {
            path: PathBuf::from("/code"),
        },
        json!({ "path": "/code" }),
    );
    round_trip(
        &LinkRequest {
            name: "api".into(),
            path: PathBuf::from("/work/api"),
        },
        json!({ "name": "api", "path": "/work/api" }),
    );
}

#[test]
fn a_site_patch_tells_null_from_a_missing_field() {
    round_trip(&SitePatch::default(), json!({}));
    round_trip(
        &SitePatch {
            php: Some(Some("8.3".into())),
            secure: Some(false),
            proxy: Some(NonZeroU16::new(5173)),
        },
        json!({ "php": "8.3", "secure": false, "proxy": 5173 }),
    );
    round_trip(
        &SitePatch {
            php: Some(None),
            secure: None,
            proxy: Some(None),
        },
        json!({ "php": null, "proxy": null }),
    );
    // `null` for `secure` keeps it, like a missing field.
    let patch: SitePatch = serde_json::from_value(json!({ "secure": null })).unwrap();
    assert_eq!(patch, SitePatch::default());
    for bad in [json!({ "proxy": 0 }), json!({ "proxy": 70000 })] {
        assert!(serde_json::from_value::<SitePatch>(bad).is_err());
    }
}

#[test]
fn a_setup_report_in_a_setup_finished_event() {
    let report = SetupReport {
        results: vec![OpResult {
            op: OpName::ResolverInstall,
            status: OpStatus::Done,
            code: None,
            message: None,
        }],
        skipped: vec![SetupSkip {
            op: OpName::PfInstall,
            code: "redirect_port_held".into(),
        }],
        port_holders: vec![PortHolder {
            port: 80,
            address: "127.0.0.1".into(),
            process: Some("nginx-arm64".into()),
            pid: Some(15464),
            user: None,
            command: None,
            app: Some("Herd".into()),
        }],
        error: None,
    };
    round_trip(
        &Event::SetupFinished(report),
        json!({
            "type": "setup_finished",
            "data": {
                "results": [{ "op": "resolver_install", "status": "done" }],
                "skipped": [{ "op": "pf_install", "code": "redirect_port_held" }],
                "port_holders": [{
                    "port": 80, "address": "127.0.0.1", "process": "nginx-arm64",
                    "pid": 15464, "app": "Herd",
                }],
            },
        }),
    );
    let failed = SetupReport {
        error: Some(ApiError {
            code: "setup_cancelled".into(),
            message: "the admin prompt was cancelled".into(),
        }),
        ..SetupReport::default()
    };
    round_trip(
        &failed,
        json!({
            "results": [], "skipped": [], "port_holders": [],
            "error": { "code": "setup_cancelled", "message": "the admin prompt was cancelled" },
        }),
    );
}
