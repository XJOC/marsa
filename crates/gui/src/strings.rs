//! Every text the window shows, in one place (AGENTS.md section 9), so a
//! second language can be added later without touching the drawing code.
//! Errors are shown by their stable code, never by their English message.

use marsa_core::types::{ServiceAction, ServiceState};

use crate::model::Connection;

pub const APP_NAME: &str = "Marsa";
pub const WINDOW_SIZE: [f32; 2] = [900.0, 600.0];
pub const WINDOW_MIN_SIZE: [f32; 2] = [480.0, 320.0];

pub const SERVICES: &str = "Services";
pub const NO_SERVICES: &str = "No services.";
pub const COLUMN_SERVICE: &str = "Service";
pub const COLUMN_STATE: &str = "State";
pub const COLUMN_PID: &str = "PID";
pub const COLUMN_ACTIONS: &str = "Actions";
pub const LOG_TITLE: &str = "Caddy log";
pub const LOG_EMPTY: &str = "No log lines yet.";

/// The window's own code: its background thread could not start.
pub const BRIDGE_FAILED: &str = "bridge_failed";

pub fn status_line(connection: &Connection) -> String {
    match connection {
        Connection::Connecting => "Connecting…".to_owned(),
        Connection::Reconnecting => "Reconnecting…".to_owned(),
        Connection::Running(health) => format!(
            "Daemon: running (v{}, PID {})",
            health.app_version, health.pid
        ),
        Connection::ShuttingDown => "Daemon: shutting down".to_owned(),
        // The bridge is gone: nothing tries again.
        Connection::Failed(code) if code == BRIDGE_FAILED => error_text(code),
        Connection::Failed(code) => format!("{} Retrying…", error_text(code)),
    }
}

pub fn state_text(state: &ServiceState) -> String {
    match state {
        ServiceState::Stopped => "Stopped".to_owned(),
        ServiceState::Starting => "Starting".to_owned(),
        ServiceState::Running => "Running".to_owned(),
        ServiceState::Stopping => "Stopping".to_owned(),
        ServiceState::Failed { code } => format!("Failed: {}", error_text(code)),
    }
}

pub fn action_text(action: ServiceAction) -> &'static str {
    match action {
        ServiceAction::Start => "Start",
        ServiceAction::Stop => "Stop",
        ServiceAction::Restart => "Restart",
    }
}

/// The message for an error code, or a general one with the code for a code
/// this build does not know (a newer daemon may send one).
pub fn error_text(code: &str) -> String {
    error_message(code).map_or_else(|| format!("Unexpected error ({code})."), str::to_owned)
}

fn error_message(code: &str) -> Option<&'static str> {
    Some(match code {
        // `marsa_core::client::ClientError`.
        "daemon_not_running" | "daemon_file_missing" => "The daemon is not running.",
        "daemon_start_failed" => "Cannot start the daemon (marsad).",
        "daemon_stop_failed" => "The running daemon did not stop in time.",
        "daemon_unreachable" => "Cannot reach the daemon.",
        "daemon_api_too_new" => "The daemon is newer than this window. Update Marsa.",
        "daemon_bad_response" => "The daemon sent an answer this window cannot read.",
        "invalid_id" => "That service name is not valid.",
        "daemon_file_invalid" => "The daemon's run file cannot be read.",
        "daemon_file_insecure" => "The daemon's run file is readable by other users.",
        "daemon_file_write" => "Cannot write the daemon's run file.",
        "random_unavailable" => "The system random source failed.",
        // Codes the daemon answers with (`ErrorBody`).
        "unauthorized" | "origin_rejected" | "host_rejected" => "The daemon refused the request.",
        "not_found" | "method_not_allowed" => "This daemon does not support that request.",
        "invalid_path" | "invalid_query" => "The daemon could not read that request.",
        "service_not_found" => "No such service.",
        "unknown_action" => "Unknown service action.",
        "service_busy" => "The service is busy with another action. Try again.",
        // Codes that come as an answer or in `ServiceState::Failed`.
        "caddy_not_installed" => "Caddy is not installed.",
        "caddy_config_write" => "Cannot write the Caddy config file.",
        "caddy_start_timeout" => "Caddy did not become ready in time.",
        "port_in_use" => "A port it needs is used by another program.",
        "caddy_orphan_stuck" => "An old Caddy from a crashed daemon could not be stopped.",
        "caddy_other_marsa" => "Another Marsa's Caddy uses the ports it needs.",
        "exited" => "The process exited.",
        "spawn_failed" => "The process could not be started.",
        "program_not_absolute" => "The service program has no full path.",
        BRIDGE_FAILED => "The window could not start its background worker.",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;

    use marsa_core::client::ClientError;
    use marsa_core::platform::daemon_file::DaemonFileError;

    use super::*;

    #[test]
    fn every_client_error_code_has_a_message() {
        let path = PathBuf::from("daemon.json");
        let io_error = || io::Error::other("test");
        let errors = [
            ClientError::NotRunning,
            ClientError::StartFailed {
                reason: String::new(),
                source: None,
            },
            ClientError::StopFailed {
                reason: String::new(),
            },
            ClientError::Unreachable {
                port: 1,
                source: Box::new(io_error()),
            },
            ClientError::ApiTooNew { daemon: 2 },
            ClientError::BadResponse {
                reason: String::new(),
            },
            ClientError::InvalidId { id: "..".into() },
            ClientError::DaemonFile(DaemonFileError::Missing { path: path.clone() }),
            ClientError::DaemonFile(DaemonFileError::Invalid {
                path: path.clone(),
                reason: "test",
            }),
            ClientError::DaemonFile(DaemonFileError::Unreadable {
                path: path.clone(),
                source: io_error(),
            }),
            ClientError::DaemonFile(DaemonFileError::Insecure {
                path: path.clone(),
                mode: 0o644,
            }),
            ClientError::DaemonFile(DaemonFileError::Write {
                path,
                source: io_error(),
            }),
        ];
        let built = errors.iter().map(|err| err.code().to_owned());
        // `RandomUnavailable` holds a `getrandom::Error`, which a test cannot
        // build.
        for code in built.chain(["random_unavailable".to_owned()]) {
            assert!(error_message(&code).is_some(), "no message for {code}");
        }
    }

    #[test]
    fn known_daemon_codes_have_a_message() {
        // Auth (M3-03), routes (M3-06), services and Caddy (M3-07), and the
        // orphan check (M3-11).
        for code in [
            "unauthorized",
            "origin_rejected",
            "host_rejected",
            "not_found",
            "method_not_allowed",
            "invalid_path",
            "invalid_query",
            "service_not_found",
            "unknown_action",
            "service_busy",
            "caddy_not_installed",
            "caddy_config_write",
            "caddy_start_timeout",
            "port_in_use",
            "caddy_orphan_stuck",
            "caddy_other_marsa",
            "exited",
            "spawn_failed",
            "program_not_absolute",
            BRIDGE_FAILED,
        ] {
            assert!(error_message(code).is_some(), "no message for {code}");
        }
    }

    #[test]
    fn an_unknown_code_is_shown_with_its_name() {
        assert_eq!(error_text("new_code"), "Unexpected error (new_code).");
    }
}
