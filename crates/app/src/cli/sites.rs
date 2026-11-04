//! `marsa sites`, `marsa park`, `marsa link`, `marsa secure`, and
//! `marsa unsecure`: the sites the daemon serves. Task M4-10c.
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
}

/// What a command asks the daemon: every folder absolute, every name checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    List,
    Park { folder: PathBuf },
    Link { name: String, folder: PathBuf },
    Secure { name: String, secure: bool },
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
        })
    }

    async fn send(&self, client: &DaemonClient) -> Result<SiteList, ClientError> {
        match self {
            Action::List => client.sites().await,
            Action::Park { folder } => client.park(folder).await,
            Action::Link { name, folder } => client.link(name, folder).await,
            Action::Secure { name, secure } => {
                let patch = SitePatch {
                    secure: Some(*secure),
                    ..SitePatch::default()
                };
                client.update_site(name, &patch).await
            }
        }
    }

    /// What to print for the daemon's answer: the table for `sites` and
    /// `park`, one line for `link`, `secure`, and `unsecure`. If that site
    /// is not in the answer as asked, the table instead.
    pub fn render(&self, list: &SiteList) -> String {
        let site = |name: &str| list.sites.iter().find(|site| site.name == name);
        let line = match self {
            Action::List | Action::Park { .. } => None,
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
        let mut failure = failure(err, data_dir, None);
        let (site, folder) = match self {
            Action::List => (None, None),
            Action::Park { folder } => (None, Some(folder.as_path())),
            Action::Link { name, folder } => (Some(name.as_str()), Some(folder.as_path())),
            Action::Secure { name, .. } => (Some(name.as_str()), None),
        };
        let sites_toml = ConfigPaths::in_dir(data_dir).sites;
        let path = match failure.code.as_str() {
            "config_read" | "config_invalid" | "config_write" => Some(sites_toml.as_path()),
            code if code.starts_with("site_") => folder,
            _ => return failure,
        };
        let details = Details {
            site,
            path,
            ..Details::default()
        };
        failure.message = strings::message(&failure.code, &details);
        failure
    }
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
        Ok(list) => {
            print!("{}", action.render(&list));
            ExitCode::SUCCESS
        }
        Err(err) => action.failure(&err, data_dir).report(),
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
}
