//! What to run: the real program, an optional script, the user's arguments,
//! and at most one variable that makes the program trust Marsa's CA. Built
//! without touching the disk or the environment, so every case is tested
//! without `exec`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use marsa_core::caddy::CaddyPaths;
use marsa_core::php::PhpFiles;
use marsa_core::resolver::Tool;
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
    /// What the command does with one variable of the environment.
    /// [`Plan::new`] leaves it `None`; the caller sets it from
    /// [`TrustVar::decide`].
    pub trust: Option<TrustEnv>,
}

/// Node adds the certificates in this file to its own list.
pub const NODE_EXTRA_CA_CERTS: &str = "NODE_EXTRA_CA_CERTS";

/// PHP reads its `php.ini` from this path.
pub const PHPRC: &str = "PHPRC";

/// A variable that makes the real program trust Marsa's CA, so a call from
/// the terminal to a `.test` site passes TLS (architecture.md section 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustVar {
    /// [`NODE_EXTRA_CA_CERTS`] or [`PHPRC`].
    pub name: &'static str,
    /// The file the variable names. It may not exist yet.
    pub file: PathBuf,
    /// A value inside this folder was set by an outer shim, not by the user:
    /// `php/` for [`PHPRC`]. `None` for [`NODE_EXTRA_CA_CERTS`]: every Node
    /// version uses the same root, so any value is the user's.
    pub marsa_dir: Option<PathBuf>,
}

/// Where the value that the shim inherited for a [`TrustVar`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inherited {
    /// The variable is not in the environment.
    Unset,
    /// The user set it, maybe to an empty value. It is never touched.
    User,
    /// An outer shim set it: for example `php` on 8.4 that runs `php` in a
    /// project on 8.3. It may name the `php.ini` of another PHP line.
    Marsa,
}

/// What the command does with the variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustEnv {
    /// Sets `name` to `file`.
    Set { name: &'static str, file: PathBuf },
    /// Removes `name`, so the program does not read the file of another PHP
    /// line.
    Remove { name: &'static str },
}

impl TrustVar {
    /// The variable for `name`. The paths come from `marsa-core`, so the
    /// shim and the daemon always agree:
    ///
    /// - `node`, `npm`, `npx`: [`NODE_EXTRA_CA_CERTS`] =
    ///   [`CaddyPaths::ca_root`], Marsa's root certificate.
    /// - `php`, `composer`: [`PHPRC`] = `php/<minor>/php.ini` from
    ///   [`PhpFiles::new`], the file the daemon writes for this PHP line. PHP
    ///   in the terminal then reads Marsa's config, with its `curl.cainfo`.
    ///
    /// `version` is the version that runs; only a PHP version is used. Nothing
    /// is read from the disk.
    pub fn for_shim(name: ShimName, data_dir: &Path, version: &Version) -> Self {
        match name.tool() {
            Tool::Node => Self {
                name: NODE_EXTRA_CA_CERTS,
                file: CaddyPaths::in_dir(data_dir).ca_root(),
                marsa_dir: None,
            },
            Tool::Php => {
                let file = PhpFiles::new(data_dir, version).ini_file;
                // The file is `php/<minor>/php.ini`, so two levels up is `php/`.
                let marsa_dir = file.parent().and_then(Path::parent).map(Path::to_path_buf);
                Self {
                    name: PHPRC,
                    file,
                    marsa_dir,
                }
            }
        }
    }

    /// Where `value`, the inherited value of the variable, came from.
    ///
    /// A path inside [`TrustVar::marsa_dir`] is Marsa's. `Path::starts_with`
    /// compares whole path parts and never resolves links, so `/data/phpx` is
    /// not inside `/data/php`. Any other value, an empty one too, is the
    /// user's.
    pub fn inherited(&self, value: Option<&OsStr>) -> Inherited {
        let marsa = |value: &OsStr| {
            self.marsa_dir
                .as_deref()
                .is_some_and(|dir| Path::new(value).starts_with(dir))
        };
        match value {
            None => Inherited::Unset,
            Some(value) if marsa(value) => Inherited::Marsa,
            Some(_) => Inherited::User,
        }
    }

    /// What the command does with the variable:
    ///
    /// - unset, and the file exists: set it.
    /// - set by the user: nothing. The user's value is never replaced.
    /// - set by Marsa, and the file exists: set it, so it names this PHP
    ///   line's file.
    /// - set by Marsa, and the file is missing: remove it.
    /// - unset, and the file is missing: nothing. Node prints a warning for a
    ///   missing file, and PHP keeps its usual search for `php.ini`.
    ///
    /// Pure, so every case is tested: the caller reads the environment and
    /// the disk.
    pub fn decide(self, inherited: Inherited, file_exists: bool) -> Option<TrustEnv> {
        match (inherited, file_exists) {
            (Inherited::User, _) | (Inherited::Unset, false) => None,
            (Inherited::Unset | Inherited::Marsa, true) => Some(TrustEnv::Set {
                name: self.name,
                file: self.file,
            }),
            (Inherited::Marsa, false) => Some(TrustEnv::Remove { name: self.name }),
        }
    }
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
            trust: None,
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

    /// The command to run. The environment is inherited: the depth counter
    /// goes up by one, and [`Plan::trust`] sets or removes one variable.
    /// `argv[0]` is the absolute path, so `PHP_BINARY` and `process.execPath`
    /// point at the real program, and `@php` in Composer scripts uses the
    /// same PHP.
    pub fn command(&self, depth: u32) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.script)
            .args(&self.args)
            .env(depth::VAR, depth.saturating_add(1).to_string());
        match &self.trust {
            Some(TrustEnv::Set { name, file }) => {
                command.env(name, file);
            }
            Some(TrustEnv::Remove { name }) => {
                command.env_remove(name);
            }
            None => {}
        }
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
    fn node_tools_point_node_at_marsas_root() {
        let data = Path::new("/data");
        let root = data
            .join("caddy")
            .join("data")
            .join("caddy")
            .join("pki")
            .join("authorities")
            .join("marsa")
            .join("root.crt");
        for name in [ShimName::Node, ShimName::Npm, ShimName::Npx] {
            let var = TrustVar::for_shim(name, data, &v("22.11.0"));
            assert_eq!(var.name, "NODE_EXTRA_CA_CERTS", "{name:?}");
            assert_eq!(var.file, root, "{name:?}");
            assert_eq!(var.file, CaddyPaths::in_dir(data).ca_root(), "{name:?}");
            assert_eq!(var.marsa_dir, None, "{name:?}");
        }
    }

    #[test]
    fn php_tools_point_phprc_at_the_ini_of_their_line() {
        let data = Path::new("/data");
        for name in [ShimName::Php, ShimName::Composer] {
            let var = TrustVar::for_shim(name, data, &v("8.4.3"));
            assert_eq!(var.name, "PHPRC", "{name:?}");
            assert_eq!(var.file, data.join("php").join("8.4").join("php.ini"));
            assert_eq!(var.marsa_dir, Some(data.join("php")), "{name:?}");
            let other_line = TrustVar::for_shim(name, data, &v("8.3.14"));
            assert_eq!(
                other_line.file,
                data.join("php").join("8.3").join("php.ini")
            );
        }
    }

    /// Sorts `value` with `var`, then decides.
    fn decide(var: &TrustVar, value: Option<&OsStr>, file_exists: bool) -> Option<TrustEnv> {
        let inherited = var.inherited(value);
        var.clone().decide(inherited, file_exists)
    }

    #[test]
    fn inherited_values_are_sorted_by_who_set_them() {
        let data = Path::new("/data");
        let php = TrustVar::for_shim(ShimName::Php, data, &v("8.4.3"));
        assert_eq!(php.inherited(None), Inherited::Unset);
        let marsa = [
            data.join("php").join("8.3").join("php.ini"),
            data.join("php").join("8.4").join("php.ini"),
        ];
        for value in &marsa {
            let inherited = php.inherited(Some(value.as_os_str()));
            assert_eq!(inherited, Inherited::Marsa, "{value:?}");
        }
        let user = [
            PathBuf::new(),
            data.join("phpx").join("8.3").join("php.ini"),
            data.join("etc").join("php.ini"),
            Path::new("php").join("8.3").join("php.ini"),
        ];
        for value in &user {
            let inherited = php.inherited(Some(value.as_os_str()));
            assert_eq!(inherited, Inherited::User, "{value:?}");
        }

        // Node keeps the simple rule: every value is the user's.
        let node = TrustVar::for_shim(ShimName::Node, data, &v("22.11.0"));
        assert_eq!(node.inherited(None), Inherited::Unset);
        for value in [node.file.clone(), marsa[0].clone(), PathBuf::new()] {
            let inherited = node.inherited(Some(value.as_os_str()));
            assert_eq!(inherited, Inherited::User, "{value:?}");
        }
    }

    #[test]
    fn phprc_decision_for_every_case() {
        let data = Path::new("/data");
        let var = TrustVar::for_shim(ShimName::Php, data, &v("8.4.3"));
        let set = Some(TrustEnv::Set {
            name: PHPRC,
            file: data.join("php").join("8.4").join("php.ini"),
        });
        let remove = Some(TrustEnv::Remove { name: PHPRC });
        let user = Path::new("/home/me/php.ini");
        let other_line = data.join("php").join("8.3").join("php.ini");
        let cases = [
            // Unset.
            (None, true, set.clone()),
            (None, false, None),
            // A user value.
            (Some(user.as_os_str()), true, None),
            (Some(user.as_os_str()), false, None),
            // An empty value is the user's too.
            (Some(OsStr::new("")), true, None),
            (Some(OsStr::new("")), false, None),
            // Marsa's value for another PHP line: replaced, or removed.
            (Some(other_line.as_os_str()), true, set),
            (Some(other_line.as_os_str()), false, remove),
        ];
        for (value, file_exists, expected) in cases {
            let result = decide(&var, value, file_exists);
            assert_eq!(result, expected, "{value:?} {file_exists}");
        }
    }

    #[test]
    fn node_decision_for_every_case() {
        let data = Path::new("/data");
        let var = TrustVar::for_shim(ShimName::Npm, data, &v("22.11.0"));
        let set = Some(TrustEnv::Set {
            name: NODE_EXTRA_CA_CERTS,
            file: var.file.clone(),
        });
        let user = Path::new("/home/me/ca.pem");
        let in_data_dir = data.join("php").join("8.3").join("php.ini");
        let cases = [
            (None, true, set),
            (None, false, None),
            (Some(user.as_os_str()), true, None),
            (Some(user.as_os_str()), false, None),
            (Some(OsStr::new("")), true, None),
            (Some(in_data_dir.as_os_str()), true, None),
            (Some(var.file.as_os_str()), false, None),
        ];
        for (value, file_exists, expected) in cases {
            let result = decide(&var, value, file_exists);
            assert_eq!(result, expected, "{value:?} {file_exists}");
        }
    }

    /// The variables of `command` that are not the depth counter.
    fn extra_vars(command: &Command) -> Vec<(&OsStr, Option<&OsStr>)> {
        command
            .get_envs()
            .filter(|(name, _)| *name != OsStr::new(depth::VAR))
            .collect()
    }

    #[test]
    fn the_command_sets_the_variable() {
        let data = Path::new("/data");
        for (name, version) in [
            (ShimName::Node, "22.11.0"),
            (ShimName::Npm, "22.11.0"),
            (ShimName::Php, "8.4.3"),
            (ShimName::Composer, "8.4.3"),
        ] {
            let var = TrustVar::for_shim(name, data, &v(version));
            let mut plan = plan(name, version, Some("2.8.4"), &[]);
            assert!(extra_vars(&plan.command(0)).is_empty(), "{name:?}");

            plan.trust = decide(&var, None, true);
            let command = plan.command(0);
            let expected = [(OsStr::new(var.name), Some(var.file.as_os_str()))];
            assert_eq!(extra_vars(&command), expected, "{name:?}");
            // The depth counter is still there.
            let counter = command
                .get_envs()
                .find(|(key, _)| *key == OsStr::new(depth::VAR));
            assert_eq!(
                counter,
                Some((OsStr::new(depth::VAR), Some(OsStr::new("1"))))
            );
        }
    }

    /// An outer shim on PHP 8.4 set `PHPRC`; this one runs PHP 8.3.
    #[test]
    fn the_command_replaces_or_removes_marsas_phprc() {
        let data = Path::new("/data");
        let outer = data.join("php").join("8.4").join("php.ini");
        for name in [ShimName::Php, ShimName::Composer] {
            let var = TrustVar::for_shim(name, data, &v("8.3.14"));
            let mut plan = plan(name, "8.3.14", Some("2.8.4"), &[]);

            plan.trust = decide(&var, Some(outer.as_os_str()), true);
            let expected = [(OsStr::new(PHPRC), Some(var.file.as_os_str()))];
            assert_eq!(extra_vars(&plan.command(0)), expected, "{name:?}");

            // `None` as the value is how `get_envs` shows `env_remove`.
            plan.trust = decide(&var, Some(outer.as_os_str()), false);
            let expected = [(OsStr::new(PHPRC), None)];
            assert_eq!(extra_vars(&plan.command(0)), expected, "{name:?}");
        }
    }

    /// A user value is neither set nor removed, so the program gets it from
    /// the inherited environment. A missing file is not named either.
    #[test]
    fn the_command_never_touches_a_user_value_or_names_a_missing_file() {
        let data = Path::new("/data");
        for (name, version) in [(ShimName::Npx, "22.11.0"), (ShimName::Php, "8.4.3")] {
            let var = TrustVar::for_shim(name, data, &v(version));
            for (value, file_exists) in [
                (Some("/home/me/file"), true),
                (Some("/home/me/file"), false),
                (Some(""), true),
                (Some(""), false),
                (None, false),
            ] {
                let mut plan = plan(name, version, None, &[]);
                plan.trust = decide(&var, value.map(OsStr::new), file_exists);
                let command = plan.command(0);
                let vars = extra_vars(&command);
                assert!(vars.is_empty(), "{name:?} {value:?} {file_exists}");
            }
        }
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
