//! The mail Mailpit caught, for the window and the CLI, which never talk to
//! Mailpit themselves. `marsad` asks Mailpit's API on `127.0.0.1:8025` and
//! answers with `marsa_core::types`.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/mail` | `MailList`: the newest mails, the total, and the unread |
//! | `GET /v1/mail/{id}` | `MailText`: one mail's plain text, and Mailpit's page for its HTML |
//! | `DELETE /v1/mail` | deletes every mail, then the empty `MailList` |
//!
//! | Error | When |
//! | --- | --- |
//! | 409 `mail_not_running` | our Mailpit does not run: another program on 8025 is never asked |
//! | 400 `mail_id_invalid` | the id is not 1 to 64 of `[A-Za-z0-9_-]` |
//! | 404 `mail_not_found` | Mailpit has no mail with this id |
//! | 502 `mail_unavailable` | Mailpit did not answer, or its answer could not be read |

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use marsa_core::codes;
use marsa_core::databases::ServerKind;
use marsa_core::databases::mailpit::MAILPIT_UI_PORT;
use marsa_core::mail::{is_mail_id, list_path, message_path, read_list, read_message};
use marsa_core::types::{MailList, MailText, ServiceState};
use reqwest::{Method, redirect};
use tracing::warn;

use crate::daemon::http::routes::HttpError;
use crate::daemon::state::Daemon;

/// For each request to Mailpit, which answers from its own database at once.
const TIMEOUT: Duration = Duration::from_secs(5);
/// The largest answer read: a list of 200 summaries is about 200 KiB, and a
/// mail's text is rarely over 1 MiB.
const MAX_ANSWER: usize = 8 * 1024 * 1024;

/// The routes, for `http::routes::router` to merge before its fallbacks.
pub fn routes() -> Router<Arc<Daemon>> {
    Router::new()
        .route("/v1/mail", get(list).delete(clear))
        .route("/v1/mail/{id}", get(one))
}

async fn list(State(daemon): State<Arc<Daemon>>) -> Result<Json<MailList>, HttpError> {
    running(&daemon)?;
    let body = ask(Method::GET, &list_path()).await?;
    read_list(&body).map(Json).map_err(unreadable)
}

async fn one(
    State(daemon): State<Arc<Daemon>>,
    Path(id): Path<String>,
) -> Result<Json<MailText>, HttpError> {
    if !is_mail_id(&id) {
        let message = "a mail id is 1 to 64 letters, digits, `_`, or `-`";
        return Err(HttpError::new(
            StatusCode::BAD_REQUEST,
            codes::MAIL_ID_INVALID,
            message,
        ));
    }
    running(&daemon)?;
    let body = ask(Method::GET, &message_path(&id)).await?;
    read_message(&id, &body).map(Json).map_err(unreadable)
}

async fn clear(State(daemon): State<Arc<Daemon>>) -> Result<Json<MailList>, HttpError> {
    running(&daemon)?;
    // No body: Mailpit deletes every mail.
    ask(Method::DELETE, "/api/v1/messages").await?;
    let body = ask(Method::GET, &list_path()).await?;
    read_list(&body).map(Json).map_err(unreadable)
}

/// Only our own Mailpit is asked: while it does not run, another program
/// may hold port 8025, and its answers would reach the window.
fn running(daemon: &Daemon) -> Result<(), HttpError> {
    match daemon.server(ServerKind::Mailpit).status().state {
        ServiceState::Running => Ok(()),
        _ => Err(HttpError::new(
            StatusCode::CONFLICT,
            codes::MAIL_NOT_RUNNING,
            "Mailpit does not run",
        )),
    }
}

/// The client for Mailpit: no proxy (one from `HTTP_PROXY` must never get
/// these requests), no redirects, and [`TIMEOUT`]. `None` if it cannot be
/// built: every request then fails as `mail_unavailable`.
fn client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .no_proxy()
                .redirect(redirect::Policy::none())
                .timeout(TIMEOUT)
                .build()
                .inspect_err(|err| warn!(%err, "cannot build the Mailpit client"))
                .ok()
        })
        .as_ref()
}

/// Sends `method path` to Mailpit and reads at most [`MAX_ANSWER`] bytes of
/// its answer. A 404 is `mail_not_found`; any other failure,
/// `mail_unavailable`.
async fn ask(method: Method, path: &str) -> Result<Vec<u8>, HttpError> {
    let client = client().ok_or_else(|| unavailable("no HTTP client"))?;
    let url = format!("http://127.0.0.1:{MAILPIT_UI_PORT}{path}");
    let mut response = client
        .request(method, url)
        .send()
        .await
        .map_err(|err| unavailable(&err.to_string()))?;
    match response.status() {
        status if status.is_success() => {}
        StatusCode::NOT_FOUND => {
            return Err(HttpError::new(
                StatusCode::NOT_FOUND,
                codes::MAIL_NOT_FOUND,
                "Mailpit has no such mail",
            ));
        }
        status => return Err(unavailable(&format!("Mailpit answered {status}"))),
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| unavailable(&err.to_string()))?
    {
        if body.len() + chunk.len() > MAX_ANSWER {
            return Err(unavailable("Mailpit's answer is too large"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn unavailable(why: &str) -> HttpError {
    warn!(%why, "mailpit request failed");
    HttpError::new(
        StatusCode::BAD_GATEWAY,
        codes::MAIL_UNAVAILABLE,
        format!("Mailpit did not answer as expected: {why}"),
    )
}

fn unreadable(err: marsa_core::mail::BadAnswer) -> HttpError {
    unavailable(&err.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::StatusCode;

    use crate::daemon::http::test_router::TestRouter;
    use crate::daemon::state::Daemon;

    /// No Mailpit runs in a test: every route says so, and a bad id is
    /// refused before that.
    #[tokio::test]
    async fn without_mailpit_every_route_is_a_conflict() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let app = TestRouter::new(Arc::new(Daemon::new(tmp.path().to_path_buf())));
        for (method, path) in [
            ("GET", "/v1/mail"),
            ("DELETE", "/v1/mail"),
            ("GET", "/v1/mail/abc"),
        ] {
            let (status, json) = app.answer(method, path, None).await;
            assert_eq!(status, StatusCode::CONFLICT, "{method} {path}");
            assert_eq!(json["error"]["code"], "mail_not_running", "{method} {path}");
        }
        for id in ["a.b", "a%2Fb", &"x".repeat(65)] {
            let (status, json) = app.answer("GET", &format!("/v1/mail/{id}"), None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{id}");
            assert_eq!(json["error"]["code"], "mail_id_invalid", "{id}");
        }
    }
}
