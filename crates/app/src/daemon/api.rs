//! The router, and the health, shutdown, and events routes. Task M3-06.
//!
//! Every route, the fallbacks included, sits behind `require_auth` (task
//! M3-03). Handlers reach the daemon through `State<Arc<Daemon>>`.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::middleware::from_fn_with_state;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use marsa_core::types::{API_VERSION, ErrorBody, Event, Health, VersionInUse};
use tokio_stream::Stream;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

use super::auth::{AuthConfig, require_auth};
use super::state::Daemon;
use super::{runtimes_api, services_api, sites_api};

const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// The whole API.
pub fn router(daemon: Arc<Daemon>, auth: AuthConfig) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/shutdown", post(shutdown))
        .route("/v1/events", get(events))
        // More routes go here, as a `Router<Arc<Daemon>>`. Before the
        // fallbacks, so `method_not_allowed_fallback` covers them too.
        .merge(services_api::routes())
        .merge(runtimes_api::routes())
        .merge(sites_api::routes())
        .merge(super::setup_api::routes())
        .merge(super::autostart_api::routes())
        .merge(super::xdebug_api::routes())
        .method_not_allowed_fallback(method_not_allowed)
        .fallback(not_found)
        .with_state(daemon)
        // Last, so it wraps every route and both fallbacks.
        .layer(from_fn_with_state(auth, require_auth))
}

/// An error response. Handlers return `Result<_, HttpError>`, so every error
/// reaches the client as an `ErrorBody` through this one conversion.
#[derive(Debug)]
pub struct HttpError {
    pub status: StatusCode,
    /// Stable error name, usually from the error's `code()`.
    pub code: &'static str,
    /// English, for logs.
    pub message: String,
    /// The facts of `version_in_use` (task M5-07). Boxed, so every
    /// `Result<_, HttpError>` stays small.
    pub in_use: Option<Box<VersionInUse>>,
}

impl HttpError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            in_use: None,
        }
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let mut body = ErrorBody::new(self.code, self.message);
        body.in_use = self.in_use.map(|in_use| *in_use);
        (self.status, Json(body)).into_response()
    }
}

async fn health() -> Json<Health> {
    Json(Health {
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        api_version: API_VERSION,
        pid: std::process::id(),
    })
}

/// Answers first; the server task does the shutdown.
async fn shutdown(State(daemon): State<Arc<Daemon>>) -> StatusCode {
    tracing::info!("shutdown requested over the API");
    daemon.shutdown.cancel();
    StatusCode::ACCEPTED
}

async fn events(State(daemon): State<Arc<Daemon>>) -> impl IntoResponse {
    // Subscribe first, then check the token. `shutting_down` is published
    // after the token is cancelled, so this reader either receives it or
    // sees the token cancelled here.
    let receiver = daemon.events.subscribe();
    let stream = if daemon.shutdown.is_cancelled() {
        EventStream::Closing
    } else {
        EventStream::Live(BroadcastStream::new(receiver))
    };
    Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
}

async fn not_found(uri: Uri) -> HttpError {
    let message = format!("no route for {}", uri.path());
    HttpError::new(StatusCode::NOT_FOUND, "not_found", message)
}

async fn method_not_allowed() -> HttpError {
    let message = "the route does not accept this method";
    HttpError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        message,
    )
}

/// The `/v1/events` body: each event as `data: <json>`, a `: lagged <n>`
/// comment when this reader fell `n` events behind, and the end right after
/// `shutting_down`, so the server can stop.
enum EventStream {
    Live(BroadcastStream<Event>),
    /// The daemon was already stopping: send `shutting_down` alone.
    Closing,
    Done,
}

impl Stream for EventStream {
    type Item = Result<SseEvent, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let next = match &mut *self {
            EventStream::Live(events) => ready!(Pin::new(events).poll_next(cx)),
            EventStream::Closing => Some(Ok(Event::ShuttingDown)),
            EventStream::Done => None,
        };
        let sse = match next {
            None => {
                *self = EventStream::Done;
                return Poll::Ready(None);
            }
            Some(Ok(event)) => {
                if event == Event::ShuttingDown {
                    *self = EventStream::Done;
                }
                SseEvent::default().json_data(&event).unwrap_or_else(|err| {
                    // `Event` holds only strings and numbers; this does not happen.
                    tracing::error!(error = %err, "cannot serialize an event");
                    SseEvent::default().comment("event_not_serialized")
                })
            }
            Some(Err(BroadcastStreamRecvError::Lagged(missed))) => {
                SseEvent::default().comment(format!("lagged {missed}"))
            }
        };
        Poll::Ready(Some(Ok(sse)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use axum::body::{Body, to_bytes};
    use axum::extract::Request;
    use marsa_core::platform::daemon_file::{Token, daemon_authority};
    use marsa_core::types::{LogLine, LogStream};
    use tokio::sync::broadcast;
    use tower::ServiceExt;

    const PORT: u16 = 51234;

    fn log(seq: u64) -> Event {
        Event::Log(LogLine {
            service: "caddy".into(),
            seq,
            at_ms: 0,
            stream: LogStream::Stdout,
            text: format!("line {seq}"),
        })
    }

    fn data(event: &Event) -> String {
        format!("data: {}\n\n", serde_json::to_string(event).unwrap())
    }

    async fn text(response: Response) -> String {
        let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn events_report_a_lag_and_end_after_shutting_down() {
        let (sender, receiver) = broadcast::channel(4);
        for event in [log(0), log(1), log(2), log(3), log(4)] {
            sender.send(event).unwrap();
        }
        sender.send(Event::ShuttingDown).unwrap();
        sender.send(log(9)).unwrap();

        // The channel keeps the last four; the first three are lost.
        let stream = EventStream::Live(BroadcastStream::new(receiver));
        let got = text(Sse::new(stream).into_response()).await;
        let expected = format!(
            ": lagged 3\n\n{}{}{}",
            data(&log(3)),
            data(&log(4)),
            data(&Event::ShuttingDown)
        );
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn shutdown_answers_202_and_late_readers_get_shutting_down() {
        let token = Token::generate().unwrap();
        let bearer = token.bearer_header();
        let daemon = Arc::new(Daemon::new(PathBuf::new()));
        let app = router(daemon.clone(), AuthConfig::new(Arc::new(token), PORT));
        let send = |method: &str, path: &str| {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header("host", daemon_authority(PORT))
                .header("authorization", &bearer)
                .body(Body::empty())
                .unwrap();
            app.clone().oneshot(request)
        };

        let response = send("GET", "/v1/shutdown").await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        let error: ErrorBody = serde_json::from_str(&text(response).await).unwrap();
        assert_eq!(error.error.code, "method_not_allowed");
        assert!(!daemon.shutdown.is_cancelled());

        let response = send("POST", "/v1/shutdown").await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(text(response).await, "");
        assert!(daemon.shutdown.is_cancelled());

        let response = send("GET", "/v1/events").await.unwrap();
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        assert_eq!(text(response).await, data(&Event::ShuttingDown));
    }
}
