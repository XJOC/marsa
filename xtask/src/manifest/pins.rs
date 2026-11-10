//! `recipes/pins.toml`: the versions the next manifest publishes, its serial,
//! the pinned static-php-cli line, and URL overrides. Pure parsing.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use marsa_core::installer::manifest::MIN_SERIAL;
use marsa_core::recipes::{self, Platform};
use marsa_core::runtimes::Runtime;
use semver::Version;
use serde::Deserialize;

/// The runtimes in manifest order.
pub const RUNTIMES: [Runtime; 4] = Runtime::ALL;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPins {
    serial: u64,
    spc_line: String,
    php: RawList,
    node: RawList,
    composer: RawList,
    caddy: RawList,
    #[serde(default, rename = "override")]
    overrides: Vec<RawOverride>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawList {
    versions: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOverride {
    runtime: String,
    version: String,
    platform: String,
    artifact: String,
    urls: Vec<String>,
}

/// Which file an override replaces: runtime folder name, version, platform,
/// artifact name.
type OverrideKey = (&'static str, Version, Platform, String);

/// A checked `pins.toml`.
#[derive(Debug)]
pub struct Pins {
    pub serial: u64,
    /// For example `2.8`: `php -v` must say `Built by static-php-cli 2.8.x`.
    pub spc_line: String,
    versions: BTreeMap<&'static str, Vec<Version>>,
    overrides: BTreeMap<OverrideKey, Vec<String>>,
}

impl Pins {
    pub fn parse(text: &str) -> Result<Pins> {
        let mut raw: RawPins = toml::from_str(text).context("pins.toml is not valid")?;
        ensure!(
            raw.serial >= MIN_SERIAL,
            "serial must be {MIN_SERIAL} or more"
        );
        ensure!(
            is_spc_line(&raw.spc_line),
            "spc_line {:?} must look like 2.8",
            raw.spc_line
        );
        let mut versions = BTreeMap::new();
        for runtime in RUNTIMES {
            // An exhaustive `match`, not a `zip` by position: a new `Runtime`
            // fails to compile here until it has a list, instead of being
            // dropped silently or paired with another runtime's list.
            let list = match runtime {
                Runtime::Php => &mut raw.php,
                Runtime::Node => &mut raw.node,
                Runtime::Composer => &mut raw.composer,
                Runtime::Caddy => &mut raw.caddy,
            };
            let list = std::mem::take(&mut list.versions);
            versions.insert(runtime.dir_name(), versions_of(runtime, list)?);
        }
        let mut overrides = BTreeMap::new();
        for o in raw.overrides {
            let what = format!(
                "override {} {} {} {}",
                o.runtime, o.version, o.platform, o.artifact
            );
            let runtime = runtime_named(&o.runtime).with_context(|| what.clone())?;
            let version = Version::parse(&o.version)
                .with_context(|| format!("{what}: version is not semver"))?;
            ensure!(
                versions[runtime.dir_name()].contains(&version),
                "{what}: {version} is not in [{}].versions",
                runtime.dir_name()
            );
            let platform = Platform::from_str(&o.platform).with_context(|| what.clone())?;
            let recipe = recipes::builtin(runtime)?;
            ensure!(
                recipe
                    .platform(platform)
                    .and_then(|p| p.artifact(&o.artifact))
                    .is_some(),
                "{what}: the {} recipe has no such platform and artifact",
                runtime.dir_name()
            );
            ensure!(!o.urls.is_empty(), "{what}: urls is empty");
            if let Some(url) = o.urls.iter().find(|u| !u.starts_with("https://")) {
                bail!("{what}: {url:?} is not an https URL");
            }
            let key = (runtime.dir_name(), version, platform, o.artifact);
            ensure!(
                overrides.insert(key, o.urls).is_none(),
                "{what}: listed twice"
            );
        }
        Ok(Pins {
            serial: raw.serial,
            spc_line: raw.spc_line,
            versions,
            overrides,
        })
    }

    /// The pinned versions of `runtime`, in file order.
    pub fn versions(&self, runtime: Runtime) -> &[Version] {
        self.versions
            .get(runtime.dir_name())
            .map_or(&[], Vec::as_slice)
    }

    /// The URLs of one file: the override if there is one, else the recipe
    /// template. A Windows PHP release moves from `releases/` to
    /// `releases/archives/` when a newer one comes out, so the template gets
    /// the archive folder as a mirror.
    pub fn urls(
        &self,
        runtime: Runtime,
        version: &Version,
        platform: Platform,
        artifact: &str,
    ) -> Result<Vec<String>> {
        let key = (
            runtime.dir_name(),
            version.clone(),
            platform,
            artifact.to_owned(),
        );
        if let Some(urls) = self.overrides.get(&key) {
            return Ok(urls.clone());
        }
        let url = recipes::builtin(runtime)?
            .artifact_url(platform, artifact, version)
            .with_context(|| format!("no recipe URL for {runtime:?} {platform} {artifact}"))?;
        let mut urls = vec![url.clone()];
        if let Some(rest) = url.strip_prefix(WINDOWS_PHP_RELEASES) {
            urls.push(format!("{WINDOWS_PHP_RELEASES}archives/{rest}"));
        }
        Ok(urls)
    }
}

const WINDOWS_PHP_RELEASES: &str = "https://windows.php.net/downloads/releases/";

fn versions_of(runtime: Runtime, raw: Vec<String>) -> Result<Vec<Version>> {
    let mut seen = BTreeSet::new();
    raw.into_iter()
        .map(|text| {
            let version = Version::parse(&text).with_context(|| {
                format!("{} version {text:?} is not semver", runtime.dir_name())
            })?;
            ensure!(
                seen.insert(version.clone()),
                "{} {version} is listed twice",
                runtime.dir_name()
            );
            Ok(version)
        })
        .collect()
}

fn runtime_named(name: &str) -> Result<Runtime> {
    RUNTIMES
        .into_iter()
        .find(|r| r.dir_name() == name)
        .with_context(|| format!("unknown runtime {name:?}"))
}

/// `2.8`: two numbers, as in ADR 0010.
fn is_spc_line(line: &str) -> bool {
    let parts: Vec<&str> = line.split('.').collect();
    parts.len() == 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
serial = 4
spc_line = "2.8"

[php]
versions = ["8.4.23", "8.3.27"]

[node]
versions = ["22.23.4"]

[composer]
versions = ["2.11.1"]

[caddy]
versions = ["2.11.8"]

[[override]]
runtime = "php"
version = "8.3.27"
platform = "windows-x64"
artifact = "zip"
urls = ["https://windows.php.net/downloads/releases/archives/php-8.3.27-nts-Win32-vs16-x64.zip"]
"#;

    fn v(text: &str) -> Version {
        Version::parse(text).expect("test version")
    }

    #[test]
    fn full_file() {
        let pins = Pins::parse(FULL).expect("valid pins");
        assert_eq!(pins.serial, 4);
        assert_eq!(pins.spc_line, "2.8");
        assert_eq!(pins.versions(Runtime::Php), &[v("8.4.23"), v("8.3.27")]);
        assert_eq!(pins.versions(Runtime::Node), &[v("22.23.4")]);
        assert_eq!(pins.versions(Runtime::Composer), &[v("2.11.1")]);
        assert_eq!(pins.versions(Runtime::Caddy), &[v("2.11.8")]);
    }

    #[test]
    fn override_wins_and_template_gets_archive_mirror() {
        let pins = Pins::parse(FULL).expect("valid pins");
        let urls = pins
            .urls(Runtime::Php, &v("8.3.27"), Platform::WindowsX64, "zip")
            .expect("urls");
        assert_eq!(
            urls,
            [
                "https://windows.php.net/downloads/releases/archives/php-8.3.27-nts-Win32-vs16-x64.zip"
            ]
        );
        let urls = pins
            .urls(Runtime::Php, &v("8.4.23"), Platform::WindowsX64, "zip")
            .expect("urls");
        assert_eq!(
            urls,
            [
                "https://windows.php.net/downloads/releases/php-8.4.23-nts-Win32-vs17-x64.zip",
                "https://windows.php.net/downloads/releases/archives/php-8.4.23-nts-Win32-vs17-x64.zip",
            ]
        );
        let urls = pins
            .urls(Runtime::Node, &v("22.23.4"), Platform::LinuxX64, "archive")
            .expect("urls");
        assert_eq!(
            urls,
            ["https://nodejs.org/dist/v22.23.4/node-v22.23.4-linux-x64.tar.gz"]
        );
        let urls = pins
            .urls(
                Runtime::Caddy,
                &v("2.11.8"),
                Platform::WindowsX64,
                "archive",
            )
            .expect("urls");
        assert_eq!(
            urls,
            [
                "https://github.com/caddyserver/caddy/releases/download/v2.11.8/caddy_2.11.8_windows_amd64.zip"
            ]
        );
    }

    fn error(text: &str) -> String {
        format!("{:#}", Pins::parse(text).expect_err("must be refused"))
    }

    #[test]
    fn refused() {
        let cases = [
            (FULL.replace("\"22.23.4\"", "\"22.23\""), "is not semver"),
            (
                FULL.replace("version = \"8.3.27\"", "version = \"8.3.1\""),
                "8.3.1 is not in [php].versions",
            ),
            (
                FULL.replace("\"windows-x64\"", "\"windows-arm64\""),
                "unknown platform",
            ),
            (
                FULL.replace("artifact = \"zip\"", "artifact = \"cli\""),
                "no such platform and artifact",
            ),
            (
                FULL.replace("urls = [\"https:", "urls = [\"http:"),
                "is not an https URL",
            ),
            (FULL.replace("serial = 4", "serial = 0"), "serial must be"),
            (FULL.replace("\"2.8\"", "\"2.8.6\""), "must look like 2.8"),
            (
                FULL.replace("[\"2.11.1\"]", "[\"2.11.1\", \"2.11.1\"]"),
                "listed twice",
            ),
            (FULL.replace("[node]", "[nodejs]"), "not valid"),
            (FULL.replace("[caddy]", "[web]"), "not valid"),
        ];
        for (text, expected) in cases {
            let message = error(&text);
            assert!(
                message.contains(expected),
                "{expected:?} not in {message:?}"
            );
        }
    }

    #[test]
    fn repository_pins_file_is_valid() {
        let text = include_str!("../../../recipes/pins.toml");
        Pins::parse(text).expect("recipes/pins.toml is valid");
    }

    /// Our PHP builds are the release assets `php-build.yml` makes:
    /// `php-<v>-r<n>/php-<v>-<platform>.tar.gz`, with a sha256sums.txt
    /// beside them that `build` checks.
    #[test]
    fn repository_php_urls_are_our_release_assets() {
        let pins = Pins::parse(include_str!("../../../recipes/pins.toml")).expect("valid pins");
        assert!(!pins.versions(Runtime::Php).is_empty());
        for version in pins.versions(Runtime::Php) {
            for platform in [
                Platform::DarwinArm64,
                Platform::DarwinX64,
                Platform::LinuxX64,
                Platform::LinuxArm64,
            ] {
                let urls = pins
                    .urls(Runtime::Php, version, platform, "php")
                    .expect("urls");
                let [url] = urls.as_slice() else {
                    panic!("one URL expected for {platform}: {urls:?}");
                };
                let rest = url
                    .strip_prefix(super::super::upstream::PHP_RELEASES)
                    .unwrap_or_else(|| panic!("{url} is not one of our releases"));
                let (tag, name) = rest.split_once('/').expect("tag/name");
                let revision = tag
                    .strip_prefix(&format!("{version}-r"))
                    .unwrap_or_else(|| panic!("tag php-{tag} is not php-{version}-r<n>"));
                assert!(
                    revision.parse::<u32>().is_ok_and(|n| n > 0),
                    "revision {revision:?}"
                );
                assert_eq!(name, format!("php-{version}-{platform}.tar.gz"));
                assert!(super::super::upstream::sum_file_for(url).is_some());
            }
        }
    }

    /// Caddy is the official release, and every file has an upstream sha512.
    #[test]
    fn repository_caddy_urls_are_official_releases() {
        use super::super::upstream::{Algorithm, CADDY_RELEASES, sum_file_for};

        let pins = Pins::parse(include_str!("../../../recipes/pins.toml")).expect("valid pins");
        assert!(!pins.versions(Runtime::Caddy).is_empty());
        let recipe = recipes::builtin(Runtime::Caddy).expect("caddy recipe");
        for version in pins.versions(Runtime::Caddy) {
            for (platform, _) in recipe.platforms() {
                let urls = pins
                    .urls(Runtime::Caddy, version, platform, "archive")
                    .expect("urls");
                let [url] = urls.as_slice() else {
                    panic!("one URL expected for {platform}: {urls:?}");
                };
                assert!(
                    url.starts_with(&format!("{CADDY_RELEASES}{version}/caddy_{version}_")),
                    "{url}"
                );
                let sums = sum_file_for(url).expect("caddy publishes sums");
                assert_eq!(sums.algorithm, Algorithm::Sha512);
            }
        }
    }
}
