//! Every text the `marsa` command shows the user, in one place (AGENTS.md
//! section 9), so a second language can be added later without a rewrite.
//!
//! Errors: [`message`] maps an error `code()` to a sentence. The error's own
//! `Display` text goes to the log only. To add a code, add one arm to
//! `known` and the code to the list in the tests.

use std::path::Path;

use marsa_core::runtimes::Runtime;

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
    const REPORT: &str = "Nothing was installed. Report it.";
    const AGAIN: &str = "Nothing was installed. Try again later; if it repeats, report it.";
    const DISK: &str = "Check the free disk space and the folder's permissions.";

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every code `marsa install` and `marsa runtimes` can meet. Core has no
    /// public list of its codes, so this one is kept by hand from the
    /// `code()` functions of `InstallError`, `ManifestError`, `FetchError`,
    /// `ExtractError`, `BadRequest`, `RecipeError`, `RuntimesError`, and
    /// `config::app_data_dir`.
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
