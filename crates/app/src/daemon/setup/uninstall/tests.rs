use super::*;
#[cfg(unix)]
use crate::daemon::setup::test_support::set_mode;
use crate::daemon::setup::test_support::{Fake, make_helper, ok, script_error};
use crate::daemon::setup::windows::{Install, Service};
use marsa_core::data_dir::DataDir;
use marsa_core::types::{Event, JobAccepted, JobOutcome, JobUpdate};
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

use crate::daemon::http::auth::{AuthConfig, require_auth};
use crate::daemon::setup::{self as setup_api, RunOutput, Runner, linux_dns, nss};

/// The daemon port in the `Host` header. No socket is bound for it.
const PORT: u16 = 51237;
const OSASCRIPT: &str = "/usr/bin/osascript";
const SECURITY: &str = "/usr/bin/security";
const TEST_CA: &str = include_str!("../../../../../../fixtures/helper/test-ca.pem");
const REJECTED: &str = include_str!("../../../../../../fixtures/helper/responses/rejected.json");
/// `openssl x509 -in test-ca.pem -outform der | shasum -a 256`, as in
/// `user_trust.rs`.
const HASH: &str = "29E433200254DFCE602212438E9D64E306B672DB9BC4642A7620CEBCC0FD8057";
/// ADR 0012, written out, as in the setup tests.
const SCRIPT: &str = "do shell script (quoted form of item 1 of argv) & \" apply \" & \
                          (quoted form of item 2 of argv) with administrator privileges";
/// The removals of `uninstall_all` on macOS (`ops/mod.rs` of the helper).
const REMOVALS: [OpName; 3] = [
    OpName::PfRemove,
    OpName::ResolverRemove,
    OpName::TrustRemove,
];

impl Fake {
    /// The helper answers `osascript()`; `security` finds the test CA
    /// trusted and in the keychain, and removes both.
    fn with_helper(
        osascript: impl Fn() -> io::Result<RunOutput> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Self::answering(move |program, args| match program {
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
        Self::answering(|program, args| panic!("{program} {args:?} ran"))
    }

    fn recorded(&self) -> Vec<(&'static str, Vec<String>, Duration)> {
        let calls = self.calls.lock().unwrap();
        calls
            .iter()
            .map(|call| (call.program, call.args.clone(), call.limit))
            .collect()
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

/// The helper's answer: one done result per op.
fn answer(ops: &[OpName]) -> String {
    let results: Vec<Value> = ops
        .iter()
        .map(|op| json!({ "op": op, "status": "done" }))
        .collect();
    json!({ "version": 1, "results": results }).to_string()
}

fn result(op: OpName, status: OpStatus) -> OpResult {
    OpResult {
        op,
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
        DataDir::new(self.dir.path()).bin().join("marsa-helper")
    }

    fn tray(&self) -> PathBuf {
        DataDir::new(self.dir.path()).bin().join("marsa-tray")
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
        daemon.set_autostart(Autostart::with(
            Some(AutostartKind::LaunchAgent),
            Some(self.login()),
            Some(self.tray()),
        ));
        Arc::new(daemon)
    }

    /// [`run`] on macOS, without the HTTP layer.
    fn run(&self, setup: &Setup) -> UninstallReport {
        let daemon = self.daemon();
        let stop = CancellationToken::new();
        run(
            setup,
            &self.data(),
            daemon.autostart(),
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

/// The routes behind the real auth layer, on one daemon. Both routes
/// share one [`Setup`], as in `http::routes::router`.
struct App {
    router: Router,
    bearer: String,
    daemon: Arc<Daemon>,
}

impl App {
    fn new(dir: &Dir, setup: Setup) -> Self {
        Self::with_daemon(setup, dir.daemon())
    }

    fn with_daemon(setup: Setup, daemon: Arc<Daemon>) -> Self {
        let token = Token::generate().unwrap();
        let bearer = token.bearer_header();
        let auth = AuthConfig::new(Arc::new(token), PORT);
        let setup = Arc::new(setup);
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
        self.daemon.shutdown().is_cancelled()
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
            result(OpName::PfRemove, OpStatus::Done),
            result(OpName::ResolverRemove, OpStatus::Done),
            result(OpName::TrustRemove, OpStatus::Done),
            result(OpName::UserTrustRemove, OpStatus::Done),
            result(OpName::AutostartRemove, OpStatus::Done),
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
    let calls = fake.recorded();
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
    // The listing first, then the record's trust, then the certificate.
    assert_eq!(calls[1].1[0], "find-certificate");
    let record = dir.record();
    assert_eq!(
        calls[2].1,
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
        let calls = fake.recorded();
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
    // macOS, then Linux: on both, the auth layer refuses before any
    // prompt (`osascript` or `pkexec`).
    for linux in [false, true] {
        refusals_run_nothing(linux).await;
    }
}

async fn refusals_run_nothing(linux: bool) {
    let (dir, fake) = if linux {
        (linux_dir(), linux_fake(|| ok(&answer(&LINUX_REMOVALS))))
    } else {
        (Dir::new(), Fake::all_done())
    };
    let app = if linux {
        linux_app(&dir, &fake)
    } else {
        App::new(&dir, dir.setup(&fake))
    };
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
        for path in ["/v1/uninstall", "/v1/jobs/uninstall"] {
            let (got, json) = app.send(path, &headers).await;
            assert_eq!(got, status, "{name} {path}");
            assert_eq!(json["error"]["code"], code, "{name} {path}");
            assert!(fake.programs().is_empty(), "{name} {path}");
            assert!(dir.untouched() && !app.stopping(), "{name} {path}");
        }
    }
    // The same route with the right headers runs: the refusals above
    // came from the auth layer.
    assert!(app.uninstall().await.stopping);
}

#[tokio::test]
async fn a_system_without_a_build_gets_uninstall_not_supported() {
    for platform in [Some(Platform::Any), None] {
        let dir = Dir::new();
        let fake = Fake::no_prompt();
        let setup = dir.setup_with(&fake, platform, Some(dir.helper()));
        let app = App::new(&dir, setup);
        let refused = (
            StatusCode::NOT_IMPLEMENTED,
            "uninstall_not_supported".to_owned(),
        );
        for path in ["/v1/uninstall", "/v1/jobs/uninstall"] {
            assert_eq!(app.refused(path).await, refused, "{platform:?} {path}");
        }
        assert!(dir.untouched() && !app.stopping());
    }
}

/// The removals of `uninstall_all` on Linux (`ops/mod.rs` of the helper).
const LINUX_REMOVALS: [OpName; 4] = [
    OpName::PortsRemove,
    OpName::ResolvedRemove,
    OpName::HostsRemove,
    OpName::TrustRemove,
];
/// The NSS nickname of the test CA: the first 16 digits of [`HASH`].
const NICKNAME: &str = "Marsa Local CA 29e433200254dfce";

/// Linux: `pkexec` answers `pkexec()`, and `certutil` lists the test
/// CA's row in each database and deletes it.
fn linux_fake(pkexec: impl Fn() -> io::Result<RunOutput> + Send + Sync + 'static) -> Arc<Fake> {
    Fake::answering(move |program, args| match program {
        setup_api::pkexec::PKEXEC => pkexec(),
        nss::CERTUTIL if args[2] == "-L" => ok(&format!("{NICKNAME}     C,,\n")),
        nss::CERTUTIL => ok(""),
        other => panic!("unexpected program {other}"),
    })
}

/// A Linux data folder: the hosts marker, the NSS record of the test
/// CA, and Chrome's database in the home folder.
fn linux_dir() -> Dir {
    let dir = Dir::new();
    fs::write(linux_dns::marker(&dir.data()), "").unwrap();
    let files = user_trust::TrustFiles::in_dir(&dir.data());
    fs::write(
        nss::record_path(&files),
        format!("{}\n", HASH.to_lowercase()),
    )
    .unwrap();
    let chrome = dir.home().join(".pki").join("nssdb");
    fs::create_dir_all(&chrome).unwrap();
    fs::write(chrome.join("cert9.db"), "").unwrap();
    dir
}

fn linux_app(dir: &Dir, fake: &Arc<Fake>) -> App {
    App::new(
        dir,
        dir.setup_with(fake, Some(Platform::LinuxX64), Some(dir.helper())),
    )
}

#[tokio::test]
async fn linux_runs_the_helper_through_pkexec_then_takes_its_roots_out_of_nss() {
    let dir = linux_dir();
    let fake = linux_fake(|| ok(&answer(&LINUX_REMOVALS)));
    let app = linux_app(&dir, &fake);

    let report = app.uninstall().await;
    let mut want: Vec<_> = LINUX_REMOVALS
        .iter()
        .map(|op| result(*op, OpStatus::Done))
        .collect();
    want.push(result(OpName::NssRemove, OpStatus::Done));
    want.push(result(OpName::AutostartRemove, OpStatus::Done));
    assert_eq!(report.results, want);
    assert!(report.stopping && app.stopping());
    assert!(!linux_dns::marker(&dir.data()).exists());
    let files = user_trust::TrustFiles::in_dir(&dir.data());
    assert!(!nss::record_path(&files).exists());

    let calls = fake.recorded();
    let (program, args, limit) = &calls[0];
    assert_eq!(*program, setup_api::pkexec::PKEXEC);
    let helper = dir.helper();
    assert_eq!(
        args[..3],
        [
            "--disable-internal-agent",
            helper.to_str().unwrap(),
            "apply"
        ]
    );
    assert_eq!(
        serde_json::from_str::<Value>(&args[3]).unwrap(),
        json!({ "version": 1, "ops": [{ "op": "uninstall_all" }] })
    );
    assert_eq!(*limit, Duration::from_secs(300));
    // Chrome's database: its rows, then the one root of this folder.
    let chrome = format!("sql:{}", dir.home().join(".pki").join("nssdb").display());
    let tails: Vec<_> = calls[1..].iter().map(|(_, args, _)| args.clone()).collect();
    assert_eq!(
        tails,
        [
            vec!["-d", &chrome, "-L"],
            vec!["-d", &chrome, "-D", "-n", NICKNAME, "-f", "/dev/null"],
        ]
    );
    // The data folder is the CLI's question.
    assert!(dir.ca_root().is_file());
}

/// No `/usr/bin/pkexec`: nothing runs or changes, and the code says so.
#[tokio::test]
async fn no_pkexec_on_linux_changes_nothing() {
    let dir = linux_dir();
    let fake = linux_fake(|| Err(io::Error::from(io::ErrorKind::NotFound)));
    let app = linux_app(&dir, &fake);
    let report = app.uninstall().await;
    assert_eq!(
        report.error.map(|error| error.code).as_deref(),
        Some("uninstall_pkexec_missing")
    );
    assert!(report.results.is_empty() && !app.stopping());
    assert!(
        fake.programs()
            .iter()
            .all(|program| *program == setup_api::pkexec::PKEXEC)
    );
    assert!(dir.untouched() && linux_dns::marker(&dir.data()).exists());
}

/// A dismissed password dialog: nothing else runs or changes.
#[tokio::test]
async fn a_dismissed_password_dialog_on_linux_changes_nothing() {
    let dir = linux_dir();
    let fake = linux_fake(|| {
        Ok(RunOutput {
            code: Some(126),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    });
    let app = linux_app(&dir, &fake);
    let report = app.uninstall().await;
    assert_eq!(
        report.error.map(|error| error.code).as_deref(),
        Some("uninstall_cancelled")
    );
    assert!(report.results.is_empty() && !app.stopping());
    assert_eq!(fake.programs(), [setup_api::pkexec::PKEXEC]);
    assert!(dir.untouched() && linux_dns::marker(&dir.data()).exists());
    let files = user_trust::TrustFiles::in_dir(&dir.data());
    assert!(nss::record_path(&files).is_file());
}

/// A failed `hosts_remove` keeps the marker; a root `certutil` could
/// not delete stays in the record.
#[tokio::test]
async fn what_was_not_removed_on_linux_stays_recorded() {
    let dir = linux_dir();
    let failed = json!({ "version": 1, "results": [
            { "op": "ports_remove", "status": "done" },
            { "op": "resolved_remove", "status": "unchanged" },
            { "op": "hosts_remove", "status": "failed", "code": "hosts_write", "message": "x" },
            { "op": "trust_remove", "status": "done" },
        ] })
    .to_string();
    let fake = Fake::answering(move |program, args| match program {
        setup_api::pkexec::PKEXEC => Ok(RunOutput {
            code: Some(1),
            stdout: failed.clone().into_bytes(),
            stderr: Vec::new(),
        }),
        nss::CERTUTIL if args[2] == "-L" => ok(&format!("{NICKNAME}     C,,\n")),
        nss::CERTUTIL => Ok(RunOutput {
            code: Some(255),
            stdout: Vec::new(),
            stderr: b"SEC_ERROR_TOKEN_NOT_LOGGED_IN".to_vec(),
        }),
        other => panic!("unexpected program {other}"),
    });
    let app = linux_app(&dir, &fake);
    let report = app.uninstall().await;
    assert!(linux_dns::marker(&dir.data()).exists());
    let nss = report
        .results
        .iter()
        .find(|r| r.op == OpName::NssRemove)
        .unwrap();
    assert_eq!(nss.code.as_deref(), Some("nss_remove_failed"));
    let files = user_trust::TrustFiles::in_dir(&dir.data());
    assert_eq!(
        fs::read_to_string(nss::record_path(&files)).unwrap(),
        format!("{}\n", HASH.to_lowercase())
    );
}

/// A Windows helper service whose `uninstall` answers `answer`. Nothing
/// else may run in an uninstall.
struct WindowsService {
    answer: Install,
    calls: Mutex<usize>,
}

impl Service for WindowsService {
    fn user_sid(&self) -> Result<String, String> {
        panic!("no install in an uninstall")
    }

    fn send(&self, _: &[u8], _: &CancellationToken) -> Result<Vec<u8>, &'static str> {
        panic!("no request through the pipe in an uninstall")
    }

    fn install(&self, _: &Path, _: &str, _: &str) -> Install {
        panic!("no install in an uninstall")
    }

    fn uninstall(&self, _helper: &Path) -> Install {
        *self.calls.lock().unwrap() += 1;
        self.answer.clone()
    }
}

/// Windows, with the helper in the user's folder (`home`, unless given),
/// as an install puts it, and no login file: Windows has none of
/// Marsa's yet.
fn windows_app(dir: &Dir, answer: Install, home: Option<PathBuf>) -> (App, Arc<WindowsService>) {
    let fake = Fake::no_prompt();
    let service = Arc::new(WindowsService {
        answer,
        calls: Mutex::new(0),
    });
    let runner: Arc<dyn Runner> = fake;
    let home = home.unwrap_or_else(|| dir.dir.path().to_path_buf());
    let setup = Setup::with(
        runner,
        Some(Platform::WindowsX64),
        Some(dir.helper()),
        Some(home),
    )
    .with_windows(service.clone());
    let mut daemon = Daemon::new(dir.data());
    daemon.set_autostart(Autostart::with(None, None, None));
    (App::with_daemon(setup, Arc::new(daemon)), service)
}

/// Windows: the helper's `uninstall` behind one UAC prompt, then the
/// marker goes and the daemon stops. With no login file step, every
/// step is done.
#[tokio::test]
async fn windows_runs_the_helpers_uninstall_then_stops() {
    let dir = Dir::new();
    let marker = dir.data().join(hosts_sync::MARKER);
    fs::write(&marker, "").unwrap();
    let (app, service) = windows_app(&dir, Install::Exited(0), None);
    let (status, report) = app.post("/v1/uninstall").await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let steps: Vec<(&str, &str)> = report["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| {
            (
                step["op"].as_str().unwrap(),
                step["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(steps, [("uninstall_all", "done")], "{report}");
    assert!(report["error"].is_null(), "{report}");
    assert!(app.stopping());
    assert!(!marker.exists());
    assert_eq!(*service.calls.lock().unwrap(), 1);
}

/// The helper did not remove everything, or did not run: nothing else
/// changes, the daemon runs on, and the marker stays, since the service
/// may still be installed.
#[tokio::test]
async fn windows_keeps_the_marker_unless_everything_went() {
    for (answer, code) in [
        (Install::Exited(1), "uninstall_helper_partial"),
        (Install::Cancelled, "uninstall_cancelled"),
        (Install::TimedOut, "uninstall_helper_running"),
        (Install::StillRunning, "uninstall_helper_running"),
        (Install::Exited(2), "uninstall_helper_failed"),
    ] {
        let dir = Dir::new();
        let marker = dir.data().join(hosts_sync::MARKER);
        fs::write(&marker, "").unwrap();
        let (app, _) = windows_app(&dir, answer.clone(), None);
        let (_, report) = app.post("/v1/uninstall").await;
        assert_eq!(report["error"]["code"], code, "{report}");
        assert!(marker.exists(), "{code}");
        assert!(dir.untouched() && !app.stopping());
    }
}

/// Windows: a helper outside the user's folder gets no prompt, and the
/// marker of the running service stays.
#[tokio::test]
async fn windows_refuses_a_helper_outside_the_users_folder() {
    let dir = Dir::new();
    let marker = dir.data().join(hosts_sync::MARKER);
    fs::write(&marker, "").unwrap();
    let (app, service) = windows_app(&dir, Install::Exited(0), Some(dir.home()));
    let (_, report) = app.post("/v1/uninstall").await;
    assert_eq!(
        report["error"]["code"], "uninstall_helper_unsafe",
        "{report}"
    );
    assert_eq!(*service.calls.lock().unwrap(), 0);
    assert!(marker.exists());
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

/// `POST /v1/jobs/uninstall` answers `202` while the helper still waits at
/// its prompt; a second uninstall or setup, on either route, gets 409;
/// and the end is in the jobs table before the daemon starts to stop, so
/// a reader gets `done` and then `shutting_down` (ADR 0026).
#[tokio::test]
async fn the_job_route_answers_at_once_and_done_comes_before_the_stop() {
    let dir = Dir::new();
    let (started, mut prompt_shown) = tokio::sync::mpsc::unbounded_channel();
    let (go_on, answers) = mpsc::channel::<()>();
    let answers = Mutex::new(answers);
    let fake = Fake::with_helper(move || {
        started.send(()).unwrap();
        answers.lock().unwrap().recv().unwrap();
        ok(&answer(&REMOVALS))
    });
    let app = App::new(&dir, dir.setup(&fake));
    let daemon = Arc::clone(&app.daemon);
    let mut events = daemon.events().subscribe();
    // What a reader of the stream, and of the table, could find when the
    // stop begins: the server sends `shutting_down` after this, on the
    // same channel.
    let at_the_stop = tokio::spawn({
        let daemon = Arc::clone(&daemon);
        async move {
            daemon.shutdown().cancelled().await;
            daemon.jobs().running().is_empty()
        }
    });

    let (status, json) = app.post("/v1/jobs/uninstall").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{json}");
    let accepted: JobAccepted = serde_json::from_value(json).unwrap();
    prompt_shown.recv().await.unwrap();
    assert!(!app.stopping());
    assert_eq!(daemon.jobs().running().len(), 1);
    let busy = (StatusCode::CONFLICT, "uninstall_running".to_owned());
    for path in [
        "/v1/uninstall",
        "/v1/jobs/uninstall",
        "/v1/setup",
        "/v1/jobs/setup",
    ] {
        assert_eq!(app.refused(path).await, busy, "{path}");
    }

    go_on.send(()).unwrap();
    assert!(at_the_stop.await.unwrap());
    // `done` is already on the stream when the stop begins.
    let last = loop {
        match events.try_recv().unwrap() {
            Event::Job(event) if event.is_last() => break event,
            _ => {}
        }
    };
    assert_eq!(last.id, accepted.id);
    assert_eq!(daemon.jobs().get(accepted.id).as_ref(), Some(&last));
    let JobUpdate::Done(JobOutcome::Uninstall(report)) = last.update else {
        panic!("not done: {last:?}");
    };
    assert!(report.stopping);
}

#[tokio::test]
async fn an_uninstall_while_a_setup_runs_gets_setup_running() {
    let dir = Dir::new();
    let (started, mut prompt_shown) = tokio::sync::mpsc::unbounded_channel();
    let (go_on, answers) = mpsc::channel::<()>();
    let answers = Mutex::new(answers);
    // Setup runs `netstat` first, then the prompt.
    let fake = Fake::answering(move |program, _| match program {
        "/usr/sbin/netstat" => ok(""),
        OSASCRIPT => {
            started.send(()).unwrap();
            answers.lock().unwrap().recv().unwrap();
            ok(&answer(&[OpName::ResolverInstall]))
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
    for path in ["/v1/uninstall", "/v1/jobs/uninstall"] {
        assert_eq!(app.refused(path).await, busy, "{path}");
    }
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
    let fake = Fake::answering(move |program, args| match program {
        OSASCRIPT => script_error(&format!("7:83: execution error: {helper_answer} (1)\n")),
        SECURITY if args[0] == "find-certificate" => security(args),
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
        .map(|result| (result.op, result.status, result.code.as_deref()))
        .collect();
    assert_eq!(
        codes,
        [
            (OpName::PfRemove, OpStatus::Failed, Some("pf_flush_failed")),
            (OpName::ResolverRemove, OpStatus::Unchanged, None),
            (OpName::TrustRemove, OpStatus::Unchanged, None),
            (
                OpName::UserTrustRemove,
                OpStatus::Failed,
                Some("trust_remove_failed")
            ),
            (
                OpName::AutostartRemove,
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
    assert_eq!(trust.op, OpName::UserTrustRemove);
    assert_eq!(trust.code.as_deref(), Some("trust_remove_failed"));
    assert_eq!(fake.programs(), [OSASCRIPT]);
    assert!(report.stopping);
}

#[test]
fn old_clients_wait_longer_than_the_slowest_uninstall() {
    // What clients before ADR 0026 wait for `POST /v1/uninstall`.
    const OLD_UNINSTALL_WAIT: Duration = Duration::from_secs(40 * 60);

    // 15 min 10 s of limits, and 2 s of grace after each of 4 programs.
    assert_eq!(SLOWEST_UNINSTALL_ALL, Duration::from_secs(918));
    // With room for the HTTP round trip and for killing a stalled
    // program.
    let margin = Duration::from_secs(30);
    assert!(
        OLD_UNINSTALL_WAIT >= SLOWEST_UNINSTALL_ALL + margin,
        "an old client waits {OLD_UNINSTALL_WAIT:?}, and an uninstall may take \
             {SLOWEST_UNINSTALL_ALL:?}"
    );
    // Windows: the helper's run after the UAC prompt, to its limit. The
    // prompt itself has none.
    let windows = crate::daemon::setup::windows::INSTALL_LIMIT;
    assert!(
        OLD_UNINSTALL_WAIT >= windows + margin,
        "an old client waits {OLD_UNINSTALL_WAIT:?}, and a Windows uninstall may take \
             {windows:?}"
    );
    // Linux: the pkexec prompt, then the NSS step, which keeps its own
    // budget, pipe grace included.
    let linux = PROMPT_LIMIT + PIPE_GRACE + nss::NSS_BUDGET;
    assert!(
        SLOWEST_UNINSTALL_ALL >= linux,
        "a Linux uninstall may take {linux:?}"
    );

    // The sum holds every program an uninstall runs: a full run runs all
    // of them, each with its limit, so a new program in the flow fails
    // here until it is in `SLOWEST_UNINSTALL_ALL`.
    let dir = Dir::new();
    let fake = Fake::all_done();
    dir.run(&dir.setup(&fake));
    let calls = fake.recorded();
    let programs = u32::try_from(calls.len()).unwrap();
    let limits: Duration = calls.iter().map(|(_, _, limit)| *limit).sum();
    assert_eq!(limits + PIPE_GRACE * programs, SLOWEST_UNINSTALL_ALL);
}
