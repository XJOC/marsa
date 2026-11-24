//! A Laravel site's workers: `php artisan queue:work` and `php artisan
//! schedule:work`, as `marsad` runs them. Pure: the daemon resolves the
//! paths and runs the spec under its supervisor.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use crate::services::{RestartPolicy, ServiceSpec, StopMethod};
use crate::types::{ServiceId, SiteKey, WorkerKind};

/// After a crash: 1 s, then twice as long each time up to 60 s, 8 times in a
/// row, then `Failed`. A queue fails for passing reasons (its database not
/// up yet after a boot), so more tries than `supervisord`'s 3. A run of
/// 60 s resets the count (the supervisor's `STABLE_RUN`), and a clean exit
/// after a few seconds (`queue:restart` after a deploy) is not counted
/// ([`RestartPolicy::Always`]).
pub const RESTART: RestartPolicy = RestartPolicy::Always {
    max_retries: 8,
    initial: Duration::from_secs(1),
    max: Duration::from_secs(60),
};

/// How long `queue:work` has to end its job after SIGTERM, before a kill.
/// Laravel's own `--timeout` default is 60 s; 30 s keeps the daemon's stop
/// short and is enough for most jobs.
pub const QUEUE_GRACE: Duration = Duration::from_secs(30);

/// How long `schedule:work` has after SIGTERM: it only waits for the next
/// minute.
pub const SCHEDULE_GRACE: Duration = Duration::from_secs(5);

/// The paths a worker needs, resolved by the daemon.
#[derive(Debug, Clone, Copy)]
pub struct WorkerPaths<'a> {
    /// The PHP CLI of the version the site runs: absolute.
    pub php: &'a Path,
    /// Marsa's `php.ini` of that PHP line (`php/<minor>/php.ini`).
    pub php_ini: &'a Path,
    /// The site's folder, where `artisan` is.
    pub root: &'a Path,
}

/// How the supervisor runs worker `kind` of `site`: `<php> -c <php.ini>
/// artisan queue:work` in the site's folder. No shell, no extra arguments:
/// the project's `config/queue.php` decides the rest. The environment is the
/// daemon's: Laravel reads the project's own settings itself.
pub fn worker_spec(kind: WorkerKind, site: SiteKey, paths: WorkerPaths<'_>) -> ServiceSpec {
    // `to_string_lossy` as in the pools: the app data dir comes from the OS
    // and is valid Unicode in practice.
    let php_ini = paths.php_ini.to_string_lossy().into_owned();
    ServiceSpec {
        id: ServiceId::Worker(kind, site),
        program: paths.php.to_owned(),
        args: vec![
            "-c".to_owned(),
            php_ini,
            "artisan".to_owned(),
            kind.command().to_owned(),
        ],
        env: BTreeMap::new(),
        env_remove: Vec::new(),
        cwd: Some(paths.root.to_owned()),
        restart: RESTART,
        stop: StopMethod::term(match kind {
            WorkerKind::Queue => QUEUE_GRACE,
            WorkerKind::Schedule => SCHEDULE_GRACE,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::restart_delay;
    use std::path::PathBuf;

    fn spec(kind: WorkerKind) -> ServiceSpec {
        let php = PathBuf::from("/data/runtimes/php/8.4.1/bin/php");
        let ini = PathBuf::from("/data/php/8.4/php.ini");
        let root = PathBuf::from("/code/shop");
        let paths = WorkerPaths {
            php: &php,
            php_ini: &ini,
            root: &root,
        };
        worker_spec(kind, SiteKey::new("shop").unwrap(), paths)
    }

    #[test]
    fn a_queue_worker_runs_artisan_in_the_site_with_marsas_php_ini() {
        let spec = spec(WorkerKind::Queue);
        assert_eq!(spec.id.to_string(), "queue-shop");
        assert_eq!(spec.program, Path::new("/data/runtimes/php/8.4.1/bin/php"));
        assert_eq!(
            spec.args,
            ["-c", "/data/php/8.4/php.ini", "artisan", "queue:work"]
        );
        assert_eq!(spec.cwd.as_deref(), Some(Path::new("/code/shop")));
        assert!(spec.env.is_empty() && spec.env_remove.is_empty());
        assert_eq!(spec.stop, StopMethod::term(QUEUE_GRACE));
        assert_eq!(spec.restart, RESTART);
    }

    #[test]
    fn a_schedule_worker_runs_schedule_work_and_stops_sooner() {
        let spec = spec(WorkerKind::Schedule);
        assert_eq!(spec.id.to_string(), "schedule-shop");
        assert_eq!(spec.args[3], "schedule:work");
        assert_eq!(spec.stop, StopMethod::term(SCHEDULE_GRACE));
    }

    #[test]
    fn restarts_wait_longer_each_time_and_end_after_eight() {
        let delays: Vec<_> = (0..9)
            .map(|attempt| restart_delay(RESTART, attempt))
            .collect();
        let secs = |s| Some(Duration::from_secs(s));
        assert_eq!(
            delays,
            [
                secs(1),
                secs(2),
                secs(4),
                secs(8),
                secs(16),
                secs(32),
                secs(60),
                secs(60),
                None
            ]
        );
    }
}
