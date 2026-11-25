//! `marsa queue` and `marsa schedule`.

use marsa_core::types::WorkerKind;

/// The column titles of the workers table.
pub const WORKERS_HEADER: [&str; 5] = ["SITE", "SETTING", "STATE", "PID", "RESTARTS"];
/// The setting column.
pub const WORKER_SETTING_ON: &str = "on";
pub const WORKER_SETTING_OFF: &str = "off";
/// The state, PID, and restarts columns of a worker that does not run.
pub const NO_WORKER: &str = "-";
/// The state column of a worker that is on and that the daemon has not
/// listed yet: it starts it on its next pass.
pub const WORKER_STARTING: &str = "starting";

/// The name of a worker at the start of a sentence.
pub fn worker_name(kind: WorkerKind) -> &'static str {
    match kind {
        WorkerKind::Queue => "Queue worker",
        WorkerKind::Schedule => "Scheduler",
    }
}

/// `marsa queue on <site>`: the setting is saved; `state` is the worker's
/// state column.
pub fn worker_turned_on(kind: WorkerKind, site: &str, state: &str) -> String {
    format!(
        "{} is on for {site}: {state}. Run \"marsa {} status {site}\" to see it.",
        worker_name(kind),
        kind.prefix()
    )
}

/// `marsa queue off <site>`.
pub fn worker_turned_off(kind: WorkerKind, site: &str) -> String {
    format!("{} is off for {site}.", worker_name(kind))
}

/// `marsa queue status` with no Laravel site.
pub fn no_laravel_sites(kind: WorkerKind) -> String {
    format!(
        "No Laravel sites yet. The {} runs for a Laravel project only.",
        match kind {
            WorkerKind::Queue => "queue worker",
            WorkerKind::Schedule => "scheduler",
        }
    )
}
