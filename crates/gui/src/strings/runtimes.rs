//! Runtimes, installs, uninstalls, and Xdebug.

use marsa_core::runtimes::Runtime;
use marsa_core::sites::TLD;
use marsa_core::types::{InstallFailure, VersionInUse, XdebugState};
use semver::Version;

use crate::model::{InstallStep, InstallView, UninstallNotice};

use super::*;

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

// The Runtimes screen: the list, the details, and the Installed table.

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

// Xdebug (`GET` and `PUT /v1/php/xdebug`).

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
/// and shows with the default arm until it gets its own.
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
pub(super) const MAX_PRINTED: usize = 60;
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

/// The name of a runtime, as the CLI writes it.
pub fn runtime_name(runtime: Runtime) -> &'static str {
    runtime_name_for(runtime.dir_name())
}

/// [`runtime_name`] by the runtime's folder name (`runtimes/<name>`), which
/// is the one name core gives a runtime. The `match` is on that text, not on
/// `Runtime`: a runtime that core adds later builds without a change here and
/// shows under its folder name (`python`) until it gets a nicer one
///.
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

/// The first row of a site's version menu: it clears the site's own setting.
pub const USE_SITE_FILES: &str = "Use the site's files";

/// The name of the dashed "+" of a version picker: "Install another PHP
/// version".
pub fn install_another_label(runtime: &str) -> String {
    format!("Install another {runtime} version")
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
