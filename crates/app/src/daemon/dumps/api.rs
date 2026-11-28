//! The dumps for the window and the CLI.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/dumps?after=<id>` | `DumpList`: the dumps newer than `id` (all without `after`), newest first, the id of the newest ever kept, and the listener's status |
//! | `DELETE /v1/dumps` | forgets every dump, then the empty `DumpList` |
//! | `GET /v1/dumps/capture` | `{"enabled": bool}`: `[dumps] enabled` of `global.toml` |
//! | `PUT /v1/dumps/capture` | `{"enabled": bool}`: saves it, then opens or closes the port. A port that another program holds is not an error: the status says `port_in_use`. |
//!
//! | Error | When |
//! | --- | --- |
//! | 400 `invalid_query` | `after` is not a number |
//! | 4xx `invalid_body` | the body of `PUT` is not `{"enabled": bool}` |
//! | 500 `config_read`, `config_invalid`, `config_write` | `global.toml` cannot be read or written |

use std::sync::Arc;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use marsa_core::codes;
use marsa_core::config::{self, ConfigPaths};
use marsa_core::types::{DumpList, DumpsCapture};
use serde::Deserialize;

use crate::daemon::http::routes::HttpError;
use crate::daemon::http::{self, Rejected as _, global_error};
use crate::daemon::state::Daemon;

/// The routes, for `http::routes::router` to merge before its fallbacks.
pub fn routes() -> Router<Arc<Daemon>> {
    Router::new()
        .route("/v1/dumps", get(list).delete(clear))
        .route("/v1/dumps/capture", get(capture).put(set_capture))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AfterQuery {
    after: Option<u64>,
}

async fn list(
    State(daemon): State<Arc<Daemon>>,
    query: Result<Query<AfterQuery>, QueryRejection>,
) -> Result<Json<DumpList>, HttpError> {
    let Query(AfterQuery { after }) = query.map_err(|err| err.reject(codes::INVALID_QUERY))?;
    Ok(Json(daemon.dumps().list(after)))
}

async fn clear(State(daemon): State<Arc<Daemon>>) -> Json<DumpList> {
    Json(daemon.dumps().clear())
}

async fn capture(State(daemon): State<Arc<Daemon>>) -> Result<Json<DumpsCapture>, HttpError> {
    http::blocking("dumps setting", move || {
        let path = ConfigPaths::in_dir(daemon.data_dir()).global;
        let global = config::load_global(&path).map_err(|err| global_error(&err))?;
        Ok(DumpsCapture {
            enabled: global.dumps.enabled,
        })
    })
    .await
    .map(Json)
}

async fn set_capture(
    State(daemon): State<Arc<Daemon>>,
    body: Result<Json<DumpsCapture>, JsonRejection>,
) -> Result<Json<DumpsCapture>, HttpError> {
    let Json(DumpsCapture { enabled }) = body.map_err(|err| err.reject(codes::INVALID_BODY))?;
    // The save and the switch in a task of their own: a client that hangs up
    // drops this handler, and must not leave the setting saved but not applied.
    let job = tokio::spawn(async move {
        // One change at a time, save and apply together: without this, a
        // second PUT could save and apply between this one's save and its
        // apply, and `global.toml` would say one thing while the listener
        // does the other. The lock is held across `.await` as a
        // serialization token only (AGENTS.md section 6).
        let _one = daemon.dumps_capture_lock().lock().await;
        let worker = Arc::clone(&daemon);
        http::blocking("dumps setting", move || save(&worker, enabled)).await?;
        // Also when nothing changed: it tries a port that was busy again.
        daemon.dumps().apply(enabled, daemon.shutdown()).await;
        Ok::<_, HttpError>(DumpsCapture { enabled })
    });
    job.await
        .unwrap_or_else(|err| Err(http::job_failed("dumps setting", &err)))
        .map(Json)
}

/// Saves `[dumps] enabled` if it differs. Blocking: it takes the lock of
/// `global.toml` and writes the file.
fn save(daemon: &Daemon, enabled: bool) -> Result<(), HttpError> {
    let _one = daemon.lock_global();
    let path = ConfigPaths::in_dir(daemon.data_dir()).global;
    let mut global = config::load_global(&path).map_err(|err| global_error(&err))?;
    if global.dumps.enabled != enabled {
        global.dumps.enabled = enabled;
        config::save_global(&path, &global).map_err(|err| global_error(&err))?;
        tracing::info!(enabled, "the dump capture setting changed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use marsa_core::types::DumpsStatus;
    use serde_json::json;

    use super::*;
    use crate::daemon::dumps::DumpsService;
    use crate::daemon::http::test_router::TestRouter;

    /// A daemon whose dump server takes any free port: a test never opens 9912.
    fn router(dir: &std::path::Path) -> (TestRouter, Arc<Daemon>) {
        let mut daemon = Daemon::new(dir.to_path_buf());
        daemon.set_dumps(DumpsService::new(0));
        let daemon = Arc::new(daemon);
        (TestRouter::new(Arc::clone(&daemon)), daemon)
    }

    #[tokio::test]
    async fn the_list_starts_empty_and_a_bad_after_is_refused() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let (app, _daemon) = router(tmp.path());
        let (status, json) = app.answer("GET", "/v1/dumps", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            json,
            json!({ "status": "disabled", "latest_id": 0, "count": 0, "dumps": [] })
        );
        let (status, _) = app.answer("GET", "/v1/dumps?after=7", None).await;
        assert_eq!(status, StatusCode::OK);
        for bad in ["after=abc", "after=-1", "after=1&after=2", "other=1"] {
            let (status, json) = app.answer("GET", &format!("/v1/dumps?{bad}"), None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
            assert_eq!(json["error"]["code"], "invalid_query", "{bad}");
        }
    }

    #[tokio::test]
    async fn delete_answers_the_empty_list() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let (app, _daemon) = router(tmp.path());
        let (status, json) = app.answer("DELETE", "/v1/dumps", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["dumps"], json!([]));
    }

    #[tokio::test]
    async fn the_capture_switch_is_saved_and_opens_or_closes_the_port() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let (app, daemon) = router(tmp.path());
        let (_, json) = app.answer("GET", "/v1/dumps/capture", None).await;
        assert_eq!(json, json!({ "enabled": true }), "on by default");

        let (status, json) = app
            .answer("PUT", "/v1/dumps/capture", Some(json!({ "enabled": true })))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, json!({ "enabled": true }));
        assert_eq!(daemon.dumps().status(), DumpsStatus::Listening);
        let (_, json) = app.answer("GET", "/v1/dumps", None).await;
        assert_eq!(json["status"], "listening");

        let (_, json) = app
            .answer(
                "PUT",
                "/v1/dumps/capture",
                Some(json!({ "enabled": false })),
            )
            .await;
        assert_eq!(json, json!({ "enabled": false }));
        assert_eq!(daemon.dumps().status(), DumpsStatus::Disabled);
        assert!(!super::super::enabled_in_config(tmp.path()), "saved");
        let (_, json) = app.answer("GET", "/v1/dumps/capture", None).await;
        assert_eq!(json, json!({ "enabled": false }));

        // On again: the default is not written.
        app.answer("PUT", "/v1/dumps/capture", Some(json!({ "enabled": true })))
            .await;
        assert!(super::super::enabled_in_config(tmp.path()));
        daemon
            .dumps()
            .shutdown(std::time::Duration::from_secs(2))
            .await;
    }

    #[tokio::test]
    async fn a_bad_body_is_refused_and_a_broken_global_toml_is_an_error() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let (app, daemon) = router(tmp.path());
        for body in [json!({}), json!({ "enabled": "yes" })] {
            let (status, json) = app.answer("PUT", "/v1/dumps/capture", Some(body)).await;
            assert!(status.is_client_error(), "{status}");
            assert_eq!(json["error"]["code"], "invalid_body");
        }
        let path = ConfigPaths::in_dir(tmp.path()).global;
        std::fs::create_dir_all(path.parent().expect("config dir")).expect("mkdir");
        std::fs::write(&path, "[dumps]\nenabled = 7\n").expect("write");
        let (status, json) = app.answer("GET", "/v1/dumps/capture", None).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json["error"]["code"], "config_invalid");
        let (status, _) = app
            .answer(
                "PUT",
                "/v1/dumps/capture",
                Some(json!({ "enabled": false })),
            )
            .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(daemon.dumps().status(), DumpsStatus::Disabled);
    }

    #[tokio::test]
    async fn a_dump_sent_to_the_port_is_listed_and_after_skips_the_old_ones() {
        use tokio::io::AsyncWriteExt as _;

        let tmp = tempfile::tempdir().expect("temp dir");
        let (app, daemon) = router(tmp.path());
        app.answer("PUT", "/v1/dumps/capture", Some(json!({ "enabled": true })))
            .await;
        let addr = daemon.dumps().local_addr().expect("it listens");
        let fixtures =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dumps");
        let text = std::fs::read_to_string(fixtures.join("multi.v74.dump")).expect("fixture");
        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        stream
            .write_all(format!("{text}\n").as_bytes())
            .await
            .expect("write");
        stream.shutdown().await.expect("close");
        for _ in 0..500 {
            if daemon.dumps().list(None).dumps.len() == 3 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let (_, json) = app.answer("GET", "/v1/dumps", None).await;
        assert_eq!(json["latest_id"], 3);
        assert_eq!(json["dumps"][0]["text"], "3", "newest first");
        assert_eq!(json["dumps"][2]["text"], "\"first\"");
        let (_, json) = app.answer("GET", "/v1/dumps?after=2", None).await;
        assert_eq!(json["dumps"].as_array().map(Vec::len), Some(1));
        let (_, json) = app.answer("DELETE", "/v1/dumps", None).await;
        assert_eq!(json["dumps"], json!([]));
        assert_eq!(json["latest_id"], 3, "the counter goes on");
        daemon
            .dumps()
            .shutdown(std::time::Duration::from_secs(2))
            .await;
    }

    #[tokio::test]
    async fn every_route_needs_the_token() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let (app, _daemon) = router(tmp.path());
        for (method, path) in [
            ("GET", "/v1/dumps"),
            ("DELETE", "/v1/dumps"),
            ("GET", "/v1/dumps/capture"),
            ("PUT", "/v1/dumps/capture"),
        ] {
            app.assert_refused_without_auth(method, path).await;
        }
    }

    /// One `PUT /v1/dumps/capture` released by `hold`.
    async fn put(
        app: std::sync::Arc<TestRouter>,
        hold: std::sync::Arc<tokio::sync::Barrier>,
        enabled: bool,
    ) -> (StatusCode, serde_json::Value) {
        hold.wait().await;
        app.answer(
            "PUT",
            "/v1/dumps/capture",
            Some(json!({ "enabled": enabled })),
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_capture_puts_at_once_keep_the_file_and_the_listener_together() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let (plain, daemon) = router(tmp.path());
        let app = std::sync::Arc::new(plain);
        app.answer("PUT", "/v1/dumps/capture", Some(json!({ "enabled": true })))
            .await;
        assert_eq!(daemon.dumps().status(), DumpsStatus::Listening);

        // Two PUTs of opposite values, released together by a barrier.
        // Who wins may change from round to round, but `global.toml` and
        // the listener may never disagree: the save and the apply are
        // one serialized operation.
        for round in 0..20 {
            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
            let values = if round % 2 == 0 {
                (false, true)
            } else {
                (true, false)
            };
            let one = tokio::spawn(put(
                std::sync::Arc::clone(&app),
                std::sync::Arc::clone(&barrier),
                values.0,
            ));
            let two = tokio::spawn(put(std::sync::Arc::clone(&app), barrier, values.1));
            // Let both PUTs reach the save before the load task lands:
            // the save is the last step whose order decides the file.
            for _ in 0..5 {
                tokio::task::yield_now().await;
            }
            // A worker that is busy here delays one PUT's apply until
            // after the other's save-and-apply — the window in which the
            // unserialized halves could leave the file and the listener
            // in opposite states.
            tokio::spawn(async move {
                let until = std::time::Instant::now() + std::time::Duration::from_millis(30);
                let mut spin = 0_u64;
                while std::time::Instant::now() < until {
                    spin = spin.wrapping_add(1);
                    std::hint::black_box(spin);
                }
            });
            for job in [one, two] {
                let (status, _) = job.await.expect("the PUT ran");
                assert_eq!(status, StatusCode::OK, "round {round}");
            }
            let saved = crate::daemon::dumps::enabled_in_config(tmp.path());
            let listening = daemon.dumps().status() == DumpsStatus::Listening;
            assert_eq!(
                saved,
                listening,
                "round {round}: global.toml says {saved} but the listener is {:?}",
                daemon.dumps().status()
            );
        }
        daemon
            .dumps()
            .shutdown(std::time::Duration::from_secs(2))
            .await;
    }
}
