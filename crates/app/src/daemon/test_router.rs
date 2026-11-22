//! The real router with the real auth layer, for the tests of the routes:
//! one token, one `Host`, no socket.

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{StatusCode, request};
use axum::response::Response;
use marsa_core::platform::daemon_file::{Token, daemon_authority};
use serde_json::Value;
use tower::ServiceExt as _;

use super::api::router;
use super::auth::AuthConfig;
use super::state::Daemon;

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
