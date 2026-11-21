use std::io;
use std::path::{MAIN_SEPARATOR, Path, PathBuf};

use marsa_core::client::ClientError;
use marsa_core::platform::daemon_file::DaemonFileError;
use marsa_core::recipes::Platform;
use marsa_core::runtimes::Runtime;
use marsa_core::types::{
    InstallFailure, OpStatus, PortHolder, SetupReport, SiteKind, VersionInUse, XdebugState,
};
use semver::Version;

use crate::model::{
    Autostart, Connection, INSTALL_INTERRUPTED, InstallStep, InstallView, Pick, Removal,
    UninstallNotice,
};
use crate::theme::Section;
use crate::tray::menu::Daemon;

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
        ClientError::InUse {
            message: String::new(),
            in_use: Box::default(),
        },
        ClientError::InUse {
            message: String::new(),
            in_use: Box::new(VersionInUse {
                sites_left_without_php: vec!["blog".into()],
                ..VersionInUse::default()
            }),
        },
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

/// Every code a client can meet has its own sentence, not the general one.
#[test]
fn every_code_has_a_message() {
    for code in marsa_core::codes::ALL {
        assert!(error_message(code).is_some(), "no message for {code}");
    }
}

#[test]
fn every_uninstall_error_code_has_a_message() {
    use marsa_core::installer::uninstall::UninstallError;

    let path = PathBuf::from("runtimes/php/8.3.27");
    let errors = [
        UninstallError::Busy,
        UninstallError::NotInstalled { path: path.clone() },
        UninstallError::Io {
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
        "That is not a full path of a folder."
    );
}

#[test]
fn a_skipped_folder_says_why() {
    assert_eq!(
        skip_reason("site_name_invalid"),
        "Its name gives no valid site name (a-z, 0-9, and -)."
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

#[test]
fn the_kind_column() {
    assert_eq!(kind_text(SiteKind::Laravel), "Laravel");
    assert_eq!(
        kind_text(SiteKind::NodeProxy { port: 5173 }),
        "Proxy to port 5173"
    );
}

/// An absolute path from Unix-style parts (a Windows path needs a drive).
fn full(unix: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("C:{}", unix.replace('/', "\\")))
    } else {
        PathBuf::from(unix)
    }
}

/// `~` and the parts after it, with the separator of the system.
fn tilde(parts: &[&str]) -> String {
    let mut text = String::from("~");
    for part in parts {
        text.push(MAIN_SEPARATOR);
        text.push_str(part);
    }
    text
}

#[test]
fn a_folder_under_home_is_shown_with_a_tilde_and_any_other_is_left_alone() {
    let home = full("/Users/omar");
    let shown = |path: &str| home_path(&full(path), Some(&home));
    assert_eq!(shown("/Users/omar/Code/shop"), tilde(&["Code", "shop"]));
    assert_eq!(shown("/Users/omar/Code"), tilde(&["Code"]));
    assert_eq!(shown("/Users/omar"), "~", "the home folder itself");
    // A trailing separator and `.` parts do not change the match.
    assert_eq!(shown("/Users/omar/Code/"), tilde(&["Code"]));
    // Outside home: the full path, as `display` writes it.
    assert_eq!(
        shown("/srv/www/shop"),
        full("/srv/www/shop").display().to_string()
    );
    // Only a prefix of the text, not of the path parts.
    assert_eq!(
        shown("/Users/omarx/Code/shop"),
        full("/Users/omarx/Code/shop").display().to_string()
    );
    assert_eq!(
        shown("/Users/oma"),
        full("/Users/oma").display().to_string()
    );
}

#[test]
fn with_no_usable_home_every_path_is_shown_in_full() {
    let path = full("/Users/omar/Code/shop");
    let text = path.display().to_string();
    assert_eq!(home_path(&path, None), text);
    // Not a full path: it would match everything or nothing.
    assert_eq!(home_path(&path, Some(Path::new(""))), text);
    assert_eq!(home_path(&path, Some(Path::new("omar"))), text);
    // The root as home would turn every path into `~/…`.
    assert_eq!(home_path(&path, Some(&full("/"))), text);
}

#[test]
fn a_reason_names_the_file_the_version_came_from() {
    assert_eq!(
        reason_file(Path::new("/code/shop/.php-version")),
        "from .php-version"
    );
    assert_eq!(
        reason_file(Path::new("/code/shop/composer.json")),
        "from composer.json"
    );
    assert_eq!(php_for_site("shop"), "PHP version for shop");
    assert_eq!(no_site_matches("sh"), "No site matches “sh”.");
    assert_eq!(search_key_hint(true), "Cmd K");
    assert_eq!(search_key_hint(false), "Ctrl K");
}

#[test]
fn the_runtimes_screen_words() {
    assert_eq!(runtime_name(Runtime::Node), "Node.js");
    assert_eq!(default_heading("PHP"), "Default PHP");
    assert_eq!(summary_default("8.4", 4), "Default 8.4, 4 installed");
    assert_eq!(summary_no_default(2), "No default, 2 installed");
    assert_eq!(summary_newest(&v("2.8.4")), "2.8.4, newest is used");
    assert_eq!(no_runtime_installed("PHP"), NO_PHP_YET);
    assert_eq!(install_runtime("Caddy"), "Install Caddy…");
    assert_eq!(install_dialog_title("Node.js"), "Install a Node.js version");
    assert_eq!(more_for("PHP", &v("8.4.27")), "More for PHP 8.4.27");
    assert!(UNINSTALL_BUTTON.ends_with('…') && INSTALL_A_VERSION.ends_with('…'));
}

#[test]
fn the_used_by_cell_counts_the_sites_and_names_a_few() {
    assert_eq!(sites_text(&[]), "No sites");
    assert_eq!(sites_text(&["shop"]), "1 site: shop");
    assert_eq!(
        sites_text(&["legacy-crm", "wiki"]),
        "2 sites: legacy-crm, wiki"
    );
    assert_eq!(sites_text(&["a", "b", "c"]), "3 sites: a, b, c");
    assert_eq!(sites_text(&["a", "b", "c", "d"]), "4 sites");
}

#[test]
fn a_version_that_cannot_be_uninstalled_says_why() {
    assert_eq!(
        uninstall_off_default("PHP"),
        "It is the default PHP. Pick another default first."
    );
    assert_eq!(
        uninstall_off_sites("PHP", &["wiki"]),
        "The site wiki.test is set to it. Pick another PHP version for it first."
    );
    assert_eq!(
        uninstall_off_sites("PHP", &["legacy-crm", "wiki"]),
        "The sites legacy-crm.test, wiki.test are set to it. Pick another PHP version for them first."
    );
}

// -- Installs, setup, and settings --

use marsa_core::installer::InstallError;
use marsa_core::installer::manifest::ManifestError;
use marsa_core::resolver::BadRequest;
use marsa_core::types::{ApiError, OpResult, SetupSkip};

#[test]
fn every_install_and_setup_code_has_a_message() {
    let built = [
        InstallError::UnsupportedPlatform {
            what: "php".into(),
            platform: "x".into(),
        },
        InstallError::OsTooOld {
            what: "php".into(),
            needs: "macOS 15.0".into(),
            found: "macOS 14.6".into(),
        },
        InstallError::Busy,
        InstallError::NoSuchVersion {
            runtime: "php",
            requested: None,
            available: Vec::new(),
        },
        InstallError::ManifestMismatch {
            what: "php".into(),
            missing: Vec::new(),
            extra: Vec::new(),
        },
        InstallError::UnsafeName { name: "..".into() },
        InstallError::CheckFailed {
            output: String::new(),
            exit_code: None,
        },
        InstallError::MissingVcRuntime,
        InstallError::XzMissing {
            what: "mysql".into(),
        },
        InstallError::SerialCorrupt {
            path: PathBuf::from("s"),
        },
        InstallError::Io {
            path: PathBuf::from("s"),
            source: io::Error::other("test"),
        },
        InstallError::Manifest(ManifestError::NoTrustedKey),
        InstallError::Manifest(ManifestError::BadSignature),
        InstallError::Version(BadRequest::Alias),
        InstallError::Version(BadRequest::Invalid),
    ];
    let built = built.iter().map(|err| err.code().to_owned());
    // The codes of the errors under `InstallError` that a test cannot
    // build here, the daemon's own install codes, and setup's codes
    // (its route, its skipped steps, and `marsa-helper`).
    let listed = [
        "install_crashed",
        INSTALL_INTERRUPTED,
        "manifest_too_large",
        "manifest_invalid",
        "manifest_schema_unsupported",
        "manifest_rollback",
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
        "recipe_invalid",
        "recipe_unknown_platform",
        "recipe_bad_path",
        "recipe_bad_template",
        "runtimes_read",
        "unknown_tool",
        "setup_running",
        "setup_not_supported",
        "setup_cancelled",
        "setup_timed_out",
        "setup_interrupted",
        "setup_not_authorized",
        "setup_helper_missing",
        "setup_helper_unsafe",
        "setup_prompt_failed",
        "setup_helper_failed",
        "setup_helper_rejected",
        "setup_helper_not_the_service",
        "setup_helper_install_failed",
        "setup_helper_install_running",
        "setup_helper_pipe_failed",
        "setup_helper_busy",
        "setup_helper_no_answer",
        "redirect_port_held",
        "ports_not_checked",
        "ca_not_ready",
        "ca_unreadable",
        "caddy_not_started",
        "op_not_implemented",
        "op_not_supported",
        "resolver_foreign",
        "resolver_read",
        "resolver_write",
        "trust_cert_invalid",
        "trust_read",
        "trust_write",
        "trust_add_failed",
        "pf_write",
        "pf_load_failed",
        "setup_pkexec_missing",
        "setup_caddy_ports_failed",
        "resolved_foreign",
        "resolved_read",
        "resolved_write",
        "resolved_delete",
        "resolved_not_running",
        "resolved_tool_missing",
        "resolved_unit_failed",
        "resolved_check_failed",
        "resolved_use_hosts",
        "resolved_systemd_unknown",
        "hosts_read",
        "hosts_write",
        "hosts_block_broken",
        "hosts_sites_unknown",
        "hosts_too_many_names",
        "hosts_not_used",
        "setup_hosts_not_written",
        "setup_hosts_not_removed",
        "resolved_restart_failed",
        "resolved_stopped",
        "ports_foreign",
        "ports_read",
        "ports_write",
        "ports_delete",
        "ports_apply_failed",
        "ports_restore_failed",
        "trust_tool_missing",
        "nss_tool_missing",
        "nss_add_failed",
        "nss_db_refused",
        "trust_store_update_failed",
    ];
    for code in built.chain(listed.map(str::to_owned)) {
        assert!(error_message(&code).is_some(), "no message for {code}");
    }
}

fn view(runtime: Option<Runtime>, version: Option<&str>, step: InstallStep) -> InstallView {
    InstallView {
        runtime,
        version: version.map(str::to_owned),
        step,
    }
}

#[test]
fn each_install_step_has_its_line() {
    let php = |step| view(Some(Runtime::Php), Some("8.4.23"), step);
    let artifact = || "cli".to_owned();
    let cases = [
        (
            view(Some(Runtime::Php), Some("8.4"), InstallStep::Waiting),
            "Asking the daemon to install PHP 8.4…",
        ),
        (
            view(Some(Runtime::Caddy), None, InstallStep::ListChecked),
            "Installing Caddy…",
        ),
        (
            php(InstallStep::Downloading {
                artifact: artifact(),
                done: 12_340_000,
                total: 30_100_000,
            }),
            "Downloading PHP 8.4.23: 12.3 of 30.1 MB",
        ),
        (
            php(InstallStep::Unpacking {
                artifact: artifact(),
            }),
            "Unpacking PHP 8.4.23…",
        ),
        (php(InstallStep::Checking), "Testing PHP 8.4.23…"),
        (
            php(InstallStep::Finishing),
            "Finishing the install of PHP 8.4.23…",
        ),
        (
            php(InstallStep::Done { already: false }),
            "Installed PHP 8.4.23.",
        ),
        (
            php(InstallStep::Done { already: true }),
            "PHP 8.4.23 was already installed.",
        ),
        (
            view(None, None, InstallStep::Checking),
            "Testing a program…",
        ),
    ];
    for (view, want) in cases {
        assert_eq!(install_status(&view), want);
    }
}

#[test]
fn a_failed_install_says_why_with_its_facts() {
    let failed = |failure| {
        view(
            Some(Runtime::Php),
            Some("8.9"),
            InstallStep::Failed(failure),
        )
    };
    let no_match = InstallFailure {
        code: "install_no_such_version".into(),
        requested: Some("8.9".into()),
        available: vec!["8.3.27".into(), "8.4.23".into()],
        ..InstallFailure::default()
    };
    assert_eq!(
        install_status(&failed(no_match)),
        "Cannot install PHP 8.9. No PHP version matches \"8.9\". Available: 8.3.27, 8.4.23."
    );
    let check = InstallFailure {
        code: "install_check_failed".into(),
        output: Some(" dyld: missing library \n".into()),
        ..InstallFailure::default()
    };
    assert!(
        install_failure(Some(Runtime::Php), &check).ends_with("It printed: dyld: missing library")
    );
    let alias = InstallFailure {
        code: "unsupported_alias".into(),
        requested: Some("latest".into()),
        ..InstallFailure::default()
    };
    assert_eq!(
        install_failure(Some(Runtime::Node), &alias),
        "\"latest\" is not a version number. Write one such as 22."
    );
    let platform = InstallFailure {
        code: "install_unsupported_platform".into(),
        platform: Some("linux-arm64".into()),
        ..InstallFailure::default()
    };
    assert_eq!(
        install_failure(Some(Runtime::Caddy), &platform),
        "Marsa has no Caddy build for linux-arm64."
    );
    // An OS that is too old: the facts, in the words of the CLI.
    let old = InstallFailure {
        code: "install_os_too_old".into(),
        os: Some(Box::new(marsa_core::types::OsMismatch {
            needs: "macOS 15.0".into(),
            found: "macOS 14.4".into(),
        })),
        ..InstallFailure::default()
    };
    assert_eq!(
        install_failure(Some(Runtime::Php), &old),
        "PHP needs macOS 15.0 or newer, and this system has macOS 14.4. Nothing was \
         installed."
    );
    // Without the facts: the same sentence without them.
    let old_no_facts = InstallFailure {
        code: "install_os_too_old".into(),
        ..InstallFailure::default()
    };
    assert_eq!(
        install_failure(Some(Runtime::Php), &old_no_facts),
        "PHP needs a newer system than this one. Nothing was installed."
    );
    assert_eq!(
        install_failure(None, &old_no_facts),
        "the program needs a newer system than this one. Nothing was installed."
    );
    assert_eq!(
        install_status(&failed(old)),
        "Cannot install PHP 8.9. PHP needs macOS 15.0 or newer, and this system has macOS \
         14.4. Nothing was installed."
    );
    // The general text, for a place with no name to give.
    assert_eq!(
        error_text("install_os_too_old"),
        "This program needs a newer system than this one. Nothing was installed."
    );
    // A long list of versions, and a long printout, are cut short.
    let many = InstallFailure {
        code: "install_no_such_version".into(),
        requested: Some("9".into()),
        available: (1..=7).map(|n| format!("8.{n}.0")).collect(),
        ..InstallFailure::default()
    };
    assert!(
        install_failure(Some(Runtime::Php), &many)
            .ends_with("Available: 8.1.0, 8.2.0, 8.3.0, 8.4.0, and 3 more."),
        "{}",
        install_failure(Some(Runtime::Php), &many)
    );
    let noisy = InstallFailure {
        code: "install_check_failed".into(),
        output: Some(format!("\n  {}\nsecond line\n", "x".repeat(200))),
        ..InstallFailure::default()
    };
    let text = install_failure(Some(Runtime::Php), &noisy);
    assert!(
        text.ends_with(&format!("{}…", "x".repeat(MAX_PRINTED))),
        "{text}"
    );
    assert!(!text.contains("second line"));
    // Any other code: its general text; a new one shows its name.
    let busy = InstallFailure {
        code: "install_busy".into(),
        ..InstallFailure::default()
    };
    assert_eq!(install_failure(None, &busy), error_text("install_busy"));
    let new = InstallFailure {
        code: "install_new".into(),
        ..InstallFailure::default()
    };
    assert_eq!(
        install_failure(None, &new),
        "Unexpected error (install_new)."
    );
}

#[test]
fn the_first_run_button_names_the_php_it_installs() {
    let line = format!("PHP {}", crate::model::FIRST_PHP);
    assert_eq!(INSTALL_PHP, format!("Install {line}"));
    assert!(FIRST_RUN_INSTALL.contains(&line), "{FIRST_RUN_INSTALL}");
    assert_eq!(FIRST_RUN_INSTALL, format!("Install {line} and Caddy"));
}

/// ADR 0018: on Windows the prompt is UAC, the first time only.
#[test]
fn setup_on_windows_names_the_uac_prompt() {
    let windows = Some(Platform::WindowsX64);
    assert!(setup_explanation(windows).contains("(UAC)"));
    assert!(setup_running(windows).contains("UAC"));
    for other in [Some(Platform::DarwinArm64), Some(Platform::LinuxX64), None] {
        assert!(!setup_explanation(other).contains("UAC"));
        assert!(!setup_running(other).contains("UAC"));
    }
    let cancelled = error_text("setup_cancelled");
    let prompt = if cfg!(windows) {
        "UAC prompt"
    } else {
        "password prompt"
    };
    assert!(cancelled.contains(prompt), "{cancelled}");
    assert!(error_text("setup_not_supported").contains("Windows"));
}

#[test]
fn setup_says_macos_asks_for_the_password_once() {
    for platform in [Platform::DarwinArm64, Platform::DarwinX64] {
        let text = setup_explanation(Some(platform));
        assert!(text.contains("macOS asks for your password once"), "{text}");
        assert!(setup_running(Some(platform)).contains("macOS"));
    }
    for platform in [Some(Platform::LinuxX64), Some(Platform::WindowsX64), None] {
        assert!(!setup_explanation(platform).contains("macOS"));
        assert!(!setup_running(platform).contains("macOS"));
    }
}

/// The first-run step says what the user gets and the prompt, and on
/// Linux the cost of the low ports (ADR 0014); not how setup works.
#[test]
fn the_first_run_setup_step_is_short_and_names_the_prompt() {
    let mac = first_run_setup_body(Some(Platform::DarwinArm64));
    assert_eq!(
        mac,
        "Your browser will open .test sites over HTTPS. macOS asks for your password once."
    );
    let linux = first_run_setup_body(Some(Platform::LinuxX64));
    assert!(linux.contains("any program on this computer may use ports 80 to 1023"));
    assert!(first_run_setup_body(Some(Platform::WindowsX64)).contains("(UAC)"));
    for platform in [
        Some(Platform::DarwinX64),
        Some(Platform::LinuxArm64),
        Some(Platform::WindowsX64),
        None,
    ] {
        let text = first_run_setup_body(platform);
        assert!(text.starts_with("Your browser will open .test sites over HTTPS."));
        assert!(!text.contains("certificates"), "{text}");
    }
}

/// ADR 0014: on Linux the setup text says that ports 80 to 1023 become
/// open to every program, before the password prompt.
#[test]
fn setup_on_linux_names_the_cost_of_the_low_ports() {
    for platform in [Platform::LinuxX64, Platform::LinuxArm64] {
        let text = setup_explanation(Some(platform));
        assert!(text.contains("any program on this computer may use ports 80 to 1023"));
        assert!(setup_running(Some(platform)).contains("desktop"));
    }
    assert_eq!(
        setup_step("ports_install"),
        "Let Caddy use ports 80 and 443"
    );
    assert_eq!(setup_step("resolved_install"), "Send .test names to Marsa");
    assert_eq!(
        setup_step("hosts_sync"),
        "Put the sites' .test names in the hosts file"
    );
}

fn result(op: &str, status: OpStatus, code: Option<&str>) -> OpResult {
    OpResult {
        op: op.into(),
        status,
        code: code.map(str::to_owned),
        message: None,
    }
}

fn texts(lines: &[ReportLine]) -> Vec<(&str, bool)> {
    lines
        .iter()
        .map(|line| (line.text.as_str(), line.problem))
        .collect()
}

#[test]
fn a_setup_report_has_one_line_per_step() {
    let report = SetupReport {
        results: vec![
            result("resolver_install", OpStatus::Done, None),
            result("trust_install", OpStatus::Unchanged, None),
            result("pf_install", OpStatus::Failed, Some("pf_load_failed")),
            result("new_op", OpStatus::Done, None),
        ],
        skipped: vec![SetupSkip {
            op: "pf_install".into(),
            code: "redirect_port_held".into(),
        }],
        port_holders: vec![
            PortHolder {
                port: 80,
                address: "127.0.0.1".into(),
                process: Some("nginx-arm64".into()),
                pid: Some(15464),
                user: None,
                command: None,
                app: Some("Herd".into()),
            },
            PortHolder {
                port: 443,
                address: "*".into(),
                process: None,
                pid: None,
                user: None,
                command: None,
                app: None,
            },
        ],
        error: None,
    };
    let lines = setup_lines(&report);
    let failed = format!(
        "Route ports 80 and 443 to Marsa: failed. {}",
        error_text("pf_load_failed")
    );
    let skipped = format!(
        "Route ports 80 and 443 to Marsa: skipped. {}",
        error_text("redirect_port_held")
    );
    assert_eq!(
        texts(&lines),
        [
            ("Send .test names to Marsa: done.", false),
            ("Trust Marsa's HTTPS certificates: already done.", false),
            (failed.as_str(), true),
            ("new_op: done.", false),
            (skipped.as_str(), false),
            ("Port 80 is used by nginx-arm64 (PID 15464, Herd).", false),
            ("Port 443 is used by an unknown program.", false),
        ]
    );
}

/// the two reasons for no CA say what to do.
#[test]
fn a_skipped_trust_says_how_to_get_the_ca() {
    let skipped = |code: &str| SetupReport {
        skipped: vec![SetupSkip {
            op: "trust_install".into(),
            code: code.into(),
        }],
        ..SetupReport::default()
    };
    let no_caddy =
        format!("Trust Marsa's HTTPS certificates: skipped. {SETUP_CADDY_NOT_INSTALLED}");
    assert_eq!(
        texts(&setup_lines(&skipped("caddy_not_installed"))),
        [(no_caddy.as_str(), false)]
    );
    for code in ["ca_not_ready", "caddy_not_started"] {
        let line = &setup_lines(&skipped(code))[0];
        assert!(line.text.contains("Caddy log"), "{}", line.text);
        assert!(line.text.ends_with("run setup again."), "{}", line.text);
    }
    assert!(error_text("caddy_not_started").starts_with("Caddy did not start"));
}

#[test]
fn a_setup_that_did_not_run_says_why() {
    let cancelled = SetupReport {
        error: Some(ApiError {
            code: "setup_cancelled".into(),
            message: "the admin prompt was cancelled".into(),
        }),
        ..SetupReport::default()
    };
    let lines = setup_lines(&cancelled);
    assert_eq!(
        texts(&lines),
        [(error_text("setup_cancelled").as_str(), true)]
    );
    assert_eq!(
        texts(&setup_lines(&SetupReport::default())),
        [(SETUP_NOTHING, false)]
    );
}

#[test]
fn the_settings_lines() {
    let health = marsa_core::types::Health {
        app_version: "0.3.0".into(),
        api_version: 1,
        pid: 77,
    };
    // The PID is for the log, not for a person.
    assert_eq!(
        daemon_hint(&Connection::Running(health)),
        "Daemon version 0.3.0"
    );
    assert_eq!(
        daemon_hint(&Connection::Reconnecting),
        "Daemon not connected"
    );
    for state in [Autostart::Stale, Autostart::Unsupported] {
        assert!(autostart_note(state).is_some(), "{state:?}");
    }
    for state in [Autostart::Unknown, Autostart::Off, Autostart::On] {
        assert!(autostart_note(state).is_none(), "{state:?}");
    }
}

// -- Uninstall, unlink, forget, and Xdebug --

fn in_use(default: Option<&str>, sites: &[&str], caddy: bool) -> UninstallNotice {
    UninstallNotice::InUse {
        runtime: Runtime::Php,
        in_use: VersionInUse {
            version: "8.3.27".into(),
            default: default.map(str::to_owned),
            sites: sites.iter().map(|site| (*site).to_owned()).collect(),
            caddy,
            ..VersionInUse::default()
        },
    }
}

/// The bar says an uninstall in one sentence; the list of what uses the
/// version stays in [`uninstall_lines`].
#[test]
fn the_uninstall_notice_is_one_sentence() {
    let notice = uninstall_notice(&in_use(Some("8.3"), &["shop", "blog"], true));
    assert_eq!(
        (notice.text.as_str(), notice.problem),
        ("PHP 8.3.27 is in use, so it was not uninstalled.", true)
    );
    let version: Version = "8.2.28".parse().expect("a version");
    let done = uninstall_notice(&UninstallNotice::Done {
        runtime: Runtime::Php,
        version: version.clone(),
    });
    assert_eq!(
        (done.text.as_str(), done.problem),
        ("Uninstalled PHP 8.2.28.", false)
    );
    let failed = uninstall_notice(&UninstallNotice::Failed {
        runtime: Runtime::Php,
        version,
        code: "uninstall_failed".into(),
    });
    assert!(failed.problem && failed.text.starts_with("Cannot uninstall PHP 8.2.28. "));
}

#[test]
fn a_version_in_use_names_what_uses_it() {
    let title = ("PHP 8.3.27 is in use, so it was not uninstalled:", true);
    let lines = uninstall_lines(&in_use(Some("8.3"), &["shop"], false));
    assert_eq!(
        texts(&lines),
        [
            title,
            (
                "It is the default PHP (set to 8.3). Pick another default above first.",
                true
            ),
            (
                "The site shop.test is set to it. Pick another PHP version for it first.",
                true
            ),
        ]
    );
    let lines = uninstall_lines(&in_use(None, &["shop", "blog"], false));
    assert_eq!(
        texts(&lines)[1],
        (
            "The sites shop.test, blog.test are set to it. Pick another PHP version for \
             them first.",
            true
        )
    );
    let caddy = UninstallNotice::InUse {
        runtime: Runtime::Caddy,
        in_use: VersionInUse {
            version: "2.11.7".into(),
            caddy: true,
            ..VersionInUse::default()
        },
    };
    assert_eq!(
        texts(&uninstall_lines(&caddy)),
        [
            ("Caddy 2.11.7 is in use, so it was not uninstalled:", true),
            (
                "Caddy is running from it. Stop Caddy under Services first.",
                true
            ),
        ]
    );
    // No fact at all: the general text.
    let lines = uninstall_lines(&in_use(None, &[], false));
    let general = error_text("version_in_use");
    assert_eq!(texts(&lines), [title, (general.as_str(), true)]);

    // The only PHP, with sites that would have no PHP after it.
    let mut last = in_use(None, &[], false);
    let one = "The site blog.test would have no PHP: PHP 8.3.27 is the only PHP installed. \
               Install another PHP version and pick it as the default above first.";
    let many = "The sites blog.test, shop.test would have no PHP: PHP 8.3.27 is the only PHP \
                installed. Install another PHP version and pick it as the default above \
                first.";
    for (sites, want) in [(&["blog"][..], one), (&["blog", "shop"][..], many)] {
        if let UninstallNotice::InUse { in_use, .. } = &mut last {
            in_use.sites_left_without_php = sites.iter().map(|site| (*site).to_owned()).collect();
        }
        assert_eq!(texts(&uninstall_lines(&last)), [title, (want, true)]);
    }
}

#[test]
fn an_uninstall_says_how_it_ended() {
    let version: Version = v("8.3.27");
    let done = UninstallNotice::Done {
        runtime: Runtime::Node,
        version: version.clone(),
    };
    assert_eq!(
        texts(&uninstall_lines(&done)),
        [("Uninstalled Node.js 8.3.27.", false)]
    );
    let failed = UninstallNotice::Failed {
        runtime: Runtime::Php,
        version,
        code: "uninstall_io".into(),
    };
    let want = format!(
        "Cannot uninstall PHP 8.3.27. {}",
        error_text("uninstall_io")
    );
    assert_eq!(texts(&uninstall_lines(&failed)), [(want.as_str(), true)]);
}

#[test]
fn each_removal_has_its_confirmation() {
    let uninstall = Removal::Uninstall {
        runtime: Runtime::Php,
        version: v("8.3.27"),
    };
    let unlink = Removal::Unlink { site: "api".into() };
    let forget = Removal::Forget {
        folder: PathBuf::from("/code"),
    };
    assert_eq!(confirm_title(&uninstall, None), "Uninstall PHP 8.3.27?");
    assert_eq!(confirm_title(&unlink, None), "Unlink api.test?");
    assert_eq!(confirm_title(&forget, None), "Forget /code?");
    assert_eq!(
        [&uninstall, &unlink, &forget].map(removal_button),
        ["Uninstall", "Unlink", "Forget"]
    );
    assert_eq!(removing(&uninstall), "Uninstalling PHP 8.3.27…");
    assert_eq!(removing(&unlink), "Unlinking api.test…");
    assert_eq!(removing(&forget), "Forgetting /code…");

    let text = confirm_text(&unlink, Some(Path::new("/work/api")), 0, None);
    assert!(
        text.ends_with("The folder /work/api and its files stay."),
        "{text}"
    );
    assert!(confirm_text(&unlink, None, 0, None).ends_with("Its folder and files stay."));
    // Under the home folder, a folder shows as `~/…`, in the title too.
    let (work, api, code_dir) = (full("/work"), full("/work/api"), full("/work/code"));
    let home = Some(work.as_path());
    let shown = home_path(&api, home);
    assert_eq!(shown, tilde(&["api"]));
    let text = confirm_text(&unlink, Some(&api), 0, home);
    assert!(
        text.ends_with(&format!("The folder {shown} and its files stay.")),
        "{text}"
    );
    let code = Removal::Forget { folder: code_dir };
    assert_eq!(
        confirm_title(&code, home),
        format!("Forget {}?", tilde(&["code"]))
    );
    for (sites, start) in [
        (0, "Marsa stops watching this folder."),
        (1, "Marsa stops serving its site,"),
        (3, "Marsa stops serving its 3 sites,"),
    ] {
        let text = confirm_text(&forget, None, sites, None);
        assert!(text.starts_with(start), "{text}");
        assert!(text.ends_with("The folder and its files stay."), "{text}");
    }
    assert!(confirm_text(&uninstall, None, 0, None).contains("install PHP again"));
}

#[test]
fn the_unlink_and_forget_lines() {
    let folder = Path::new("/code");
    assert_eq!(
        unlinked("api"),
        "Unlinked api.test. Its folder and files stay."
    );
    assert!(forgot(folder, None).starts_with("Forgot /code: "));
    assert_eq!(forget_folder(folder, None), "Forget /code…");
    assert_eq!(
        site_is_parked("shop", Some(folder), None),
        "shop.test is part of the parked folder /code."
    );
    assert_eq!(
        site_is_parked("shop", None, None),
        error_text("site_is_parked")
    );
    assert!(unlink_parked_hint(folder).contains("/code"));
    assert!(unlink_parked_hint(folder).contains(PARKED_TITLE));
}

/// A folder under the home folder is `~/…` in every notice text and in
/// the action that offers to forget it.
#[test]
fn a_notice_shows_a_folder_under_home_with_a_tilde() {
    let (home, code) = (full("/Users/omar"), full("/Users/omar/Code"));
    let home = Some(home.as_path());
    let tilde = tilde(&["Code"]);
    assert_eq!(parked(&code, home), format!("Parked {tilde}."));
    assert_eq!(
        linked("shop", &code, home),
        format!("Linked {tilde} as shop.test.")
    );
    assert!(forgot(&code, home).starts_with(&format!("Forgot {tilde}: ")));
    assert_eq!(
        site_is_parked("shop", Some(&code), home),
        format!("shop.test is part of the parked folder {tilde}.")
    );
    assert_eq!(forget_folder(&code, home), format!("Forget {tilde}…"));
    // A long path keeps its last parts in the button, and the sentence
    // leaves it to the button: a notice is two lines at most.
    let long = full("/Users/omar/Code/clients/acme/projects");
    let sep = MAIN_SEPARATOR;
    assert_eq!(
        forget_folder(&long, home),
        format!("Forget …{sep}clients{sep}acme{sep}projects…")
    );
    assert_eq!(
        site_is_parked("shop", Some(&long), home),
        "shop.test is part of a parked folder."
    );
    assert_eq!(path_tail("~/short", 22), "~/short");
    let one = path_tail(&"x".repeat(50), 10);
    assert_eq!(one, format!("…{}", "x".repeat(10)));
    // Elsewhere the whole path stays.
    assert_eq!(
        parked(&full("/srv/www"), home),
        format!("Parked {}.", full("/srv/www").display())
    );
}

#[test]
fn the_xdebug_lines() {
    let on = XdebugState {
        enabled: true,
        loaded: vec!["8.3.27".into(), "8.4.23".into()],
        missing: vec!["8.2.1".into()],
    };
    assert_eq!(
        xdebug_lines(&on, None),
        [
            XDEBUG_TURN_OFF,
            "Loaded in PHP 8.3.27, 8.4.23.",
            "PHP 8.2.1 has no Xdebug and runs without it."
        ]
    );
    let off = XdebugState {
        enabled: false,
        loaded: Vec::new(),
        missing: vec!["8.2.1".into(), "8.3.0".into()],
    };
    assert_eq!(
        xdebug_lines(&off, None),
        ["PHP 8.2.1, 8.3.0 have no Xdebug and run without it."]
    );
    assert!(xdebug_lines(&XdebugState::default(), None).is_empty());
    let none = XdebugState {
        enabled: true,
        ..XdebugState::default()
    };
    assert_eq!(
        xdebug_lines(&none, None),
        [XDEBUG_TURN_OFF, "No installed PHP loads it."]
    );
    // While the daemon writes the files: what is asked for.
    assert_eq!(
        xdebug_lines(&XdebugState::default(), Some(true)),
        ["Turning Xdebug on…"]
    );
    assert_eq!(xdebug_lines(&none, Some(false)), ["Turning Xdebug off…"]);
    // The cost is in the caption, whatever the state.
    assert!(XDEBUG_CAPTION.ends_with("PHP runs two to four times slower while it is on."));
}

#[test]
fn the_link_lines() {
    let folder = Path::new("/work/api");
    assert_eq!(linked("api", folder, None), "Linked /work/api as api.test.");
    assert_eq!(link_folder("/work/api"), "Link /work/api as:");
    assert_eq!(link_folder("~/Work/api"), "Link ~/Work/api as:");
    assert_eq!(site_suffix(), ".test");
    let hint = version_field_hint(runtime_name(Runtime::Node), example_version(Runtime::Node));
    assert!(
        hint.starts_with("A version such as 22: the newest Node.js"),
        "{hint}"
    );
    assert_ne!(pick_title(Pick::Park), pick_title(Pick::Link));
}

/// the line over "Add to /etc/hosts".
#[test]
fn the_hosts_line_names_the_waiting_sites() {
    let names =
        |count: usize| -> Vec<String> { (1..=count).map(|i| format!("s{i}.test")).collect() };
    assert_eq!(
        hosts_waiting(&names(1)),
        "s1.test is not in /etc/hosts yet, so it does not open."
    );
    assert_eq!(
        hosts_waiting(&names(6)),
        "s1.test, s2.test, s3.test, s4.test, s5.test, and 1 more are not in /etc/hosts yet, \
         so they do not open."
    );
}

/// a runtime that core adds later (Python, Go) shows with no
/// entry here: core's name for it and two letters of it.
#[test]
fn a_runtime_core_adds_later_needs_no_entry_for_its_name_letters_and_example() {
    for (folder, letters) in [("python", "Py"), ("go", "Go"), ("ruby", "Ru")] {
        let name = runtime_name_for(folder);
        assert_eq!(name, folder, "core's own name");
        assert_eq!(monogram_for(folder, name), letters);
        assert_eq!(example_version_for(folder), "1.0");
    }
    // The nicer names are entries, not the rule.
    assert_eq!(runtime_name_for("node"), "Node.js");
    assert_eq!(monogram_for("php", "PHP"), "PHP");
    assert_eq!(monogram_for("x", ""), "", "an empty name has no letters");
}

/// the words of the widgets' tiles and badges.
#[test]
fn every_runtime_has_a_short_monogram_and_every_section_a_name() {
    let mut monograms = Vec::new();
    for runtime in Runtime::ALL {
        let letters = runtime_monogram(runtime);
        assert!((2..=3).contains(&letters.len()), "{runtime:?}: {letters}");
        monograms.push(letters);
    }
    monograms.sort_unstable();
    monograms.dedup();
    assert_eq!(
        monograms.len(),
        Runtime::ALL.len(),
        "two runtimes share letters"
    );
    assert_eq!(runtime_monogram(Runtime::Php), "PHP");
    assert_eq!(runtime_monogram(Runtime::Node), "JS");
    assert_eq!(runtime_monogram(Runtime::Composer), "Co");
    assert_eq!(runtime_monogram(Runtime::Caddy), "Ca");

    let mut names: Vec<&str> = Section::ALL.iter().map(|s| section_name(*s)).collect();
    assert!(names.iter().all(|name| !name.is_empty()));
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), Section::ALL.len(), "two sections share a name");
    assert_eq!(section_name(Section::Sites), SITES);
    assert_eq!(section_name(Section::Services), SERVICES);
}

/// The Phase 2 engines have their own entries before core has them: they
/// stay out of the Runtimes screen (`Kind::Engine`), and these words are
/// for their future screens and messages.
#[test]
fn the_phase_two_engines_have_their_name_example_and_monogram() {
    for (folder, name, example, monogram) in [
        ("postgres", "PostgreSQL", "18", "Pg"),
        ("mysql", "MySQL", "8.4", "My"),
        ("valkey", "Valkey", "8.1", "Vk"),
    ] {
        assert_eq!(runtime_name_for(folder), name, "{folder}");
        assert_eq!(example_version_for(folder), example, "{folder}");
        assert_eq!(monogram_for(folder, name), monogram, "{folder}");
    }
}

#[test]
fn the_version_widgets_have_their_words_for_any_runtime() {
    assert_eq!(install_another_label("PHP"), "Install another PHP version");
    assert_eq!(
        install_another_label(runtime_name(Runtime::Node)),
        "Install another Node.js version"
    );
    assert_eq!(USE_SITE_FILES, "Use the site's files");
}

#[test]
fn a_row_badge_label_counts_in_words() {
    assert_eq!(row_badge_label(Section::Mail, 3), "3 new mails");
    assert_eq!(row_badge_label(Section::Mail, 1), "1 new mail");
    assert_eq!(row_badge_label(Section::Dumps, 9), "9 dumps");
    assert_eq!(row_badge_label(Section::Dumps, 1), "1 dump");
    for section in Section::ALL {
        assert!(row_badge_label(section, 2).starts_with("2 "));
    }
}
