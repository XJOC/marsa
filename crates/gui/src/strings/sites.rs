//! The Sites screen and a site's page: columns, folders, notices, and the confirmations of removals.

use std::path::{MAIN_SEPARATOR, Path};

use marsa_core::runtimes::Runtime;
use marsa_core::sites::TLD;
use marsa_core::types::{Site, SiteKind};
use semver::Version;

use super::*;

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

/// A site's own page. The way back to Sites at the top is the
/// word `SITES` after an arrow icon.
pub const OPEN_FOLDER: &str = "Open folder";
pub const OPEN_IN_BROWSER: &str = "Open in browser";
/// On a Laravel site's page: Logs, on the site's own log.
pub const OPEN_LOG: &str = "Open log";
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

/// The title of a folder dialog.
pub const PICK_PARK_TITLE: &str = "Choose a folder of projects to park";
pub const PICK_LINK_TITLE: &str = "Choose a project folder to link";

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
/// sentence: the site's page says the rest ([`unlink_parked_hint`]). A long
/// folder is left to the Forget button beside the sentence
/// ([`forget_folder`]): a notice is two lines at most, and a path written
/// with `\` has no place to wrap on Windows.
pub fn site_is_parked(site: &str, folder: Option<&Path>, home: Option<&Path>) -> String {
    match folder {
        Some(folder) => {
            let shown = home_path(folder, home);
            if shown.chars().count() <= MAX_ACTION_PATH {
                format!("{site}.{TLD} is part of the parked folder {shown}.")
            } else {
                format!("{site}.{TLD} is part of a parked folder.")
            }
        }
        None => error_text("site_is_parked"),
    }
}

/// The action of the notice [`site_is_parked`]: it asks to forget the folder.
/// A long path keeps its last parts (`…/clients/acme`): the sentence before
/// the button names no long path, and the tooltip of the button has the
/// whole one.
pub fn forget_folder(folder: &Path, home: Option<&Path>) -> String {
    forget_folder_shown(&path_tail(&home_path(folder, home), MAX_ACTION_PATH))
}

/// The tooltip of the action [`forget_folder`]: the whole path of the folder,
/// which the label may show only the end of.
pub fn forget_folder_hint(folder: &Path) -> String {
    format!(
        "Forget {}: its sites are no longer served. Its files stay.",
        folder.display()
    )
}

/// The longest path a button names in full.
const MAX_ACTION_PATH: usize = 22;

/// `shown`, or its last parts after an ellipsis if it is longer than `most`
/// characters: `~/Code/clients/acme/projects` is `…/clients/acme/projects`
/// at 25. A last part that is too long on its own is cut.
pub(super) fn path_tail(shown: &str, most: usize) -> String {
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

/// The title of the confirmation of an uninstall: the exact question.
pub fn uninstall_title(runtime: Runtime, version: &Version) -> String {
    format!("Uninstall {} {version}?", runtime_name(runtime))
}

/// The title of the confirmation of an unlink.
pub fn unlink_title(site: &str) -> String {
    format!("Unlink {site}.{TLD}?")
}

/// The title of the confirmation of a forget. A folder under `home` shows as
/// `~/…` ([`home_path`]).
pub fn forget_title(folder: &Path, home: Option<&Path>) -> String {
    format!("Forget {}?", home_path(folder, home))
}

/// What the confirmed uninstall does, and what stays.
pub fn uninstall_text(runtime: Runtime) -> String {
    format!(
        "Its files are deleted from Marsa's data folder. Marsa refuses if the default, a \
         site, or Caddy uses it. You can install {} again later.",
        runtime_name(runtime)
    )
}

/// What the confirmed unlink does, and what stays. `site_root`: the folder of
/// the site to unlink; a folder under `home` shows as `~/…`.
pub fn unlink_text(site_root: Option<&Path>, home: Option<&Path>) -> String {
    let kept = site_root.map_or_else(
        || "Its folder and files stay.".to_owned(),
        |root| format!("The folder {} and its files stay.", home_path(root, home)),
    );
    format!(
        "Marsa stops serving this site, and its own settings (PHP version, HTTPS) are \
         removed. {kept}"
    )
}

/// What the confirmed forget does, and what stays. `sites`: how many sites the
/// folder gives now.
pub fn forget_text(sites: usize) -> String {
    let served = match sites {
        0 => "Marsa stops watching this folder.".to_owned(),
        1 => "Marsa stops serving its site, and the site's own settings are removed.".to_owned(),
        n => format!("Marsa stops serving its {n} sites, and their own settings are removed."),
    };
    format!("{served} The folder and its files stay.")
}

/// The buttons that confirm.
pub const UNINSTALL_CONFIRM: &str = "Uninstall";
pub const UNLINK_CONFIRM: &str = "Unlink";
pub const FORGET_CONFIRM: &str = "Forget";

/// While the daemon removes it.
pub fn uninstalling(runtime: Runtime, version: &Version) -> String {
    format!("Uninstalling {} {version}…", runtime_name(runtime))
}

pub fn unlinking(site: &str) -> String {
    format!("Unlinking {site}.{TLD}…")
}

pub fn forgetting(folder: &Path) -> String {
    format!("Forgetting {}…", folder.display())
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
