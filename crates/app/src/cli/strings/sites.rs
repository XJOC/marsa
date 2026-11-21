//! `marsa sites`, `park`, `link`, `secure`, `unlink`, `isolate`, and `open`.

use marsa_core::types::SiteKind;

use super::*;

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

// `marsa unlink`, `forget`, `isolate`, `unisolate`, and `open`.

/// `marsa unlink`.
pub fn unlinked(name: &str) -> String {
    format!("Unlinked {name}: it is no longer served, and its settings are gone.")
}

/// `marsa unlink`, when a parked folder has a site of the same name, which
/// is served again.
pub fn unlinked_parked_back(name: &str, url: &str, folder: &str) -> String {
    format!("Unlinked {name}. {url} now serves {folder}, from a parked folder.")
}

/// `marsa forget`.
pub fn forgot(folder: &str) -> String {
    format!("Forgot {folder}: its sites are no longer served.")
}

/// `marsa isolate`: the version asked for, and the installed one it picks
/// now, if any.
pub fn isolated(url: &str, asked: &str, version: Option<&str>) -> String {
    match version {
        Some(version) if version == asked => format!("{url} now runs PHP {version}."),
        Some(version) => format!("{url} now runs PHP {asked} ({version})."),
        None => format!(
            "{url} now asks for PHP {asked}, but no installed version matches. Run \"marsa \
             install php {asked}\"."
        ),
    }
}

/// `marsa unisolate`: the installed version the site runs now, if any.
pub fn unisolated(url: &str, version: Option<&str>) -> String {
    match version {
        Some(version) => {
            format!("{url} has no PHP version of its own now, and runs PHP {version}.")
        }
        None => {
            format!("{url} has no PHP version of its own now. No installed PHP version fits it.")
        }
    }
}

/// `marsa open`.
pub fn opened(url: &str) -> String {
    format!("Opened {url} in your browser.")
}
