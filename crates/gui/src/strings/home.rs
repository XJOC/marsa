//! Home: the status sentence, its fixes, and the request path.

use crate::widgets::StepState;

// Home: the status sentence, its fixes, the default PHP, and the
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
