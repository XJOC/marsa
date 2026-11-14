//! Every text the window shows, in one place (AGENTS.md section 9), so a
//! second language can be added later without touching the drawing code.
//! Errors are shown by their stable code, never by their English message.

use std::path::Path;

use marsa_core::runtimes::Runtime;
use marsa_core::types::{ServiceAction, ServiceState, Site, SiteKind, Source};
use semver::Version;

use crate::model::{Choice, Connection};
use crate::tray::menu::Daemon;

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
/// The PHP column of a site with no PHP version: no PHP is installed, or
/// none matches what the site asks for.
pub const NO_PHP: &str = "–";
/// The first entry of a site's PHP list when the window cannot tell which
/// version clearing the site's setting gives.
pub const AUTO_PHP: &str = "Default";
pub const AUTO_PHP_HINT: &str = "No setting for this site: its .php-version file, its \
     composer.json, or the default PHP picks the version.";
pub const HTTPS_HINT: &str = "Serve this site over HTTPS";
pub const PARK_BUTTON: &str = "Park folder…";
pub const PARK_FIELD_HINT: &str = "Full path of a folder of projects";
pub const PARK_BUTTON_HINT: &str =
    "Every sub-folder of this folder becomes a site, named after it.";
pub const SKIPPED_TITLE: &str = "Skipped folders";

pub const RUNTIMES: &str = "Runtimes";
pub const NO_RUNTIMES: &str = "No runtimes listed yet.";
pub const COLUMN_RUNTIME: &str = "Runtime";
pub const COLUMN_INSTALLED: &str = "Installed";
pub const COLUMN_DEFAULT: &str = "Default";
pub const NONE_INSTALLED: &str = "None installed";
/// The default column of a runtime with a default but none set.
pub const NO_DEFAULT: &str = "Not set";
pub const DEFAULT_HINT: &str = "Used where nothing else picks a version: no site setting, \
     no version file, and no composer.json or package.json.";
/// The default column of Composer and Caddy.
pub const NEWEST: &str = "Newest installed";
pub const NEWEST_HINT: &str =
    "This program has no default version: the newest installed one is used.";

/// The window's own code: its background thread could not start.
pub const BRIDGE_FAILED: &str = "bridge_failed";

// The menu-bar icon (`marsa-tray`, task M5-04).

/// The tooltip of the icon.
pub const TRAY_TOOLTIP: &str = "Marsa";
pub const TRAY_OPEN: &str = "Open Marsa";
/// The PHP submenu: the installed versions, the default checked.
pub const TRAY_PHP: &str = "PHP";
pub const TRAY_AUTOSTART: &str = "Start at Login";
/// A login file is there, but it starts another `marsa-tray` (the app
/// moved): a click writes it again.
pub const TRAY_AUTOSTART_STALE: &str = "Start at Login (outdated, click to fix)";
pub const TRAY_STOP: &str = "Stop Marsa";
pub const TRAY_START: &str = "Start Marsa";
/// Closes the icon only; the daemon and the sites keep running.
pub const TRAY_QUIT: &str = "Quit";
/// The icon's own code: its background thread could not start.
pub const TRAY_WORKER_FAILED: &str = "tray_worker_failed";
/// The icon's own code: `marsa-gui` could not be started.
pub const WINDOW_START_FAILED: &str = "window_start_failed";

/// The first line of the menu: the daemon's state.
pub fn tray_status(daemon: &Daemon) -> String {
    match daemon {
        Daemon::Starting => "Starting Marsa…".to_owned(),
        Daemon::Running => "Marsa is running".to_owned(),
        Daemon::Stopping => "Stopping Marsa…".to_owned(),
        Daemon::Stopped => "Marsa is stopped".to_owned(),
        Daemon::Failed(code) => error_text(code),
    }
}

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

/// The first entry of a site's PHP list: what the site runs with no setting
/// of its own. `Default (8.4.23)`, `From .php-version (8.3.27)`, or `From
/// composer.json (8.3.27)` by where its version came from; [`AUTO_PHP`]
/// when the site has its own setting, so the window cannot tell.
pub fn auto_php(site: &Site) -> String {
    match (&site.php_source, &site.php) {
        (Some(Source::GlobalDefault), Some(version)) => format!("Default ({version})"),
        (Some(Source::VersionFile(path) | Source::Constraint(path)), Some(version)) => {
            format!("From {} ({version})", file_name(path))
        }
        _ => AUTO_PHP.to_owned(),
    }
}

/// The closed PHP list of a site: the entry shown, or [`NO_PHP`] when the
/// site has no PHP version.
pub fn php_choice_text(choice: &Choice, site: &Site) -> String {
    match choice {
        Choice::Version(version) => version.to_string(),
        Choice::Auto if site.php.is_none() => NO_PHP.to_owned(),
        Choice::Auto => auto_php(site),
    }
}

/// Over the closed PHP list of a site: what the list does, and the file its
/// version came from, if any.
pub fn site_php_hint(site: &Site) -> String {
    let what = "PHP for this site. A version picked here wins over the project's \
                .php-version and composer.json.";
    match &site.php_source {
        Some(Source::VersionFile(path) | Source::Constraint(path)) => {
            format!("{what}\nNow from {}.", path.display())
        }
        _ => what.to_owned(),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The name column of the Runtimes section, as the CLI writes it.
pub fn runtime_name(runtime: Runtime) -> &'static str {
    match runtime {
        Runtime::Php => "PHP",
        Runtime::Node => "Node.js",
        Runtime::Composer => "Composer",
        Runtime::Caddy => "Caddy",
    }
}

/// The installed column of the Runtimes section: `8.3.27, 8.4.23`.
pub fn installed_text(versions: &[Version]) -> String {
    if versions.is_empty() {
        return NONE_INSTALLED.to_owned();
    }
    let texts: Vec<String> = versions.iter().map(ToString::to_string).collect();
    texts.join(", ")
}

/// The closed default list of PHP or Node: the version the default picks,
/// the saved text when it picks none, or [`NO_DEFAULT`].
pub fn default_text(chosen: Option<&Version>, saved: Option<&str>) -> String {
    match (chosen, saved) {
        (Some(version), _) => version.to_string(),
        (None, Some(text)) => format!("{text} (not installed)"),
        (None, None) => NO_DEFAULT.to_owned(),
    }
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
        // `PUT /v1/runtimes/{tool}/default` (M5-01), `GET /v1/runtimes`, and
        // the PHP version of a site (M5-02).
        "version_not_installed" => "That version is not installed. Install it first.",
        "no_default_version" => "This program has no default version: the newest one is used.",
        "invalid_version" => "That is not a valid version number.",
        "unsupported_alias" => "Version names such as \"latest\" are not supported. Pick a number.",
        "unknown_tool" => "This daemon does not know that program.",
        "runtimes_read" => "Cannot read the folder of installed versions.",
        // `GET` and `PUT /v1/autostart` (M5-03), behind the tray's "Start at
        // Login" line (M5-04).
        "tray_not_found" => {
            "marsa-tray was not found next to marsad, so Marsa cannot start at login."
        }
        "autostart_path_invalid" => "The path of marsa-tray cannot go into a login file.",
        "autostart_unsupported" => "Starting at login is not supported on this system yet.",
        "autostart_no_home" => "The system gives no home folder for the login file.",
        "autostart_read" => "Cannot read the login file.",
        "autostart_write" => "Cannot write the login file.",
        BRIDGE_FAILED => "The window could not start its background worker.",
        TRAY_WORKER_FAILED => "The menu-bar icon could not start its background worker.",
        WINDOW_START_FAILED => "Cannot start the window (marsa-gui).",
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
        // orphan check (M3-11), sites (M4-10b), php-fpm pools (M4-23), the
        // default version (M5-01), and the runtimes list (M5-02).
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
            // The default version (M5-01) and the PHP lists (M5-02).
            "version_not_installed",
            "no_default_version",
            "invalid_version",
            "unsupported_alias",
            "unknown_tool",
            "runtimes_read",
            // Autostart (M5-03), shown by the tray (M5-04).
            "tray_not_found",
            "autostart_path_invalid",
            "autostart_unsupported",
            "autostart_no_home",
            "autostart_read",
            "autostart_write",
            BRIDGE_FAILED,
            TRAY_WORKER_FAILED,
            WINDOW_START_FAILED,
        ] {
            assert!(error_message(code).is_some(), "no message for {code}");
        }
    }

    #[test]
    fn every_autostart_error_code_has_a_message() {
        use marsa_core::platform::autostart::AutostartError;

        let path = PathBuf::from("login.plist");
        let errors = [
            AutostartError::InvalidPath {
                path: path.clone(),
                reason: "test",
            },
            AutostartError::Read {
                path: path.clone(),
                source: io::Error::other("test"),
            },
            AutostartError::Write {
                path,
                source: io::Error::other("test"),
            },
        ];
        for err in &errors {
            assert!(
                error_message(err.code()).is_some(),
                "no message for {}",
                err.code()
            );
        }
    }

    #[test]
    fn the_tray_status_line() {
        assert_eq!(tray_status(&Daemon::Starting), "Starting Marsa…");
        assert_eq!(tray_status(&Daemon::Running), "Marsa is running");
        assert_eq!(tray_status(&Daemon::Stopping), "Stopping Marsa…");
        assert_eq!(tray_status(&Daemon::Stopped), "Marsa is stopped");
        assert_eq!(
            tray_status(&Daemon::Failed("daemon_start_failed".into())),
            "Cannot start the daemon (marsad)."
        );
        assert_eq!(
            tray_status(&Daemon::Failed("new_code".into())),
            "Unexpected error (new_code)."
        );
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

    fn v(text: &str) -> Version {
        text.parse().unwrap()
    }

    fn shop(php: Option<&str>, source: Option<Source>) -> Site {
        Site {
            name: "shop".into(),
            root: "/code/shop".into(),
            docroot: "/code/shop/public".into(),
            kind: SiteKind::Laravel,
            php: php.map(v),
            php_source: source,
            secure: true,
        }
    }

    #[test]
    fn the_kind_column() {
        assert_eq!(kind_text(SiteKind::Laravel), "Laravel");
        assert_eq!(
            kind_text(SiteKind::NodeProxy { port: 5173 }),
            "Proxy to port 5173"
        );
    }

    #[test]
    fn the_first_php_entry_says_where_the_version_comes_from() {
        let file = PathBuf::from("/code/shop/.php-version");
        let composer = PathBuf::from("/code/shop/composer.json");
        let cases = [
            (Some(Source::GlobalDefault), "Default (8.4.23)"),
            (
                Some(Source::VersionFile(file)),
                "From .php-version (8.4.23)",
            ),
            (
                Some(Source::Constraint(composer)),
                "From composer.json (8.4.23)",
            ),
            // The site's own setting: clearing it may give any of the above.
            (Some(Source::ProjectSetting), "Default"),
        ];
        for (source, want) in cases {
            let site = shop(Some("8.4.23"), source);
            assert_eq!(auto_php(&site), want);
            assert_eq!(php_choice_text(&Choice::Auto, &site), want);
        }
        let pinned = shop(Some("8.3.27"), Some(Source::ProjectSetting));
        assert_eq!(
            php_choice_text(&Choice::Version(v("8.3.27")), &pinned),
            "8.3.27"
        );
        // No PHP version at all.
        assert_eq!(php_choice_text(&Choice::Auto, &shop(None, None)), NO_PHP);
    }

    #[test]
    fn the_php_hint_names_the_file() {
        let file = Source::VersionFile(PathBuf::from("/code/shop/.php-version"));
        let hint = site_php_hint(&shop(Some("8.4.23"), Some(file)));
        assert!(
            hint.ends_with("\nNow from /code/shop/.php-version."),
            "{hint}"
        );
        let hint = site_php_hint(&shop(Some("8.4.23"), Some(Source::GlobalDefault)));
        assert!(!hint.contains("Now from"), "{hint}");
    }

    #[test]
    fn the_runtimes_columns() {
        assert_eq!(runtime_name(Runtime::Node), "Node.js");
        assert_eq!(installed_text(&[]), NONE_INSTALLED);
        assert_eq!(
            installed_text(&[v("8.3.27"), v("8.4.23")]),
            "8.3.27, 8.4.23"
        );
        assert_eq!(default_text(Some(&v("8.4.23")), Some("8.4")), "8.4.23");
        assert_eq!(default_text(None, Some("8.2")), "8.2 (not installed)");
        assert_eq!(default_text(None, None), NO_DEFAULT);
    }
}
