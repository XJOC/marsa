//! Every text the `marsa` command shows the user, in one place (AGENTS.md
//! section 9), so a second language can be added later without a rewrite.
//!
//! Errors: [`message`] maps an error `code()` to a sentence. The error's own
//! `Display` text goes to the log only. To add a code, add one arm to
//! `known` and the code to the list in the tests.

use std::path::Path;

use marsa_core::runtimes::Runtime;
use marsa_core::types::ServiceState;

/// The facts a message may show. Fill in what the error has; a message whose
/// fact is missing reads without it.
#[derive(Debug, Default, Clone, Copy)]
pub struct Details<'a> {
    pub runtime: Option<Runtime>,
    /// The version text the user asked for.
    pub requested: Option<&'a str>,
    /// Versions the user could ask for instead.
    pub available: &'a [String],
    /// A platform key, such as `darwin-arm64`, or "this system".
    pub platform: Option<&'a str>,
    pub path: Option<&'a Path>,
    pub url: Option<&'a str>,
    /// The code of the error under this one, such as the last mirror's error
    /// under `download_failed`.
    pub cause: Option<&'a str>,
    /// What a program printed.
    pub output: Option<&'a str>,
    /// The id of the service the command is about, such as `caddy`.
    pub service: Option<&'a str>,
}

/// The sentence for `code`. An unknown code gets a general sentence; the
/// caller prints the code next to it.
pub fn message(code: &str, details: &Details<'_>) -> String {
    known(code, details).unwrap_or_else(|| {
        "Something went wrong. Run again with MARSA_LOG=debug for details.".to_owned()
    })
}

/// The sentence for `code`, or `None` if this file has none.
fn known(code: &str, d: &Details<'_>) -> Option<String> {
    let name = d.runtime.map_or("the program", runtime_name);
    let path = d
        .path
        .map_or("the data folder".to_owned(), |p| p.display().to_string());
    let url = |prefix: &str| d.url.map_or(String::new(), |u| format!("{prefix}{u}"));
    // A service by its id (`caddy`), and by its name in a sentence (`Caddy`).
    let id = d.service.unwrap_or("<service>");
    let service = d.service.map_or("the service", service_name);
    let service_first = d.service.map_or("The service", service_name);
    const REPORT: &str = "Nothing was installed. Report it.";
    const AGAIN: &str = "Nothing was installed. Try again later; if it repeats, report it.";
    const DISK: &str = "Check the free disk space and the folder's permissions.";
    const RESTART_DAEMON: &str =
        "Try again; if it repeats, run \"marsa daemon stop\", then try again.";
    const UPDATE: &str = "Update Marsa, or report it.";

    let text = match code {
        // Installer: `InstallError`.
        "install_unsupported_platform" => format!(
            "Marsa has no {name} build for {}.",
            d.platform.unwrap_or("this system")
        ),
        "install_busy" => "Another install is running. Wait for it to end, then try again.".into(),
        "install_no_such_version" => no_such_version(name, d.requested, d.available),
        "install_manifest_mismatch" => format!(
            "The version list and this build of Marsa disagree on the files of {name}. \
             Nothing was installed. Update Marsa, or report it."
        ),
        "install_unsafe_name" => {
            format!("The version list has a file name Marsa refuses. {REPORT}")
        }
        "install_check_failed" => match d.output.map(str::trim).filter(|o| !o.is_empty()) {
            Some(output) => {
                format!(
                    "The new {name} failed its test run, so it was not installed. It printed: {output}"
                )
            }
            None => format!("The new {name} failed its test run, so it was not installed."),
        },
        "install_missing_vc_runtime" => format!(
            "{name} needs the Microsoft Visual C++ Redistributable: \
             https://aka.ms/vs/17/release/vc_redist.x64.exe"
        ),
        "install_serial_corrupt" => format!(
            "{path} is damaged. Delete it, then try again: Marsa then accepts the current \
             version list."
        ),
        "install_io" => format!("Cannot write to {path}. {DISK}"),
        "install_interrupted" => "The Marsa daemon stopped before the install ended. Run \
             \"marsa runtimes\" to see what is installed, then try again."
            .into(),

        // The signed version list: `ManifestError`.
        "manifest_no_trusted_key" => "This build of Marsa has no trusted manifest key yet, so it \
             cannot check the version list. Nothing was installed."
            .into(),
        "manifest_bad_signature" => format!("The version list failed its signature check. {AGAIN}"),
        "manifest_too_large" => format!("The version list is larger than allowed. {AGAIN}"),
        "manifest_invalid" => format!("The version list cannot be read. {AGAIN}"),
        "manifest_schema_unsupported" => {
            "The version list uses a newer format. Update Marsa, then try again.".into()
        }
        "manifest_rollback" => format!(
            "The version list is older than one this computer already accepted, so it was \
             refused. {AGAIN}"
        ),

        // Downloads: `FetchError`.
        "download_failed" if d.cause == Some("download_http_status") => format!(
            "Download failed: the server answered with an error{}. Try again later.",
            url(" for ")
        ),
        "download_failed" => format!("Download failed{}. Check your connection.", url(": ")),
        "download_http_status" => format!(
            "The server answered with an error{}. Try again later.",
            url(" for ")
        ),
        "download_insecure_url" => format!(
            "Refused a download that is not https{}. {REPORT}",
            url(": ")
        ),
        "download_size_mismatch" => {
            format!("A downloaded file has the wrong size{}. {AGAIN}", url(": "))
        }
        "download_too_large" => format!("A download is larger than allowed{}. {REPORT}", url(": ")),
        "download_io" => format!("Cannot save a download to {path}. {DISK}"),
        "checksum_mismatch" => {
            "A downloaded file does not match its signed checksum. Nothing was installed.".into()
        }

        // Archives: `ExtractError`.
        "extract_unsafe_path"
        | "extract_unsafe_link"
        | "extract_unsupported_entry"
        | "extract_duplicate" => {
            format!("A downloaded archive has an entry Marsa refuses. {REPORT}")
        }
        "extract_too_large" => {
            format!("A downloaded archive unpacks to more than allowed. {REPORT}")
        }
        "extract_corrupt" => format!("A downloaded archive is damaged. {AGAIN}"),
        "extract_io" => format!("Cannot unpack a download to {path}. {DISK}"),

        // The version the user wrote: `BadRequest`.
        "unsupported_alias" => format!(
            "\"{}\" is a name, not a version. Write a version number, such as {}.",
            d.requested.unwrap_or_default(),
            example_version(d.runtime)
        ),
        "invalid_version" => format!(
            "\"{}\" is not a version. Write a version number, such as {}.",
            d.requested.unwrap_or_default(),
            example_version(d.runtime)
        ),

        // Built-in recipes: `RecipeError`. Tests check them, so a user should
        // never see these.
        "recipe_invalid"
        | "recipe_unknown_platform"
        | "recipe_bad_path"
        | "recipe_bad_template" => {
            format!("This build of Marsa has a broken install recipe for {name}. Report it.")
        }

        // Folders.
        "runtimes_read" => format!("Cannot read {path}. Check the folder's permissions."),
        "config_no_home" => "Cannot find your home folder, so Marsa does not know where to keep \
             its data."
            .into(),

        // The daemon client: `ClientError` and `DaemonFileError`.
        "daemon_not_running" | "daemon_file_missing" => {
            "The Marsa daemon is not running. Start it with \"marsa daemon start\".".into()
        }
        "daemon_start_failed" => {
            format!("Cannot start the Marsa daemon (marsad). Its log may say why: {path}")
        }
        "daemon_stop_failed" => "The Marsa daemon did not stop in time. Try again; if it \
             repeats, quit marsad in your system's process list."
            .into(),
        "daemon_unreachable" => format!("Cannot reach the Marsa daemon. {RESTART_DAEMON}"),
        "daemon_api_too_new" => {
            "The running Marsa daemon is newer than this marsa command. Update Marsa.".into()
        }
        "daemon_bad_response" => {
            format!("The Marsa daemon sent an answer this command cannot read. {UPDATE}")
        }
        "invalid_id" => format!("\"{id}\" is not a valid service name."),
        "daemon_file_invalid" => format!("Cannot read {path}. {RESTART_DAEMON}"),
        "daemon_file_insecure" => format!(
            "Other users can read {path}, so Marsa does not use it. Quit marsad in your \
             system's process list, then try again."
        ),
        "daemon_file_write" => format!("Cannot write {path}. {DISK}"),
        "random_unavailable" => {
            "The system's random number source failed, so the daemon cannot make its key. \
             Report it."
                .into()
        }

        // The daemon's own codes: its `ErrorBody`, and `Failed { code }` in
        // a service status.
        "service_not_found" => {
            format!("There is no service named \"{id}\". Run \"marsa services\" to see them.")
        }
        "unknown_action" => {
            "The daemon does not know this action. Use start, stop, or restart.".into()
        }
        "service_busy" => format!(
            "Another start, stop, or restart of {service} is running. Wait for it to end, \
             then try again."
        ),
        "caddy_not_installed" => format!(
            "Caddy is not installed: there is no program at {path}. Put the caddy program \
             there, then try again."
        ),
        "caddy_config_write" => format!("Cannot write the Caddy config file {path}. {DISK}"),
        "caddy_start_timeout" => "Caddy started but did not answer in time, so it was stopped. \
             Run \"marsa logs caddy\" to see why."
            .into(),
        "port_in_use" => format!(
            "A port {service} needs is used by another program. Run \"marsa logs {id}\" to \
             see which one."
        ),
        "exited" => {
            format!("{service_first} stopped by itself. Run \"marsa logs {id}\" to see why.")
        }
        "spawn_failed" => format!("Cannot run {service}. The daemon's log may say why: {path}"),
        "program_not_absolute" => {
            format!("Marsa refused to run {service}: its program has no full path. Report it.")
        }
        "unknown_tool" => format!("The daemon does not know this program. {UPDATE}"),
        "invalid_path" | "invalid_query" | "invalid_body" | "not_found" | "method_not_allowed" => {
            format!("The daemon did not understand the request of this command. {UPDATE}")
        }
        "unauthorized" => format!("The daemon refused the key of this command. {RESTART_DAEMON}"),
        "origin_rejected" | "host_rejected" => {
            "The daemon refused the request of this command. Report it.".into()
        }

        _ => return None,
    };
    Some(text)
}

fn no_such_version(name: &str, requested: Option<&str>, available: &[String]) -> String {
    let wanted = match requested {
        Some(text) => format!("No {name} version matches \"{text}\"."),
        None => format!("The version list has no stable {name} version."),
    };
    if available.is_empty() {
        format!("{wanted} The version list has no {name} versions.")
    } else {
        format!("{wanted} Available: {}", available.join(", "))
    }
}

fn example_version(runtime: Option<Runtime>) -> &'static str {
    match runtime {
        Some(Runtime::Node) => "22",
        Some(Runtime::Composer) => "2.8",
        Some(Runtime::Php) | None => "8.4",
    }
}

/// The name of a runtime in sentences.
pub fn runtime_name(runtime: Runtime) -> &'static str {
    match runtime {
        Runtime::Php => "PHP",
        Runtime::Node => "Node.js",
        Runtime::Composer => "Composer",
    }
}

/// The name of a service in sentences: `Caddy` for `caddy`, else the id.
pub fn service_name(id: &str) -> &str {
    match id {
        "caddy" => "Caddy",
        other => other,
    }
}

// `marsa install`: progress (stderr) and the result (stdout).

pub fn manifest_verified(serial: u64) -> String {
    format!("Verified the version list (serial {serial})")
}

pub fn installing(runtime: Runtime, version: &str, platform: &str) -> String {
    format!(
        "Installing {} {version} for {platform}",
        runtime_name(runtime)
    )
}

pub const CHECKSUM_OK: &str = "sha256 ok";
pub const UNPACKING: &str = "unpacking";

pub fn checking(runtime: Option<Runtime>, version: Option<&str>) -> String {
    match (runtime, version) {
        (Some(runtime), Some(version)) => {
            format!("Checking {} {version}...", runtime_name(runtime))
        }
        _ => "Checking the new program...".to_owned(),
    }
}

pub fn installed(runtime: Runtime, version: &str, dir: &Path) -> String {
    format!(
        "Installed {} {version} in {}",
        runtime_name(runtime),
        dir.display()
    )
}

pub fn already_installed(runtime: Runtime, version: &str) -> String {
    format!(
        "{} {version} is already installed. Use --force to reinstall.",
        runtime_name(runtime)
    )
}

pub const COMPOSER_TIP: &str = "Tip: run \"marsa install composer\" for Composer.";

// `marsa runtimes`.

pub const NONE_INSTALLED: &str = "none installed";

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every code a `marsa` command can meet. Core has no public list of its
    /// codes, so this one is kept by hand from the `code()` functions of
    /// `InstallError` (which reach `marsa install` in `failed` events),
    /// `ManifestError`, `FetchError`, `ExtractError`, `BadRequest`,
    /// `RecipeError`, `RuntimesError`, and `config::app_data_dir`.
    const CODES: &[&str] = &[
        "install_unsupported_platform",
        "install_busy",
        "install_no_such_version",
        "install_manifest_mismatch",
        "install_unsafe_name",
        "install_check_failed",
        "install_missing_vc_runtime",
        "install_serial_corrupt",
        "install_io",
        // `marsa install` itself: the daemon stopped during the install.
        "install_interrupted",
        "manifest_too_large",
        "manifest_bad_signature",
        "manifest_invalid",
        "manifest_schema_unsupported",
        "manifest_rollback",
        "manifest_no_trusted_key",
        "download_insecure_url",
        "download_http_status",
        "download_failed",
        "download_size_mismatch",
        "checksum_mismatch",
        "download_too_large",
        "download_io",
        "extract_unsafe_path",
        "extract_unsafe_link",
        "extract_unsupported_entry",
        "extract_duplicate",
        "extract_too_large",
        "extract_corrupt",
        "extract_io",
        "unsupported_alias",
        "invalid_version",
        "recipe_invalid",
        "recipe_unknown_platform",
        "recipe_bad_path",
        "recipe_bad_template",
        "runtimes_read",
        "config_no_home",
        // `ClientError::code()` and `DaemonFileError::code()`.
        "daemon_not_running",
        "daemon_start_failed",
        "daemon_stop_failed",
        "daemon_unreachable",
        "daemon_api_too_new",
        "daemon_bad_response",
        "invalid_id",
        "daemon_file_missing",
        "daemon_file_invalid",
        "daemon_file_insecure",
        "daemon_file_write",
        "random_unavailable",
        // The daemon's codes, passed on by `ClientError::Api` or found in
        // `ServiceState::Failed`: `services_api`, `caddy`, `supervisor`,
        // `runtimes_api`, `api`, and `auth` in `crates/app/src/daemon/`, and
        // `ProgramNotAbsolute` in core.
        "service_not_found",
        "unknown_action",
        "service_busy",
        "caddy_not_installed",
        "caddy_config_write",
        "caddy_start_timeout",
        "port_in_use",
        "exited",
        "spawn_failed",
        "program_not_absolute",
        "invalid_path",
        "invalid_query",
        "unknown_tool",
        "invalid_body",
        "not_found",
        "method_not_allowed",
        "unauthorized",
        "origin_rejected",
        "host_rejected",
    ];

    #[test]
    fn every_code_has_a_message() {
        let empty = Details::default();
        for code in CODES {
            let text = known(code, &empty).unwrap_or_else(|| panic!("no message for {code}"));
            assert!(!text.is_empty(), "{code}");
            assert!(!text.contains('{'), "{code}: {text}");
        }
    }

    /// One `ClientError` of each kind the client makes itself. `Api` passes
    /// the daemon's code on, and those are in `CODES`. The `match` has no `_`
    /// arm, so a new kind does not compile until it is added here.
    #[test]
    fn every_client_error_has_a_message() {
        use marsa_core::client::ClientError;
        use marsa_core::platform::daemon_file::DaemonFileError;
        use std::path::PathBuf;

        let io = || std::io::Error::other("x");
        let path = || PathBuf::from("/d/run/daemon.json");
        let errors = [
            ClientError::NotRunning,
            ClientError::StartFailed {
                reason: "x".into(),
                source: Some(io()),
            },
            ClientError::StopFailed { reason: "x".into() },
            ClientError::Unreachable {
                port: 1,
                source: Box::new(io()),
            },
            ClientError::ApiTooNew { daemon: 2 },
            ClientError::BadResponse { reason: "x".into() },
            ClientError::InvalidId { id: "..".into() },
            ClientError::Api {
                status: 409,
                code: "caddy_not_installed".into(),
                message: "x".into(),
            },
            ClientError::DaemonFile(DaemonFileError::Missing { path: path() }),
            ClientError::DaemonFile(DaemonFileError::Invalid {
                path: path(),
                reason: "x",
            }),
            ClientError::DaemonFile(DaemonFileError::Unreadable {
                path: path(),
                source: io(),
            }),
            ClientError::DaemonFile(DaemonFileError::Insecure {
                path: path(),
                mode: 0o644,
            }),
            ClientError::DaemonFile(DaemonFileError::Write {
                path: path(),
                source: io(),
            }),
        ];
        for err in &errors {
            match err {
                ClientError::NotRunning
                | ClientError::StartFailed { .. }
                | ClientError::StopFailed { .. }
                | ClientError::Unreachable { .. }
                | ClientError::ApiTooNew { .. }
                | ClientError::BadResponse { .. }
                | ClientError::InvalidId { .. }
                | ClientError::Api { .. }
                | ClientError::DaemonFile(_) => {}
            }
            let code = err.code();
            assert!(CODES.contains(&code), "{code} is missing from CODES");
            assert!(known(code, &Details::default()).is_some(), "{code}");
        }
    }

    #[test]
    fn daemon_messages_name_the_service() {
        let caddy = Details {
            service: Some("caddy"),
            ..Details::default()
        };
        assert_eq!(
            message("exited", &caddy),
            "Caddy stopped by itself. Run \"marsa logs caddy\" to see why."
        );
        let other = Details {
            service: Some("php-8.4"),
            ..Details::default()
        };
        assert_eq!(
            message("service_not_found", &other),
            "There is no service named \"php-8.4\". Run \"marsa services\" to see them."
        );
        assert_eq!(
            message("exited", &Details::default()),
            "The service stopped by itself. Run \"marsa logs <service>\" to see why."
        );
    }

    #[test]
    fn lag_notes_count_the_skipped_lines() {
        assert!(lagged(0).starts_with("Some lines were skipped"));
        assert!(lagged(1).starts_with("1 line was skipped"));
        assert!(lagged(7).starts_with("7 lines were skipped"));
    }

    #[test]
    fn an_unknown_code_gets_the_general_message() {
        assert!(known("nope", &Details::default()).is_none());
        assert!(message("nope", &Details::default()).starts_with("Something went wrong."));
    }

    #[test]
    fn no_such_version_lists_what_is_available() {
        let available = ["8.3.27".to_owned(), "8.4.23".to_owned()];
        let details = Details {
            runtime: Some(Runtime::Php),
            requested: Some("8.9"),
            available: &available,
            ..Details::default()
        };
        assert_eq!(
            message("install_no_such_version", &details),
            "No PHP version matches \"8.9\". Available: 8.3.27, 8.4.23"
        );
        let none = Details {
            runtime: Some(Runtime::Node),
            ..Details::default()
        };
        assert_eq!(
            message("install_no_such_version", &none),
            "The version list has no stable Node.js version. The version list has no \
             Node.js versions."
        );
    }

    #[test]
    fn messages_show_their_details() {
        let path = Path::new("/data/runtimes/.manifest-serial");
        let details = Details {
            path: Some(path),
            ..Details::default()
        };
        let text = message("install_serial_corrupt", &details);
        assert!(
            text.starts_with(&format!("{} is damaged. Delete it", path.display())),
            "{text}"
        );

        let download = Details {
            url: Some("https://example.com/php.tar.gz"),
            ..Details::default()
        };
        assert_eq!(
            message("download_failed", &download),
            "Download failed: https://example.com/php.tar.gz. Check your connection."
        );
        let refused = Details {
            cause: Some("download_http_status"),
            ..download
        };
        assert_eq!(
            message("download_failed", &refused),
            "Download failed: the server answered with an error for \
             https://example.com/php.tar.gz. Try again later."
        );

        let bad = Details {
            runtime: Some(Runtime::Node),
            requested: Some("lts/*"),
            ..Details::default()
        };
        assert_eq!(
            message("unsupported_alias", &bad),
            "\"lts/*\" is a name, not a version. Write a version number, such as 22."
        );
        assert!(
            message("manifest_no_trusted_key", &bad)
                .starts_with("This build of Marsa has no trusted manifest key yet")
        );
    }
}
