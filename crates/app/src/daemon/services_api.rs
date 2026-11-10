//! The services and logs routes. Task M3-07; the php-fpm pools since M4-23.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/services` | `Vec<ServiceStatus>`: Caddy, then each pool |
//! | `POST /v1/services/{id}/{action}` | `ServiceStatus` after the action |
//! | `GET /v1/logs/{id}` | `Vec<LogLine>`, the recent lines |
//! | `GET /v1/logs/{id}?follow=true` | SSE: the recent lines, then new ones |
//!
//! The services are `caddy` and each php-fpm pool started in this run
//! (`php-fpm-8.4`). The sites sync starts and stops the pools, so an action
//! on a pool is `409 service_managed_by_sites`.
//!
//! Every error, the extractors' own included, is an `ErrorBody`.

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
use marsa_core::types::{Event, LogLine, ServiceAction, ServiceStatus, UnknownAction};
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::{Stream, StreamExt as _};

use super::api::HttpError;
use super::caddy::{ActionError, CADDY_ID, CaddyManager};
use super::state::Daemon;
use super::supervisor::{ServiceRuntime, lock};

/// The answer to an action on a service that the sites sync starts and stops.
pub const MANAGED_BY_SITES: &str = "service_managed_by_sites";

/// A `:` comment this often on a quiet stream, as on `/v1/events`: the
/// client gives up after 45 s without a byte.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// The routes, for `api::router` to merge before its fallbacks.
pub fn routes() -> Router<Arc<Daemon>> {
    Router::new()
        .route("/v1/services", get(services))
        .route("/v1/services/{id}/{action}", post(service_action))
        .route("/v1/logs/{id}", get(logs))
}

async fn services(State(daemon): State<Arc<Daemon>>) -> Json<Vec<ServiceStatus>> {
    let mut services = vec![daemon.caddy.status()];
    services.extend(daemon.pools.statuses());
    Json(services)
}

async fn service_action(
    State(daemon): State<Arc<Daemon>>,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Json<ServiceStatus>, HttpError> {
    let Path((id, action)) = path.map_err(bad_path)?;
    let service = service(&daemon, &id)?;
    let action: ServiceAction = action.parse().map_err(|err: UnknownAction| {
        HttpError::new(StatusCode::BAD_REQUEST, err.code(), err.to_string())
    })?;
    let Service::Caddy(caddy) = service else {
        let message = format!(
            "{id} starts and stops with the sites that need its PHP version; it has no \
             actions of its own"
        );
        return Err(HttpError::new(
            StatusCode::CONFLICT,
            MANAGED_BY_SITES,
            message,
        ));
    };
    tracing::info!(service = %id, %action, "service action");
    match caddy.run(&daemon, action).await {
        Ok(status) => Ok(Json(status)),
        Err(err) => {
            tracing::warn!(service = %id, %action, code = err.code(), error = %err, "service action failed");
            Err(action_error(&err))
        }
    }
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
    let service = service(&daemon, &id)?;
    let Query(query) = query.map_err(bad_query)?;
    if !query.follow {
        return Ok(Json(service.recent_logs()).into_response());
    }
    // Subscribe first, then copy the recent lines: a line printed in between
    // is then in both (and skipped by `seq`), never in neither.
    let receiver = daemon.events.subscribe();
    // Checked after subscribing, as in `/v1/events`: `shutting_down` comes
    // after the token is cancelled.
    let closing = daemon.shutdown.is_cancelled();
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
    id: String,
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
    /// A php-fpm pool: the routes only read it.
    Pool(Arc<Mutex<ServiceRuntime>>),
}

impl Service<'_> {
    /// The last lines, oldest first.
    fn recent_logs(&self) -> Vec<LogLine> {
        match self {
            Service::Caddy(caddy) => caddy.recent_logs(),
            Service::Pool(runtime) => lock(runtime).logs.recent(),
        }
    }
}

/// Service `id`: Caddy, or a php-fpm pool started in this run.
fn service<'a>(daemon: &'a Daemon, id: &str) -> Result<Service<'a>, HttpError> {
    if id == CADDY_ID {
        return Ok(Service::Caddy(&daemon.caddy));
    }
    if let Some(runtime) = daemon.pools.find(id) {
        return Ok(Service::Pool(runtime));
    }
    let message = format!("no service named {id:?}");
    Err(HttpError::new(
        StatusCode::NOT_FOUND,
        "service_not_found",
        message,
    ))
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
    HttpError::new(rejection.status(), "invalid_path", rejection.body_text())
}

fn bad_query(rejection: QueryRejection) -> HttpError {
    HttpError::new(rejection.status(), "invalid_query", rejection.body_text())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use marsa_core::types::LogStream;

    fn line(service: &str, seq: u64) -> LogLine {
        LogLine {
            service: service.into(),
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
        let recent = vec![line("caddy", 0), line("caddy", 1), line("caddy", 2)];
        for event in [
            Event::Log(line("caddy", 1)),
            Event::Log(line("caddy", 2)),
            Event::Log(line("php", 3)),
            Event::Log(line("caddy", 3)),
            Event::ShuttingDown,
            Event::Log(line("caddy", 4)),
        ] {
            sender.send(event).unwrap();
        }
        let stream = follow("caddy".into(), recent, receiver, false);
        let got = text(sse(stream, KEEP_ALIVE)).await;
        let want: String = (0..=3).map(|seq| data(&line("caddy", seq))).collect();
        assert_eq!(got, want);
    }

    #[tokio::test]
    async fn follow_during_shutdown_sends_the_recent_lines_and_ends() {
        let (sender, receiver) = broadcast::channel(4);
        let stream = follow("caddy".into(), vec![line("caddy", 7)], receiver, true);
        assert_eq!(text(sse(stream, KEEP_ALIVE)).await, data(&line("caddy", 7)));
        drop(sender);
    }

    #[tokio::test]
    async fn a_quiet_follow_stream_sends_keep_alive_comments() {
        let (sender, receiver) = broadcast::channel(4);
        let stream = follow("caddy".into(), vec![line("caddy", 0)], receiver, false);
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
        assert!(got.starts_with(&data(&line("caddy", 0))), "{got:?}");
        // The route uses the same helper with this interval.
        assert_eq!(KEEP_ALIVE, Duration::from_secs(15));
        drop(sender);
    }

    #[tokio::test]
    async fn pools_are_listed_after_caddy_with_their_logs_and_no_actions() {
        use crate::daemon::api::router;
        use crate::daemon::auth::AuthConfig;
        use axum::http::Request;
        use marsa_core::php::PhpMinor;
        use marsa_core::platform::daemon_file::{Token, daemon_authority};
        use marsa_core::types::{ErrorBody, ServiceState};
        use tower::ServiceExt as _;

        const PORT: u16 = 51236;
        let token = Token::generate().unwrap();
        let bearer = token.bearer_header();
        let daemon = Arc::new(Daemon::new(std::path::PathBuf::new()));
        let runtime = daemon.pools.runtime(PhpMinor { major: 8, minor: 4 });
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
        let ids: Vec<_> = list.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["caddy", "php-fpm-8.4"]);
        assert_eq!(list[1].state, ServiceState::Running);
        assert_eq!(list[1].pid, Some(4242));

        let (status, body) = send("GET", "/v1/logs/php-fpm-8.4").await;
        assert_eq!(status, StatusCode::OK);
        let lines: Vec<LogLine> = serde_json::from_str(&body).unwrap();
        assert_eq!(lines, std::slice::from_ref(&ready));
        // Follow during a shutdown: the recent lines, then the end.
        daemon.shutdown.cancel();
        let (status, body) = send("GET", "/v1/logs/php-fpm-8.4?follow=true").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, data(&ready));

        let cases = [
            ("POST /v1/services/php-fpm-8.4/start", 409, MANAGED_BY_SITES),
            ("POST /v1/services/php-fpm-8.4/stop", 409, MANAGED_BY_SITES),
            (
                "POST /v1/services/php-fpm-8.4/restart",
                409,
                MANAGED_BY_SITES,
            ),
            ("POST /v1/services/php-fpm-8.4/kill", 400, "unknown_action"),
            (
                "POST /v1/services/php-fpm-8.3/start",
                404,
                "service_not_found",
            ),
            ("GET /v1/logs/php-fpm-8.3", 404, "service_not_found"),
        ];
        for (request, want, code) in cases {
            let (method, path) = request.split_once(' ').unwrap();
            let (status, body) = send(method, path).await;
            assert_eq!(status.as_u16(), want, "{request}: {body}");
            let error: ErrorBody = serde_json::from_str(&body).unwrap();
            assert_eq!(error.error.code, code, "{request}");
        }
        // The actions changed nothing.
        assert_eq!(lock(&runtime).status.state, ServiceState::Running);
    }
}
