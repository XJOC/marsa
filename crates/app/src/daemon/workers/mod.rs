//! A Laravel site's workers: `php artisan queue:work` and `php artisan
//! schedule:work`, run while the site's setting asks for them
//! (`[sites.<name>] queue = true`, `schedule = true`).
//!
//! The sites sync owns the [`WorkerManager`], so one task starts and stops
//! the workers, after the PHP step of its pass has written each `php.ini`.
//! Each worker is one supervisor task in `Daemon::procs`, so a shutdown
//! stops it; [`Workers`] keeps their statuses and lines for the routes.
//!
//! - Wanted: a Laravel site whose setting asks for the worker. It runs the
//!   PHP CLI of the version the site runs, with Marsa's `php.ini` of that
//!   line, in the site's folder (`marsa_core::workers::worker_spec`).
//! - No longer wanted (turned off, the site gone or no longer Laravel): it is
//!   stopped by its stop method and leaves the list.
//! - Another PHP version, folder, or `php.ini` text: stopped, then started
//!   with the new one.
//! - Its restarts ran out (`worker_failed`): a pass leaves it `Failed`, or
//!   it would crash again in a loop. `restart` on the route starts it again,
//!   and so does turning it off and on.
//! - The site has no PHP: `Failed { php_not_installed }`, and nothing runs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use marsa_core::codes;
use marsa_core::data_dir::DataDir;
use marsa_core::php::PhpFiles;
use marsa_core::recipes::Platform;
use marsa_core::services::{RestartPolicy, ServiceSpec};
use marsa_core::sites::{Site, SiteKind};
use marsa_core::types::{ServiceId, ServiceState, ServiceStatus, SiteKey, WorkerKind};
use marsa_core::workers::{self, WorkerPaths, worker_spec};
use tracing::{info, warn};

use crate::daemon::services::php;
use crate::daemon::services::supervisor::{ServiceRuntime, Supervisor, lock};
use crate::daemon::state::Daemon;

/// The statuses and recent lines of the workers that are wanted now, for
/// the routes, and the restarts the routes asked for.
#[derive(Debug, Default)]
pub struct Workers {
    inner: Mutex<Inner>,
    restarts: Mutex<BTreeSet<ServiceId>>,
}

#[derive(Debug, Default)]
struct Inner {
    /// Every worker of this run. A worker turned off keeps its runtime, so
    /// its `seq` goes on if it is turned on again: a client never sees it go
    /// back. One per site and kind ever wanted in this run, each at most
    /// `LOG_LINES` lines, like the pools.
    runtimes: BTreeMap<ServiceId, Arc<Mutex<ServiceRuntime>>>,
    /// The ones wanted now: the routes see these only.
    listed: BTreeSet<ServiceId>,
}

impl Workers {
    /// The status of each worker, by id.
    pub fn statuses(&self) -> Vec<ServiceStatus> {
        // Copied out first: one lock at a time.
        let runtimes: Vec<_> = {
            let inner = lock(&self.inner);
            let listed = inner.listed.iter();
            listed
                .filter_map(|id| inner.runtimes.get(id).cloned())
                .collect()
        };
        runtimes
            .iter()
            .map(|runtime| lock(runtime).status.clone())
            .collect()
    }

    /// The runtime of worker `id`, if it is wanted now.
    pub fn find(&self, id: ServiceId) -> Option<Arc<Mutex<ServiceRuntime>>> {
        let inner = lock(&self.inner);
        inner
            .listed
            .contains(&id)
            .then(|| inner.runtimes.get(&id).cloned())
            .flatten()
    }

    /// Asks the next pass to start worker `id` again, also after its restarts
    /// ran out. The route then asks for a pass.
    pub fn ask_restart(&self, id: ServiceId) {
        lock(&self.restarts).insert(id);
    }

    fn take_restarts(&self) -> BTreeSet<ServiceId> {
        std::mem::take(&mut *lock(&self.restarts))
    }

    /// The runtime of worker `id`, made on first use, and listed.
    fn runtime(&self, id: ServiceId) -> Arc<Mutex<ServiceRuntime>> {
        self.list(id).0
    }

    /// [`Workers::runtime`], and whether it was not listed before.
    fn list(&self, id: ServiceId) -> (Arc<Mutex<ServiceRuntime>>, bool) {
        let mut inner = lock(&self.inner);
        let new = inner.listed.insert(id);
        let runtime = inner
            .runtimes
            .entry(id)
            .or_insert_with(|| Arc::new(Mutex::new(ServiceRuntime::new(id))));
        (Arc::clone(runtime), new)
    }

    fn listed(&self) -> Vec<ServiceId> {
        lock(&self.inner).listed.iter().copied().collect()
    }

    fn unlist(&self, id: ServiceId) {
        lock(&self.inner).listed.remove(&id);
    }
}

/// How often [`stop_workers`] looks whether the stopped tasks ended.
const STOP_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// How one worker runs. A change of any part starts it again.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Plan {
    spec: ServiceSpec,
    /// The text of its `php.ini`: Xdebug turned on or off, or the CA bundle
    /// that appeared, reaches a running worker only through a new start.
    /// `None`: it could not be read now (a lock held for a moment on
    /// Windows), which is no reason to stop a running worker.
    ini: Option<String>,
}

impl Plan {
    /// Whether a worker that runs `self` already runs `wanted`.
    fn runs(&self, wanted: &Plan) -> bool {
        self.spec == wanted.spec
            && wanted
                .ini
                .as_ref()
                .is_none_or(|ini| Some(ini) == self.ini.as_ref())
    }
}

/// Each wanted worker, and how it runs or the code of why it cannot.
type Wanted = BTreeMap<ServiceId, Result<Plan, &'static str>>;

/// The workers that `sites` ask for. Reads each `php.ini`: on a blocking
/// thread.
fn wanted(data_dir: &Path, platform: Option<Platform>, sites: &[Site]) -> Wanted {
    let mut wanted = Wanted::new();
    for site in sites.iter().filter(|site| site.kind == SiteKind::Laravel) {
        // A site's name is always valid (`sites::validate_name`).
        let Some(key) = SiteKey::new(&site.name) else {
            continue;
        };
        for kind in WorkerKind::ALL {
            let on = match kind {
                WorkerKind::Queue => site.queue,
                WorkerKind::Schedule => site.schedule,
            };
            if on {
                let plan = plan(data_dir, platform, site, kind, key);
                wanted.insert(ServiceId::Worker(kind, key), plan);
            }
        }
    }
    wanted
}

fn plan(
    data_dir: &Path,
    platform: Option<Platform>,
    site: &Site,
    kind: WorkerKind,
    key: SiteKey,
) -> Result<Plan, &'static str> {
    let version = site.php.as_ref().ok_or(codes::PHP_NOT_INSTALLED)?;
    let php = php_program(data_dir, platform, version).ok_or(codes::PHP_NOT_INSTALLED)?;
    let files = PhpFiles::new(data_dir, version);
    // Not written yet (no pass of PHP so far): the worker starts, and the
    // next pass, which writes it, starts it again.
    let ini = match fs::read_to_string(&files.ini_file) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(String::new()),
        Err(_) => None,
    };
    let paths = WorkerPaths {
        php: &php,
        php_ini: &files.ini_file,
        root: &site.root,
    };
    Ok(Plan {
        spec: worker_spec(kind, key, paths),
        ini,
    })
}

/// The PHP CLI of `version`: `bin/php` on macOS and Linux, `php.exe` on
/// Windows. `None` on a system Marsa has no PHP for.
fn php_program(
    data_dir: &Path,
    platform: Option<Platform>,
    version: &semver::Version,
) -> Option<PathBuf> {
    php::php_program(data_dir, platform?, version, "php")
}

/// Starts and stops the workers. The sites sync owns it.
#[derive(Debug)]
pub struct WorkerManager {
    data_dir: PathBuf,
    platform: Option<Platform>,
    /// Each worker started, as it was started.
    running: BTreeMap<ServiceId, Plan>,
    /// `workers::RESTART`; the tests make it short.
    restart: RestartPolicy,
}

impl WorkerManager {
    pub fn new(data_dir: &Path, platform: Option<Platform>) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            platform,
            running: BTreeMap::new(),
            restart: workers::RESTART,
        }
    }

    /// Brings the workers in line with `sites` (see the module).
    pub async fn sync(&mut self, daemon: &Daemon, sites: &[Site]) {
        // `stop_procs` may have taken the handles: a start now would run a
        // second process next to one that is still stopping.
        if daemon.shutdown().is_cancelled() {
            return;
        }
        let data_dir = self.data_dir.clone();
        let platform = self.platform;
        let list = sites.to_vec();
        let job = move || wanted(&data_dir, platform, &list);
        let wanted = match tokio::task::spawn_blocking(job).await {
            Ok(wanted) => wanted,
            Err(err) => {
                // Nothing is stopped for a job that failed.
                warn!(error = %err, "the workers job failed");
                return;
            }
        };
        let forced = daemon.workers().take_restarts();
        let gone: Vec<ServiceId> = daemon
            .workers()
            .listed()
            .into_iter()
            .filter(|id| !wanted.contains_key(id))
            .collect();
        let mut stop = gone.clone();
        let mut start = Vec::new();
        for (id, plan) in &wanted {
            match plan {
                Err(_) => stop.push(*id),
                Ok(plan)
                    if !self
                        .running
                        .get(id)
                        .is_some_and(|running| running.runs(plan))
                        || forced.contains(id) =>
                {
                    // The run before, if any: changed, or asked to restart.
                    stop.push(*id);
                    start.push((*id, plan));
                }
                Ok(_) => {}
            }
        }
        stop_workers(daemon, &stop).await;
        for id in &stop {
            self.running.remove(id);
        }
        for id in gone {
            daemon.workers().unlist(id);
            info!(service = %id, "worker stopped: its site no longer asks for it");
        }
        for (id, plan) in &wanted {
            if let Err(code) = plan {
                cannot_run(daemon, *id, code);
            }
        }
        // The stops above may have taken a while.
        if daemon.shutdown().is_cancelled() {
            return;
        }
        for (id, plan) in start {
            if self.start(daemon, plan) {
                self.running.insert(id, plan.clone());
            }
        }
    }

    /// Starts the worker under the supervisor. `false` if it cannot start:
    /// its status then says why.
    fn start(&self, daemon: &Daemon, plan: &Plan) -> bool {
        let id = plan.spec.id;
        let runtime = daemon.workers().runtime(id);
        let mut spec = plan.spec.clone();
        spec.restart = self.restart;
        let logs_dir = DataDir::new(daemon.data_dir()).logs();
        let events = daemon.events().clone();
        match Supervisor::spawn_on(spec, events, &logs_dir, Arc::clone(&runtime)) {
            Ok(handle) => {
                info!(service = %id, program = %plan.spec.program.display(), "worker started");
                // `stop_workers` took the handle before, so none is replaced.
                if let Some(old) = daemon.add_proc(id, handle) {
                    old.cancel.cancel();
                }
                true
            }
            Err(err) => {
                warn!(service = %id, code = err.code(), %err, "cannot start the worker");
                lock(&runtime).set_failed(daemon.events(), err.code());
                false
            }
        }
    }
}

/// `Failed { code }` for a wanted worker that cannot run, once: a pass that
/// finds it so again sends no event. A worker listed again (turned off and
/// on) sends it again, so a client that follows the events sees it come back.
fn cannot_run(daemon: &Daemon, id: ServiceId, code: &str) {
    let (runtime, new) = daemon.workers().list(id);
    let mut runtime = lock(&runtime);
    let already =
        matches!(&runtime.status.state, ServiceState::Failed { code: now } if now == code);
    if new || !already {
        warn!(service = %id, code, "the worker cannot run");
        let text = format!("marsad: the worker cannot run: {code}");
        runtime.note(daemon.events(), &text);
        runtime.set_failed(daemon.events(), code);
    }
}

/// Stops the workers `ids` by their stop methods, all at once, so their
/// waits (up to 30 s for a queue job) overlap instead of adding up. The
/// handles stay in `Daemon::procs` until each task ends, so a shutdown that
/// comes meanwhile still waits for their stop.
async fn stop_workers(daemon: &Daemon, ids: &[ServiceId]) {
    let asked: Vec<ServiceId> = ids
        .iter()
        .copied()
        .filter(|id| daemon.cancel_proc(*id))
        .collect();
    while asked.iter().any(|id| daemon.proc_alive(*id)) {
        tokio::time::sleep(STOP_POLL).await;
    }
    for id in asked {
        if let Some(handle) = daemon.take_proc(id)
            && let Err(err) = handle.task.await
        {
            warn!(service = %id, %err, "the worker task failed");
        }
    }
}

#[cfg(test)]
mod tests;
