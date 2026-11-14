//! `POST /v1/uninstall`: take Marsa out of the system (macOS). Task M5-13.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `POST /v1/uninstall` | `UninstallReport`; then the daemon stops |
//!
//! The steps, on a blocking thread, in this order:
//!
//! 1. **Preflight**, as for setup but without its folder check: a
//!    `marsa-helper` that is missing or that another user could change ends
//!    the request before any prompt (`uninstall_helper_missing`,
//!    `uninstall_helper_unsafe`). A helper inside `~/Desktop`, `~/Documents`,
//!    or `~/Downloads` runs: root can run it from there, and `uninstall_all`
//!    does not read the binary, which is what fails there (task M4-17).
//! 2. **Helper:** `uninstall_all` behind the one admin prompt of ADR 0012:
//!    the `pf` redirect and its boot job, `/etc/resolver/test`, a trust
//!    added as root before ADR 0015, then `/Library/Application
//!    Support/Marsa` if that left it empty. A prompt cancelled, refused, or
//!    left open, or an answer that cannot be used, ends the request here:
//!    nothing else changes, and the daemon keeps running.
//! 3. **User trust:** `user_trust::remove`, as the user, from the record of
//!    the trusted certificate (ADR 0015). macOS may show its dialog.
//! 4. **Login file:** the file of task M5-03 that starts the tray icon.
//! 5. **Answer, then stop:** once the helper ran, the daemon cancels its own
//!    shutdown token. The server's graceful stop lets this answer finish, as
//!    for `POST /v1/shutdown`, then stops Caddy, the PHP pools, and the DNS
//!    server. The CLI waits for that.
//!
//! A failed step 3 or 4 is a failed result, not an error: the other steps
//! still ran, and running the uninstall again is safe, since each step is
//! `unchanged` once done. The data folder is the CLI's question, never the
//! daemon's.
//!
//! One request at a time, shared with setup ([`Setup`]): 409
//! `uninstall_running` or `setup_running`. Linux and Windows get 501
//! `uninstall_not_supported` for now.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use marsa_core::caddy::CaddyPaths;
use marsa_core::helper::{HelperOp, OpResult};
use marsa_core::recipes::Platform;
use marsa_core::types::{OpStatus, UninstallReport};
use tokio_util::sync::CancellationToken;

use super::api::HttpError;
use super::autostart_api::Autostart;
use super::setup_api::{
    CHECK_LIMIT, Flow, PIPE_GRACE, PROMPT_LIMIT, Running, Setup, is_macos, user_trust,
};
use super::state::Daemon;

/// The op name of the login file step in the report.
const AUTOSTART_REMOVE: &str = "autostart_remove";

/// The longest an uninstall takes, when every program runs to its limit:
/// `osascript`, then `security remove-trusted-cert` and `delete-certificate`
/// at [`PROMPT_LIMIT`] each (both may show a dialog), `find-certificate` at
/// [`CHECK_LIMIT`], and [`PIPE_GRACE`] after each of these four. A client
/// has to wait longer: `marsa_core::client::UNINSTALL_TIMEOUT` does, and a
/// test holds the two together.
pub const SLOWEST_UNINSTALL: Duration = Duration::from_secs(
    3 * PROMPT_LIMIT.as_secs() + CHECK_LIMIT.as_secs() + 4 * PIPE_GRACE.as_secs(),
);

/// The route on `setup`, the same one the setup route gets, for
/// `api::router` to merge before its fallbacks.
pub fn routes(setup: Arc<Setup>) -> Router<Arc<Daemon>> {
    let handler = move |State(daemon): State<Arc<Daemon>>| {
        let setup = Arc::clone(&setup);
        async move { start(daemon, setup).await }
    };
    Router::new().route("/v1/uninstall", post(handler))
}

async fn start(daemon: Arc<Daemon>, setup: Arc<Setup>) -> Result<Json<UninstallReport>, HttpError> {
    let platform = setup
        .platform()
        .filter(|platform| is_macos(*platform))
        .ok_or_else(|| {
            let message = "the uninstall runs on macOS only for now";
            HttpError::new(
                StatusCode::NOT_IMPLEMENTED,
                "uninstall_not_supported",
                message,
            )
        })?;
    let running = Running::take(&setup, Flow::Uninstall).map_err(Flow::busy)?;
    tracing::info!("uninstall started");
    let job = tokio::task::spawn_blocking(move || {
        // Moved in, so no setup starts until this one ended, even if the
        // client went away.
        let _running = running;
        let report = run(
            &setup,
            &daemon.data_dir,
            &daemon.autostart,
            platform,
            &daemon.shutdown,
        );
        tracing::info!(
            results = report.results.len(),
            error = report.error.as_ref().map(|error| error.code.as_str()),
            stopping = report.stopping,
            "uninstall finished"
        );
        if report.stopping {
            // Here, not in the handler: the daemon stops even if the client
            // went away. The server stops its processes first, then lets
            // open requests finish, so this answer still goes out.
            tracing::info!("Marsa was uninstalled; the daemon stops");
            daemon.shutdown.cancel();
        }
        report
    });
    job.await.map(Json).map_err(|err| {
        tracing::error!(error = %err, "the uninstall job failed");
        let message = "the uninstall job failed";
        HttpError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
    })
}

/// The uninstall itself. Blocking: it runs programs and deletes files. The
/// steps after the helper run only once the helper ran: a prompt cancelled
/// or refused leaves everything as it was.
pub fn run(
    setup: &Setup,
    data_dir: &Path,
    autostart: &Autostart,
    platform: Platform,
    stop: &CancellationToken,
) -> UninstallReport {
    let mut report = UninstallReport::default();
    match setup.launch(
        Flow::Uninstall,
        vec![HelperOp::UninstallAll],
        platform,
        stop,
    ) {
        Ok(results) => report.results = results,
        Err(error) => {
            tracing::warn!(code = %error.code, "the helper did not run; nothing else changes");
            report.error = Some(error);
            return report;
        }
    }
    report.results.push(untrust_for_user(setup, data_dir, stop));
    report.results.push(remove_login_file(autostart));
    report.stopping = true;
    report
}

/// The current user's trust in Marsa's CA (ADR 0015), from the record in
/// `data_dir`, or `trust_remove_failed` when the home folder, and so the
/// login keychain, is not known.
fn untrust_for_user(setup: &Setup, data_dir: &Path, stop: &CancellationToken) -> OpResult {
    let ca_root = CaddyPaths::in_dir(data_dir).ca_root();
    let record = user_trust::record_path(data_dir);
    match setup.home() {
        Some(home) => user_trust::remove(setup.runner(), home, &ca_root, &record, stop),
        None => {
            tracing::warn!("the home folder is not known, so the login keychain cannot be found");
            let message = "the home folder of the current user is not known, so the login \
                           keychain cannot be found";
            user_trust::failed(
                user_trust::TRUST_REMOVE,
                user_trust::TRUST_REMOVE_FAILED,
                message,
            )
        }
    }
}

/// The login file of task M5-03: `done` if it was there, `unchanged` if
/// not, or failed with the code of the autostart route.
fn remove_login_file(autostart: &Autostart) -> OpResult {
    let (status, code, message) = match autostart.remove_for_uninstall() {
        Ok(true) => (OpStatus::Done, None, None),
        Ok(false) => (OpStatus::Unchanged, None, None),
        Err(err) => (
            OpStatus::Failed,
            Some(err.code.to_owned()),
            Some(err.message),
        ),
    };
    OpResult {
        op: AUTOSTART_REMOVE.to_owned(),
        status,
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::{Mutex, mpsc};

    use axum::body::{Body, to_bytes};
    use axum::extract::Request;
    use axum::middleware::from_fn_with_state;
    use marsa_core::helper::HelperRequest;
    use marsa_core::platform::autostart::{self, AutostartKind};
    use marsa_core::platform::daemon_file::{Token, daemon_authority};
    use marsa_core::types::ErrorBody;
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tower::ServiceExt as _;

    use crate::daemon::auth::{AuthConfig, require_auth};
    use crate::daemon::setup_api::{self, RunOutput, Runner};

    /// The daemon port in the `Host` header. No socket is bound for it.
    const PORT: u16 = 51237;
    const OSASCRIPT: &str = "/usr/bin/osascript";
    const SECURITY: &str = "/usr/bin/security";
    const TEST_CA: &str = include_str!("../../../../fixtures/helper/test-ca.pem");
    const REJECTED: &str = include_str!("../../../../fixtures/helper/responses/rejected.json");
    /// `openssl x509 -in test-ca.pem -outform der | shasum -a 256`, as in
    /// `user_trust.rs`.
    const HASH: &str = "29E433200254DFCE602212438E9D64E306B672DB9BC4642A7620CEBCC0FD8057";
    /// ADR 0012, written out, as in the setup tests.
    const SCRIPT: &str = "do shell script (quoted form of item 1 of argv) & \" apply \" & \
                          (quoted form of item 2 of argv) with administrator privileges";
    /// The removals of `uninstall_all` on macOS (`ops/mod.rs` of the helper).
    const REMOVALS: [&str; 3] = ["pf_remove", "resolver_remove", "trust_remove"];

    type Answer = Box<dyn Fn(&'static str, &[String]) -> io::Result<RunOutput> + Send + Sync>;

    /// Records every call and answers by program and arguments. Runs
    /// nothing: no `osascript`, no `security`, no helper.
    struct Fake {
        calls: Mutex<Vec<(&'static str, Vec<String>, Duration)>>,
        answer: Answer,
    }

    impl Fake {
        fn new(
            answer: impl Fn(&'static str, &[String]) -> io::Result<RunOutput> + Send + Sync + 'static,
        ) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::default(),
                answer: Box::new(answer),
            })
        }

        /// The helper answers `osascript()`; `security` finds the test CA
        /// trusted and in the keychain, and removes both.
        fn with_helper(
            osascript: impl Fn() -> io::Result<RunOutput> + Send + Sync + 'static,
        ) -> Arc<Self> {
            Self::new(move |program, args| match program {
                OSASCRIPT => osascript(),
                SECURITY => security(args),
                other => panic!("unexpected program {other}"),
            })
        }

        /// A run where every step goes as planned.
        fn all_done() -> Arc<Self> {
            Self::with_helper(|| ok(&answer(&REMOVALS)))
        }

        /// A helper that must never be asked.
        fn no_prompt() -> Arc<Self> {
            Self::new(|program, args| panic!("{program} {args:?} ran"))
        }

        fn programs(&self) -> Vec<&'static str> {
            let calls = self.calls.lock().unwrap();
            calls.iter().map(|(program, _, _)| *program).collect()
        }

        fn calls(&self) -> Vec<(&'static str, Vec<String>, Duration)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Runner for Fake {
        fn run(
            &self,
            program: &'static str,
            args: &[OsString],
            limit: Duration,
            _stop: &CancellationToken,
        ) -> io::Result<RunOutput> {
            let args: Vec<String> = args
                .iter()
                .map(|arg| arg.to_str().unwrap().to_owned())
                .collect();
            self.calls
                .lock()
                .unwrap()
                .push((program, args.clone(), limit));
            (self.answer)(program, &args)
        }
    }

    /// `security` on a Mac where the user trusts the test CA: every command
    /// exits 0, and the listing holds the test CA.
    fn security(args: &[String]) -> io::Result<RunOutput> {
        match args[0].as_str() {
            "find-certificate" => ok(&format!(
                "SHA-256 hash: {HASH}\nSHA-1 hash: 0123456789ABCDEF0123456789ABCDEF01234567\n\
                 {TEST_CA}"
            )),
            _ => ok(""),
        }
    }

    fn ok(stdout: &str) -> io::Result<RunOutput> {
        Ok(RunOutput {
            code: Some(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }

    /// What `osascript` gives when the script failed.
    fn script_error(stderr: &str) -> io::Result<RunOutput> {
        Ok(RunOutput {
            code: Some(1),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    /// The helper's answer: one done result per op.
    fn answer(ops: &[&str]) -> String {
        let results: Vec<Value> = ops
            .iter()
            .map(|op| json!({ "op": op, "status": "done" }))
            .collect();
        json!({ "version": 1, "results": results }).to_string()
    }

    fn result(op: &str, status: OpStatus) -> OpResult {
        OpResult {
            op: op.to_owned(),
            status,
            code: None,
            message: None,
        }
    }

    /// A data folder with Caddy's CA root and the record of the trusted
    /// certificate, a `marsa-helper` in `bin/` with the modes of an
    /// install, a login folder with the login file in it, and a home folder
    /// that is a path only.
    struct Dir {
        dir: TempDir,
    }

    impl Dir {
        fn new() -> Self {
            let dir = Self {
                dir: TempDir::new().unwrap(),
            };
            for path in [dir.ca_root(), dir.record()] {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, TEST_CA).unwrap();
            }
            make_helper(&dir.helper());
            autostart::write(AutostartKind::LaunchAgent, &dir.login(), &dir.tray()).unwrap();
            dir
        }

        fn data(&self) -> PathBuf {
            self.dir.path().join("data")
        }

        fn ca_root(&self) -> PathBuf {
            CaddyPaths::in_dir(&self.data()).ca_root()
        }

        fn record(&self) -> PathBuf {
            user_trust::record_path(&self.data())
        }

        fn helper(&self) -> PathBuf {
            self.dir.path().join("bin").join("marsa-helper")
        }

        fn tray(&self) -> PathBuf {
            self.dir.path().join("bin").join("marsa-tray")
        }

        fn home(&self) -> PathBuf {
            self.dir.path().join("home")
        }

        fn login(&self) -> PathBuf {
            self.dir.path().join("login")
        }

        fn login_file(&self) -> PathBuf {
            AutostartKind::LaunchAgent.file_in(&self.login())
        }

        fn setup(&self, fake: &Arc<Fake>) -> Setup {
            self.setup_with(fake, Some(Platform::DarwinArm64), Some(self.helper()))
        }

        fn setup_with(
            &self,
            fake: &Arc<Fake>,
            platform: Option<Platform>,
            helper: Option<PathBuf>,
        ) -> Setup {
            let runner: Arc<dyn Runner> = fake.clone();
            Setup::with(runner, platform, helper, Some(self.home()))
        }

        fn daemon(&self) -> Arc<Daemon> {
            let mut daemon = Daemon::new(self.data());
            daemon.autostart = Autostart::with(
                Some(AutostartKind::LaunchAgent),
                Some(self.login()),
                Some(self.tray()),
            );
            Arc::new(daemon)
        }

        /// [`run`] on macOS, without the HTTP layer.
        fn run(&self, setup: &Setup) -> UninstallReport {
            let daemon = self.daemon();
            let stop = CancellationToken::new();
            run(
                setup,
                &self.data(),
                &daemon.autostart,
                Platform::DarwinArm64,
                &stop,
            )
        }

        /// Nothing of the user's side changed: the login file, the record,
        /// and Caddy's CA are all still there.
        fn untouched(&self) -> bool {
            self.login_file().is_file() && self.record().is_file() && self.ca_root().is_file()
        }
    }

    fn make_helper(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
        #[cfg(unix)]
        for path in [path, path.parent().unwrap()] {
            set_mode(path, 0o755);
        }
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// The routes behind the real auth layer, on one daemon. Both routes
    /// share one [`Setup`], as in `api::router`.
    struct App {
        router: Router,
        bearer: String,
        daemon: Arc<Daemon>,
    }

    impl App {
        fn new(dir: &Dir, setup: Setup) -> Self {
            let token = Token::generate().unwrap();
            let bearer = token.bearer_header();
            let auth = AuthConfig::new(Arc::new(token), PORT);
            let setup = Arc::new(setup);
            let daemon = dir.daemon();
            let router = setup_api::routes(Arc::clone(&setup))
                .merge(routes(setup))
                .with_state(Arc::clone(&daemon))
                .layer(from_fn_with_state(auth, require_auth));
            Self {
                router,
                bearer,
                daemon,
            }
        }

        /// `POST <path>` with the right headers.
        async fn post(&self, path: &str) -> (StatusCode, Value) {
            let host = daemon_authority(PORT);
            let headers = [("host", host.as_str()), ("authorization", &self.bearer)];
            self.send(path, &headers).await
        }

        async fn send(&self, path: &str, headers: &[(&str, &str)]) -> (StatusCode, Value) {
            let mut request = Request::builder().method("POST").uri(path);
            for &(name, value) in headers {
                request = request.header(name, value);
            }
            let request = request.body(Body::empty()).unwrap();
            let response = self.router.clone().oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
            (status, serde_json::from_slice(&bytes).unwrap())
        }

        async fn uninstall(&self) -> UninstallReport {
            let (status, json) = self.post("/v1/uninstall").await;
            assert_eq!(status, StatusCode::OK, "{json}");
            serde_json::from_value(json).unwrap()
        }

        /// The status and error code of a refused request.
        async fn refused(&self, path: &str) -> (StatusCode, String) {
            let (status, json) = self.post(path).await;
            let error: ErrorBody = serde_json::from_value(json).unwrap();
            (status, error.error.code)
        }

        fn stopping(&self) -> bool {
            self.daemon.shutdown.is_cancelled()
        }
    }

    #[tokio::test]
    async fn the_whole_uninstall_over_http_then_the_daemon_stops() {
        let dir = Dir::new();
        let fake = Fake::all_done();
        let app = App::new(&dir, dir.setup(&fake));

        let report = app.uninstall().await;
        let want = UninstallReport {
            results: vec![
                result("pf_remove", OpStatus::Done),
                result("resolver_remove", OpStatus::Done),
                result("trust_remove", OpStatus::Done),
                result("user_trust_remove", OpStatus::Done),
                result("autostart_remove", OpStatus::Done),
            ],
            error: None,
            stopping: true,
        };
        assert_eq!(report, want);
        // The answer went out, and the daemon stops after it.
        assert!(app.stopping());
        assert!(!dir.login_file().exists());
        assert!(!dir.record().exists());
        // The data folder is the CLI's question: Caddy's CA is still there.
        assert!(dir.ca_root().is_file());

        // One admin prompt, with the constant script of ADR 0012 and the
        // one request; then the user's trust, from the record.
        let calls = fake.calls();
        assert_eq!(fake.programs(), [OSASCRIPT, SECURITY, SECURITY, SECURITY]);
        let (_, args, limit) = &calls[0];
        let helper = dir.helper();
        let helper = helper.to_str().unwrap();
        assert_eq!(
            args[..7],
            ["-e", "on run argv", "-e", SCRIPT, "-e", "end run", helper]
        );
        assert_eq!(args.len(), 8);
        let request: HelperRequest = serde_json::from_str(&args[7]).unwrap();
        assert_eq!(request, HelperRequest::new(vec![HelperOp::UninstallAll]));
        assert_eq!(
            serde_json::from_str::<Value>(&args[7]).unwrap(),
            json!({ "version": 1, "ops": [{ "op": "uninstall_all" }] })
        );
        assert_eq!(*limit, Duration::from_secs(300));
        let record = dir.record();
        assert_eq!(
            calls[1].1,
            ["remove-trusted-cert", record.to_str().unwrap()]
        );
        assert_eq!(calls[3].1[0..3], ["delete-certificate", "-Z", HASH]);
    }

    #[tokio::test]
    async fn a_cancelled_prompt_changes_nothing_else_and_the_daemon_keeps_running() {
        // The prompt cancelled (in two languages), a wrong password or no
        // prompt at all, left open, and the daemon stopping meanwhile.
        type Osascript = fn() -> io::Result<RunOutput>;
        let cases: [(&str, Osascript); 6] = [
            ("uninstall_cancelled", || {
                script_error("0:180: execution error: User canceled. (-128)\n")
            }),
            ("uninstall_cancelled", || {
                script_error("0:180: execution error: Vom Benutzer abgebrochen. (-128)\r")
            }),
            ("uninstall_not_authorized", || {
                script_error(
                    "0:180: execution error: The administrator user name or password was incorrect. (-60005)\n",
                )
            }),
            ("uninstall_not_authorized", || {
                script_error(
                    "0:180: execution error: The authorization was denied since no user interaction was possible. (-60007)\n",
                )
            }),
            ("uninstall_timed_out", || {
                Err(io::Error::from(io::ErrorKind::TimedOut))
            }),
            ("uninstall_interrupted", || {
                Err(io::Error::from(io::ErrorKind::Interrupted))
            }),
        ];
        for (code, osascript) in cases {
            let dir = Dir::new();
            let fake = Fake::with_helper(osascript);
            let app = App::new(&dir, dir.setup(&fake));

            let report = app.uninstall().await;
            let error = report.error.as_ref().expect(code);
            assert_eq!(error.code, code);
            assert!(report.results.is_empty(), "{code}");
            assert!(!report.stopping, "{code}");
            // Nothing else ran: no `security`, no login file, no shutdown.
            assert_eq!(fake.programs(), [OSASCRIPT], "{code}");
            assert!(dir.untouched(), "{code}");
            assert!(!app.stopping(), "{code}");
        }
    }

    #[tokio::test]
    async fn a_helper_that_refuses_or_cannot_be_read_changes_nothing_else() {
        let rejected = || {
            let line = serde_json::from_str::<Value>(REJECTED).unwrap();
            script_error(&format!("7:83: execution error: {line} (2)\n"))
        };
        type Osascript = Box<dyn Fn() -> io::Result<RunOutput> + Send + Sync>;
        let cases: [(&str, Osascript); 3] = [
            ("uninstall_helper_rejected", Box::new(rejected)),
            (
                "uninstall_helper_failed",
                Box::new(|| ok("garbage from the helper")),
            ),
            (
                "uninstall_helper_failed",
                Box::new(|| Err(io::Error::from(io::ErrorKind::NotFound))),
            ),
        ];
        for (code, osascript) in cases {
            let dir = Dir::new();
            let fake = Fake::with_helper(osascript);
            let app = App::new(&dir, dir.setup(&fake));
            let report = app.uninstall().await;
            assert_eq!(report.error.as_ref().map(|e| e.code.as_str()), Some(code));
            assert!(report.results.is_empty() && !report.stopping, "{code}");
            assert_eq!(fake.programs(), [OSASCRIPT], "{code}");
            assert!(dir.untouched() && !app.stopping(), "{code}");
        }
    }

    #[tokio::test]
    async fn a_helper_in_a_protected_folder_runs_the_uninstall_as_usual() {
        // Root runs a helper from these folders through `osascript`; only
        // reading the binary fails there, and `uninstall_all` reads none.
        for folder in ["Desktop", "Documents", "Downloads", "desktop"] {
            let dir = Dir::new();
            let helper = dir.home().join(folder).join("marsa").join("marsa-helper");
            make_helper(&helper);
            let fake = Fake::all_done();
            let setup = dir.setup_with(&fake, Some(Platform::DarwinArm64), Some(helper.clone()));
            let app = App::new(&dir, setup);

            let report = app.uninstall().await;
            assert_eq!(report.error, None, "{folder}");
            assert!(report.stopping && app.stopping(), "{folder}");
            assert_eq!(report.results.len(), 5, "{folder}");
            assert!(
                report
                    .results
                    .iter()
                    .all(|result| result.status == OpStatus::Done),
                "{folder}: {:?}",
                report.results
            );
            // The one prompt ran that helper.
            let calls = fake.calls();
            assert_eq!(calls[0].0, OSASCRIPT, "{folder}");
            assert_eq!(calls[0].1[6], helper.to_str().unwrap(), "{folder}");
            assert!(!dir.login_file().exists() && !dir.record().exists());
        }
    }

    #[test]
    fn a_missing_helper_or_one_others_could_change_is_refused_before_any_prompt() {
        let dir = Dir::new();
        let fake = Fake::no_prompt();
        let missing = dir.dir.path().join("nowhere").join("marsa-helper");
        for helper in [None, Some(missing)] {
            let setup = dir.setup_with(&fake, Some(Platform::DarwinArm64), helper);
            let report = dir.run(&setup);
            let code = report.error.map(|error| error.code);
            assert_eq!(code.as_deref(), Some("uninstall_helper_missing"));
        }
        #[cfg(unix)]
        {
            set_mode(&dir.helper(), 0o775);
            let report = dir.run(&dir.setup(&fake));
            let code = report.error.map(|error| error.code);
            assert_eq!(code.as_deref(), Some("uninstall_helper_unsafe"));
        }
        assert!(fake.programs().is_empty());
        assert!(dir.untouched());
    }

    #[tokio::test]
    async fn a_request_without_the_token_with_an_origin_or_another_host_runs_nothing() {
        let dir = Dir::new();
        let fake = Fake::all_done();
        let app = App::new(&dir, dir.setup(&fake));
        let host = daemon_authority(PORT);
        let other_host = daemon_authority(PORT + 1);
        let wrong = Token::generate().unwrap().bearer_header();
        let bearer = app.bearer.clone();
        let cases = [
            (
                "no token",
                vec![("host", host.as_str())],
                (StatusCode::UNAUTHORIZED, "unauthorized"),
            ),
            (
                "a wrong token",
                vec![("host", host.as_str()), ("authorization", wrong.as_str())],
                (StatusCode::UNAUTHORIZED, "unauthorized"),
            ),
            (
                "an Origin header",
                vec![
                    ("host", host.as_str()),
                    ("authorization", bearer.as_str()),
                    ("origin", "http://evil.test"),
                ],
                (StatusCode::FORBIDDEN, "origin_rejected"),
            ),
            (
                "another port in Host",
                vec![
                    ("host", other_host.as_str()),
                    ("authorization", bearer.as_str()),
                ],
                (StatusCode::FORBIDDEN, "host_rejected"),
            ),
        ];
        for (name, headers, (status, code)) in cases {
            let (got, json) = app.send("/v1/uninstall", &headers).await;
            assert_eq!(got, status, "{name}");
            assert_eq!(json["error"]["code"], code, "{name}");
            assert!(fake.programs().is_empty(), "{name}");
            assert!(dir.untouched() && !app.stopping(), "{name}");
        }
        // The same route with the right headers runs: the refusals above
        // came from the auth layer.
        assert!(app.uninstall().await.stopping);
    }

    #[tokio::test]
    async fn linux_and_windows_get_uninstall_not_supported() {
        for platform in [Some(Platform::LinuxX64), Some(Platform::WindowsX64), None] {
            let dir = Dir::new();
            let fake = Fake::no_prompt();
            let setup = dir.setup_with(&fake, platform, Some(dir.helper()));
            let app = App::new(&dir, setup);
            let refused = (
                StatusCode::NOT_IMPLEMENTED,
                "uninstall_not_supported".to_owned(),
            );
            assert_eq!(app.refused("/v1/uninstall").await, refused, "{platform:?}");
            assert!(dir.untouched() && !app.stopping());
        }
    }

    #[tokio::test]
    async fn one_uninstall_or_setup_at_a_time() {
        let dir = Dir::new();
        let (started, mut prompt_shown) = tokio::sync::mpsc::unbounded_channel();
        let (go_on, answers) = mpsc::channel::<()>();
        let answers = Mutex::new(answers);
        let fake = Fake::with_helper(move || {
            started.send(()).unwrap();
            answers.lock().unwrap().recv().unwrap();
            ok(&answer(&REMOVALS))
        });
        let app = Arc::new(App::new(&dir, dir.setup(&fake)));

        let first = tokio::spawn({
            let app = Arc::clone(&app);
            async move { app.uninstall().await }
        });
        prompt_shown.recv().await.unwrap();
        // A second uninstall, and a setup, while the first waits at the
        // prompt.
        let busy = (StatusCode::CONFLICT, "uninstall_running".to_owned());
        assert_eq!(app.refused("/v1/uninstall").await, busy);
        assert_eq!(app.refused("/v1/setup").await, busy);
        go_on.send(()).unwrap();
        assert!(first.await.unwrap().stopping);
        assert_eq!(
            fake.programs().iter().filter(|p| **p == OSASCRIPT).count(),
            1
        );
    }

    #[tokio::test]
    async fn an_uninstall_while_a_setup_runs_gets_setup_running() {
        let dir = Dir::new();
        let (started, mut prompt_shown) = tokio::sync::mpsc::unbounded_channel();
        let (go_on, answers) = mpsc::channel::<()>();
        let answers = Mutex::new(answers);
        // Setup runs `netstat` first, then the prompt.
        let fake = Fake::new(move |program, _| match program {
            "/usr/sbin/netstat" => ok(""),
            OSASCRIPT => {
                started.send(()).unwrap();
                answers.lock().unwrap().recv().unwrap();
                ok(&answer(&["resolver_install"]))
            }
            other => panic!("unexpected program {other}"),
        });
        // No CA, so the setup asks the helper for the resolver only.
        fs::remove_file(dir.ca_root()).unwrap();
        let app = Arc::new(App::new(&dir, dir.setup(&fake)));
        let setup = tokio::spawn({
            let app = Arc::clone(&app);
            async move { app.post("/v1/setup").await.0 }
        });
        prompt_shown.recv().await.unwrap();
        let busy = (StatusCode::CONFLICT, "setup_running".to_owned());
        assert_eq!(app.refused("/v1/uninstall").await, busy);
        go_on.send(()).unwrap();
        assert_eq!(setup.await.unwrap(), StatusCode::OK);
        assert!(dir.login_file().is_file() && !app.stopping());
    }

    #[tokio::test]
    async fn failed_later_steps_are_results_and_the_daemon_still_stops() {
        let dir = Dir::new();
        // A removal of the helper failed, the user refused the dialog, and
        // the login file cannot be deleted.
        let helper_answer = json!({ "version": 1, "results": [
            { "op": "pf_remove", "status": "failed", "code": "pf_flush_failed", "message": "x" },
            { "op": "resolver_remove", "status": "unchanged" },
            { "op": "trust_remove", "status": "unchanged" },
        ] })
        .to_string();
        let fake = Fake::new(move |program, args| match program {
            OSASCRIPT => script_error(&format!("7:83: execution error: {helper_answer} (1)\n")),
            SECURITY if args[0] == "remove-trusted-cert" => {
                script_error("SecTrustSettingsRemoveTrustSettings: The authorization was denied.")
            }
            other => panic!("{other} {args:?} ran"),
        });
        // A folder where the login file goes: deleting it as a file fails on
        // every system. (A file where the folder goes is "not found" on
        // Windows, which counts as nothing to delete.)
        fs::remove_file(dir.login_file()).unwrap();
        fs::create_dir(dir.login_file()).unwrap();
        fs::write(dir.login_file().join("keep"), "x").unwrap();
        let app = App::new(&dir, dir.setup(&fake));

        let report = app.uninstall().await;
        assert_eq!(report.error, None);
        assert!(report.stopping && app.stopping());
        let codes: Vec<_> = report
            .results
            .iter()
            .map(|result| (result.op.as_str(), result.status, result.code.as_deref()))
            .collect();
        assert_eq!(
            codes,
            [
                ("pf_remove", OpStatus::Failed, Some("pf_flush_failed")),
                ("resolver_remove", OpStatus::Unchanged, None),
                ("trust_remove", OpStatus::Unchanged, None),
                (
                    "user_trust_remove",
                    OpStatus::Failed,
                    Some("trust_remove_failed")
                ),
                (
                    "autostart_remove",
                    OpStatus::Failed,
                    Some("autostart_write")
                ),
            ]
        );
        // The record stays, so the next uninstall still knows the CA.
        assert!(dir.record().is_file());
    }

    #[test]
    fn no_home_folder_is_a_failed_user_trust_step() {
        let dir = Dir::new();
        let fake = Fake::all_done();
        let runner: Arc<dyn Runner> = fake.clone();
        let setup = Setup::with(
            runner,
            Some(Platform::DarwinArm64),
            Some(dir.helper()),
            None,
        );
        let report = dir.run(&setup);
        let trust = &report.results[3];
        assert_eq!(trust.op, "user_trust_remove");
        assert_eq!(trust.code.as_deref(), Some("trust_remove_failed"));
        assert_eq!(fake.programs(), [OSASCRIPT]);
        assert!(report.stopping);
    }

    #[test]
    fn the_client_waits_longer_than_the_slowest_uninstall() {
        use marsa_core::client::UNINSTALL_TIMEOUT;

        // 15 min 10 s of limits, and 2 s of grace after each of 4 programs.
        assert_eq!(SLOWEST_UNINSTALL, Duration::from_secs(918));
        // With room for the HTTP round trip and for killing a stalled
        // program.
        let margin = Duration::from_secs(30);
        assert!(
            UNINSTALL_TIMEOUT >= SLOWEST_UNINSTALL + margin,
            "the client waits {UNINSTALL_TIMEOUT:?}, and an uninstall may take \
             {SLOWEST_UNINSTALL:?}"
        );

        // The sum holds every program an uninstall runs: a full run runs all
        // of them, each with its limit, so a new program in the flow fails
        // here until it is in `SLOWEST_UNINSTALL`.
        let dir = Dir::new();
        let fake = Fake::all_done();
        dir.run(&dir.setup(&fake));
        let calls = fake.calls();
        let programs = u32::try_from(calls.len()).unwrap();
        let limits: Duration = calls.iter().map(|(_, _, limit)| *limit).sum();
        assert_eq!(limits + PIPE_GRACE * programs, SLOWEST_UNINSTALL);
    }
}
