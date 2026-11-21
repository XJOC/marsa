//! Fakes and fixtures shared by the tests of `setup_api` and its children.

use super::helper::op_name;
use super::osascript::OSASCRIPT;
use super::ports::{NETSTAT, PS};
use super::user_trust::SECURITY;
use super::*;
use crate::daemon::auth::{AuthConfig, require_auth};
use crate::daemon::state::Daemon;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use marsa_core::caddy::CaddyInput;
use marsa_core::caddy::CaddyPaths;
use marsa_core::dns::NETWORKD_UNITS;
use marsa_core::helper::HelperOp;
use marsa_core::helper::HelperRequest;
use marsa_core::helper::HelperResponse;
use marsa_core::helper::OpResult;
use marsa_core::platform::daemon_file::{Token, daemon_authority};
use marsa_core::recipes::Platform;
use marsa_core::types::SetupReport;
use marsa_core::types::{ErrorBody, OpStatus};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;

/// The daemon port in the `Host` header. No socket is bound for it.
pub(super) const PORT: u16 = 51237;

/// Real `netstat -anv -p tcp` output with Herd's nginx on 80 and 443.
pub(super) const NETSTAT_HERD: &str =
    include_str!("../../../../../fixtures/ports/netstat-macos.txt");

/// Real `ps` output for an nginx worker: its title only.
pub(super) const PS_WORKER: &str =
    include_str!("../../../../../fixtures/ports/ps-nginx-worker.txt");

pub(super) const DONE: &str = include_str!("../../../../../fixtures/helper/responses/done.json");

pub(super) const FAILED: &str =
    include_str!("../../../../../fixtures/helper/responses/failed.json");

pub(super) const REJECTED: &str =
    include_str!("../../../../../fixtures/helper/responses/rejected.json");

pub(super) const TEST_CA: &str = include_str!("../../../../../fixtures/helper/test-ca.pem");

/// Another root with the subject of Marsa's roots, `CN=Marsa Local CA`.
pub(super) const TEST_CA_2: &str = include_str!("../../../../../fixtures/helper/test-ca-2.pem");

/// What `security verify-cert` prints for a root nobody trusts.
pub(super) const NOT_TRUSTED: &str = "Cert Verify Result: CSSMERR_TP_NOT_TRUSTED\n";

/// One call the fake runner got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::daemon) struct Call {
    pub(in crate::daemon) program: &'static str,
    pub(in crate::daemon) args: Vec<String>,
    pub(in crate::daemon) limit: Duration,
}

pub(in crate::daemon) type Answer =
    Box<dyn Fn(&'static str, &[String]) -> io::Result<RunOutput> + Send + Sync>;

/// Records every call and answers by program and arguments. Runs
/// nothing.
pub(in crate::daemon) struct Fake {
    pub(in crate::daemon) calls: Mutex<Vec<Call>>,
    pub(super) answer: Answer,
}

impl Fake {
    /// Answers by program alone.
    pub(super) fn new(
        answer: impl Fn(&'static str) -> io::Result<RunOutput> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Self::answering(move |program, _| answer(program))
    }

    /// Answers by program and arguments.
    pub(in crate::daemon) fn answering(
        answer: impl Fn(&'static str, &[String]) -> io::Result<RunOutput> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::default(),
            answer: Box::new(answer),
        })
    }

    /// `netstat` prints `netstat`, `ps` prints the nginx worker,
    /// `osascript` gives `osascript()`, and `security` is a Mac that does
    /// not trust the CA yet: `verify-cert` exits 1, the add exits 0.
    pub(super) fn scripted(
        netstat: &'static str,
        osascript: impl Fn() -> io::Result<RunOutput> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Self::answering(move |program, args| match program {
            NETSTAT => ok(netstat),
            PS => ok(PS_WORKER),
            OSASCRIPT => osascript(),
            SECURITY => security_untrusted(args),
            other => panic!("unexpected program {other}"),
        })
    }

    pub(super) fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    /// The `security` calls only.
    pub(super) fn security_calls(&self) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|call| call.program == SECURITY)
            .collect()
    }

    pub(in crate::daemon) fn programs(&self) -> Vec<&'static str> {
        self.calls().iter().map(|call| call.program).collect()
    }

    /// How many times `pkexec` ran.
    pub(super) fn pkexec_calls(&self) -> usize {
        self.programs()
            .iter()
            .filter(|program| **program == pkexec::PKEXEC)
            .count()
    }

    /// The request the helper would have got, from the last `pkexec`.
    pub(super) fn pkexec_request(&self) -> HelperRequest {
        let calls = self.calls();
        let call = calls.iter().rfind(|call| call.program == pkexec::PKEXEC);
        let request = &call.expect("a pkexec call").args[3];
        serde_json::from_str(request).unwrap()
    }

    /// The request the helper would have got, from the last `osascript`.
    pub(super) fn request(&self) -> HelperRequest {
        let calls = self.calls();
        let call = calls.iter().rfind(|call| call.program == OSASCRIPT);
        let request = &call.expect("an osascript call").args[7];
        serde_json::from_str(request).unwrap()
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
        self.calls.lock().unwrap().push(Call {
            program,
            args: args.clone(),
            limit,
        });
        (self.answer)(program, &args)
    }
}

/// `security` on a Mac that does not trust the CA yet: `verify-cert`
/// exits 1, and every other command exits 0.
pub(super) fn security_untrusted(args: &[String]) -> io::Result<RunOutput> {
    if args.first().map(String::as_str) == Some("verify-cert") {
        exit(1, NOT_TRUSTED)
    } else {
        ok("")
    }
}

pub(in crate::daemon) fn ok(stdout: &str) -> io::Result<RunOutput> {
    Ok(RunOutput {
        code: Some(0),
        stdout: stdout.as_bytes().to_vec(),
        stderr: Vec::new(),
    })
}

/// A program that exited with `code` and printed `stderr`.
pub(super) fn exit(code: i32, stderr: &str) -> io::Result<RunOutput> {
    Ok(RunOutput {
        code: Some(code),
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    })
}

/// What `osascript` gives when the script failed: exit 1 and `stderr`.
pub(in crate::daemon) fn script_error(stderr: &str) -> io::Result<RunOutput> {
    exit(1, stderr)
}

/// The helper's answer as one line, as it prints it.
pub(super) fn line(json: &str) -> String {
    serde_json::from_str::<Value>(json).unwrap().to_string()
}

/// The real netstat output without Herd's nginx: nobody on 80 or 443.
pub(super) fn netstat_free() -> &'static str {
    let text: Vec<&str> = NETSTAT_HERD
        .lines()
        .filter(|line| !line.contains("nginx-arm64"))
        .collect();
    text.join("\n").leak()
}

/// One done result per op, as the helper prints it.
pub(super) fn all_done(ops: &[&str]) -> String {
    let results: Vec<Value> = ops
        .iter()
        .map(|op| json!({ "op": op, "status": "done" }))
        .collect();
    json!({ "version": 1, "results": results }).to_string()
}

pub(super) fn results(json: &str) -> Vec<OpResult> {
    serde_json::from_str::<HelperResponse>(json)
        .unwrap()
        .results
}

/// A data folder, with Caddy's CA root if `ca`, and a `marsa-helper`
/// file in `bin/`. The user's home folder is `home/` in it: a path only,
/// never made. Caddy is not installed, unless [`Dir::with_caddy`] says
/// otherwise.
pub(super) struct Dir {
    pub(super) dir: TempDir,
    pub(super) caddy: Arc<FakeCaddy>,
    /// How long setup waits for Caddy and its CA root.
    pub(super) ca_wait: Duration,
}

impl Dir {
    pub(super) fn new(ca: bool) -> Self {
        let dir = TempDir::new().unwrap();
        if ca {
            let root = CaddyPaths::in_dir(dir.path()).ca_root();
            fs::create_dir_all(root.parent().unwrap()).unwrap();
            fs::write(&root, TEST_CA).unwrap();
        }
        let helper = dir.path().join("bin").join("marsa-helper");
        fs::create_dir_all(helper.parent().unwrap()).unwrap();
        fs::write(&helper, "").unwrap();
        // As cargo and an install leave them, whatever the umask.
        #[cfg(unix)]
        for path in [&helper, &dir.path().join("bin")] {
            set_mode(path, 0o755);
        }
        Self {
            dir,
            caddy: Arc::new(FakeCaddy::new(CaddyStart::NotInstalled, None)),
            ca_wait: CA_WAIT,
        }
    }

    /// A Caddy that answers `answer`, and that makes the CA root
    /// `ca_after` the start, if given.
    pub(super) fn with_caddy(self, answer: CaddyStart, ca_after: Option<Duration>) -> Self {
        let ca = ca_after.map(|delay| (self.ca_root(), delay));
        self.with_fake(FakeCaddy::new(answer, ca))
    }

    pub(super) fn with_fake(mut self, caddy: FakeCaddy) -> Self {
        self.caddy = Arc::new(caddy);
        self
    }

    pub(super) fn helper(&self) -> PathBuf {
        self.dir.path().join("bin").join("marsa-helper")
    }

    pub(super) fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    pub(super) fn ca_root(&self) -> PathBuf {
        CaddyPaths::in_dir(self.dir.path()).ca_root()
    }

    pub(super) fn setup(&self, fake: &Arc<Fake>) -> Setup {
        let mut setup = Setup::with(
            fake.clone(),
            Some(Platform::DarwinArm64),
            Some(self.helper()),
            Some(self.home()),
        )
        .with_caddy(self.caddy.clone());
        setup.ca_wait = self.ca_wait;
        setup
    }

    /// [`Setup::run`] with the default Caddy ports.
    pub(super) fn run(&self, fake: &Arc<Fake>) -> SetupReport {
        let caddy = CaddyInput::default();
        let stop = CancellationToken::new();
        let platform = Platform::DarwinArm64;
        self.setup(fake).run(
            self.dir.path(),
            caddy,
            platform,
            self.caddy.as_ref(),
            Some(&[]),
            None,
            &stop,
        )
    }
}

/// A Caddy for the CA step. It records the limit of each start, takes
/// `takes`, and answers `answer`; with `ca`, the start also makes the CA
/// root after that delay, as Caddy does once it has loaded its config:
/// on its own thread, or at once for no delay. Through a rename, as
/// Caddy's file storage writes, so setup never reads half a file, unless
/// `half` first writes half of a root there in place.
pub(super) struct FakeCaddy {
    pub(super) limits: Mutex<Vec<Duration>>,
    pub(super) answer: CaddyStart,
    pub(super) takes: Duration,
    pub(super) half: Option<PathBuf>,
    pub(super) ca: Option<(PathBuf, Duration)>,
}

impl FakeCaddy {
    pub(super) fn new(answer: CaddyStart, ca: Option<(PathBuf, Duration)>) -> Self {
        Self {
            limits: Mutex::default(),
            answer,
            takes: Duration::ZERO,
            half: None,
            ca,
        }
    }

    /// The limit of each start, in order.
    pub(super) fn limits(&self) -> Vec<Duration> {
        self.limits.lock().unwrap().clone()
    }
}

impl StartCaddy for FakeCaddy {
    fn start(&self, limit: Duration, _stop: &CancellationToken) -> CaddyStart {
        self.limits.lock().unwrap().push(limit);
        thread::sleep(self.takes);
        if let Some(root) = &self.half {
            fs::create_dir_all(root.parent().unwrap()).unwrap();
            fs::write(root, &TEST_CA[..TEST_CA.len() / 2]).unwrap();
        }
        if let Some((root, delay)) = self.ca.clone() {
            let make = move || {
                thread::sleep(delay);
                let temp = root.with_extension("tmp");
                fs::create_dir_all(root.parent().unwrap()).unwrap();
                fs::write(&temp, TEST_CA).unwrap();
                fs::rename(&temp, &root).unwrap();
            };
            if delay.is_zero() {
                make();
            } else {
                // Setup waits for the file, so the thread is done before
                // the test ends.
                thread::spawn(make);
            }
        }
        self.answer.clone()
    }
}

#[cfg(unix)]
pub(in crate::daemon) fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

pub(super) fn trust() -> HelperOp {
    HelperOp::TrustInstall {
        pem: TEST_CA.to_owned(),
    }
}

pub(super) fn resolver() -> HelperOp {
    HelperOp::ResolverInstall { port: 18053 }
}

pub(super) fn pf(http_port: u16, https_port: u16) -> HelperOp {
    HelperOp::PfInstall {
        http_port,
        https_port,
    }
}

pub(super) fn error_code(report: &SetupReport) -> Option<&str> {
    report.error.as_ref().map(|error| error.code.as_str())
}

pub(super) fn done(op: &str) -> OpResult {
    OpResult {
        op: op.into(),
        status: OpStatus::Done,
        code: None,
        message: None,
    }
}

/// A helper file at `path`, with the modes of an install, and its
/// folder.
pub(in crate::daemon) fn make_helper(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "").unwrap();
    #[cfg(unix)]
    for path in [path, path.parent().unwrap()] {
        set_mode(path, 0o755);
    }
}

/// The route with `setup` behind the real auth layer, on `daemon`.
pub(super) struct App {
    pub(super) router: Router,
    pub(super) bearer: String,
}

impl App {
    pub(super) fn new(daemon: Arc<Daemon>, setup: Setup) -> Self {
        let token = Token::generate().unwrap();
        let bearer = token.bearer_header();
        let auth = AuthConfig::new(Arc::new(token), PORT);
        let router = routes_with(setup)
            .with_state(daemon)
            .layer(from_fn_with_state(auth, require_auth));
        Self { router, bearer }
    }

    pub(super) async fn post(&self) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/setup")
            .header("host", daemon_authority(PORT))
            .header("authorization", &self.bearer)
            .body(Body::empty())
            .unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    pub(super) async fn refused(&self) -> (StatusCode, String) {
        let (status, json) = self.post().await;
        let error: ErrorBody = serde_json::from_value(json).unwrap();
        (status, error.error.code)
    }

    /// `POST /v1/setup` with only `headers`, expecting an error body.
    pub(super) async fn send(&self, headers: &[(&str, &str)]) -> (StatusCode, String) {
        let mut request = Request::builder().method("POST").uri("/v1/setup");
        for &(name, value) in headers {
            request = request.header(name, value);
        }
        let request = request.body(Body::empty()).unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let error: ErrorBody = serde_json::from_slice(&bytes).unwrap();
        (status, error.error.code)
    }
}

/// What `systemctl` says on a test system.
#[derive(Debug, Clone, Copy)]
pub(super) struct Systemd {
    /// The first line of `systemctl --version`.
    pub(super) version: &'static str,
    /// The exit code of `is-active --quiet systemd-resolved.service`.
    pub(super) resolved: i32,
    /// What `is-active` and `is-enabled` print for networkd's units.
    pub(super) networkd: (&'static str, &'static str),
}

/// Ubuntu 24.04: the `marsa0` link, networkd or not.
pub(super) const UBUNTU_2404: Systemd = Systemd {
    version: "systemd 255 (255.4-1ubuntu8)",
    resolved: 0,
    networkd: (
        "active\ninactive\ninactive\n",
        "enabled\nenabled\nenabled\n",
    ),
};

/// Ubuntu 22.04 with systemd-networkd, as on servers and CI runners.
pub(super) const UBUNTU_2204_SERVER: Systemd = Systemd {
    version: "systemd 249 (249.11-0ubuntu3.12)",
    resolved: 0,
    networkd: ("active\nactive\ninactive\n", "enabled\nenabled\ndisabled\n"),
};

/// Ubuntu 22.04 desktop: NetworkManager, networkd off for good.
pub(super) const UBUNTU_2204_DESKTOP: Systemd = Systemd {
    version: "systemd 249 (249.11-0ubuntu3.12)",
    resolved: 0,
    networkd: (
        "inactive\ninactive\ninactive\n",
        "disabled\ndisabled\nmasked\n",
    ),
};

/// Debian 12 as installed: no systemd-resolved.
pub(super) const DEBIAN_12: Systemd = Systemd {
    version: "systemd 252 (252.30-1~deb12u2)",
    resolved: 3,
    networkd: (
        "inactive\ninactive\ninactive\n",
        "disabled\ndisabled\ndisabled\n",
    ),
};

/// `systemctl` on `system`, for the commands of `linux_dns` only.
pub(super) fn systemctl(system: Systemd, args: &[String]) -> io::Result<RunOutput> {
    let printed = |code, stdout: &str| {
        Ok(RunOutput {
            code: Some(code),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["--version"] => printed(0, &format!("{}\n+PAM +AUDIT -SELINUX\n", system.version)),
        ["is-active", "--quiet", "systemd-resolved.service"] => printed(system.resolved, ""),
        ["is-active", units @ ..] if units == NETWORKD_UNITS => printed(3, system.networkd.0),
        ["is-enabled", units @ ..] if units == NETWORKD_UNITS => printed(1, system.networkd.1),
        other => panic!("systemctl {other:?}"),
    }
}

/// A Linux machine with the `marsa0` link ([`UBUNTU_2404`]) whose
/// `pkexec` gives `answer()`. Nothing else may run.
pub(super) fn linux_fake(
    answer: impl Fn() -> io::Result<RunOutput> + Send + Sync + 'static,
) -> Arc<Fake> {
    linux_fake_on(UBUNTU_2404, move |_| answer())
}

/// `system`, whose `pkexec` gives `answer(request)`.
pub(super) fn linux_fake_on(
    system: Systemd,
    answer: impl Fn(HelperRequest) -> io::Result<RunOutput> + Send + Sync + 'static,
) -> Arc<Fake> {
    Fake::answering(move |program, args| match program {
        pkexec::PKEXEC => answer(serde_json::from_str(&args[3]).unwrap()),
        linux_dns::SYSTEMCTL => systemctl(system, args),
        other => panic!("{other} ran on Linux"),
    })
}

/// The helper's answer: `done` for each op of `request`, except the
/// `(op, code)` pairs of `failed`.
pub(super) fn answer_for(
    request: &HelperRequest,
    failed: &[(&str, &str)],
) -> io::Result<RunOutput> {
    let results: Vec<Value> = request
        .ops
        .iter()
        .map(|op| {
            let op = op_name(op).unwrap();
            match failed.iter().find(|(name, _)| *name == op) {
                Some((_, code)) => json!({ "op": op, "status": "failed", "code": code,
                                           "message": "x" }),
                None => json!({ "op": op, "status": "done" }),
            }
        })
        .collect();
    let code = if failed.is_empty() { 0 } else { 1 };
    Ok(RunOutput {
        code: Some(code),
        stdout: json!({ "version": 1, "results": results })
            .to_string()
            .into_bytes(),
        stderr: Vec::new(),
    })
}
