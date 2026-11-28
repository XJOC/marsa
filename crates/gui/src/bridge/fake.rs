//! A daemon for the bridge's tests that is not `marsad`: it holds
//! `run/daemon.lock` and writes `run/daemon.json` in a temp folder, answers
//! `/v1/health`, serves event streams the test can end, and whatever routes
//! the test adds. Loopback only; every other route is 404.

use std::convert::Infallible;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use marsa_core::data_dir::DataDir;
use marsa_core::platform::daemon_file::{DaemonFile, Token, daemon_lock_path, write_daemon_file};
use marsa_core::types::{API_VERSION, Health};
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{Sender, channel};
use tokio_stream::wrappers::ReceiverStream;

/// The longest a test waits for the bridge or the fake daemon.
pub(crate) const PATIENCE: Duration = Duration::from_secs(10);

type Shared<T> = Arc<Mutex<T>>;
type EventStreams = Shared<Vec<Sender<Result<String, Infallible>>>>;

fn lock<T>(shared: &Shared<T>) -> MutexGuard<'_, T> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) struct Fake {
    dir: TempDir,
    /// Every request seen: "METHOD /path".
    seen: Shared<Vec<String>>,
    /// One for each open event stream: dropping it ends the stream.
    streams: EventStreams,
    /// Held as `marsad` holds it, so the client sees a running daemon.
    _lock: File,
    /// The server's own runtime: the tests are plain threads.
    _runtime: Runtime,
}

impl Fake {
    pub(crate) fn start(routes: Router) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let run = DataDir::new(dir.path()).run();
        std::fs::create_dir_all(&run).expect("the run folder");
        let held = File::create(daemon_lock_path(&run)).expect("the lock file");
        held.try_lock().expect("the daemon lock");
        let seen = Shared::<Vec<String>>::default();
        let streams = EventStreams::default();
        let health = Health {
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            api_version: API_VERSION,
            pid: 42,
        };
        let open = Arc::clone(&streams);
        let router = routes
            .route(
                "/v1/health",
                get(move || {
                    let health = health.clone();
                    async move { Json(health) }
                }),
            )
            .route("/v1/events", get(move || events(Arc::clone(&open))))
            .layer(middleware::from_fn_with_state(Arc::clone(&seen), record));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime");
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("a loopback port");
        let port = listener.local_addr().expect("an address").port();
        runtime.spawn(async move { axum::serve(listener, router).await });
        let file = DaemonFile {
            pid: 42,
            port,
            token: Token::generate().expect("a token"),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
        };
        write_daemon_file(&run, &file).expect("the daemon file");
        Self {
            dir,
            seen,
            streams,
            _lock: held,
            _runtime: runtime,
        }
    }

    pub(crate) fn data_dir(&self) -> &Path {
        self.dir.path()
    }

    /// Ends every open event stream: the bridge loses its session.
    pub(crate) fn end_events(&self) {
        lock(&self.streams).clear();
    }

    /// How often `request` ("METHOD /path") came.
    pub(crate) fn count(&self, request: &str) -> usize {
        lock(&self.seen).iter().filter(|r| *r == request).count()
    }

    /// Waits until `request` came.
    pub(crate) fn wait_for(&self, request: &str) {
        let until = Instant::now() + PATIENCE;
        while self.count(request) == 0 {
            assert!(Instant::now() < until, "never came: {request}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

async fn events(open: EventStreams) -> Response {
    let (events, stream) = channel(1);
    lock(&open).push(events);
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(ReceiverStream::new(stream)))
        .expect("a response")
}

async fn record(State(seen): State<Shared<Vec<String>>>, request: Request, next: Next) -> Response {
    let line = format!("{} {}", request.method(), request.uri().path());
    lock(&seen).push(line);
    next.run(request).await
}
