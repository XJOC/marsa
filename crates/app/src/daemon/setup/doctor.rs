//! The doctor route: the checks of `marsa doctor`, for the CLI and the window.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/doctor` | `DoctorReport`: each check with its verdict, and the warnings |
//!
//! The daemon reads its own state for the facts of `marsad` and Caddy, and
//! `marsa_core::platform::checkup` reads the system for the rest, as
//! `marsa doctor` does when it finds no daemon. Read-only. The `PATH` check
//! is left out: it judges a terminal's `PATH`, which only the CLI has.
//!
//! Every error is an `ErrorBody`; the checks themselves never fail the
//! request, a check that cannot run says so in its verdict.

use std::path::Path;
use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use marsa_core::platform::checkup::{self, DaemonFacts, Item};
use marsa_core::types::{DoctorLine, DoctorReport, Site};

use crate::daemon::http;
use crate::daemon::http::routes::{HttpError, current_health};
use crate::daemon::state::Daemon;

/// Gathers the report from the daemon's own facts. A function pointer so a
/// test can answer without asking the system.
type Gather = fn(&Path, &DaemonFacts) -> DoctorReport;

/// The routes, for `http::routes::router` to merge before its fallbacks. `port` is the
/// one the daemon listens on, for the `marsad` check.
pub fn routes(port: u16) -> Router<Arc<Daemon>> {
    routes_with(port, system)
}

fn routes_with(port: u16, gather: Gather) -> Router<Arc<Daemon>> {
    Router::new().route(
        "/v1/doctor",
        get(move |State(daemon): State<Arc<Daemon>>| report(daemon, port, gather)),
    )
}

async fn report(
    daemon: Arc<Daemon>,
    port: u16,
    gather: Gather,
) -> Result<Json<DoctorReport>, HttpError> {
    let facts = facts(&daemon, port);
    // The checks run programs and sockets: off the async threads.
    http::blocking("doctor", move || Ok(gather(daemon.data_dir(), &facts)))
        .await
        .map(Json)
}

/// What the daemon knows of itself, the way a client would learn it from
/// `/v1/health`, `/v1/services`, and `/v1/sites`.
fn facts(daemon: &Daemon, port: u16) -> DaemonFacts {
    let caddy = daemon.caddy().status();
    let sites = daemon.sites();
    DaemonFacts {
        health: Ok((current_health(), port)),
        caddy: Some(caddy.state),
        caddy_pid: caddy.pid,
        secure_site: sites
            .as_ref()
            .map(|list| list.sites.iter().any(|site| site.secure)),
        site_names: sites.map(|list| list.sites.iter().map(Site::host).collect()),
    }
}

/// The real checks.
fn system(data_dir: &Path, facts: &DaemonFacts) -> DoctorReport {
    let mut report = DoctorReport {
        lines: Vec::new(),
        warnings: Vec::new(),
    };
    checkup::run(data_dir, facts, None, &mut |item| match item {
        Item::Line(check, verdict) => report.lines.push(DoctorLine { check, verdict }),
        Item::Warning(warning) => report.warnings.push(warning),
    });
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::extract::Request;
    use axum::http::StatusCode;
    use axum::middleware::from_fn_with_state;
    use marsa_core::doctor::{Check, Pass, Verdict, Warning};
    use marsa_core::platform::daemon_file::{Token, daemon_authority};
    use serde_json::Value;
    use tower::ServiceExt as _;

    use crate::daemon::http::auth::{AuthConfig, require_auth};
    use crate::daemon::http::routes::router;

    /// The daemon port in the `Host` header. No socket is bound for it.
    const PORT: u16 = 51239;

    fn canned(_: &Path, facts: &DaemonFacts) -> DoctorReport {
        let port = facts.health.as_ref().map_or(0, |(_, port)| *port);
        DoctorReport {
            lines: vec![DoctorLine {
                check: Check::Daemon,
                verdict: Verdict::Pass(Pass::Daemon {
                    version: "9.9.9".into(),
                    pid: 7,
                    port,
                }),
            }],
            warnings: vec![Warning::DaemonElevated],
        }
    }

    /// The route behind the auth layer, answering from `canned`, and the
    /// real router with the real checks, which the rejected requests never
    /// reach.
    struct App {
        canned: Router,
        real: Router,
        bearer: String,
    }

    impl App {
        fn new() -> Self {
            let token = Token::generate().unwrap();
            let bearer = token.bearer_header();
            let auth = AuthConfig::new(Arc::new(token), PORT);
            let daemon = Arc::new(Daemon::new(std::env::temp_dir().join("marsa-doctor-api")));
            let canned = routes_with(PORT, canned)
                .with_state(Arc::clone(&daemon))
                .layer(from_fn_with_state(auth.clone(), require_auth));
            Self {
                canned,
                real: router(daemon, auth),
                bearer,
            }
        }
    }

    async fn get_doctor(router: &Router, headers: &[(&str, &str)]) -> (StatusCode, Value) {
        let mut request = Request::builder().uri("/v1/doctor");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// A name, the headers, and the status and code that must come back.
    type Case<'a> = (&'a str, Vec<(&'a str, &'a str)>, StatusCode, &'a str);

    #[tokio::test]
    async fn the_report_has_the_daemons_own_facts() {
        let app = App::new();
        let host = daemon_authority(PORT);
        let headers = [("host", host.as_str()), ("authorization", &app.bearer)];
        let (status, body) = get_doctor(&app.canned, &headers).await;
        assert_eq!(status, StatusCode::OK);
        let report: DoctorReport = serde_json::from_value(body).unwrap();
        assert_eq!(
            report,
            canned(
                Path::new(""),
                &DaemonFacts {
                    health: Ok((current_health(), PORT)),
                    caddy: None,
                    caddy_pid: None,
                    secure_site: None,
                    site_names: None,
                }
            )
        );
    }

    /// Every rejection of the other routes holds here too, and none of them
    /// runs a check.
    #[tokio::test]
    async fn token_host_and_origin_are_checked_before_any_check_runs() {
        let app = App::new();
        let host = daemon_authority(PORT);
        let bad_host = "127.0.0.1:1";
        let bearer = app.bearer.as_str();
        let wrong = "Bearer wrong";
        let cases: [Case; 6] = [
            (
                "no token",
                vec![("host", &host)],
                StatusCode::UNAUTHORIZED,
                "unauthorized",
            ),
            (
                "wrong token",
                vec![("host", &host), ("authorization", wrong)],
                StatusCode::UNAUTHORIZED,
                "unauthorized",
            ),
            (
                "Origin, good token",
                vec![
                    ("host", &host),
                    ("authorization", bearer),
                    ("origin", "http://evil.example"),
                ],
                StatusCode::FORBIDDEN,
                "origin_rejected",
            ),
            (
                "Origin null",
                vec![
                    ("host", &host),
                    ("authorization", bearer),
                    ("origin", "null"),
                ],
                StatusCode::FORBIDDEN,
                "origin_rejected",
            ),
            (
                "wrong Host",
                vec![("host", bad_host), ("authorization", bearer)],
                StatusCode::FORBIDDEN,
                "host_rejected",
            ),
            (
                "no Host",
                vec![("authorization", bearer)],
                StatusCode::FORBIDDEN,
                "host_rejected",
            ),
        ];
        for (name, headers, status, code) in cases {
            let (got, body) = get_doctor(&app.real, &headers).await;
            assert_eq!(
                (got, body["error"]["code"].as_str()),
                (status, Some(code)),
                "{name}"
            );
            assert!(body.get("lines").is_none(), "{name}: no report");
        }
    }

    #[tokio::test]
    async fn the_route_takes_get_only() {
        let app = App::new();
        let host = daemon_authority(PORT);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/doctor")
            .header("host", &host)
            .header("authorization", &app.bearer)
            .body(Body::empty())
            .unwrap();
        let response = app.real.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}
