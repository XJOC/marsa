//! A Laravel site's own log, read by the daemon: the window and the CLI
//! never open the project's files.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/sites/{name}/log` | `SiteLog`: the end of the newest `laravel*.log` |
//! | `GET /v1/sites/{name}/log?follow=true` | SSE: those lines, then each new one, as `data: <SiteLogLine>` |
//!
//! Refusals: `404 site_not_found`, `409 site_not_laravel` (no `artisan`, so
//! no Laravel log), and `409 site_log_unsafe` (`storage/logs` leads out of
//! the site, or the file is a link). No log yet is an empty answer, and a
//! stream waits for the file.
//!
//! A stream looks at the file every [`LOOK_EVERY`] in its own task, which
//! ends when the client goes away, when the daemon stops, or when the folder
//! becomes unsafe (a last `: site_log_unsafe` comment). A file that became
//! shorter is read again from its start after a `: truncated` comment.
//! The lines are never logged: they may hold the user's data. A stream
//! keeps the folder it started with, still checked at each look, until the
//! client leaves: a site unlinked meanwhile is not stopped for it.

mod reader;

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use marsa_core::codes;
use marsa_core::types::{SiteKind, SiteLogLine};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt as _};
use tokio_util::sync::CancellationToken;

use self::reader::{LogError, Tail};
use crate::daemon::http::routes::HttpError;
use crate::daemon::http::{Rejected as _, job_failed};
use crate::daemon::state::Daemon;

/// How often a stream looks for new lines.
const LOOK_EVERY: Duration = Duration::from_millis(500);

/// A `:` comment this often on a quiet stream, as on `/v1/logs/{id}`: the
/// client gives up after 45 s without a byte.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// Lines waiting for a slow client. The task waits when it is full, so a
/// stream never holds more.
const QUEUE: usize = 256;

/// The route, for `sites::api::routes` to merge.
pub(super) fn routes() -> Router<Arc<Daemon>> {
    Router::new().route("/v1/sites/{name}/log", get(site_log))
}

#[derive(Debug, Deserialize)]
struct LogQuery {
    #[serde(default)]
    follow: bool,
}

async fn site_log(
    State(daemon): State<Arc<Daemon>>,
    path: Result<UrlPath<String>, PathRejection>,
    query: Result<Query<LogQuery>, QueryRejection>,
) -> Result<Response, HttpError> {
    let UrlPath(name) = path.map_err(|err| err.reject(codes::INVALID_PATH))?;
    let Query(query) = query.map_err(|err| err.reject(codes::INVALID_QUERY))?;
    let root = laravel_root(&daemon, &name).await?;
    // The reader uses blocking `std::fs`, and an open may wait (module
    // `reader`): never on the server's own threads.
    let started = tokio::task::spawn_blocking(move || Tail::start(&root))
        .await
        .map_err(|err| job_failed("site log", &err))?;
    let (mut log, mut tail) = started.map_err(|err| log_error(&name, &err))?;
    if !query.follow {
        log.lines.extend(tail.finish());
        return Ok(Json(log).into_response());
    }
    let stream = follow(name, log.lines, tail, daemon.shutdown().clone(), LOOK_EVERY);
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
        .into_response())
}

/// The folder of the Laravel site `name`.
async fn laravel_root(daemon: &Arc<Daemon>, name: &str) -> Result<PathBuf, HttpError> {
    let sites = match daemon.sites() {
        Some(sites) => sites,
        // No build yet: see `Daemon::rebuild_sites`.
        None => daemon.rebuild_sites().await?,
    };
    let site = sites
        .sites
        .into_iter()
        .find(|site| site.name == name)
        .ok_or_else(|| {
            let message = format!("no site named {name:?}");
            HttpError::new(StatusCode::NOT_FOUND, codes::SITE_NOT_FOUND, message)
        })?;
    if site.kind != SiteKind::Laravel {
        let message = format!("{name} is not a Laravel project, so it has no Laravel log");
        return Err(HttpError::new(
            StatusCode::CONFLICT,
            codes::SITE_NOT_LARAVEL,
            message,
        ));
    }
    Ok(site.root)
}

fn log_error(site: &str, err: &LogError) -> HttpError {
    tracing::warn!(%site, code = err.code(), error = %err, "cannot read the site log");
    let status = match err {
        LogError::Unsafe(_) => StatusCode::CONFLICT,
        LogError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    HttpError::new(status, err.code(), err.to_string())
}

/// The `?follow=true` body: `first`, then the lines that one task finds
/// every `every`. The task ends with the stream, at `stop`, or when the
/// folder becomes unsafe.
fn follow(
    site: String,
    first: Vec<SiteLogLine>,
    tail: Tail,
    stop: CancellationToken,
    every: Duration,
) -> impl Stream<Item = Result<SseEvent, Infallible>> + Send + 'static {
    let (sender, receiver) = mpsc::channel(QUEUE);
    tokio::spawn(look(site, tail, sender, stop, every));
    tokio_stream::iter(first)
        .map(|line| line_event(&line))
        .chain(ReceiverStream::new(receiver))
        .map(Ok)
}

/// The task of one stream. Each wait also watches `stop`, so a client that
/// stops reading, or a look that hangs, never holds the daemon's stop.
async fn look(
    site: String,
    mut tail: Tail,
    sender: mpsc::Sender<SseEvent>,
    stop: CancellationToken,
    every: Duration,
) {
    let mut ticks = tokio::time::interval(every);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // The first tick is now, and the first read is already sent.
    ticks.tick().await;
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => return,
            () = sender.closed() => return,
            _ = ticks.tick() => {}
        }
        let job = tokio::task::spawn_blocking(move || {
            let step = tail.poll();
            (tail, step)
        });
        // A look that never ends (a FIFO swapped in, see `reader`) keeps its
        // blocking thread, but not this task.
        let looked = tokio::select! {
            biased;
            () = stop.cancelled() => return,
            looked = job => looked,
        };
        let (back, step) = match looked {
            Ok(looked) => looked,
            Err(err) => {
                tracing::error!(%site, error = %err, "the site log job failed");
                return;
            }
        };
        tail = back;
        let mut events = Vec::new();
        let mut last = false;
        match step {
            Ok(step) => {
                if step.truncated {
                    events.push(SseEvent::default().comment("truncated"));
                }
                events.extend(step.lines.iter().map(line_event));
            }
            Err(err @ LogError::Unsafe(_)) => {
                tracing::warn!(%site, code = err.code(), error = %err, "stopped following the site log");
                events.push(SseEvent::default().comment(err.code()));
                last = true;
            }
            Err(err) => {
                tracing::debug!(%site, error = %err, "cannot read the site log now; trying again");
            }
        }
        for event in events {
            tokio::select! {
                biased;
                () = stop.cancelled() => return,
                sent = sender.send(event) => {
                    if sent.is_err() {
                        return;
                    }
                }
            }
        }
        if last {
            return;
        }
    }
}

fn line_event(line: &SiteLogLine) -> SseEvent {
    SseEvent::default().json_data(line).unwrap_or_else(|err| {
        // `SiteLogLine` holds a string and a number; this does not happen.
        tracing::error!(error = %err, "cannot serialize a site log line");
        SseEvent::default().comment("line_not_serialized")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::http::test_router::TestRouter;
    use axum::body::Body;
    use marsa_core::types::{ErrorBody, SiteLog};
    use serde_json::json;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    struct App {
        dir: TempDir,
        daemon: Arc<Daemon>,
        router: TestRouter,
    }

    impl App {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let daemon = Arc::new(Daemon::new(dir.path().join("data")));
            Self {
                router: TestRouter::new(Arc::clone(&daemon)),
                daemon,
                dir,
            }
        }

        /// Links `<tmp>/code/<name>`: a Laravel project (`artisan` and
        /// `storage/logs`), or a plain PHP one.
        async fn site(&self, name: &str, laravel: bool) -> PathBuf {
            let root = self.dir.path().join("code").join(name);
            fs::create_dir_all(root.join("public")).unwrap();
            fs::write(root.join("public/index.php"), "<?php\n").unwrap();
            if laravel {
                fs::write(root.join("artisan"), "<?php\n").unwrap();
                fs::create_dir_all(root.join("storage/logs")).unwrap();
            }
            let body = json!({ "name": name, "path": root });
            let (status, json) = self
                .router
                .answer("POST", "/v1/sites/link", Some(body))
                .await;
            assert_eq!(status, StatusCode::OK, "{json}");
            root
        }

        async fn log(&self, name: &str) -> SiteLog {
            let uri = format!("/v1/sites/{name}/log");
            let (status, json) = self.router.answer("GET", &uri, None).await;
            assert_eq!(status, StatusCode::OK, "{json}");
            serde_json::from_value(json).unwrap()
        }

        async fn refused(&self, name: &str) -> (StatusCode, String) {
            let uri = format!("/v1/sites/{name}/log");
            let (status, json) = self.router.answer("GET", &uri, None).await;
            let error: ErrorBody = serde_json::from_value(json).unwrap();
            (status, error.error.code)
        }
    }

    fn log_file(root: &Path) -> PathBuf {
        root.join("storage/logs/laravel.log")
    }

    #[tokio::test]
    async fn the_end_of_the_log_comes_with_a_last_line_without_its_end() {
        let app = App::new();
        // `park` is also a fixed path under `/v1/sites`.
        let root = app.site("park", true).await;
        assert_eq!(app.log("park").await, SiteLog::default());
        fs::write(log_file(&root), "one\ntwo").unwrap();
        let log = app.log("park").await;
        assert_eq!(log.file.as_deref(), Some("laravel.log"));
        let texts: Vec<_> = log.lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, ["one", "two"]);
        assert!(!log.cut_at_start);
    }

    #[tokio::test]
    async fn an_unknown_site_and_a_site_that_is_not_laravel_are_refused() {
        let app = App::new();
        app.site("blog", false).await;
        let not_found = (StatusCode::NOT_FOUND, codes::SITE_NOT_FOUND.to_owned());
        assert_eq!(app.refused("shop").await, not_found);
        let not_laravel = (StatusCode::CONFLICT, codes::SITE_NOT_LARAVEL.to_owned());
        assert_eq!(app.refused("blog").await, not_laravel);
    }

    // Made with `std::os::unix::fs::symlink`; the reader's own tests cover
    // Windows links.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_log_folder_linked_out_of_the_site_is_refused() {
        let app = App::new();
        let root = app.site("shop", true).await;
        let outside = app.dir.path().join("secrets");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("laravel.log"), "secret\n").unwrap();
        fs::remove_dir(root.join("storage/logs")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("storage/logs")).unwrap();
        let unsafe_log = (StatusCode::CONFLICT, codes::SITE_LOG_UNSAFE.to_owned());
        assert_eq!(app.refused("shop").await, unsafe_log);
    }

    #[tokio::test]
    async fn the_route_is_behind_the_auth_layer() {
        let app = App::new();
        let root = app.site("shop", true).await;
        fs::write(log_file(&root), "secret\n").unwrap();
        for uri in ["/v1/sites/shop/log", "/v1/sites/shop/log?follow=true"] {
            app.router.assert_refused_without_auth("GET", uri).await;
        }
    }

    /// Reads the stream until it holds `needle`, or ends.
    async fn until(
        chunks: &mut (impl Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin),
        read: &mut String,
        needle: &str,
    ) {
        // Time is paused: this is a guard against a hang, not a wait.
        let found = tokio::time::timeout(Duration::from_secs(60), async {
            while !read.contains(needle) {
                let chunk = chunks.next().await.expect("the stream ended").unwrap();
                read.push_str(std::str::from_utf8(&chunk).unwrap());
            }
        });
        found
            .await
            .unwrap_or_else(|_| panic!("no {needle:?} in {read:?}"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_followed_log_sends_new_lines_and_a_truncation_and_ends_with_the_daemon() {
        let app = App::new();
        let root = app.site("shop", true).await;
        fs::write(log_file(&root), "one\n").unwrap();
        let response = app
            .router
            .send("GET", "/v1/sites/shop/log?follow=true", None)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let mut chunks = Body::into_data_stream(response.into_body());
        let mut read = String::new();
        until(&mut chunks, &mut read, r#"{"seq":0,"text":"one"}"#).await;
        append(&log_file(&root), "two\n");
        until(&mut chunks, &mut read, r#"{"seq":1,"text":"two"}"#).await;
        fs::write(log_file(&root), "new\n").unwrap();
        until(&mut chunks, &mut read, r#"{"seq":2,"text":"new"}"#).await;
        assert!(read.contains(": truncated\n"), "{read}");
        app.daemon.shutdown().cancel();
        while let Some(chunk) = chunks.next().await {
            chunk.unwrap();
        }
    }

    // Made with `std::os::unix::fs::symlink`.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn a_followed_log_whose_folder_leads_out_ends_without_its_lines() {
        let app = App::new();
        let root = app.site("shop", true).await;
        fs::write(log_file(&root), "one\n").unwrap();
        let response = app
            .router
            .send("GET", "/v1/sites/shop/log?follow=true", None)
            .await;
        let mut chunks = Body::into_data_stream(response.into_body());
        let mut read = String::new();
        until(&mut chunks, &mut read, r#""text":"one""#).await;
        let outside = app.dir.path().join("secrets");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("laravel.log"), "one\nsecret\n").unwrap();
        fs::remove_dir_all(root.join("storage/logs")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("storage/logs")).unwrap();
        until(&mut chunks, &mut read, ": site_log_unsafe\n").await;
        let rest = tokio::time::timeout(Duration::from_secs(60), async {
            while let Some(chunk) = chunks.next().await {
                read.push_str(std::str::from_utf8(&chunk.unwrap()).unwrap());
            }
        });
        rest.await.expect("the stream ends");
        assert!(!read.contains("secret"), "{read}");
    }

    #[tokio::test(start_paused = true)]
    async fn the_task_ends_when_the_client_leaves() {
        let temp = TempDir::new().unwrap();
        let (_, tail) = Tail::start(temp.path()).unwrap();
        let (sender, receiver) = mpsc::channel(1);
        let task = tokio::spawn(look(
            "shop".into(),
            tail,
            sender,
            CancellationToken::new(),
            LOOK_EVERY,
        ));
        drop(receiver);
        tokio::time::timeout(Duration::from_secs(60), task)
            .await
            .expect("the task ends")
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_follow_of_a_site_with_no_log_waits_for_the_file() {
        let app = App::new();
        let root = app.site("shop", true).await;
        let response = app
            .router
            .send("GET", "/v1/sites/shop/log?follow=true", None)
            .await;
        let mut chunks = Body::into_data_stream(response.into_body());
        tokio::time::sleep(LOOK_EVERY * 3).await;
        fs::write(log_file(&root), "first\n").unwrap();
        let mut read = String::new();
        until(&mut chunks, &mut read, r#"{"seq":0,"text":"first"}"#).await;
    }

    fn append(path: &Path, text: &str) {
        use std::io::Write as _;
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }
}
