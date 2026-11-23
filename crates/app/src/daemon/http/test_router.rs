//! The real router with the real auth layer, for the tests of the routes:
//! one token, one `Host`, no socket.

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{StatusCode, request};
use axum::response::Response;
use marsa_core::codes;
use marsa_core::platform::daemon_file::{Token, daemon_authority};
use marsa_core::types::ErrorBody;
use serde_json::Value;
use tower::ServiceExt as _;

use super::auth::AuthConfig;
use super::routes::router;
use crate::daemon::state::Daemon;

/// The daemon port in the `Host` header. No socket is bound for it.
const PORT: u16 = 51236;

pub(crate) struct TestRouter {
    router: Router,
    bearer: String,
}

impl TestRouter {
    pub(crate) fn new(daemon: Arc<Daemon>) -> Self {
        let token = Token::generate().unwrap();
        let bearer = token.bearer_header();
        Self {
            router: router(daemon, AuthConfig::new(Arc::new(token), PORT)),
            bearer,
        }
    }

    /// A request with the right `Host` and no token.
    pub(crate) fn builder(&self, method: &str, uri: &str) -> request::Builder {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("host", daemon_authority(PORT))
    }

    /// A request with the right `Host` and the token.
    pub(crate) fn authorized(&self, method: &str, uri: &str) -> request::Builder {
        self.builder(method, uri)
            .header("authorization", &self.bearer)
    }

    pub(crate) async fn call(&self, request: Request) -> Response {
        self.router.clone().oneshot(request).await.unwrap()
    }

    /// One request with the token; a JSON body, if given, comes with its
    /// `content-type`.
    pub(crate) async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> Response {
        let mut request = self.authorized(method, uri);
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let body = body.map_or_else(Body::empty, |json| Body::from(json.to_string()));
        self.call(request.body(body).unwrap()).await
    }

    /// The refusals of the auth layer for `method uri`: no token and a wrong
    /// token (401 `unauthorized`), an `Origin` (403 `origin_rejected`), and
    /// another port or `localhost` in `Host` (403 `host_rejected`). The
    /// status and the code of each answer are checked here, so a handler's
    /// own 401 or 403 cannot pass for the auth layer's; the caller checks
    /// that nothing changed.
    pub(crate) async fn assert_refused_without_auth(&self, method: &str, uri: &str) {
        let wrong = Token::generate().unwrap().bearer_header();
        let with_host = |host: String| {
            Request::builder()
                .method(method)
                .uri(uri)
                .header("host", host)
                .header("authorization", &self.bearer)
        };
        let unauthorized = (StatusCode::UNAUTHORIZED, codes::UNAUTHORIZED);
        let host_rejected = (StatusCode::FORBIDDEN, codes::HOST_REJECTED);
        let cases = [
            ("no token", self.builder(method, uri), unauthorized),
            (
                "a wrong token",
                self.builder(method, uri).header("authorization", &wrong),
                unauthorized,
            ),
            (
                "an Origin",
                self.authorized(method, uri)
                    .header("origin", "http://evil.test"),
                (StatusCode::FORBIDDEN, codes::ORIGIN_REJECTED),
            ),
            (
                "another port in Host",
                with_host(daemon_authority(PORT + 1)),
                host_rejected,
            ),
            (
                "localhost in Host",
                with_host(format!("localhost:{PORT}")),
                host_rejected,
            ),
        ];
        for (name, request, (status, code)) in cases {
            let response = self.call(request.body(Body::empty()).unwrap()).await;
            assert_eq!(response.status(), status, "{name}: {method} {uri}");
            let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
            let body: ErrorBody = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body.error.code, code, "{name}: {method} {uri}");
        }
    }

    /// The status and the JSON body of `send`.
    pub(crate) async fn answer(
        &self,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let response = self.send(method, uri, body).await;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}
