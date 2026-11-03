//! What to run: the real program, an optional script, and the user's
//! arguments. Built without touching the disk, so every case is tested
//! without `exec`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use marsa_core::runtimes::{Runtime, RuntimesDir};
use semver::Version;

use crate::depth;
use crate::error::ShimError;
use crate::name::ShimName;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Always absolute, so starting it never searches `PATH`.
    pub program: PathBuf,
    /// Passed to `program` before the user's arguments: `npm-cli.js`,
    /// `npx-cli.js`, or `composer.phar`.
    pub script: Option<PathBuf>,
    pub args: Vec<OsString>,
}

impl Plan {
    /// `version` is the PHP or Node version. `composer` is the highest
    /// installed Composer version; only `composer` uses it.
    ///
    /// Both must come from [`RuntimesDir::installed`], never from user text, so
    /// a value like `../../x` cannot reach the path.
    ///
    /// `npm` and `npx` run through `node`, not through `bin/npm`: that file
    /// starts with `#!/usr/bin/env node`, which searches `PATH` and may find the
    /// shim again or another `node`.
    ///
    /// # Errors
    ///
    /// `shim_composer_not_installed` for `composer` when `composer` is `None`.
    pub fn new(
        name: ShimName,
        runtimes: &RuntimesDir,
        version: &Version,
        composer: Option<&Version>,
        user_args: impl IntoIterator<Item = OsString>,
    ) -> Result<Self, ShimError> {
        let runtime = name.runtime();
        let version_dir = runtimes.version_dir(runtime, version);
        let npm_bin = || {
            version_dir
                .join("lib")
                .join("node_modules")
                .join("npm")
                .join("bin")
        };
        let script = match name {
            ShimName::Php | ShimName::Node => None,
            ShimName::Npm => Some(npm_bin().join("npm-cli.js")),
            ShimName::Npx => Some(npm_bin().join("npx-cli.js")),
            ShimName::Composer => {
                let composer = composer.ok_or_else(|| ShimError::ComposerNotInstalled {
                    dir: runtimes.runtime_dir(Runtime::Composer),
                })?;
                let dir = runtimes.version_dir(Runtime::Composer, composer);
                Some(dir.join("composer.phar"))
            }
        };
        Ok(Self {
            program: version_dir.join("bin").join(runtime.dir_name()),
            script,
            args: user_args.into_iter().collect(),
        })
    }

    /// # Errors
    ///
    /// `shim_program_missing` if the program or the script is not a file. The
    /// version folder exists, so this is a broken install, not a missing
    /// version.
    pub fn check(&self) -> Result<(), ShimError> {
        let missing = std::iter::once(&self.program)
            .chain(&self.script)
            .find(|path| !path.is_file());
        match missing {
            Some(path) => Err(ShimError::ProgramMissing { path: path.clone() }),
            None => Ok(()),
        }
    }

    /// The command to run. The environment is inherited; only the depth
    /// counter goes up by one. `argv[0]` is the absolute path, so `PHP_BINARY`
    /// and `process.execPath` point at the real program, and `@php` in
    /// Composer scripts uses the same PHP.
    pub fn command(&self, depth: u32) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.script)
            .args(&self.args)
            .env(depth::VAR, depth.saturating_add(1).to_string());
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::path::Path;

    fn v(text: &str) -> Version {
        Version::parse(text).expect("valid test version")
    }

    fn plan(name: ShimName, version: &str, composer: Option<&str>, args: &[&str]) -> Plan {
        let runtimes = RuntimesDir::in_dir(Path::new("/data"));
        let composer = composer.map(v);
        let args = args.iter().map(OsString::from);
        Plan::new(name, &runtimes, &v(version), composer.as_ref(), args).expect("valid plan")
    }

    #[test]
    fn php_runs_its_own_binary() {
        let plan = plan(ShimName::Php, "8.4.3", None, &["-v", "a b"]);
        let expected = Path::new("/data/runtimes/php/8.4.3/bin/php");
        assert_eq!(plan.program, expected);
        assert_eq!(plan.script, None);
        assert_eq!(plan.args, ["-v", "a b"]);
    }

    #[test]
    fn node_runs_its_own_binary() {
        let plan = plan(ShimName::Node, "22.11.0", None, &[]);
        let expected = Path::new("/data/runtimes/node/22.11.0/bin/node");
        assert_eq!(plan.program, expected);
        assert_eq!(plan.script, None);
        assert!(plan.args.is_empty());
    }

    #[test]
    fn npm_and_npx_run_through_node() {
        let node = Path::new("/data/runtimes/node/22.11.0");
        for (name, script) in [(ShimName::Npm, "npm-cli.js"), (ShimName::Npx, "npx-cli.js")] {
            let plan = plan(name, "22.11.0", None, &["install"]);
            assert_eq!(plan.program, node.join("bin/node"));
            let expected = node.join("lib/node_modules/npm/bin").join(script);
            assert_eq!(plan.script, Some(expected));
            assert_eq!(plan.args, ["install"]);
        }
    }

    #[test]
    fn composer_runs_through_php() {
        let plan = plan(ShimName::Composer, "8.3.14", Some("2.8.4"), &["install"]);
        assert_eq!(plan.program, Path::new("/data/runtimes/php/8.3.14/bin/php"));
        let expected = Path::new("/data/runtimes/composer/2.8.4/composer.phar");
        assert_eq!(plan.script.as_deref(), Some(expected));
        assert_eq!(plan.args, ["install"]);
    }

    #[test]
    fn composer_needs_an_installed_composer() {
        let runtimes = RuntimesDir::in_dir(Path::new("/data"));
        let err = Plan::new(ShimName::Composer, &runtimes, &v("8.4.3"), None, [])
            .expect_err("no composer version");
        assert_eq!(err.code(), "shim_composer_not_installed");
    }

    // Only Unix lets an argument hold bytes that are not UTF-8.
    #[cfg(unix)]
    #[test]
    fn arguments_that_are_not_utf8_pass_unchanged() {
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(vec![b'a', 0xff, b'b']);
        let runtimes = RuntimesDir::in_dir(Path::new("/data"));
        let plan = Plan::new(ShimName::Php, &runtimes, &v("8.4.3"), None, [raw.clone()])
            .expect("valid plan");
        assert_eq!(plan.args, [raw]);
    }

    #[test]
    fn the_command_puts_the_script_first_and_adds_one_to_the_depth() {
        let plan = plan(ShimName::Npm, "22.11.0", None, &["run", "dev"]);
        let command = plan.command(3);
        assert_eq!(command.get_program(), plan.program.as_os_str());
        let script = plan.script.as_deref().expect("npm has a script");
        let expected = [script.as_os_str(), OsStr::new("run"), OsStr::new("dev")];
        assert!(command.get_args().eq(expected));
        let vars: Vec<_> = command.get_envs().collect();
        assert_eq!(vars, [(OsStr::new(depth::VAR), Some(OsStr::new("4")))]);
    }

    #[test]
    fn check_reports_a_missing_program_or_script() {
        let data = tempfile::TempDir::new().expect("create temp dir");
        let runtimes = RuntimesDir::in_dir(data.path());
        let plan =
            Plan::new(ShimName::Npm, &runtimes, &v("22.11.0"), None, []).expect("valid plan");
        let script = plan.script.clone().expect("npm has a script");
        let missing = |plan: &Plan| match plan.check() {
            Err(ShimError::ProgramMissing { path }) => Some(path),
            Err(err) => panic!("unexpected error {err}"),
            Ok(()) => None,
        };
        assert_eq!(missing(&plan), Some(plan.program.clone()));

        for file in [&plan.program, &script] {
            let dir = file.parent().expect("the file has a folder");
            std::fs::create_dir_all(dir).expect("create folder");
        }
        std::fs::write(&plan.program, "").expect("write program");
        assert_eq!(missing(&plan), Some(script.clone()));

        std::fs::write(&script, "").expect("write script");
        assert_eq!(missing(&plan), None);
    }
}
