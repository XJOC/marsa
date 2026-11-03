//! Picks the PHP or Node version for a folder.
//!
//! First match wins (architecture.md section 4):
//!
//! 1. the project setting stored by Marsa,
//! 2. version files, from the folder up to the root of the disk,
//! 3. constraints in `composer.json` or `package.json`, from the folder up to
//!    the root of the disk,
//! 4. the global default.
//!
//! All version files up to the root come before any constraint. A version
//! file is a pin the user wrote; a constraint is a range, often written by a
//! library author (`vendor/*/composer.json`, `node_modules/*/package.json`).
//!
//! The caller passes the settings and the installed versions. The only thing
//! this module does on its own is read version files and manifests.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use semver::{Prerelease, Version};
use serde_json::Value;
use toml::de::DeTable;

use constraint::Constraint;

mod constraint;

/// A runtime that Marsa picks a version for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Php,
    Node,
}

impl Tool {
    /// The version files to check in each folder, in priority order.
    fn version_files(self) -> &'static [FileKind] {
        match self {
            Tool::Php => &[
                FileKind::Plain(".php-version"),
                FileKind::ToolVersions,
                FileKind::Mise,
            ],
            Tool::Node => &[
                FileKind::Plain(".nvmrc"),
                FileKind::Plain(".node-version"),
                FileKind::ToolVersions,
                FileKind::Mise,
            ],
        }
    }

    /// Names of the tool in `.tool-versions`. asdf calls its Node plugin `nodejs`.
    fn tool_versions_keys(self) -> &'static [&'static str] {
        match self {
            Tool::Php => &["php"],
            Tool::Node => &["nodejs", "node"],
        }
    }

    /// Name of the tool under `[tools]` in `mise.toml`.
    fn mise_key(self) -> &'static str {
        match self {
            Tool::Php => "php",
            Tool::Node => "node",
        }
    }

    /// The manifest that may hold a constraint, and where in it:
    /// `require.php` in `composer.json`, `engines.node` in `package.json`.
    fn manifest(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Tool::Php => ("composer.json", "require", "php"),
            Tool::Node => ("package.json", "engines", "node"),
        }
    }
}

impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Tool::Php => "PHP",
            Tool::Node => "Node",
        })
    }
}

/// Where the chosen version came from, so the UI can explain the choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The project setting stored by Marsa (`sites.toml`).
    ProjectSetting,
    /// A version file, such as `.php-version`, with its full path.
    VersionFile(PathBuf),
    /// A constraint in `composer.json` or `package.json`, with its full path.
    Constraint(PathBuf),
    /// The global default (`global.toml`).
    GlobalDefault,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::ProjectSetting => f.write_str("the project setting"),
            Source::VersionFile(path) | Source::Constraint(path) => {
                write!(f, "{}", path.display())
            }
            Source::GlobalDefault => f.write_str("the global default"),
        }
    }
}

/// The version that runs in a folder, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub tool: Tool,
    pub version: Version,
    pub source: Source,
}

/// Why no version could be picked. [`ResolveError::code`] gives a stable name
/// that the UI maps to a message.
///
/// The field is called `origin`, not `source`: `thiserror` treats a field
/// named `source` as the underlying error.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("no {tool} version is set for this folder, and there is no global default")]
    NoVersion { tool: Tool },

    #[error("{tool} {requested} is not installed (asked for by {origin})")]
    NotInstalled {
        tool: Tool,
        requested: String,
        origin: Source,
    },

    #[error("no installed {tool} version satisfies {constraint} (in {origin})")]
    NoMatchingVersion {
        tool: Tool,
        constraint: String,
        origin: Source,
    },

    #[error("{value:?} is not a valid {tool} version (in {origin})")]
    InvalidVersion {
        tool: Tool,
        value: String,
        origin: Source,
    },

    #[error(
        "{tool} version alias {value:?} is not supported, write a version number (in {origin})"
    )]
    UnsupportedAlias {
        tool: Tool,
        value: String,
        origin: Source,
    },

    #[error("{}: {reason}", .path.display())]
    Unsupported { path: PathBuf, reason: String },

    #[error("cannot read {}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl ResolveError {
    /// Stable error name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            ResolveError::NoVersion { .. } => "no_version",
            ResolveError::NotInstalled { .. } => "not_installed",
            ResolveError::NoMatchingVersion { .. } => "no_matching_version",
            ResolveError::InvalidVersion { .. } => "invalid_version",
            ResolveError::UnsupportedAlias { .. } => "unsupported_alias",
            ResolveError::Unsupported { .. } => "unsupported",
            ResolveError::Io { .. } => "io",
        }
    }
}

/// Picks the `tool` version for `dir`. First match wins: `project_setting`,
/// then version files from `dir` up to the root, then the constraint in
/// `composer.json` or `package.json` from `dir` up to the root, then
/// `global_default`.
///
/// A constraint gives the highest installed version that satisfies it. A
/// constraint that allows any version (`*`) is skipped.
///
/// `dir` should be absolute. A relative path stops the upward search at the
/// current directory.
///
/// # Errors
///
/// If the first source found asks for a version that is not in `installed`,
/// this returns `not_installed` (`no_matching_version` for a constraint) and
/// does not try the next source. Running another version without saying so
/// would confuse the user.
/// A missing file is skipped; any other read error is `io`.
pub fn resolve(
    tool: Tool,
    dir: &Path,
    project_setting: Option<&str>,
    installed: &[Version],
    global_default: Option<&str>,
) -> Result<Resolution, ResolveError> {
    if let Some(value) = project_setting {
        return pick(tool, value, Source::ProjectSetting, installed);
    }
    if let Some((value, path)) = find_in_files(tool, dir)? {
        return pick(tool, &value, Source::VersionFile(path), installed);
    }
    if let Some((constraint, value, path)) = find_constraint(tool, dir)? {
        return match constraint.best(installed) {
            Some(version) => Ok(Resolution {
                tool,
                version: version.clone(),
                source: Source::Constraint(path),
            }),
            None => Err(ResolveError::NoMatchingVersion {
                tool,
                constraint: value,
                origin: Source::Constraint(path),
            }),
        };
    }
    if let Some(value) = global_default {
        return pick(tool, value, Source::GlobalDefault, installed);
    }
    Err(ResolveError::NoVersion { tool })
}

/// Turns the text the user wrote into the best installed version.
fn pick(
    tool: Tool,
    value: &str,
    origin: Source,
    installed: &[Version],
) -> Result<Resolution, ResolveError> {
    let value = value.trim();
    match best_match(value, installed) {
        Ok(Some(version)) => Ok(Resolution {
            tool,
            version,
            source: origin,
        }),
        Ok(None) => Err(ResolveError::NotInstalled {
            tool,
            requested: value.to_owned(),
            origin,
        }),
        Err(BadRequest::Alias) => Err(ResolveError::UnsupportedAlias {
            tool,
            value: value.to_owned(),
            origin,
        }),
        Err(BadRequest::Invalid) => Err(ResolveError::InvalidVersion {
            tool,
            value: value.to_owned(),
            origin,
        }),
    }
}

/// The highest of `candidates` that `value` asks for, by the rules `resolve`
/// uses for a version file: `8.4` is the newest `8.4.x` (never `8.9`), and a
/// pre-release is picked only when written in full (`8.5.0-rc1`). So `8.4`
/// means the same in `.php-version` and in `marsa install` (task M2-05).
///
/// `Ok(None)` when no candidate matches.
///
/// # Errors
///
/// `unsupported_alias` for `latest`, `lts/*`, and the like, or
/// `invalid_version` for text that is not a version.
pub fn best_match(value: &str, candidates: &[Version]) -> Result<Option<Version>, BadRequest> {
    let request = Request::parse(value.trim())?;
    Ok(candidates
        .iter()
        .filter(|v| request.matches(v))
        .max()
        .cloned())
}

/// A version as the user wrote it: `8`, `8.4`, `8.4.1`, or `8.5.0-rc1`.
/// A missing part matches any installed number.
///
/// This is not `semver::VersionReq`: there, `8.4` means `>=8.4.0, <9.0.0`, so
/// it would pick 8.9 when the user asked for the newest 8.4.x.
#[derive(Debug, PartialEq, Eq)]
struct Request {
    major: u64,
    minor: Option<u64>,
    patch: Option<u64>,
    /// Empty unless the user wrote a full pre-release version. So `8` never
    /// picks `8.5.0-rc1` by surprise.
    pre: Prerelease,
}

/// Why the text of a version request is refused by [`best_match`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BadRequest {
    /// A word that version managers accept instead of a number.
    #[error("version aliases such as `latest` are not supported, write a version number")]
    Alias,
    /// Anything else that is not a version.
    #[error("this is not a valid version")]
    Invalid,
}

impl BadRequest {
    /// The same stable names as [`ResolveError::code`].
    pub fn code(&self) -> &'static str {
        match self {
            BadRequest::Alias => "unsupported_alias",
            BadRequest::Invalid => "invalid_version",
        }
    }
}

/// Aliases from nvm, asdf, and mise. Marsa does not guess what they mean.
/// Anything that starts with `lts/` is an alias too.
const ALIASES: &[&str] = &["latest", "system", "node", "stable", "lts"];

impl Request {
    /// Parses a trimmed value.
    fn parse(value: &str) -> Result<Request, BadRequest> {
        let is_lts_name = value
            .get(..4)
            .is_some_and(|start| start.eq_ignore_ascii_case("lts/"));
        if is_lts_name || ALIASES.iter().any(|a| value.eq_ignore_ascii_case(a)) {
            return Err(BadRequest::Alias);
        }

        let value = value.strip_prefix('v').unwrap_or(value);

        // Three numbers, maybe with a pre-release tag.
        if let Ok(version) = Version::parse(value) {
            return Ok(Request {
                major: version.major,
                minor: Some(version.minor),
                patch: Some(version.patch),
                pre: version.pre,
            });
        }

        // One or two numbers.
        let (major, minor) = match value.split_once('.') {
            Some((major, minor)) => (major, Some(minor)),
            None => (value, None),
        };
        Ok(Request {
            major: number(major).ok_or(BadRequest::Invalid)?,
            minor: minor
                .map(|m| number(m).ok_or(BadRequest::Invalid))
                .transpose()?,
            patch: None,
            pre: Prerelease::EMPTY,
        })
    }

    fn matches(&self, version: &Version) -> bool {
        version.major == self.major
            && self.minor.is_none_or(|minor| version.minor == minor)
            && self.patch.is_none_or(|patch| version.patch == patch)
            && version.pre == self.pre
    }
}

/// Digits only. `str::parse` alone would also accept `+8`.
fn number(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A kind of version file. Each kind stores the version in its own format.
#[derive(Debug, Clone, Copy)]
enum FileKind {
    /// The first useful line is the version: `.php-version`, `.nvmrc`, `.node-version`.
    Plain(&'static str),
    /// asdf: one `tool version` pair per line.
    ToolVersions,
    /// mise: a `[tools]` table.
    Mise,
}

impl FileKind {
    fn file_name(self) -> &'static str {
        match self {
            FileKind::Plain(name) => name,
            FileKind::ToolVersions => ".tool-versions",
            FileKind::Mise => "mise.toml",
        }
    }

    /// The version this file asks for, or `None` if the file does not
    /// mention `tool`.
    fn read_value(
        self,
        tool: Tool,
        text: &str,
        path: &Path,
    ) -> Result<Option<String>, ResolveError> {
        match self {
            // A plain file is only about one tool, so an empty one is still an
            // answer: "" fails later as `invalid_version`.
            FileKind::Plain(_) => Ok(Some(plain_value(text).to_owned())),
            FileKind::ToolVersions => {
                Ok(tool_versions_value(text, tool.tool_versions_keys()).map(str::to_owned))
            }
            FileKind::Mise => mise_value(text, tool.mise_key(), path),
        }
    }
}

/// Walks from `dir` up to the root. In each folder, checks the tool's version
/// files in order. Returns the first value found and the file it came from.
fn find_in_files(tool: Tool, dir: &Path) -> Result<Option<(String, PathBuf)>, ResolveError> {
    for folder in dir.ancestors() {
        for &kind in tool.version_files() {
            let path = folder.join(kind.file_name());
            let Some(contents) = read_if_exists(&path)? else {
                continue;
            };
            if let Some(value) = kind.read_value(tool, without_bom(&contents), &path)? {
                return Ok(Some((value, path)));
            }
        }
    }
    Ok(None)
}

/// Walks from `dir` up to the root and returns the first constraint that asks
/// for something, with its text and the manifest it came from. A manifest
/// without the key, or with `*`, is skipped.
fn find_constraint(
    tool: Tool,
    dir: &Path,
) -> Result<Option<(Constraint, String, PathBuf)>, ResolveError> {
    let (file_name, _, _) = tool.manifest();
    for folder in dir.ancestors() {
        let path = folder.join(file_name);
        let Some(contents) = read_if_exists(&path)? else {
            continue;
        };
        let Some(value) = manifest_value(tool, without_bom(&contents), &path)? else {
            continue;
        };
        match Constraint::parse(&value, tool) {
            Ok(Some(constraint)) => return Ok(Some((constraint, value, path))),
            // Any version: the global default is the user's real choice.
            Ok(None) => continue,
            Err(_) => {
                return Err(ResolveError::InvalidVersion {
                    tool,
                    value,
                    origin: Source::Constraint(path),
                });
            }
        }
    }
    Ok(None)
}

/// `require.php` or `engines.node`, or `None` if the manifest does not have
/// it. JSON allows any whitespace between tokens, so `\r\n` needs no work.
fn manifest_value(tool: Tool, text: &str, path: &Path) -> Result<Option<String>, ResolveError> {
    let (_, section, key) = tool.manifest();
    let unsupported = |reason: String| ResolveError::Unsupported {
        path: path.to_path_buf(),
        reason,
    };

    let document: Value = serde_json::from_str(text)
        .map_err(|error| unsupported(format!("not valid JSON: {error}")))?;
    // PHP's `json_encode` writes an empty `require` as `[]`, so a section that
    // is not an object is the same as no section.
    let Some(value) = document
        .get(section)
        .and_then(Value::as_object)
        .and_then(|section| section.get(key))
    else {
        return Ok(None);
    };
    match value.as_str() {
        Some(constraint) => Ok(Some(constraint.trim().to_owned())),
        None => Err(unsupported(format!(
            "`{section}.{key}` must be a version constraint string"
        ))),
    }
}

/// Some Windows editors start a file with a byte order mark.
fn without_bom(contents: &str) -> &str {
    contents.strip_prefix('\u{feff}').unwrap_or(contents)
}

/// Reads a file. A missing file is `None`; any other read error is `io`.
fn read_if_exists(path: &Path) -> Result<Option<String>, ResolveError> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ResolveError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// The first line that is not empty and not a `#` comment, or "" if none.
/// `str::trim` also removes the `\r` of Windows line endings.
fn plain_value(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("")
}

/// The version on the first line whose first word is one of `keys`.
/// `php 8.4.1 8.3.14` gives `8.4.1`. A line with a key and no version gives "".
fn tool_versions_value<'a>(text: &'a str, keys: &[&str]) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let line = line.split_once('#').map_or(line, |(before, _)| before);
        let mut words = line.split_whitespace();
        let key = words.next()?;
        keys.contains(&key).then(|| words.next().unwrap_or(""))
    })
}

/// `[tools] node = "22"`. Only a string value is supported for now.
fn mise_value(text: &str, key: &str, path: &Path) -> Result<Option<String>, ResolveError> {
    let unsupported = |reason: String| ResolveError::Unsupported {
        path: path.to_path_buf(),
        reason,
    };

    let document = DeTable::parse(text)
        .map_err(|error| unsupported(format!("not valid TOML: {}", error.message())))?;
    let Some(tools) = document.get_ref().get("tools") else {
        return Ok(None);
    };
    let Some(tools) = tools.get_ref().as_table() else {
        return Err(unsupported("`tools` must be a table".to_owned()));
    };
    let Some(value) = tools.get(key) else {
        return Ok(None);
    };
    match value.get_ref().as_str() {
        Some(version) => Ok(Some(version.to_owned())),
        None => Err(unsupported(format!(
            "`tools.{key}` must be a version string; arrays and tables are not supported yet"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn versions(list: &[&str]) -> Vec<Version> {
        list.iter().map(|v| Version::parse(v).unwrap()).collect()
    }

    /// A temp folder with these files. Names may contain `/` for subfolders.
    fn project(files: &[(&str, &str)]) -> TempDir {
        let root = TempDir::new().unwrap();
        for (name, contents) in files {
            let path = root.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        root
    }

    /// Resolves through the project setting, so only matching is tested.
    fn pick_php(value: &str, installed: &[&str]) -> Result<Version, ResolveError> {
        let empty = TempDir::new().unwrap();
        resolve(
            Tool::Php,
            empty.path(),
            Some(value),
            &versions(installed),
            None,
        )
        .map(|r| r.version)
    }

    fn php(dir: &Path, installed: &[&str]) -> Result<Resolution, ResolveError> {
        resolve(Tool::Php, dir, None, &versions(installed), None)
    }

    fn node(dir: &Path, installed: &[&str]) -> Result<Resolution, ResolveError> {
        resolve(Tool::Node, dir, None, &versions(installed), None)
    }

    fn from_file(root: &TempDir, name: &str) -> Source {
        Source::VersionFile(root.path().join(name))
    }

    fn from_manifest(root: &TempDir, name: &str) -> Source {
        Source::Constraint(root.path().join(name))
    }

    // Matching table in the task file.

    #[test]
    fn major_minor_picks_highest_patch() {
        let got = pick_php("8.4", &["8.3.14", "8.4.1", "8.4.3"]).unwrap();
        assert_eq!(got, Version::new(8, 4, 3));
    }

    #[test]
    fn major_picks_highest_minor() {
        let got = pick_php("8", &["8.3.14", "8.4.3"]).unwrap();
        assert_eq!(got, Version::new(8, 4, 3));
    }

    #[test]
    fn full_version_matches_exactly() {
        let got = pick_php("8.4.1", &["8.4.1", "8.4.3"]).unwrap();
        assert_eq!(got, Version::new(8, 4, 1));
    }

    #[test]
    fn missing_version_is_not_installed() {
        let err = pick_php("8.2", &["8.3.14", "8.4.3"]).unwrap_err();
        assert_eq!(err.code(), "not_installed");
    }

    #[test]
    fn aliases_are_unsupported() {
        for alias in [
            "lts/*", "lts/iron", "LTS/Jod", "latest", "system", "node", "stable", "lts",
        ] {
            let err = pick_php(alias, &["8.4.3"]).unwrap_err();
            assert_eq!(err.code(), "unsupported_alias", "{alias}");
        }
    }

    #[test]
    fn empty_file_is_invalid_version_with_its_path() {
        for contents in ["", "\n\n", "# only a comment\n   \n"] {
            let root = project(&[(".php-version", contents)]);
            match php(root.path(), &["8.4.3"]).unwrap_err() {
                ResolveError::InvalidVersion { origin, .. } => {
                    assert_eq!(origin, from_file(&root, ".php-version"));
                }
                other => panic!("{contents:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn text_is_invalid_version_with_its_path() {
        let root = project(&[(".php-version", "abc\n")]);
        match php(root.path(), &["8.4.3"]).unwrap_err() {
            ResolveError::InvalidVersion { value, origin, .. } => {
                assert_eq!(value, "abc");
                assert_eq!(origin, from_file(&root, ".php-version"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn malformed_numbers_are_invalid() {
        for value in ["8.", ".4", "8.4.x", "8.4.1.2", "+8", "8.-4", "8.4.01", "v"] {
            let err = pick_php(value, &["8.4.3"]).unwrap_err();
            assert_eq!(err.code(), "invalid_version", "{value}");
        }
    }

    #[test]
    fn pre_release_is_picked_only_when_asked_for_exactly() {
        let installed = ["8.4.3", "8.5.0-rc1"];
        assert_eq!(pick_php("8", &installed).unwrap(), Version::new(8, 4, 3));
        assert_eq!(
            pick_php("8.5", &installed).unwrap_err().code(),
            "not_installed"
        );
        assert_eq!(
            pick_php("8.5.0-rc1", &installed).unwrap(),
            Version::parse("8.5.0-rc1").unwrap()
        );
    }

    #[test]
    fn leading_v_is_ignored() {
        let root = project(&[(".nvmrc", "v22\n")]);
        let got = node(root.path(), &["20.18.0", "22.1.0", "22.11.0"]).unwrap();
        assert_eq!(got.version, Version::new(22, 11, 0));
        assert_eq!(
            pick_php("v8.4.1", &["8.4.1"]).unwrap(),
            Version::new(8, 4, 1)
        );
    }

    // Upward search.

    #[test]
    fn file_in_parent_folder_is_used() {
        let root = project(&[(".php-version", "8.3"), ("app/src/.keep", "")]);
        let got = php(&root.path().join("app/src"), &["8.3.14", "8.4.3"]).unwrap();
        assert_eq!(got.version, Version::new(8, 3, 14));
        assert_eq!(got.source, from_file(&root, ".php-version"));
    }

    #[test]
    fn nearest_folder_wins() {
        let root = project(&[(".php-version", "8.3"), ("app/.php-version", "8.4")]);
        let got = php(&root.path().join("app"), &["8.3.14", "8.4.3"]).unwrap();
        assert_eq!(got.version, Version::new(8, 4, 3));
        assert_eq!(got.source, from_file(&root, "app/.php-version"));
    }

    #[test]
    fn not_installed_stops_the_search() {
        let root = project(&[(".php-version", "8.4"), ("app/.php-version", "8.2")]);
        let err = php(&root.path().join("app"), &["8.4.3"]).unwrap_err();
        assert_eq!(err.code(), "not_installed");
    }

    // Each kind of version file.

    #[test]
    fn reads_php_version() {
        let root = project(&[(".php-version", "# pinned\n8.4\n")]);
        let got = php(root.path(), &["8.4.3"]).unwrap();
        assert_eq!(got.version, Version::new(8, 4, 3));
        assert_eq!(got.source, from_file(&root, ".php-version"));
    }

    #[test]
    fn reads_nvmrc() {
        let root = project(&[(".nvmrc", "22\n")]);
        let got = node(root.path(), &["22.11.0"]).unwrap();
        assert_eq!(got.source, from_file(&root, ".nvmrc"));
    }

    #[test]
    fn reads_node_version() {
        let root = project(&[(".node-version", "22.11.0\n")]);
        let got = node(root.path(), &["22.11.0"]).unwrap();
        assert_eq!(got.source, from_file(&root, ".node-version"));
    }

    #[test]
    fn nvmrc_wins_over_node_version() {
        let root = project(&[(".nvmrc", "20"), (".node-version", "22")]);
        let got = node(root.path(), &["20.18.0", "22.11.0"]).unwrap();
        assert_eq!(got.source, from_file(&root, ".nvmrc"));
    }

    #[test]
    fn reads_tool_versions() {
        let root = project(&[(
            ".tool-versions",
            "# runtimes\nruby 3.3.0\n  php   8.4.1   8.3.14  # main first\nnodejs 22.11.0\n",
        )]);
        let installed = ["8.3.14", "8.4.1", "22.11.0"];
        let got = php(root.path(), &installed).unwrap();
        assert_eq!(got.version, Version::new(8, 4, 1));
        assert_eq!(got.source, from_file(&root, ".tool-versions"));
        let got = node(root.path(), &installed).unwrap();
        assert_eq!(got.version, Version::new(22, 11, 0));
    }

    #[test]
    fn tool_versions_accepts_node_key() {
        let root = project(&[(".tool-versions", "node 20\n")]);
        let got = node(root.path(), &["20.18.0"]).unwrap();
        assert_eq!(got.version, Version::new(20, 18, 0));
    }

    #[test]
    fn tool_versions_key_is_case_sensitive() {
        let root = project(&[(".tool-versions", "PHP 8.4\n")]);
        assert_eq!(
            php(root.path(), &["8.4.3"]).unwrap_err().code(),
            "no_version"
        );
    }

    #[test]
    fn tool_versions_key_without_version_is_invalid() {
        let root = project(&[(".tool-versions", "php\n")]);
        assert_eq!(
            php(root.path(), &["8.4.3"]).unwrap_err().code(),
            "invalid_version"
        );
    }

    #[test]
    fn reads_mise_toml() {
        let root = project(&[("mise.toml", "[env]\nA = \"1\"\n\n[tools]\nnode = \"22\"\n")]);
        let got = node(root.path(), &["22.11.0"]).unwrap();
        assert_eq!(got.version, Version::new(22, 11, 0));
        assert_eq!(got.source, from_file(&root, "mise.toml"));
    }

    #[test]
    fn mise_array_or_table_is_unsupported() {
        for contents in [
            "[tools]\nnode = [\"22\", \"20\"]\n",
            "[tools]\nnode = { version = \"22\" }\n",
            "tools = \"node\"\n",
            "[tools\n",
        ] {
            let root = project(&[("mise.toml", contents)]);
            let err = node(root.path(), &["22.11.0"]).unwrap_err();
            assert_eq!(err.code(), "unsupported", "{contents:?}");
        }
    }

    #[test]
    fn file_without_the_tool_is_skipped() {
        // Same folder: `.tool-versions` has no php, so `mise.toml` is next.
        let root = project(&[
            (".tool-versions", "nodejs 22.11.0\n"),
            ("mise.toml", "[tools]\nphp = \"8.3\"\n"),
        ]);
        let got = php(root.path(), &["8.3.14"]).unwrap();
        assert_eq!(got.source, from_file(&root, "mise.toml"));

        // Parent folder: the child's files have no node at all.
        let root = project(&[
            (".nvmrc", "22"),
            ("app/.tool-versions", "php 8.4\n"),
            ("app/mise.toml", "[env]\nA = \"1\"\n"),
        ]);
        let got = node(&root.path().join("app"), &["22.11.0"]).unwrap();
        assert_eq!(got.source, from_file(&root, ".nvmrc"));
    }

    #[test]
    fn order_inside_one_folder() {
        let root = project(&[
            (".php-version", "8.4"),
            (".tool-versions", "php 8.3\n"),
            ("mise.toml", "[tools]\nphp = \"8.2\"\n"),
        ]);
        let got = php(root.path(), &["8.2.27", "8.3.14", "8.4.3"]).unwrap();
        assert_eq!(got.source, from_file(&root, ".php-version"));
    }

    #[test]
    fn windows_line_endings_and_bom() {
        let root = project(&[
            (".nvmrc", "\u{feff}v22\r\n"),
            (".tool-versions", "php 8.4\r\n"),
            ("app/mise.toml", "[tools]\r\nphp = \"8.3\"\r\n"),
        ]);
        let installed = ["8.3.14", "8.4.3", "22.11.0"];
        assert_eq!(
            node(root.path(), &installed).unwrap().version,
            Version::new(22, 11, 0)
        );
        assert_eq!(
            php(root.path(), &installed).unwrap().version,
            Version::new(8, 4, 3)
        );
        let got = php(&root.path().join("app"), &installed).unwrap();
        assert_eq!(got.version, Version::new(8, 3, 14));
    }

    // Constraints in manifests.

    #[test]
    fn reads_composer_json() {
        let root = project(&[(
            "composer.json",
            r#"{"require": {"php": "^8.2", "laravel/framework": "^11.0"}}"#,
        )]);
        let got = php(root.path(), &["8.1.31", "8.3.14", "8.4.3"]).unwrap();
        assert_eq!(got.version, Version::new(8, 4, 3));
        assert_eq!(got.source, from_manifest(&root, "composer.json"));
    }

    #[test]
    fn reads_package_json() {
        let root = project(&[("package.json", r#"{"engines": {"node": ">=18 <23"}}"#)]);
        let got = node(root.path(), &["18.20.4", "22.11.0", "23.3.0"]).unwrap();
        assert_eq!(got.version, Version::new(22, 11, 0));
        assert_eq!(got.source, from_manifest(&root, "package.json"));
    }

    #[test]
    fn each_tool_reads_only_its_manifest() {
        let root = project(&[
            ("composer.json", r#"{"require": {"php": "^8.2"}}"#),
            (
                "package.json",
                r#"{"engines": {"node": "^22"}, "require": {"php": "^8.2"}}"#,
            ),
        ]);
        let got = node(root.path(), &["22.11.0"]).unwrap();
        assert_eq!(got.source, from_manifest(&root, "package.json"));
        let only_node = project(&[("package.json", r#"{"engines": {"node": "^22"}}"#)]);
        let err = php(only_node.path(), &["8.4.3"]).unwrap_err();
        assert_eq!(err.code(), "no_version");
    }

    #[test]
    fn constraint_in_parent_folder_is_used() {
        // `app` has no `require.php`, and `app/src` has `require` as `[]`.
        let root = project(&[
            ("composer.json", r#"{"require": {"php": "^8.2"}}"#),
            (
                "app/composer.json",
                r#"{"require": {"laravel/framework": "^11.0"}}"#,
            ),
            ("app/src/composer.json", r#"{"name": "a/b", "require": []}"#),
        ]);
        let got = php(&root.path().join("app/src"), &["8.3.14"]).unwrap();
        assert_eq!(got.source, from_manifest(&root, "composer.json"));
    }

    #[test]
    fn version_file_in_parent_wins_over_constraint() {
        let root = project(&[
            (".php-version", "8.3"),
            ("app/composer.json", r#"{"require": {"php": "^8.4"}}"#),
        ]);
        let got = php(&root.path().join("app"), &["8.3.14", "8.4.3"]).unwrap();
        assert_eq!(got.source, from_file(&root, ".php-version"));
    }

    #[test]
    fn no_matching_version_stops_the_search() {
        let root = project(&[
            ("composer.json", r#"{"require": {"php": "^7.4"}}"#),
            ("app/composer.json", r#"{"require": {"php": "^8.2"}}"#),
        ]);
        match php(&root.path().join("app"), &["7.4.33", "8.1.31"]).unwrap_err() {
            ResolveError::NoMatchingVersion {
                constraint, origin, ..
            } => {
                assert_eq!(constraint, "^8.2");
                assert_eq!(origin, from_manifest(&root, "app/composer.json"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn any_version_is_skipped() {
        let installed = versions(&["20.18.0", "22.11.0"]);
        let root = project(&[
            ("package.json", r#"{"engines": {"node": "^20"}}"#),
            ("app/package.json", r#"{"engines": {"node": "*"}}"#),
        ]);
        let got = node(&root.path().join("app"), &["20.18.0", "22.11.0"]).unwrap();
        assert_eq!(got.source, from_manifest(&root, "package.json"));

        for any in ["*", "x", ""] {
            let json = format!(r#"{{"engines": {{"node": "{any}"}}}}"#);
            let root = project(&[("package.json", &json)]);
            let got = resolve(Tool::Node, root.path(), None, &installed, Some("20")).unwrap();
            assert_eq!(got.source, Source::GlobalDefault, "{any:?}");
            assert_eq!(got.version, Version::new(20, 18, 0));
        }
    }

    #[test]
    fn bad_manifest_is_unsupported_with_its_path() {
        for contents in [
            r#"{"require": {"php": "^8.2"}"#,
            r#"{"require": {"php": 8.2}}"#,
            r#"{"require": {"php": ["^8.2"]}}"#,
            "",
        ] {
            let root = project(&[("composer.json", contents)]);
            match php(root.path(), &["8.4.3"]).unwrap_err() {
                ResolveError::Unsupported { path, .. } => {
                    assert_eq!(path, root.path().join("composer.json"));
                }
                other => panic!("{contents:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn invalid_constraint_is_invalid_version_with_its_path() {
        let root = project(&[("composer.json", r#"{"require": {"php": " dev-master "}}"#)]);
        match php(root.path(), &["8.4.3"]).unwrap_err() {
            ResolveError::InvalidVersion { value, origin, .. } => {
                assert_eq!(value, "dev-master");
                assert_eq!(origin, from_manifest(&root, "composer.json"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn manifest_with_windows_line_endings_and_bom() {
        let root = project(&[
            (
                "composer.json",
                "\u{feff}{\r\n  \"require\": {\r\n    \"php\": \"^8.2\"\r\n  }\r\n}\r\n",
            ),
            (
                "package.json",
                "\u{feff}{\r\n  \"engines\": {\r\n    \"node\": \">=20\"\r\n  }\r\n}\r\n",
            ),
        ]);
        let installed = ["8.4.3", "22.11.0"];
        let got = php(root.path(), &installed).unwrap();
        assert_eq!(got.version, Version::new(8, 4, 3));
        let got = node(root.path(), &installed).unwrap();
        assert_eq!(got.version, Version::new(22, 11, 0));
    }

    // Priority between sources.

    #[test]
    fn project_setting_wins_over_files() {
        let root = project(&[(".php-version", "8.3")]);
        let got = resolve(
            Tool::Php,
            root.path(),
            Some("8.4"),
            &versions(&["8.3.14", "8.4.3"]),
            Some("8.3"),
        )
        .unwrap();
        assert_eq!(got.version, Version::new(8, 4, 3));
        assert_eq!(got.source, Source::ProjectSetting);

        let root = project(&[("composer.json", r#"{"require": {"php": "^8.4"}}"#)]);
        let installed = versions(&["8.3.14", "8.4.3"]);
        let got = resolve(Tool::Php, root.path(), Some("8.3"), &installed, None).unwrap();
        assert_eq!(got.source, Source::ProjectSetting);
    }

    #[test]
    fn global_default_only_without_files() {
        let installed = versions(&["8.3.14", "8.4.3"]);

        let with_file = project(&[(".php-version", "8.4")]);
        let got = resolve(Tool::Php, with_file.path(), None, &installed, Some("8.3")).unwrap();
        assert_eq!(got.source, from_file(&with_file, ".php-version"));

        let with_constraint = project(&[("composer.json", r#"{"require": {"php": "^8.4"}}"#)]);
        let got = resolve(
            Tool::Php,
            with_constraint.path(),
            None,
            &installed,
            Some("8.3"),
        )
        .unwrap();
        assert_eq!(got.source, from_manifest(&with_constraint, "composer.json"));

        let without = TempDir::new().unwrap();
        let got = resolve(Tool::Php, without.path(), None, &installed, Some("8.3")).unwrap();
        assert_eq!(got.version, Version::new(8, 3, 14));
        assert_eq!(got.source, Source::GlobalDefault);
    }

    #[test]
    fn nothing_set_is_no_version() {
        let empty = TempDir::new().unwrap();
        let err = resolve(
            Tool::Node,
            empty.path(),
            None,
            &versions(&["22.11.0"]),
            None,
        );
        assert_eq!(err.unwrap_err().code(), "no_version");
    }

    // Errors.

    #[test]
    fn unreadable_file_is_io() {
        // A folder where the file should be: reading it fails, but not with NotFound.
        let root = project(&[(".php-version/.keep", "")]);
        match php(root.path(), &["8.4.3"]).unwrap_err() {
            ResolveError::Io { path, .. } => assert_eq!(path, root.path().join(".php-version")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn every_error_has_its_code() {
        let path = PathBuf::from("mise.toml");
        let origin = Source::VersionFile(path.clone());
        let cases = [
            (ResolveError::NoVersion { tool: Tool::Php }, "no_version"),
            (
                ResolveError::NotInstalled {
                    tool: Tool::Php,
                    requested: "8.2".to_owned(),
                    origin: origin.clone(),
                },
                "not_installed",
            ),
            (
                ResolveError::NoMatchingVersion {
                    tool: Tool::Php,
                    constraint: "^8.2".to_owned(),
                    origin: Source::Constraint(PathBuf::from("composer.json")),
                },
                "no_matching_version",
            ),
            (
                ResolveError::InvalidVersion {
                    tool: Tool::Php,
                    value: "abc".to_owned(),
                    origin: origin.clone(),
                },
                "invalid_version",
            ),
            (
                ResolveError::UnsupportedAlias {
                    tool: Tool::Node,
                    value: "lts/*".to_owned(),
                    origin,
                },
                "unsupported_alias",
            ),
            (
                ResolveError::Unsupported {
                    path: path.clone(),
                    reason: "array".to_owned(),
                },
                "unsupported",
            ),
            (
                ResolveError::Io {
                    path,
                    source: io::Error::from(io::ErrorKind::PermissionDenied),
                },
                "io",
            ),
        ];
        for (error, code) in cases {
            assert_eq!(error.code(), code, "{error}");
        }
    }

    #[test]
    fn best_match_uses_the_resolver_rules() {
        let list = versions(&["8.3.14", "8.4.1", "8.4.23", "8.4.24-rc1", "8.5.0"]);
        let best = |value| best_match(value, &list).expect("a valid request");
        let v = |text| Some(Version::parse(text).unwrap());
        assert_eq!(best("8.4"), v("8.4.23"));
        assert_eq!(best(" v8.4 "), v("8.4.23"));
        assert_eq!(best("8"), v("8.5.0"));
        assert_eq!(best("8.4.1"), v("8.4.1"));
        assert_eq!(best("8.4.24-rc1"), v("8.4.24-rc1"));
        assert_eq!(best("8.2"), None);
        assert_eq!(best("8.4.24"), None);
    }

    #[test]
    fn best_match_refuses_aliases_and_junk() {
        let list = versions(&["22.11.0"]);
        for alias in ["latest", "lts/*", "LTS/iron", "node"] {
            let err = best_match(alias, &list).expect_err(alias);
            assert_eq!(err, BadRequest::Alias);
            assert_eq!(err.code(), "unsupported_alias");
        }
        for junk in ["", "x", "8.x", "+8", "^8.4"] {
            let err = best_match(junk, &list).expect_err(junk);
            assert_eq!(err.code(), "invalid_version", "{junk:?}");
        }
    }
}
