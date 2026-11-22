//! The CA root: reading it, and starting Caddy to make it. Caddy makes the
//! root when it loads its config, sites or not. With no root yet, setup starts
//! Caddy the way `marsa services start caddy` does ([`StartCaddy`]) and waits
//! for the root up to [`CA_WAIT`]. No Caddy installed skips the trust as
//! `caddy_not_installed`, a Caddy that did not start as `caddy_not_started`,
//! and a root that did not come in time as `ca_not_ready`. A file the helper
//! or `security` would refuse is skipped as `ca_unreadable`, so the other
//! steps still go.

use super::Setup;
use super::user_trust;
use crate::daemon::services::caddy::ActionError;
use crate::daemon::state::Daemon;
use marsa_core::codes;
use marsa_core::types::ServiceAction;
use marsa_core::types::ServiceState;
use std::fmt;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

/// The helper refuses a larger certificate (ADR 0011).
const MAX_CA_BYTES: u64 = 16 * 1024;

/// How long setup waits for Caddy to start and make its CA root, when there
/// is no root yet. Caddy's own start waits up to 5 s for its
/// admin API, and up to about 10 s more to stop an old Caddy of a crashed
/// daemon first (ADR 0009); making the root then takes well under a second.
pub const CA_WAIT: Duration = Duration::from_secs(30);

/// How often setup looks for the CA root meanwhile.
const CA_POLL: Duration = Duration::from_millis(100);

/// The first line the helper's `trust_install` accepts (`ops/trust.rs`).
pub(super) const PEM_BEGIN: &str = "-----BEGIN CERTIFICATE-----";

// `#[cfg]` outside `platform`: std has no `O_NONBLOCK`, its value differs by
// system, and `libc` is not an approved dependency (AGENTS.md section 8).
// These are the values of `<sys/fcntl.h>` on macOS and of
// `<asm-generic/fcntl.h>` on Linux x86-64 and arm64. Another Unix opens
// without it; Marsa has no build there.
#[cfg(target_os = "macos")]
const O_NONBLOCK: i32 = 0x0004;

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
const O_NONBLOCK: i32 = 0o4000;

#[cfg(all(
    unix,
    not(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    ))
))]
const O_NONBLOCK: i32 = 0;

/// Starts Caddy for setup when its CA root does not exist yet: Caddy makes
/// the CA when it loads its config. The daemon uses its own
/// Caddy (`DaemonCaddy`); tests use a fake, as for `Runner`.
pub trait StartCaddy: Send + Sync {
    /// Makes Caddy run if it is installed, and waits at most `limit` for
    /// that, or until `stop`. Blocking: setup runs on a blocking thread.
    fn start(&self, limit: Duration, stop: &CancellationToken) -> CaddyStart;
}

/// What [`StartCaddy::start`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaddyStart {
    /// Caddy runs, or is still starting: its CA root may come at any moment.
    Started,
    /// No Caddy is installed, so nothing can make the CA.
    NotInstalled,
    /// Caddy did not start. Why, for the log: the code of its status or of
    /// the start.
    Failed(String),
}

/// [`StartCaddy`] through the daemon's own `CaddyManager`, the path of
/// `marsa services start caddy`: the orphan check, the newest installed
/// Caddy under the supervisor, and the wait for its admin API. No shell.
pub(super) struct DaemonCaddy {
    pub(super) daemon: Arc<Daemon>,
    /// The daemon's runtime: the start is async, and setup runs on a
    /// blocking thread of this runtime.
    pub(super) runtime: Handle,
}

impl StartCaddy for DaemonCaddy {
    fn start(&self, limit: Duration, stop: &CancellationToken) -> CaddyStart {
        let caddy = &self.daemon.caddy();
        // First, so a missing Caddy does not mark Caddy's status failed.
        if !caddy.installed() {
            return CaddyStart::NotInstalled;
        }
        let deadline = tokio::time::Instant::now() + limit;
        let start = async {
            loop {
                match caddy.run(&self.daemon, ServiceAction::Start).await {
                    // Another start or stop runs now. Ask again until it has
                    // ended, so a stop in the meantime does not leave Caddy
                    // stopped. A refused call returns at once, and the
                    // timeout below bounds the loop. A start waits up to
                    // Caddy's ready limit for its admin API, so none begins
                    // with less left, where the timeout would cut that wait
                    // short. Only that wait is covered: an
                    // orphan to stop first can still make a start longer.
                    Err(ActionError::Busy) => {
                        let next = tokio::time::Instant::now() + CA_POLL;
                        if deadline.saturating_duration_since(next) < caddy.ready_limit {
                            return Err(ActionError::Busy);
                        }
                        tokio::time::sleep(CA_POLL).await;
                    }
                    other => return other,
                }
            }
        };
        let started = self.runtime.block_on(async {
            tokio::select! {
                () = stop.cancelled() => None,
                result = tokio::time::timeout(limit, start) => Some(result),
            }
        });
        match started {
            None => CaddyStart::Failed("the daemon is stopping".to_owned()),
            // Still starting. The wait for the root has the same limit.
            Some(Err(_)) => CaddyStart::Started,
            // Another action still runs with too little time left to start
            // after it. It may be the start that makes the root, so the
            // wait for it has the rest of the limit. If it was a stop or a
            // failed start, the skip says `ca_not_ready`, not
            // `caddy_not_started`.
            Some(Ok(Err(ActionError::Busy))) => CaddyStart::Started,
            Some(Ok(Ok(status))) => match status.state {
                ServiceState::Running | ServiceState::Starting => CaddyStart::Started,
                ServiceState::Failed { code } => CaddyStart::Failed(code),
                ServiceState::Stopped | ServiceState::Stopping => {
                    CaddyStart::Failed("Caddy stopped".to_owned())
                }
            },
            Some(Ok(Err(err))) if err.code() == codes::CADDY_NOT_INSTALLED => {
                CaddyStart::NotInstalled
            }
            Some(Ok(Err(err))) => CaddyStart::Failed(err.code().to_owned()),
        }
    }
}

/// Looks for the CA root at `path` every [`CA_POLL`] until it can be used,
/// `deadline`, or `stop`, and gives what [`read_ca`] gives then. A root that
/// is there but cannot be used yet, or has no END line yet, is waited for
/// too: the root was missing or partial at the first look, so it may be
/// half written.
fn wait_for_ca(
    path: &Path,
    deadline: Instant,
    stop: &CancellationToken,
) -> Result<String, &'static str> {
    loop {
        let problem = match load_ca(path) {
            Ok(pem) => return Ok(pem),
            Err(problem) => problem,
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || stop.is_cancelled() {
            if problem == CaProblem::Missing {
                tracing::warn!("Caddy did not make its CA root in time; marsa logs caddy says why");
            }
            return Err(ca_code(path, problem));
        }
        thread::sleep(CA_POLL.min(left));
    }
}

/// Why the CA root cannot be used, before it becomes a code.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CaProblem {
    /// No file yet.
    Missing,
    /// A file that may be half written: empty, a part of the BEGIN line,
    /// or a start that passes the checks with no END line yet.
    Partial,
    /// A file that cannot be used, and why, for the log.
    Unusable(String),
}

/// The code of `problem`, for the report: `ca_not_ready` or
/// `ca_unreadable`. An unusable file goes to the log with its reason.
fn ca_code(path: &Path, problem: CaProblem) -> &'static str {
    match problem {
        CaProblem::Missing => codes::CA_NOT_READY,
        CaProblem::Partial => ca_code(
            path,
            CaProblem::Unusable(format!("no {} line", user_trust::PEM_END)),
        ),
        CaProblem::Unusable(reason) => {
            tracing::warn!(path = %path.display(), %reason, "cannot use the CA root certificate");
            codes::CA_UNREADABLE
        }
    }
}

/// The text of Caddy's root certificate at `path`, for `trust_install`.
///
/// # Errors
///
/// `ca_not_ready` if there is no file yet, and `ca_unreadable` if it is not
/// a regular file, is larger than 16 KiB, cannot be read, would fail the
/// helper's own checks ([`pem_problem`]), or has no END line.
pub(super) fn read_ca(path: &Path) -> Result<String, &'static str> {
    load_ca(path).map_err(|problem| ca_code(path, problem))
}

/// [`read_ca`] without the log, so a wait can look many times.
fn load_ca(path: &Path) -> Result<String, CaProblem> {
    let unusable = |reason: &dyn fmt::Display| CaProblem::Unusable(reason.to_string());
    let file = match open_nonblocking(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(CaProblem::Missing),
        Err(err) => return Err(unusable(&err)),
    };
    // The metadata of the open file, not of the path: a file swapped after
    // a check by path would slip through.
    let meta = file.metadata().map_err(|err| unusable(&err))?;
    if !meta.is_file() || meta.len() > MAX_CA_BYTES {
        return Err(unusable(&"not a regular file of at most 16 KiB"));
    }
    let mut pem = String::new();
    file.take(MAX_CA_BYTES + 1)
        .read_to_string(&mut pem)
        .map_err(|err| unusable(&err))?;
    if pem.len() as u64 > MAX_CA_BYTES {
        return Err(unusable(&"larger than 16 KiB"));
    }
    let start = pem.trim_start_matches([' ', '\t', '\r', '\n']);
    match pem_problem(&pem) {
        Some(_) if PEM_BEGIN.starts_with(start) => Err(CaProblem::Partial),
        Some(problem) => Err(unusable(&problem)),
        None if !pem.contains(user_trust::PEM_END) => Err(CaProblem::Partial),
        None => Ok(pem),
    }
}

/// Opens `path` to read. On Unix with `O_NONBLOCK`: a FIFO put in place of
/// the file then opens at once, and fails the regular file check, instead of
/// waiting for a writer and holding up setup. A regular file reads as usual.
fn open_nonblocking(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, O_NONBLOCK);
    options.open(path)
}

/// Why the helper would refuse `pem` in `trust_install` (`request.rs` and
/// `ops/trust.rs` of `marsa-helper`), or `None`. Checked here too, so the
/// password is never asked for a request the helper rejects as a whole.
fn pem_problem(pem: &str) -> Option<&'static str> {
    let start = pem.trim_start_matches([' ', '\t', '\r', '\n']);
    if pem.is_empty() {
        Some("is empty")
    } else if !pem.is_ascii() {
        Some("is not ASCII")
    } else if pem.contains('\0') {
        Some("contains a NUL byte")
    } else if !start.starts_with(PEM_BEGIN) {
        Some("does not start with -----BEGIN CERTIFICATE-----")
    } else {
        None
    }
}

impl Setup {
    /// The text of Caddy's root certificate at `path`, as [`read_ca`] gives
    /// it. A whole root that is there is read at once. With no root yet, or
    /// one that may be half written (empty, or no END line yet),
    /// `starter` starts Caddy if it does not run, and setup waits for the
    /// root up to `ca_wait`: Caddy makes it when it loads its config.
    ///
    /// # Errors
    ///
    /// The codes of [`read_ca`]: `ca_not_ready` when the root did not come
    /// in time. `caddy_not_installed` when there is no root and no Caddy to
    /// make it, and `caddy_not_started` when Caddy did not start and made
    /// none.
    pub(super) fn ca(
        &self,
        path: &Path,
        starter: &dyn StartCaddy,
        stop: &CancellationToken,
    ) -> Result<String, &'static str> {
        let partial = match load_ca(path) {
            Ok(pem) => return Ok(pem),
            Err(CaProblem::Missing) => {
                tracing::info!("no CA root yet; starting Caddy so that it makes one");
                false
            }
            Err(CaProblem::Partial) => {
                tracing::info!("the CA root has no END line yet; waiting for Caddy to finish it");
                true
            }
            Err(problem) => return Err(ca_code(path, problem)),
        };
        let deadline = Instant::now() + self.ca_wait;
        match starter.start(self.ca_wait, stop) {
            CaddyStart::Started => wait_for_ca(path, deadline, stop),
            // No Caddy will finish it: the root that is there is unusable.
            CaddyStart::NotInstalled if partial => Err(ca_code(path, CaProblem::Partial)),
            CaddyStart::NotInstalled => {
                tracing::warn!("no CA root, and no Caddy to make one: install Caddy first");
                Err(codes::CADDY_NOT_INSTALLED)
            }
            CaddyStart::Failed(reason) => {
                tracing::warn!(%reason, "Caddy did not start; marsa logs caddy says why");
                // Caddy makes the CA before it opens its ports, so a start
                // that failed on a port may still have made it.
                match load_ca(path) {
                    Ok(pem) => Ok(pem),
                    Err(CaProblem::Missing) => Err(codes::CADDY_NOT_STARTED),
                    Err(problem) => Err(ca_code(path, problem)),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::osascript::OSASCRIPT;
    use marsa_core::platform::ports::NETSTAT;

    use super::super::test_support::*;
    use super::super::user_trust::SECURITY;
    use super::super::*;
    use super::*;

    #[cfg(unix)]
    use crate::daemon::services::caddy::ActionError;
    use crate::daemon::state::Daemon;

    use axum::http::StatusCode;

    #[cfg(unix)]
    use marsa_core::caddy::CaddyInput;
    use marsa_core::caddy::CaddyPaths;

    use marsa_core::recipes::Platform;
    #[cfg(unix)]
    use marsa_core::types::ServiceAction;
    use marsa_core::types::ServiceState;
    use marsa_core::types::SetupReport;

    use std::fs;

    #[cfg(unix)]
    use std::path::PathBuf;
    #[cfg(unix)]
    use std::process::Command;
    use std::sync::Arc;

    #[cfg(unix)]
    use std::sync::mpsc;
    #[cfg(unix)]
    use std::thread;
    use std::time::Duration;
    use std::time::Instant;

    #[cfg(unix)]
    use tokio::runtime::Handle;
    #[cfg(unix)]
    use tokio_util::sync::CancellationToken;

    /// A new user runs setup before any site. Setup starts
    /// Caddy, which makes its CA root a moment later, waits for it, and
    /// trusts it.
    #[test]
    fn no_ca_yet_starts_caddy_waits_for_the_root_and_trusts_it() {
        let after = Duration::from_millis(300);
        let dir = Dir::new(false).with_caddy(CaddyStart::Started, Some(after));
        let answer = all_done(&[OpName::ResolverInstall, OpName::PfInstall]);
        let fake = Fake::scripted(netstat_free(), move || ok(&answer));
        let began = Instant::now();
        let report = dir.run(&fake);

        assert!(began.elapsed() >= after, "{:?}", began.elapsed());
        assert_eq!(dir.caddy.limits(), [CA_WAIT]);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        let want = [
            done(OpName::ResolverInstall),
            done(OpName::PfInstall),
            done(OpName::TrustInstall),
        ];
        assert_eq!(report.results, want);
        assert_eq!(report.error, None);
        // Caddy first, then the checks and the prompt.
        assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT, SECURITY, SECURITY]);
        assert_eq!(fake.request().ops, [resolver(), pf(18080, 18443)]);
    }

    /// A root that is already there is read at once: no start, no wait.
    #[test]
    fn a_ca_already_there_starts_no_caddy() {
        let dir = Dir::new(true).with_caddy(CaddyStart::Started, None);
        let answer = all_done(&[OpName::ResolverInstall, OpName::PfInstall]);
        let fake = Fake::scripted(netstat_free(), move || ok(&answer));
        let report = dir.run(&fake);

        assert!(dir.caddy.limits().is_empty());
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert_eq!(report.results.len(), 3);
        assert_eq!(report.results[2], done(OpName::TrustInstall));
    }

    /// Caddy started, but no root came in time: `ca_not_ready`, after the
    /// limit and not much later. The other steps still go.
    #[test]
    fn a_root_that_does_not_come_in_time_is_ca_not_ready() {
        let mut dir = Dir::new(false).with_caddy(CaddyStart::Started, None);
        dir.ca_wait = Duration::from_millis(300);
        let fake = Fake::scripted(netstat_free(), || ok(DONE));
        let began = Instant::now();
        let report = dir.run(&fake);

        let took = began.elapsed();
        assert!(
            took >= dir.ca_wait && took < Duration::from_secs(5),
            "{took:?}"
        );
        assert_eq!(dir.caddy.limits(), [dir.ca_wait]);
        assert_eq!(report.skipped, [skip(OpName::TrustInstall, "ca_not_ready")]);
        assert_eq!(fake.request().ops, [resolver(), pf(18080, 18443)]);
        assert_eq!(report.results, results(DONE));
        assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT]);
        assert_eq!(report.error, None);
    }

    /// No Caddy installed: nothing can make the CA, so no wait, and a skip
    /// that says to install Caddy. The other steps still go.
    #[test]
    fn no_caddy_installed_skips_the_trust_as_caddy_not_installed() {
        let dir = Dir::new(false);
        let fake = Fake::scripted(netstat_free(), || ok(DONE));
        let began = Instant::now();
        let report = dir.run(&fake);

        assert!(began.elapsed() < Duration::from_secs(5));
        assert_eq!(dir.caddy.limits(), [CA_WAIT]);
        let skipped = [skip(OpName::TrustInstall, "caddy_not_installed")];
        assert_eq!(report.skipped, skipped);
        assert_eq!(fake.request().ops, [resolver(), pf(18080, 18443)]);
        assert_eq!(report.results, results(DONE));
        assert!(report.port_holders.is_empty());
        assert_eq!(report.error, None);
    }

    /// Caddy did not start: one more look and no wait. Caddy makes its CA
    /// before it opens its ports, so a root made by a start that failed on
    /// a port is still trusted.
    #[test]
    fn a_caddy_that_did_not_start_is_not_waited_for() {
        let failed = CaddyStart::Failed("port_in_use".into());
        let dir = Dir::new(false).with_caddy(failed.clone(), None);
        let fake = Fake::scripted(netstat_free(), || ok(DONE));
        let began = Instant::now();
        let report = dir.run(&fake);
        assert!(began.elapsed() < Duration::from_secs(5));
        assert_eq!(
            report.skipped,
            [skip(OpName::TrustInstall, "caddy_not_started")]
        );

        let dir = Dir::new(false).with_caddy(failed, Some(Duration::ZERO));
        let answer = all_done(&[OpName::ResolverInstall, OpName::PfInstall]);
        let fake = Fake::scripted(netstat_free(), move || ok(&answer));
        let report = dir.run(&fake);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert_eq!(report.results[2], done(OpName::TrustInstall));
    }

    /// A root that is there but half written is waited for,
    /// and the whole certificate is the one trusted.
    #[test]
    fn a_half_written_root_is_waited_for() {
        let dir = Dir::new(false);
        let ca = Some((dir.ca_root(), Duration::from_millis(300)));
        let fake_caddy = FakeCaddy {
            half: Some(dir.ca_root()),
            ..FakeCaddy::new(CaddyStart::Started, ca)
        };
        let dir = dir.with_fake(fake_caddy);
        let answer = all_done(&[OpName::ResolverInstall, OpName::PfInstall]);
        let fake = Fake::scripted(netstat_free(), move || ok(&answer));
        let report = dir.run(&fake);

        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert_eq!(report.results[2], done(OpName::TrustInstall));
        let record = user_trust::TrustFiles::in_dir(dir.dir.path()).record();
        assert_eq!(fs::read_to_string(record).unwrap(), TEST_CA);

        // Still half at the limit: `ca_unreadable`, after the limit.
        let mut dir = Dir::new(false);
        dir.ca_wait = Duration::from_millis(300);
        let fake_caddy = FakeCaddy {
            half: Some(dir.ca_root()),
            ..FakeCaddy::new(CaddyStart::Started, None)
        };
        let dir = dir.with_fake(fake_caddy);
        let fake = Fake::scripted(netstat_free(), || ok(DONE));
        let began = Instant::now();
        let report = dir.run(&fake);
        assert!(began.elapsed() >= dir.ca_wait, "{:?}", began.elapsed());
        assert_eq!(
            report.skipped,
            [skip(OpName::TrustInstall, "ca_unreadable")]
        );
    }

    /// A root with no END line at the first look may be half
    /// written too, so it is waited for, never trusted as it is. So is an
    /// empty file or a part of the BEGIN line.
    #[test]
    fn a_root_with_no_end_line_at_the_first_look_is_waited_for() {
        for part in [&TEST_CA[..TEST_CA.len() / 2], "", "-----BEG"] {
            let dir = Dir::new(false);
            let root = dir.ca_root();
            fs::create_dir_all(root.parent().unwrap()).unwrap();
            fs::write(&root, part).unwrap();
            let ca = Some((root, Duration::from_millis(300)));
            let dir = dir.with_fake(FakeCaddy::new(CaddyStart::Started, ca));
            let answer = all_done(&[OpName::ResolverInstall, OpName::PfInstall]);
            let fake = Fake::scripted(netstat_free(), move || ok(&answer));
            let report = dir.run(&fake);

            assert_eq!(dir.caddy.limits(), [CA_WAIT], "{part:?}");
            assert!(report.skipped.is_empty(), "{part:?} {:?}", report.skipped);
            assert_eq!(report.results[2], done(OpName::TrustInstall), "{part:?}");
            let record = user_trust::TrustFiles::in_dir(dir.dir.path()).record();
            assert_eq!(fs::read_to_string(record).unwrap(), TEST_CA, "{part:?}");
        }
    }

    /// Caddy did not start, and half a root is there. The one
    /// more look does not trust it, and does not wait for the rest.
    #[test]
    fn a_half_root_after_a_failed_start_is_ca_unreadable() {
        let dir = Dir::new(false);
        let fake_caddy = FakeCaddy {
            half: Some(dir.ca_root()),
            ..FakeCaddy::new(CaddyStart::Failed("port_in_use".into()), None)
        };
        let dir = dir.with_fake(fake_caddy);
        let fake = Fake::scripted(netstat_free(), || ok(DONE));
        let began = Instant::now();
        let report = dir.run(&fake);

        assert!(began.elapsed() < Duration::from_secs(5));
        assert_eq!(
            report.skipped,
            [skip(OpName::TrustInstall, "ca_unreadable")]
        );
        assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT]);
    }

    /// Caddy's start and the wait for its root share one
    /// limit. A start that takes most of it leaves only the rest to wait.
    #[test]
    fn the_start_and_the_wait_share_one_limit() {
        let mut dir = Dir::new(false);
        dir.ca_wait = Duration::from_secs(1);
        let fake_caddy = FakeCaddy {
            takes: Duration::from_millis(900),
            ..FakeCaddy::new(CaddyStart::Started, None)
        };
        let dir = dir.with_fake(fake_caddy);
        let fake = Fake::scripted(netstat_free(), || ok(DONE));
        let began = Instant::now();
        let report = dir.run(&fake);

        let took = began.elapsed();
        assert!(took >= dir.ca_wait, "{took:?}");
        // Two limits one after the other would be 1.9 s.
        assert!(took < Duration::from_millis(1500), "{took:?}");
        assert_eq!(report.skipped, [skip(OpName::TrustInstall, "ca_not_ready")]);
    }

    #[test]
    fn a_ca_that_cannot_be_used_is_ca_unreadable() {
        let dir = Dir::new(false);
        let root = CaddyPaths::in_dir(dir.dir.path()).ca_root();
        fs::create_dir_all(&root).unwrap();
        assert_eq!(read_ca(&root), Err("ca_unreadable"));
        fs::remove_dir(&root).unwrap();
        fs::write(&root, "x".repeat(16 * 1024 + 1)).unwrap();
        assert_eq!(read_ca(&root), Err("ca_unreadable"));
        fs::write(&root, [0xff, 0xfe]).unwrap();
        assert_eq!(read_ca(&root), Err("ca_unreadable"));
        fs::write(&root, TEST_CA).unwrap();
        assert_eq!(read_ca(&root).as_deref(), Ok(TEST_CA));
    }

    #[test]
    fn a_pem_the_helper_would_refuse_is_ca_unreadable_and_the_rest_goes() {
        let refused = [
            ("empty", String::new()),
            ("not ASCII", format!("{TEST_CA}é")),
            ("a NUL byte", format!("{TEST_CA}\0")),
            (
                "another block",
                "-----BEGIN PRIVATE KEY-----\nAAAA\n".to_owned(),
            ),
            ("text first", format!("garbage\n{TEST_CA}")),
        ];
        for (name, pem) in refused {
            let dir = Dir::new(false);
            let root = CaddyPaths::in_dir(dir.dir.path()).ca_root();
            fs::create_dir_all(root.parent().unwrap()).unwrap();
            fs::write(&root, &pem).unwrap();
            assert_eq!(read_ca(&root), Err("ca_unreadable"), "{name}");

            // Only the trust step is skipped: DNS and pf still go.
            let answer = all_done(&[OpName::ResolverInstall, OpName::PfInstall]);
            let fake = Fake::scripted(netstat_free(), move || ok(&answer));
            let report = dir.run(&fake);
            assert_eq!(
                report.skipped,
                [skip(OpName::TrustInstall, "ca_unreadable")],
                "{name}"
            );
            assert_eq!(fake.request().ops, [resolver(), pf(18080, 18443)], "{name}");
            assert_eq!(report.error, None, "{name}");
            // A certificate that cannot be used never reaches `security`:
            // only the helper's two results come back.
            assert_eq!(fake.programs(), [NETSTAT, OSASCRIPT], "{name}");
            assert_eq!(report.results.len(), 2, "{name}");
        }
        // The helper allows whitespace before the BEGIN line.
        assert_eq!(pem_problem(&format!(" \t\r\n{TEST_CA}")), None);
        assert_eq!(pem_problem(TEST_CA), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_in_place_of_the_ca_does_not_hold_up_setup() {
        let dir = Dir::new(false);
        let root = CaddyPaths::in_dir(dir.dir.path()).ca_root();
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        // std cannot make a FIFO, so `mkfifo` does, in the test's own folder.
        let made = Command::new("/usr/bin/mkfifo").arg(&root).status().unwrap();
        assert!(made.success());
        // On a thread: a blocking `open` would wait for a writer forever.
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || sender.send(read_ca(&root)));
        let got = receiver.recv_timeout(Duration::from_secs(5));
        assert_eq!(
            got.expect("read_ca waited on the FIFO"),
            Err("ca_unreadable")
        );
    }

    /// The daemon's own Caddy, not a fake: with no Caddy installed, setup
    /// says so, and Caddy's status is not marked failed.
    #[tokio::test]
    async fn the_daemons_caddy_not_installed_is_a_skip_and_no_failed_status() {
        let dir = Dir::new(false);
        let fake = Fake::scripted(netstat_free(), || ok(DONE));
        let daemon = Arc::new(Daemon::new(dir.dir.path().to_path_buf()));
        let app = App::new(daemon.clone(), with_the_daemons_caddy(&dir, &fake));

        let (status, json) = app.post().await;
        assert_eq!(status, StatusCode::OK, "{json}");
        let report: SetupReport = serde_json::from_value(json).unwrap();
        let skipped = [skip(OpName::TrustInstall, "caddy_not_installed")];
        assert_eq!(report.skipped, skipped);
        assert_eq!(daemon.caddy().status().state, ServiceState::Stopped);
    }

    /// A daemon on `dir` whose Caddy is `/bin/sleep` on free ports: it
    /// starts, but its admin API never answers, so each start ends with
    /// `caddy_start_timeout` after `ready`.
    #[cfg(unix)]
    fn sleeping_caddy(dir: &Dir, ready: Duration) -> Arc<Daemon> {
        use crate::daemon::services::caddy::caddy_spec_for;
        use marsa_core::services::{ServiceSpec, StopMethod};

        let mut daemon = Daemon::new(dir.dir.path().to_path_buf());
        // Free ports, so nothing of this machine answers there.
        let free: Vec<std::net::TcpListener> = (0..3)
            .map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap())
            .collect();
        let port = |index: usize| free[index].local_addr().unwrap().port();
        let caddy = daemon.caddy_mut();
        caddy.input = CaddyInput {
            admin_port: port(0),
            http_port: port(1),
            https_port: port(2),
        };
        drop(free);
        caddy.ready_limit = ready;
        caddy.spec = Some(ServiceSpec {
            args: vec!["30".to_owned()],
            stop: StopMethod::term(Duration::from_millis(200)),
            ..caddy_spec_for(&caddy.paths, PathBuf::from("/bin/sleep"))
        });
        Arc::new(daemon)
    }

    /// Setup with the daemon's own Caddy, not a fake one.
    fn with_the_daemons_caddy(dir: &Dir, fake: &Arc<Fake>) -> Setup {
        Setup::with(
            fake.clone(),
            Some(Platform::DarwinArm64),
            Some(dir.helper()),
            Some(dir.home()),
        )
    }

    /// The daemon's own Caddy, started from setup's blocking thread through
    /// the path of `marsa services start caddy`. Its start fails, so setup
    /// does not wait for a root, and says that Caddy did not start.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn setup_starts_the_daemons_caddy_and_does_not_wait_when_it_fails() {
        let dir = Dir::new(false);
        let fake = Fake::scripted(netstat_free(), || ok(DONE));
        let daemon = sleeping_caddy(&dir, Duration::from_millis(300));
        let app = App::new(daemon.clone(), with_the_daemons_caddy(&dir, &fake));

        let began = Instant::now();
        let (status, json) = app.post().await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert!(began.elapsed() < CA_WAIT, "{:?}", began.elapsed());
        let report: SetupReport = serde_json::from_value(json).unwrap();
        let skipped = [skip(OpName::TrustInstall, "caddy_not_started")];
        assert_eq!(report.skipped, skipped);
        let failed = ServiceState::Failed {
            code: "caddy_start_timeout".into(),
        };
        assert_eq!(daemon.caddy().status().state, failed);
        daemon.stop_procs(Duration::from_secs(5)).await;
    }

    /// Another start of Caddy runs when setup asks for one.
    /// Setup asks again once it has ended, so its own start runs too, and
    /// the busy answer is not taken for a running Caddy while Caddy's ready
    /// limit still fits in the time left.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_busy_caddy_is_asked_again_once_the_other_action_ends() {
        let dir = Dir::new(false);
        let fake = Fake::scripted(netstat_free(), || ok(DONE));
        let daemon = sleeping_caddy(&dir, Duration::from_millis(500));
        let first = tokio::spawn({
            let daemon = Arc::clone(&daemon);
            async move { daemon.caddy().run(&daemon, ServiceAction::Start).await }
        });
        // Spawned: the first start holds the busy flag until it ends.
        while daemon.caddy().status().state == ServiceState::Stopped {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let mut setup = with_the_daemons_caddy(&dir, &fake);
        setup.ca_wait = Duration::from_secs(20);
        let app = App::new(daemon.clone(), setup);

        let (status, json) = app.post().await;
        assert_eq!(status, StatusCode::OK, "{json}");
        let report: SetupReport = serde_json::from_value(json).unwrap();
        // Busy taken for a running Caddy would wait for a root, then say
        // `ca_not_ready`.
        let skipped = [skip(OpName::TrustInstall, "caddy_not_started")];
        assert_eq!(report.skipped, skipped);
        let first = first.await.unwrap().unwrap();
        let last = daemon.caddy().status();
        assert_eq!(last.state, first.state);
        // A second start, after the first one ended.
        assert!(last.since_ms > first.since_ms, "{first:?} {last:?}");
        daemon.stop_procs(Duration::from_secs(5)).await;
    }

    /// A start of the daemon's Caddy, which holds the busy flag until its
    /// ready limit runs out ([`sleeping_caddy`]). Returns once it began.
    /// Unix only, as `sleeping_caddy`: it runs `/bin/sleep`.
    #[cfg(unix)]
    async fn a_first_start(
        daemon: &Arc<Daemon>,
    ) -> tokio::task::JoinHandle<Result<marsa_core::types::ServiceStatus, ActionError>> {
        let first = tokio::spawn({
            let daemon = Arc::clone(daemon);
            async move { daemon.caddy().run(&daemon, ServiceAction::Start).await }
        });
        while daemon.caddy().status().state == ServiceState::Stopped {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        first
    }

    /// [`DaemonCaddy::start`] from a blocking thread, as setup calls it.
    /// Unix only: its callers use `sleeping_caddy`.
    #[cfg(unix)]
    async fn daemon_start(
        daemon: &Arc<Daemon>,
        limit: Duration,
        stop: &CancellationToken,
    ) -> CaddyStart {
        let starter = DaemonCaddy {
            daemon: Arc::clone(daemon),
            runtime: Handle::current(),
        };
        let stop = stop.clone();
        tokio::task::spawn_blocking(move || starter.start(limit, &stop))
            .await
            .unwrap()
    }

    /// Another action holds Caddy, and less than Caddy's ready
    /// limit would be left for a start after it. Setup asks no more, so no
    /// start is cut off half way, and waits for a root the other action may
    /// make.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn no_start_begins_with_less_than_the_ready_limit_left() {
        let dir = Dir::new(false);
        let daemon = sleeping_caddy(&dir, Duration::from_secs(2));
        let first = a_first_start(&daemon).await;

        let began = Instant::now();
        let limit = Duration::from_secs(1);
        let started = daemon_start(&daemon, limit, &CancellationToken::new()).await;
        assert_eq!(started, CaddyStart::Started);
        assert!(began.elapsed() < limit, "{:?}", began.elapsed());

        let first = first.await.unwrap().unwrap();
        // Only the first start ran.
        assert_eq!(daemon.caddy().status(), first);
        daemon.stop_procs(Duration::from_secs(5)).await;
    }

    /// The daemon stops while setup asks a busy Caddy again.
    /// The retry ends at once, without a start of its own.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stop_during_the_retry_ends_it() {
        let dir = Dir::new(false);
        let daemon = sleeping_caddy(&dir, Duration::from_secs(2));
        let first = a_first_start(&daemon).await;

        let stop = CancellationToken::new();
        let cancel = stop.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel.cancel();
        });
        let began = Instant::now();
        let started = daemon_start(&daemon, Duration::from_secs(20), &stop).await;
        let stopping = CaddyStart::Failed("the daemon is stopping".to_owned());
        assert_eq!(started, stopping);
        assert!(
            began.elapsed() < Duration::from_secs(1),
            "{:?}",
            began.elapsed()
        );

        let first = first.await.unwrap().unwrap();
        assert_eq!(daemon.caddy().status(), first);
        daemon.stop_procs(Duration::from_secs(5)).await;
    }
}
