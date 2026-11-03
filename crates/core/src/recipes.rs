//! Reads and validates the TOML recipes that describe runtimes and services.
//!
//! A recipe says **how** a version is installed: the archive format, which
//! folder its files go to, where each program lands, and how to check the
//! result. The signed manifest (`installer::manifest`) says **what** exists:
//! the versions, the real URL of each file, and its sha256. The installer only
//! downloads URLs from the signed manifest; the `url` template here is for
//! `cargo xtask manifest`.
//!
//! The recipes live in `recipes/*.toml` and are built into the binary with
//! `include_str!`, so nobody can change them after install. [`builtin`] parses
//! and validates each one once.
//!
//! This module is not behind the `full` feature: it is pure parsing, and the
//! shim may read program paths from it later.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::OnceLock;

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::runtimes::Runtime;

/// An operating system and CPU pair that a recipe can target. The text form
/// (`darwin-arm64`) is the key in recipes and in the manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    /// macOS on Apple Silicon.
    DarwinArm64,
    /// macOS on Intel.
    DarwinX64,
    /// Linux on x86_64.
    LinuxX64,
    /// Linux on aarch64.
    LinuxArm64,
    /// Windows on x86_64 (MSVC target only).
    WindowsX64,
    /// One file for every system (Composer).
    Any,
}

impl Platform {
    /// Every platform, in the order of the recipe files.
    pub const ALL: [Platform; 6] = [
        Platform::DarwinArm64,
        Platform::DarwinX64,
        Platform::LinuxX64,
        Platform::LinuxArm64,
        Platform::WindowsX64,
        Platform::Any,
    ];

    /// The key used in recipes and in the manifest.
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::DarwinArm64 => "darwin-arm64",
            Platform::DarwinX64 => "darwin-x64",
            Platform::LinuxX64 => "linux-x64",
            Platform::LinuxArm64 => "linux-arm64",
            Platform::WindowsX64 => "windows-x64",
            Platform::Any => "any",
        }
    }

    /// The platform this binary was built for, or `None` if Marsa has no
    /// builds for it (the installer then fails with
    /// `install_unsupported_platform`).
    pub fn current() -> Option<Platform> {
        // `cfg!` outside `platform` on purpose: that module sits behind `full`,
        // and recipes do not (task M2-01). This only reads the build target.
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Some(Platform::DarwinArm64)
        } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            Some(Platform::DarwinX64)
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Some(Platform::LinuxX64)
        } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            Some(Platform::LinuxArm64)
        } else if cfg!(all(
            target_os = "windows",
            target_arch = "x86_64",
            target_env = "msvc"
        )) {
            Some(Platform::WindowsX64)
        } else {
            None
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Platform {
    type Err = UnknownPlatform;

    fn from_str(key: &str) -> Result<Self, Self::Err> {
        Platform::ALL
            .into_iter()
            .find(|p| p.as_str() == key)
            .ok_or_else(|| UnknownPlatform(key.to_string()))
    }
}

/// A platform key that is not in [`Platform::ALL`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown platform {0:?}")]
pub struct UnknownPlatform(pub String);

/// How a downloaded file is unpacked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ArchiveFormat {
    #[serde(rename = "tar.gz")]
    TarGz,
    #[serde(rename = "zip")]
    Zip,
    /// One file, saved as is under [`Artifact::file_name`].
    #[serde(rename = "raw")]
    Raw,
}

/// One downloaded file of a version. `name` links it to the same entry in the
/// manifest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub name: String,
    /// Template with `{version}`, used by `cargo xtask manifest` only.
    pub url: String,
    pub format: ArchiveFormat,
    /// Leading path components dropped from every archive entry.
    #[serde(default)]
    pub strip_components: u32,
    /// Folder inside the version folder, with `/` between components. Empty
    /// means the version folder itself.
    #[serde(default)]
    pub into: String,
    /// The saved file name. Required for `raw`, refused for the others.
    #[serde(default)]
    pub file_name: Option<String>,
}

impl Artifact {
    /// The folder this artifact unpacks into: `version_dir` joined with
    /// [`Artifact::into`], one component at a time.
    pub fn into_dir(&self, version_dir: &Path) -> PathBuf {
        join_relative(version_dir, &self.into)
    }
}

/// The check run after install: `program` (a key in the platform's programs)
/// with `args`; its output must start with `expect_prefix`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Template with `{version}`.
    pub expect_prefix: String,
}

impl Check {
    /// [`Check::expect_prefix`] with `{version}` filled in.
    pub fn expected_prefix(&self, version: &Version) -> String {
        fill(&self.expect_prefix, version)
    }
}

/// What a recipe says for one platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformRecipe {
    programs: BTreeMap<String, String>,
    artifacts: Vec<Artifact>,
}

impl PlatformRecipe {
    /// The files to download, in recipe order. Never empty.
    pub fn artifacts(&self) -> &[Artifact] {
        &self.artifacts
    }

    /// The artifact called `name`.
    pub fn artifact(&self, name: &str) -> Option<&Artifact> {
        self.artifacts.iter().find(|a| a.name == name)
    }

    /// Program names (`php`, `npm-cli`), sorted.
    pub fn program_names(&self) -> impl Iterator<Item = &str> {
        self.programs.keys().map(String::as_str)
    }

    /// The path of program `name` inside `version_dir`, built one component
    /// at a time with `Path::push`. `None` if the recipe has no such program.
    pub fn program_path(&self, version_dir: &Path, name: &str) -> Option<PathBuf> {
        self.programs
            .get(name)
            .map(|rel| join_relative(version_dir, rel))
    }
}

/// A parsed and validated recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    name: String,
    check: Option<Check>,
    platforms: BTreeMap<Platform, PlatformRecipe>,
}

impl Recipe {
    /// The runtime name (`php`), the same as the folder under `runtimes/`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The check run after install, if the recipe has one.
    pub fn check(&self) -> Option<&Check> {
        self.check.as_ref()
    }

    /// The recipe for `platform`. A recipe with an `any` entry (Composer)
    /// returns it for every platform.
    pub fn platform(&self, platform: Platform) -> Option<&PlatformRecipe> {
        self.platforms
            .get(&platform)
            .or_else(|| self.platforms.get(&Platform::Any))
    }

    /// Every platform the recipe lists, in [`Platform`] order.
    pub fn platforms(&self) -> impl Iterator<Item = (Platform, &PlatformRecipe)> {
        self.platforms.iter().map(|(p, r)| (*p, r))
    }

    /// The `url` template of artifact `name` on `platform`, with `{version}`
    /// filled in. `None` if there is no such platform or artifact.
    pub fn artifact_url(
        &self,
        platform: Platform,
        name: &str,
        version: &Version,
    ) -> Option<String> {
        self.platform(platform)?
            .artifact(name)
            .map(|a| fill(&a.url, version))
    }
}

/// Why a recipe was refused. [`RecipeError::code`] gives a stable name that
/// the UI maps to a message. Built-in recipes are checked by tests, so these
/// are programmer errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecipeError {
    /// Bad TOML, a missing or unknown field, or a broken rule (in `message`).
    #[error("recipe {recipe} is not valid: {message}")]
    Invalid { recipe: String, message: String },

    #[error("recipe {recipe}: unknown platform {key:?}")]
    UnknownPlatform { recipe: String, key: String },

    /// A path that is absolute, or has `..`, `.`, `\`, `:`, or an empty part.
    #[error("recipe {recipe}: path {path:?} must be relative, with `/` and no `..`")]
    BadPath { recipe: String, path: String },

    /// A template with a `{...}` other than `{version}`, or a lone brace.
    #[error("recipe {recipe}: template {template:?} may only use {{version}}")]
    BadTemplate { recipe: String, template: String },
}

impl RecipeError {
    /// Stable error name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            RecipeError::Invalid { .. } => "recipe_invalid",
            RecipeError::UnknownPlatform { .. } => "recipe_unknown_platform",
            RecipeError::BadPath { .. } => "recipe_bad_path",
            RecipeError::BadTemplate { .. } => "recipe_bad_template",
        }
    }
}

/// The built-in recipe for `runtime`, parsed and validated on first use.
///
/// # Errors
///
/// Any [`RecipeError`] from [`parse`], or `recipe_invalid` if the recipe's
/// `name` is not the runtime's. A test reads every built-in recipe, so this
/// fails in CI before it can reach a user.
pub fn builtin(runtime: Runtime) -> Result<&'static Recipe, RecipeError> {
    // `OnceLock`: parsed once per process, then shared (like a singleton).
    static PHP: OnceLock<Result<Recipe, RecipeError>> = OnceLock::new();
    static NODE: OnceLock<Result<Recipe, RecipeError>> = OnceLock::new();
    static COMPOSER: OnceLock<Result<Recipe, RecipeError>> = OnceLock::new();

    let (cell, file, text) = match runtime {
        Runtime::Php => (&PHP, "php.toml", include_str!("../../../recipes/php.toml")),
        Runtime::Node => (
            &NODE,
            "node.toml",
            include_str!("../../../recipes/node.toml"),
        ),
        Runtime::Composer => (
            &COMPOSER,
            "composer.toml",
            include_str!("../../../recipes/composer.toml"),
        ),
    };
    cell.get_or_init(|| {
        let recipe = parse(file, text)?;
        if recipe.name != runtime.dir_name() {
            return Err(invalid(
                file,
                format!(
                    "name is {:?}, expected {:?}",
                    recipe.name,
                    runtime.dir_name()
                ),
            ));
        }
        Ok(recipe)
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// Parses and validates the text of a recipe. `source` (a file name) is only
/// used in errors.
///
/// # Errors
///
/// `recipe_invalid`, `recipe_unknown_platform`, `recipe_bad_path`, or
/// `recipe_bad_template` (see [`RecipeError`]).
pub fn parse(source: &str, text: &str) -> Result<Recipe, RecipeError> {
    let raw: RawRecipe = toml::from_str(text).map_err(|e| invalid(source, e.to_string()))?;

    if let Some(check) = &raw.check {
        check_template(source, &check.expect_prefix)?;
    }
    if raw.platform.is_empty() {
        return Err(invalid(source, "no platform".to_string()));
    }

    let mut platforms = BTreeMap::new();
    for (key, entry) in raw.platform {
        let platform = key.parse::<Platform>().map_err(|UnknownPlatform(key)| {
            RecipeError::UnknownPlatform {
                recipe: source.to_string(),
                key,
            }
        })?;
        let recipe = validate_platform(source, platform, entry)?;
        if let Some(check) = &raw.check
            && !recipe.programs.contains_key(&check.program)
        {
            return Err(invalid(
                source,
                format!(
                    "{platform}: check program {:?} is not in programs",
                    check.program
                ),
            ));
        }
        platforms.insert(platform, recipe);
    }

    Ok(Recipe {
        name: raw.name,
        check: raw.check,
        platforms,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecipe {
    name: String,
    check: Option<Check>,
    // String keys, so an unknown key gives `recipe_unknown_platform`, not a
    // generic serde error.
    #[serde(default)]
    platform: BTreeMap<String, RawPlatform>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlatform {
    #[serde(default)]
    programs: BTreeMap<String, String>,
    #[serde(default)]
    artifact: Vec<Artifact>,
}

fn validate_platform(
    source: &str,
    platform: Platform,
    raw: RawPlatform,
) -> Result<PlatformRecipe, RecipeError> {
    if raw.artifact.is_empty() {
        return Err(invalid(source, format!("{platform}: no artifact")));
    }

    let mut names = BTreeSet::new();
    for artifact in &raw.artifact {
        if !names.insert(artifact.name.as_str()) {
            return Err(invalid(
                source,
                format!("{platform}: artifact {:?} is listed twice", artifact.name),
            ));
        }
        check_template(source, &artifact.url)?;
        check_path(source, &artifact.into, true)?;
        match (artifact.format, &artifact.file_name) {
            (ArchiveFormat::Raw, None) => {
                return Err(invalid(
                    source,
                    format!(
                        "{platform}: raw artifact {:?} needs file_name",
                        artifact.name
                    ),
                ));
            }
            (ArchiveFormat::Raw, Some(file_name)) => {
                // A file name is one component: no `/` at all.
                if file_name.contains('/') {
                    return Err(bad_path(source, file_name));
                }
                check_path(source, file_name, false)?;
            }
            (_, Some(_)) => {
                return Err(invalid(
                    source,
                    format!(
                        "{platform}: file_name is only for raw ({:?})",
                        artifact.name
                    ),
                ));
            }
            (_, None) => {}
        }
    }

    for path in raw.programs.values() {
        check_path(source, path, false)?;
    }

    Ok(PlatformRecipe {
        programs: raw.programs,
        artifacts: raw.artifact,
    })
}

/// A recipe path is relative, uses `/` between components, and has no empty,
/// `.`, or `..` component. `\` and `:` are refused so that `C:\x` or `C:x`
/// cannot name a drive on Windows. `allow_empty` is for `into`.
fn check_path(source: &str, path: &str, allow_empty: bool) -> Result<(), RecipeError> {
    if path.is_empty() {
        return if allow_empty {
            Ok(())
        } else {
            Err(bad_path(source, path))
        };
    }
    let ok = !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    if ok {
        Ok(())
    } else {
        Err(bad_path(source, path))
    }
}

/// A template may hold `{version}` and nothing else between braces.
fn check_template(source: &str, template: &str) -> Result<(), RecipeError> {
    let rest = template.replace("{version}", "");
    if rest.contains(['{', '}']) {
        return Err(RecipeError::BadTemplate {
            recipe: source.to_string(),
            template: template.to_string(),
        });
    }
    Ok(())
}

fn fill(template: &str, version: &Version) -> String {
    template.replace("{version}", &version.to_string())
}

/// `base` joined with each `/`-separated part of a checked relative path. An
/// empty path gives `base` itself, with no trailing separator.
fn join_relative(base: &Path, rel: &str) -> PathBuf {
    let mut path = base.to_path_buf();
    for part in rel.split('/').filter(|part| !part.is_empty()) {
        path.push(part);
    }
    path
}

fn invalid(source: &str, message: String) -> RecipeError {
    RecipeError::Invalid {
        recipe: source.to_string(),
        message,
    }
}

fn bad_path(source: &str, path: &str) -> RecipeError {
    RecipeError::BadPath {
        recipe: source.to_string(),
        path: path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNTIMES: [Runtime; 3] = [Runtime::Php, Runtime::Node, Runtime::Composer];

    /// The five platforms Marsa builds for (all but `any`).
    const CONCRETE: [Platform; 5] = [
        Platform::DarwinArm64,
        Platform::DarwinX64,
        Platform::LinuxX64,
        Platform::LinuxArm64,
        Platform::WindowsX64,
    ];

    const GOOD_PLATFORM: &str = r#"
[platform.linux-x64]
programs = { php = "bin/php" }

[[platform.linux-x64.artifact]]
name = "cli"
url = "https://example.test/php-{version}.tar.gz"
format = "tar.gz"
into = "bin"
"#;

    fn v(text: &str) -> Version {
        Version::parse(text).expect("test versions are valid")
    }

    /// A small valid recipe head; `platform` is the TOML of the platform part.
    fn recipe_with(platform: &str) -> String {
        format!(
            "name = \"php\"\n\
             [check]\nprogram = \"php\"\nargs = [\"-v\"]\nexpect_prefix = \"PHP {{version}} \"\n\
             {platform}"
        )
    }

    /// `GOOD_PLATFORM` with `from` replaced by `to`, inside a full recipe.
    fn good_with(from: &str, to: &str) -> String {
        recipe_with(&GOOD_PLATFORM.replace(from, to))
    }

    fn error_code(text: &str) -> &'static str {
        parse("test.toml", text)
            .expect_err("the recipe must be refused")
            .code()
    }

    // --- the built-in recipes ---

    #[test]
    fn builtin_recipes_parse_and_every_platform_has_an_artifact() {
        for runtime in RUNTIMES {
            let recipe = builtin(runtime).expect("built-in recipe is valid");
            assert_eq!(recipe.name(), runtime.dir_name());
            for (platform, entry) in recipe.platforms() {
                assert!(!entry.artifacts().is_empty(), "{runtime:?} {platform}");
            }
        }
    }

    #[test]
    fn php_and_node_cover_the_five_platforms() {
        for runtime in [Runtime::Php, Runtime::Node] {
            let recipe = builtin(runtime).expect("built-in recipe is valid");
            let listed: Vec<Platform> = recipe.platforms().map(|(p, _)| p).collect();
            assert_eq!(listed, CONCRETE, "{runtime:?}");
            assert!(recipe.check().is_some(), "{runtime:?} has a check");
        }
    }

    #[test]
    fn composer_is_one_raw_file_for_every_platform() {
        let recipe = builtin(Runtime::Composer).expect("built-in recipe is valid");
        assert!(recipe.check().is_none());
        for platform in CONCRETE {
            let entry = recipe
                .platform(platform)
                .expect("any covers every platform");
            let [artifact] = entry.artifacts() else {
                panic!("one artifact expected");
            };
            assert_eq!(artifact.format, ArchiveFormat::Raw);
            assert_eq!(artifact.file_name.as_deref(), Some("composer.phar"));
        }
    }

    #[test]
    fn program_paths_match_the_shim_layout() {
        let dir = Path::new("data").join("runtimes").join("php").join("8.4.3");
        let php = builtin(Runtime::Php).expect("built-in recipe is valid");
        let unix = php.platform(Platform::LinuxX64).expect("linux-x64");
        assert_eq!(
            unix.program_path(&dir, "php"),
            Some(dir.join("bin").join("php"))
        );
        let windows = php.platform(Platform::WindowsX64).expect("windows-x64");
        assert_eq!(windows.program_path(&dir, "php"), Some(dir.join("php.exe")));
        assert_eq!(windows.program_path(&dir, "php-fpm"), None);

        let node = builtin(Runtime::Node).expect("built-in recipe is valid");
        let npm_bin = |base: &Path| base.join("node_modules").join("npm").join("bin");
        let mac = node.platform(Platform::DarwinArm64).expect("darwin-arm64");
        assert_eq!(
            mac.program_path(&dir, "npm-cli"),
            Some(npm_bin(&dir.join("lib")).join("npm-cli.js"))
        );
        let win = node.platform(Platform::WindowsX64).expect("windows-x64");
        assert_eq!(
            win.program_path(&dir, "npx-cli"),
            Some(npm_bin(&dir).join("npx-cli.js"))
        );
    }

    #[test]
    fn artifact_url_fills_the_version() {
        let php = builtin(Runtime::Php).expect("built-in recipe is valid");
        assert_eq!(
            php.artifact_url(Platform::DarwinArm64, "fpm", &v("8.4.23"))
                .as_deref(),
            Some(
                "https://dl.static-php.dev/static-php-cli/bulk/php-8.4.23-fpm-macos-aarch64.tar.gz"
            )
        );
        let node = builtin(Runtime::Node).expect("built-in recipe is valid");
        assert_eq!(
            node.artifact_url(Platform::WindowsX64, "archive", &v("22.11.0"))
                .as_deref(),
            Some("https://nodejs.org/dist/v22.11.0/node-v22.11.0-win-x64.zip")
        );
        assert_eq!(
            php.artifact_url(Platform::LinuxX64, "nope", &v("8.4.23")),
            None
        );
        assert_eq!(php.artifact_url(Platform::Any, "cli", &v("8.4.23")), None);
    }

    #[test]
    fn check_prefix_fills_the_version() {
        let node = builtin(Runtime::Node).expect("built-in recipe is valid");
        let check = node.check().expect("node has a check");
        assert_eq!(check.program, "node");
        assert_eq!(check.expected_prefix(&v("22.11.0")), "v22.11.0");
    }

    #[test]
    fn into_dir_joins_each_component() {
        let php = builtin(Runtime::Php).expect("built-in recipe is valid");
        let artifact = |platform, name| {
            php.platform(platform)
                .and_then(|p| p.artifact(name))
                .expect("artifact exists")
        };
        let dir = Path::new("v");
        assert_eq!(
            artifact(Platform::LinuxArm64, "cli").into_dir(dir),
            dir.join("bin")
        );
        assert_eq!(artifact(Platform::WindowsX64, "zip").into_dir(dir), dir);
    }

    // --- Platform ---

    #[test]
    fn platform_text_round_trips() {
        for platform in Platform::ALL {
            let key = platform.to_string();
            assert_eq!(key.parse::<Platform>(), Ok(platform));
            // serde uses the same keys as `as_str`.
            let json = serde_json::to_string(&platform).expect("serialize");
            assert_eq!(json, format!("\"{key}\""));
            let back: Platform = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, platform);
        }
        assert_eq!(
            "darwin-amd64".parse::<Platform>(),
            Err(UnknownPlatform("darwin-amd64".to_string()))
        );
    }

    #[test]
    fn current_platform_is_known_on_ci_systems() {
        // CI runs on macOS arm64, Linux x64, and Windows x64 (MSVC).
        let current = Platform::current().expect("CI systems are supported");
        assert_ne!(current, Platform::Any);
    }

    // --- one test per row of the validation table ---

    #[test]
    fn small_recipe_is_valid() {
        let recipe = parse("test.toml", &recipe_with(GOOD_PLATFORM)).expect("valid");
        let linux = recipe.platform(Platform::LinuxX64).expect("linux-x64");
        assert_eq!(linux.artifacts()[0].strip_components, 0);
        assert_eq!(linux.program_names().collect::<Vec<_>>(), ["php"]);
        assert!(recipe.platform(Platform::DarwinArm64).is_none());
    }

    #[test]
    fn invalid_toml_or_missing_field() {
        assert_eq!(error_code("name = "), "recipe_invalid");
        assert_eq!(
            error_code(&good_with("name = \"cli\"", "")),
            "recipe_invalid"
        );
        assert_eq!(error_code(&recipe_with("")), "recipe_invalid");
        assert_eq!(
            error_code(&good_with("into", "unpack_to")),
            "recipe_invalid"
        );
    }

    #[test]
    fn unknown_platform() {
        assert_eq!(
            error_code(&good_with("linux-x64", "linux-riscv64")),
            "recipe_unknown_platform"
        );
    }

    #[test]
    fn unknown_format_or_raw_without_file_name() {
        assert_eq!(
            error_code(&good_with("\"tar.gz\"", "\"tar.zst\"")),
            "recipe_invalid"
        );
        assert_eq!(
            error_code(&good_with("\"tar.gz\"", "\"raw\"")),
            "recipe_invalid"
        );
        let file_name_on_zip = good_with(
            "format = \"tar.gz\"",
            "format = \"zip\"\nfile_name = \"php.zip\"",
        );
        assert_eq!(error_code(&file_name_on_zip), "recipe_invalid");
    }

    #[test]
    fn bad_paths_are_refused() {
        let bad_paths = [
            "/abs",
            "../up",
            "bin/../..",
            "bin\\php",
            "C:/php",
            "bin//php",
            "./bin",
            "bin/",
        ];
        for bad in bad_paths {
            let into = good_with("into = \"bin\"", &format!("into = {bad:?}"));
            assert_eq!(error_code(&into), "recipe_bad_path", "into {bad:?}");
            let program = good_with("\"bin/php\"", &format!("{bad:?}"));
            assert_eq!(error_code(&program), "recipe_bad_path", "program {bad:?}");
        }
        assert_eq!(
            error_code(&good_with("\"bin/php\"", "\"\"")),
            "recipe_bad_path"
        );
        let raw_in_folder = good_with(
            "format = \"tar.gz\"",
            "format = \"raw\"\nfile_name = \"x/composer.phar\"",
        );
        assert_eq!(error_code(&raw_in_folder), "recipe_bad_path");
    }

    #[test]
    fn templates_accept_only_version() {
        for bad in [
            "{arch}",
            "{ version }",
            "{version",
            "version}",
            "{{version}}",
        ] {
            let url = good_with("{version}", bad);
            assert_eq!(error_code(&url), "recipe_bad_template", "url {bad:?}");
        }
        let prefix = recipe_with(GOOD_PLATFORM).replace("PHP {version} ", "PHP {os}");
        assert_eq!(error_code(&prefix), "recipe_bad_template");
    }

    #[test]
    fn platform_without_artifact_or_with_a_twice_listed_name() {
        let none = recipe_with("[platform.linux-x64]\nprograms = { php = \"bin/php\" }\n");
        assert_eq!(error_code(&none), "recipe_invalid");

        let twice = format!(
            "{GOOD_PLATFORM}\n[[platform.linux-x64.artifact]]\nname = \"cli\"\n\
             url = \"https://example.test/other-{{version}}.tar.gz\"\nformat = \"tar.gz\"\n"
        );
        assert_eq!(error_code(&recipe_with(&twice)), "recipe_invalid");
    }

    #[test]
    fn check_program_must_exist_on_every_platform() {
        let text = good_with("php = \"bin/php\"", "php-cli = \"bin/php\"");
        assert_eq!(error_code(&text), "recipe_invalid");
    }
}
