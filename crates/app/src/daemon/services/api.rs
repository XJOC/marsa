//! The services and logs routes: Caddy, the php-fpm pools, and PostgreSQL.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/services` | `Vec<ServiceStatus>`: Caddy, `dns`, PostgreSQL when installed, then each pool |
//! | `POST /v1/jobs/services/{id}/{action}` | `202 JobAccepted`; the end is a `job` event with the `ServiceStatus` after the action, or its failure |
//! | `POST /v1/services/{id}/{action}` | The same job, answered with the `ServiceStatus` when it ends (clients before ADR 0026) |
//! | `GET /v1/logs/{id}` | `Vec<LogLine>`, the recent lines |
//! | `GET /v1/logs/{id}?follow=true` | SSE: the recent lines, then new ones |
//!
//! The services are `caddy`, `dns` on macOS and Linux (the daemon's own DNS
//! server), each php-fpm pool started in this run
//! (`php-fpm-8.4`), and each site's worker that its site asks for
//! (`queue-shop`, `schedule-shop`). The sites sync starts and stops the
//! pools, so an action on a pool is `409 service_managed_by_sites`. It runs
//! the workers too: a worker runs while its site's setting asks for it, so
//! `start` and `stop` on one are `409 worker_follows_site`; `restart` asks
//! the sync to start it again, also after its restarts ran out.
//!
//! An action on Caddy that finds another one running waits for it, up to
//! [`ACTION_WAIT`], then runs; a wait that runs out is still
//! `409 service_busy`. The usual one is the start of Caddy with the daemon:
//! `marsa services stop caddy` starts the daemon itself, and
//! its stop then runs after that start instead of failing.
//!
//! The database servers (`postgres`) have the same actions. A start or
//! restart that ran turns the server on in `global.toml` (`[postgres]
//! enabled = true`), so it starts with the daemon; a stop turns it off (ADR
//! 0023, section 5); a refusal or a dropped start changes nothing
//! (`ServerManager::run_and_save`). A stop drops a start that runs, and those before it in
//! line, instead of waiting for them.
//!
//! Every error, the extractors' own included, is an `ErrorBody`.

use marsa_core::codes;
use marsa_core::types::ServiceId;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use marsa_core::databases::ServerKind;
use marsa_core::types::{
    Event, JobAccepted, JobKind, JobOutcome, LogLine, ServiceAction, ServiceStatus, UnknownAction,
};
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::{Stream, StreamExt as _};

use super::caddy::{ActionError, CaddyManager};
use super::databases::{self, ServerManager};
use super::dns::DnsService;
use super::supervisor::{ServiceRuntime, lock};
use crate::daemon::http::Rejected as _;
use crate::daemon::http::routes::HttpError;
use crate::daemon::jobs;
use crate::daemon::sites::sync::PassNotRun;
use crate::daemon::state::Daemon;

/// The answer to an action on a service that the sites sync starts and stops.
pub const MANAGED_BY_SITES: &str = codes::SERVICE_MANAGED_BY_SITES;

/// How long an action on Caddy waits for another one to end before
/// `409 service_busy`. Longer than the slowest start, about 19 s: about 10 s
/// to stop an old Caddy (ADR 0009: `POST /stop` 1 s, then 2 s, SIGTERM and
/// 5 s, SIGKILL and 2 s), 5 s for the admin API, then up to 4 s to stop a
/// Caddy that never answered. The action that waited runs after: a restart
/// is a stop (up to 4 s) and such a start, about 23 s. So one request takes
/// up to about 43 s. Clients before ADR 0026 wait 60 s for it, which
/// `old_clients_wait_longer_than_the_slowest_action` holds.
pub const ACTION_WAIT: Duration = Duration::from_secs(20);

/// How long an action on a database server waits for another one to end
/// before
/// `409 service_busy`. A stop never waits for a start (it drops it), so
/// the longest it waits for is another stop: for PostgreSQL SIGINT and
/// SIGQUIT, 10 s each, then a kill, or a start's own stop after its ready
/// timeout. A start that waits for a first start (up to about 3 minutes of
/// `initdb` and single-user mode) gets `service_busy`.
pub const SERVER_ACTION_WAIT: Duration = Duration::from_secs(30);

/// A `:` comment this often on a quiet stream, as on `/v1/events`: the
/// client gives up after 45 s without a byte.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// The routes, for `http::routes::router` to merge before its fallbacks.
pub fn routes() -> Router<Arc<Daemon>> {
    Router::new()
        .route("/v1/services", get(services))
        .route("/v1/jobs/services/{id}/{action}", post(service_job))
        .route("/v1/services/{id}/{action}", post(service_action))
        .route("/v1/logs/{id}", get(logs))
}

async fn services(State(daemon): State<Arc<Daemon>>) -> Json<Vec<ServiceStatus>> {
    let mut services = vec![daemon.caddy().status()];
    services.extend(daemon.dns().status());
    for kind in ServerKind::ALL {
        // It reads `runtimes/<runtime>/`.
        let worker = Arc::clone(&daemon);
        let listed =
            tokio::task::spawn_blocking(move || worker.server(kind).listed(worker.data_dir()));
        if listed.await.unwrap_or(true) {
            services.push(daemon.server(kind).status());
        }
    }
    services.extend(daemon.pools().statuses());
    services.extend(daemon.workers().statuses());
    Json(services)
}

/// The old waiting route of an action (API v1 keeps it, ADR 0026): the same
/// job, answered with the status when it ends.
async fn service_action(
    State(daemon): State<Arc<Daemon>>,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Json<ServiceStatus>, HttpError> {
    let action = check(&daemon, path)?;
    let (_, ended) = start(&daemon, action);
    jobs::answer(ended, "service action").await
}

/// `POST /v1/jobs/services/{id}/{action}`: `202` and the job's id once the
/// service and the action are known; the end is the job's `done` with the
/// status, or its `failed` with the action's own code.
async fn service_job(
    State(daemon): State<Arc<Daemon>>,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<(StatusCode, Json<JobAccepted>), HttpError> {
    let action = check(&daemon, path)?;
    Ok(jobs::accepted(start(&daemon, action)))
}

/// A request that names a service that takes the action.
struct Checked {
    /// As the path gave it.
    id: String,
    action: ServiceAction,
}

/// The refusals that need no wait: no such service (404), no such action
/// (400), and a php-fpm pool, which starts and stops with its sites (409).
fn check(
    daemon: &Daemon,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Checked, HttpError> {
    let Path((id, action)) = path.map_err(bad_path)?;
    let (service_id, service) = service(daemon, &id)?;
    let action: ServiceAction = action.parse().map_err(|err: UnknownAction| {
        HttpError::new(StatusCode::BAD_REQUEST, err.code(), err.to_string())
    })?;
    match service {
        Service::Pool(_) => return Err(managed_by_sites(service_id)),
        Service::Worker(_) if action != ServiceAction::Restart => {
            return Err(follows_site(service_id));
        }
        _ => {}
    }
    Ok(Checked { id, action })
}

/// The 409 for a php-fpm pool.
fn managed_by_sites(id: ServiceId) -> HttpError {
    let message = format!(
        "{id} starts and stops with the sites that need its PHP version; it has no actions of \
         its own"
    );
    HttpError::new(StatusCode::CONFLICT, MANAGED_BY_SITES, message)
}

/// The 409 for `start` or `stop` on a site's worker.
fn follows_site(id: ServiceId) -> HttpError {
    let message = format!(
        "{id} runs while its site's setting asks for it; turn the setting on or off instead"
    );
    HttpError::new(StatusCode::CONFLICT, codes::WORKER_FOLLOWS_SITE, message)
}

/// `restart` on a site's worker: the next pass of the sites sync starts it
/// again, and the answer waits for that pass.
async fn restart_worker(daemon: &Daemon, id: ServiceId) -> Result<ServiceStatus, HttpError> {
    daemon.workers().ask_restart(id);
    daemon
        .pass_requests()
        .run_pass()
        .await
        .map_err(|PassNotRun| {
            let message = "the daemon stops: the worker was not started again";
            HttpError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                codes::INTERNAL_ERROR,
                message,
            )
        })?;
    // Gone if its site stopped asking for it in the meantime.
    let runtime = daemon
        .workers()
        .find(id)
        .ok_or_else(|| not_found(&id.to_string()))?;
    Ok(lock(&runtime).status.clone())
}

fn start(daemon: &Arc<Daemon>, checked: Checked) -> (u64, jobs::Ended<ServiceStatus>) {
    jobs::start(
        daemon,
        JobKind::ServiceAction,
        "service action",
        JobOutcome::ServiceAction,
        run(Arc::clone(daemon), checked),
        |_, _| {},
    )
}

/// The action itself, in the job's own task: a client that hangs up cannot
/// stop it, for a database server between the action and the write of
/// `enabled` either.
async fn run(
    daemon: Arc<Daemon>,
    Checked { id, action }: Checked,
) -> Result<ServiceStatus, HttpError> {
    let (id, service) = service(&daemon, &id)?;
    tracing::info!(service = %id, %action, "service action");
    match service {
        Service::Server(server) => server
            .run_and_save(&daemon, action, SERVER_ACTION_WAIT)
            .await
            .map_err(|err| {
                tracing::warn!(service = %id, %action, code = err.code(), error = %err, "service action failed");
                server_error(&err)
            }),
        Service::Dns(dns) => Ok(dns.run(daemon.events(), daemon.shutdown(), action).await),
        Service::Caddy(caddy) => caddy
            .run_waiting(&daemon, action, ACTION_WAIT)
            .await
            .map_err(|err| {
                tracing::warn!(service = %id, %action, code = err.code(), error = %err, "service action failed");
                action_error(&err)
            }),
        Service::Worker(_) => restart_worker(&daemon, id).await,
        // Refused by `check`.
        Service::Pool(_) => Err(managed_by_sites(id)),
    }
}

fn server_error(err: &databases::ActionError) -> HttpError {
    use databases::ActionError as E;
    let status = match err {
        E::Unsupported(_)
        | E::Elevated(_)
        | E::NotInstalled(..)
        | E::InUse(_)
        | E::Busy
        | E::OrphanStuck(..) => StatusCode::CONFLICT,
        E::Config(_) | E::Init(_) | E::ProgramNotAbsolute(_) | E::Internal | E::Path(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    HttpError::new(status, err.code(), err.to_string())
}

#[derive(Debug, Deserialize)]
struct LogsQuery {
    #[serde(default)]
    follow: bool,
}

async fn logs(
    State(daemon): State<Arc<Daemon>>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<LogsQuery>, QueryRejection>,
) -> Result<Response, HttpError> {
    let Path(id) = path.map_err(bad_path)?;
    let (id, service) = service(&daemon, &id)?;
    let Query(query) = query.map_err(bad_query)?;
    if !query.follow {
        return Ok(Json(service.recent_logs()).into_response());
    }
    // Subscribe first, then copy the recent lines: a line printed in between
    // is then in both (and skipped by `seq`), never in neither.
    let receiver = daemon.events().subscribe();
    // Checked after subscribing, as in `/v1/events`: `shutting_down` comes
    // after the token is cancelled.
    let closing = daemon.shutdown().is_cancelled();
    let recent = service.recent_logs();
    let stream = follow(id, recent, receiver, closing);
    Ok(sse(stream, KEEP_ALIVE))
}

/// An SSE response that sends a keep-alive comment after `every` without data.
fn sse(
    stream: impl Stream<Item = Result<SseEvent, Infallible>> + Send + 'static,
    every: Duration,
) -> Response {
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(every))
        .into_response()
}

/// The `?follow=true` body: each line as `data: <LogLine>`, `recent` first,
/// then the new lines of service `id`. A line with a `seq` already sent is
/// skipped (`seq` grows across runs, so this is exact). `: lagged <n>` marks
/// `n` lost events. The stream ends at `shutting_down`, so the server can
/// stop.
fn follow(
    id: ServiceId,
    recent: Vec<LogLine>,
    receiver: broadcast::Receiver<Event>,
    closing: bool,
) -> impl Stream<Item = Result<SseEvent, Infallible>> + Send + 'static {
    let last_sent = recent.last().map(|line| line.seq);
    let live = BroadcastStream::new(receiver)
        // `take(0)` ends at once: the daemon was already stopping, and its
        // `shutting_down` went out before we subscribed.
        .take(if closing { 0 } else { usize::MAX })
        .take_while(|item| !matches!(item, Ok(Event::ShuttingDown)))
        .filter_map(move |item| match item {
            Ok(Event::Log(line))
                if line.service == id && last_sent.is_none_or(|seq| line.seq > seq) =>
            {
                Some(line_event(&line))
            }
            Ok(_) => None,
            Err(BroadcastStreamRecvError::Lagged(missed)) => {
                Some(SseEvent::default().comment(format!("lagged {missed}")))
            }
        });
    tokio_stream::iter(recent)
        .map(|line| line_event(&line))
        .chain(live)
        .map(Ok)
}

fn line_event(line: &LogLine) -> SseEvent {
    SseEvent::default().json_data(line).unwrap_or_else(|err| {
        // `LogLine` holds only strings and numbers; this does not happen.
        tracing::error!(error = %err, "cannot serialize a log line");
        SseEvent::default().comment("line_not_serialized")
    })
}

/// A managed service.
enum Service<'a> {
    Caddy(&'a CaddyManager),
    /// Known whether or not a version is installed: an action then says
    /// `postgres_not_installed`.
    Server(&'a ServerManager),
    /// The DNS server for `.test`, inside the daemon.
    Dns(&'a DnsService),
    /// A php-fpm pool: the routes only read it.
    Pool(Arc<Mutex<ServiceRuntime>>),
    /// A site's worker: the routes read it, and ask the sync to restart it.
    Worker(Arc<Mutex<ServiceRuntime>>),
}

impl Service<'_> {
    /// The last lines, oldest first.
    fn recent_logs(&self) -> Vec<LogLine> {
        match self {
            Service::Caddy(caddy) => caddy.recent_logs(),
            Service::Server(server) => server.recent_logs(),
            Service::Dns(dns) => dns.recent_logs(),
            Service::Pool(runtime) | Service::Worker(runtime) => lock(runtime).logs.recent(),
        }
    }
}

/// Service `id`: Caddy, a database server, the DNS server, or a php-fpm
/// pool started in this run.
fn service<'a>(daemon: &'a Daemon, id: &str) -> Result<(ServiceId, Service<'a>), HttpError> {
    let found = id.parse().ok().and_then(|parsed| {
        let service = match parsed {
            ServiceId::Caddy => Some(Service::Caddy(daemon.caddy())),
            ServiceId::Database(kind) => Some(Service::Server(daemon.server(kind))),
            ServiceId::Dns => daemon.dns().listed().then_some(Service::Dns(daemon.dns())),
            ServiceId::Pool(..) => daemon.pools().find(parsed).map(Service::Pool),
            ServiceId::Worker(..) => daemon.workers().find(parsed).map(Service::Worker),
        };
        service.map(|service| (parsed, service))
    });
    found.ok_or_else(|| not_found(id))
}

fn not_found(id: &str) -> HttpError {
    let message = format!("no service named {id:?}");
    HttpError::new(StatusCode::NOT_FOUND, codes::SERVICE_NOT_FOUND, message)
}

fn action_error(err: &ActionError) -> HttpError {
    let status = match err {
        ActionError::NoCaddy(_) | ActionError::NotInstalled(_) | ActionError::Busy => {
            StatusCode::CONFLICT
        }
        ActionError::ConfigWrite(_) | ActionError::ProgramNotAbsolute(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    HttpError::new(status, err.code(), err.to_string())
}

fn bad_path(rejection: PathRejection) -> HttpError {
    rejection.reject(codes::INVALID_PATH)
}

fn bad_query(rejection: QueryRejection) -> HttpError {
    rejection.reject(codes::INVALID_QUERY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use marsa_core::types::LogStream;

    fn line(service: ServiceId, seq: u64) -> LogLine {
        LogLine {
            service,
            seq,
            at_ms: 0,
            stream: LogStream::Stdout,
            text: format!("line {seq}"),
        }
    }

    fn data(line: &LogLine) -> String {
        format!("data: {}\n\n", serde_json::to_string(line).unwrap())
    }

    async fn text(response: Response) -> String {
        let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn follow_skips_lines_already_sent_and_ends_at_shutting_down() {
        let (sender, receiver) = broadcast::channel(16);
        // Lines 1 and 2 were published after the subscription and before the
        // copy, so they are in both.
        let recent = vec![
            line(ServiceId::Caddy, 0),
            line(ServiceId::Caddy, 1),
            line(ServiceId::Caddy, 2),
        ];
        for event in [
            Event::Log(line(ServiceId::Caddy, 1)),
            Event::Log(line(ServiceId::Caddy, 2)),
            Event::Log(line(ServiceId::Dns, 3)),
            Event::Log(line(ServiceId::Caddy, 3)),
            Event::ShuttingDown,
            Event::Log(line(ServiceId::Caddy, 4)),
        ] {
            sender.send(event).unwrap();
        }
        let stream = follow(ServiceId::Caddy, recent, receiver, false);
        let got = text(sse(stream, KEEP_ALIVE)).await;
        let want: String = (0..=3)
            .map(|seq| data(&line(ServiceId::Caddy, seq)))
            .collect();
        assert_eq!(got, want);
    }

    #[tokio::test]
    async fn follow_during_shutdown_sends_the_recent_lines_and_ends() {
        let (sender, receiver) = broadcast::channel(4);
        let stream = follow(
            ServiceId::Caddy,
            vec![line(ServiceId::Caddy, 7)],
            receiver,
            true,
        );
        assert_eq!(
            text(sse(stream, KEEP_ALIVE)).await,
            data(&line(ServiceId::Caddy, 7))
        );
        drop(sender);
    }

    #[tokio::test]
    async fn a_quiet_follow_stream_sends_keep_alive_comments() {
        let (sender, receiver) = broadcast::channel(4);
        let stream = follow(
            ServiceId::Caddy,
            vec![line(ServiceId::Caddy, 0)],
            receiver,
            false,
        );
        let body = sse(stream, Duration::from_millis(20)).into_body();
        let mut chunks = Body::into_data_stream(body);
        let mut got = String::new();
        while !got.contains(":\n\n") {
            let chunk = tokio::time::timeout(Duration::from_secs(10), chunks.next())
                .await
                .expect("a keep-alive in time")
                .expect("the stream stays open")
                .unwrap();
            got.push_str(std::str::from_utf8(&chunk).unwrap());
        }
        assert!(
            got.starts_with(&data(&line(ServiceId::Caddy, 0))),
            "{got:?}"
        );
        // The route uses the same helper with this interval.
        assert_eq!(KEEP_ALIVE, Duration::from_secs(15));
        drop(sender);
    }

    #[tokio::test]
    async fn pools_are_listed_after_caddy_with_their_logs_and_no_actions() {
        use crate::daemon::http::auth::AuthConfig;
        use crate::daemon::http::routes::router;
        use axum::http::Request;
        use marsa_core::php::PhpMinor;
        use marsa_core::platform::daemon_file::{Token, daemon_authority};
        use marsa_core::types::{ErrorBody, ServiceState};
        use tower::ServiceExt as _;

        const PORT: u16 = 51236;
        let token = Token::generate().unwrap();
        let bearer = token.bearer_header();
        let daemon = Arc::new(Daemon::new(std::path::PathBuf::new()));
        let php84 = PhpMinor { major: 8, minor: 4 };
        let runtime = daemon
            .pools()
            .runtime(crate::daemon::services::php::fpm_id(php84), php84);
        // `php-fpm-8.4`, or `php-cgi-8.4` on Windows.
        let pool = crate::daemon::services::php::fpm_id(PhpMinor { major: 8, minor: 4 });
        let other = crate::daemon::services::php::fpm_id(PhpMinor { major: 8, minor: 3 });
        let ready = {
            let mut runtime = lock(&runtime);
            runtime.status.state = ServiceState::Running;
            runtime.status.pid = Some(4242);
            runtime.logs.push(LogStream::Stderr, "fpm is running", 7)
        };
        let app = router(daemon.clone(), AuthConfig::new(Arc::new(token), PORT));
        let send = async |method: &str, path: &str| {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header("host", daemon_authority(PORT))
                .header("authorization", &bearer)
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            (response.status(), text(response).await)
        };

        let (status, body) = send("GET", "/v1/services").await;
        assert_eq!(status, StatusCode::OK);
        let list: Vec<ServiceStatus> = serde_json::from_str(&body).unwrap();
        let ids: Vec<_> = list.iter().map(|s| s.id).collect();
        assert_eq!(ids, [ServiceId::Caddy, pool]);
        assert_eq!(list[1].state, ServiceState::Running);
        assert_eq!(list[1].pid, Some(4242));

        let (status, body) = send("GET", &format!("/v1/logs/{pool}")).await;
        assert_eq!(status, StatusCode::OK);
        let lines: Vec<LogLine> = serde_json::from_str(&body).unwrap();
        assert_eq!(lines, std::slice::from_ref(&ready));
        // Follow during a shutdown: the recent lines, then the end.
        daemon.shutdown().cancel();
        let (status, body) = send("GET", &format!("/v1/logs/{pool}?follow=true")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, data(&ready));

        let cases = [
            (
                "POST",
                format!("/v1/services/{pool}/start"),
                409,
                MANAGED_BY_SITES,
            ),
            (
                "POST",
                format!("/v1/services/{pool}/stop"),
                409,
                MANAGED_BY_SITES,
            ),
            (
                "POST",
                format!("/v1/services/{pool}/restart"),
                409,
                MANAGED_BY_SITES,
            ),
            (
                "POST",
                format!("/v1/services/{pool}/kill"),
                400,
                "unknown_action",
            ),
            (
                "POST",
                format!("/v1/services/{other}/start"),
                404,
                "service_not_found",
            ),
            ("GET", format!("/v1/logs/{other}"), 404, "service_not_found"),
        ];
        // The job routes refuse the same requests at once, with no job.
        let mut all = Vec::new();
        for (method, path, want, code) in cases {
            if method == "POST" {
                let job = path.replacen("/v1/services/", "/v1/jobs/services/", 1);
                all.push((method, job, want, code));
            }
            all.push((method, path, want, code));
        }
        for (method, path, want, code) in all {
            let request = format!("{method} {path}");
            let (status, body) = send(method, &path).await;
            assert_eq!(status.as_u16(), want, "{request}: {body}");
            let error: ErrorBody = serde_json::from_str(&body).unwrap();
            assert_eq!(error.error.code, code, "{request}");
        }
        // Refused before any job: the first id is still free.
        assert_eq!(daemon.jobs().get(1), None);
        // The actions changed nothing.
        assert_eq!(lock(&runtime).status.state, ServiceState::Running);
    }

    /// PostgreSQL is listed once a version is installed, has its actions
    /// and its logs, and a stop turns it off in `global.toml`.
    #[tokio::test]
    async fn postgres_is_listed_when_installed_and_a_stop_turns_it_off() {
        use crate::daemon::http::auth::AuthConfig;
        use crate::daemon::http::routes::router;
        use axum::http::Request;
        use marsa_core::config::{self, ConfigPaths, GlobalConfig, ServerSettings};
        use marsa_core::platform::daemon_file::{Token, daemon_authority};
        use marsa_core::types::{ErrorBody, ServiceState};
        use tower::ServiceExt as _;

        const PORT: u16 = 51237;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_path_buf();
        let global = ConfigPaths::in_dir(&data).global;
        let on = GlobalConfig {
            postgres: ServerSettings {
                default: Some("18".into()),
                enabled: true,
                port: None,
            },
            ..GlobalConfig::default()
        };
        config::save_global(&global, &on).unwrap();
        let token = Token::generate().unwrap();
        let bearer = token.bearer_header();
        let daemon = Arc::new(Daemon::new(data.clone()));
        let app = router(daemon.clone(), AuthConfig::new(Arc::new(token), PORT));
        let send = async |method: &str, path: &str| {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header("host", daemon_authority(PORT))
                .header("authorization", &bearer)
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            (response.status(), text(response).await)
        };
        let ids = async || {
            let (status, body) = send("GET", "/v1/services").await;
            assert_eq!(status, StatusCode::OK);
            let list: Vec<ServiceStatus> = serde_json::from_str(&body).unwrap();
            list.into_iter().map(|s| s.id).collect::<Vec<_>>()
        };

        // Not installed: not listed, but known.
        assert_eq!(ids().await, [ServiceId::Caddy]);
        let (status, body) = send("GET", "/v1/logs/postgres").await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "[]"));

        // A stop of a PostgreSQL that does not run: stopped, and off.
        let (status, body) = send("POST", "/v1/services/postgres/stop").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let stopped: ServiceStatus = serde_json::from_str(&body).unwrap();
        assert_eq!(stopped.state, ServiceState::Stopped);
        assert!(!config::load_global(&global).unwrap().postgres.enabled);

        // Installed (an empty version folder): listed, stopped.
        std::fs::create_dir_all(data.join("runtimes/postgres/18.7.0")).unwrap();
        assert_eq!(
            ids().await,
            [ServiceId::Caddy, ServiceId::Database(ServerKind::Postgres)]
        );

        // A start without its program: refused, and still off.
        let (status, body) = send("POST", "/v1/services/postgres/start").await;
        let error: ErrorBody = serde_json::from_str(&body).unwrap();
        assert_eq!(
            (status, error.error.code.as_str()),
            (StatusCode::CONFLICT, "postgres_not_installed")
        );
        assert!(!config::load_global(&global).unwrap().postgres.enabled);
        // The job route: `202` at once, and the refusal of the action is the
        // job's `failed`, with the status the old route answered.
        let mut events = daemon.events().subscribe();
        let (status, body) = send("POST", "/v1/jobs/services/postgres/start").await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let accepted: marsa_core::types::JobAccepted = serde_json::from_str(&body).unwrap();
        let last = loop {
            match events.recv().await.unwrap() {
                Event::Job(event) if event.is_last() && event.id == accepted.id => break event,
                _ => {}
            }
        };
        let marsa_core::types::JobUpdate::Failed(failure) = last.update else {
            panic!("not failed: {last:?}");
        };
        assert_eq!(failure.code, "postgres_not_installed");
        assert_eq!(failure.http_status, Some(409));
        let (status, body) = send("POST", "/v1/services/postgres/kill").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        // Valkey the same way, with its own codes.
        std::fs::create_dir_all(data.join("runtimes/valkey/8.1.11")).unwrap();
        assert_eq!(
            ids().await,
            [
                ServiceId::Caddy,
                ServiceId::Database(ServerKind::Postgres),
                ServiceId::Database(ServerKind::Valkey)
            ]
        );
        let (status, body) = send("POST", "/v1/services/valkey/start").await;
        let error: ErrorBody = serde_json::from_str(&body).unwrap();
        assert_eq!(
            (status, error.error.code.as_str()),
            (StatusCode::CONFLICT, "valkey_not_installed")
        );
        assert!(!config::load_global(&global).unwrap().valkey.enabled);
    }

    #[tokio::test]
    async fn a_held_dns_port_is_listed_as_failed_and_a_start_tries_again() {
        use crate::daemon::http::auth::AuthConfig;
        use crate::daemon::http::routes::router;
        use crate::daemon::services::dns::held_dns_port;
        use axum::http::Request;
        use marsa_core::platform::daemon_file::{Token, daemon_authority};
        use marsa_core::types::ServiceState;
        use tower::ServiceExt as _;

        const PORT: u16 = 51237;
        let token = Token::generate().unwrap();
        let bearer = token.bearer_header();
        // A port free for TCP too: on Windows a UDP port picked alone may be
        // in a reserved TCP range, and the last start would fail on it.
        let held = held_dns_port().await;
        let mut daemon = Daemon::new(std::path::PathBuf::new());
        daemon.set_dns(DnsService::new(Some(held.0.local_addr().unwrap())));
        let daemon = Arc::new(daemon);
        daemon
            .dns()
            .run(daemon.events(), daemon.shutdown(), ServiceAction::Start)
            .await;
        let app = router(daemon.clone(), AuthConfig::new(Arc::new(token), PORT));
        let send = async |method: &str, path: &str| {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header("host", daemon_authority(PORT))
                .header("authorization", &bearer)
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            (response.status(), text(response).await)
        };
        let failed = ServiceState::Failed {
            code: codes::DNS_PORT_IN_USE.into(),
        };

        let (status, body) = send("GET", "/v1/services").await;
        assert_eq!(status, StatusCode::OK);
        let list: Vec<ServiceStatus> = serde_json::from_str(&body).unwrap();
        let ids: Vec<_> = list.iter().map(|s| s.id).collect();
        assert_eq!(ids, [ServiceId::Caddy, ServiceId::Dns]);
        assert_eq!(list[1].state, failed);
        let (status, body) = send("GET", "/v1/logs/dns").await;
        assert_eq!(status, StatusCode::OK);
        let lines: Vec<LogLine> = serde_json::from_str(&body).unwrap();
        assert!(lines[0].text.contains("cannot bind UDP"), "{lines:?}");

        // Still held: the start fails again, with the same code.
        let (status, body) = send("POST", "/v1/services/dns/start").await;
        assert_eq!(status, StatusCode::OK);
        let after: ServiceStatus = serde_json::from_str(&body).unwrap();
        assert_eq!(after.state, failed);
        drop(held);
        let (_, body) = send("POST", "/v1/services/dns/start").await;
        let after: ServiceStatus = serde_json::from_str(&body).unwrap();
        // The server's own line says why, if it did not start.
        let (_, lines) = send("GET", "/v1/logs/dns").await;
        assert_eq!(after.state, ServiceState::Running, "{lines}");
        daemon.dns().shutdown(Duration::from_secs(5)).await;
    }

    /// A job route is behind the same auth layer as every other route: no
    /// token, a wrong token, an `Origin`, and another `Host` all refuse
    /// before any job exists.
    #[tokio::test]
    async fn the_job_route_needs_the_token_and_refuses_an_origin() {
        use crate::daemon::http::test_router::TestRouter;

        let daemon = Arc::new(Daemon::new(std::path::PathBuf::new()));
        let app = TestRouter::new(Arc::clone(&daemon));
        app.assert_refused_without_auth("POST", "/v1/jobs/services/caddy/start")
            .await;
        assert_eq!(daemon.jobs().get(1), None);
    }

    /// Clients before ADR 0026 wait 60 s for an action on Caddy and 300 s
    /// for one on a database server (their old timeouts); the daemon's own
    /// limits must stay under that, so an old client still gets its answer.
    #[test]
    fn old_clients_wait_longer_than_the_slowest_action() {
        const OLD_ACTION_WAIT: u64 = 60;
        const OLD_SERVER_WAIT: u64 = 300;
        // Waiting 20 for another action, then a restart that stops an old
        // Caddy first: about 23.
        assert!(OLD_ACTION_WAIT > ACTION_WAIT.as_secs() + 23);
        // PostgreSQL: waiting 30, stopping 23, then lsof 8, initdb 120,
        // single-user 60, ready 15, and the stop of a server that never
        // answered, 23.
        assert!(OLD_SERVER_WAIT > SERVER_ACTION_WAIT.as_secs() + 23 + 8 + 120 + 60 + 15 + 23);
        // MySQL: waiting 30, stopping 35, lsof 8, init 120, ready 15, stop 35.
        assert!(OLD_SERVER_WAIT > SERVER_ACTION_WAIT.as_secs() + 35 + 8 + 120 + 15 + 35);
        // Valkey: waiting 30, stopping 29 (the request, SIGTERM, and the
        // margin), lsof 8, ready 15, stop 29. More than the 60 s of the
        // other services, so it needs the longer wait.
        let valkey = SERVER_ACTION_WAIT.as_secs() + 29 + 8 + 15 + 29;
        assert!(OLD_SERVER_WAIT > valkey);
        assert!(OLD_ACTION_WAIT < valkey);
    }

    #[tokio::test]
    async fn no_dns_service_where_it_is_not_used() {
        let daemon = Arc::new(Daemon::new(std::path::PathBuf::new()));
        let Json(list) = services(State(daemon.clone())).await;
        assert!(list.iter().all(|s| s.id != ServiceId::Dns));
        assert!(service(&daemon, "dns").is_err());
    }
}
