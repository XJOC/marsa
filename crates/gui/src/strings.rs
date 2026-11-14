//! Every text the window shows, in one place (AGENTS.md section 9), so a
//! second language can be added later without touching the drawing code.
//! Errors are shown by their stable code, never by their English message.

use std::path::Path;

use marsa_core::recipes::Platform;
use marsa_core::runtimes::Runtime;
use marsa_core::sites::TLD;
use marsa_core::types::{
    InstallFailure, OpStatus, PortHolder, ServiceAction, ServiceState, SetupReport, Site, SiteKind,
    Source, VersionInUse, XdebugState,
};
use semver::Version;

use crate::model::{
    Autostart, Choice, Connection, INSTALL_INTERRUPTED, InstallStep, InstallView, Pick, Removal,
    UninstallNotice,
};
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
pub const NO_SITES: &str = "No sites yet. Park a folder of projects, where each sub-folder becomes a site, or link one \
     project folder.";
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
pub const PARK_BUTTON: &str = "Park a folder…";
pub const PARK_BUTTON_HINT: &str =
    "Choose a folder of projects: every sub-folder becomes a site, named after it.";
pub const LINK_BUTTON: &str = "Link a folder…";
pub const LINK_BUTTON_HINT: &str = "Choose one project folder and give it a site name.";
/// The label before the site name field of the link form.
pub const LINK_NAME: &str = "Site name:";
pub const LINK_NAME_HINT: &str = "a-z, 0-9, and -";
/// The button that sends the link form.
pub const LINK_CONFIRM: &str = "Link";
pub const CANCEL: &str = "Cancel";
/// While a folder dialog is open: the buttons wait for it.
pub const PICKING_HINT: &str = "A folder dialog is open.";
pub const SKIPPED_TITLE: &str = "Skipped folders";
/// The column of the Unlink buttons, and of the Uninstall lists.
pub const COLUMN_REMOVE: &str = "Remove";
pub const UNLINK_BUTTON: &str = "Unlink…";
pub const UNLINK_HINT: &str = "Stop serving this site. Its folder and files stay.";
pub const PARKED_TITLE: &str = "Parked folders";
pub const FORGET_BUTTON: &str = "Forget…";
pub const FORGET_HINT: &str =
    "Stop serving the sites in this folder. The folder and its files stay.";
/// Over a disabled Unlink or Forget button while another removal runs.
pub const REMOVE_BUSY_HINT: &str = "Wait for the removal that runs to end.";

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
pub const COLUMN_INSTALL: &str = "Install";
/// In an empty version field.
pub const VERSION_FIELD_EMPTY: &str = "newest";
pub const INSTALL_BUTTON: &str = "Install";
pub const INSTALL_HINT: &str = "Download and install this version from Marsa's signed version \
     list. With an empty field: the newest stable version.";
/// Over a disabled Install button.
pub const INSTALL_BUSY_HINT: &str = "Wait for the install or uninstall that runs to end.";
/// Opens the list of installed versions to uninstall.
pub const UNINSTALL_BUTTON: &str = "Uninstall…";
pub const UNINSTALL_HINT: &str = "Choose an installed version to delete from Marsa's data folder.";
/// Over a disabled Uninstall list.
pub const UNINSTALL_BUSY_HINT: &str = "Wait for the install or removal that runs to end.";

// Xdebug (`GET` and `PUT /v1/php/xdebug`, task M5-11).

pub const XDEBUG: &str = "Xdebug";
pub const XDEBUG_HINT: &str = "Load Xdebug in every installed PHP that has it, in sites and in \
     the terminal. PHP runs two to four times slower while it is on.";
/// Under the Xdebug box while it is on.
pub const XDEBUG_SLOW: &str = "PHP runs two to four times slower while Xdebug is on. Turn it \
     off when you are done debugging.";

// The first-run panel (task M5-10).

pub const FIRST_RUN_TITLE: &str = "Get started";
pub const FIRST_RUN_TEXT: &str =
    "Marsa needs PHP and the Caddy web server to serve your sites. Neither is installed yet.";
/// Names the version of `model::FIRST_PHP`; a test keeps the two the same.
pub const FIRST_RUN_INSTALL: &str = "Install PHP 8.4 and Caddy";
pub const FIRST_RUN_INSTALL_HINT: &str =
    "Downloads PHP 8.4 and Caddy from Marsa's signed version list, one after the other.";
pub const FIRST_RUN_READY: &str = "PHP and Caddy are installed. One last step, needed once:";
pub const NOT_NOW: &str = "Not now";
pub const CLOSE: &str = "Close";

// Setup (`POST /v1/setup`), in the first-run panel and in Settings.

pub const RUN_SETUP: &str = "Run setup";
pub const RUN_SETUP_HINT: &str =
    "Each step checks what is already done, so running setup again is safe.";
/// When setup had no step to report.
pub const SETUP_NOTHING: &str = "Nothing to set up.";
/// Why setup skipped the trust when no Caddy is installed to make the
/// certificate authority (task M5-15).
const SETUP_CADDY_NOT_INSTALLED: &str = "Caddy is not installed, so Marsa's certificate \
     authority does not exist yet. Install Caddy, then run setup again.";

// Settings (task M5-10).

pub const SETTINGS: &str = "Settings";
pub const AUTOSTART: &str = "Start at login";
pub const AUTOSTART_HINT: &str =
    "Start Marsa's menu-bar icon when you log in. The icon starts the daemon.";
pub const DATA_FOLDER: &str = "Data folder";
pub const DATA_FOLDER_HINT: &str =
    "Where Marsa keeps its programs, settings, and logs. Chosen when Marsa starts.";
pub const APP_VERSION: &str = "Marsa version";
pub const DAEMON_VERSION: &str = "Daemon version";
pub const NOT_CONNECTED: &str = "Not connected";

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

/// The daemon column of Settings: its version and PID while connected.
pub fn daemon_version(connection: &Connection) -> String {
    match connection {
        Connection::Running(health) => format!("{} (PID {})", health.app_version, health.pid),
        _ => NOT_CONNECTED.to_owned(),
    }
}

/// The line next to "Start at login", for the states the box alone cannot
/// show.
pub fn autostart_note(state: Autostart) -> Option<&'static str> {
    match state {
        Autostart::Stale => Some(
            "The login file starts Marsa from another folder, or it was changed. Turn this on \
             to fix it.",
        ),
        Autostart::Unsupported => Some("Not supported on this system yet."),
        Autostart::Unknown | Autostart::Off | Autostart::On => None,
    }
}

/// The title of a folder dialog.
pub fn pick_title(pick: Pick) -> &'static str {
    match pick {
        Pick::Park => "Choose a folder of projects to park",
        Pick::Link => "Choose a project folder to link",
    }
}

/// The first line of the link form.
pub fn link_folder(folder: &Path) -> String {
    format!("Link {} as:", folder.display())
}

/// After the site name field: `.test`.
pub fn site_suffix() -> String {
    format!(".{TLD}")
}

/// Under the link form when the name belongs to another folder's site.
pub fn link_replaces(site: &Site) -> String {
    format!(
        "A site named {} already exists for {}. Linking gives the name to this folder.",
        site.name,
        site.root.display()
    )
}

/// Under the sites buttons once the daemon linked the folder.
pub fn linked(name: &str, folder: &Path) -> String {
    format!("Linked {} as {name}.{TLD}.", folder.display())
}

/// Over an Install field: what to type for this runtime.
pub fn version_field_hint(runtime: Runtime) -> String {
    let example = example_version(runtime);
    format!(
        "A version such as {example}: the newest {} that matches it. Empty: the newest stable \
         version.",
        runtime_name(runtime)
    )
}

fn example_version(runtime: Runtime) -> &'static str {
    match runtime {
        Runtime::Php => "8.4",
        Runtime::Node => "22",
        Runtime::Composer => "2.8",
        Runtime::Caddy => "2.11",
    }
}

/// The text on the progress bar, or the line after an install.
pub fn install_status(view: &InstallView) -> String {
    let what = install_subject(view);
    match &view.step {
        InstallStep::Waiting => format!("Asking the daemon to install {what}…"),
        InstallStep::ListChecked => format!("Installing {what}…"),
        InstallStep::Downloading {
            artifact,
            done,
            total,
        } => format!(
            "Downloading {what} ({artifact}): {} of {} MB",
            megabytes(*done),
            megabytes(*total)
        ),
        InstallStep::Unpacking { artifact } => format!("Unpacking {what} ({artifact})…"),
        InstallStep::Checking => format!("Testing {what}…"),
        InstallStep::Finishing => format!("Finishing the install of {what}…"),
        InstallStep::Done { already: false } => format!("Installed {what}."),
        InstallStep::Done { already: true } => format!("{what} was already installed."),
        InstallStep::Failed(failure) => {
            format!(
                "Cannot install {what}. {}",
                install_failure(view.runtime, failure)
            )
        }
    }
}

/// `PHP 8.4.23`, `PHP 8.4`, `Caddy`, or `a program` for an install from a
/// terminal before its version is known.
fn install_subject(view: &InstallView) -> String {
    match (view.runtime, &view.version) {
        (Some(runtime), Some(version)) => format!("{} {version}", runtime_name(runtime)),
        (Some(runtime), None) => runtime_name(runtime).to_owned(),
        (None, _) => "a program".to_owned(),
    }
}

/// Bytes as megabytes with one decimal: `12.3`.
fn megabytes(bytes: u64) -> String {
    // Display only: `f64` holds any file size closely enough.
    format!("{:.1}", bytes as f64 / 1_000_000.0)
}

/// Why an install failed, with the facts the daemon sent. The daemon's
/// English `message` goes to the log only.
pub fn install_failure(runtime: Option<Runtime>, failure: &InstallFailure) -> String {
    let name = runtime.map_or("the program", runtime_name);
    let requested = failure.requested.as_deref();
    match failure.code.as_str() {
        "install_unsupported_platform" => format!(
            "Marsa has no {name} build for {}.",
            failure.platform.as_deref().unwrap_or("this system")
        ),
        "install_no_such_version" => {
            let wanted = match requested {
                Some(text) => format!("No {name} version matches \"{text}\"."),
                None => format!("The version list has no stable {name} version."),
            };
            if failure.available.is_empty() {
                format!("{wanted} The version list has no {name} versions.")
            } else {
                format!("{wanted} Available: {}.", failure.available.join(", "))
            }
        }
        "install_check_failed" => {
            let output = failure.output.as_deref().map(str::trim);
            match output.filter(|output| !output.is_empty()) {
                Some(output) => format!(
                    "The new {name} failed its test run, so it was not installed. It printed: \
                     {output}"
                ),
                None => format!("The new {name} failed its test run, so it was not installed."),
            }
        }
        "unsupported_alias" | "invalid_version" if requested.is_some() => format!(
            "\"{}\" is not a version number. Write one such as {}.",
            requested.unwrap_or_default(),
            runtime.map_or("8.4", example_version)
        ),
        code => error_text(code),
    }
}

/// What setup does, and the prompt it may bring, on this system.
pub fn setup_explanation(platform: Option<Platform>) -> &'static str {
    match platform {
        Some(Platform::DarwinArm64 | Platform::DarwinX64) => {
            "Setup lets your browser open .test sites over HTTPS: it sends .test names to \
             Marsa, trusts Marsa's certificates, and routes ports 80 and 443 to Marsa. macOS \
             asks for your password once."
        }
        // ADR 0014: the setup text says what the low ports cost.
        Some(Platform::LinuxX64 | Platform::LinuxArm64) => {
            "Setup lets your browser open .test sites over HTTPS: it sends .test names to \
             Marsa, trusts Marsa's certificates, and lets Caddy use ports 80 and 443. After \
             it, any program on this computer may use ports 80 to 1023. Your desktop asks for \
             your password once."
        }
        _ => {
            "Setup lets your browser open .test sites over HTTPS. Your system may ask for an \
             administrator's approval once."
        }
    }
}

/// Next to "Run setup" while it runs.
pub fn setup_running(platform: Option<Platform>) -> &'static str {
    match platform {
        Some(Platform::DarwinArm64 | Platform::DarwinX64) => {
            "Setting up… Enter your password when macOS asks."
        }
        Some(Platform::LinuxX64 | Platform::LinuxArm64) => {
            "Setting up… Enter your password when your desktop asks."
        }
        _ => "Setting up… Answer your system's prompt if one opens.",
    }
}

/// One line of a setup report. `problem`: a step that failed, or why the
/// helper did not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportLine {
    pub text: String,
    pub problem: bool,
}

/// A setup report, as `marsa setup` prints it: one line per step (done,
/// already done, failed, or skipped and why), the programs on port 80 or
/// 443 under the skipped redirect, and why the helper did not run.
pub fn setup_lines(report: &SetupReport) -> Vec<ReportLine> {
    let mut lines = Vec::new();
    let mut push = |text: String, problem: bool| lines.push(ReportLine { text, problem });
    for result in &report.results {
        let step = setup_step(&result.op);
        match result.status {
            OpStatus::Done => push(format!("{step}: done."), false),
            OpStatus::Unchanged => push(format!("{step}: already done."), false),
            OpStatus::Failed => {
                let reason = error_text(result.code.as_deref().unwrap_or_default());
                push(format!("{step}: failed. {reason}"), true);
            }
        }
    }
    for skipped in &report.skipped {
        let step = setup_step(&skipped.op);
        let reason = if skipped.code == "caddy_not_installed" {
            SETUP_CADDY_NOT_INSTALLED.to_owned()
        } else {
            error_text(&skipped.code)
        };
        push(format!("{step}: skipped. {reason}"), false);
        if skipped.code == "redirect_port_held" {
            for holder in &report.port_holders {
                push(port_held(holder), false);
            }
        }
    }
    if let Some(error) = &report.error {
        push(error_text(&error.code), true);
    } else if report.results.is_empty() && report.skipped.is_empty() {
        push(SETUP_NOTHING.to_owned(), false);
    }
    lines
}

/// A helper operation, as a step of setup. An operation this build does not
/// know shows its own name.
fn setup_step(op: &str) -> String {
    match op {
        "trust_install" => "Trust Marsa's HTTPS certificates".to_owned(),
        "resolver_install" => "Send .test names to Marsa".to_owned(),
        "pf_install" => "Route ports 80 and 443 to Marsa".to_owned(),
        "resolved_install" => "Send .test names to Marsa".to_owned(),
        "ports_install" => "Let Caddy use ports 80 and 443".to_owned(),
        other => other.to_owned(),
    }
}

/// `Port 80 is used by nginx-arm64 (PID 15464, Herd).`
fn port_held(holder: &PortHolder) -> String {
    let name = holder
        .process
        .as_deref()
        .or(holder.command.as_deref())
        .unwrap_or("an unknown program");
    let about = match (holder.pid, holder.app.as_deref()) {
        (Some(pid), Some(app)) => format!(" (PID {pid}, {app})"),
        (Some(pid), None) => format!(" (PID {pid})"),
        (None, Some(app)) => format!(" ({app})"),
        (None, None) => String::new(),
    };
    format!("Port {} is used by {name}{about}.", holder.port)
}

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

/// Under the sites buttons once the daemon accepted the folder to park.
pub fn parked(folder: &Path) -> String {
    format!("Parked {}.", folder.display())
}

/// Under the sites buttons once the daemon unlinked the site.
pub fn unlinked(site: &str) -> String {
    format!("Unlinked {site}.{TLD}. Its folder and files stay.")
}

/// Under the sites buttons once the daemon forgot the parked folder.
pub fn forgot(folder: &Path) -> String {
    format!(
        "Forgot {}: its sites are no longer served. The folder and its files stay.",
        folder.display()
    )
}

/// Why `site` cannot be unlinked: it comes from a parked folder.
pub fn site_is_parked(site: &str, folder: Option<&Path>) -> String {
    match folder {
        Some(folder) => format!(
            "{site}.{TLD} comes from the parked folder {}, so it cannot be unlinked alone. \
             Forget that folder to stop serving it and the folder's other sites.",
            folder.display()
        ),
        None => error_text("site_is_parked"),
    }
}

/// The button next to [`site_is_parked`]: it asks to forget the folder.
pub fn forget_folder(folder: &Path) -> String {
    format!("Forget {}…", folder.display())
}

/// Over the disabled Unlink button of a site from a parked folder.
pub fn unlink_parked_hint(folder: &Path) -> String {
    format!(
        "This site comes from the parked folder {}. Forget that folder under {PARKED_TITLE} \
         to stop serving its sites.",
        folder.display()
    )
}

/// The title of the confirmation.
pub fn confirm_title(removal: &Removal) -> String {
    match removal {
        Removal::Uninstall { runtime, version } => {
            format!("Uninstall {} {version}?", runtime_name(*runtime))
        }
        Removal::Unlink { site } => format!("Unlink {site}.{TLD}?"),
        Removal::Forget { folder } => format!("Forget {}?", folder.display()),
    }
}

/// What the confirmed removal does. `site_root`: the folder of the site to
/// unlink; `sites`: how many sites the folder to forget gives now.
pub fn confirm_text(removal: &Removal, site_root: Option<&Path>, sites: usize) -> String {
    match removal {
        Removal::Uninstall { runtime, .. } => format!(
            "Its files are deleted from Marsa's data folder. Marsa refuses if the default, a \
             site, or Caddy uses it. You can install {} again later.",
            runtime_name(*runtime)
        ),
        Removal::Unlink { .. } => {
            let kept = site_root.map_or_else(
                || "Its folder and files stay.".to_owned(),
                |root| format!("The folder {} and its files stay.", root.display()),
            );
            format!(
                "Marsa stops serving this site, and its own settings (PHP version, HTTPS) are \
                 removed. {kept}"
            )
        }
        Removal::Forget { .. } => {
            let served = match sites {
                0 => "Marsa stops watching this folder.".to_owned(),
                1 => "Marsa stops serving its site, and the site's own settings are removed."
                    .to_owned(),
                n => format!(
                    "Marsa stops serving its {n} sites, and their own settings are removed."
                ),
            };
            format!("{served} The folder and its files stay.")
        }
    }
}

/// The button that confirms.
pub fn removal_button(removal: &Removal) -> &'static str {
    match removal {
        Removal::Uninstall { .. } => "Uninstall",
        Removal::Unlink { .. } => "Unlink",
        Removal::Forget { .. } => "Forget",
    }
}

/// While the daemon removes it.
pub fn removing(removal: &Removal) -> String {
    match removal {
        Removal::Uninstall { runtime, version } => {
            format!("Uninstalling {} {version}…", runtime_name(*runtime))
        }
        Removal::Unlink { site } => format!("Unlinking {site}.{TLD}…"),
        Removal::Forget { folder } => format!("Forgetting {}…", folder.display()),
    }
}

/// The lines under the Runtimes table about the last uninstall. A refusal
/// for `version_in_use` or `sites_lose_php` names each thing that uses the
/// version, and what to do about it.
pub fn uninstall_lines(notice: &UninstallNotice) -> Vec<ReportLine> {
    let problem = |text: String| ReportLine {
        text,
        problem: true,
    };
    match notice {
        UninstallNotice::Done { runtime, version } => vec![ReportLine {
            text: format!("Uninstalled {} {version}.", runtime_name(*runtime)),
            problem: false,
        }],
        UninstallNotice::Failed {
            runtime,
            version,
            code,
        } => vec![problem(format!(
            "Cannot uninstall {} {version}. {}",
            runtime_name(*runtime),
            error_text(code)
        ))],
        UninstallNotice::InUse { runtime, in_use } => {
            let mut lines = vec![problem(format!(
                "{} {} is in use, so it was not uninstalled:",
                runtime_name(*runtime),
                in_use.version
            ))];
            lines.extend(in_use_reasons(*runtime, in_use).into_iter().map(problem));
            lines
        }
    }
}

/// One line for each thing that uses the version, with what to do first.
fn in_use_reasons(runtime: Runtime, in_use: &VersionInUse) -> Vec<String> {
    let name = runtime_name(runtime);
    let mut reasons = Vec::new();
    if let Some(default) = &in_use.default {
        reasons.push(format!(
            "It is the default {name} (set to {default}). Pick another default above first."
        ));
    }
    match in_use.sites.as_slice() {
        [] => {}
        [site] => reasons.push(format!(
            "The site {site}.{TLD} is set to it. Pick another {name} version for it first."
        )),
        sites => {
            let names: Vec<String> = sites.iter().map(|site| format!("{site}.{TLD}")).collect();
            reasons.push(format!(
                "The sites {} are set to it. Pick another {name} version for them first.",
                names.join(", ")
            ));
        }
    }
    let names: Vec<String> = in_use
        .sites_left_without_php
        .iter()
        .map(|site| format!("{site}.{TLD}"))
        .collect();
    let left = match names.as_slice() {
        [] => None,
        [site] => Some(format!("The site {site}")),
        sites => Some(format!("The sites {}", sites.join(", "))),
    };
    if let Some(left) = left {
        let version = &in_use.version;
        reasons.push(format!(
            "{left} would have no {name}: {name} {version} is the only {name} installed. \
             Install another {name} version and pick it as the default above first."
        ));
    }
    if in_use.caddy {
        reasons.push("Caddy is running from it. Stop Caddy under Services first.".to_owned());
    }
    if reasons.is_empty() {
        reasons.push(error_text("version_in_use"));
    }
    reasons
}

/// The lines under the Xdebug box: what it does now, and the PHP versions
/// that run without it. `pending`: the state asked for, until the daemon
/// answers.
pub fn xdebug_lines(state: &XdebugState, pending: Option<bool>) -> Vec<String> {
    let mut lines = Vec::new();
    match pending {
        Some(true) => lines.push("Turning Xdebug on…".to_owned()),
        Some(false) => lines.push("Turning Xdebug off…".to_owned()),
        None if state.enabled => {
            lines.push(XDEBUG_SLOW.to_owned());
            lines.push(if state.loaded.is_empty() {
                "No installed PHP loads it.".to_owned()
            } else {
                format!("Loaded in PHP {}.", state.loaded.join(", "))
            });
        }
        None => {}
    }
    match state.missing.as_slice() {
        [] => {}
        [version] => lines.push(format!("PHP {version} has no Xdebug and runs without it.")),
        versions => lines.push(format!(
            "PHP {} have no Xdebug and run without it.",
            versions.join(", ")
        )),
    }
    lines
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
        "invalid_id" => "That name is not valid.",
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
        "site_path_relative" => "That is not a full path of a folder.",
        "site_path_not_utf8" => {
            "That folder's path is not valid UTF-8, so Marsa cannot serve it. Rename the folder."
        }
        "site_read_dir" => "Cannot read that folder. Check its permissions.",
        // M5-09: `DELETE /v1/sites/{name}` and `DELETE /v1/sites/park`.
        "site_is_parked" => {
            "This site comes from a parked folder, so it cannot be unlinked alone. Forget the \
             parked folder to stop serving it and the folder's other sites."
        }
        "folder_not_parked" => "That folder is not parked. It may have just been removed.",
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
        // `DELETE /v1/runtimes/{tool}/{version}` (M5-07).
        "version_in_use" => {
            "This version is in use: it is a default, a site is set to it, or Caddy runs from \
             it. Nothing was uninstalled."
        }
        // M5-19: the only PHP and the default, and sites run it.
        "sites_lose_php" => {
            "This is the only PHP version installed, and sites run it: they would have no PHP. \
             Install another PHP version and pick it as the default first."
        }
        "version_ambiguous" => "More than one installed version matches. Pick one in full.",
        "uninstall_io" => {
            "Cannot delete this version: a program from it may still run, or its folder is not \
             writable. Quit that program, then try again."
        }
        // `PUT /v1/php/xdebug` (M5-11).
        "xdebug_interrupted" => {
            "The daemon stopped before the change reached PHP. The setting is saved: Marsa \
             applies it when it starts again."
        }
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
        // Installs (task M5-10): `InstallError` and the codes under it, in
        // the `failed` event or the answer to the request.
        "install_busy" => {
            "Another install or uninstall is running. Wait for it to end, then try again."
        }
        "install_unsupported_platform" => "Marsa has no build of this program for this system.",
        "install_no_such_version" => "The version list has no version that matches.",
        "install_manifest_mismatch" => {
            "The version list and this build of Marsa disagree on the files to install. \
             Nothing was installed. Update Marsa, or report it."
        }
        "install_unsafe_name" => {
            "The version list has a file name Marsa refuses. Nothing was installed. Report it."
        }
        "install_check_failed" => "The new program failed its test run, so it was not installed.",
        "install_missing_vc_runtime" => {
            "This program needs the Microsoft Visual C++ Redistributable: \
             https://aka.ms/vs/17/release/vc_redist.x64.exe"
        }
        "install_serial_corrupt" => {
            "The file runtimes/.manifest-serial in the data folder is damaged. Delete it, then \
             try again: Marsa then accepts the current version list."
        }
        "install_io" => {
            "Cannot write to the data folder. Check the free disk space and the folder's \
             permissions."
        }
        "install_crashed" => {
            "The install stopped because of a bug in Marsa. Try again; if it repeats, report it \
             with the file logs/marsad.log."
        }
        INSTALL_INTERRUPTED => "The daemon stopped before the install ended. Try again.",
        "manifest_no_trusted_key" => {
            "This build of Marsa has no trusted key for the version list, so it cannot check \
             it. Nothing was installed."
        }
        "manifest_bad_signature" => {
            "The version list failed its signature check. Nothing was installed. Try again \
             later; if it repeats, report it."
        }
        "manifest_too_large" | "manifest_invalid" => {
            "The version list cannot be read. Nothing was installed. Try again later; if it \
             repeats, report it."
        }
        "manifest_schema_unsupported" => {
            "The version list uses a newer format. Update Marsa, then try again."
        }
        "manifest_rollback" => {
            "The version list is older than one this computer already accepted, so it was \
             refused. Try again later; if it repeats, report it."
        }
        "download_failed" => "A download failed. Check your connection, then try again.",
        "download_http_status" => "The download server answered with an error. Try again later.",
        "download_insecure_url" => {
            "Marsa refused a download that is not https. Nothing was installed. Report it."
        }
        "download_size_mismatch" => {
            "A downloaded file has the wrong size. Nothing was installed. Try again."
        }
        "download_too_large" => {
            "A download is larger than allowed. Nothing was installed. Report it."
        }
        "download_io" => {
            "Cannot save a download in the data folder. Check the free disk space and the \
             folder's permissions."
        }
        "checksum_mismatch" => {
            "A downloaded file does not match its signed checksum. Nothing was installed."
        }
        "extract_unsafe_path"
        | "extract_unsafe_link"
        | "extract_unsupported_entry"
        | "extract_duplicate"
        | "extract_too_large" => {
            "A downloaded archive has content Marsa refuses. Nothing was installed. Report it."
        }
        "extract_corrupt" => "A downloaded archive is damaged. Nothing was installed. Try again.",
        "extract_io" => {
            "Cannot unpack a download in the data folder. Check the free disk space and the \
             folder's permissions."
        }
        "recipe_invalid"
        | "recipe_unknown_platform"
        | "recipe_bad_path"
        | "recipe_bad_template" => "This build of Marsa has a broken install recipe. Report it.",
        // Setup (`POST /v1/setup`, task M4-13): why it did not run, the
        // steps it skipped, and the codes of `marsa-helper`.
        "setup_running" => "Another setup is running. Wait for it to end, then try again.",
        // `marsa uninstall --all` runs (M5-13): setup waits for it.
        "uninstall_running" => {
            "Marsa is being removed from this computer (marsa uninstall --all). Wait for it to \
             end, then try again."
        }
        "setup_not_supported" => "Setup from Marsa works on macOS and Linux only for now.",
        "setup_cancelled" => {
            "The password prompt was cancelled, so nothing was changed. Run setup again when \
             you are ready."
        }
        "setup_timed_out" => {
            "Marsa stopped waiting for setup after 5 minutes. Some steps may already have run; \
             running setup again is safe."
        }
        "setup_interrupted" => {
            "The daemon stopped during setup. Some steps may already have run; running setup \
             again is safe."
        }
        "setup_not_authorized" => {
            "Your system did not give Marsa admin rights, so nothing was changed: a wrong \
             password, or no password prompt (as over SSH). Run setup again from your desktop \
             and enter the password of an administrator account."
        }
        // Linux (tasks M4-18e, M4-18h, M4-18b, M4-18c).
        "setup_pkexec_missing" => {
            "pkexec is not installed, so Marsa cannot ask for the administrator password. \
             Install the package pkexec (Debian, Ubuntu), polkit (Fedora), or policykit-1 \
             (older systems), then run setup again."
        }
        "setup_caddy_ports_failed" => {
            "Linux now lets Caddy use ports 80 and 443, but Caddy did not take them. Most often \
             another program listens there (Apache, nginx). Stop it, then restart Caddy; the \
             Caddy log says why."
        }
        "resolved_foreign" => {
            "/etc/systemd/system/marsa-dns-link.service belongs to another tool, so Marsa left \
             it as it is. Move that file away, then run setup again."
        }
        "resolved_read" | "resolved_write" | "resolved_delete" => {
            "Linux did not let Marsa change its files for .test names. Run setup again; if it \
             repeats, report it."
        }
        "resolved_not_running" => {
            "Marsa sends .test names to its DNS server through systemd-resolved, which does not \
             run here, so nothing was changed. Start systemd-resolved, then run setup again."
        }
        "resolved_tool_missing" => {
            "The ip program (package iproute2) is not on this system, so Marsa could not make \
             its link for .test names. Install iproute2, then run setup again."
        }
        "resolved_unit_failed" => {
            "systemd did not start or stop Marsa's link for .test names \
             (marsa-dns-link.service). Run setup again; if it repeats, report it."
        }
        "resolved_check_failed" => {
            "Marsa could not confirm that systemd-resolved sends .test names, and only them, to \
             its DNS server, so it took its link out again. Run setup again; if it repeats, \
             report it."
        }
        "resolved_systemd_too_old" => {
            "This system's systemd is older than version 250, where systemd-resolved can drop \
             the setting that sends .test names to Marsa, so Marsa did not make it. .test names \
             do not open by name here yet."
        }
        "resolved_restart_failed" => {
            "systemd-resolved did not restart after Marsa removed an old setting, so it may \
             still send every name to Marsa's DNS server. Restart systemd-resolved."
        }
        "resolved_stopped" => {
            "systemd-resolved stopped and did not start again, so no name resolves on this \
             computer now. Start systemd-resolved, then run setup again."
        }
        "ports_foreign" => {
            "/etc/sysctl.d/60-marsa.conf belongs to another tool, so Caddy cannot use ports 80 \
             and 443. Move that file away, then run setup again."
        }
        "ports_read" | "ports_write" | "ports_delete" => {
            "Linux did not let Marsa change /etc/sysctl.d/60-marsa.conf. Run setup again; if it \
             repeats, report it."
        }
        "ports_apply_failed" => {
            "Linux did not let Caddy use ports 80 and 443, so Marsa took its setting out again. \
             Run setup again; if it repeats, report it."
        }
        "ports_restore_failed" => {
            "Marsa removed its setting for ports 80 and 443, but every program may use ports 80 \
             to 1023 until the computer restarts."
        }
        "trust_tool_missing" => {
            "This system has no tool Marsa knows for its trusted certificates, so the trust in \
             Marsa's certificate authority was not changed. On Debian, Ubuntu, or Fedora, \
             install the ca-certificates package, then run setup again."
        }
        "trust_store_update_failed" => {
            "The system's store of trusted certificates could not be updated. Run setup again; \
             if it repeats, report it."
        }
        "setup_helper_missing" => {
            "The marsa-helper program is not next to marsad, so nothing was changed. Reinstall \
             Marsa, then try again."
        }
        "setup_helper_unsafe" => {
            "Another user could change the marsa-helper program or its folder, so Marsa did not \
             ask for the password. Make both writable only by you, then run setup again."
        }
        "setup_prompt_failed" => {
            "macOS could not show the administrator password prompt, so nothing was changed. \
             Make sure this Mac's desktop is unlocked. \
             Quit the Marsa window and the menu-bar icon, run \"marsa daemon stop\" in \
             Terminal, then run \"marsa setup\" there, on your desktop."
        }
        "setup_helper_failed" => {
            "marsa-helper gave an answer Marsa cannot read, so the result of setup is not known. \
             Run setup again; if it repeats, report it with the file logs/marsad.log."
        }
        "setup_helper_rejected" => {
            "marsa-helper refused the setup request, so nothing was changed. Report it with the \
             file logs/marsad.log."
        }
        "redirect_port_held" => {
            "Another program uses port 80 or 443, and the redirect would take its traffic. Stop \
             that program, or turn off its web server, then run setup again."
        }
        "ports_not_checked" => {
            "Marsa could not check which programs use ports 80 and 443, so it left them alone. \
             Run setup again; if it repeats, report it."
        }
        "ca_not_ready" => {
            "Caddy did not make Marsa's certificate authority in time. See why in the Caddy \
             log, then run setup again."
        }
        "caddy_not_started" => {
            "Caddy did not start, so Marsa's certificate authority is not there yet. See why in \
             the Caddy log, then run setup again."
        }
        "ca_unreadable" => {
            "Cannot read the root certificate of Marsa's certificate authority, or it is not a \
             certificate. Run setup again; if it repeats, report it."
        }
        "op_not_implemented" => "This build of marsa-helper cannot do this yet. Update Marsa.",
        "op_not_supported" => "marsa-helper cannot do this on this system.",
        "resolver_foreign" => {
            "/etc/resolver/test belongs to another tool, so Marsa left it as it is. Remove that \
             tool's .test setting, then run setup again."
        }
        "resolver_read" | "resolver_write" => {
            "macOS did not let Marsa write /etc/resolver/test. Run setup again; if it repeats, \
             report it."
        }
        "trust_cert_invalid" => {
            "The root certificate of Marsa's certificate authority is not valid, so it was not \
             trusted. Report it."
        }
        // Setup's trust folder in the data folder (M5-13).
        "trust_record_unusable" => {
            "Marsa cannot use its folder for the trusted certificate (trust in the data folder): \
             it must be your own folder, not a link. Fix or delete it, then run setup again."
        }
        "trust_read" | "trust_write" | "trust_add_failed" => {
            "Marsa's certificate authority was not trusted. Run setup again; if it repeats, \
             report it."
        }
        "pf_write" | "pf_load_failed" => {
            "macOS did not load the redirect of ports 80 and 443. Run setup again; if it \
             repeats, report it."
        }
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
            // M5-09: unlink and forget.
            "site_is_parked",
            "folder_not_parked",
            // M5-07 and M5-19: uninstall a version.
            "version_in_use",
            "sites_lose_php",
            "version_ambiguous",
            "uninstall_io",
            "install_busy",
            // M5-11: Xdebug.
            "xdebug_interrupted",
            // M5-13: setup during `marsa uninstall --all`, and its trust folder.
            "uninstall_running",
            "trust_record_unusable",
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

    // -- Installs, setup, and settings (task M5-10) --

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
            "resolved_systemd_too_old",
            "resolved_restart_failed",
            "resolved_stopped",
            "ports_foreign",
            "ports_read",
            "ports_write",
            "ports_delete",
            "ports_apply_failed",
            "ports_restore_failed",
            "trust_tool_missing",
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
                "Downloading PHP 8.4.23 (cli): 12.3 of 30.1 MB",
            ),
            (
                php(InstallStep::Unpacking {
                    artifact: artifact(),
                }),
                "Unpacking PHP 8.4.23 (cli)…",
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
            install_failure(Some(Runtime::Php), &check)
                .ends_with("It printed: dyld: missing library")
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
        assert!(FIRST_RUN_INSTALL.contains(&line), "{FIRST_RUN_INSTALL}");
        assert!(
            FIRST_RUN_INSTALL_HINT.contains(&line),
            "{FIRST_RUN_INSTALL_HINT}"
        );
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

    /// Task M5-15: the two reasons for no CA say what to do.
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
        assert_eq!(
            daemon_version(&Connection::Running(health)),
            "0.3.0 (PID 77)"
        );
        assert_eq!(daemon_version(&Connection::Reconnecting), NOT_CONNECTED);
        for state in [Autostart::Stale, Autostart::Unsupported] {
            assert!(autostart_note(state).is_some(), "{state:?}");
        }
        for state in [Autostart::Unknown, Autostart::Off, Autostart::On] {
            assert!(autostart_note(state).is_none(), "{state:?}");
        }
    }

    // -- Uninstall, unlink, forget, and Xdebug (task M5-10) --

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

        // M5-19: the only PHP, with sites that would have no PHP after it.
        let mut last = in_use(None, &[], false);
        let one = "The site blog.test would have no PHP: PHP 8.3.27 is the only PHP installed. \
                   Install another PHP version and pick it as the default above first.";
        let many = "The sites blog.test, shop.test would have no PHP: PHP 8.3.27 is the only PHP \
                    installed. Install another PHP version and pick it as the default above \
                    first.";
        for (sites, want) in [(&["blog"][..], one), (&["blog", "shop"][..], many)] {
            if let UninstallNotice::InUse { in_use, .. } = &mut last {
                in_use.sites_left_without_php =
                    sites.iter().map(|site| (*site).to_owned()).collect();
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
        assert_eq!(confirm_title(&uninstall), "Uninstall PHP 8.3.27?");
        assert_eq!(confirm_title(&unlink), "Unlink api.test?");
        assert_eq!(confirm_title(&forget), "Forget /code?");
        assert_eq!(
            [&uninstall, &unlink, &forget].map(removal_button),
            ["Uninstall", "Unlink", "Forget"]
        );
        assert_eq!(removing(&uninstall), "Uninstalling PHP 8.3.27…");
        assert_eq!(removing(&unlink), "Unlinking api.test…");
        assert_eq!(removing(&forget), "Forgetting /code…");

        let text = confirm_text(&unlink, Some(Path::new("/work/api")), 0);
        assert!(
            text.ends_with("The folder /work/api and its files stay."),
            "{text}"
        );
        assert!(confirm_text(&unlink, None, 0).ends_with("Its folder and files stay."));
        for (sites, start) in [
            (0, "Marsa stops watching this folder."),
            (1, "Marsa stops serving its site,"),
            (3, "Marsa stops serving its 3 sites,"),
        ] {
            let text = confirm_text(&forget, None, sites);
            assert!(text.starts_with(start), "{text}");
            assert!(text.ends_with("The folder and its files stay."), "{text}");
        }
        assert!(confirm_text(&uninstall, None, 0).contains("install PHP again"));
    }

    #[test]
    fn the_unlink_and_forget_lines() {
        let folder = Path::new("/code");
        assert_eq!(
            unlinked("api"),
            "Unlinked api.test. Its folder and files stay."
        );
        assert!(forgot(folder).starts_with("Forgot /code: "));
        assert_eq!(forget_folder(folder), "Forget /code…");
        let text = site_is_parked("shop", Some(folder));
        assert!(
            text.starts_with("shop.test comes from the parked folder /code,"),
            "{text}"
        );
        assert_eq!(site_is_parked("shop", None), error_text("site_is_parked"));
        assert!(unlink_parked_hint(folder).contains("/code"));
        assert!(unlink_parked_hint(folder).contains(PARKED_TITLE));
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
                XDEBUG_SLOW,
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
            [XDEBUG_SLOW, "No installed PHP loads it."]
        );
        // While the daemon writes the files: what is asked for.
        assert_eq!(
            xdebug_lines(&XdebugState::default(), Some(true)),
            ["Turning Xdebug on…"]
        );
        assert_eq!(xdebug_lines(&none, Some(false)), ["Turning Xdebug off…"]);
        assert!(XDEBUG_HINT.contains("slower") && XDEBUG_SLOW.contains("slower"));
    }

    #[test]
    fn the_link_lines() {
        let folder = Path::new("/work/api");
        assert_eq!(linked("api", folder), "Linked /work/api as api.test.");
        assert_eq!(link_folder(folder), "Link /work/api as:");
        assert_eq!(site_suffix(), ".test");
        let hint = version_field_hint(Runtime::Node);
        assert!(
            hint.starts_with("A version such as 22: the newest Node.js"),
            "{hint}"
        );
        assert_ne!(pick_title(Pick::Park), pick_title(Pick::Link));
    }
}
