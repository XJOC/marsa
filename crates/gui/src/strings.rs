//! Every text the window shows, in one place (AGENTS.md section 9), so a
//! second language can be added later without touching the drawing code.
//! Errors are shown by their stable code, never by their English message.

use std::path::{MAIN_SEPARATOR, Path};

use marsa_core::recipes::Platform;
use marsa_core::runtimes::Runtime;
use marsa_core::sites::TLD;
use marsa_core::types::{
    InstallFailure, OpStatus, PortHolder, ServiceAction, ServiceState, SetupReport, Site, SiteKind,
    VersionInUse, XdebugState,
};
use semver::Version;

use crate::model::{
    Appearance, Autostart, Connection, INSTALL_INTERRUPTED, InstallStep, InstallView, Overall,
    Pick, Removal, UninstallNotice,
};
use crate::screens::settings::Check;
use crate::theme::Section;
use crate::tray::menu::Daemon;
use crate::widgets::{PillState, Stage, StepState};

pub const APP_NAME: &str = "Marsa";

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
pub const COLUMN_NAME: &str = "Name";
pub const COLUMN_SITE: &str = "Site";
pub const COLUMN_CONNECTION: &str = "Connection";
pub const COLUMN_PHP: &str = "PHP";
pub const COLUMN_FOLDER: &str = "Folder";
pub const COLUMN_WHY: &str = "Why";
/// The PHP column of a site with no PHP version: no PHP is installed, or
/// none matches what the site asks for.
pub const NO_PHP: &str = "–";
/// The button at the end of the Sites header, and of its empty state. It
/// opens a menu, so it ends with "…".
pub const ADD_SITE_BUTTON: &str = "Add site…";
/// The empty state of the Sites screen: what this is, and the button that
/// fills it.
pub const SITES_EMPTY: &str =
    "No sites yet. Park a folder of projects, or link one project folder.";
/// The Sites screen while the window has no daemon to ask.
pub const SITES_OFFLINE: &str = "Your sites show here once Marsa is connected to its daemon.";
/// Over the disabled "Add site…" button while the window has no daemon.
pub const ADD_SITE_OFFLINE: &str = "Waiting for the daemon.";
/// The name of the search field for a screen reader, and its placeholder.
pub const FIND_SITE: &str = "Find a site";
/// The key that focuses the search field, as the field shows it at its end.
pub const fn search_key_hint(mac: bool) -> &'static str {
    if mac { "Cmd K" } else { "Ctrl K" }
}

/// Under the header when the search matches no site.
pub fn no_site_matches(search: &str) -> String {
    format!("No site matches “{search}”.")
}

/// The connection cell of a site's row: the word beside the lock.
pub const HTTPS: &str = "HTTPS";
pub const HTTP: &str = "HTTP";
/// What a screen reader says for the version menu of a site's row.
pub fn php_for_site(site: &str) -> String {
    format!("PHP version for {site}")
}

/// A site's own page (task M5-D7). The way back to Sites at the top is the
/// word `SITES` after an arrow icon.
pub const OPEN_FOLDER: &str = "Open folder";
pub const OPEN_IN_BROWSER: &str = "Open in browser";
/// The heading of the large version picker of the page, and its name for a
/// screen reader.
pub const PHP_FOR_THIS_SITE: &str = "PHP for this site";

/// The facts card of the page: its labels, and the HTTPS switch.
pub const FACT_CONNECTION: &str = "Connection";
pub const FACT_FOLDER: &str = "Folder";
pub const FACT_DOCROOT: &str = "Document root";
pub const FACT_KIND: &str = "Kind";
pub const FACT_ADDED_AS: &str = "Added as";
pub const SERVE_OVER_HTTPS: &str = "Serve over HTTPS";
/// "Added as": a folder the user linked.
pub const ADDED_AS_LINKED: &str = "Linked folder";

/// "Added as": a site of a parked folder (as the window shows a folder,
/// `~/Code`).
pub fn added_as_parked(shown: &str) -> String {
    format!("Part of parked folder {shown}")
}

/// Between the reason after a heading and the link that follows it: "8.3 is
/// set for this site · Use the site's files".
pub const HINT_SEPARATOR: &str = "·";

/// The quiet button at the end of the page: the confirmation opens.
pub const UNLINK_THIS_SITE: &str = "Unlink this site…";

/// Under the disabled "Unlink this site…" of a site from a parked folder.
pub fn site_from_parked(shown: &str) -> String {
    format!("This site comes from the parked folder {shown}, so it cannot be unlinked alone.")
}

/// The button beside [`site_from_parked`].
pub fn forget_folder_shown(shown: &str) -> String {
    format!("Forget {shown}…")
}

/// The page of a site that is not in the list: it was unlinked meanwhile, or
/// the window has no daemon to ask.
pub fn site_missing(site: &str) -> String {
    format!("{site}.{TLD} is not in your sites. It may have been unlinked.")
}
pub const SITE_OFFLINE: &str = "This site shows here once Marsa is connected to its daemon.";

/// The sentence after the heading of the page's version picker: which
/// version the site has, and why (components.md, Version menu).
pub fn php_hint_site(version: &str) -> String {
    format!("{version} is set for this site")
}
pub fn php_hint_file(version: &str, path: &Path) -> String {
    format!(
        "{version} comes from the {} file in this folder",
        file_name(path)
    )
}
pub fn php_hint_default(version: &str) -> String {
    format!("{version} is the default: the site's files ask for no version")
}
pub const PHP_HINT_FILES: &str = "the site's files decide the version";
pub const PHP_HINT_NO_MATCH: &str = "no installed version fits what the site asks for";

/// Why a site has the PHP version its row shows (components.md, Version
/// menu): after the version, in `caption`.
pub const REASON_SITE: &str = "set for this site";
pub const REASON_DEFAULT: &str = "default";
/// Right after the user cleared the site's own setting: the window does not
/// know yet what the site's files give.
pub const REASON_FILES: &str = "from the site's files";
pub const REASON_NO_PHP: &str = "no PHP installed";
pub const REASON_NO_MATCH: &str = "no installed version fits";

/// A folder as the window shows it: `~` for the home folder and `~/Code/shop`
/// for one under it, with the separator of the system; any other path as it
/// is. Display only: this text is never given to a program, and the tooltip
/// shows the full path. The match is by whole path parts
/// ([`Path::strip_prefix`]), so `/Users/omarx/Code` is not under
/// `/Users/omar`, and a home that is not a full path, or is the root, is
/// ignored.
pub fn home_path(path: &Path, home: Option<&Path>) -> String {
    let under = home
        .filter(|home| home.is_absolute() && home.parent().is_some())
        .and_then(|home| path.strip_prefix(home).ok());
    let Some(rest) = under else {
        return path.display().to_string();
    };
    let mut shown = String::from("~");
    for part in rest.components() {
        shown.push(MAIN_SEPARATOR);
        shown.push_str(&part.as_os_str().to_string_lossy());
    }
    shown
}

/// `from .php-version`, `from composer.json`: the file the version came
/// from, by its name.
pub fn reason_file(path: &Path) -> String {
    format!("from {}", file_name(path))
}

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
pub const UNLINK_HINT: &str = "Stop serving this site. Its folder and files stay.";
pub const PARKED_TITLE: &str = "Parked folders";
/// After the heading of each list under the sites table.
pub const PARKED_HINT: &str = "every sub-folder is a site";
pub const SKIPPED_HINT: &str = "these did not become sites";
pub const FORGET_BUTTON: &str = "Forget…";
pub const FORGET_HINT: &str =
    "Stop serving the sites in this folder. The folder and its files stay.";
/// Over a disabled Unlink or Forget button while another removal runs.
pub const REMOVE_BUSY_HINT: &str = "Wait for the removal that runs to end.";

pub const RUNTIMES: &str = "Runtimes";
pub const NO_RUNTIMES: &str = "No runtimes listed yet.";
pub const NONE_INSTALLED: &str = "None installed";
/// In an empty version field of the install dialog.
pub const VERSION_FIELD_EMPTY: &str = "newest";
/// The name of the version field, for a screen reader.
pub const VERSION_FIELD_LABEL: &str = "Version";
/// The confirm button of the install dialog.
pub const INSTALL_BUTTON: &str = "Install";
/// Over the button that opens the install dialog.
pub const INSTALL_HINT: &str = "Download and install a version from Marsa's signed version \
     list. Leave the version empty for the newest stable one.";
/// Over a disabled Install button.
pub const INSTALL_BUSY_HINT: &str = "Wait for the install or uninstall that runs to end.";
/// The row of a version's menu that asks to uninstall it.
pub const UNINSTALL_BUTTON: &str = "Uninstall…";
pub const UNINSTALL_HINT: &str = "Delete this version from Marsa's data folder.";
/// Over a disabled Uninstall row while another install or removal runs.
pub const UNINSTALL_BUSY_HINT: &str = "Wait for the install or removal that runs to end.";

// The Runtimes screen: the list, the details, and the Installed table (task
// M5-D8).

pub const INSTALLED: &str = "Installed";
pub const INSTALL_A_VERSION: &str = "Install a version…";
pub const COLUMN_VERSION: &str = "Version";
pub const COLUMN_USED_BY: &str = "Used by";
pub const COLUMN_STATUS: &str = "Status";
/// The column of the row menus (⋯).
pub const COLUMN_MORE: &str = "More";
/// The tag of the version the default picks.
pub const TAG_DEFAULT: &str = "Default";
/// The tag of the version Marsa uses when its runtime has no default.
pub const TAG_IN_USE: &str = "In use";
pub const NO_SITES: &str = "No sites";
/// After the heading "Default PHP".
pub const DEFAULT_RUNTIME_HINT: &str = "used where a project does not ask for a version";
/// The heading of a runtime with no default to pick.
pub const IN_USE_HEADING: &str = "In use";

/// The heading of the large version buttons: "Default PHP".
pub fn default_heading(name: &str) -> String {
    format!("Default {name}")
}

/// After the heading of a runtime with no default: why there is no picker.
pub fn newest_is_used(name: &str) -> String {
    format!("{name} has no default to pick: the newest installed version is used")
}

/// The line under a runtime in the list: its default and how many versions.
pub fn summary_default(default: &str, installed: usize) -> String {
    format!("Default {default}, {installed} installed")
}

/// The line under a runtime whose default picks no installed version.
pub fn summary_no_default(installed: usize) -> String {
    format!("No default, {installed} installed")
}

/// The line under a runtime with no default: the version in use.
pub fn summary_newest(newest: &Version) -> String {
    format!("{newest}, newest is used")
}

/// The empty state of the Installed list.
pub fn no_runtime_installed(name: &str) -> String {
    format!("No {name} installed yet.")
}

/// The button of the empty state: "Install PHP…".
pub fn install_runtime(name: &str) -> String {
    format!("Install {name}…")
}

/// The title of the install dialog.
pub fn install_dialog_title(name: &str) -> String {
    format!("Install a {name} version")
}

/// The name of a version's menu button, for a screen reader and its tooltip.
pub fn more_for(name: &str, version: &Version) -> String {
    format!("More for {name} {version}")
}

/// The "Used by" cell of a version: how many sites run it, and which ones
/// when there are few.
pub fn sites_text(names: &[&str]) -> String {
    match names {
        [] => NO_SITES.to_owned(),
        [site] => format!("1 site: {site}"),
        // A long list does not fit its cell: the count says enough.
        names if names.len() > SITES_NAMED => format!("{} sites", names.len()),
        names => format!("{} sites: {}", names.len(), names.join(", ")),
    }
}

/// The most sites named in a "Used by" cell.
const SITES_NAMED: usize = 3;

/// Why a version cannot be uninstalled: it is the default.
pub fn uninstall_off_default(name: &str) -> String {
    format!("It is the default {name}. Pick another default first.")
}

/// Why a version cannot be uninstalled: sites are set to it.
pub fn uninstall_off_sites(name: &str, sites: &[&str]) -> String {
    match sites {
        [site] => {
            format!("The site {site}.{TLD} is set to it. Pick another {name} version for it first.")
        }
        sites => {
            let names: Vec<String> = sites.iter().map(|site| format!("{site}.{TLD}")).collect();
            format!(
                "The sites {} are set to it. Pick another {name} version for them first.",
                names.join(", ")
            )
        }
    }
}

// Xdebug (`GET` and `PUT /v1/php/xdebug`, task M5-11).

pub const XDEBUG: &str = "Xdebug";
/// Under the Xdebug switch: what it does, and what it costs.
pub const XDEBUG_CAPTION: &str = "Loads Xdebug in every installed PHP that has it, in sites and in \
     the terminal. PHP runs two to four times slower while it is on.";
/// Over the disabled switch before the state is read.
pub const XDEBUG_UNREAD: &str = "Marsa has not read the Xdebug state yet.";
/// Over the disabled switch while a change runs.
pub const XDEBUG_BUSY: &str = "Marsa is changing Xdebug. Wait for it to end.";
/// Under the caption while Xdebug is on: the cost is already in the caption.
pub const XDEBUG_TURN_OFF: &str = "Turn it off when you are done debugging.";

// The first-run panel (tasks M5-10 and M5-D5).

pub const FIRST_RUN_WELCOME: &str = "Welcome to Marsa";
pub const FIRST_RUN_INTRO: &str = "Three steps, and your first site opens over HTTPS.";
/// Names the version of `model::FIRST_PHP`; a test keeps the two the same.
pub const FIRST_RUN_INSTALL: &str = "Install PHP 8.4 and Caddy";
pub const FIRST_RUN_INSTALL_BODY: &str = "Downloaded from Marsa's signed version list. PHP runs \
     your code; Caddy serves your sites.";
pub const FIRST_RUN_INSTALLED: &str = "PHP and Caddy are installed";
pub const FIRST_RUN_SETUP: &str = "Set up HTTPS for .test sites";
pub const FIRST_RUN_SITE: &str = "Add your first site";
pub const FIRST_RUN_SITE_BODY: &str = "Park a folder of projects, where each sub-folder becomes a \
     site, or link one project folder.";
/// The primary button while its step runs.
pub const INSTALLING: &str = "Installing…";
pub const INSTALLING_HINT: &str = "The install runs by itself. Wait for it to end.";
pub const SETTING_UP: &str = "Setting up…";

/// The title of the first step, and the label of its button: what is left
/// to install.
pub const fn first_run_install_title(php: bool, caddy: bool) -> &'static str {
    match (php, caddy) {
        (true, true) => FIRST_RUN_INSTALL,
        (true, false) => INSTALL_PHP,
        (false, true) => INSTALL_CADDY,
        (false, false) => FIRST_RUN_INSTALLED,
    }
}

/// What a screen reader says for a step card: "Step 2: Set up HTTPS, to do
/// now".
pub fn step_label(number: u8, title: &str, stage: Stage) -> String {
    let word = match stage {
        Stage::Done => "done",
        Stage::Current => "to do now",
        Stage::Next => "comes next",
    };
    format!("Step {number}: {title}, {word}")
}
pub const NOT_NOW: &str = "Not now";
pub const CLOSE: &str = "Close";

// Setup (`POST /v1/setup`), in the first-run panel and in Settings.

pub const RUN_SETUP: &str = "Run setup";
/// Linux on the hosts way (task M4-18k3): the button under the sites.
pub const ADD_TO_HOSTS: &str = "Add to /etc/hosts";
pub const ADD_TO_HOSTS_HINT: &str = "Linux asks for your password once, then the sites' .test \
     names go into /etc/hosts, so they open in the browser.";
pub const HOSTS_ADDING: &str = "Waiting for the password…";

/// The sites the hosts file lacks; at most five are named.
pub fn hosts_waiting(missing: &[String]) -> String {
    let mut names = missing
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if missing.len() > 5 {
        names.push_str(&format!(", and {} more", missing.len() - 5));
    }
    match missing.len() {
        1 => format!("{names} is not in /etc/hosts yet, so it does not open."),
        _ => format!("{names} are not in /etc/hosts yet, so they do not open."),
    }
}
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
/// The three groups of the Settings screen (task M5-D9).
pub const SETTINGS_GENERAL: &str = "General";
pub const SETTINGS_SETUP: &str = "Setup";
pub const SETTINGS_ABOUT: &str = "About";
pub const APPEARANCE: &str = "Appearance";
pub const APPEARANCE_HINT: &str = "System follows your computer's setting.";

/// An option of the Appearance control.
pub const fn appearance_name(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::System => "System",
        Appearance::Light => "Light",
        Appearance::Dark => "Dark",
    }
}

pub const LANGUAGE: &str = "Language";
pub const LANGUAGE_NAME: &str = "English";
pub const LANGUAGE_HINT: &str = "More languages come later.";
/// Why Start at login or Run setup cannot be pressed now.
pub const NEEDS_DAEMON_HINT: &str = "Marsa is not connected to its daemon.";
pub const SETUP_IS_RUNNING: &str = "Setup is running.";
pub const AUTOSTART_READING: &str = "Marsa is reading this setting.";
pub const AUTOSTART_UNSUPPORTED: &str = "Not supported on this system yet.";
pub const AUTOSTART: &str = "Start at login";
pub const AUTOSTART_HINT: &str =
    "Start Marsa's menu-bar icon when you log in. The icon starts the daemon.";
pub const DATA_FOLDER: &str = "Data folder";
pub const DATA_FOLDER_HINT: &str =
    "Where Marsa keeps its programs, settings, and logs. Chosen when Marsa starts.";
/// The Setup group: what the window knows of the HTTPS certificate and the
/// `.test` names. It learns it only from a setup report of this session, so
/// everything else is "Not checked", never a claim.
pub const SETUP_ROW: &str = "Setup";
pub const HTTPS_CERT: &str = "HTTPS certificate";
pub const HTTPS_CERT_HINT: &str = "Marsa knows this only from a setup run in this window.";
pub const TEST_NAMES: &str = ".test addresses";
pub const TEST_NAMES_HINT: &str = "Marsa knows this only from a setup run in this window.";

/// How a check of the Setup group stands, by its word.
pub const fn check_word(check: Check) -> &'static str {
    match check {
        Check::Done => "Done",
        Check::Failed => "Failed",
        Check::Skipped => "Skipped",
        Check::NotChecked => "Not checked",
    }
}

pub const APP_VERSION: &str = "Marsa version";
pub const DAEMON_VERSION: &str = "Daemon version";
pub const DAEMON_NOT_CONNECTED: &str = "Daemon not connected";

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

/// Under the Marsa version in Settings: the daemon's version while
/// connected.
pub fn daemon_hint(connection: &Connection) -> String {
    match connection {
        Connection::Running(health) => format!("{DAEMON_VERSION} {}", health.app_version),
        _ => DAEMON_NOT_CONNECTED.to_owned(),
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
        Autostart::Unsupported => Some(AUTOSTART_UNSUPPORTED),
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

/// The first line of the link form: the folder as the window shows it
/// (`~/Work/blog`, with the full path in a tooltip).
pub fn link_folder(shown: &str) -> String {
    format!("Link {shown} as:")
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

/// The notice after the daemon linked the folder. A folder under `home`
/// shows as `~/…` ([`home_path`]).
pub fn linked(name: &str, folder: &Path, home: Option<&Path>) -> String {
    format!("Linked {} as {name}.{TLD}.", home_path(folder, home))
}

/// In the install dialog: what to type for the runtime called `name`.
pub fn version_field_hint(name: &str, example: &str) -> String {
    format!(
        "A version such as {example}: the newest {name} that matches it. Empty: the newest \
         stable version."
    )
}

/// A version to write as an example for `runtime`.
pub fn example_version(runtime: Runtime) -> &'static str {
    example_version_for(runtime.dir_name())
}

/// [`example_version`] by the runtime's folder name. These `match`es are on
/// the name text, not on `Runtime`, so a runtime that core adds later builds
/// and shows with the default arm until it gets its own (task M5-D8).
pub fn example_version_for(folder: &str) -> &'static str {
    match folder {
        "php" => "8.4",
        "node" => "22",
        "composer" => "2.8",
        "caddy" => "2.11",
        // The Phase 2 engines, named before core has them: they stay out of
        // the Runtimes screen, and these are for their own screens and
        // messages.
        "postgres" => "18",
        "mysql" => "8.4",
        "valkey" => "8.1",
        // Any runtime added later: a plain number.
        _ => "1.0",
    }
}

/// The text on the progress bar, or the line after an install.
pub fn install_status(view: &InstallView) -> String {
    let what = install_subject(view);
    match &view.step {
        InstallStep::Waiting => format!("Asking the daemon to install {what}…"),
        InstallStep::ListChecked => format!("Installing {what}…"),
        // The archive name is for the log, not the user (design review, D5).
        InstallStep::Downloading { done, total, .. } => format!(
            "Downloading {what}: {} of {} MB",
            megabytes(*done),
            megabytes(*total)
        ),
        InstallStep::Unpacking { .. } => format!("Unpacking {what}…"),
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
        // Same wording as the CLI (`marsa install`).
        "install_os_too_old" => match failure.os.as_deref() {
            Some(os) => format!(
                "{name} needs {} or newer, and this system has {}. Nothing was installed.",
                os.needs, os.found
            ),
            None => format!("{name} needs a newer system than this one. Nothing was installed."),
        },
        "install_no_such_version" => {
            let wanted = match requested {
                Some(text) => format!("No {name} version matches \"{text}\"."),
                None => format!("The version list has no stable {name} version."),
            };
            if failure.available.is_empty() {
                format!("{wanted} The version list has no {name} versions.")
            } else {
                format!(
                    "{wanted} Available: {}.",
                    versions_shown(&failure.available)
                )
            }
        }
        "install_check_failed" => {
            // A notice is at most two lines: the first line of what the
            // program printed, cut short.
            let output = failure
                .output
                .as_deref()
                .and_then(|output| output.lines().map(str::trim).find(|line| !line.is_empty()))
                .map(|line| cut(line, MAX_PRINTED));
            match output {
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

/// The most characters of a program's output the install failure shows.
const MAX_PRINTED: usize = 60;
/// The most versions "Available:" names.
const MAX_AVAILABLE: usize = 4;

/// `text` cut to `most` characters, with an ellipsis if it was longer.
fn cut(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_owned();
    }
    let kept: String = text.chars().take(most).collect();
    format!("{}…", kept.trim_end())
}

/// The versions the list has, the first few named: `8.3.27, 8.4.23, and 3
/// more`.
fn versions_shown(available: &[String]) -> String {
    if available.len() <= MAX_AVAILABLE {
        return available.join(", ");
    }
    format!(
        "{}, and {} more",
        available[..MAX_AVAILABLE].join(", "),
        available.len() - MAX_AVAILABLE
    )
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
        // ADR 0018: a helper service keeps the hosts file and the trust.
        Some(Platform::WindowsX64) => {
            "Setup lets your browser open .test sites over HTTPS: it adds your sites to the \
             hosts file and trusts Marsa's certificates, through a small Marsa service. Windows \
             asks for your approval (UAC) the first time."
        }
        _ => {
            "Setup lets your browser open .test sites over HTTPS. Your system may ask for an \
             administrator's approval once."
        }
    }
}

/// The setup step of the first-run panel: what the user gets, and the prompt
/// that comes. The details (DNS, certificates, ports) are in
/// `setup_explanation`, for Settings. Linux keeps the cost of the low ports
/// (ADR 0014), because the button here runs setup.
pub fn first_run_setup_body(platform: Option<Platform>) -> &'static str {
    match platform {
        Some(Platform::DarwinArm64 | Platform::DarwinX64) => {
            "Your browser will open .test sites over HTTPS. macOS asks for your password once."
        }
        Some(Platform::LinuxX64 | Platform::LinuxArm64) => {
            "Your browser will open .test sites over HTTPS. After setup, any program on this \
             computer may use ports 80 to 1023. Your desktop asks for your password once."
        }
        Some(Platform::WindowsX64) => {
            "Your browser will open .test sites over HTTPS. Windows asks for your approval \
             (UAC) the first time."
        }
        _ => {
            "Your browser will open .test sites over HTTPS. Your system may ask for an \
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
        Some(Platform::WindowsX64) => "Setting up… Choose Yes if Windows asks (UAC).",
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
        // The hosts way on Linux (task M4-18k).
        "resolved_remove" => "Take out Marsa's link for .test names".to_owned(),
        "hosts_sync" => "Put the sites' .test names in the hosts file".to_owned(),
        "hosts_remove" => "Take Marsa's names out of the hosts file".to_owned(),
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

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The name of a runtime, as the CLI writes it.
pub fn runtime_name(runtime: Runtime) -> &'static str {
    runtime_name_for(runtime.dir_name())
}

/// [`runtime_name`] by the runtime's folder name (`runtimes/<name>`), which
/// is the one name core gives a runtime. The `match` is on that text, not on
/// `Runtime`: a runtime that core adds later builds without a change here and
/// shows under its folder name (`python`) until it gets a nicer one
/// (task M5-D8).
pub fn runtime_name_for(folder: &'static str) -> &'static str {
    match folder {
        "php" => "PHP",
        "node" => "Node.js",
        "composer" => "Composer",
        "caddy" => "Caddy",
        // The Phase 2 engines, named before core has them.
        "postgres" => "PostgreSQL",
        "mysql" => "MySQL",
        "valkey" => "Valkey",
        // Any runtime added later: core's own name.
        other => other,
    }
}

/// The two or three letters on a runtime's tile: a new language needs a
/// monogram, not a new icon (components.md, Section icon tile).
pub fn runtime_monogram(runtime: Runtime) -> String {
    monogram_for(runtime.dir_name(), runtime_name(runtime))
}

/// [`runtime_monogram`] by the folder name and the name shown. Three letters
/// for PHP, "JS" for Node.js, "Pg", "My", and "Vk" for the Phase 2 engines;
/// every other runtime gets the first two letters of its name ("Co", "Ca",
/// "Py" for Python), so a new one needs no entry.
pub fn monogram_for(folder: &str, name: &str) -> String {
    match folder {
        "php" => "PHP".to_owned(),
        "node" => "JS".to_owned(),
        "postgres" => "Pg".to_owned(),
        "mysql" => "My".to_owned(),
        "valkey" => "Vk".to_owned(),
        _ => {
            let mut letters = name.chars();
            let first = letters.next().into_iter().flat_map(char::to_uppercase);
            let second = letters.next().into_iter().flat_map(char::to_lowercase);
            first.chain(second).collect()
        }
    }
}

/// A section's name: its sidebar label, and what a screen reader says for
/// its tile.
pub const fn section_name(section: Section) -> &'static str {
    match section {
        Section::Home => "Home",
        Section::Sites => SITES,
        Section::Runtimes => "Runtimes",
        Section::Services => SERVICES,
        Section::Mail => "Mail",
        Section::Dumps => "Dumps",
        Section::Logs => "Logs",
        Section::Databases => "Databases",
        Section::Queues => "Queues",
    }
}

/// The sidebar's fold button, by what a press does.
pub const HIDE_LABELS: &str = "Hide labels";
pub const SHOW_LABELS: &str = "Show labels";
/// Why the fold button does nothing in a narrow window.
pub const FOLD_NARROW_HINT: &str =
    "The window is too narrow for labels. Make it wider to show them.";

/// The status at the bottom of the sidebar: a title and a caption, short
/// enough for its width.
pub const fn overall_text(overall: Overall) -> (&'static str, &'static str) {
    match overall {
        Overall::Connecting => ("Connecting…", "Waiting for Marsa"),
        Overall::Stopping => ("Marsa is stopping", "Sites stop with it"),
        Overall::DaemonDown => ("Sites not opening", "Marsa is not running"),
        Overall::NoSites => ("Marsa is running", "No sites yet"),
        Overall::Starting => ("Sites starting", "Caddy is starting"),
        Overall::WebServerDown => ("Sites not opening", "Caddy stopped"),
        Overall::Working => ("All sites working", "Caddy is running"),
    }
}

// Home (task M5-D5): the status sentence, its fixes, the default PHP, and the
// first sites.

/// The sentence and the line under it, while the window waits for the daemon.
pub const HOME_CONNECTING: (&str, &str) = ("Connecting to Marsa…", "Waiting for the daemon.");
pub const HOME_STOPPING: (&str, &str) = ("Marsa is stopping.", "Your sites stop with it.");
/// The line under it is the connection's own (`status_line`).
pub const HOME_DAEMON_DOWN: &str = "Sites are not opening: Marsa is not running.";
pub const HOME_CADDY_NOT_INSTALLED: (&str, &str) = (
    "Sites are not opening: Caddy is not installed.",
    "Caddy is the web server that answers every .test address. Install it to serve your sites.",
);
pub const HOME_CADDY_STOPPED: (&str, &str) = (
    "Sites are not opening: Caddy stopped.",
    "Caddy is the web server that answers every .test address. It is not running.",
);
pub const HOME_CADDY_FAILED: &str = "Sites are not opening: Caddy failed.";
pub const HOME_CADDY_STARTING: (&str, &str) =
    ("Caddy is starting.", "Your sites open as soon as it runs.");
pub const HOME_CADDY_STOPPING: (&str, &str) = (
    "Caddy is stopping.",
    "Your sites do not open until it starts again.",
);
pub const HOME_PHP_NOT_INSTALLED: (&str, &str) = (
    "Sites are not opening: PHP is not installed.",
    "PHP runs your code. Install a version to serve your sites.",
);
pub const HOME_DNS_MISSING: &str = "Sites are not opening: DNS does not know them yet.";
pub const HOME_NO_SITES: &str = "No sites yet.";

/// The sentence when a pool failed: only the sites of that PHP are down.
pub fn home_php_failed(minor: &str) -> String {
    format!("PHP {minor} failed: its sites are not opening.")
}

/// The sentence when all work.
pub fn home_working(sites: usize) -> String {
    match sites {
        1 => "Your site is working.".to_owned(),
        n => format!("All {n} sites are working."),
    }
}

/// The line under it: what runs. `php` is `8.4`, or nothing while the list
/// of runtimes is not read.
pub fn home_running(php: Option<&str>) -> String {
    match php {
        Some(minor) => format!("Caddy is running and PHP {minor} is ready."),
        None => "Caddy is running.".to_owned(),
    }
}

/// The line under "No sites yet." when all else works.
pub fn home_no_sites(php: Option<&str>) -> String {
    format!(
        "{} Park a folder of projects, or link one, to add your first site.",
        home_running(php)
    )
}

/// `PHP 8.4`, or `PHP` while the line is not known.
pub fn php_line(minor: Option<&str>) -> String {
    minor.map_or_else(|| "PHP".to_owned(), |minor| format!("PHP {minor}"))
}

pub const PATH_DNS: &str = "DNS";
pub const PATH_CADDY: &str = "Caddy";

/// The fixes of the status sentence.
pub const START_CADDY: &str = "Start Caddy";
pub const SHOW_LOG: &str = "Show log";
pub const INSTALL_CADDY: &str = "Install Caddy";
/// Names the version of `model::FIRST_PHP`; a test keeps the two the same.
pub const INSTALL_PHP: &str = "Install PHP 8.4";

pub const DEFAULT_PHP: &str = "Default PHP";
pub const NO_PHP_YET: &str = "No PHP installed yet.";
pub const HOME_SITES_EMPTY: &str = "Sites you park or link show here.";
pub const ADD_SITE: &str = "Add a site";
pub const OPEN: &str = "Open";

/// The link after the heading "Sites" on Home.
pub fn all_sites(count: usize) -> String {
    format!("All {count} sites")
}

/// What a screen reader says for a row badge: "3 new mails", "9 dumps".
pub fn row_badge_label(section: Section, count: u32) -> String {
    let (one, many) = match section {
        Section::Home => ("event", "events"),
        Section::Sites => ("site", "sites"),
        Section::Runtimes => ("runtime", "runtimes"),
        Section::Services => ("service", "services"),
        Section::Mail => ("new mail", "new mails"),
        Section::Dumps => ("dump", "dumps"),
        Section::Logs => ("log line", "log lines"),
        Section::Databases => ("database", "databases"),
        Section::Queues => ("job", "jobs"),
    };
    format!("{count} {}", if count == 1 { one } else { many })
}

/// The name of the request path for a screen reader (components.md,
/// Status): the steps are DNS, Caddy, the runtime, and the site.
pub const REQUEST_PATH: &str = "Where the request stops";

/// What a screen reader says for one step of the request path: "Caddy, not
/// working". The icon and the weight say it to the eye (R8).
pub fn path_step_label(label: &str, state: StepState) -> String {
    let word = match state {
        StepState::Working => "working",
        StepState::Broken => "not working",
        StepState::Unreached => "not reached",
        StepState::NotChecked => "not checked",
    };
    format!("{label}, {word}")
}

/// The first row of a site's version menu: it clears the site's own setting.
pub const USE_SITE_FILES: &str = "Use the site's files";

/// The name of the dashed "+" of a version picker: "Install another PHP
/// version".
pub fn install_another_label(runtime: &str) -> String {
    format!("Install another {runtime} version")
}

/// The word of a service's state pill (components.md, Status).
pub const fn pill_state_word(state: PillState) -> &'static str {
    match state {
        PillState::Running => "Running",
        PillState::Waiting => "Waiting",
        PillState::Stopped => "Stopped",
        PillState::Starting => "Starting",
    }
}

/// The notice after the daemon accepted the folder to park.
pub fn parked(folder: &Path, home: Option<&Path>) -> String {
    format!("Parked {}.", home_path(folder, home))
}

/// Under the sites buttons once the daemon unlinked the site.
pub fn unlinked(site: &str) -> String {
    format!("Unlinked {site}.{TLD}. Its folder and files stay.")
}

/// The notice after the daemon forgot the parked folder.
pub fn forgot(folder: &Path, home: Option<&Path>) -> String {
    format!(
        "Forgot {}: its sites are no longer served. Its files stay.",
        home_path(folder, home)
    )
}

/// Why `site` cannot be unlinked: it is part of a parked folder. One short
/// sentence: the site's page says the rest ([`unlink_parked_hint`]).
pub fn site_is_parked(site: &str, folder: Option<&Path>, home: Option<&Path>) -> String {
    match folder {
        Some(folder) => format!(
            "{site}.{TLD} is part of the parked folder {}.",
            home_path(folder, home)
        ),
        None => error_text("site_is_parked"),
    }
}

/// The action of the notice [`site_is_parked`]: it asks to forget the folder.
/// A long path keeps its last parts (`…/clients/acme`): the sentence before
/// the button has the whole path.
pub fn forget_folder(folder: &Path, home: Option<&Path>) -> String {
    forget_folder_shown(&path_tail(&home_path(folder, home), MAX_ACTION_PATH))
}

/// The longest path a button names in full.
const MAX_ACTION_PATH: usize = 22;

/// `shown`, or its last parts after an ellipsis if it is longer than `most`
/// characters: `~/Code/clients/acme/projects` is `…/clients/acme/projects`
/// at 25. A last part that is too long on its own is cut.
fn path_tail(shown: &str, most: usize) -> String {
    if shown.chars().count() <= most {
        return shown.to_owned();
    }
    let mut tail = String::new();
    for part in shown.rsplit(MAIN_SEPARATOR) {
        let joined = if tail.is_empty() {
            part.to_owned()
        } else {
            format!("{part}{MAIN_SEPARATOR}{tail}")
        };
        if joined.chars().count() > most {
            break;
        }
        tail = joined;
    }
    if tail.is_empty() {
        // One part longer than `most`: its end says more than its start.
        let kept: Vec<char> = shown.chars().rev().take(most).collect();
        tail = kept.into_iter().rev().collect();
        return format!("…{tail}");
    }
    format!("…{MAIN_SEPARATOR}{tail}")
}

/// Over the disabled Unlink button of a site from a parked folder.
pub fn unlink_parked_hint(folder: &Path) -> String {
    format!(
        "This site comes from the parked folder {}. Forget that folder under {PARKED_TITLE} \
         to stop serving its sites.",
        folder.display()
    )
}

/// The title of the confirmation: the exact question. A folder under `home`
/// shows as `~/…` ([`home_path`]).
pub fn confirm_title(removal: &Removal, home: Option<&Path>) -> String {
    match removal {
        Removal::Uninstall { runtime, version } => {
            format!("Uninstall {} {version}?", runtime_name(*runtime))
        }
        Removal::Unlink { site } => format!("Unlink {site}.{TLD}?"),
        Removal::Forget { folder } => format!("Forget {}?", home_path(folder, home)),
    }
}

/// What the confirmed removal does, and what stays. `site_root`: the folder
/// of the site to unlink; `sites`: how many sites the folder to forget gives
/// now; a folder under `home` shows as `~/…`.
pub fn confirm_text(
    removal: &Removal,
    site_root: Option<&Path>,
    sites: usize,
    home: Option<&Path>,
) -> String {
    match removal {
        Removal::Uninstall { runtime, .. } => format!(
            "Its files are deleted from Marsa's data folder. Marsa refuses if the default, a \
             site, or Caddy uses it. You can install {} again later.",
            runtime_name(*runtime)
        ),
        Removal::Unlink { .. } => {
            let kept = site_root.map_or_else(
                || "Its folder and files stay.".to_owned(),
                |root| format!("The folder {} and its files stay.", home_path(root, home)),
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
            text: uninstalled(*runtime, version),
            problem: false,
        }],
        UninstallNotice::Failed {
            runtime,
            version,
            code,
        } => vec![problem(uninstall_failed(*runtime, version, code))],
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

/// The notice about the last uninstall: one sentence. A refusal for a version
/// that is in use does not list what uses it: the Runtimes screen says that
/// beside the version's menu ([`uninstall_lines`] has the whole list).
pub fn uninstall_notice(notice: &UninstallNotice) -> ReportLine {
    let (text, problem) = match notice {
        UninstallNotice::Done { runtime, version } => (uninstalled(*runtime, version), false),
        UninstallNotice::Failed {
            runtime,
            version,
            code,
        } => (uninstall_failed(*runtime, version, code), true),
        UninstallNotice::InUse { runtime, in_use } => (
            format!(
                "{} {} is in use, so it was not uninstalled.",
                runtime_name(*runtime),
                in_use.version
            ),
            true,
        ),
    };
    ReportLine { text, problem }
}

fn uninstalled(runtime: Runtime, version: &Version) -> String {
    format!("Uninstalled {} {version}.", runtime_name(runtime))
}

fn uninstall_failed(runtime: Runtime, version: &Version, code: &str) -> String {
    format!(
        "Cannot uninstall {} {version}. {}",
        runtime_name(runtime),
        error_text(code)
    )
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
            lines.push(XDEBUG_TURN_OFF.to_owned());
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
        // `marsa_core::platform::OpenError`, and the window's own folder opener.
        "open_no_program" => "This system has no program to open a folder with.",
        "open_failed" => "Cannot open the folder.",
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
        "site_is_parked" => "This site is part of a parked folder, so it cannot be unlinked alone.",
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
        "install_os_too_old" => {
            "This program needs a newer system than this one. Nothing was installed."
        }
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
             try again."
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
        "setup_not_supported" => "Setup from Marsa works on macOS, Linux, and Windows only.",
        // The window runs on the machine marsad runs on, so `cfg!` is the
        // daemon's system too. Its admin prompt is UAC (ADR 0018).
        "setup_cancelled" if cfg!(windows) => {
            "The UAC prompt was declined, so nothing was changed. Run setup again when you are \
             ready."
        }
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
        // Windows: the helper service and the hosts file (task M4-19g2, ADR 0018).
        "setup_helper_not_the_service" => {
            "Another program holds the name of Marsa's helper service, so Marsa sent it nothing \
             and nothing was changed. Restart Windows, then run setup again; if it repeats, \
             report it with the file logs/marsad.log."
        }
        "setup_helper_install_failed" => {
            "Marsa could not install its helper service, so nothing was changed. Windows' \
             service log and the file logs/marsad.log say why. Run setup again; if it repeats, \
             report it."
        }
        "setup_helper_install_running" => {
            "The install of Marsa's helper service is still running. Wait a few minutes, then \
             run setup again."
        }
        "setup_helper_pipe_failed" => {
            "Marsa could not reach its helper service, so nothing was changed. Run setup again; \
             if it repeats, report it with the file logs/marsad.log."
        }
        "setup_helper_busy" => {
            "Marsa's helper service is busy with another request. Wait a moment, then run \
             setup again."
        }
        "setup_helper_no_answer" => {
            "Marsa's helper service gave no answer, so the result of setup is not known. Run \
             \"marsa doctor\" in a terminal to see what is set up, then run setup again."
        }
        // Linux (tasks M4-18e, M4-18h, M4-18b, M4-18c).
        "setup_pkexec_missing" => {
            "pkexec is not installed, so Marsa cannot ask for the administrator password. \
             Install the package pkexec (Debian, Ubuntu), polkit (Fedora), or policykit-1 \
             (older systems), then run setup again."
        }
        // Linux, the second request of setup (task M4-18k2).
        "setup_hosts_not_written" => {
            "The steps above are done, but the second password prompt did not go through, so \
             the sites' .test names are not in /etc/hosts yet and the sites do not open. Run \
             setup again."
        }
        "setup_hosts_not_removed" => {
            "The steps above are done, and .test names now reach Marsa without /etc/hosts. The \
             second password prompt did not go through, so Marsa's old lines stay in \
             /etc/hosts; they do no harm. Run setup again to take them out."
        }
        "hosts_not_used" => {
            "Marsa has no lines in /etc/hosts for this data folder yet. Run setup first."
        }
        "hosts_sites_unknown" => {
            "Marsa could not read its site list, so it left the hosts file as it is. Look at \
             the sites, then run setup again."
        }
        "hosts_too_many_names" => {
            "There are more sites than Marsa can put in the hosts file at once (1000 names), \
             so it left the file as it is. Remove sites you do not use, then run setup again."
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
        // Setup then puts the names in /etc/hosts (task M4-18k2).
        "resolved_not_running" => {
            "systemd-resolved does not run here, so Marsa did not make its link for .test \
             names: they go into /etc/hosts instead."
        }
        // Linux (task M4-18k) and Windows (task M4-19).
        "hosts_read" => {
            "Marsa did not change the hosts file: it cannot be read, or it is a link, \
             read-only, or larger than 16 MiB. Fix the file, then run setup again."
        }
        "hosts_write" => {
            "Marsa could not write the hosts file. Another program may hold it (some security \
             tools do); close it, then run setup again."
        }
        "hosts_block_broken" => {
            "The hosts file has a \"# Marsa start\" line without its \"# Marsa end\" line, or \
             the other way round, so Marsa left it as it is. Remove Marsa's lines from it, then \
             run setup again."
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
        "resolved_use_hosts" => {
            "This system's systemd is older than version 250 and systemd-networkd may run, so \
             systemd-resolved can drop the setting that sends .test names to Marsa. Marsa did \
             not make it: .test names here go into /etc/hosts instead."
        }
        "resolved_systemd_unknown" => {
            "Marsa could not read the version of systemd here, so it did not make its link for \
             .test names. Report it."
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
        // A long path keeps its last parts in the button, and the whole path
        // stays in the sentence.
        let long = full("/Users/omar/Code/clients/acme/projects");
        let sep = MAIN_SEPARATOR;
        assert_eq!(
            forget_folder(&long, home),
            format!("Forget …{sep}clients{sep}acme{sep}projects…")
        );
        assert!(site_is_parked("shop", Some(&long), home).contains(&home_path(&long, home)));
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

    /// Task M4-18k3: the line over "Add to /etc/hosts".
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

    /// Task M5-D8: a runtime that core adds later (Python, Go) shows with no
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

    /// Task M5-D2: the words of the widgets' tiles and badges.
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
}
