//! The checks on every request to `marsad`, before any route (architecture.md
//! section 5, "Security"). Task M3-03 (sensitive).
//!
//! In this order:
//!
//! 1. No `Origin` header. Any web page in a browser can send requests to
//!    `127.0.0.1`. Browsers add `Origin` to cross-site `fetch` calls and form
//!    posts; our clients never send it.
//! 2. `Host` is exactly `127.0.0.1:<port>`. This stops DNS rebinding, where a
//!    page's own host name is pointed at `127.0.0.1`.
//! 3. `Authorization: Bearer <token>`. Only programs that can read
//!    `run/daemon.json` know the token.
//!
//! There is no CORS: a browser never gets a way in.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use marsa_core::platform::daemon_file::{Token, daemon_authority};
use marsa_core::types::ErrorBody;

/// What [`require_auth`] checks against. `Debug` is safe: the token prints
/// redacted.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    token: Arc<Token>,
    /// `127.0.0.1:<port>`, built once.
    host: String,
}

impl AuthConfig {
    /// `port` is the port the daemon listens on.
    pub fn new(token: Arc<Token>, port: u16) -> Self {
        Self {
            token,
            host: daemon_authority(port),
        }
    }
}

/// The middleware, for `axum::middleware::from_fn_with_state(config,
/// require_auth)`. Add it with `Router::layer` after the last route and the
/// fallback, so that every request passes through it.
///
/// Rejections are logged at `debug`, not `warn`: any web page can send them
/// without limit, and they must not flood the log. The line names the error
/// code and the path only, never a header value.
pub async fn require_auth(
    State(config): State<AuthConfig>,
    request: Request,
    next: Next,
) -> Response {
    match check(&config, &request) {
        Ok(()) => next.run(request).await,
        Err(rejection) => {
            tracing::debug!(
                code = rejection.code,
                path = %request.uri().path(),
                "request rejected"
            );
            rejection.into_response()
        }
    }
}

struct Rejection {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

const ORIGIN_REJECTED: Rejection = Rejection {
    status: StatusCode::FORBIDDEN,
    code: "origin_rejected",
    message: "requests with an Origin header are not accepted",
};

const HOST_REJECTED: Rejection = Rejection {
    status: StatusCode::FORBIDDEN,
    code: "host_rejected",
    message: "the Host header must be 127.0.0.1 and the daemon's port",
};

const UNAUTHORIZED: Rejection = Rejection {
    status: StatusCode::UNAUTHORIZED,
    code: "unauthorized",
    message: "a valid bearer token is required",
};

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        let body = Json(ErrorBody::new(self.code, self.message));
        if self.status == StatusCode::UNAUTHORIZED {
            // A 401 must name the scheme it wants (RFC 9110 section 11.6.1).
            (self.status, [(header::WWW_AUTHENTICATE, "Bearer")], body).into_response()
        } else {
            (self.status, body).into_response()
        }
    }
}

fn check(config: &AuthConfig, request: &Request) -> Result<(), Rejection> {
    let headers = request.headers();
    if headers.contains_key(header::ORIGIN) {
        return Err(ORIGIN_REJECTED);
    }

    // An absolute request target (`GET http://other/ HTTP/1.1`) carries a
    // host too, and it must agree. HTTP/2 may send no `Host` at all; that
    // fails closed here (axum serves HTTP/1 only with its default features).
    let host = config.host.as_str();
    let host_ok = single(headers, header::HOST) == Some(host.as_bytes())
        && request
            .uri()
            .authority()
            .is_none_or(|authority| authority.as_str() == host);
    if !host_ok {
        return Err(HOST_REJECTED);
    }

    let token_ok = single(headers, header::AUTHORIZATION)
        .and_then(|value| value.strip_prefix(b"Bearer "))
        .and_then(|token| std::str::from_utf8(token).ok())
        .is_some_and(|token| config.token.verify(token));
    if !token_ok {
        return Err(UNAUTHORIZED);
    }
    Ok(())
}

/// The value of a header that appears exactly once. A repeated `Host` or
/// `Authorization` is refused, so no two layers can read different values.
fn single(headers: &HeaderMap, name: HeaderName) -> Option<&[u8]> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?;
    values.next().is_none().then(|| first.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::Mutex;

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::HeaderValue;
    use axum::middleware::from_fn_with_state;
    use axum::routing::get;
    use tower::ServiceExt;

    const PORT: u16 = 51234;
    const PATH: &str = "/v1/health";

    fn app(token: Token) -> Router {
        let config = AuthConfig::new(Arc::new(token), PORT);
        Router::new()
            .route(PATH, get(|| async { "ok" }))
            .fallback(|| async {
                let body = ErrorBody::new("not_found", "no such route");
                (StatusCode::NOT_FOUND, Json(body))
            })
            .layer(from_fn_with_state(config, require_auth))
    }

    /// Sends `GET <uri>` with `headers`, one `name: value` per line. Values go
    /// out as Latin-1, so `ÿ` becomes the byte 0xFF, which is not UTF-8. Gives
    /// the status and the error code, if any.
    async fn send(app: &Router, uri: &str, headers: &str) -> (StatusCode, Option<String>) {
        let mut request = Request::builder().uri(uri);
        for line in headers.lines() {
            let (name, value) = line.split_once(": ").unwrap();
            let latin1: Vec<u8> = value
                .chars()
                .map(|c| u8::try_from(u32::from(c)).unwrap())
                .collect();
            request = request.header(name, HeaderValue::from_bytes(&latin1).unwrap());
        }
        let request = request.body(Body::empty()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
        }
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        if status == StatusCode::OK {
            assert_eq!(&body[..], b"ok");
            return (status, None);
        }
        let error: ErrorBody = serde_json::from_slice(&body).unwrap();
        (status, Some(error.error.code))
    }

    #[tokio::test]
    async fn each_check_gives_its_status_and_code() {
        let token = Token::generate().unwrap();
        let bearer = token.bearer_header();
        let hex = &bearer["Bearer ".len()..];
        let other = Token::generate().unwrap().bearer_header();
        let app = app(token);

        let host = format!("host: 127.0.0.1:{PORT}");
        let auth = format!("authorization: {bearer}");
        let good = format!("{host}\n{auth}");
        let ok = (StatusCode::OK, None);
        let origin = (StatusCode::FORBIDDEN, Some("origin_rejected"));
        let bad_host = (StatusCode::FORBIDDEN, Some("host_rejected"));
        let unauthorized = (StatusCode::UNAUTHORIZED, Some("unauthorized"));
        let cases = [
            ("all checks pass", good.clone(), ok),
            (
                "Origin, good token",
                format!("origin: http://evil.example\n{good}"),
                origin,
            ),
            ("Origin null", format!("origin: null\n{good}"), origin),
            ("Origin comes first", "origin: null".to_owned(), origin),
            ("no Host", auth.clone(), bad_host),
            (
                "Host localhost",
                format!("host: localhost:{PORT}\n{auth}"),
                bad_host,
            ),
            (
                "Host [::1]",
                format!("host: [::1]:{PORT}\n{auth}"),
                bad_host,
            ),
            (
                "another port",
                format!("host: 127.0.0.1:{}\n{auth}", PORT + 1),
                bad_host,
            ),
            ("no port", format!("host: 127.0.0.1\n{auth}"), bad_host),
            (
                "Host not UTF-8",
                format!("host: 127.0.0.1:{PORT}ÿ\n{auth}"),
                bad_host,
            ),
            ("two Hosts", format!("{host}\n{good}"), bad_host),
            (
                "Host before the token",
                format!("host: localhost:{PORT}"),
                bad_host,
            ),
            ("no token", host.clone(), unauthorized),
            (
                "wrong token",
                format!("{host}\nauthorization: {other}"),
                unauthorized,
            ),
            (
                "Basic",
                format!("{host}\nauthorization: Basic {hex}"),
                unauthorized,
            ),
            (
                "lowercase bearer",
                format!("{host}\nauthorization: bearer {hex}"),
                unauthorized,
            ),
            (
                "two spaces",
                format!("{host}\nauthorization: Bearer  {hex}"),
                unauthorized,
            ),
            (
                "one character short",
                good[..good.len() - 1].to_owned(),
                unauthorized,
            ),
            (
                "token not UTF-8",
                format!("{host}\nauthorization: Bearer {}ÿ", &hex[..63]),
                unauthorized,
            ),
            (
                "Bearer, no token",
                format!("{host}\nauthorization: Bearer "),
                unauthorized,
            ),
            (
                "bare token",
                format!("{host}\nauthorization: {hex}"),
                unauthorized,
            ),
            (
                "two tokens, good first",
                format!("{good}\nauthorization: {other}"),
                unauthorized,
            ),
            (
                "two tokens, good last",
                format!("{host}\nauthorization: {other}\n{auth}"),
                unauthorized,
            ),
        ];
        for (name, headers, (status, code)) in cases {
            let got = send(&app, PATH, &headers).await;
            assert_eq!(got, (status, code.map(str::to_owned)), "{name}");
        }

        // An absolute request target names a host too, and it must agree.
        let absolute = [
            (format!("http://127.0.0.1:{PORT}{PATH}"), ok),
            ("http://evil.example/v1/health".to_owned(), bad_host),
            (format!("http://u@127.0.0.1:{PORT}{PATH}"), bad_host),
        ];
        for (uri, (status, code)) in absolute {
            let got = send(&app, &uri, &good).await;
            assert_eq!(got, (status, code.map(str::to_owned)), "{uri}");
        }
    }

    #[tokio::test]
    async fn unknown_paths_are_checked_too() {
        let token = Token::generate().unwrap();
        let host = format!("host: 127.0.0.1:{PORT}");
        let good = format!("{host}\nauthorization: {}", token.bearer_header());
        let app = app(token);
        let path = "/v1/nope";
        let code = |code: &str| Some(code.to_owned());

        let got = send(&app, path, &host).await;
        assert_eq!(got, (StatusCode::UNAUTHORIZED, code("unauthorized")));
        let got = send(&app, path, &format!("origin: null\n{good}")).await;
        assert_eq!(got, (StatusCode::FORBIDDEN, code("origin_rejected")));
        let got = send(&app, path, &good).await;
        assert_eq!(got, (StatusCode::NOT_FOUND, code("not_found")));
    }

    /// Collects log output in memory.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn the_log_names_the_code_and_path_but_no_header_value() {
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let token = Token::generate().unwrap();
        let bearer = token.bearer_header();
        let other = Token::generate().unwrap().bearer_header();
        let app = app(token);
        let auth = format!("authorization: {bearer}");
        send(&app, PATH, &format!("origin: http://evil.example\n{auth}")).await;
        send(&app, PATH, &format!("host: rebind.example:{PORT}\n{auth}")).await;
        send(
            &app,
            PATH,
            &format!("host: 127.0.0.1:{PORT}\nauthorization: {other}"),
        )
        .await;

        let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        // `DEBUG`, not `WARN`: rejections must not flood the log.
        for wanted in [
            "DEBUG",
            "origin_rejected",
            "host_rejected",
            "unauthorized",
            PATH,
        ] {
            assert!(log.contains(wanted), "{wanted} missing from: {log}");
        }
        assert!(!log.contains("WARN"), "{log}");
        for value in [&bearer[7..], &other[7..], "evil.example", "rebind.example"] {
            assert!(!log.contains(value), "the log shows a header value: {log}");
        }
    }
}
