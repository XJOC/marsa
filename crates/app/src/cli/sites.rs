//! `marsa sites`, `marsa park`, `marsa link`, `marsa secure`, and
//! `marsa unsecure`: the sites the daemon serves. Task M4-10c. Then `marsa
//! unlink`, `marsa forget`, `marsa isolate`, `marsa unisolate`, and `marsa
//! open` (task M5-09).
//!
//! Each command is one call to `marsad` (the routes in
//! `daemon/sites_api.rs`), started if needed. A folder the user names is made
//! absolute here, against the current folder, without reading the disk: the
//! daemon keeps absolute paths only, and `canonicalize` would give `\\?\`
//! paths on Windows, which must not reach Caddy.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use marsa_core::client::{ClientError, DaemonClient};
use marsa_core::config::ConfigPaths;
use marsa_core::platform::{OpenError, open_url};
use marsa_core::runtimes::Runtime;
use marsa_core::sites;
use marsa_core::types::{Site, SiteList, SitePatch};

use super::daemon::{ClientFailure, connect, failure};
use super::strings::{self, Details};

/// The sites commands. `bin/marsa.rs` flattens them into the top level, so
/// each is a command of its own: `marsa park`, not `marsa sites park`.
#[derive(Debug, Clone, PartialEq, Eq, clap::Subcommand)]
pub enum SitesCommand {
    /// List the sites, then the folders that did not become one.
    Sites,
    /// Serve every sub-folder of a folder as a site, named after it.
    Park {
        /// The folder. Default: the current folder.
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,
    },
    /// Serve one folder as a site.
    Link {
        /// The site name, as in https://<name>.test. Default: made from the
        /// folder name.
        name: Option<String>,
        /// The folder. Default: the current folder.
        #[arg(long, value_name = "DIR")]
        path: Option<PathBuf>,
    },
    /// Serve a site over HTTPS (the default).
    Secure {
        /// The site name.
        name: String,
    },
    /// Serve a site over plain HTTP.
    Unsecure {
        /// The site name.
        name: String,
    },
    /// Stop serving a linked site. Its settings (PHP version, HTTPS) go too.
    Unlink {
        /// The site name.
        name: String,
    },
    /// Stop serving a parked folder: its sites, and their settings, go.
    Forget {
        /// The folder. Default: the current folder.
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,
    },
    /// Run a site with its own PHP version.
    Isolate {
        /// The site name.
        name: String,
        /// The PHP version, as in a .php-version file: 8.3 or 8.3.27.
        php: String,
    },
    /// Run a site with the PHP version its files or the default give.
    Unisolate {
        /// The site name.
        name: String,
    },
    /// Open a site in the browser.
    Open {
        /// The site name.
        name: String,
    },
}

/// What a command asks the daemon: every folder absolute, every name checked.
/// `Isolate` with `php: None` clears the site's own version (`marsa
/// unisolate`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    List,
    Park { folder: PathBuf },
    Link { name: String, folder: PathBuf },
    Secure { name: String, secure: bool },
    Unlink { name: String },
    Forget { folder: PathBuf },
    Isolate { name: String, php: Option<String> },
    Open { name: String },
}

impl Action {
    /// The action of `command`. `cwd` gives the current folder; it is asked
    /// only for a missing or relative folder.
    ///
    /// # Errors
    ///
    /// `no_current_dir` if `cwd` fails, and `site_name_invalid` for a name
    /// that is not valid or that the folder name cannot give.
    pub fn new(
        command: &SitesCommand,
        cwd: impl FnOnce() -> io::Result<PathBuf>,
    ) -> Result<Self, ClientFailure> {
        Ok(match command {
            SitesCommand::Sites => Action::List,
            SitesCommand::Park { dir } => Action::Park {
                folder: folder(dir.as_deref(), cwd)?,
            },
            SitesCommand::Link { name, path } => {
                let folder = folder(path.as_deref(), cwd)?;
                let name = link_name(name.as_deref(), &folder)?;
                Action::Link { name, folder }
            }
            SitesCommand::Secure { name } => Action::Secure {
                name: checked(name)?,
                secure: true,
            },
            SitesCommand::Unsecure { name } => Action::Secure {
                name: checked(name)?,
                secure: false,
            },
            SitesCommand::Unlink { name } => Action::Unlink {
                name: checked(name)?,
            },
            SitesCommand::Forget { dir } => Action::Forget {
                folder: folder(dir.as_deref(), cwd)?,
            },
            SitesCommand::Isolate { name, php } => Action::Isolate {
                name: checked(name)?,
                php: Some(php.clone()),
            },
            SitesCommand::Unisolate { name } => Action::Isolate {
                name: checked(name)?,
                php: None,
            },
            SitesCommand::Open { name } => Action::Open {
                name: checked(name)?,
            },
        })
    }

    async fn send(&self, client: &DaemonClient) -> Result<SiteList, ClientError> {
        match self {
            Action::List | Action::Open { .. } => client.sites().await,
            Action::Park { folder } => client.park(folder).await,
            Action::Link { name, folder } => client.link(name, folder).await,
            Action::Secure { name, secure } => {
                let patch = SitePatch {
                    secure: Some(*secure),
                    ..SitePatch::default()
                };
                client.update_site(name, &patch).await
            }
            Action::Unlink { name } => client.unlink(name).await,
            Action::Forget { folder } => client.forget(folder).await,
            Action::Isolate { name, php } => {
                let patch = SitePatch {
                    php: Some(php.clone()),
                    ..SitePatch::default()
                };
                client.update_site(name, &patch).await
            }
        }
    }

    /// What to print for the daemon's answer: [`Action::render`], except for
    /// `marsa open`, which opens the site's address here with `open`.
    ///
    /// # Errors
    ///
    /// `site_not_found` when the list has no site of that name, and the code
    /// of `open`'s error.
    pub fn finish(
        &self,
        list: &SiteList,
        open: impl FnOnce(&str) -> Result<(), OpenError>,
    ) -> Result<String, ClientFailure> {
        let Action::Open { name } = self else {
            return Ok(self.render(list));
        };
        let Some(site) = list.sites.iter().find(|site| site.name == *name) else {
            let details = Details {
                site: Some(name.as_str()),
                ..Details::default()
            };
            return Err(local("site_not_found", &details));
        };
        let url = url(site);
        open(&url).map_err(|err| {
            tracing::debug!(code = err.code(), error = %err, "cannot open the browser");
            let details = Details {
                url: Some(&url),
                ..Details::default()
            };
            local(err.code(), &details)
        })?;
        Ok(format!("{}\n", strings::opened(&url)))
    }

    /// What to print for the daemon's answer: the table for `sites` and
    /// `park`, one line for the others. If that site is not in the answer as
    /// asked, the table instead.
    pub fn render(&self, list: &SiteList) -> String {
        let site = |name: &str| list.sites.iter().find(|site| site.name == name);
        let php = |site: &Site| site.php.as_ref().map(ToString::to_string);
        let line = match self {
            Action::List | Action::Park { .. } | Action::Open { .. } => None,
            Action::Link { name, folder } => site(name)
                .filter(|site| site.root == *folder)
                .map(|site| strings::linked(&url(site), &shown(&site.root))),
            Action::Secure { name, secure } => site(name)
                .filter(|site| site.secure == *secure)
                .map(|site| {
                    if *secure {
                        strings::secured(&url(site))
                    } else {
                        strings::unsecured(&url(site))
                    }
                }),
            // A parked folder's site of the same name may be served again.
            Action::Unlink { name } => Some(match site(name) {
                Some(site) => strings::unlinked_parked_back(name, &url(site), &shown(&site.root)),
                None => strings::unlinked(name),
            }),
            Action::Forget { folder } => Some(strings::forgot(&shown(folder))),
            Action::Isolate {
                name,
                php: Some(asked),
            } => site(name).map(|site| {
                let asked = printable(asked.trim());
                strings::isolated(&url(site), &asked, php(site).as_deref())
            }),
            Action::Isolate { name, php: None } => {
                site(name).map(|site| strings::unisolated(&url(site), php(site).as_deref()))
            }
        };
        match line {
            Some(line) => format!("{line}\n"),
            None => render(list),
        }
    }

    /// The text and code the user sees for `err`. A site code names the site
    /// and the folder of this action, and a `config_*` code names
    /// `sites.toml`, the file the daemon could not use.
    pub fn failure(&self, err: &ClientError, data_dir: &Path) -> ClientFailure {
        self.failure_with(err, data_dir, None)
    }

    /// [`Action::failure`], with the site list read after the error, if any:
    /// `site_is_parked` then names the parked folder of the site, and
    /// `folder_not_parked` the parked folders. A version code of `marsa
    /// isolate` names the version.
    pub fn failure_with(
        &self,
        err: &ClientError,
        data_dir: &Path,
        list: Option<&SiteList>,
    ) -> ClientFailure {
        let mut failure = failure(err, data_dir, None);
        let (site, folder) = match self {
            Action::List => (None, None),
            Action::Park { folder } | Action::Forget { folder } => (None, Some(folder.as_path())),
            Action::Link { name, folder } => (Some(name.as_str()), Some(folder.as_path())),
            Action::Secure { name, .. }
            | Action::Unlink { name }
            | Action::Isolate { name, .. }
            | Action::Open { name } => (Some(name.as_str()), None),
        };
        let requested = match self {
            Action::Isolate { php: Some(php), .. } => Some(printable(php.trim())),
            _ => None,
        };
        let parked: Vec<String> = list.map_or_else(Vec::new, |list| {
            list.parked.iter().map(|dir| shown(dir)).collect()
        });
        let holder = list
            .zip(site)
            .and_then(|(list, name)| parked_folder(list, name));
        let sites_toml = ConfigPaths::in_dir(data_dir).sites;
        let path = match failure.code.as_str() {
            "config_read" | "config_invalid" | "config_write" => Some(sites_toml.as_path()),
            "site_is_parked" => holder.as_deref(),
            code if code.starts_with("site_") || code == "folder_not_parked" => folder,
            "invalid_version" | "unsupported_alias" if requested.is_some() => None,
            _ => return failure,
        };
        let details = Details {
            runtime: requested.as_ref().map(|_| Runtime::Php),
            requested: requested.as_deref(),
            available: &parked,
            site,
            path,
            ..Details::default()
        };
        failure.message = strings::message(&failure.code, &details);
        failure
    }
}

/// The parked folder in `list` that holds the site `name`.
fn parked_folder(list: &SiteList, name: &str) -> Option<PathBuf> {
    let site = list.sites.iter().find(|site| site.name == name)?;
    let parent = site.root.parent()?;
    list.parked.iter().find(|dir| *dir == parent).cloned()
}

/// Starts the daemon if needed, sends the command, and prints the answer.
/// Exit code 1 if the arguments or the call fail.
pub async fn run(command: &SitesCommand, data_dir: &Path) -> ExitCode {
    let action = match Action::new(command, std::env::current_dir) {
        Ok(action) => action,
        Err(failure) => return failure.report(),
    };
    let client = match connect(data_dir).await {
        Ok(client) => client,
        Err(failure) => return failure.report(),
    };
    match action.send(&client).await {
        Ok(list) => match action.finish(&list, open_url) {
            Ok(text) => {
                print!("{text}");
                ExitCode::SUCCESS
            }
            Err(failure) => failure.report(),
        },
        Err(err) => {
            // These answers name no folder; the site list does.
            let list = match err.code() {
                "site_is_parked" | "folder_not_parked" => client.sites().await.ok(),
                _ => None,
            };
            action.failure_with(&err, data_dir, list.as_ref()).report()
        }
    }
}

/// `dir`, or the current folder, as an absolute path. `cwd` is asked only
/// when `dir` is missing or relative.
///
/// # Errors
///
/// `no_current_dir` if `cwd` fails.
pub fn folder(
    dir: Option<&Path>,
    cwd: impl FnOnce() -> io::Result<PathBuf>,
) -> Result<PathBuf, ClientFailure> {
    let dir = dir.unwrap_or(Path::new(""));
    if dir.is_absolute() {
        return Ok(dir.components().collect());
    }
    let cwd = cwd().map_err(|err| {
        tracing::debug!(error = %err, "cannot read the current folder");
        local("no_current_dir", &Details::default())
    })?;
    Ok(absolute_in(dir, &cwd))
}

/// `dir` against `cwd`, without reading the disk. `.` parts and doubled
/// separators go; `..` stays, because only the disk knows where it leads
/// through a link.
pub fn absolute_in(dir: &Path, cwd: &Path) -> PathBuf {
    cwd.join(dir).components().collect()
}

/// The site name of `marsa link`: `name` if given, else the one
/// [`sites::name_from_folder`] makes from the last part of `folder`.
///
/// # Errors
///
/// `site_name_invalid`, with a hint to pass a name when it came from the
/// folder.
pub fn link_name(name: Option<&str>, folder: &Path) -> Result<String, ClientFailure> {
    if let Some(name) = name {
        return checked(name);
    }
    let last = folder.file_name();
    last.and_then(sites::name_from_folder).ok_or_else(|| {
        // `/`, or a path that ends in `..`, has no last part: show it all.
        let named = last.map_or_else(|| shown(folder), |last| printable(&last.to_string_lossy()));
        let details = Details {
            site: Some(&named),
            ..Details::default()
        };
        let mut failure = local("site_name_invalid", &details);
        failure.message = format!("{} {}", failure.message, strings::PASS_A_NAME);
        failure
    })
}

/// `name` if it is a valid site name. No site has any other name, so the
/// daemon need not be asked.
fn checked(name: &str) -> Result<String, ClientFailure> {
    match sites::validate_name(name) {
        Ok(()) => Ok(name.to_owned()),
        Err(err) => {
            let named = printable(name);
            let details = Details {
                site: Some(&named),
                ..Details::default()
            };
            Err(local(err.code(), &details))
        }
    }
}

/// A failure found before any call.
fn local(code: &str, details: &Details<'_>) -> ClientFailure {
    ClientFailure {
        code: code.to_owned(),
        message: strings::message(code, details),
    }
}

/// The table of sites, then the folders that did not become a site and why.
pub fn render(list: &SiteList) -> String {
    let mut text = if list.sites.is_empty() {
        format!("{}\n", strings::NO_SITES)
    } else {
        table(&list.sites)
    };
    if !list.skipped.is_empty() {
        text.push('\n');
        text.push_str(strings::SKIPPED_HEADER);
        text.push('\n');
        for folder in &list.skipped {
            let code = printable(&folder.code);
            let reason = strings::skip_reason(&code);
            text.push_str(&strings::skipped_folder(
                &shown(&folder.path),
                &reason,
                &code,
            ));
            text.push('\n');
        }
    }
    text
}

/// `https://shop.test` for a secure site, else `http://shop.test`.
pub fn url(site: &Site) -> String {
    let scheme = if site.secure { "https" } else { "http" };
    format!("{scheme}://{}", printable(&site.host()))
}

/// One row per site, with a title row. Each column is as wide as its widest
/// cell, and two spaces apart, as in `marsa services`.
fn table(sites: &[Site]) -> String {
    let mut rows = vec![strings::SITES_HEADER.map(str::to_owned)];
    rows.extend(sites.iter().map(|site| {
        [
            printable(&site.name),
            url(site),
            strings::kind_name(site.kind),
            site.php
                .as_ref()
                .map_or_else(|| strings::NO_PHP.to_owned(), ToString::to_string),
            shown(&site.root),
        ]
    }));
    let mut widths = [0; 5];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut text = String::new();
    for row in &rows {
        let mut line = String::new();
        for (cell, width) in row.iter().zip(widths) {
            line.push_str(&format!("{cell:<width$}  "));
        }
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text
}

/// A path, safe to print.
fn shown(path: &Path) -> String {
    printable(&path.to_string_lossy())
}

/// Text from the daemon or a folder name, safe to print: no control
/// characters, which could move the cursor.
pub(super) fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;
    use marsa_core::types::{SiteKind, SkippedFolder};

    #[derive(Debug, clap::Parser)]
    struct Cli {
        #[command(subcommand)]
        command: SitesCommand,
    }

    fn parse(args: &[&str]) -> Result<SitesCommand, clap::Error> {
        let args = std::iter::once("marsa").chain(args.iter().copied());
        Cli::try_parse_from(args).map(|cli| cli.command)
    }

    /// An absolute folder on every system. Nothing is created there.
    fn base() -> PathBuf {
        std::env::temp_dir().join("marsa-cli-sites")
    }

    fn no_cwd() -> io::Result<PathBuf> {
        panic!("the current folder was asked for")
    }

    /// From JSON, as the daemon sends it: this crate does not depend on
    /// `semver`, the type of `php`.
    fn site(name: &str, kind: SiteKind, php: Option<&str>, secure: bool) -> Site {
        let root = format!("/code/{name}");
        serde_json::from_value(serde_json::json!({
            "name": name, "root": root, "docroot": format!("{root}/public"),
            "kind": kind, "php": php, "secure": secure,
        }))
        .unwrap()
    }

    fn list() -> SiteList {
        SiteList {
            parked: vec![PathBuf::from("/code")],
            sites: vec![
                site("blog", SiteKind::Static, None, false),
                site("shop", SiteKind::Laravel, Some("8.4.23"), true),
            ],
            skipped: vec![SkippedFolder {
                path: PathBuf::from("/code/متجر"),
                code: "site_name_invalid".into(),
            }],
        }
    }

    #[test]
    fn each_command_parses_its_arguments() {
        let path = |p: &str| Some(PathBuf::from(p));
        for (args, want) in [
            (&["sites"][..], SitesCommand::Sites),
            (&["park"], SitesCommand::Park { dir: None }),
            (&["park", "code"], SitesCommand::Park { dir: path("code") }),
            (
                &["link"],
                SitesCommand::Link {
                    name: None,
                    path: None,
                },
            ),
            (
                &["link", "api"],
                SitesCommand::Link {
                    name: Some("api".into()),
                    path: None,
                },
            ),
            (
                &["link", "api", "--path", "../work/api"],
                SitesCommand::Link {
                    name: Some("api".into()),
                    path: path("../work/api"),
                },
            ),
            (
                &["link", "--path", "work"],
                SitesCommand::Link {
                    name: None,
                    path: path("work"),
                },
            ),
            (
                &["secure", "shop"],
                SitesCommand::Secure {
                    name: "shop".into(),
                },
            ),
            (
                &["unsecure", "shop"],
                SitesCommand::Unsecure {
                    name: "shop".into(),
                },
            ),
        ] {
            assert_eq!(parse(args).expect("valid arguments"), want, "{args:?}");
        }
    }

    #[test]
    fn bad_arguments_exit_with_code_2() {
        for args in [
            &["sites", "shop"][..],
            &["park", "a", "b"],
            &["link", "a", "b"],
            &["link", "--path"],
            &["secure"],
            &["unsecure", "a", "b"],
        ] {
            let err = parse(args).expect_err("invalid arguments");
            assert_eq!(err.exit_code(), 2, "{args:?}: {:?}", err.kind());
        }
    }

    #[test]
    fn a_relative_folder_is_joined_to_the_current_one() {
        let cwd = base();
        let from = |dir: Option<&str>| folder(dir.map(Path::new), || Ok(cwd.clone())).unwrap();
        assert_eq!(from(None), cwd);
        assert_eq!(from(Some(".")), cwd);
        assert_eq!(from(Some("code")), cwd.join("code"));
        assert_eq!(
            from(Some("./code/./shop")),
            cwd.join("code").join("shop"),
            "`.` parts go"
        );
        // `..` stays: no disk is read, so a link is not followed.
        assert_eq!(from(Some("../code")), cwd.join("..").join("code"));
        assert!(from(Some("../code")).is_absolute());
    }

    #[test]
    fn an_absolute_folder_does_not_ask_for_the_current_one() {
        let dir = base().join("code");
        assert_eq!(folder(Some(dir.as_path()), no_cwd).unwrap(), dir);
        let dotted = base().join(".").join("code");
        assert_eq!(folder(Some(dotted.as_path()), no_cwd).unwrap(), dir);
    }

    #[test]
    fn no_current_folder_has_its_own_code() {
        let gone = || Err(io::Error::from(io::ErrorKind::NotFound));
        let err = folder(None, gone).unwrap_err();
        assert_eq!(err.code, "no_current_dir");
        assert!(err.message.starts_with("Cannot read the current folder"));
        let err = Action::new(&SitesCommand::Park { dir: None }, gone).unwrap_err();
        assert_eq!(err.code, "no_current_dir");
    }

    #[test]
    fn the_default_name_comes_from_the_folder_name() {
        let named = |dir: &str| link_name(None, &base().join(dir));
        assert_eq!(named("shop").unwrap(), "shop");
        assert_eq!(named("My_App.v2").unwrap(), "my-app-v2");
        assert_eq!(link_name(Some("api"), &base().join("x")).unwrap(), "api");

        let err = named("متجر").unwrap_err();
        assert_eq!(err.code, "site_name_invalid");
        assert_eq!(
            err.message,
            "\"متجر\" is not a valid site name. Use 1 to 63 of a-z, 0-9, and -, with no - at \
             the start or the end. Pass a name: marsa link <name>"
        );
        // A path that ends in `..` has no last part.
        let up = base().join("..");
        let err = link_name(None, &up).unwrap_err();
        assert_eq!(err.code, "site_name_invalid");
        assert!(err.message.contains(&shown(&up)), "{}", err.message);
    }

    #[test]
    fn a_name_that_is_not_valid_is_refused_before_any_call() {
        let err = link_name(Some("Shop"), &base()).unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.message.as_str()),
            (
                "site_name_invalid",
                "\"Shop\" is not a valid site name. Use 1 to 63 of a-z, 0-9, and -, with no - \
                 at the start or the end."
            )
        );
        for name in ["..", "", "a/b", "-a"] {
            let command = SitesCommand::Secure { name: name.into() };
            let err = Action::new(&command, no_cwd).unwrap_err();
            assert_eq!(err.code, "site_name_invalid", "{name:?}");
        }
    }

    #[test]
    fn each_command_becomes_its_action() {
        let cwd = base();
        let current = || Ok(cwd.clone());
        let link = SitesCommand::Link {
            name: None,
            path: Some("work/api".into()),
        };
        assert_eq!(
            Action::new(&link, current).unwrap(),
            Action::Link {
                name: "api".into(),
                folder: cwd.join("work").join("api"),
            }
        );
        let unsecure = SitesCommand::Unsecure {
            name: "shop".into(),
        };
        assert_eq!(
            Action::new(&unsecure, no_cwd).unwrap(),
            Action::Secure {
                name: "shop".into(),
                secure: false,
            }
        );
        assert_eq!(
            Action::new(&SitesCommand::Sites, no_cwd).unwrap(),
            Action::List
        );
    }

    #[test]
    fn the_table_shows_each_site_then_the_skipped_folders() {
        assert_eq!(
            render(&list()),
            "NAME  URL                KIND     PHP     FOLDER\n\
             blog  http://blog.test   static   -       /code/blog\n\
             shop  https://shop.test  laravel  8.4.23  /code/shop\n\
             \n\
             Skipped folders:\n  \
             /code/متجر: its name gives no valid site name [site_name_invalid]\n"
        );
        assert_eq!(Action::List.render(&list()), render(&list()));
    }

    #[test]
    fn no_sites_is_a_sentence_and_a_proxy_shows_its_port() {
        assert_eq!(
            render(&SiteList::default()),
            "No sites yet. Run \"marsa park\" in a folder of projects, or \"marsa link\" in \
             one project.\n"
        );
        let proxy = SiteList {
            sites: vec![site("app", SiteKind::NodeProxy { port: 5173 }, None, true)],
            ..SiteList::default()
        };
        assert_eq!(
            render(&proxy),
            "NAME  URL               KIND         PHP  FOLDER\n\
             app   https://app.test  proxy :5173  -    /code/app\n"
        );
    }

    #[test]
    fn text_from_the_daemon_has_no_control_characters() {
        let odd = SiteList {
            skipped: vec![SkippedFolder {
                path: PathBuf::from("/code/a\u{1b}[2Jb"),
                code: "site_\nname_taken".into(),
            }],
            ..SiteList::default()
        };
        let text = render(&odd);
        assert!(
            text.ends_with(
                "  /code/a[2Jb: another folder has the same site name [site_name_taken]\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn link_and_secure_print_one_line() {
        let link = Action::Link {
            name: "shop".into(),
            folder: PathBuf::from("/code/shop"),
        };
        assert_eq!(
            link.render(&list()),
            "Linked /code/shop as https://shop.test.\n"
        );
        let secure = |name: &str, secure| Action::Secure {
            name: name.into(),
            secure,
        };
        assert_eq!(
            secure("shop", true).render(&list()),
            "https://shop.test is served over HTTPS.\n"
        );
        assert_eq!(
            secure("blog", false).render(&list()),
            "http://blog.test is served over plain HTTP.\n"
        );
        // Not in the answer as asked: the whole table.
        assert_eq!(secure("blog", true).render(&list()), render(&list()));
        let elsewhere = Action::Link {
            name: "shop".into(),
            folder: PathBuf::from("/work/shop"),
        };
        assert_eq!(elsewhere.render(&list()), render(&list()));
    }

    #[test]
    fn a_daemon_error_names_the_site_the_folder_or_sites_toml() {
        let data = base().join("data");
        let api = |code: &str| ClientError::Api {
            status: 400,
            code: code.into(),
            message: "x".into(),
        };
        let secure = Action::Secure {
            name: "nope".into(),
            secure: true,
        };
        let failure = secure.failure(&api("site_not_found"), &data);
        assert_eq!(
            failure.line(),
            "marsa: There is no site named \"nope\". Run \"marsa sites\" to see them. \
             [site_not_found]"
        );
        let file = base().join("notes.txt");
        let park = Action::Park {
            folder: file.clone(),
        };
        let failure = park.failure(&api("site_not_a_folder"), &data);
        assert_eq!(
            failure.message,
            format!(
                "{} is not a folder. Check the path, then try again.",
                file.display()
            )
        );
        let failure = Action::List.failure(&api("config_invalid"), &data);
        let sites_toml = data.join("config").join("sites.toml");
        assert!(
            failure
                .message
                .starts_with(&format!("{} has an error", sites_toml.display())),
            "{}",
            failure.message
        );
        // Other codes keep the text every command gives them.
        let failure = park.failure(&ClientError::NotRunning, &data);
        assert_eq!(failure.code, "daemon_not_running");
        assert!(
            failure
                .message
                .starts_with("The Marsa daemon is not running.")
        );
    }

    // M5-09: `unlink`, `forget`, `isolate`, `unisolate`, and `open`.

    #[test]
    fn the_new_commands_parse_their_arguments() {
        let name = || "shop".to_owned();
        for (args, want) in [
            (
                &["unlink", "shop"][..],
                SitesCommand::Unlink { name: name() },
            ),
            (&["forget"], SitesCommand::Forget { dir: None }),
            (
                &["forget", "code"],
                SitesCommand::Forget {
                    dir: Some("code".into()),
                },
            ),
            (
                &["isolate", "shop", "8.3"],
                SitesCommand::Isolate {
                    name: name(),
                    php: "8.3".into(),
                },
            ),
            (
                &["unisolate", "shop"],
                SitesCommand::Unisolate { name: name() },
            ),
            (&["open", "shop"], SitesCommand::Open { name: name() }),
        ] {
            assert_eq!(parse(args).expect("valid arguments"), want, "{args:?}");
        }
        for args in [
            &["unlink"][..],
            &["forget", "a", "b"],
            &["isolate", "shop"],
            &["isolate", "shop", "8.3", "8.4"],
            &["unisolate"],
            &["open"],
            &["open", "a", "b"],
        ] {
            let err = parse(args).expect_err("invalid arguments");
            assert_eq!(err.exit_code(), 2, "{args:?}: {:?}", err.kind());
        }
    }

    #[test]
    fn the_new_commands_become_their_actions() {
        let cwd = base();
        let action = |command: SitesCommand| Action::new(&command, || Ok(cwd.clone()));
        let forget = |dir: Option<&str>| SitesCommand::Forget {
            dir: dir.map(PathBuf::from),
        };
        // The folder is made absolute as `marsa park` makes it.
        let here = Action::Forget {
            folder: cwd.clone(),
        };
        assert_eq!(action(forget(None)).unwrap(), here);
        let code = Action::Forget {
            folder: cwd.join("code"),
        };
        assert_eq!(action(forget(Some("./code/"))).unwrap(), code);
        let isolate = SitesCommand::Isolate {
            name: "shop".into(),
            php: "8.3".into(),
        };
        let want = Action::Isolate {
            name: "shop".into(),
            php: Some("8.3".into()),
        };
        assert_eq!(Action::new(&isolate, no_cwd).unwrap(), want);
        let unisolate = SitesCommand::Unisolate {
            name: "shop".into(),
        };
        let want = Action::Isolate {
            name: "shop".into(),
            php: None,
        };
        assert_eq!(Action::new(&unisolate, no_cwd).unwrap(), want);

        // A name that is not valid is refused before any call.
        for command in [
            SitesCommand::Unlink {
                name: "Shop".into(),
            },
            SitesCommand::Isolate {
                name: "a/b".into(),
                php: "8.3".into(),
            },
            SitesCommand::Unisolate { name: "-a".into() },
            SitesCommand::Open { name: "..".into() },
        ] {
            let err = Action::new(&command, no_cwd).unwrap_err();
            assert_eq!(err.code, "site_name_invalid", "{command:?}");
        }
    }

    #[test]
    fn unlink_forget_isolate_and_unisolate_print_one_line() {
        let unlink = |name: &str| Action::Unlink { name: name.into() };
        assert_eq!(
            unlink("api").render(&list()),
            "Unlinked api: it is no longer served, and its settings are gone.\n"
        );
        // A parked `shop` is served again.
        assert_eq!(
            unlink("shop").render(&list()),
            "Unlinked shop. https://shop.test now serves /code/shop, from a parked folder.\n"
        );
        let forget = Action::Forget {
            folder: PathBuf::from("/code"),
        };
        assert_eq!(
            forget.render(&SiteList::default()),
            "Forgot /code: its sites are no longer served.\n"
        );

        let isolate = |name: &str, php: Option<&str>| Action::Isolate {
            name: name.into(),
            php: php.map(str::to_owned),
        };
        for (action, want) in [
            (
                isolate("shop", Some(" 8.4 ")),
                "https://shop.test now runs PHP 8.4 (8.4.23).\n",
            ),
            (
                isolate("shop", Some("8.4.23")),
                "https://shop.test now runs PHP 8.4.23.\n",
            ),
            (
                isolate("blog", Some("7.4")),
                "http://blog.test now asks for PHP 7.4, but no installed version matches. Run \
                 \"marsa install php 7.4\".\n",
            ),
            (
                isolate("shop", None),
                "https://shop.test has no PHP version of its own now, and runs PHP 8.4.23.\n",
            ),
            (
                isolate("blog", None),
                "http://blog.test has no PHP version of its own now. No installed PHP version \
                 fits it.\n",
            ),
        ] {
            assert_eq!(action.render(&list()), want, "{action:?}");
        }
        // Not in the answer: the whole table.
        assert_eq!(isolate("nope", None).render(&list()), render(&list()));
    }

    #[test]
    fn open_opens_the_address_of_the_site_and_nothing_else() {
        let open = |name: &str| Action::Open { name: name.into() };
        let mut opened = Vec::new();
        for name in ["shop", "blog"] {
            let text = open(name)
                .finish(&list(), |url| {
                    opened.push(url.to_owned());
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                text,
                format!("Opened {} in your browser.\n", opened[opened.len() - 1])
            );
        }
        assert_eq!(opened, ["https://shop.test", "http://blog.test"]);

        let never = |url: &str| -> Result<(), OpenError> { panic!("opened {url}") };
        let err = open("nope").finish(&list(), never).unwrap_err();
        assert_eq!(
            err.line(),
            "marsa: There is no site named \"nope\". Run \"marsa sites\" to see them. \
             [site_not_found]"
        );
        let none = |_: &str| {
            Err(OpenError::NoOpener {
                tried: "xdg-open".into(),
            })
        };
        assert_eq!(
            open("shop").finish(&list(), none).unwrap_err().line(),
            "marsa: Cannot find a program that opens the browser (xdg-open on Linux). Open \
             https://shop.test yourself. [open_no_program]"
        );
        let failed = |_: &str| {
            Err(OpenError::Spawn {
                program: PathBuf::from("/usr/bin/open"),
                source: io::Error::other("x"),
            })
        };
        assert_eq!(
            open("shop").finish(&list(), failed).unwrap_err().line(),
            "marsa: Cannot open the browser. Open https://shop.test yourself. [open_failed]"
        );
        // The other actions print what `render` gives, and open nothing.
        assert_eq!(
            Action::List.finish(&list(), never).unwrap(),
            render(&list())
        );
    }

    #[test]
    fn a_refused_unlink_or_forget_names_the_folders() {
        let data = base().join("data");
        let api = |status, code: &str| ClientError::Api {
            status,
            code: code.into(),
            message: "x".into(),
        };
        let unlink = Action::Unlink {
            name: "shop".into(),
        };
        let parked = api(409, "site_is_parked");
        assert_eq!(
            unlink.failure_with(&parked, &data, Some(&list())).line(),
            "marsa: \"shop\" comes from the parked folder /code, so it cannot be unlinked \
             alone. To stop serving that folder and all its sites, run \"marsa forget /code\". \
             [site_is_parked]"
        );
        // Without the list, no folder.
        let message = unlink.failure(&parked, &data).message;
        assert!(
            message.ends_with("run \"marsa forget <folder>\"."),
            "{message}"
        );

        let forget = Action::Forget {
            folder: PathBuf::from("/work"),
        };
        let missing = api(404, "folder_not_parked");
        assert_eq!(
            forget.failure_with(&missing, &data, Some(&list())).message,
            "/work is not a parked folder. Parked folders: /code."
        );
        let none = SiteList::default();
        assert_eq!(
            forget.failure_with(&missing, &data, Some(&none)).message,
            "/work is not a parked folder. No folder is parked."
        );

        let isolate = Action::Isolate {
            name: "shop".into(),
            php: Some(" latest".into()),
        };
        assert_eq!(
            isolate
                .failure(&api(400, "unsupported_alias"), &data)
                .message,
            "\"latest\" is a name, not a version. Write a version number, such as 8.4."
        );
        assert_eq!(
            isolate.failure(&api(404, "site_not_found"), &data).message,
            "There is no site named \"shop\". Run \"marsa sites\" to see them."
        );
    }
}
