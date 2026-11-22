//! The daemon's life as the client sees it: find it, start it, replace an
//! older one, stop it (architecture.md section 6).

use std::cmp::Ordering;
use std::fs::{File, TryLockError};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use tokio::time::{Instant, sleep, timeout_at};

use super::error::daemon_text;
use super::{APP_VERSION, ClientError, DaemonClient, run_dir};
use crate::platform::daemon_file::{DaemonFileError, daemon_lock_path, read_daemon_file};
use crate::platform::process::{LoginTimes, read_login_times};
use crate::platform::spawn::{EXIT_ALREADY_RUNNING, marsad_path, spawn_marsad};
use crate::types::{API_VERSION, Health};

/// How long a new daemon has to answer. The wait ends as soon as it does, so
/// this only bounds a real failure. macOS checks a program the first time it
/// runs after an install or an update: about 2 s when the machine is idle,
/// more under load, so 5 s was too tight.
pub const START_WAIT: Duration = Duration::from_secs(15);
/// How long an old daemon has to stop. `marsad` lets go of its lock after
/// these steps (`server::run`), at most:
///
/// - its processes: 25 s while PostgreSQL runs (SIGINT and SIGQUIT, 10 s
///   each, and a 5 s margin for the kill), 10 s otherwise;
/// - its open connections: 5 s;
/// - the parked folder watcher, the sites sync, the starts of Caddy and of
///   PostgreSQL with the daemon, and the DNS server: 2 s each, 10 s.
///
/// 40 s in all. Then a second sweep of the processes waits for one that a
/// start added after the first sweep. That process was told to stop when it
/// was added, so the wait is normally under 1 s, and its stop overlaps the
/// steps above. So 45 s.
const STOP_WAIT: Duration = Duration::from_secs(45);
/// How much earlier than the login session a daemon may have started and
/// still count as of that session, in seconds. `ps` gives times
/// to the second. On the Mac measured for this task the login items, which
/// start a daemon at login, started about 10 s after `loginwindow` and about
/// 2 s before the Dock (read only when there is no `loginwindow`). A daemon
/// of the session before started before its logout, and a logout and a login
/// take longer.
const LOGIN_MARGIN_SECS: i64 = 10;
const POLL: Duration = Duration::from_millis(50);

/// A client for a daemon that fits this build, started if needed, with
/// `marsad` from the folder of the running program (version handshake,
/// architecture.md section 6).
///
/// 1. A daemon of this version or newer, with an API this build knows: use it.
/// 2. No daemon holds the lock: start `marsad`, then wait up to 5 s for it to
///    answer.
/// 3. A daemon with a newer API: `daemon_api_too_new`, and it keeps running.
/// 4. An older daemon: ask it to stop, wait up to 45 s for it to let go of
///    the lock, then step 2. If another client has replaced it meanwhile, the
///    new daemon is used.
/// 5. On macOS, a daemon that started before the user's current GUI login
///    session: replaced as in step 4. It outlived a logout, and
///    macOS shows no admin prompt for a process of a session that ended.
///    When either start time is unknown, the daemon is kept.
///
/// A daemon that holds the lock but does not answer yet (starting or
/// stopping) is waited for, up to 45 s.
///
/// # Errors
///
/// Any [`ClientError`]; `daemon_start_failed` if `marsad` cannot be started,
/// stops early, or does not answer, and `daemon_stop_failed` if an old
/// daemon does not stop.
pub async fn ensure_running(data_dir: &Path) -> Result<DaemonClient, ClientError> {
    ensure(data_dir, marsad_path, read_login_times).await
}

/// [`ensure_running`] with the `marsad` program given. Public so the
/// daemon tests can pass `env!("CARGO_BIN_EXE_marsad")`.
///
/// # Errors
///
/// As [`ensure_running`].
pub async fn ensure_running_with(
    data_dir: &Path,
    marsad: &Path,
) -> Result<DaemonClient, ClientError> {
    ensure(data_dir, || Ok(marsad.to_path_buf()), read_login_times).await
}

/// Stops the running daemon (`marsa daemon stop`): asks it to shut down,
/// then waits up to 45 s until it has let go of `run/daemon.lock`.
/// `Ok(false)` if no daemon was running.
///
/// # Errors
///
/// `daemon_stop_failed` if it still runs after 45 s, or the error of the
/// shutdown call if waiting cannot fix it.
pub async fn stop_daemon(data_dir: &Path) -> Result<bool, ClientError> {
    let lock = daemon_lock_path(&run_dir(data_dir));
    if !lock_is_held(&lock) {
        return Ok(false);
    }
    let deadline = Instant::now() + STOP_WAIT;
    let mut asked = false;
    let mut last_error = None;
    while lock_is_held(&lock) {
        if !asked {
            // No file or no answer yet: the daemon is starting or already
            // stopping. Asked again at the next poll.
            let ask = async { DaemonClient::connect(data_dir)?.shutdown().await };
            match timeout_at(deadline, ask).await {
                Ok(Ok(())) => asked = true,
                Ok(Err(err)) if err.is_transient() => last_error = Some(err),
                Ok(Err(err)) => return Err(err),
                Err(_) => {}
            }
        }
        if Instant::now() + POLL >= deadline {
            return Err(still_running(last_error.as_ref()));
        }
        sleep(POLL).await;
    }
    Ok(true)
}

/// Whether a daemon holds `<data_dir>/run/daemon.lock` now: one runs, or is
/// starting or stopping. Sends nothing and starts nothing. `marsa uninstall
/// --all` uses it to see whether the Marsa window or the menu-bar icon
/// started a daemon again.
pub fn is_daemon_running(data_dir: &Path) -> bool {
    lock_is_held(&daemon_lock_path(&run_dir(data_dir)))
}

/// `marsad` is looked up only when it must be started, so a running daemon
/// is found even if `current_exe` fails. `login` gives the [`LoginTimes`] of
/// the daemon at a pid: [`read_login_times`], or a fake in the tests.
async fn ensure<L, F>(
    data_dir: &Path,
    marsad: impl FnOnce() -> io::Result<PathBuf>,
    login: L,
) -> Result<DaemonClient, ClientError>
where
    L: Fn(u32) -> F,
    F: Future<Output = Option<LoginTimes>>,
{
    match current_daemon(data_dir, login).await? {
        Some(client) => Ok(client),
        None => start(data_dir, marsad).await,
    }
}

enum Probe {
    /// No daemon holds the lock.
    NotRunning,
    /// A daemon holds the lock but does not answer yet: it is starting or
    /// stopping.
    Busy,
    /// A daemon of this version or newer, with an API this build knows, from
    /// this login session or of one that cannot be told.
    Ready(DaemonClient),
    /// A daemon to stop and start again, and why.
    Replace(DaemonClient, Replace),
}

/// Why a daemon that answers is replaced.
enum Replace {
    /// It is older than this build: its version.
    Older(String),
    /// It started before the user's current GUI login session.
    OutlivedLogin(LoginTimes),
}

impl Replace {
    fn log(&self) {
        match self {
            Replace::Older(version) => {
                tracing::info!(daemon = %version, client = APP_VERSION, "stopping an older daemon");
            }
            Replace::OutlivedLogin(times) => tracing::info!(
                daemon_started = %times.started,
                login_started = %times.login,
                "stopping a daemon from an earlier login session"
            ),
        }
    }
}

/// The login check of the last daemon probed: its pid, and its times when it
/// outlived the login. A daemon is checked once, so `ps` does not run at
/// every poll while it stops.
type LoginCheck = Option<(u32, Option<LoginTimes>)>;

/// Steps 1, 3, 4, and 5 of [`ensure_running`]: a client for the running
/// daemon if it fits, or `None` once no daemon holds the lock. A daemon that
/// is older, or from an earlier login session, is asked to stop.
async fn current_daemon<L, F>(
    data_dir: &Path,
    login: L,
) -> Result<Option<DaemonClient>, ClientError>
where
    L: Fn(u32) -> F,
    F: Future<Output = Option<LoginTimes>>,
{
    let deadline = Instant::now() + STOP_WAIT;
    let mut asked_to_stop = false;
    let mut checked: LoginCheck = None;
    let mut last_error = None;
    loop {
        match timeout_at(deadline, probe(data_dir, &login, &mut checked)).await {
            Ok(Ok(Probe::Ready(client))) => return Ok(Some(client)),
            Ok(Ok(Probe::NotRunning)) => return Ok(None),
            Ok(Ok(Probe::Replace(client, why))) if !asked_to_stop => {
                why.log();
                match timeout_at(deadline, client.shutdown()).await {
                    Ok(Ok(())) => asked_to_stop = true,
                    Ok(Err(err)) if err.is_transient() => last_error = Some(err),
                    Ok(Err(err)) => return Err(err),
                    Err(_) => {}
                }
            }
            Ok(Ok(Probe::Replace(..) | Probe::Busy)) | Err(_) => {}
            Ok(Err(err)) if err.is_transient() => last_error = Some(err),
            Ok(Err(err)) => return Err(err),
        }
        if Instant::now() + POLL >= deadline {
            return Err(still_running(last_error.as_ref()));
        }
        sleep(POLL).await;
    }
}

/// Who holds the lock, which version it is, and whether it outlived the
/// login. `checked` keeps the login check from one probe to the next.
async fn probe<L, F>(
    data_dir: &Path,
    login: &L,
    checked: &mut LoginCheck,
) -> Result<Probe, ClientError>
where
    L: Fn(u32) -> F,
    F: Future<Output = Option<LoginTimes>>,
{
    let run = run_dir(data_dir);
    // The lock first: a file left by a crash is never contacted.
    if !lock_is_held(&daemon_lock_path(&run)) {
        return Ok(Probe::NotRunning);
    }
    let file = match read_daemon_file(&run) {
        Ok(file) => file,
        Err(DaemonFileError::Missing { .. }) => return Ok(Probe::Busy),
        Err(err) => return Err(err.into()),
    };
    let client = DaemonClient::from_file(&file)?;
    let health = match client.health().await {
        Err(ClientError::NotRunning) => return Ok(Probe::Busy),
        other => other?,
    };
    if is_older(&health)? {
        let version = daemon_text(&health.app_version);
        return Ok(Probe::Replace(client, Replace::Older(version)));
    }
    let outlived = match checked {
        Some((pid, outlived)) if *pid == health.pid => outlived.clone(),
        _ => {
            let now = SystemTime::now();
            let outlived = login(health.pid)
                .await
                .filter(|times| outlived_login(times, now));
            *checked = Some((health.pid, outlived.clone()));
            outlived
        }
    };
    match outlived {
        Some(times) => Ok(Probe::Replace(client, Replace::OutlivedLogin(times))),
        None => Ok(Probe::Ready(client)),
    }
}

/// Whether a daemon started before the current login session, more than
/// [`LOGIN_MARGIN_SECS`] before it: then it outlived a logout.
/// `false` when a time cannot be read, or when the login seems to come after
/// `now`: the clock was set back since, so the times say nothing. Without
/// that check, each new daemon would seem older than the login too, and be
/// replaced again at every call until the clock caught up.
fn outlived_login(times: &LoginTimes, now: SystemTime) -> bool {
    let (Some(started), Some(login)) = (times.started.utc_seconds(), times.login.utc_seconds())
    else {
        return false;
    };
    let Some(now) = now
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_secs()).ok())
    else {
        return false;
    };
    if login > now + LOGIN_MARGIN_SECS {
        return false;
    }
    started + LOGIN_MARGIN_SECS < login
}

/// Starts `marsad` and waits up to 5 s for it to answer.
///
/// If it exits at once with code 3, another daemon held the lock: if the
/// lock is free now (that daemon was stopping), `marsad` is started once
/// more; if not, that daemon is starting, and this waits for it. Any other
/// early exit fails at once.
async fn start(
    data_dir: &Path,
    marsad: impl FnOnce() -> io::Result<PathBuf>,
) -> Result<DaemonClient, ClientError> {
    let run = run_dir(data_dir);
    let lock = daemon_lock_path(&run);
    let log = data_dir.join("logs").join("marsad.log");
    // A file left by a crash stays until the new daemon replaces it. Its
    // daemon is gone, so it must not be contacted.
    let stale = read_daemon_file(&run)
        .ok()
        .map(|file| (file.pid, file.port))
        // Another client's daemon may have taken the lock and written this
        // file since our probe. Then the file is live: our `marsad` exits
        // with code 3 and the wait below uses that daemon.
        .filter(|_| !lock_is_held(&lock));
    let marsad = marsad().map_err(|err| {
        ClientError::start_failed("cannot find marsad next to this program", Some(err))
    })?;
    let spawn = || {
        let child = spawn_marsad(&marsad, data_dir).map_err(|err| {
            ClientError::start_failed(format!("cannot run {}", marsad.display()), Some(err))
        })?;
        tracing::info!(pid = child.pid(), path = %marsad.display(), "started marsad");
        Ok::<_, ClientError>(child)
    };
    let mut child = spawn()?;
    let mut respawned = false;
    let deadline = Instant::now() + START_WAIT;
    let mut last_error = None;
    loop {
        if let Some(status) = child.exit_status() {
            if status.code() != Some(EXIT_ALREADY_RUNNING) {
                return Err(ClientError::start_failed(
                    format!(
                        "marsad stopped during start-up ({status}); see {}",
                        log.display()
                    ),
                    None,
                ));
            }
            if !respawned && !lock_is_held(&lock) {
                respawned = true;
                child = spawn()?;
            }
        }
        match timeout_at(deadline, probe_new(data_dir, stale)).await {
            Ok(Ok(Some(client))) => return Ok(client),
            Ok(Ok(None)) | Err(_) => {}
            Ok(Err(err)) if err.is_transient() => last_error = Some(err),
            Ok(Err(err)) => return Err(err),
        }
        if Instant::now() + POLL >= deadline {
            let last = last_error
                .map(|err| format!(" (last error: {})", err.code()))
                .unwrap_or_default();
            return Err(ClientError::start_failed(
                format!(
                    "marsad did not answer within {} seconds{last}; see {}",
                    START_WAIT.as_secs(),
                    log.display()
                ),
                None,
            ));
        }
        sleep(POLL).await;
    }
}

/// A client for the new daemon once it holds the lock and answers. A file
/// whose `(pid, port)` is `stale` belongs to the crashed daemon and is not
/// contacted.
async fn probe_new(
    data_dir: &Path,
    stale: Option<(u32, u16)>,
) -> Result<Option<DaemonClient>, ClientError> {
    let run = run_dir(data_dir);
    if !lock_is_held(&daemon_lock_path(&run)) {
        return Ok(None);
    }
    let file = match read_daemon_file(&run) {
        Ok(file) if Some((file.pid, file.port)) != stale => file,
        Ok(_) | Err(DaemonFileError::Missing { .. }) => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let client = DaemonClient::from_file(&file)?;
    let health = client.health().await?;
    if is_older(&health)? {
        return Err(ClientError::start_failed(
            format!(
                "the daemon that answered is version {}, older than {APP_VERSION}",
                daemon_text(&health.app_version)
            ),
            None,
        ));
    }
    Ok(Some(client))
}

/// Steps 3 and 4 of the handshake for one answer: `daemon_api_too_new` for a
/// newer API, else whether the daemon is older than this build.
fn is_older(health: &Health) -> Result<bool, ClientError> {
    if health.api_version > API_VERSION {
        return Err(ClientError::ApiTooNew {
            daemon: health.api_version,
        });
    }
    Ok(older_than_this_build(&health.app_version))
}

/// Compared by semver precedence, so build metadata does not count. A
/// version that does not parse is replaced, unless it is exactly ours.
fn older_than_this_build(version: &str) -> bool {
    match (Version::parse(version), Version::parse(APP_VERSION)) {
        (Ok(theirs), Ok(ours)) => theirs.cmp_precedence(&ours) == Ordering::Less,
        _ => version != APP_VERSION,
    }
}

fn still_running(last_error: Option<&ClientError>) -> ClientError {
    let last = last_error
        .map(|err| format!(" (last error: {})", err.code()))
        .unwrap_or_default();
    ClientError::StopFailed {
        reason: format!(
            "it still holds its lock after {} seconds{last}",
            STOP_WAIT.as_secs()
        ),
    }
}

/// Whether a daemon holds `lock`. A missing lock file, or one this process
/// cannot lock or open, means no daemon could hold it either.
pub(super) fn lock_is_held(lock: &Path) -> bool {
    let Ok(file) = File::open(lock) else {
        return false;
    };
    match file.try_lock_shared() {
        Ok(()) => {
            // Released at once and on purpose: on Windows a lock released
            // only by closing the handle may stay a little longer, and a
            // daemon starting now would find it taken.
            if let Err(err) = file.unlock() {
                tracing::debug!(error = %err, "cannot release the probe lock");
            }
            false
        }
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(err)) => {
            tracing::debug!(error = %err, "cannot test the daemon lock");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering::SeqCst;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::platform::daemon_file::{DaemonFile, Token, remove_daemon_file, write_daemon_file};
    use crate::ps::StartTime;

    #[test]
    fn only_an_older_daemon_is_replaced() {
        let ours = Version::parse(APP_VERSION).unwrap();
        let newer = Version::new(ours.major, ours.minor, ours.patch + 1).to_string();
        let build = format!("{APP_VERSION}+other-build");
        for version in [APP_VERSION, newer.as_str(), build.as_str()] {
            assert!(!older_than_this_build(version), "{version}");
        }
        // A pre-release of this build's version is older. `-0` is the lowest
        // pre-release, so this holds when this build is a pre-release too
        // (0.1.0-0 < 0.1.0-rc.1); appending `-rc.1` to `0.1.0-rc.1` would not.
        let pre = format!("{}.{}.{}-0", ours.major, ours.minor, ours.patch);
        for version in [pre.as_str(), "not a version", ""] {
            assert!(older_than_this_build(version), "{version}");
        }
    }

    #[test]
    fn a_newer_api_is_refused() {
        let health = |api_version| Health {
            app_version: APP_VERSION.to_owned(),
            api_version,
            pid: 1,
        };
        assert!(!is_older(&health(API_VERSION)).unwrap());
        let err = is_older(&health(API_VERSION + 1)).unwrap_err();
        assert_eq!(err.code(), "daemon_api_too_new");
    }

    // ---------------------------------------------------------------------
    // A daemon left from an earlier login session

    /// A login on 2026-11-14 (UTC).
    const LOGIN: &str = "Sat Nov 14 13:28:04 2026";

    fn at(text: &str) -> StartTime {
        text.parse().expect("a start time")
    }

    fn times(started: &str, login: &str) -> LoginTimes {
        LoginTimes {
            started: at(started),
            login: at(login),
        }
    }

    /// `seconds` after the login.
    fn after_login(seconds: u64) -> SystemTime {
        let login = at(LOGIN).utc_seconds().unwrap();
        UNIX_EPOCH + Duration::from_secs(u64::try_from(login).unwrap() + seconds)
    }

    #[test]
    fn a_daemon_older_than_the_login_outlived_it() {
        let now = after_login(3_600);
        // The incident: started three days before.
        assert!(outlived_login(
            &times("Wed Nov 11 10:12:30 2026", LOGIN),
            now
        ));
        // Just past the margin.
        assert!(outlived_login(
            &times("Sat Nov 14 13:27:53 2026", LOGIN),
            now
        ));
    }

    #[test]
    fn a_daemon_of_this_login_or_within_the_margin_is_kept() {
        let now = after_login(3_600);
        for started in [
            // Started by the menu-bar icon at login, and later.
            "Sat Nov 14 13:28:26 2026",
            "Sat Nov 14 14:00:00 2026",
            // At the same second, and up to 10 s before (the Dock is read
            // when there is no loginwindow, and starts after login items).
            LOGIN,
            "Sat Nov 14 13:28:00 2026",
            "Sat Nov 14 13:27:54 2026",
        ] {
            assert!(!outlived_login(&times(started, LOGIN), now), "{started}");
        }
    }

    #[test]
    fn unknown_times_keep_the_daemon() {
        let now = after_login(3_600);
        let old = "Wed Nov 11 10:12:30 2026";
        // Text that is no time cannot reach here: it does not parse.
        for bad in ["", "Sat Nov 14 13:28 2026"] {
            assert!(bad.parse::<StartTime>().is_err(), "{bad:?}");
        }
        // Linux clock ticks, which are no time of day.
        let ticks = StartTime::Ticks(987_654);
        for (started, login) in [(ticks, at(LOGIN)), (at(old), ticks)] {
            let times = LoginTimes { started, login };
            assert!(!outlived_login(&times, now), "{times:?}");
        }
        // The clock was set back since the login: it seems to come later.
        let before_login = UNIX_EPOCH
            + Duration::from_secs(u64::try_from(at(LOGIN).utc_seconds().unwrap() - 60).unwrap());
        assert!(!outlived_login(&times(old, LOGIN), before_login));
        assert!(!outlived_login(&times(old, LOGIN), UNIX_EPOCH));
        // Within the margin it still counts.
        assert!(outlived_login(&times(old, LOGIN), after_login(0)));
    }

    /// The requests a [`fake_daemon`] got.
    #[derive(Default)]
    struct Calls {
        health: AtomicUsize,
        shutdown: AtomicUsize,
    }

    /// A daemon of this build that answers as `pid` and holds the lock in
    /// `data_dir` until it is asked to stop. Like `marsad`, which stops its
    /// processes first, it answers for 300 ms more, then lets go.
    async fn fake_daemon(data_dir: &Path, pid: u32) -> Arc<Calls> {
        use axum::routing::{get, post};

        let run = run_dir(data_dir);
        std::fs::create_dir_all(&run).unwrap();
        let lock = File::create(daemon_lock_path(&run)).unwrap();
        lock.try_lock().unwrap();
        let lock = Arc::new(Mutex::new(Some(lock)));
        let calls = Arc::new(Calls::default());
        let health = Health {
            app_version: APP_VERSION.to_owned(),
            api_version: API_VERSION,
            pid,
        };
        let shutdown = {
            let (run, calls) = (run.clone(), calls.clone());
            move || {
                calls.shutdown.fetch_add(1, SeqCst);
                let (run, lock) = (run.clone(), lock.clone());
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    remove_daemon_file(&run).unwrap();
                    if let Some(lock) = lock.lock().unwrap().take() {
                        lock.unlock().unwrap();
                    }
                });
                async { axum::http::StatusCode::ACCEPTED }
            }
        };
        let answer = {
            let calls = calls.clone();
            move || {
                calls.health.fetch_add(1, SeqCst);
                let health = health.clone();
                async move { axum::Json(health) }
            }
        };
        let router = axum::Router::new()
            .route("/v1/health", get(answer))
            .route("/v1/shutdown", post(shutdown));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let file = DaemonFile {
            pid,
            port,
            token: Token::generate().unwrap(),
            app_version: APP_VERSION.to_owned(),
        };
        write_daemon_file(&run, &file).unwrap();
        calls
    }

    /// A login check that answers `answer` for every pid, and counts its calls.
    fn fake_login(
        answer: Option<LoginTimes>,
        calls: &AtomicUsize,
    ) -> impl Fn(u32) -> std::future::Ready<Option<LoginTimes>> {
        move |_pid| {
            calls.fetch_add(1, SeqCst);
            std::future::ready(answer.clone())
        }
    }

    #[tokio::test]
    async fn a_daemon_from_an_earlier_login_is_asked_to_stop() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = fake_daemon(dir.path(), 5793).await;
        let checks = AtomicUsize::new(0);
        // Far in the past, so the real clock is past the login.
        let old = times("Thu Jan  1 00:00:00 1970", "Thu Jan  1 01:00:00 1970");
        let found = current_daemon(dir.path(), fake_login(Some(old), &checks)).await;
        // Stopped, so `ensure_running` starts a new one.
        assert!(found.unwrap().is_none());
        assert_eq!(daemon.shutdown.load(SeqCst), 1);
        assert!(!is_daemon_running(dir.path()));
        // It answered at more than one poll while it stopped, and was
        // checked at the first only.
        assert!(daemon.health.load(SeqCst) > 1);
        assert_eq!(checks.load(SeqCst), 1);
    }

    #[tokio::test]
    async fn a_daemon_of_this_login_or_of_an_unknown_one_is_used() {
        let new = times("Thu Jan  1 01:00:00 1970", "Thu Jan  1 00:00:00 1970");
        for answer in [Some(new), None] {
            let dir = tempfile::tempdir().unwrap();
            let daemon = fake_daemon(dir.path(), 5793).await;
            let checks = AtomicUsize::new(0);
            let found = current_daemon(dir.path(), fake_login(answer.clone(), &checks)).await;
            assert!(found.unwrap().is_some(), "{answer:?}");
            assert_eq!(daemon.shutdown.load(SeqCst), 0, "{answer:?}");
            assert!(is_daemon_running(dir.path()));
            assert_eq!(checks.load(SeqCst), 1);
        }
    }
}
