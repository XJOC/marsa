//! Every text the `marsa` command shows the user, in one place (AGENTS.md
//! section 9), so a second language can be added later without a rewrite.
//!
//! Errors: [`message`] maps an error `code()` to a sentence. The error's own
//! `Display` text goes to the log only. To add a code, add one arm to
//! `known` and the code to the list in the tests.

use std::path::Path;

use marsa_core::doctor::{Check, Skip};
use marsa_core::runtimes::Runtime;
use marsa_core::types::{ServiceState, SiteKind, VersionInUse};

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
    /// The site name the command is about, such as `shop`, or the folder
    /// name a site name could not be made from.
    pub site: Option<&'a str>,
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
    // A path at the start of a sentence, or what to say without one.
    let shown = |default: &str| {
        d.path
            .map_or(default.to_owned(), |p| p.display().to_string())
    };
    let site = d.site.unwrap_or("<name>");
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
        "install_crashed" => "The install stopped because of a bug in Marsa. Run \
             \"marsa runtimes\" to see what is installed, then try again. If it repeats, \
             report it with the file logs/marsad.log."
            .into(),
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

        // `marsa use`: `PUT /v1/runtimes/{tool}/default` (task M5-01).
        "version_not_installed" => match (d.runtime, d.requested) {
            (Some(runtime), Some(version)) => format!(
                "{name} {version} is not installed. Run \"marsa install {} {version}\", then \
                 try again.",
                runtime.dir_name()
            ),
            _ => "This version is not installed. Run \"marsa runtimes\" to see the installed \
                  versions."
                .into(),
        },
        "no_default_version" => format!(
            "{} has no default version: Marsa always runs the newest installed one.",
            d.runtime.map_or("This program", runtime_name)
        ),

        // `marsa uninstall`: `DELETE /v1/runtimes/{tool}/{version}` (task
        // M5-07). `version_in_use` has its facts in `version_in_use()`.
        "version_in_use" => "This version is in use: it is a default, a site is set to it, \
             or Caddy runs from it. Nothing was uninstalled."
            .into(),
        "version_ambiguous" => format!(
            "{} matches more than one installed {}version. Name one in full; \"marsa \
             runtimes{}\" lists them.",
            d.requested
                .map_or("This version".to_owned(), |r| format!("\"{r}\"")),
            d.runtime
                .map_or(String::new(), |r| format!("{} ", runtime_name(r))),
            d.runtime
                .map_or(String::new(), |r| format!(" {}", r.dir_name()))
        ),
        "uninstall_io" => "Cannot delete this version: a program from it may still run, or \
             its folder is not writable. Quit that program, then try again."
            .into(),

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
            "Caddy is not installed in {path}. Run \"marsa install caddy\", then try again."
        ),
        "caddy_config_write" => format!("Cannot write the Caddy config file {path}. {DISK}"),
        "caddy_start_timeout" => "Caddy started but did not answer in time, so it was stopped. \
             Run \"marsa logs caddy\" to see why."
            .into(),
        "port_in_use" => format!(
            "A port {service} needs is used by another program. Run \"marsa logs {id}\" to \
             see which one."
        ),
        "caddy_orphan_stuck" => format!(
            "An old Caddy, left running when the Marsa daemon crashed, still holds the ports, \
             and Marsa could not stop it safely. Run \"marsa logs {id}\" to see its process, \
             stop it, then try again."
        ),
        "service_managed_by_sites" => format!(
            "{service_first} starts and stops with the sites that need its PHP version, so it \
             cannot be started or stopped by hand. Run \"marsa logs {id}\" to see its lines."
        ),
        "php_fpm_orphan_stuck" => format!(
            "An old php-fpm, left running when the Marsa daemon crashed, still holds the socket \
             or port {service} needs, and Marsa could not stop it safely. Run \"marsa logs \
             {id}\" to see its process and stop it. Then run \"marsa services restart caddy\" \
             to try again; it briefly interrupts every site."
        ),
        "caddy_other_marsa" => format!(
            "The Caddy of another Marsa (another data folder, or another user) uses the ports \
             {service} needs. Stop that Marsa first. Run \"marsa logs {id}\" for details."
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
        "internal_error" => "The Marsa daemon hit a bug. Try again; if it repeats, report it \
             with the file logs/marsad.log."
            .into(),

        // Sites: `SiteError`, `SkipReason`, and the sites routes.
        "site_not_found" => {
            format!("There is no site named \"{site}\". Run \"marsa sites\" to see them.")
        }
        "site_name_invalid" => format!(
            "{} is not a valid site name. Use 1 to 63 of a-z, 0-9, and -, with no - at the \
             start or the end.",
            d.site
                .map_or("This".to_owned(), |name| format!("\"{name}\""))
        ),
        "site_name_taken" => format!("Another folder already has the site name \"{site}\"."),
        "site_not_a_folder" => format!(
            "{} is not a folder. Check the path, then try again.",
            shown("The path")
        ),
        "site_path_relative" => format!(
            "{} is not a full path. Pass a full path, then try again.",
            shown("The path")
        ),
        "site_path_not_utf8" => format!(
            "{} is not valid UTF-8, so Marsa cannot serve it. Rename the folder, then try again.",
            shown("The path")
        ),
        "site_read_dir" => format!(
            "Cannot read {}. Check the folder's permissions.",
            shown("the folder")
        ),
        "no_current_dir" => "Cannot read the current folder; it may have been deleted. Go to \
             another folder, or pass a full path."
            .into(),

        // The settings files: `ConfigError`.
        "config_read" => format!(
            "Cannot read {}. Check the file's permissions.",
            shown("the settings file")
        ),
        "config_invalid" => format!(
            "{} has an error, so Marsa cannot read it. Fix the file, then try again.",
            shown("The settings file")
        ),
        "config_write" => format!("Cannot write {}. {DISK}", shown("the settings file")),

        // `marsa setup`: the setup route (task M4-13), its skipped steps, and
        // the failure codes of `marsa-helper`.
        "setup_running" => "Another setup is running. Wait for it to end, then try again.".into(),
        "setup_not_supported" => "\"marsa setup\" works on macOS only for now.".into(),
        "setup_cancelled" => "The password prompt was cancelled, so nothing was changed. Run \
             \"marsa setup\" again when you are ready."
            .into(),
        "setup_timed_out" => {
            format!("Marsa stopped waiting for setup after 5 minutes. {SETUP_MAY_HAVE_RUN}")
        }
        "setup_interrupted" => format!(
            "The Marsa daemon stopped during setup, so Marsa stopped waiting. \
             {SETUP_MAY_HAVE_RUN}"
        ),
        "setup_not_authorized" => "macOS did not give Marsa admin rights, so nothing was \
             changed. The password was not accepted, or the prompt could not open (over SSH, \
             for example). Run \"marsa daemon stop\", then run \"marsa setup\" again in \
             Terminal on your desktop and enter the password of an administrator account."
            .into(),
        "setup_helper_missing" => "The marsa-helper program is not next to marsad, so nothing \
             was changed. Reinstall Marsa, then try again."
            .into(),
        "setup_helper_unsafe" => "Another user could change the marsa-helper program or its \
             folder, so Marsa did not ask for the password and nothing was changed. Make both \
             yours and writable only by you (chmod go-w), then run \"marsa setup\" again. The \
             path is in the file logs/marsad.log."
            .into(),
        "setup_helper_protected_folder" => "marsa-helper is inside Desktop, Documents, or \
             Downloads, and macOS keeps the admin part of Marsa out of those folders, so \
             nothing was changed. Run \"marsa daemon stop\", move Marsa's programs to \
             ~/Library/Application Support/dev.marsa.Marsa/bin, then run \"marsa setup\" from \
             there."
            .into(),
        "setup_helper_failed" => "marsa-helper gave an answer Marsa cannot read, so the result \
             of setup is not known. Run \"marsa setup\" again; if it repeats, report it with \
             the file logs/marsad.log."
            .into(),
        "setup_helper_rejected" => "marsa-helper refused the setup request, so nothing was \
             changed. This is a bug in Marsa: report it with the file logs/marsad.log."
            .into(),
        "redirect_port_held" => "Another program uses port 80 or 443, and the redirect would \
             take its traffic. Stop that program, or turn off its web server, then run \
             \"marsa setup\" again."
            .into(),
        "ports_not_checked" => "Marsa could not check which programs use ports 80 and 443, so \
             it left them alone. Run \"marsa setup\" again; if it repeats, report it."
            .into(),
        "ca_not_ready" => "Marsa's certificate authority does not exist yet: Caddy makes it \
             when it first serves a secure site. Run \"marsa setup\" again after that."
            .into(),
        "ca_unreadable" => format!(
            "Cannot read the root certificate of Marsa's certificate authority{}, or it is \
             not a certificate. Check the file, then run \"marsa setup\" again.",
            d.path
                .map_or(String::new(), |p| format!(" at {}", p.display()))
        ),
        "op_not_implemented" => format!("This build of marsa-helper cannot do this yet. {UPDATE}"),
        "op_not_supported" => "marsa-helper cannot do this on this system.".into(),
        "resolver_foreign" => "/etc/resolver/test belongs to another tool, so Marsa left it as \
             it is. Remove that tool's .test setting, then run \"marsa setup\" again."
            .into(),
        "resolver_read" | "resolver_write" => "macOS did not let Marsa write \
             /etc/resolver/test. Run \"marsa setup\" again; if it repeats, report it."
            .into(),
        "trust_cert_invalid" => "The root certificate of Marsa's certificate authority is not \
             valid, so it was not trusted. Report it."
            .into(),
        "trust_read" | "trust_write" => "Marsa could not add its certificate authority to \
             the system's trusted certificates. Run \"marsa setup\" again; if it repeats, \
             report it."
            .into(),
        "trust_add_failed" => "Your browser will not trust Marsa's sites yet: the trust for \
             your user was not added. Run \"marsa setup\" in Terminal on the Mac's desktop and \
             approve the dialog macOS shows. If Marsa was started over SSH, run \"marsa daemon \
             stop\" first. If it repeats, report it with the file logs/marsad.log."
            .into(),
        "trust_remove_failed" => "Marsa could not take away the trust in its certificate \
             authority. Run the command again; if it repeats, remove \"Marsa Local CA\" in \
             Keychain Access and report it."
            .into(),
        "pf_write" | "pf_load_failed" => "macOS did not load the redirect of ports 80 and 443. \
             Run \"marsa setup\" again; if it repeats, report it."
            .into(),
        "pf_helper_unreadable" => "The admin part of Marsa could not read marsa-helper, so \
             ports 80 and 443 were not redirected. Run \"marsa daemon stop\" first. If \
             Marsa's programs are in Desktop, Documents, or Downloads, move them to \
             ~/Library/Application Support/dev.marsa.Marsa/bin; otherwise reinstall Marsa. \
             Then run \"marsa setup\" again."
            .into(),

        // `marsa autostart`: the autostart route (task M5-03).
        "autostart_unsupported" => {
            "Starting Marsa at login works on macOS and Linux only for now.".into()
        }
        "tray_not_found" => "The marsa-tray program is not next to marsad, so Marsa cannot \
             start at login. Reinstall Marsa, then try again."
            .into(),
        "autostart_path_invalid" => "The folder of marsa-tray has a character a login file \
             cannot hold: a line break or another control character, or % on Linux. Move \
             Marsa to another folder, then try again."
            .into(),
        "autostart_no_home" => "Cannot find your home folder, so Marsa does not know where \
             the login file goes."
            .into(),
        "autostart_read" => format!(
            "Cannot read {}. Check the file's permissions.",
            shown("the login file")
        ),
        "autostart_write" => format!(
            "Cannot write {}. Check the folder's permissions.",
            shown("the login file")
        ),

        // The Xdebug route (task M5-11).
        "xdebug_interrupted" => "The Marsa daemon stopped before the change reached PHP. The \
             setting is saved: Marsa applies it when it starts again."
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
        Some(Runtime::Caddy) => "2.11",
        Some(Runtime::Php) | None => "8.4",
    }
}

/// The name of a runtime in sentences.
pub fn runtime_name(runtime: Runtime) -> &'static str {
    match runtime {
        Runtime::Php => "PHP",
        Runtime::Node => "Node.js",
        Runtime::Composer => "Composer",
        Runtime::Caddy => "Caddy",
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

// `marsa use`.

/// The new default, and the installed version it picks now when that says
/// more (`8.3` picks `8.3.12`).
pub fn default_set(runtime: Runtime, default: &str, version: &str) -> String {
    let name = runtime_name(runtime);
    if default == version {
        format!("{name} {default} is now the default.")
    } else {
        format!("{name} {default} is now the default ({version}).")
    }
}

// `marsa uninstall` (task M5-07).

/// `Uninstalled PHP 8.3.12.`
pub fn uninstalled(runtime: Runtime, version: &str) -> String {
    format!("Uninstalled {} {version}.", runtime_name(runtime))
}

/// `version_not_installed` from `marsa uninstall`: it points to the list,
/// where `marsa use` points to an install.
pub fn nothing_to_uninstall(runtime: Runtime, requested: &str) -> String {
    format!(
        "{} {requested} is not installed. Run \"marsa runtimes {}\" to see the installed \
         versions.",
        runtime_name(runtime),
        runtime.dir_name()
    )
}

/// `version_in_use` with its facts: one sentence for each use, each with
/// what to do first.
pub fn version_in_use(runtime: Runtime, in_use: &VersionInUse) -> String {
    let name = runtime_name(runtime);
    let version = &in_use.version;
    let mut text = format!("{name} {version} is in use, so it was not uninstalled.");
    if let Some(default) = &in_use.default {
        // `8.3` picks `8.3.12`; `8.3.12` is said once.
        let written = if default == version {
            String::new()
        } else {
            format!(" ({default})")
        };
        text.push_str(&format!(
            " It is the default{written}: make another version the default first with \
             \"marsa use {} <version>\".",
            runtime.dir_name()
        ));
    }
    match &in_use.sites[..] {
        [] => {}
        [site] => text.push_str(&format!(
            " The site {site} is set to it: give it another version first."
        )),
        sites => text.push_str(&format!(
            " The sites {} are set to it: give them another version first.",
            sites.join(", ")
        )),
    }
    if in_use.caddy {
        text.push_str(" Caddy runs from it: stop Caddy first with \"marsa services stop caddy\".");
    }
    text
}

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

// `marsa sites`, `park`, `link`, `secure`, and `unsecure`.

/// The column titles of the sites table.
pub const SITES_HEADER: [&str; 5] = ["NAME", "URL", "KIND", "PHP", "FOLDER"];
/// The PHP column of a site with no PHP version: no PHP is installed.
pub const NO_PHP: &str = "-";
pub const NO_SITES: &str = "No sites yet. Run \"marsa park\" in a folder of projects, or \
     \"marsa link\" in one project.";
/// The title over the folders that did not become a site.
pub const SKIPPED_HEADER: &str = "Skipped folders:";
/// After the `site_name_invalid` text when `marsa link` cannot make a name
/// from the folder name.
pub const PASS_A_NAME: &str = "Pass a name: marsa link <name>";

/// The kind column of the sites table.
pub fn kind_name(kind: SiteKind) -> String {
    match kind {
        SiteKind::Laravel => "laravel".to_owned(),
        SiteKind::Php => "php".to_owned(),
        SiteKind::Static => "static".to_owned(),
        SiteKind::NodeProxy { port } => format!("proxy :{port}"),
    }
}

/// Why a folder did not become a site, in the list under the table. Shorter
/// than [`message`], which is for a command that failed.
pub fn skip_reason(code: &str) -> String {
    match code {
        "site_name_invalid" => "its name gives no valid site name".to_owned(),
        "site_path_not_utf8" => "its path is not valid UTF-8".to_owned(),
        "site_name_taken" => "another folder has the same site name".to_owned(),
        "site_read_dir" => "it cannot be read".to_owned(),
        "site_not_a_folder" => "it is gone or is not a folder".to_owned(),
        other => message(other, &Details::default()),
    }
}

/// One line of the list under the table.
pub fn skipped_folder(path: &str, reason: &str, code: &str) -> String {
    format!("  {path}: {reason} [{code}]")
}

/// `marsa link`.
pub fn linked(url: &str, folder: &str) -> String {
    format!("Linked {folder} as {url}.")
}

/// `marsa secure`.
pub fn secured(url: &str) -> String {
    format!("{url} is served over HTTPS.")
}

/// `marsa unsecure`.
pub fn unsecured(url: &str) -> String {
    format!("{url} is served over plain HTTP.")
}

// `marsa setup`.

/// On stderr before the call, which may wait for the admin prompt and then
/// for the trust dialog. On macOS only: other systems get
/// `setup_not_supported` and no prompt.
pub const SETUP_STARTED: &str = "Setting up Marsa. macOS may ask for an administrator \
     password, and may then ask you to approve trusting Marsa's certificate authority for your \
     user.";
/// After Marsa stopped waiting for the helper, which may still have run.
const SETUP_MAY_HAVE_RUN: &str = "Some steps may already have run with admin rights. Running \
     \"marsa setup\" again is safe: each step checks what is already done.";
/// When setup had no step to report.
pub const SETUP_NOTHING: &str = "Nothing to set up.";

/// A helper operation, as a step of `marsa setup`. An operation this build
/// does not know shows its own name.
pub fn setup_step(op: &str) -> String {
    match op {
        "trust_install" => "Trust Marsa's HTTPS certificates".to_owned(),
        "resolver_install" => "Send .test names to Marsa".to_owned(),
        "pf_install" => "Route ports 80 and 443 to Marsa".to_owned(),
        other => other.to_owned(),
    }
}

pub fn setup_done(step: &str) -> String {
    format!("{step}: done.")
}

pub fn setup_unchanged(step: &str) -> String {
    format!("{step}: already done.")
}

pub fn setup_failed(step: &str, reason: &str, code: &str) -> String {
    format!("{step}: failed. {reason} [{code}]")
}

pub fn setup_skipped(step: &str, reason: &str, code: &str) -> String {
    format!("{step}: skipped. {reason} [{code}]")
}

// `marsa autostart`.

/// `marsa autostart`, when the login file starts this Marsa.
pub const AUTOSTART_ON: &str = "Start at login: on. Marsa's tray icon starts when you log in.";
/// `marsa autostart`, when there is no login file.
pub const AUTOSTART_OFF: &str = "Start at login: off. Run \"marsa autostart on\" to turn it on.";
/// `marsa autostart`, when the login file is not the one this Marsa writes.
pub const AUTOSTART_STALE: &str = "Start at login: off. The login file starts Marsa from \
     another folder, or it was changed. Run \"marsa autostart on\" to fix it.";
/// `marsa autostart on`.
pub const AUTOSTART_TURNED_ON: &str =
    "Start at login: on. From your next login, Marsa's tray icon starts by itself.";
/// `marsa autostart off`.
pub const AUTOSTART_TURNED_OFF: &str = "Start at login: off.";

// `marsa xdebug`.

/// `marsa xdebug`, while it is off.
pub const XDEBUG_OFF: &str = "Xdebug: off. Run \"marsa xdebug on\" to turn it on.";
/// `marsa xdebug off`.
pub const XDEBUG_TURNED_OFF: &str = "Xdebug: off. PHP runs at full speed again.";

/// `marsa xdebug` and `marsa xdebug on`, while it is on: the PHP versions
/// that load Xdebug (`loaded`), and those installed without it (`missing`).
/// `turned_on` adds what it costs (ADR 0010).
pub fn xdebug_on(loaded: &[String], missing: &[String], turned_on: bool) -> String {
    if loaded.is_empty() {
        return "Xdebug: on, but no installed PHP has Xdebug.".to_owned();
    }
    let mut text = format!(
        "Xdebug: on, in your sites and in the terminal. Loaded by PHP {}.",
        loaded.join(", ")
    );
    if !missing.is_empty() {
        let missing = missing.join(", ");
        text.push_str(&format!(
            " Not loaded by PHP {missing}: installed without Xdebug."
        ));
    }
    if turned_on {
        text.push_str(
            " PHP can run up to 4 times slower while it is on. Run \"marsa xdebug off\" when \
             you finish.",
        );
    }
    text
}

/// A program on a port the redirect needs, under the skipped step: its
/// netstat name, its PID, and the app it seems to belong to.
pub fn port_held(port: u16, name: Option<&str>, pid: Option<u32>, app: Option<&str>) -> String {
    let name = name.unwrap_or("an unknown program");
    let about = match (pid, app) {
        (Some(pid), Some(app)) => format!(" (PID {pid}, {app})"),
        (Some(pid), None) => format!(" (PID {pid})"),
        (None, Some(app)) => format!(" ({app})"),
        (None, None) => String::new(),
    };
    format!("  Port {port} is used by {name}{about}.")
}

// `marsa doctor`.

/// A check that passed.
pub fn doctor_pass(text: &str) -> String {
    format!("✓ {text}")
}

/// A check that failed, with its fix on the next line.
pub fn doctor_fail(text: &str, fix: &str) -> String {
    format!("✗ {text}\n  Fix: {fix}")
}

/// A check that did not run, and why.
pub fn doctor_skip(check: Check, skip: Skip) -> String {
    let check = match check {
        Check::Daemon => "marsad",
        Check::Runtimes => "PHP and Caddy",
        Check::Caddy => "Caddy",
        Check::Dns => ".test names",
        Check::Ports => "Ports 80 and 443",
        Check::Trust => "Marsa's certificate authority",
        Check::Path => "PATH",
        Check::Log => "The daemon log",
    };
    let reason = match skip {
        Skip::NotOnThisSystem => "Marsa checks this on macOS only for now.",
        Skip::NeedsDaemon => "It needs marsad, which is not running.",
        Skip::NeedsCaddy => "It needs Caddy, which is not running.",
        Skip::CaddyNotInstalled => "Caddy is not installed.",
        Skip::NoCa => "It does not exist yet: Caddy makes it when it first serves a secure site.",
        Skip::NoLog => "marsad has not written it yet.",
        Skip::NotChecked => "The check could not run. Run again with MARSA_LOG=debug for details.",
    };
    format!("- {check}: not checked. {reason}")
}

pub fn doctor_runtimes_ok(default: &str, version: &str) -> String {
    if default == version {
        format!("PHP and Caddy are installed, and PHP {default} is the default.")
    } else {
        format!("PHP and Caddy are installed, and PHP {default} is the default ({version}).")
    }
}

pub const DOCTOR_CADDY_OK: &str = "Caddy is running.";
pub const DOCTOR_DNS_OK: &str = ".test names resolve to 127.0.0.1.";
pub const DOCTOR_PORTS_OK: &str = "Ports 80 and 443 reach Caddy.";
pub const DOCTOR_TRUST_OK: &str = "Marsa's certificate authority is trusted.";
pub const DOCTOR_PATH_OK: &str = "Marsa's shims come first in PATH.";

pub fn doctor_log_ok(log: &Path) -> String {
    format!("No errors in {} since marsad last started.", log.display())
}

pub const DOCTOR_START_DAEMON: &str = "Run \"marsa daemon start\".";

pub fn doctor_daemon_error(code: &str) -> String {
    format!("Cannot reach marsad. [{code}]")
}

/// The line and the fix when PHP, Caddy, or both are missing.
pub fn doctor_not_installed(php: bool, caddy: bool) -> (String, String) {
    let (missing, fix) = match (php, caddy) {
        (true, true) => (
            "PHP and Caddy are",
            "Run \"marsa install php\", then \"marsa install caddy\".",
        ),
        (true, false) => ("PHP is", "Run \"marsa install php\"."),
        _ => ("Caddy is", "Run \"marsa install caddy\"."),
    };
    (format!("{missing} not installed."), fix.to_owned())
}

pub fn doctor_runtimes_unreadable(code: &str) -> String {
    format!("Cannot list the installed versions. [{code}]")
}

pub fn doctor_config_unreadable(code: &str) -> String {
    format!("Cannot read the default PHP version. [{code}]")
}

pub const DOCTOR_NO_DEFAULT: &str =
    "No PHP version is the default, so a folder without a version of its own runs no PHP.";

pub fn doctor_default_not_installed(default: &str) -> String {
    format!("The default PHP is {default}, but no installed PHP matches it.")
}

pub fn doctor_use_php(version: &str) -> String {
    format!("Run \"marsa use php {version}\".")
}

pub fn doctor_caddy_state(state: &ServiceState) -> String {
    format!("Caddy is {}.", state_name(state))
}

pub const DOCTOR_START_CADDY: &str = "Run \"marsa services start caddy\".";

pub fn doctor_caddy_failed(code: &str) -> String {
    format!("Caddy stopped with an error. [{code}]")
}

pub const DOCTOR_DNS_NO_ANSWER: &str =
    ".test names do not resolve, so browsers cannot find Marsa's sites.";
pub const DOCTOR_DNS_SETUP: &str = "Run \"marsa setup\": it sends .test names to Marsa. If it \
     says /etc/resolver/test belongs to another tool, remove that tool's .test setting first.";

pub fn doctor_dns_wrong(addresses: &str) -> String {
    format!(".test names resolve to {addresses}, not to 127.0.0.1.")
}

pub const DOCTOR_DNS_FOREIGN: &str = "Another tool answers .test names, such as the dnsmasq of \
     Valet or Herd, or a VPN. Turn off its .test domain, then run \"marsa setup\".";
/// Above the [`port_held`] lines.
pub const DOCTOR_PORTS_HELD: &str = "Another program uses port 80 or 443:";
pub const DOCTOR_FREE_PORTS: &str = "Stop that program, or turn off its web server, then run \
     \"marsa setup\".";

pub fn doctor_ports_not_routed(ports: &[u16]) -> String {
    match ports {
        [port] => format!("Port {port} does not reach Caddy."),
        _ => "Ports 80 and 443 do not reach Caddy.".to_owned(),
    }
}

pub const DOCTOR_PORTS_SETUP: &str = "Run \"marsa setup\" to route ports 80 and 443 to Caddy. If \
     you already did, run it again: a macOS update or another tool may have removed the route.";
pub const DOCTOR_UNTRUSTED: &str =
    "Marsa's certificate authority is not trusted, so browsers warn on https://*.test sites.";
pub const DOCTOR_TRUST_SETUP: &str = "Run \"marsa setup\" in Terminal on the Mac's desktop and \
     approve the dialog macOS shows.";

pub fn doctor_shims_missing(shims: &Path) -> String {
    format!(
        "Marsa's shims ({}) are not in PATH, so php does not run Marsa's PHP.",
        shims.display()
    )
}

/// `windows`: the user's PATH is a setting there, not a line in a file.
pub fn doctor_add_shims(shims: &Path, windows: bool) -> String {
    if windows {
        format!(
            "Add {} to the start of your user PATH, then open a new terminal.",
            shims.display()
        )
    } else {
        format!(
            "Add this line at the end of ~/.zprofile and ~/.zshrc (or your shell's startup \
             file), then open a new terminal: export PATH=\"{}:$PATH\"",
            shims.display()
        )
    }
}

pub fn doctor_php_first(php: &Path) -> String {
    format!(
        "{} comes before Marsa's shims in PATH, so php runs that PHP.",
        php.display()
    )
}

pub fn doctor_move_shims(shims: &Path, windows: bool) -> String {
    if windows {
        format!(
            "Move {} to the start of your user PATH, then open a new terminal.",
            shims.display()
        )
    } else {
        format!(
            "Move the line that adds {} to PATH to the end of ~/.zprofile and ~/.zshrc (or your \
             shell's startup file), after Homebrew's, then open a new terminal.",
            shims.display()
        )
    }
}

pub fn doctor_log_error(log: &Path, line: &str) -> String {
    format!(
        "The last error in {} since marsad started:\n    {line}",
        log.display()
    )
}

pub fn doctor_read_log(log: &Path) -> String {
    format!(
        "Read {} around that line. If the error repeats, report it with that file.",
        log.display()
    )
}

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
        // The daemon: the install task panicked.
        "install_crashed",
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
        // `PUT /v1/runtimes/{tool}/default` (task M5-01).
        "version_not_installed",
        "no_default_version",
        // `DELETE /v1/runtimes/{tool}/{version}` (task M5-07).
        "version_in_use",
        "version_ambiguous",
        "uninstall_io",
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
        // `ServiceState::Failed`: `services_api`, `caddy`, `php`, `supervisor`,
        // `runtimes_api`, `api`, and `auth` in `crates/app/src/daemon/`, and
        // `ProgramNotAbsolute` in core.
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
        "invalid_path",
        "invalid_query",
        "unknown_tool",
        "invalid_body",
        "not_found",
        "method_not_allowed",
        "unauthorized",
        "origin_rejected",
        "host_rejected",
        "internal_error",
        // The sites routes (task M4-10b), `SiteError`, `SkipReason`, and
        // `ConfigError`; `site_path_not_utf8` is a `ClientError` too.
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
        // `marsa park` and `marsa link` themselves.
        "no_current_dir",
        // The setup route (task M4-13): its errors, the codes of its skipped
        // steps, and the failure codes of `marsa-helper` for the setup ops.
        "setup_running",
        "setup_not_supported",
        "setup_cancelled",
        "setup_timed_out",
        "setup_interrupted",
        "setup_not_authorized",
        "setup_helper_missing",
        "setup_helper_unsafe",
        "setup_helper_protected_folder",
        "setup_helper_failed",
        "setup_helper_rejected",
        "redirect_port_held",
        "ports_not_checked",
        "ca_not_ready",
        "ca_unreadable",
        "op_not_implemented",
        "op_not_supported",
        "resolver_foreign",
        "resolver_read",
        "resolver_write",
        "trust_cert_invalid",
        "trust_read",
        "trust_write",
        "trust_add_failed",
        "trust_remove_failed",
        "pf_write",
        "pf_load_failed",
        "pf_helper_unreadable",
        // The autostart route (task M5-03) and `AutostartError` in core.
        "autostart_unsupported",
        "tray_not_found",
        "autostart_path_invalid",
        "autostart_no_home",
        "autostart_read",
        "autostart_write",
        // The Xdebug route (task M5-11). Its other codes (`config_*`,
        // `runtimes_read`, `invalid_body`, `internal_error`) are above.
        "xdebug_interrupted",
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
            ClientError::PathNotUtf8 { path: path() },
            ClientError::Api {
                status: 409,
                code: "caddy_not_installed".into(),
                message: "x".into(),
            },
            ClientError::InUse {
                message: "x".into(),
                in_use: VersionInUse::default(),
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
        assert_eq!(names, ["PHP", "Node.js", "Composer", "Caddy"]);
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
}
