//! `marsa install`, `runtimes`, `use`, `uninstall`, and `xdebug`.

use std::path::Path;

use marsa_core::runtimes::Runtime;
use marsa_core::types::VersionInUse;

use super::*;

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

// `marsa uninstall`.

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

/// `version_in_use` or `sites_lose_php` with its facts: one sentence for
/// each use, each with what to do first.
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
    let left = match &in_use.sites_left_without_php[..] {
        [] => None,
        [site] => Some(format!("The site {site}")),
        sites => Some(format!("The sites {}", sites.join(", "))),
    };
    if let Some(left) = left {
        let tool = runtime.dir_name();
        text.push_str(&format!(
            " {left} would have no {name}: {name} {version} is the only {name} installed. \
             Install another version first with \"marsa install {tool} <version>\", then make \
             it the default with \"marsa use {tool} <version>\"."
        ));
    }
    if in_use.caddy {
        text.push_str(" Caddy runs from it: stop Caddy first with \"marsa services stop caddy\".");
    }
    if in_use.server {
        text.push_str(&format!(
            " {name} runs from it: stop it first with \"marsa services stop {}\". Its \
             databases stay either way.",
            runtime.dir_name()
        ));
    }
    text
}

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
