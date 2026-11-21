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

use super::upstream::is_segment;

/// The runtimes in manifest order.
pub const RUNTIMES: [Runtime; 7] = Runtime::ALL;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPins {
    serial: u64,
    spc_line: String,
    php: RawList,
    node: RawList,
    composer: RawList,
    caddy: RawList,
    postgres: RawList,
    /// Optional until the first MySQL is published.
    #[serde(default)]
    mysql: RawList,
    /// Optional until the first Valkey is published, as MySQL.
    #[serde(default)]
    valkey: RawList,
    #[serde(default, rename = "override")]
    overrides: Vec<RawOverride>,
    #[serde(default, rename = "skip")]
    skips: Vec<RawSkip>,
}

#[derive(Default, Deserialize)]
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

/// A version that has no build for a platform, so `build` leaves it out.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSkip {
    runtime: String,
    version: String,
    platform: String,
    /// Why, for the next reader of `pins.toml`. Required: a skipped platform
    /// is a promise that `marsa install` will not offer it.
    reason: String,
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
    /// The reason of each `[[skip]]`.
    skips: BTreeMap<(&'static str, Version, Platform), String>,
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
                Runtime::Postgres => &mut raw.postgres,
                Runtime::Mysql => &mut raw.mysql,
                Runtime::Valkey => &mut raw.valkey,
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
            for url in &o.urls {
                check_url(url).with_context(|| what.clone())?;
            }
            let key = (runtime.dir_name(), version, platform, o.artifact);
            ensure!(
                overrides.insert(key, o.urls).is_none(),
                "{what}: listed twice"
            );
        }
        let mut skips = BTreeMap::new();
        for skip in raw.skips {
            let what = format!("skip {} {} {}", skip.runtime, skip.version, skip.platform);
            let runtime = runtime_named(&skip.runtime).with_context(|| what.clone())?;
            let version = Version::parse(&skip.version)
                .with_context(|| format!("{what}: version is not semver"))?;
            ensure!(
                versions[runtime.dir_name()].contains(&version),
                "{what}: {version} is not in [{}].versions",
                runtime.dir_name()
            );
            let platform = Platform::from_str(&skip.platform).with_context(|| what.clone())?;
            ensure!(
                recipes::builtin(runtime)?.platform(platform).is_some(),
                "{what}: the {} recipe has no such platform",
                runtime.dir_name()
            );
            ensure!(!skip.reason.trim().is_empty(), "{what}: reason is empty");
            ensure!(
                !overrides.keys().any(|(r, v, p, _)| *r == runtime.dir_name()
                    && *v == version
                    && *p == platform),
                "{what}: it has an override too"
            );
            ensure!(
                skips
                    .insert((runtime.dir_name(), version, platform), skip.reason)
                    .is_none(),
                "{what}: listed twice"
            );
        }
        // A version with no platform left would be published with no file.
        for runtime in RUNTIMES {
            let recipe = recipes::builtin(runtime)?;
            for version in &versions[runtime.dir_name()] {
                ensure!(
                    recipe.platforms().any(|(platform, _)| {
                        !skips.contains_key(&(runtime.dir_name(), version.clone(), platform))
                    }),
                    "{} {version}: every platform is skipped",
                    runtime.dir_name()
                );
            }
        }
        Ok(Pins {
            serial: raw.serial,
            spc_line: raw.spc_line,
            versions,
            overrides,
            skips,
        })
    }

    /// Why `build` leaves this version out for `platform` (`[[skip]]` in
    /// `pins.toml`), or `None` when it builds it.
    pub fn skip_reason(
        &self,
        runtime: Runtime,
        version: &Version,
        platform: Platform,
    ) -> Option<&str> {
        self.skips
            .get(&(runtime.dir_name(), version.clone(), platform))
            .map(String::as_str)
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
        for url in &urls {
            check_url(url).with_context(|| format!("the {} recipe", runtime.dir_name()))?;
        }
        Ok(urls)
    }
}

/// Refuses a URL that the HTTP client may read another way than it is
/// written. It must be `https://`, a host of letters, digits, and `-` between
/// dots, then path parts that are each one [`is_segment`]. So there is no
/// `\` (read as `/`), `%` (`%2e%2e` is `..`), `?`, `#`, `@`, `:`, whitespace,
/// control character, or `.` or `..` part, any of which could point the URL
/// of a known source at another repository.
fn check_url(url: &str) -> Result<()> {
    let Some(rest) = url.strip_prefix("https://") else {
        bail!("{url:?} is not an https URL");
    };
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host_ok = host.split('.').all(|label| {
        !label.is_empty()
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    });
    ensure!(
        host_ok && path.split('/').all(is_segment),
        "{url:?} is not a plain https URL: a host, then path parts of [A-Za-z0-9._+-] \
         that are not `.` or `..`"
    );
    Ok(())
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

[postgres]
versions = ["18.7.0", "17.12.0"]

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
    fn postgres_is_required() {
        let pins = Pins::parse(FULL).expect("valid pins");
        assert_eq!(
            pins.versions(Runtime::Postgres),
            &[v("18.7.0"), v("17.12.0")]
        );
        let without = FULL.replace("[postgres]\nversions = [\"18.7.0\", \"17.12.0\"]\n", "");
        assert!(error(&without).contains("missing field `postgres`"));
        let bad = FULL.replace("17.12.0", "17.12");
        assert!(error(&bad).contains("is not semver"));
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

    /// `FULL` with MySQL 8.4.13, and the `[[skip]]` text given.
    fn with_mysql(skips: &str) -> String {
        format!("{FULL}[mysql]\nversions = [\"8.4.13\"]\n{skips}")
    }

    fn skip(version: &str, platform: &str, reason: &str) -> String {
        format!(
            "[[skip]]\nruntime = \"mysql\"\nversion = \"{version}\"\n\
             platform = \"{platform}\"\nreason = \"{reason}\"\n"
        )
    }

    #[test]
    fn a_skipped_platform_is_left_out_of_that_version_only() {
        let pins = Pins::parse(&with_mysql(&skip("8.4.13", "darwin-x64", "no Intel build")))
            .expect("valid pins");
        let version = v("8.4.13");
        assert_eq!(
            pins.skip_reason(Runtime::Mysql, &version, Platform::DarwinX64),
            Some("no Intel build")
        );
        let none = |runtime, version: &Version, platform| {
            pins.skip_reason(runtime, version, platform).is_none()
        };
        assert!(none(Runtime::Mysql, &version, Platform::DarwinArm64));
        assert!(none(Runtime::Mysql, &v("8.4.11"), Platform::DarwinX64));
        assert!(none(Runtime::Postgres, &v("18.7.0"), Platform::DarwinX64));
        // No skip at all is the default.
        let pins = Pins::parse(&with_mysql("")).expect("valid pins");
        assert!(
            pins.skip_reason(Runtime::Mysql, &version, Platform::DarwinX64)
                .is_none()
        );
    }

    #[test]
    fn a_bad_skip_is_refused() {
        let every_platform = [
            "darwin-arm64",
            "darwin-x64",
            "linux-x64",
            "linux-arm64",
            "windows-x64",
        ]
        .iter()
        .map(|platform| skip("8.4.13", platform, "x"))
        .collect::<String>();
        let override_too = format!(
            "{}[[override]]\nruntime = \"mysql\"\nversion = \"8.4.13\"\nplatform = \"darwin-x64\"\n\
             artifact = \"archive\"\nurls = [\"https://cdn.mysql.com/archives/mysql-8.4/x.tar.gz\"]\n",
            skip("8.4.13", "darwin-x64", "x")
        );
        let cases = [
            (
                skip("8.4.9", "darwin-x64", "x"),
                "8.4.9 is not in [mysql].versions",
            ),
            (skip("8.4.13", "darwin-arm32", "x"), "unknown platform"),
            (skip("8.4.13", "darwin-x64", "  "), "reason is empty"),
            (
                format!(
                    "{}{}",
                    skip("8.4.13", "darwin-x64", "x"),
                    skip("8.4.13", "darwin-x64", "y")
                ),
                "listed twice",
            ),
            (every_platform, "every platform is skipped"),
            (override_too, "it has an override too"),
        ];
        for (skips, expected) in cases {
            let text = error(&with_mysql(&skips));
            assert!(text.contains(expected), "{expected}: {text}");
        }
        // A skip needs its reason field at all.
        let no_reason =
            "[[skip]]\nruntime = \"mysql\"\nversion = \"8.4.13\"\nplatform = \"darwin-x64\"\n";
        assert!(error(&with_mysql(no_reason)).contains("missing field `reason`"));
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

    /// PostgreSQL is the theseus-rs release named by the recipe, with no
    /// override, for all five platforms, and every file has a `.sha256`
    /// beside it: without one, `build` stops (ADR 0023, section 3).
    #[test]
    fn repository_postgres_urls_are_theseus_releases() {
        use super::super::upstream::{Layout, Source, sum_file_for};

        let pins = Pins::parse(include_str!("../../../recipes/pins.toml")).expect("valid pins");
        assert!(!pins.versions(Runtime::Postgres).is_empty());
        let recipe = recipes::builtin(Runtime::Postgres).expect("postgres recipe");
        for version in pins.versions(Runtime::Postgres) {
            for platform in Platform::ALL.into_iter().filter(|p| *p != Platform::Any) {
                let template = recipe
                    .artifact_url(platform, "archive", version)
                    .unwrap_or_else(|| panic!("{version} has no {platform} archive"));
                let urls = pins
                    .urls(Runtime::Postgres, version, platform, "archive")
                    .expect("urls");
                assert_eq!(
                    urls,
                    std::slice::from_ref(&template),
                    "no override for postgres"
                );
                let sums = sum_file_for(&template).expect("theseus publishes sums");
                assert_eq!(
                    (sums.source, sums.layout, sums.url),
                    (
                        Source::Theseus,
                        Layout::Single,
                        format!("{template}.sha256")
                    )
                );
            }
        }
    }

    /// Valkey is the archive of our release that the recipe names, with no
    /// override, and the `sha256sums.txt` of that release beside it, on all
    /// five platforms: a `.zip` for Windows.
    #[test]
    fn repository_valkey_urls_are_our_release_assets() {
        use super::super::upstream::{Layout, Source, VALKEY_RELEASES, sum_file_for};

        let pins = Pins::parse(include_str!("../../../recipes/pins.toml")).expect("valid pins");
        assert_eq!(pins.versions(Runtime::Valkey), &[v("8.1.11")]);
        let recipe = recipes::builtin(Runtime::Valkey).expect("valkey recipe");
        for version in pins.versions(Runtime::Valkey) {
            assert_eq!(recipe.platforms().count(), 5);
            for (platform, _) in recipe.platforms() {
                assert_eq!(pins.skip_reason(Runtime::Valkey, version, platform), None);
                let template = recipe
                    .artifact_url(platform, "archive", version)
                    .unwrap_or_else(|| panic!("{version} has no {platform} archive"));
                let urls = pins
                    .urls(Runtime::Valkey, version, platform, "archive")
                    .expect("urls");
                assert_eq!(urls, std::slice::from_ref(&template), "no override");
                let format = if platform == Platform::WindowsX64 {
                    "zip"
                } else {
                    "tar.gz"
                };
                let want =
                    format!("{VALKEY_RELEASES}{version}-r1/valkey-{version}-{platform}.{format}");
                assert_eq!(template, want);
                let sums = sum_file_for(&template).expect("our release has sums");
                assert_eq!(
                    (sums.source, sums.layout, sums.url),
                    (
                        Source::OurValkey,
                        Layout::List,
                        format!("{VALKEY_RELEASES}{version}-r1/sha256sums.txt")
                    )
                );
            }
        }
    }

    /// An override URL of any source that the HTTP client would resolve to
    /// another place is refused: `\\` is read as `/`, `%2e%2e` and `..` go up.
    #[test]
    fn override_urls_must_be_plain() {
        use super::super::upstream::{CADDY_RELEASES, PHP_RELEASES, THESEUS_RELEASES};

        let evil = r"x-\..\..\..\..\..\..\evil\repo\releases\download\18.7.0\";
        for (runtime, version, platform, artifact, dir, name) in [
            (
                "php",
                "8.4.23",
                "darwin-arm64",
                "php",
                format!("{PHP_RELEASES}8.4.23-r1"),
                "php-8.4.23-darwin-arm64.tar.gz",
            ),
            (
                "caddy",
                "2.11.8",
                "windows-x64",
                "archive",
                format!("{CADDY_RELEASES}2.11.8"),
                "caddy_2.11.8_windows_amd64.zip",
            ),
            (
                "postgres",
                "18.7.0",
                "linux-x64",
                "archive",
                format!("{THESEUS_RELEASES}18.7.0"),
                "postgresql-18.7.0-x86_64-unknown-linux-gnu.tar.gz",
            ),
        ] {
            // A TOML literal string, so `\\` stays as it is.
            let pins = |url: &str| {
                format!(
                    "{FULL}[[override]]\nruntime = \"{runtime}\"\nversion = \"{version}\"\n\
                     platform = \"{platform}\"\nartifact = \"{artifact}\"\nurls = ['{url}']\n"
                )
            };
            Pins::parse(&pins(&format!("{dir}/{name}"))).expect("a plain URL");
            for url in [
                format!("{dir}/{evil}{name}"),
                format!("{dir}/%2e%2e/%2e%2e/evil/{name}"),
                format!("{dir}/{name}?x"),
                format!("{dir}/{name}#x"),
                format!("{dir}/../../evil/{name}"),
                format!("{dir}/./{name}"),
                format!("{dir}/{name} "),
                format!("{dir}/{name}\t"),
            ] {
                let message = error(&pins(&url));
                assert!(
                    message.contains("is not a plain https URL"),
                    "{url}: {message}"
                );
            }
        }
    }

    #[test]
    fn plain_urls_only() {
        check_url("https://nodejs.org/dist/v22.23.4/node-v22.23.4-linux-x64.tar.gz")
            .expect("plain");
        for url in [
            // The URL from the review of #175: reqwest goes to github.com/evil/repo.
            r"https://github.com/theseus-rs/postgresql-binaries/releases/download/18.7.0/postgresql-18.7.0-\..\..\..\..\..\..\evil\repo\releases\download\18.7.0\postgresql-18.7.0-x86_64-pc-windows-msvc.zip",
            "https://github.com/x/%2e%2e/y.zip",
            "https://github.com/x/y.zip?a",
            "https://github.com/x/y.zip#a",
            "https://github.com/x/../y.zip",
            "https://github.com/./y.zip",
            "https://github.com/x/y .zip",
            "https://github.com/x/y\u{0}.zip",
            "https://github.com/x/y\n.zip",
            "https://github.com@evil.example/y.zip",
            "https://github.com:443/y.zip",
            "https://github..com/y.zip",
            "https:///y.zip",
            "https://github.com",
            "https://github.com/",
            "https://github.com/x//y.zip",
        ] {
            let err = check_url(url).expect_err(url);
            assert!(
                err.to_string().contains("is not a plain https URL"),
                "{err}"
            );
        }
        let err = check_url("http://nodejs.org/dist/x.tar.gz").expect_err("http");
        assert!(err.to_string().contains("is not an https URL"), "{err}");
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
