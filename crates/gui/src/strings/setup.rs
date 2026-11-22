//! The first-run panel, setup, and Settings.

use marsa_core::recipes::Platform;
use marsa_core::types::{OpName, OpStatus, PortHolder, SetupReport};

use crate::prefs::Appearance;

use super::*;

// The first-run panel.

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
pub fn step_label(number: u8, title: &str, word: &str) -> String {
    format!("Step {number}: {title}, {word}")
}

/// The words of a step's stage, for a screen reader.
pub const STAGE_DONE: &str = "done";
pub const STAGE_CURRENT: &str = "to do now";
pub const STAGE_NEXT: &str = "comes next";
pub const NOT_NOW: &str = "Not now";
pub const CLOSE: &str = "Close";

// Setup (`POST /v1/setup`), in the first-run panel and in Settings.

pub const RUN_SETUP: &str = "Run setup";
/// Linux on the hosts way: the button under the sites.
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
/// certificate authority.
pub(super) const SETUP_CADDY_NOT_INSTALLED: &str = "Caddy is not installed, so Marsa's certificate \
     authority does not exist yet. Install Caddy, then run setup again.";

// Settings.

pub const SETTINGS: &str = "Settings";
/// The three groups of the Settings screen.
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
pub const CHECK_DONE: &str = "Done";
pub const CHECK_FAILED: &str = "Failed";
pub const CHECK_SKIPPED: &str = "Skipped";
pub const CHECK_NOT_CHECKED: &str = "Not checked";

pub const APP_VERSION: &str = "Marsa version";
pub const DAEMON_VERSION: &str = "Daemon version";
pub const DAEMON_NOT_CONNECTED: &str = "Daemon not connected";

/// The window's own code: its background thread could not start.
pub const BRIDGE_FAILED: &str = "bridge_failed";

/// Under the Marsa version in Settings: the daemon's version while
/// connected.
pub fn daemon_hint(version: Option<&str>) -> String {
    match version {
        Some(version) => format!("{DAEMON_VERSION} {version}"),
        None => DAEMON_NOT_CONNECTED.to_owned(),
    }
}

/// The line next to "Start at login" when the login file is there but starts
/// another program.
pub const AUTOSTART_STALE: &str = "The login file starts Marsa from another folder, or it was changed. Turn this on \
     to fix it.";

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
        let step = setup_step(result.op);
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
        let step = setup_step(skipped.op);
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

/// A step of setup. A step that is not part of setup shows its own name.
pub(super) fn setup_step(op: OpName) -> String {
    match op {
        OpName::TrustInstall => "Trust Marsa's HTTPS certificates".to_owned(),
        OpName::ResolverInstall => "Send .test names to Marsa".to_owned(),
        OpName::PfInstall => "Route ports 80 and 443 to Marsa".to_owned(),
        OpName::ResolvedInstall => "Send .test names to Marsa".to_owned(),
        OpName::PortsInstall => "Let Caddy use ports 80 and 443".to_owned(),
        // Chrome and Firefox on Linux.
        OpName::NssInstall => "Trust Marsa's certificates in Chrome and Firefox".to_owned(),
        // The hosts way on Linux.
        OpName::ResolvedRemove => "Take out Marsa's link for .test names".to_owned(),
        OpName::HostsSync => "Put the sites' .test names in the hosts file".to_owned(),
        OpName::HostsRemove => "Take Marsa's names out of the hosts file".to_owned(),
        other => other.as_str().to_owned(),
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
