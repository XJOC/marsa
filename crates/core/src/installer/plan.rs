//! The install plan: picks the version from the manifest and the recipe for
//! this platform, and lists the files to download and how each is unpacked.
//! Pure: no network, no disk.

use std::collections::BTreeSet;

use semver::Version;

use super::InstallError;
use super::manifest::{ArtifactSource, Manifest, Release};
use crate::recipes::{Artifact, Platform, PlatformRecipe};
use crate::resolver;
use crate::runtimes::Runtime;

/// What to download and how to unpack it, for one version on one platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan<'a> {
    pub runtime: Runtime,
    pub version: Version,
    /// In recipe order.
    pub steps: Vec<ArtifactStep<'a>>,
}

/// One file: where the signed manifest says it is, and how the recipe says
/// to unpack it. Both have the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactStep<'a> {
    pub artifact: &'a Artifact,
    pub source: &'a ArtifactSource,
}

/// The release of `runtime` that `requested` asks for.
///
/// `None` gives the highest version without a pre-release tag. Otherwise the
/// text is matched with [`resolver::best_match`], so `8.4` is the newest
/// `8.4.x` here too, never `8.5.0` or `8.4.24-rc1`.
///
/// # Errors
///
/// `install_no_such_version` if nothing matches, or `unsupported_alias` and
/// `invalid_version` from the resolver.
pub fn select<'m>(
    manifest: &'m Manifest,
    runtime: Runtime,
    requested: Option<&str>,
) -> Result<&'m Release, InstallError> {
    let releases = manifest.releases(runtime);
    let picked = match requested {
        None => releases
            .iter()
            .map(Release::version)
            .filter(|v| v.pre.is_empty())
            .max()
            .cloned(),
        Some(text) => {
            let versions: Vec<Version> = releases.iter().map(|r| r.version().clone()).collect();
            resolver::best_match(text, &versions)?
        }
    };
    picked
        .and_then(|version| manifest.release(runtime, &version))
        .ok_or_else(|| {
            let mut available: Vec<&Version> = releases.iter().map(Release::version).collect();
            available.sort();
            InstallError::NoSuchVersion {
                runtime: runtime.dir_name(),
                requested: requested.map(str::to_owned),
                available: available.iter().map(ToString::to_string).collect(),
            }
        })
}

/// Pairs each artifact of the recipe with the manifest's file of the same
/// name. The manifest's files for `platform` are used, or its `any` files
/// (Composer).
///
/// # Errors
///
/// - `install_unsupported_platform` if the release has no files for
///   `platform`;
/// - `install_unsafe_name` if a name that becomes a path is empty, starts
///   with `.`, or has a character outside `[a-z0-9._-]`;
/// - `install_manifest_mismatch` if a name is in only one of the two lists.
pub fn build<'a>(
    runtime: Runtime,
    release: &'a Release,
    recipe: &'a PlatformRecipe,
    platform: Platform,
) -> Result<InstallPlan<'a>, InstallError> {
    let what = format!("{} {}", runtime.dir_name(), release.version());
    let sources = release
        .platform(platform)
        .or_else(|| release.platform(Platform::Any))
        .ok_or_else(|| InstallError::UnsupportedPlatform {
            what: what.clone(),
            platform: platform.to_string(),
        })?;

    let mut steps = Vec::new();
    let mut missing = Vec::new();
    for artifact in recipe.artifacts() {
        check_name(&artifact.name)?;
        if let Some(file_name) = &artifact.file_name {
            check_name(file_name)?;
        }
        match sources.iter().find(|s| s.name() == artifact.name) {
            Some(source) => steps.push(ArtifactStep { artifact, source }),
            None => missing.push(artifact.name.clone()),
        }
    }
    let in_recipe: BTreeSet<&str> = recipe.artifacts().iter().map(|a| a.name.as_str()).collect();
    let extra: Vec<String> = sources
        .iter()
        .map(ArtifactSource::name)
        .filter(|name| !in_recipe.contains(name))
        .map(str::to_owned)
        .collect();
    if !missing.is_empty() || !extra.is_empty() {
        return Err(InstallError::ManifestMismatch {
            what,
            missing,
            extra,
        });
    }
    Ok(InstallPlan {
        runtime,
        version: release.version().clone(),
        steps,
    })
}

/// Artifact names and `raw` file names become file names on disk
/// (`dl/<name>`, `<version>/<file_name>`). A leading `.` is refused, so
/// none of them can be `.`, `..`, or a hidden file.
fn check_name(name: &str) -> Result<(), InstallError> {
    let safe = !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'));
    if safe {
        Ok(())
    } else {
        Err(InstallError::UnsafeName {
            name: name.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipes::{self, Recipe};

    const SHA: &str = "2e17f6527a6c92f5713ba16e5dfd1b55869ab5e98de46677e994fed5536316ba";

    /// One file called `name`.
    fn file(name: &str) -> String {
        format!(
            r#"{{"name": "{name}", "urls": ["https://example.test/{name}"], "sha256": "{SHA}", "size": 1}}"#
        )
    }

    /// A manifest with these PHP versions, each with `files` for `linux-x64`,
    /// and Composer 2.8.12 for `any`.
    fn manifest(versions: &[&str], files: &[&str]) -> Manifest {
        let files: Vec<String> = files.iter().map(|name| file(name)).collect();
        let releases: Vec<String> = versions
            .iter()
            .map(|v| {
                format!(
                    r#"{{"version": "{v}", "platforms": {{"linux-x64": [{}]}}}}"#,
                    files.join(", ")
                )
            })
            .collect();
        let json = format!(
            r#"{{"schema": 1, "serial": 1, "runtimes": {{"php": [{}],
                "composer": [{{"version": "2.8.12", "platforms": {{"any": [{}]}}}}]}}}}"#,
            releases.join(", "),
            file("phar")
        );
        Manifest::unsigned_for_tests(json.as_bytes(), None).expect("test manifest is valid")
    }

    /// A PHP recipe for `linux-x64` with these artifacts.
    fn recipe(artifacts: &[&str]) -> Recipe {
        let mut text = String::from(
            "name = \"php\"\n[platform.linux-x64]\nprograms = { php = \"bin/php\" }\n",
        );
        for name in artifacts {
            text.push_str(&format!(
                "[[platform.linux-x64.artifact]]\nname = \"{name}\"\n\
                 url = \"https://example.test/x\"\nformat = \"tar.gz\"\n"
            ));
        }
        recipes::parse("test.toml", &text).expect("test recipe is valid")
    }

    fn version(text: &str) -> Version {
        Version::parse(text).expect("test versions are valid")
    }

    fn pick(manifest: &Manifest, requested: Option<&str>) -> Result<Version, InstallError> {
        select(manifest, Runtime::Php, requested).map(|r| r.version().clone())
    }

    fn plan_error(manifest: &Manifest, recipe: &Recipe, platform: Platform) -> InstallError {
        let release = select(manifest, Runtime::Php, None).expect("select");
        let linux = recipe.platform(Platform::LinuxX64).expect("linux");
        build(Runtime::Php, release, linux, platform).expect_err("the plan must be refused")
    }

    #[test]
    fn select_uses_the_resolver_rules() {
        let m = manifest(&["8.3.14", "8.4.23", "8.4.24-rc1", "8.5.0"], &["cli"]);
        assert_eq!(pick(&m, Some("8.4")).expect("8.4"), version("8.4.23"));
        assert_eq!(pick(&m, Some("8.3.14")).expect("full"), version("8.3.14"));
        assert_eq!(pick(&m, Some("8")).expect("8"), version("8.5.0"));
        assert_eq!(
            pick(&m, Some("8.4.24-rc1")).expect("pre-release written in full"),
            version("8.4.24-rc1")
        );
        assert_eq!(pick(&m, None).expect("latest"), version("8.5.0"));
    }

    #[test]
    fn select_without_a_version_skips_pre_releases() {
        let m = manifest(&["8.4.23", "8.5.0-rc1"], &["cli"]);
        assert_eq!(pick(&m, None).expect("latest stable"), version("8.4.23"));
        let only_rc = manifest(&["8.5.0-rc1"], &["cli"]);
        let err = pick(&only_rc, None).expect_err("no stable version");
        assert_eq!(err.code(), "install_no_such_version");
    }

    #[test]
    fn select_refuses_unknown_versions_and_aliases() {
        let m = manifest(&["8.4.23"], &["cli"]);
        for (text, code) in [
            ("8.2", "install_no_such_version"),
            ("8.4.22", "install_no_such_version"),
            ("latest", "unsupported_alias"),
            ("lts/*", "unsupported_alias"),
            ("8.x", "invalid_version"),
        ] {
            let err = pick(&m, Some(text)).expect_err(text);
            assert_eq!(err.code(), code, "{text}");
        }
        let err = select(&m, Runtime::Node, None).expect_err("no node in the manifest");
        assert_eq!(err.code(), "install_no_such_version");

        let m = manifest(&["8.4.23", "8.3.27"], &["cli"]);
        let err = pick(&m, Some("8.2")).expect_err("not listed");
        let InstallError::NoSuchVersion {
            requested,
            available,
            ..
        } = err
        else {
            panic!("expected install_no_such_version");
        };
        assert_eq!(requested.as_deref(), Some("8.2"));
        assert_eq!(available, ["8.3.27", "8.4.23"]);
    }

    #[test]
    fn build_pairs_recipe_and_manifest_by_name() {
        let m = manifest(&["8.4.23"], &["fpm", "cli"]);
        let r = recipe(&["cli", "fpm"]);
        let release = select(&m, Runtime::Php, None).expect("select");
        let linux = r.platform(Platform::LinuxX64).expect("linux");
        let plan = build(Runtime::Php, release, linux, Platform::LinuxX64).expect("build");
        assert_eq!(plan.version, version("8.4.23"));
        let names: Vec<(&str, &str)> = plan
            .steps
            .iter()
            .map(|s| (s.artifact.name.as_str(), s.source.name()))
            .collect();
        assert_eq!(names, [("cli", "cli"), ("fpm", "fpm")]);
    }

    #[test]
    fn build_refuses_a_platform_without_files() {
        let m = manifest(&["8.4.23"], &["cli"]);
        let err = plan_error(&m, &recipe(&["cli"]), Platform::DarwinArm64);
        assert_eq!(err.code(), "install_unsupported_platform");
        assert!(err.to_string().contains("darwin-arm64"), "{err}");
    }

    #[test]
    fn build_uses_the_any_files_on_every_platform() {
        let m = manifest(&["8.4.23"], &["cli"]);
        let composer = recipes::builtin(Runtime::Composer).expect("built-in recipe");
        let release = select(&m, Runtime::Composer, Some("2.8")).expect("select");
        for platform in [Platform::LinuxX64, Platform::WindowsX64] {
            let entry = composer.platform(platform).expect("any");
            let plan = build(Runtime::Composer, release, entry, platform).expect("build");
            assert_eq!(plan.steps[0].source.name(), "phar");
        }
    }

    #[test]
    fn build_refuses_a_name_in_only_one_list() {
        let m = manifest(&["8.4.23"], &["cli", "fpm"]);
        for (artifacts, missing, extra) in [
            (&["cli", "fpm", "dbg"][..], &["dbg"][..], &[][..]),
            (&["cli"][..], &[][..], &["fpm"][..]),
        ] {
            let err = plan_error(&m, &recipe(artifacts), Platform::LinuxX64);
            assert_eq!(err.code(), "install_manifest_mismatch");
            let InstallError::ManifestMismatch {
                missing: got_missing,
                extra: got_extra,
                ..
            } = err
            else {
                panic!("expected a mismatch");
            };
            assert_eq!(got_missing, missing);
            assert_eq!(got_extra, extra);
        }
    }

    #[test]
    fn names_that_start_with_a_dot_are_refused() {
        // The manifest allows `.x` names, so the plan must refuse them.
        let m = manifest(&["8.4.23"], &[".hidden"]);
        let err = plan_error(&m, &recipe(&[".hidden"]), Platform::LinuxX64);
        assert_eq!(err.code(), "install_unsafe_name");

        for bad in ["", ".", "..", ".env", "a/b", "A", "a b"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        for good in ["cli", "composer.phar", "php-8.4_x"] {
            assert!(check_name(good).is_ok(), "{good:?}");
        }
    }
}
