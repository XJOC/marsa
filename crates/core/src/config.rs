//! User settings in TOML, written atomically.
//!
//! Two files live in `<app data dir>/config/` (architecture.md section 7):
//!
//! - `global.toml`: the default PHP and Node versions, and Xdebug on or off,
//! - `sites.toml`: the settings of each site.
//!
//! The functions take the file path as input, so tests use temp folders. Only
//! [`app_data_dir`] asks the OS where the real folder is.
//!
//! This module is not behind the `full` feature: the shim reads both files.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

/// The first line of every file Marsa writes.
const HEADER: &str = "# Managed by Marsa. Comments in this file are not kept.\n";

/// `global.toml`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GlobalConfig {
    pub php: ToolDefaults,
    #[serde(deserialize_with = "node_defaults")]
    pub node: ToolDefaults,
}

/// The `[php]` or `[node]` table in `global.toml`. One table per tool leaves
/// room for more per-tool settings later.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolDefaults {
    /// The version as the user wrote it. The resolver checks it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// `[php]` only: every `php.ini` Marsa writes loads Xdebug (task M5-11).
    /// The default is `false`, so the file only says `xdebug = true`, and a
    /// file from before this key reads as off.
    #[serde(skip_serializing_if = "is_false")]
    pub xdebug: bool,
}

/// `sites.toml`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SitesConfig {
    /// The parked folders: every sub-folder in one is a site. Each path is
    /// absolute and listed once, in the order the user added it. First in the
    /// struct, so `toml` writes it above the `[sites.<name>]` tables.
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "parked_paths"
    )]
    pub parked: Vec<PathBuf>,
    /// A `BTreeMap`, not a `HashMap`: the file is written in a stable order.
    /// Every key is a valid site name (see [`crate::sites::validate_name`]).
    #[serde(deserialize_with = "site_tables")]
    pub sites: BTreeMap<String, SiteSettings>,
}

/// One `[sites.<name>]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteSettings {
    /// The project folder. Always absolute: a relative path would never match.
    #[serde(deserialize_with = "absolute_path")]
    pub root: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub php: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Served over HTTPS. The default is `true`, so the file only says
    /// `secure = false`.
    #[serde(default = "default_secure", skip_serializing_if = "is_true")]
    pub secure: bool,
    /// The port of the program that serves this site, for example `npm run
    /// dev` on `5173`. When set, the site is a Node proxy. Never `0`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "proxy_port"
    )]
    pub proxy: Option<u16>,
}

impl SitesConfig {
    /// The site whose `root` is `dir` or the nearest folder above it.
    ///
    /// Paths are compared by component, so `/code/shop-old` does not match
    /// `/code/shop`. Nothing is canonicalized and symlinks are not followed.
    /// If two sites have the same `root`, the first by name wins.
    pub fn site_for(&self, dir: &Path) -> Option<(&str, &SiteSettings)> {
        dir.ancestors().find_map(|folder| {
            self.sites
                .iter()
                .find(|(_, site)| site.root == folder)
                .map(|(name, site)| (name.as_str(), site))
        })
    }
}

/// Where the two config files are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    pub global: PathBuf,
    pub sites: PathBuf,
}

impl ConfigPaths {
    /// `<app_data_dir>/config/global.toml` and `<app_data_dir>/config/sites.toml`.
    pub fn in_dir(app_data_dir: &Path) -> Self {
        let dir = app_data_dir.join("config");
        Self {
            global: dir.join("global.toml"),
            sites: dir.join("sites.toml"),
        }
    }
}

/// Marsa's data folder, for example `~/Library/Application Support/dev.marsa.Marsa`
/// on macOS.
///
/// `data_local_dir`, not `data_dir`: on Windows the second one is the Roaming
/// folder, and runtimes (hundreds of MB) must not roam with the user profile.
///
/// # Errors
///
/// `config_no_home` if the OS gives no home folder.
pub fn app_data_dir() -> Result<PathBuf, ConfigError> {
    directories::ProjectDirs::from("dev", "marsa", "Marsa")
        .map(|dirs| dirs.data_local_dir().to_path_buf())
        .ok_or(ConfigError::NoHome)
}

/// Why a config file could not be read or written. [`ConfigError::code`]
/// gives a stable name that the UI maps to a message.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {}", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// `message` comes from `toml` and holds the line and the key.
    #[error("{} is not valid: {message}", .path.display())]
    Invalid { path: PathBuf, message: String },

    #[error("cannot write {}", .path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("cannot find the home folder of the current user")]
    NoHome,
}

impl ConfigError {
    /// Stable error name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            ConfigError::Read { .. } => "config_read",
            ConfigError::Invalid { .. } => "config_invalid",
            ConfigError::Write { .. } => "config_write",
            ConfigError::NoHome => "config_no_home",
        }
    }
}

/// Parses the text of `global.toml`. `path` is only used in errors.
///
/// # Errors
///
/// `config_invalid` for bad TOML, a wrong type, or an unknown key.
pub fn parse_global(text: &str, path: &Path) -> Result<GlobalConfig, ConfigError> {
    parse(text, path)
}

/// Parses the text of `sites.toml`. `path` is only used in errors.
///
/// # Errors
///
/// `config_invalid` for bad TOML, a wrong type, an unknown key, a relative
/// `root`, a relative or repeated `parked` folder, a `proxy` outside 1 to
/// 65535, or a site name that [`crate::sites::validate_name`] rejects.
pub fn parse_sites(text: &str, path: &Path) -> Result<SitesConfig, ConfigError> {
    parse(text, path)
}

/// Reads `global.toml`. A missing file (or folder) gives the defaults.
///
/// # Errors
///
/// `config_read` if the file exists but cannot be read, otherwise as
/// [`parse_global`].
pub fn load_global(path: &Path) -> Result<GlobalConfig, ConfigError> {
    match read_text(path)? {
        Some(text) => parse_global(&text, path),
        None => Ok(GlobalConfig::default()),
    }
}

/// Reads `sites.toml`. A missing file (or folder) gives the defaults.
///
/// # Errors
///
/// `config_read` if the file exists but cannot be read, otherwise as
/// [`parse_sites`].
pub fn load_sites(path: &Path) -> Result<SitesConfig, ConfigError> {
    match read_text(path)? {
        Some(text) => parse_sites(&text, path),
        None => Ok(SitesConfig::default()),
    }
}

/// Writes `global.toml` atomically. Only the daemon calls this.
///
/// # Errors
///
/// `config_write` if the folder or the file cannot be written.
pub fn save_global(path: &Path, config: &GlobalConfig) -> Result<(), ConfigError> {
    save(path, config)
}

/// Writes `sites.toml` atomically. Only the daemon calls this.
///
/// # Errors
///
/// `config_write` if the folder or the file cannot be written, or a `root` is
/// not valid UTF-8 (TOML strings must be).
pub fn save_sites(path: &Path, config: &SitesConfig) -> Result<(), ConfigError> {
    save(path, config)
}

fn parse<T: DeserializeOwned>(text: &str, path: &Path) -> Result<T, ConfigError> {
    // Old Notepad versions start UTF-8 files with a byte order mark.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    toml::from_str(text).map_err(|err| ConfigError::Invalid {
        path: path.to_path_buf(),
        message: err.to_string(),
    })
}

/// The file's text, or `None` if the file or its folder does not exist.
fn read_text(path: &Path) -> Result<Option<String>, ConfigError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| ConfigError::Invalid {
            path: path.to_path_buf(),
            message: "the file is not UTF-8 text".to_owned(),
        })
}

fn save<T: Serialize>(path: &Path, config: &T) -> Result<(), ConfigError> {
    let fail = |source| ConfigError::Write {
        path: path.to_path_buf(),
        source,
    };
    let body = toml::to_string(config).map_err(|err| fail(io::Error::other(err)))?;
    write_atomic(path, &format!("{HEADER}\n{body}")).map_err(fail)
}

/// Writes `text` to a temp file next to `path`, then renames it over `path`.
/// A reader sees the old file or the new one, never half of one.
///
/// The temp file is in the same folder, not the system temp folder, because
/// `rename` fails across volumes.
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp");
    let result = write_synced(&tmp, text).and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        // Best effort: the write already failed, and that is the error to report.
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Writes and flushes to disk, so a power cut cannot leave an empty file
/// after the rename.
fn write_synced(path: &Path, text: &str) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// Accepts only absolute paths for `root`. Rejecting it here, not after
/// parsing, lets `toml` add the line number to the error. The other checks
/// below are done while parsing for the same reason.
fn absolute_path<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
    let path = PathBuf::deserialize(deserializer)?;
    check_absolute("root", &path).map_err(serde::de::Error::custom)?;
    Ok(path)
}

/// Accepts only absolute folders for `parked`, each listed once.
///
/// Paths are compared as [`PathBuf`] values: `/code/` and `/code` are the same
/// folder. Nothing is canonicalized, like in [`SitesConfig::site_for`].
fn parked_paths<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<PathBuf>, D::Error> {
    let paths = Vec::<PathBuf>::deserialize(deserializer)?;
    for (index, path) in paths.iter().enumerate() {
        check_absolute("parked", path).map_err(serde::de::Error::custom)?;
        if paths[..index].contains(path) {
            return Err(serde::de::Error::custom(format!(
                "parked lists {:?} more than once",
                path.display().to_string()
            )));
        }
    }
    Ok(paths)
}

/// Fails with a message that names `key` if `path` is not absolute.
fn check_absolute(key: &str, path: &Path) -> Result<(), String> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(format!(
            "{key} must be an absolute path, not {:?}",
            path.display().to_string()
        ))
    }
}

/// `proxy` is a TCP port from 1 to 65535: port `0` is not a real port. Read as
/// `i64` (a TOML integer), so `0`, `-1`, and `70000` all get the same message.
fn proxy_port<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u16>, D::Error> {
    let number = i64::deserialize(deserializer)?;
    match u16::try_from(number) {
        Ok(port) if port != 0 => Ok(Some(port)),
        _ => Err(serde::de::Error::custom(format!(
            "proxy must be a port from 1 to 65535, not {number}"
        ))),
    }
}

fn default_secure() -> bool {
    true
}

/// For `skip_serializing_if`, which passes a reference.
fn is_true(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The `[node]` table. It has no `xdebug`: the key is refused there like any
/// unknown key, with its line.
fn node_defaults<'de, D: Deserializer<'de>>(deserializer: D) -> Result<ToolDefaults, D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Node {
        default: Option<String>,
    }
    let Node { default } = Node::deserialize(deserializer)?;
    Ok(ToolDefaults {
        default,
        xdebug: false,
    })
}

/// The `[sites.<name>]` tables. Each name is checked as its key is read, so
/// `toml` adds the line of the bad table to the error.
fn site_tables<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, SiteSettings>, D::Error> {
    let tables = BTreeMap::<SiteName, SiteSettings>::deserialize(deserializer)?;
    Ok(tables
        .into_iter()
        .map(|(name, site)| (name.0, site))
        .collect())
}

/// A key of `[sites]` that passed [`crate::sites::validate_name`]. Only used
/// while parsing: [`SitesConfig::sites`] keeps plain `String` keys.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct SiteName(String);

impl<'de> Deserialize<'de> for SiteName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        crate::sites::validate_name(&name).map_err(serde::de::Error::custom)?;
        Ok(SiteName(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Absolute paths look different on Windows, so the fixtures differ by OS.
    #[cfg(windows)]
    const SHOP: &str = r"C:\code\shop";
    #[cfg(not(windows))]
    const SHOP: &str = "/code/shop";

    /// `SHOP` as `toml` writes it.
    #[cfg(windows)]
    const SHOP_TOML: &str = r"'C:\code\shop'";
    #[cfg(not(windows))]
    const SHOP_TOML: &str = "\"/code/shop\"";

    /// The folder above `SHOP`, used as a parked folder.
    #[cfg(windows)]
    const CODE: &str = r"C:\code";
    #[cfg(not(windows))]
    const CODE: &str = "/code";

    /// `CODE` as `toml` writes it.
    #[cfg(windows)]
    const CODE_TOML: &str = r"'C:\code'";
    #[cfg(not(windows))]
    const CODE_TOML: &str = "\"/code\"";

    fn site(root: &Path, php: Option<&str>) -> SiteSettings {
        SiteSettings {
            root: root.to_path_buf(),
            php: php.map(str::to_owned),
            node: None,
            secure: true,
            proxy: None,
        }
    }

    fn global(php: &str, node: &str) -> GlobalConfig {
        GlobalConfig {
            php: ToolDefaults {
                default: Some(php.to_owned()),
                xdebug: false,
            },
            node: ToolDefaults {
                default: Some(node.to_owned()),
                xdebug: false,
            },
        }
    }

    /// Writes `text` to `<tmp>/<name>` and returns its path.
    fn file(dir: &TempDir, name: &str, text: &str) -> PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, text).unwrap();
        path
    }

    /// `text` with `{root}` replaced by an absolute path that is valid on this OS.
    fn sites_text(dir: &TempDir, text: &str) -> String {
        // A literal string ('...') keeps Windows backslashes as they are.
        text.replace("{root}", &format!("'{}'", dir.path().display()))
    }

    #[test]
    fn missing_file_or_folder_gives_defaults() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("no-such-dir").join("global.toml");
        assert_eq!(load_global(&missing).unwrap(), GlobalConfig::default());
        let missing = dir.path().join("no-such-dir").join("sites.toml");
        assert_eq!(load_sites(&missing).unwrap(), SitesConfig::default());
        assert_eq!(
            load_global(&dir.path().join("global.toml")).unwrap(),
            GlobalConfig::default()
        );
    }

    #[test]
    fn empty_file_gives_defaults() {
        let dir = TempDir::new().unwrap();
        let path = file(&dir, "global.toml", "");
        assert_eq!(load_global(&path).unwrap(), GlobalConfig::default());
        let path = file(&dir, "sites.toml", "# only a comment\n");
        assert_eq!(load_sites(&path).unwrap(), SitesConfig::default());
    }

    #[test]
    fn reads_every_field_of_global() {
        let dir = TempDir::new().unwrap();
        let path = file(
            &dir,
            "global.toml",
            "[php]\ndefault = \"8.4\"\n\n[node]\ndefault = \"22\"\n",
        );
        assert_eq!(load_global(&path).unwrap(), global("8.4", "22"));
    }

    #[test]
    fn xdebug_is_a_php_key_and_off_unless_the_file_says_true() {
        let dir = TempDir::new().unwrap();
        // A file from before the key.
        let old = file(&dir, "old.toml", "[php]\ndefault = \"8.4\"\n");
        assert!(!load_global(&old).unwrap().php.xdebug);

        let path = file(&dir, "global.toml", "[php]\nxdebug = true\n");
        let mut config = load_global(&path).unwrap();
        assert!(config.php.xdebug);
        assert_eq!(config.php.default, None);
        save_global(&path, &config).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text, format!("{HEADER}\n[php]\nxdebug = true\n\n[node]\n"));
        assert_eq!(load_global(&path).unwrap(), config);

        // Off is not written.
        config.php.xdebug = false;
        save_global(&path, &config).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text, format!("{HEADER}\n[php]\n\n[node]\n"));

        let path = file(&dir, "node.toml", "[node]\nxdebug = true\n");
        assert_invalid(&load_global(&path).unwrap_err(), &path, "xdebug");
        let path = file(&dir, "text.toml", "[php]\nxdebug = \"on\"\n");
        assert_invalid(&load_global(&path).unwrap_err(), &path, "line 2");
    }

    #[test]
    fn reads_every_field_of_sites() {
        let dir = TempDir::new().unwrap();
        let text = sites_text(
            &dir,
            "[sites.shop]\nroot = {root}\nphp = \"8.3\"\n\n\
             [sites.api]\nroot = {root}\nnode = \"20\"\n",
        );
        let config = load_sites(&file(&dir, "sites.toml", &text)).unwrap();
        let api = SiteSettings {
            root: dir.path().to_path_buf(),
            php: None,
            node: Some("20".to_owned()),
            secure: true,
            proxy: None,
        };
        assert_eq!(config.sites.len(), 2);
        assert_eq!(config.sites["shop"], site(dir.path(), Some("8.3")));
        assert_eq!(config.sites["api"], api);
    }

    #[test]
    fn crlf_lines_are_read() {
        let dir = TempDir::new().unwrap();
        let path = file(
            &dir,
            "global.toml",
            "[php]\r\ndefault = \"8.4\"\r\n[node]\r\ndefault = \"22\"\r\n",
        );
        assert_eq!(load_global(&path).unwrap(), global("8.4", "22"));
    }

    #[test]
    fn leading_bom_is_skipped() {
        let dir = TempDir::new().unwrap();
        let path = file(&dir, "global.toml", "\u{feff}[php]\ndefault = \"8.4\"\n");
        assert_eq!(
            load_global(&path).unwrap().php.default.as_deref(),
            Some("8.4")
        );
    }

    /// Checks the code, and that the message names the file.
    fn assert_invalid(err: &ConfigError, path: &Path, needle: &str) {
        assert_eq!(err.code(), "config_invalid", "{err}");
        let message = err.to_string();
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains(needle), "{message}");
    }

    #[test]
    fn bad_toml_is_invalid_with_line() {
        let dir = TempDir::new().unwrap();
        let path = file(&dir, "global.toml", "[php]\ndefault = \n");
        let err = load_global(&path).unwrap_err();
        assert_invalid(&err, &path, "line 2");
    }

    #[test]
    fn wrong_type_is_invalid() {
        let dir = TempDir::new().unwrap();
        let path = file(&dir, "global.toml", "[php]\ndefault = 8.4\n");
        assert_invalid(&load_global(&path).unwrap_err(), &path, "line 2");
    }

    #[test]
    fn unknown_key_is_invalid() {
        let dir = TempDir::new().unwrap();
        let path = file(&dir, "global.toml", "[php]\ndefaut = \"8.4\"\n");
        assert_invalid(&load_global(&path).unwrap_err(), &path, "defaut");

        let text = sites_text(&dir, "[sites.shop]\nroot = {root}\nphpp = \"8.3\"\n");
        let path = file(&dir, "sites.toml", &text);
        assert_invalid(&load_sites(&path).unwrap_err(), &path, "phpp");

        let path = file(&dir, "top.toml", "[defaults]\nphp = \"8.4\"\n");
        assert_invalid(&load_global(&path).unwrap_err(), &path, "defaults");
    }

    #[test]
    fn missing_root_is_invalid() {
        let dir = TempDir::new().unwrap();
        let path = file(&dir, "sites.toml", "[sites.shop]\nphp = \"8.3\"\n");
        assert_invalid(&load_sites(&path).unwrap_err(), &path, "root");
    }

    #[test]
    fn relative_root_is_invalid_with_line() {
        let dir = TempDir::new().unwrap();
        let path = file(&dir, "sites.toml", "[sites.shop]\nroot = \"code/shop\"\n");
        let err = load_sites(&path).unwrap_err();
        assert_invalid(&err, &path, "absolute");
        assert_invalid(&err, &path, "line 2");
    }

    #[test]
    fn non_utf8_file_is_invalid() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("global.toml");
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert_invalid(&load_global(&path).unwrap_err(), &path, "UTF-8");
    }

    #[test]
    fn unreadable_file_is_read_error() {
        // A folder where the file should be: reading it fails, but not with NotFound.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("global.toml");
        fs::create_dir(&path).unwrap();
        let err = load_global(&path).unwrap_err();
        assert_eq!(err.code(), "config_read");
        assert!(err.to_string().contains(&path.display().to_string()));
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = TempDir::new().unwrap();
        let paths = ConfigPaths::in_dir(dir.path());

        let global = global("8.4", "22");
        save_global(&paths.global, &global).unwrap();
        assert_eq!(load_global(&paths.global).unwrap(), global);

        let mut sites = SitesConfig::default();
        sites
            .sites
            .insert("shop".to_owned(), site(dir.path(), Some("8.3")));
        save_sites(&paths.sites, &sites).unwrap();
        assert_eq!(load_sites(&paths.sites).unwrap(), sites);

        for path in [&paths.global, &paths.sites] {
            let text = fs::read_to_string(path).unwrap();
            assert!(text.starts_with(HEADER), "{text}");
        }
    }

    #[test]
    fn save_writes_exact_text() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("global.toml");
        save_global(&path, &global("8.4", "22")).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# Managed by Marsa. Comments in this file are not kept.\n\
             \n\
             [php]\n\
             default = \"8.4\"\n\
             \n\
             [node]\n\
             default = \"22\"\n"
        );

        let path = dir.path().join("sites.toml");
        let mut sites = SitesConfig::default();
        sites
            .sites
            .insert("shop".to_owned(), site(Path::new(SHOP), Some("8.3")));
        save_sites(&path, &sites).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!(
                "# Managed by Marsa. Comments in this file are not kept.\n\
                 \n\
                 [sites.shop]\n\
                 root = {SHOP_TOML}\n\
                 php = \"8.3\"\n"
            )
        );
    }

    #[test]
    fn save_replaces_file_creates_folder_and_leaves_no_temp() {
        let dir = TempDir::new().unwrap();
        let config_dir = dir.path().join("a").join("config");
        let path = config_dir.join("global.toml");

        save_global(&path, &global("8.3", "20")).unwrap();
        save_global(&path, &global("8.4", "22")).unwrap();

        assert_eq!(load_global(&path).unwrap(), global("8.4", "22"));
        let names: Vec<_> = fs::read_dir(&config_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["global.toml"]);
    }

    #[test]
    fn failed_save_is_write_error_and_removes_temp() {
        let dir = TempDir::new().unwrap();
        // A folder where the file should be: the rename fails.
        let path = dir.path().join("global.toml");
        fs::create_dir(&path).unwrap();

        let err = save_global(&path, &GlobalConfig::default()).unwrap_err();
        assert_eq!(err.code(), "config_write");
        assert!(err.to_string().contains(&path.display().to_string()));
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn save_fails_when_parent_is_a_file() {
        let dir = TempDir::new().unwrap();
        let blocker = file(&dir, "blocker", "");
        let err = save_global(&blocker.join("global.toml"), &GlobalConfig::default()).unwrap_err();
        assert_eq!(err.code(), "config_write");
    }

    fn sites_at(base: &Path, entries: &[(&str, &str)]) -> SitesConfig {
        let sites = entries
            .iter()
            .map(|(name, rel)| ((*name).to_owned(), site(&base.join(rel), None)))
            .collect();
        SitesConfig {
            parked: Vec::new(),
            sites,
        }
    }

    #[test]
    fn site_for_matches_root_and_subfolders() {
        let base = Path::new(SHOP).parent().unwrap();
        let config = sites_at(base, &[("shop", "shop")]);

        let (name, _) = config.site_for(&base.join("shop")).unwrap();
        assert_eq!(name, "shop");
        let (name, _) = config.site_for(&base.join("shop").join("public")).unwrap();
        assert_eq!(name, "shop");
    }

    #[test]
    fn site_for_ignores_other_folders() {
        let base = Path::new(SHOP).parent().unwrap();
        let config = sites_at(base, &[("shop", "shop")]);

        assert!(config.site_for(&base.join("shop-old")).is_none());
        assert!(config.site_for(base).is_none());
        assert!(config.site_for(&base.join("blog")).is_none());
    }

    #[test]
    fn site_for_prefers_nearest_root() {
        let base = Path::new(SHOP).parent().unwrap();
        let config = sites_at(base, &[("outer", "shop"), ("inner", "shop/admin")]);

        let dir = base.join("shop").join("admin").join("src");
        assert_eq!(config.site_for(&dir).unwrap().0, "inner");
        assert_eq!(config.site_for(&base.join("shop")).unwrap().0, "outer");
    }

    #[test]
    fn site_for_same_root_picks_first_name() {
        let base = Path::new(SHOP).parent().unwrap();
        let config = sites_at(base, &[("zeta", "shop"), ("alpha", "shop")]);
        assert_eq!(config.site_for(&base.join("shop")).unwrap().0, "alpha");
    }

    #[test]
    fn config_paths_are_under_config_folder() {
        let base = Path::new(SHOP);
        let paths = ConfigPaths::in_dir(base);
        assert_eq!(paths.global, base.join("config").join("global.toml"));
        assert_eq!(paths.sites, base.join("config").join("sites.toml"));
    }

    /// A `sites.toml` as M1-05 wrote it: no `parked`, `secure`, or `proxy`.
    fn old_sites_text() -> String {
        format!(
            "{HEADER}\n\
             [sites.shop]\n\
             root = {SHOP_TOML}\n\
             php = \"8.3\"\n\
             node = \"22\"\n"
        )
    }

    /// A `sites.toml` that uses every new key.
    fn new_sites_text() -> String {
        format!(
            "{HEADER}\n\
             parked = [{SHOP_TOML}, {CODE_TOML}]\n\
             \n\
             [sites.shop]\n\
             root = {SHOP_TOML}\n\
             php = \"8.4\"\n\
             secure = false\n\
             proxy = 5173\n"
        )
    }

    /// The config in [`new_sites_text`].
    fn new_sites() -> SitesConfig {
        let shop = SiteSettings {
            secure: false,
            proxy: Some(5173),
            ..site(Path::new(SHOP), Some("8.4"))
        };
        SitesConfig {
            parked: vec![PathBuf::from(SHOP), PathBuf::from(CODE)],
            sites: BTreeMap::from([("shop".to_owned(), shop)]),
        }
    }

    /// Parses in-memory `sites.toml` text.
    fn parse_text(text: &str) -> Result<SitesConfig, ConfigError> {
        parse_sites(text, Path::new("sites.toml"))
    }

    #[test]
    fn old_sites_file_reads_with_defaults() {
        let config = parse_text(&old_sites_text()).unwrap();
        assert!(config.parked.is_empty());
        let shop = &config.sites["shop"];
        assert_eq!(shop.root, Path::new(SHOP));
        assert_eq!(shop.php.as_deref(), Some("8.3"));
        assert_eq!(shop.node.as_deref(), Some("22"));
        assert!(shop.secure);
        assert_eq!(shop.proxy, None);
    }

    #[test]
    fn old_sites_file_is_written_back_without_new_keys() {
        let dir = TempDir::new().unwrap();
        let path = file(&dir, "sites.toml", &old_sites_text());
        let config = load_sites(&path).unwrap();
        save_sites(&path, &config).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), old_sites_text());
    }

    #[test]
    fn reads_new_fields() {
        assert_eq!(parse_text(&new_sites_text()).unwrap(), new_sites());
    }

    #[test]
    fn new_fields_are_written_exactly() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sites.toml");
        save_sites(&path, &new_sites()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), new_sites_text());
    }

    #[test]
    fn new_fields_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sites.toml");
        save_sites(&path, &new_sites()).unwrap();
        assert_eq!(load_sites(&path).unwrap(), new_sites());
    }

    #[test]
    fn parked_only_file_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sites.toml");
        let config = SitesConfig {
            parked: vec![PathBuf::from(CODE)],
            sites: BTreeMap::new(),
        };
        save_sites(&path, &config).unwrap();
        assert_eq!(load_sites(&path).unwrap(), config);
    }

    #[test]
    fn parked_keeps_insertion_order() {
        // `SHOP` sorts after `CODE`: a sorted list would swap them.
        let config = parse_text(&format!("parked = [{SHOP_TOML}, {CODE_TOML}]\n")).unwrap();
        assert_eq!(config.parked, [PathBuf::from(SHOP), PathBuf::from(CODE)]);
    }

    #[test]
    fn explicit_secure_true_is_read_and_not_written() {
        let text = format!("[sites.shop]\nroot = {SHOP_TOML}\nsecure = true\n");
        let config = parse_text(&text).unwrap();
        assert!(config.sites["shop"].secure);

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sites.toml");
        save_sites(&path, &config).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert!(!written.contains("secure"), "{written}");
    }

    #[test]
    fn relative_parked_folder_is_invalid_with_line() {
        let dir = TempDir::new().unwrap();
        let text = sites_text(&dir, "parked = [\n  {root},\n  \"code\",\n]\n");
        let path = file(&dir, "sites.toml", &text);
        let err = load_sites(&path).unwrap_err();
        assert_invalid(&err, &path, "parked must be an absolute path");
        assert_invalid(&err, &path, "\"code\"");
        assert_invalid(&err, &path, "line 1");
    }

    #[test]
    fn repeated_parked_folder_is_invalid() {
        let dir = TempDir::new().unwrap();
        let text = sites_text(&dir, "parked = [{root}, {root}]\n");
        let path = file(&dir, "sites.toml", &text);
        let err = load_sites(&path).unwrap_err();
        assert_invalid(&err, &path, "more than once");
        assert_invalid(&err, &path, "line 1");
    }

    #[test]
    fn parked_folder_with_trailing_separator_is_a_repeat() {
        let sep = std::path::MAIN_SEPARATOR;
        let text = format!("parked = ['{CODE}', '{CODE}{sep}']\n");
        let err = parse_text(&text).unwrap_err();
        assert_invalid(&err, Path::new("sites.toml"), "more than once");
    }

    #[test]
    fn proxy_zero_is_invalid_with_line() {
        let dir = TempDir::new().unwrap();
        let text = sites_text(&dir, "[sites.shop]\nroot = {root}\nproxy = 0\n");
        let path = file(&dir, "sites.toml", &text);
        let err = load_sites(&path).unwrap_err();
        assert_invalid(&err, &path, "proxy must be a port from 1 to 65535");
        assert_invalid(&err, &path, "line 3");
    }

    #[test]
    fn proxy_accepts_every_real_port() {
        for port in [1, 5173, 65535] {
            let text = format!("[sites.shop]\nroot = {SHOP_TOML}\nproxy = {port}\n");
            let config = parse_text(&text).unwrap();
            assert_eq!(config.sites["shop"].proxy, Some(port));
        }
    }

    #[test]
    fn proxy_out_of_range_is_invalid() {
        for port in ["65536", "-1"] {
            let text = format!("[sites.shop]\nroot = {SHOP_TOML}\nproxy = {port}\n");
            let err = parse_text(&text).unwrap_err();
            let path = Path::new("sites.toml");
            assert_invalid(&err, path, &format!("from 1 to 65535, not {port}"));
            assert_invalid(&err, path, "line 3");
        }
    }

    #[test]
    fn proxy_as_text_is_invalid() {
        let text = format!("[sites.shop]\nroot = {SHOP_TOML}\nproxy = \"5173\"\n");
        let err = parse_text(&text).unwrap_err();
        assert_invalid(&err, Path::new("sites.toml"), "line 3");
    }

    #[test]
    fn uppercase_site_name_is_invalid_with_line() {
        let dir = TempDir::new().unwrap();
        let text = sites_text(&dir, "[sites.Shop]\nroot = {root}\n");
        let path = file(&dir, "sites.toml", &text);
        let err = load_sites(&path).unwrap_err();
        assert_invalid(&err, &path, "\"Shop\" is not a valid site name");
        assert_invalid(&err, &path, "line 1");
    }

    #[test]
    fn other_bad_site_names_are_invalid() {
        let too_long = "a".repeat(crate::sites::MAX_NAME_LEN + 1);
        for name in ["-shop", "shop-", "my_shop", "\"a.b\"", "\"\"", &too_long] {
            let text = format!("[sites.{name}]\nroot = {SHOP_TOML}\n");
            let err = parse_text(&text).unwrap_err();
            assert_invalid(&err, Path::new("sites.toml"), "is not a valid site name");
        }
    }

    #[test]
    fn good_site_names_are_accepted() {
        let longest = "a".repeat(crate::sites::MAX_NAME_LEN);
        for name in ["a", "shop-2", "0day", &longest] {
            let text = format!("[sites.{name}]\nroot = {SHOP_TOML}\n");
            let config = parse_text(&text).unwrap();
            assert!(config.sites.contains_key(name), "{name}");
        }
    }

    #[test]
    fn every_error_has_its_code() {
        let path = PathBuf::from("global.toml");
        let cases = [
            (
                ConfigError::Read {
                    path: path.clone(),
                    source: io::Error::from(io::ErrorKind::PermissionDenied),
                },
                "config_read",
            ),
            (
                ConfigError::Invalid {
                    path: path.clone(),
                    message: "bad".to_owned(),
                },
                "config_invalid",
            ),
            (
                ConfigError::Write {
                    path,
                    source: io::Error::from(io::ErrorKind::PermissionDenied),
                },
                "config_write",
            ),
            (ConfigError::NoHome, "config_no_home"),
        ];
        for (error, code) in cases {
            assert_eq!(error.code(), code, "{error}");
        }
    }
}
