//! php-fpm pools for the sites, one per PHP line. Task M4-11.
//!
//! For each `major.minor` line that a Laravel or PHP site needs, the daemon
//! writes `php/<minor>/php.ini` and `php/<minor>/php-fpm.conf` (the text
//! comes from `marsa_core::php`), and runs `php-fpm` of the newest version of
//! that line under the supervisor, as `php-fpm-<minor>`. Like the
//! `valet-fpm.conf` that Valet writes per version, but the daemon owns the
//! processes:
//!
//! - a line that no site needs any more is stopped;
//! - a pool whose files or program changed is restarted, and only that one;
//! - nothing else is touched.
//!
//! Caddy reaches each pool on its Unix socket, or on `127.0.0.1:<port>` when
//! the socket path is too long ([`fallback_port`]). Windows has no php-fpm:
//! there each line runs one php-cgi on its fallback port instead ([`cgi`],
//! ADR 0016), as the service `php-cgi-<minor>`.
//!
//! [`write_ca_bundle`] writes the CA bundle that `php.ini` points to, so PHP
//! trusts the certificates of the other `.test` sites.
//!
//! Every other installed line gets its `php.ini` too (task M5-11): `php` in
//! the terminal reads it through the shim's `PHPRC`. While `[php] xdebug` is
//! on in `global.toml`, each `php.ini` loads `ext/xdebug.so` of its PHP, and
//! a pool restarts when its `php.ini` changes, like for any other change.
//!
//! Each pool is a service like Caddy (task M4-23): [`Pools`] keeps its
//! status and recent lines for `GET /v1/services` and `GET /v1/logs`, and
//! [`orphan`] stops a php-fpm that a crashed daemon left on the pool's
//! socket before the pool starts (ADR 0009).

pub mod cgi;
pub mod orphan;

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use marsa_core::caddy::{CaddyPaths, Dial, PhpPools, ca_bundle_text};
use marsa_core::config::{self, ConfigError, ConfigPaths};
use marsa_core::php::{Listen, PhpError, PhpFiles, PhpMinor, fpm_conf, fpm_listen, php_ini};
use marsa_core::platform::process;
use marsa_core::recipes::{self, Platform};
use marsa_core::runtimes::{Runtime, RuntimesDir, RuntimesError};
use marsa_core::services::ServiceSpec;
use marsa_core::sites::{Site, SiteKind};
use marsa_core::types::{Event, LogStream, ServiceState, ServiceStatus};
use semver::Version;
use tokio::sync::broadcast;
use tracing::{info, warn};

use self::orphan::{Blocked, FpmProcesses, Orphans, PidFile, SystemFpm};
use super::caddy::orphan::Ladder;
use super::caddy::{RESTART, STOP_GRACE};
use super::state::Daemon;
use super::supervisor::{ServiceRuntime, Supervisor, lock, now_ms};

/// The TCP fallback of PHP `major.minor` is `19000 + major * 10 + minor`:
/// 19084 for PHP 8.4.
const FALLBACK_BASE: u64 = 19000;

/// The PHP lines that the sites need, each with the newest version that one
/// of its sites picked. Only Laravel and PHP sites with a version count.
pub fn wanted_pools(sites: &[Site]) -> BTreeMap<PhpMinor, Version> {
    let mut wanted: BTreeMap<PhpMinor, Version> = BTreeMap::new();
    for site in sites {
        if !matches!(site.kind, SiteKind::Laravel | SiteKind::Php) {
            continue;
        }
        let Some(version) = &site.php else {
            continue;
        };
        let newest = wanted
            .entry(PhpMinor::from(version))
            .or_insert_with(|| version.clone());
        if version > newest {
            *newest = version.clone();
        }
    }
    wanted
}

/// The service id of the pool of `minor`: `php-fpm-8.4`, or `php-cgi-8.4` on
/// Windows, where php-cgi serves PHP (ADR 0016). Also its log file,
/// `logs/<id>.log`.
pub fn fpm_id(minor: PhpMinor) -> String {
    // `cfg!` outside `platform`: the id names the program that runs on this
    // system, and the daemon always runs on the system it was built for.
    let program = if cfg!(windows) { "php-cgi" } else { "php-fpm" };
    format!("{program}-{minor}")
}

/// The port php-fpm listens on when the socket path is too long:
/// `19000 + major * 10 + minor`, so 19084 for PHP 8.4.
pub fn fallback_port(minor: PhpMinor) -> u16 {
    let port = minor
        .major
        .saturating_mul(10)
        .saturating_add(minor.minor)
        .saturating_add(FALLBACK_BASE);
    // Only a made-up version is that high; its pool then fails to bind.
    u16::try_from(port).unwrap_or(u16::MAX)
}

/// Where the recipe puts `php-fpm` of `version`:
/// `runtimes/php/<version>/bin/php-fpm` on macOS and Linux. `None` where the
/// recipe has no `php-fpm` (Windows).
pub fn fpm_program(data_dir: &Path, platform: Platform, version: &Version) -> Option<PathBuf> {
    // A test parses every built-in recipe, so an error here never ships.
    let recipe = recipes::builtin(Runtime::Php).ok()?;
    let version_dir = RuntimesDir::in_dir(data_dir).version_dir(Runtime::Php, version);
    recipe
        .platform(platform)?
        .program_path(&version_dir, "php-fpm")
}

/// How the supervisor runs the pool of `files`: `program --nodaemonize
/// --force-stderr -y <php-fpm.conf> -c <php.ini>`, restarted after a failure
/// like Caddy. `--force-stderr` sends php-fpm's own log (and the workers'
/// output it catches) to stderr, which the supervisor reads, instead of the
/// `error_log` file, so `GET /v1/logs/php-fpm-<minor>` shows it.
pub fn fpm_spec(program: PathBuf, files: &PhpFiles) -> ServiceSpec {
    // `to_string_lossy` as in `caddy_spec`: the app data dir comes from the
    // OS and is valid Unicode in practice.
    let path = |path: &Path| path.to_string_lossy().into_owned();
    ServiceSpec {
        id: fpm_id(files.minor),
        program,
        args: vec![
            "--nodaemonize".to_owned(),
            "--force-stderr".to_owned(),
            "-y".to_owned(),
            path(&files.fpm_conf_file),
            "-c".to_owned(),
            path(&files.ini_file),
        ],
        env: BTreeMap::new(),
        cwd: None,
        restart: RESTART,
        stop_grace: STOP_GRACE,
    }
}

/// One pool as it should run: its files and their text, how it runs, and
/// where Caddy reaches it. A running pool restarts only when this changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pool {
    pub files: PhpFiles,
    /// The text of `php-fpm.conf`. Empty for php-cgi (Windows), which has no
    /// config file of its own: then no `php-fpm.conf` is written.
    pub fpm_conf: String,
    pub php_ini: String,
    pub spec: ServiceSpec,
    pub dial: Dial,
}

/// Why a PHP line gets no pool. Only logged: its sites then answer 503 from
/// Caddy, with a text that names the PHP line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skip {
    /// Stable name: `php_path_not_allowed`, `php_fpm_unavailable`,
    /// `php_fpm_not_installed`, or `php_config_write`.
    pub code: &'static str,
    pub reason: String,
}

/// The pool of the installed PHP `version` under `data_dir`. Pure: nothing is
/// read or written. `php.ini` points to `ca_bundle` when it is given.
///
/// # Errors
///
/// `php_path_not_allowed` if a path cannot go in the files, and
/// `php_fpm_unavailable` if the recipe has no `php-fpm` for `platform`.
/// Windows gets the php-cgi pool of [`cgi::pool_for`].
pub fn pool_for(
    data_dir: &Path,
    platform: Platform,
    version: &Version,
    ca_bundle: Option<&Path>,
) -> Result<Pool, Skip> {
    let mut files = PhpFiles::new(data_dir, version);
    files.ca_bundle = ca_bundle.map(Path::to_path_buf);
    pool_with(data_dir, platform, version, files)
}

/// [`pool_for`] with `files` as they are, so with the Xdebug they name.
fn pool_with(
    data_dir: &Path,
    platform: Platform,
    version: &Version,
    files: PhpFiles,
) -> Result<Pool, Skip> {
    if platform == Platform::WindowsX64 {
        return cgi::pool_with(files);
    }
    let listen = fpm_listen(&files.socket, fallback_port(files.minor));
    let conf = fpm_conf(&listen, &files).map_err(php_skip)?;
    let ini = php_ini(platform, &files).map_err(php_skip)?;
    let program = fpm_program(data_dir, platform, version).ok_or_else(|| Skip {
        code: "php_fpm_unavailable",
        reason: format!("Marsa has no php-fpm for {platform}"),
    })?;
    let dial = match listen {
        Listen::Unix(socket) => Dial::Unix(socket),
        Listen::Tcp(port) => Dial::Tcp(port),
    };
    Ok(Pool {
        spec: fpm_spec(program, &files),
        files,
        fpm_conf: conf,
        php_ini: ini,
        dial,
    })
}

fn php_skip(err: PhpError) -> Skip {
    Skip {
        code: err.code(),
        reason: err.to_string(),
    }
}

/// `[php] xdebug` in `global.toml`: whether each `php.ini` loads Xdebug.
///
/// # Errors
///
/// A `config_*` code if the file is there but cannot be read.
pub fn xdebug_enabled(data_dir: &Path) -> Result<bool, ConfigError> {
    config::load_global(&ConfigPaths::in_dir(data_dir).global).map(|global| global.php.xdebug)
}

/// The version that stands for each installed PHP line. `php.ini` is one
/// file per line, so one version gives it its paths: the one the line's pool
/// runs (`wanted`), else the newest installed.
///
/// # Errors
///
/// `runtimes_read` if the folder of the PHP versions cannot be read.
pub fn line_versions(
    data_dir: &Path,
    wanted: &BTreeMap<PhpMinor, Version>,
) -> Result<BTreeMap<PhpMinor, Version>, RuntimesError> {
    let installed = RuntimesDir::in_dir(data_dir).installed(Runtime::Php)?;
    // Oldest first, so the newest version of a line is the one that stays.
    let newest = installed.into_iter().map(|v| (PhpMinor::from(&v), v));
    let mut lines: BTreeMap<PhpMinor, Version> = newest.collect();
    lines.extend(
        wanted
            .iter()
            .map(|(minor, version)| (*minor, version.clone())),
    );
    Ok(lines)
}

/// The files of the line that `version` stands for. `php.ini` points to
/// `ca_bundle` when it is given. With `xdebug`, it loads `ext/xdebug.so` of
/// `version` if that file is there: the official Windows build and an older
/// install have none, and run without Xdebug. The path is absolute like
/// `data_dir`, and not canonicalized.
pub fn line_files(
    data_dir: &Path,
    version: &Version,
    ca_bundle: Option<&Path>,
    xdebug: bool,
) -> PhpFiles {
    let mut files = PhpFiles::new(data_dir, version);
    files.ca_bundle = ca_bundle.map(Path::to_path_buf);
    files.xdebug = Some(files.xdebug_so()).filter(|file| xdebug && file.is_file());
    files
}

/// Writes `php.ini` of a line with no running pool, if its text changed.
fn write_ini(platform: Platform, files: &PhpFiles) -> Result<(), Skip> {
    let text = php_ini(platform, files).map_err(php_skip)?;
    match write_if_changed(&files.ini_file, &text) {
        Ok(_) => Ok(()),
        Err(err) => Err(Skip {
            code: "php_config_write",
            reason: format!("cannot write {}: {err}", files.ini_file.display()),
        }),
    }
}

/// Checks that php-fpm (or php-cgi) is installed, makes the folders it
/// needs, and writes the files whose text changed.
fn write_pool(pool: &Pool) -> Result<(), Skip> {
    if !pool.spec.program.is_file() {
        return Err(Skip {
            code: "php_fpm_not_installed",
            reason: format!("no PHP program at {}", pool.spec.program.display()),
        });
    }
    let files = &pool.files;
    let cannot = |path: &Path| {
        let path = path.to_path_buf();
        move |err: io::Error| Skip {
            code: "php_config_write",
            reason: format!("cannot write {}: {err}", path.display()),
        }
    };
    // php-fpm does not start without the folder of its log and its socket.
    for file in [&files.fpm_log, &files.socket] {
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir).map_err(cannot(dir))?;
        }
    }
    if !pool.fpm_conf.is_empty() {
        write_if_changed(&files.fpm_conf_file, &pool.fpm_conf)
            .map_err(cannot(&files.fpm_conf_file))?;
    }
    write_if_changed(&files.ini_file, &pool.php_ini).map_err(cannot(&files.ini_file))?;
    Ok(())
}

/// The pool of each wanted line, with its files written, and `php.ini` of
/// every other installed line. Each `php.ini` loads Xdebug while `[php]
/// xdebug` is on. A pool that cannot run is logged and left out, and its
/// line still gets `php.ini` if that PHP is installed. Reads and writes the
/// disk, so it runs on a blocking thread.
fn prepare(
    data_dir: &Path,
    platform: Platform,
    wanted: &BTreeMap<PhpMinor, Version>,
    ca_bundle: Option<&Path>,
) -> BTreeMap<PhpMinor, Pool> {
    let xdebug = xdebug_enabled(data_dir).unwrap_or_else(|err| {
        warn!(code = err.code(), error = %err, "global.toml; PHP runs without Xdebug");
        false
    });
    let lines = line_versions(data_dir, wanted).unwrap_or_else(|err| {
        warn!(code = err.code(), error = %err, "cannot list the installed PHP versions");
        wanted.clone()
    });
    let runtimes = RuntimesDir::in_dir(data_dir);
    let mut pools = BTreeMap::new();
    for (minor, version) in &lines {
        let files = line_files(data_dir, version, ca_bundle, xdebug);
        if wanted.contains_key(minor) {
            let pool = pool_with(data_dir, platform, version, files.clone())
                .and_then(|pool| write_pool(&pool).map(|()| pool));
            match pool {
                Ok(pool) => {
                    pools.insert(*minor, pool);
                    continue;
                }
                Err(skip) => warn!(
                    php = %version,
                    code = skip.code,
                    reason = %skip.reason,
                    "no PHP pool; its sites answer 503"
                ),
            }
            // A site may name a PHP that is not installed: then no `php`
            // in the terminal reads this file either.
            if !runtimes.version_dir(Runtime::Php, version).is_dir() {
                continue;
            }
        }
        // No pool, or one that cannot run (no php-fpm or php-cgi): `php` in
        // the terminal still reads this file, so it follows the switch.
        if let Err(skip) = write_ini(platform, &files) {
            warn!(
                php = %version,
                code = skip.code,
                reason = %skip.reason,
                "php.ini was not written; php in the terminal runs without it"
            );
        }
    }
    pools
}

/// What a sync changes in the pools, each list in PHP line order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PoolChanges {
    /// Started before, and no site needs the line any more.
    pub stop: Vec<PhpMinor>,
    /// Running, but it would now run with other files or another program.
    pub restart: Vec<PhpMinor>,
    /// Needed and not running: a new line, or a pool whose task ended after
    /// its last restart (it gets a new try).
    pub start: Vec<PhpMinor>,
}

impl PoolChanges {
    pub fn is_empty(&self) -> bool {
        self.stop.is_empty() && self.restart.is_empty() && self.start.is_empty()
    }
}

/// Compares the pools started before (`running`, as they were started) with
/// the pools the sites need now. `alive` says whether the task of a line
/// still runs.
pub fn plan_pools<P: PartialEq>(
    running: &BTreeMap<PhpMinor, P>,
    wanted: &BTreeMap<PhpMinor, P>,
    alive: impl Fn(PhpMinor) -> bool,
) -> PoolChanges {
    let mut changes = PoolChanges::default();
    for (minor, pool) in wanted {
        match running.get(minor) {
            Some(_) if !alive(*minor) => changes.start.push(*minor),
            Some(old) if old != pool => changes.restart.push(*minor),
            Some(_) => {}
            None => changes.start.push(*minor),
        }
    }
    changes.stop = running
        .keys()
        .filter(|minor| !wanted.contains_key(minor))
        .copied()
        .collect();
    changes
}

/// The status and recent lines of each pool started in this run of the
/// daemon, for `GET /v1/services` and `GET /v1/logs/php-fpm-<minor>`. A
/// pool's entry stays after it stops, as Caddy's does: its lines stay
/// readable, and `seq` keeps growing across its runs.
#[derive(Debug, Default)]
pub struct Pools(Mutex<BTreeMap<PhpMinor, Arc<Mutex<ServiceRuntime>>>>);

impl Pools {
    /// The runtime of the pool of `minor`, made on first use.
    pub fn runtime(&self, minor: PhpMinor) -> Arc<Mutex<ServiceRuntime>> {
        let mut pools = lock(&self.0);
        let runtime = pools
            .entry(minor)
            .or_insert_with(|| Arc::new(Mutex::new(ServiceRuntime::new(&fpm_id(minor)))));
        Arc::clone(runtime)
    }

    /// The status of each pool, in PHP line order.
    pub fn statuses(&self) -> Vec<ServiceStatus> {
        // Copied out first: one lock at a time.
        let runtimes: Vec<_> = lock(&self.0).values().cloned().collect();
        runtimes
            .iter()
            .map(|runtime| lock(runtime).status.clone())
            .collect()
    }

    /// The runtime of the pool whose service id is `id` (`php-fpm-8.4`), if
    /// it was started in this run.
    pub fn find(&self, id: &str) -> Option<Arc<Mutex<ServiceRuntime>>> {
        let pools = lock(&self.0);
        let mut found = pools.iter().filter(|(minor, _)| fpm_id(**minor) == id);
        found.next().map(|(_, runtime)| Arc::clone(runtime))
    }
}

/// The pools of this daemon. The sync task owns it, so only that task starts
/// or stops a pool. The process tasks live in `Daemon::procs`, so a shutdown
/// stops them.
#[derive(Debug)]
pub struct PhpManager {
    data_dir: PathBuf,
    /// `None` on a system Marsa has no builds for.
    platform: Option<Platform>,
    /// Each pool started, as it was started.
    running: BTreeMap<PhpMinor, Pool>,
    /// The warning about Windows was logged: once per run is enough.
    warned: bool,
    /// Whether pid files are written and orphans stopped:
    /// `process::ORPHAN_RECOVERY`. Only tests change it.
    recovery: bool,
    /// Who runs under a pid, and signals, for the orphan check.
    processes: Arc<dyn FpmProcesses>,
    /// How long SIGTERM and SIGKILL to an orphan master wait (ADR 0009).
    ladder: Ladder,
    /// The same for the workers it left.
    worker_ladder: Ladder,
}

impl PhpManager {
    pub fn new(data_dir: &Path, platform: Option<Platform>) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            platform,
            running: BTreeMap::new(),
            warned: false,
            recovery: process::ORPHAN_RECOVERY,
            processes: Arc::new(SystemFpm),
            ladder: Ladder::default(),
            worker_ladder: Ladder::default(),
        }
    }

    /// Brings the pools in line with `sites`: writes the files, then stops,
    /// restarts, and starts only the pools that need it. `php.ini` points to
    /// `ca_bundle` when it is given, and loads Xdebug while `[php] xdebug` is
    /// on in `global.toml`. Returns where Caddy reaches each pool.
    pub async fn sync(
        &mut self,
        daemon: &Daemon,
        sites: &[Site],
        ca_bundle: Option<&Path>,
    ) -> PhpPools {
        let wanted = wanted_pools(sites);
        let pools = match self.platform {
            Some(platform) if runs_pools(platform) => {
                let data_dir = self.data_dir.clone();
                let ca_bundle = ca_bundle.map(Path::to_path_buf);
                let job = move || prepare(&data_dir, platform, &wanted, ca_bundle.as_deref());
                tokio::task::spawn_blocking(job)
                    .await
                    .unwrap_or_else(|err| {
                        warn!(error = %err, "the PHP files job failed");
                        BTreeMap::new()
                    })
            }
            _ => {
                if !wanted.is_empty() && !self.warned {
                    self.warned = true;
                    warn!("Marsa has no PHP for this system; PHP sites answer 503");
                }
                BTreeMap::new()
            }
        };

        let changes = plan_pools(&self.running, &pools, |minor| {
            daemon.proc_alive(&fpm_id(minor))
        });
        for minor in &changes.stop {
            stop_pool(daemon, *minor).await;
            self.running.remove(minor);
            info!(php = %minor, "php-fpm stopped: no site needs it");
        }
        for minor in changes.restart.iter().chain(&changes.start) {
            let Some(pool) = pools.get(minor) else {
                continue;
            };
            // The run before, if any: changed, or ended.
            stop_pool(daemon, *minor).await;
            if self.start_pool(daemon, pool).await {
                self.running.insert(*minor, pool.clone());
            } else {
                self.running.remove(minor);
            }
        }
        self.dials()
    }

    /// Where Caddy reaches each started pool.
    fn dials(&self) -> PhpPools {
        let dials = self.running.iter();
        PhpPools(
            dials
                .map(|(minor, pool)| (*minor, vec![pool.dial.clone()]))
                .collect(),
        )
    }

    /// Starts the pool under the supervisor, after the orphan check. `false`
    /// if it cannot start: its status then says why.
    async fn start_pool(&self, daemon: &Daemon, pool: &Pool) -> bool {
        let id = &pool.spec.id;
        let runtime = daemon.pools.runtime(pool.files.minor);
        let pid_file = PidFile::new(&daemon.data_dir, pool.files.minor);
        if let Err(blocked) = self.clear_orphan(daemon, pool, &pid_file).await {
            warn!(service = %id, code = blocked.code, reason = %blocked.reason, "php-fpm was not started");
            let text = format!("marsad: php-fpm was not started: {}.", blocked.reason);
            note(&runtime, &daemon.events, &text);
            set_failed(&runtime, &daemon.events, blocked.code);
            return false;
        }
        // Before the spawn, so the watcher cannot miss the first `running`.
        let lines = daemon.events.subscribe();
        let logs_dir = daemon.data_dir.join("logs");
        let spec = pool.spec.clone();
        let events = daemon.events.clone();
        match Supervisor::spawn_on(spec, events, &logs_dir, Arc::clone(&runtime)) {
            Ok(handle) => {
                info!(service = %id, program = %pool.spec.program.display(), "php-fpm started");
                let handle = if self.recovery {
                    let program = pool.spec.program.clone();
                    let processes = Arc::clone(&self.processes);
                    orphan::watch(handle, lines, pid_file, program, processes)
                } else {
                    handle
                };
                // `stop_pool` took the handle before, so none is replaced.
                if let Some(old) = daemon.add_proc(id.clone(), handle) {
                    old.cancel.cancel();
                }
                true
            }
            Err(err) => {
                warn!(service = %id, code = err.code(), %err, "cannot start php-fpm");
                set_failed(&runtime, &daemon.events, err.code());
                false
            }
        }
    }

    /// ADR 0009: stops our own php-fpm that a crashed daemon left running for
    /// this pool, and touches nothing that is not ours. Where recovery is off,
    /// only deletes a pid file left from before.
    async fn clear_orphan(
        &self,
        daemon: &Daemon,
        pool: &Pool,
        pid_file: &PidFile,
    ) -> Result<(), Blocked> {
        let (true, Some(platform)) = (self.recovery, self.platform) else {
            pid_file.discard();
            return Ok(());
        };
        let accepted = orphan::installed_programs(&self.data_dir, platform);
        let orphans = Orphans {
            pid_file,
            processes: &*self.processes,
            accepted: &accepted,
            listen: &pool.dial,
            ladder: self.ladder,
            worker_ladder: self.worker_ladder,
        };
        // The master, or the workers it left behind.
        let found = orphans.inspect().await?;
        let runtime = daemon.pools.runtime(pool.files.minor);
        // Only right before a signal: a stale file alone stops nothing.
        let note_stop = || note(&runtime, &daemon.events, orphan::STOPPING_ORPHAN);
        orphans.reap(found, &note_stop).await
    }
}

/// php-fpm runs on macOS and Linux, php-cgi on Windows ([`cgi`]).
/// `Platform::Any` is not a system.
pub fn runs_pools(platform: Platform) -> bool {
    platform != Platform::Any
}

async fn stop_pool(daemon: &Daemon, minor: PhpMinor) {
    if let Some(handle) = daemon.take_proc(&fpm_id(minor))
        && let Err(err) = handle.stop().await
    {
        warn!(php = %minor, %err, "the php-fpm task failed");
    }
}

/// Adds a line from the daemon to a pool's recent lines, so `marsa logs
/// php-fpm-<minor>` shows why the pool did not start.
fn note(runtime: &Mutex<ServiceRuntime>, events: &broadcast::Sender<Event>, text: &str) {
    let mut runtime = lock(runtime);
    let line = runtime.logs.push(LogStream::Stderr, text, now_ms());
    // Under the lock, like the supervisor, so events come in `seq` order.
    let _ = events.send(Event::Log(line));
}

/// Sets `Failed { code }` and tells every reader. Only while no task runs
/// the pool: the supervisor owns the status then.
fn set_failed(runtime: &Mutex<ServiceRuntime>, events: &broadcast::Sender<Event>, code: &str) {
    let mut runtime = lock(runtime);
    let status = &mut runtime.status;
    status.state = ServiceState::Failed {
        code: code.to_owned(),
    };
    status.pid = None;
    status.since_ms = Some(now_ms());
    // An error only means that nobody listens right now.
    let _ = events.send(Event::ServiceChanged(status.clone()));
}

/// Writes `caddy/ca-bundle.pem` once Caddy has made Marsa's CA: the first
/// file of `system` that exists, then Marsa's root ([`ca_bundle_text`]).
/// `false` while there is no root yet: Caddy makes it when it first loads a
/// config, with or without sites. The same text already there is left alone.
///
/// A system bundle that cannot be read is logged and left out, so PHP still
/// trusts Marsa's own sites.
///
/// # Errors
///
/// The root cannot be read, or the bundle cannot be written.
pub fn write_ca_bundle(paths: &CaddyPaths, system: &[&Path]) -> io::Result<bool> {
    let root = match fs::read_to_string(paths.ca_root()) {
        Ok(root) => root,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };
    let system_pem = match system.iter().find(|path| path.is_file()) {
        Some(path) => fs::read_to_string(path).unwrap_or_else(|err| {
            warn!(path = %path.display(), %err, "cannot read the system CA bundle");
            String::new()
        }),
        None => String::new(),
    };
    write_if_changed(&paths.ca_bundle(), &ca_bundle_text(&system_pem, &root))?;
    Ok(true)
}

/// Writes `text` to `path` through a temp file next to it and a rename, so a
/// reader never sees half a file. A file that already holds `text` is left
/// alone. `true` when the file was written.
///
/// # Errors
///
/// The old file cannot be read, or the new one cannot be written.
pub fn write_if_changed(path: &Path, text: &str) -> io::Result<bool> {
    match fs::read(path) {
        Ok(old) if old == text.as_bytes() => return Ok(false),
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let temp = path.with_file_name(name);
    let written = (|| {
        let mut file = File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        // Closed before the rename: Windows cannot rename an open file.
        drop(file);
        fs::rename(&temp, path)
    })();
    if written.is_err() {
        // Best effort: the write already failed, and that is the error.
        let _ = fs::remove_file(&temp);
    }
    written.map(|()| true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::resolver::Source;

    const PHP84: PhpMinor = PhpMinor { major: 8, minor: 4 };
    const PHP83: PhpMinor = PhpMinor { major: 8, minor: 3 };

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn site(name: &str, kind: SiteKind, php: Option<&str>) -> Site {
        let root = PathBuf::from("/sites").join(name);
        Site {
            name: name.into(),
            docroot: root.join("public"),
            root,
            kind,
            php: php.map(v),
            php_source: php.map(|_| Source::GlobalDefault),
            secure: true,
        }
    }

    #[test]
    fn wanted_pools_take_the_newest_version_of_each_php_line() {
        let sites = [
            site("a", SiteKind::Laravel, Some("8.4.3")),
            site("b", SiteKind::Php, Some("8.4.10")),
            site("c", SiteKind::Laravel, Some("8.4.5")),
            site("d", SiteKind::Php, Some("8.3.1")),
            // No PHP, or not a PHP site: no pool.
            site("e", SiteKind::Laravel, None),
            site("f", SiteKind::Static, Some("8.2.0")),
            site("g", SiteKind::NodeProxy { port: 5173 }, Some("8.1.0")),
        ];
        let wanted = wanted_pools(&sites);
        let expected = BTreeMap::from([(PHP83, v("8.3.1")), (PHP84, v("8.4.10"))]);
        assert_eq!(wanted, expected);
        assert!(wanted_pools(&[]).is_empty());
    }

    #[test]
    fn ids_and_fallback_ports_follow_the_php_line() {
        let program = if cfg!(windows) { "php-cgi" } else { "php-fpm" };
        assert_eq!(fpm_id(PHP84), format!("{program}-8.4"));
        assert_eq!(fallback_port(PHP84), 19084);
        assert_eq!(fallback_port(PhpMinor { major: 7, minor: 4 }), 19074);
        let huge = PhpMinor {
            major: u64::MAX,
            minor: 1,
        };
        assert_eq!(fallback_port(huge), u16::MAX);
    }

    #[test]
    fn fpm_program_comes_from_the_recipe() {
        let data = Path::new("/data");
        let version = v("8.4.3");
        let dir = data.join("runtimes").join("php").join("8.4.3");
        for platform in [Platform::DarwinArm64, Platform::LinuxX64] {
            let program = fpm_program(data, platform, &version);
            assert_eq!(program, Some(dir.join("bin").join("php-fpm")));
        }
        assert_eq!(fpm_program(data, Platform::WindowsX64, &version), None);
    }

    #[test]
    fn fpm_spec_runs_php_fpm_in_the_foreground_with_our_files() {
        let files = PhpFiles::new(Path::new("/data"), &v("8.4.3"));
        let program = PathBuf::from("/data/runtimes/php/8.4.3/bin/php-fpm");
        let spec = fpm_spec(program.clone(), &files);

        assert_eq!(spec.id, fpm_id(PHP84));
        assert_eq!(spec.program, program);
        let conf = files.fpm_conf_file.to_str().unwrap();
        let ini = files.ini_file.to_str().unwrap();
        let want = ["--nodaemonize", "--force-stderr", "-y", conf, "-c", ini];
        assert_eq!(spec.args, want);
        assert!(spec.env.is_empty());
        assert_eq!(spec.cwd, None);
        assert_eq!(spec.restart, RESTART);
        assert_eq!(spec.stop_grace, STOP_GRACE);
    }

    #[test]
    fn a_pool_uses_its_socket_or_the_fallback_port() {
        let version = v("8.4.3");
        let short = pool_for(Path::new("/data"), Platform::LinuxX64, &version, None).unwrap();
        assert_eq!(short.dial, Dial::Unix(short.files.socket.clone()));
        assert!(short.fpm_conf.contains("listen.mode = 0600"));
        assert!(!short.php_ini.contains("curl.cainfo"));

        // A data folder so deep that the socket path is over the limit.
        let deep = Path::new("/data").join("x".repeat(120));
        let bundle = deep.join("caddy").join("ca-bundle.pem");
        let long = pool_for(&deep, Platform::LinuxX64, &version, Some(&bundle)).unwrap();
        assert_eq!(long.dial, Dial::Tcp(19084));
        assert!(long.fpm_conf.contains("listen = 127.0.0.1:19084"));
        let cainfo = format!("curl.cainfo = \"{}\"", bundle.to_str().unwrap());
        assert!(long.php_ini.contains(&cainfo), "{}", long.php_ini);

        // Windows: php-cgi on the fallback port, with no php-fpm.conf.
        let windows = pool_for(Path::new("/data"), Platform::WindowsX64, &version, None).unwrap();
        assert_eq!(windows.dial, Dial::Tcp(19084));
        assert!(windows.fpm_conf.is_empty());
    }

    #[test]
    fn plan_pools_touches_only_what_changed() {
        let running = BTreeMap::from([
            (PhpMinor { major: 7, minor: 4 }, "same"),
            (PHP83, "old"),
            (PHP84, "same"),
            (PhpMinor { major: 8, minor: 2 }, "same"),
        ]);
        let wanted = BTreeMap::from([
            // 7.4 is gone.
            (PHP83, "new"),
            (PHP84, "same"),
            (PhpMinor { major: 8, minor: 2 }, "same"),
            (PhpMinor { major: 8, minor: 5 }, "same"),
        ]);
        // The 8.2 task ended after its last restart.
        let alive = |minor: PhpMinor| minor != PhpMinor { major: 8, minor: 2 };
        let changes = plan_pools(&running, &wanted, alive);
        let expected = PoolChanges {
            stop: vec![PhpMinor { major: 7, minor: 4 }],
            restart: vec![PHP83],
            start: vec![
                PhpMinor { major: 8, minor: 2 },
                PhpMinor { major: 8, minor: 5 },
            ],
        };
        assert_eq!(changes, expected);

        // The same pools again: nothing to do.
        assert!(plan_pools(&wanted, &wanted, |_| true).is_empty());
        // No site any more: every pool stops.
        let none = plan_pools(&wanted, &BTreeMap::new(), |_| true);
        assert_eq!(none.stop.len(), 4);
        assert!(none.start.is_empty() && none.restart.is_empty());
    }

    #[test]
    fn write_if_changed_writes_whole_files_only_when_needed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("php").join("8.4").join("php.ini");
        assert!(write_if_changed(&path, "a = 1\n").unwrap());
        assert!(!write_if_changed(&path, "a = 1\n").unwrap());
        assert!(write_if_changed(&path, "a = 2\n").unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "a = 2\n");
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["php.ini"], "no temp file left");
    }

    const ROOT: &str = "-----BEGIN CERTIFICATE-----\nMARSA\n-----END CERTIFICATE-----\n";
    const SYSTEM: &str = "-----BEGIN CERTIFICATE-----\nSYSTEM\n-----END CERTIFICATE-----\n";

    #[test]
    fn the_ca_bundle_waits_for_caddys_root() {
        let dir = tempfile::tempdir().unwrap();
        let paths = CaddyPaths::in_dir(dir.path());
        let system = dir.path().join("cert.pem");
        fs::write(&system, SYSTEM).unwrap();
        let missing = dir.path().join("missing.pem");
        let bundles = [missing.as_path(), system.as_path()];

        // No root yet: no bundle.
        assert!(!write_ca_bundle(&paths, &bundles).unwrap());
        assert!(!paths.ca_bundle().exists());

        // The first system bundle that exists, then the root.
        fs::create_dir_all(paths.ca_root().parent().unwrap()).unwrap();
        fs::write(paths.ca_root(), ROOT).unwrap();
        assert!(write_ca_bundle(&paths, &bundles).unwrap());
        let text = fs::read_to_string(paths.ca_bundle()).unwrap();
        assert_eq!(text, ca_bundle_text(SYSTEM, ROOT));
        assert!(text.contains("SYSTEM") && text.contains("MARSA"));

        // No system bundle (Windows): the root alone.
        assert!(write_ca_bundle(&paths, &[missing.as_path()]).unwrap());
        let text = fs::read_to_string(paths.ca_bundle()).unwrap();
        assert_eq!(text, ca_bundle_text("", ROOT));
    }

    #[tokio::test]
    async fn windows_without_php_cgi_runs_no_pool() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::new(dir.path().to_path_buf());
        let mut php = PhpManager::new(dir.path(), Some(Platform::WindowsX64));
        let sites = [site("shop", SiteKind::Laravel, Some("8.4.3"))];
        let pools = php.sync(&daemon, &sites, None).await;
        assert_eq!(pools, PhpPools::default());
        assert!(!daemon.proc_alive(&fpm_id(PHP84)));
        assert!(!dir.path().join("php").exists(), "no files written");
    }

    /// Windows: one php-cgi per line, on the fallback port, with `php.ini`
    /// and no `php-fpm.conf`. The stand-in `php-cgi.exe` is an empty file:
    /// whether the supervisor can run it is not this test's concern.
    #[tokio::test]
    async fn windows_runs_one_php_cgi_per_line() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::new(dir.path().to_path_buf());
        let program = PhpFiles::new(dir.path(), &v("8.4.3")).php_cgi;
        fs::create_dir_all(program.parent().unwrap()).unwrap();
        fs::write(&program, "").unwrap();

        let mut php = PhpManager::new(dir.path(), Some(Platform::WindowsX64));
        // No orphan check: it would look at this machine's processes.
        php.recovery = false;
        let sites = [site("shop", SiteKind::Laravel, Some("8.4.3"))];
        let pools = php.sync(&daemon, &sites, None).await;
        assert_eq!(pools.0, BTreeMap::from([(PHP84, vec![Dial::Tcp(19084)])]));
        assert!(daemon.proc_alive(&fpm_id(PHP84)));
        let config = dir.path().join("php").join("8.4");
        assert!(config.join("php.ini").is_file());
        assert!(!config.join("php-fpm.conf").exists());
        daemon.stop_procs(std::time::Duration::from_secs(5)).await;
    }

    /// A shell script stands in for php-fpm, which runs on macOS and Linux
    /// only: Windows has no `/bin/sh` (php-cgi: see above).
    #[cfg(unix)]
    #[tokio::test]
    async fn sync_starts_restarts_and_stops_only_what_changed() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let platform = Platform::current().unwrap();
        let daemon = Daemon::new(dir.path().to_path_buf());
        for version in ["8.4.3", "8.3.1"] {
            let program = fpm_program(dir.path(), platform, &v(version)).unwrap();
            fs::create_dir_all(program.parent().unwrap()).unwrap();
            fs::write(&program, "#!/bin/sh\nexec sleep 30\n").unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let pid = |id: &str| {
            let procs = daemon.procs.lock().unwrap();
            procs[id].runtime.lock().unwrap().status.pid
        };
        let wait_for_pid = async |id: &str| {
            for _ in 0..100 {
                if let Some(pid) = pid(id) {
                    return pid;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            panic!("{id} did not start");
        };

        let mut php = PhpManager::new(dir.path(), Some(platform));
        let sites = vec![
            site("shop", SiteKind::Laravel, Some("8.4.3")),
            site("blog", SiteKind::Php, Some("8.3.1")),
        ];
        let pools = php.sync(&daemon, &sites, None).await;
        assert_eq!(pools.0.keys().copied().collect::<Vec<_>>(), [PHP83, PHP84]);
        let first84 = wait_for_pid("php-fpm-8.4").await;
        let first83 = wait_for_pid("php-fpm-8.3").await;
        let ini = dir.path().join("php").join("8.4").join("php.ini");
        assert!(!fs::read_to_string(&ini).unwrap().contains("curl.cainfo"));
        assert!(
            dir.path()
                .join("php")
                .join("8.4")
                .join("php-fpm.conf")
                .is_file()
        );

        // The same sites again: nothing restarts.
        php.sync(&daemon, &sites, None).await;
        assert_eq!(pid("php-fpm-8.4"), Some(first84));

        // php.ini changes for both lines; only 8.4 is still needed after.
        let bundle = dir.path().join("caddy").join("ca-bundle.pem");
        let only_shop = &sites[..1];
        let pools = php.sync(&daemon, only_shop, Some(&bundle)).await;
        assert_eq!(pools.0.keys().copied().collect::<Vec<_>>(), [PHP84]);
        assert!(!daemon.proc_alive("php-fpm-8.3"), "8.3 stopped");
        let second84 = wait_for_pid("php-fpm-8.4").await;
        assert_ne!(second84, first84, "8.4 restarted");
        assert_ne!(second84, first83);
        assert!(fs::read_to_string(&ini).unwrap().contains("curl.cainfo"));

        // No PHP site any more.
        let pools = php.sync(&daemon, &[], Some(&bundle)).await;
        assert_eq!(pools, PhpPools::default());
        assert!(!daemon.proc_alive("php-fpm-8.4"));
        daemon.stop_procs(std::time::Duration::from_secs(5)).await;
    }

    /// `[php] xdebug` in the `global.toml` of `data`.
    fn set_xdebug(data: &Path, on: bool) {
        let mut global = config::GlobalConfig::default();
        global.php.xdebug = on;
        config::save_global(&ConfigPaths::in_dir(data).global, &global).unwrap();
    }

    #[test]
    fn each_line_takes_its_pools_version_or_the_newest_and_its_own_xdebug() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        assert!(line_versions(data, &BTreeMap::new()).unwrap().is_empty());
        for version in ["8.3.1", "8.4.3", "8.4.10"] {
            fs::create_dir_all(PhpFiles::new(data, &v(version)).ext_dir).unwrap();
        }
        let newest = BTreeMap::from([(PHP83, v("8.3.1")), (PHP84, v("8.4.10"))]);
        assert_eq!(line_versions(data, &BTreeMap::new()).unwrap(), newest);
        // A site pinned 8.4.3, so its pool runs that one.
        let wanted = BTreeMap::from([(PHP84, v("8.4.3"))]);
        let lines = line_versions(data, &wanted).unwrap();
        assert_eq!(lines[&PHP84], v("8.4.3"));
        assert_eq!(lines[&PHP83], v("8.3.1"));

        // Only 8.4.3 has the file, and only while Xdebug is on.
        let so = PhpFiles::new(data, &v("8.4.3")).xdebug_so();
        fs::write(&so, "").unwrap();
        assert!(
            so.is_absolute() && so.starts_with(data),
            "not canonicalized"
        );
        let bundle = data.join("ca.pem");
        let files = line_files(data, &v("8.4.3"), Some(&bundle), true);
        assert_eq!(files.xdebug, Some(so));
        assert_eq!(files.ca_bundle, Some(bundle));
        assert_eq!(line_files(data, &v("8.4.3"), None, false).xdebug, None);
        assert_eq!(line_files(data, &v("8.4.10"), None, true).xdebug, None);
        // On Windows the temp folder starts with `C:`, and `fpm_conf` refuses
        // the `:` in the `PATH` entry of `shims/` (`path_entry`,
        // `php_path_not_allowed`), so a Linux pool cannot live there.
        #[cfg(unix)]
        assert_eq!(
            pool_for(data, Platform::LinuxX64, &v("8.4.3"), None)
                .unwrap()
                .files
                .xdebug,
            None
        );

        assert!(!xdebug_enabled(data).unwrap(), "no global.toml: off");
        set_xdebug(data, true);
        assert!(xdebug_enabled(data).unwrap());
    }

    /// A pool that cannot run (no php-fpm, or no php-cgi on Windows) still
    /// leaves its line a `php.ini`, so `php` in the terminal follows the
    /// switch.
    #[test]
    fn a_line_whose_pool_cannot_run_still_gets_its_php_ini() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        let platform = Platform::current().unwrap();
        let so = PhpFiles::new(data, &v("8.4.3")).xdebug_so();
        fs::create_dir_all(so.parent().unwrap()).unwrap();
        fs::write(&so, "").unwrap();
        set_xdebug(data, true);

        // 8.3.1 is named by a site, and not installed.
        let wanted = BTreeMap::from([(PHP83, v("8.3.1")), (PHP84, v("8.4.3"))]);
        let pools = prepare(data, platform, &wanted, None);
        assert!(pools.is_empty(), "no php-fpm, so no pool");
        assert!(!data.join("php").join("8.3").exists(), "not installed");
        let line = data.join("php").join("8.4");
        let ini = fs::read_to_string(line.join("php.ini")).unwrap();
        let loads = format!("\nzend_extension = \"{}\"\n", so.display());
        assert!(ini.contains(&loads), "{ini}");
        assert!(!line.join("php-fpm.conf").exists());

        set_xdebug(data, false);
        prepare(data, platform, &wanted, None);
        let ini = fs::read_to_string(line.join("php.ini")).unwrap();
        assert!(!ini.contains("xdebug"), "{ini}");
    }

    /// As above: a shell script stands in for php-fpm.
    #[cfg(unix)]
    #[tokio::test]
    async fn xdebug_goes_into_every_php_ini_and_restarts_only_the_pools_it_changes() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        let platform = Platform::current().unwrap();
        let daemon = Daemon::new(data.to_path_buf());
        // 8.4.3 and 8.3.1 have a site; 8.2.0 has none. 8.3.1 has no Xdebug.
        for version in ["8.4.3", "8.3.1"] {
            let program = fpm_program(data, platform, &v(version)).unwrap();
            fs::create_dir_all(program.parent().unwrap()).unwrap();
            fs::write(&program, "#!/bin/sh\nexec sleep 30\n").unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let so = |version: &str| PhpFiles::new(data, &v(version)).xdebug_so();
        for version in ["8.4.3", "8.2.0"] {
            fs::create_dir_all(so(version).parent().unwrap()).unwrap();
            fs::write(so(version), "").unwrap();
        }
        let pid = async |id: &str, not: Option<u32>| {
            for _ in 0..100 {
                let pid = daemon.procs.lock().unwrap()[id]
                    .runtime
                    .lock()
                    .unwrap()
                    .status
                    .pid;
                if pid.is_some() && pid != not {
                    return pid;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            panic!("{id} did not start");
        };
        let ini = |line: &str| fs::read_to_string(data.join("php").join(line).join("php.ini"));
        let loads = |version: &str| format!("\nzend_extension = \"{}\"\n", so(version).display());

        let mut php = PhpManager::new(data, Some(platform));
        let sites = [
            site("shop", SiteKind::Laravel, Some("8.4.3")),
            site("blog", SiteKind::Php, Some("8.3.1")),
        ];
        php.sync(&daemon, &sites, None).await;
        let (off84, off83) = (
            pid("php-fpm-8.4", None).await,
            pid("php-fpm-8.3", None).await,
        );
        for line in ["8.2", "8.3", "8.4"] {
            assert!(!ini(line).unwrap().contains("xdebug"), "{line}");
        }
        assert!(!data.join("php").join("8.2").join("php-fpm.conf").exists());

        // On: each PHP that has the file loads it, the one with no pool too.
        set_xdebug(data, true);
        php.sync(&daemon, &sites, None).await;
        assert!(ini("8.4").unwrap().contains(&loads("8.4.3")));
        assert!(ini("8.2").unwrap().contains(&loads("8.2.0")));
        assert!(!ini("8.3").unwrap().contains("xdebug"), "8.3.1 has none");
        let on84 = pid("php-fpm-8.4", off84).await;
        assert_eq!(
            pid("php-fpm-8.3", None).await,
            off83,
            "its php.ini is the same"
        );

        // Off: the lines are gone, and 8.4 restarts again.
        set_xdebug(data, false);
        php.sync(&daemon, &sites, None).await;
        assert!(!ini("8.4").unwrap().contains("xdebug"));
        assert!(!ini("8.2").unwrap().contains("xdebug"));
        pid("php-fpm-8.4", on84).await;
        assert_eq!(pid("php-fpm-8.3", None).await, off83);
        daemon.stop_procs(std::time::Duration::from_secs(5)).await;
    }
}
