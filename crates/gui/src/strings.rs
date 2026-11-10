//! Every text the window shows, in one place (AGENTS.md section 9), so a
//! second language can be added later without touching the drawing code.
//! Errors are shown by their stable code, never by their English message.

use std::path::Path;

use marsa_core::types::{ServiceAction, ServiceState, Site, SiteKind};

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
/// Over the disabled buttons of a php-fpm pool.
pub const MANAGED_BY_SITES_HINT: &str =
    "Starts and stops with the sites that need its PHP version.";
pub const LOG_TITLE: &str = "Caddy log";
pub const LOG_EMPTY: &str = "No log lines yet.";

pub const SITES: &str = "Sites";
pub const NO_SITES: &str =
    "No sites yet. Park a folder of projects: each sub-folder becomes a site.";
pub const COLUMN_NAME: &str = "Name";
pub const COLUMN_URL: &str = "URL";
pub const COLUMN_HTTPS: &str = "HTTPS";
pub const COLUMN_KIND: &str = "Kind";
pub const COLUMN_PHP: &str = "PHP";
pub const COLUMN_FOLDER: &str = "Folder";
/// The PHP column of a site with no PHP version: no PHP is installed.
pub const NO_PHP: &str = "–";
pub const HTTPS_HINT: &str = "Serve this site over HTTPS";
pub const PARK_BUTTON: &str = "Park folder…";
pub const PARK_FIELD_HINT: &str = "Full path of a folder of projects";
pub const PARK_BUTTON_HINT: &str =
    "Every sub-folder of this folder becomes a site, named after it.";
pub const SKIPPED_TITLE: &str = "Skipped folders";

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

/// The kind column of the sites table.
pub fn kind_text(kind: SiteKind) -> String {
    match kind {
        SiteKind::Laravel => "Laravel".to_owned(),
        SiteKind::Php => "PHP".to_owned(),
        SiteKind::Static => "Static".to_owned(),
        SiteKind::NodeProxy { port } => format!("Proxy to port {port}"),
    }
}

/// The PHP column of the sites table: the version, or [`NO_PHP`].
pub fn php_text(site: &Site) -> String {
    site.php
        .as_ref()
        .map_or_else(|| NO_PHP.to_owned(), ToString::to_string)
}

/// Under the park field once the daemon accepted the folder.
pub fn parked(folder: &Path) -> String {
    format!("Parked {}.", folder.display())
}

/// Why a folder did not become a site, by its `SkippedFolder::code`. Shorter
/// than [`error_text`], which is for an action that failed.
pub fn skip_reason(code: &str) -> String {
    let reason = match code {
        "site_name_invalid" => "Its name gives no valid site name (a-z, 0-9, and -).",
        "site_path_not_utf8" => "Its path is not valid UTF-8.",
        "site_name_taken" => "Another folder has the same site name.",
        "site_read_dir" => "It cannot be read.",
        "site_not_a_folder" => "It is gone or is not a folder.",
        _ => return error_text(code),
    };
    reason.to_owned()
}

/// One line of the skipped folders list.
pub fn skipped_line(folder: &Path, code: &str) -> String {
    format!("{}: {}", folder.display(), skip_reason(code))
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
        "invalid_path" | "invalid_query" | "invalid_body" => {
            "The daemon could not read that request."
        }
        "service_not_found" => "No such service.",
        "unknown_action" => "Unknown service action.",
        "service_busy" => "The service is busy with another action. Try again.",
        // Codes that come as an answer or in `ServiceState::Failed`.
        "caddy_not_installed" => "Caddy is not installed.",
        "caddy_config_write" => "Cannot write the Caddy config file.",
        "caddy_start_timeout" => "Caddy did not become ready in time.",
        "port_in_use" => "A port it needs is used by another program.",
        "caddy_orphan_stuck" => {
            "An old Caddy from a crashed daemon holds the ports and could not be stopped safely."
        }
        "caddy_other_marsa" => "Another Marsa's Caddy uses the ports it needs.",
        "service_managed_by_sites" => {
            "This service starts and stops with the sites that need its PHP version."
        }
        "php_fpm_orphan_stuck" => {
            "An old php-fpm from a crashed daemon holds the socket or port and could not be \
             stopped safely."
        }
        "exited" => "The process exited.",
        "spawn_failed" => "The process could not be started.",
        "program_not_absolute" => "The service program has no full path.",
        "internal_error" => "The daemon hit a bug. Try again; if it repeats, report it.",
        // The sites routes (M4-10b), `SiteError`, and `ClientError::PathNotUtf8`.
        "site_not_found" => "No such site. It may have just been removed.",
        "site_name_invalid" => "That is not a valid site name.",
        "site_name_taken" => "Another folder already has that site name.",
        "site_not_a_folder" => "That path is not a folder.",
        "site_path_relative" => "That is not a full path. Type the whole path of the folder.",
        "site_path_not_utf8" => {
            "That folder's path is not valid UTF-8, so Marsa cannot serve it. Rename the folder."
        }
        "site_read_dir" => "Cannot read that folder. Check its permissions.",
        // `ConfigError`, for the sites settings file.
        "config_read" => "Cannot read the Marsa settings file.",
        "config_invalid" => "The Marsa settings file has an error. Fix the file, then try again.",
        "config_write" => "Cannot write the Marsa settings file.",
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
            ClientError::PathNotUtf8 { path: path.clone() },
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
        // Auth (M3-03), routes (M3-06), services and Caddy (M3-07), the
        // orphan check (M3-11), sites (M4-10b), and php-fpm pools (M4-23).
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
            "service_managed_by_sites",
            "php_fpm_orphan_stuck",
            "exited",
            "spawn_failed",
            "program_not_absolute",
            "invalid_body",
            "internal_error",
            // Sites (M4-10b) and the settings files.
            "site_not_found",
            "site_name_invalid",
            "site_name_taken",
            "site_not_a_folder",
            "site_path_relative",
            "site_path_not_utf8",
            "site_read_dir",
            "config_read",
            "config_invalid",
            "config_write",
            BRIDGE_FAILED,
        ] {
            assert!(error_message(code).is_some(), "no message for {code}");
        }
    }

    #[test]
    fn an_unknown_code_is_shown_with_its_name() {
        assert_eq!(error_text("new_code"), "Unexpected error (new_code).");
    }

    #[test]
    fn a_site_error_code_has_its_own_text() {
        assert_eq!(
            error_text("site_path_not_utf8"),
            "That folder's path is not valid UTF-8, so Marsa cannot serve it. Rename the folder."
        );
        assert_eq!(
            error_text("site_path_relative"),
            "That is not a full path. Type the whole path of the folder."
        );
    }

    #[test]
    fn a_skipped_folder_says_why() {
        let folder = Path::new("/code/متجر");
        assert_eq!(
            skipped_line(folder, "site_name_invalid"),
            "/code/متجر: Its name gives no valid site name (a-z, 0-9, and -)."
        );
        for code in [
            "site_path_not_utf8",
            "site_name_taken",
            "site_read_dir",
            "site_not_a_folder",
        ] {
            assert!(!skip_reason(code).contains(code), "no reason for {code}");
        }
        // A code from a newer daemon still shows, with its name.
        assert_eq!(skip_reason("site_new"), "Unexpected error (site_new).");
    }

    #[test]
    fn the_kind_and_php_columns() {
        assert_eq!(kind_text(SiteKind::Laravel), "Laravel");
        assert_eq!(
            kind_text(SiteKind::NodeProxy { port: 5173 }),
            "Proxy to port 5173"
        );
        let mut site = Site {
            name: "shop".into(),
            root: "/code/shop".into(),
            docroot: "/code/shop/public".into(),
            kind: SiteKind::Laravel,
            php: None,
            secure: true,
        };
        assert_eq!(php_text(&site), NO_PHP);
        site.php = Some("8.4.23".parse().unwrap());
        assert_eq!(php_text(&site), "8.4.23");
    }
}
