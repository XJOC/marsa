//! `marsa daemon`, `services`, and `logs`.

use marsa_core::types::ServiceState;

// `marsa daemon`.

/// `marsa daemon start` and `marsa daemon status`. Never the token.
pub fn daemon_running(version: &str, pid: u32, port: u16) -> String {
    format!("marsad is running: version {version}, PID {pid}, port {port}.")
}

pub const DAEMON_NOT_RUNNING: &str = "marsad is not running.";
pub const DAEMON_STOPPED: &str = "Stopped marsad.";

// `marsa services`.

/// The column titles of the services table.
pub const SERVICES_HEADER: [&str; 4] = ["SERVICE", "STATE", "PID", "RESTARTS"];
/// The PID column of a service that does not run.
pub const NO_PID: &str = "-";
pub const NO_SERVICES: &str = "The daemon manages no services.";

/// The state column of the services table.
pub fn state_name(state: &ServiceState) -> &'static str {
    match state {
        ServiceState::Stopped => "stopped",
        ServiceState::Starting => "starting",
        ServiceState::Running => "running",
        ServiceState::Stopping => "stopping",
        ServiceState::Failed { .. } => "failed",
    }
}

/// The lines under the table while MySQL runs: how a Laravel `.env`
/// connects to it, over TCP, as `root@127.0.0.1`, which has no
/// administrative privilege (ADR 0023, "Access").
pub fn mysql_connection(port: u16) -> String {
    sql_connection("mysql", "mysql", port)
}

/// The lines under the table while Valkey runs: how Laravel's Redis cache
/// and queue reach it. No user and no password.
pub fn valkey_connection(port: u16) -> String {
    format!(
        "valkey: 127.0.0.1:{port}, no password. For a Laravel .env:\n\
         REDIS_HOST=127.0.0.1\n\
         REDIS_PORT={port}"
    )
}

/// The lines under the table while PostgreSQL runs: how a Laravel `.env`
/// connects to it. `root` is not a superuser (ADR 0023, "Access").
pub fn postgres_connection(port: u16) -> String {
    sql_connection("postgres", "pgsql", port)
}

/// A summary line, then the five lines of a Laravel `.env`, each ready to
/// copy as it is. `driver` is Laravel's `DB_CONNECTION`.
fn sql_connection(id: &str, driver: &str, port: u16) -> String {
    format!(
        "{id}: 127.0.0.1:{port}, user root, no password. For a Laravel .env:\n\
         DB_CONNECTION={driver}\n\
         DB_HOST=127.0.0.1\n\
         DB_PORT={port}\n\
         DB_USERNAME=root\n\
         DB_PASSWORD="
    )
}

/// The line under the table for a failed service: why it failed.
pub fn service_failed(id: &str, message: &str, code: &str) -> String {
    format!("{id} failed: {message} [{code}]")
}

// `marsa logs`.

pub fn no_log_lines(id: &str) -> String {
    format!("No lines from {id} yet.")
}

/// On stderr once `-f` follows the log.
pub fn following(id: &str) -> String {
    format!("Showing new lines from {id} as they come. Press Ctrl+C to stop.")
}

/// On stderr when the daemon dropped lines. `0`: it gave no count.
pub fn lagged(count: u64) -> String {
    match count {
        0 => "Some lines were skipped: they came faster than this command could show them."
            .to_owned(),
        1 => "1 line was skipped: it came faster than this command could show it.".to_owned(),
        n => format!("{n} lines were skipped: they came faster than this command could show them."),
    }
}

/// On stderr when the daemon closes a followed log, for example when it stops.
pub const LOG_ENDED: &str = "The daemon closed the log.";
