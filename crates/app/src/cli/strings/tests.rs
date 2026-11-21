use std::path::Path;

use marsa_core::runtimes::Runtime;
use marsa_core::types::VersionInUse;

use super::*;

use marsa_core::codes::ALL;

#[test]
fn every_code_has_a_message() {
    let empty = Details::default();
    for code in ALL {
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
        ClientError::PathNotUtf8 { path: path() },
        ClientError::Api {
            status: 409,
            code: "caddy_not_installed".into(),
            message: "x".into(),
        },
        ClientError::InUse {
            message: "x".into(),
            in_use: Box::default(),
        },
        ClientError::InUse {
            message: "x".into(),
            in_use: Box::new(VersionInUse {
                sites_left_without_php: vec!["blog".into()],
                ..VersionInUse::default()
            }),
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
            | ClientError::PathNotUtf8 { .. }
            | ClientError::Api { .. }
            | ClientError::InUse { .. }
            | ClientError::DaemonFile(_) => {}
        }
        let code = err.code();
        assert!(ALL.contains(&code), "{code} is missing from codes::ALL");
        assert!(known(code, &Details::default()).is_some(), "{code}");
    }
}

/// The PostgreSQL refusals name it and say what to do.
#[test]
fn postgres_messages_name_it_and_what_to_do() {
    let postgres = Details {
        service: Some("postgres"),
        ..Details::default()
    };
    assert_eq!(
        message("service_port_busy", &postgres),
        "Another program uses the port PostgreSQL needs, and Marsa stops nothing. Quit it, \
         or set another port in global.toml ([postgres] port), then try again. Run \
         \"marsa logs postgres\" to see which program."
    );
    let busy = message("service_data_dir_busy", &postgres);
    assert!(busy.starts_with("Another PostgreSQL server"), "{busy}");
    assert!(message("postgres_not_installed", &postgres).contains("marsa install postgres"));
    // Windows: the daemon runs elevated.
    let elevated = message("service_elevated", &postgres);
    assert!(
        elevated.starts_with("PostgreSQL does not run with administrator rights"),
        "{elevated}"
    );
    assert!(elevated.contains("Run Marsa without administrator rights"));
    assert!(elevated.contains("\"marsa daemon stop\""), "{elevated}");
}

/// The Valkey refusals name it, its table, and what to do: Homebrew's
/// Redis or Herd on 6379 is the usual one.
#[test]
fn valkey_messages_name_it_and_what_to_do() {
    let valkey = Details {
        service: Some("valkey"),
        ..Details::default()
    };
    assert_eq!(
        message("service_port_busy", &valkey),
        "Another program uses the port Valkey needs, and Marsa stops nothing. Quit it, \
         or set another port in global.toml ([valkey] port), then try again. Run \
         \"marsa logs valkey\" to see which program."
    );
    assert!(message("valkey_not_installed", &valkey).contains("marsa install valkey"));
    for code in [
        "valkey_init_failed",
        "valkey_start_timeout",
        "valkey_orphan_stuck",
        "valkey_path_unsupported",
    ] {
        let text = message(code, &valkey);
        assert!(text.contains("marsa logs valkey"), "{code}: {text}");
    }
    assert_eq!(service_name("valkey"), "Valkey");
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
fn an_old_system_message_names_both_versions() {
    let details = Details {
        runtime: Some(Runtime::Php),
        needs: Some("macOS 15.0"),
        found: Some("macOS 14.6"),
        ..Details::default()
    };
    assert_eq!(
        message("install_os_too_old", &details),
        "PHP needs macOS 15.0 or newer, and this system has macOS 14.6. \
         Nothing was installed."
    );
}

#[test]
fn site_messages_name_the_site_and_the_path() {
    let path = std::env::temp_dir().join("notes.txt");
    let details = Details {
        site: Some("Shop"),
        path: Some(&path),
        ..Details::default()
    };
    assert_eq!(
        message("site_name_invalid", &details),
        "\"Shop\" is not a valid site name. Use 1 to 63 of a-z, 0-9, and -, with no - at \
         the start or the end."
    );
    assert_eq!(
        message("site_not_found", &details),
        "There is no site named \"Shop\". Run \"marsa sites\" to see them."
    );
    assert_eq!(
        message("site_not_a_folder", &details),
        format!(
            "{} is not a folder. Check the path, then try again.",
            path.display()
        )
    );
    let none = Details::default();
    assert!(message("site_name_invalid", &none).starts_with("This is not a valid site name."));
    assert!(message("site_not_a_folder", &none).starts_with("The path is not a folder."));
    assert!(message("config_invalid", &none).starts_with("The settings file has an error"));
}

#[test]
fn every_skip_code_has_a_short_reason() {
    for code in [
        "site_name_invalid",
        "site_path_not_utf8",
        "site_name_taken",
        "site_read_dir",
        "site_not_a_folder",
    ] {
        let reason = skip_reason(code);
        assert!(
            reason.chars().next().is_some_and(char::is_lowercase),
            "{code}"
        );
        assert!(!reason.ends_with('.'), "{code}: {reason}");
    }
    assert!(skip_reason("newer_code").starts_with("Something went wrong."));
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

#[test]
fn every_runtime_has_its_own_name_and_example() {
    let names: Vec<&str> = Runtime::ALL.into_iter().map(runtime_name).collect();
    assert_eq!(
        names,
        [
            "PHP",
            "Node.js",
            "Composer",
            "Caddy",
            "PostgreSQL",
            "MySQL",
            "Valkey"
        ]
    );
    for runtime in Runtime::ALL {
        let example = example_version(Some(runtime));
        assert!(
            semver::VersionReq::parse(example).is_ok(),
            "{runtime:?}: {example}"
        );
    }

    let caddy = Details {
        runtime: Some(Runtime::Caddy),
        requested: Some("latest"),
        ..Details::default()
    };
    assert_eq!(
        message("invalid_version", &caddy),
        "\"latest\" is not a version. Write a version number, such as 2.11."
    );
    assert_eq!(
        installing(Runtime::Caddy, "2.11.8", "darwin-arm64"),
        "Installing Caddy 2.11.8 for darwin-arm64"
    );
    assert_eq!(
        already_installed(Runtime::Caddy, "2.11.8"),
        "Caddy 2.11.8 is already installed. Use --force to reinstall."
    );
    assert_eq!(
        checking(Some(Runtime::Caddy), Some("2.11.8")),
        "Checking Caddy 2.11.8..."
    );
}

/// The line before the prompt of `park` and `link`, and
/// the one when the names did not go in. Not on Windows, whose hosts
/// file has another path in the text.
#[cfg(not(windows))]
#[test]
fn park_and_link_say_which_sites_go_into_etc_hosts() {
    let names =
        |count: usize| -> Vec<String> { (1..=count).map(|i| format!("s{i}.test")).collect() };
    assert_eq!(
        hosts_adding(&names(1)),
        "Adding s1.test to /etc/hosts so that it opens. Linux may ask for your password."
    );
    assert_eq!(
        hosts_adding(&names(7)),
        "Adding s1.test, s2.test, s3.test, s4.test, s5.test, and 2 more to /etc/hosts so \
         that they open. Linux may ask for your password."
    );
    assert_eq!(
        hosts_not_added("setup_cancelled"),
        "marsa: The password prompt was cancelled. The new sites do not open until they are \
         in /etc/hosts: run \"marsa setup\" to add them. [setup_cancelled]"
    );
    assert!(hosts_not_added("uninstall_running").starts_with("marsa: Another setup or"));
    assert!(hosts_not_checked("internal_error").contains("run \"marsa setup\""));
}
