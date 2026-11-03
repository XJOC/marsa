//! What to run: the real program and its arguments. Built without touching
//! the disk, so every case is tested without `exec`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use marsa_core::runtimes::RuntimesDir;
use semver::Version;

use crate::depth;
use crate::error::ShimError;
use crate::name::ShimName;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Always absolute, so starting it never searches `PATH`.
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

impl Plan {
    /// `version` must come from [`RuntimesDir::installed`], never from user
    /// text, so a value like `../../x` cannot reach the path.
    pub fn new(
        name: ShimName,
        runtimes: &RuntimesDir,
        version: &Version,
        user_args: impl IntoIterator<Item = OsString>,
    ) -> Self {
        let runtime = name.runtime();
        let program = runtimes
            .version_dir(runtime, version)
            .join("bin")
            .join(runtime.dir_name());
        Self {
            program,
            args: user_args.into_iter().collect(),
        }
    }

    /// # Errors
    ///
    /// `shim_program_missing` if the program is not a file. The version folder
    /// exists, so this is a broken install, not a missing version.
    pub fn check(&self) -> Result<(), ShimError> {
        if self.program.is_file() {
            Ok(())
        } else {
            Err(ShimError::ProgramMissing {
                path: self.program.clone(),
            })
        }
    }

    /// The command to run. The environment is inherited; only the depth
    /// counter goes up by one. `argv[0]` is the absolute path, so `PHP_BINARY`
    /// and `process.execPath` point at the real program.
    pub fn command(&self, depth: u32) -> Command {
        let mut command = Command::new(&self.program);
        command
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

    fn plan(name: ShimName, version: &str, args: &[&str]) -> Plan {
        let runtimes = RuntimesDir::in_dir(Path::new("/data"));
        let version = Version::parse(version).expect("valid test version");
        Plan::new(name, &runtimes, &version, args.iter().map(OsString::from))
    }

    #[test]
    fn php_runs_its_own_binary() {
        let plan = plan(ShimName::Php, "8.4.3", &["-v", "a b"]);
        let expected = Path::new("/data/runtimes/php/8.4.3/bin/php");
        assert_eq!(plan.program, expected);
        assert_eq!(plan.args, ["-v", "a b"]);
    }

    #[test]
    fn node_runs_its_own_binary() {
        let plan = plan(ShimName::Node, "22.11.0", &[]);
        let expected = Path::new("/data/runtimes/node/22.11.0/bin/node");
        assert_eq!(plan.program, expected);
        assert!(plan.args.is_empty());
    }

    // Only Unix lets an argument hold bytes that are not UTF-8.
    #[cfg(unix)]
    #[test]
    fn arguments_that_are_not_utf8_pass_unchanged() {
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(vec![b'a', 0xff, b'b']);
        let runtimes = RuntimesDir::in_dir(Path::new("/data"));
        let version = Version::new(8, 4, 3);
        let plan = Plan::new(ShimName::Php, &runtimes, &version, [raw.clone()]);
        assert_eq!(plan.args, [raw]);
    }

    #[test]
    fn the_command_adds_one_to_the_depth() {
        let plan = plan(ShimName::Php, "8.4.3", &["-r", "echo 1;"]);
        let command = plan.command(3);
        assert_eq!(command.get_program(), plan.program.as_os_str());
        assert!(command.get_args().eq(["-r", "echo 1;"].map(OsStr::new)));
        let vars: Vec<_> = command.get_envs().collect();
        assert_eq!(vars, [(OsStr::new(depth::VAR), Some(OsStr::new("4")))]);
    }

    #[test]
    fn check_reports_a_missing_program() {
        let data = tempfile::TempDir::new().expect("create temp dir");
        let runtimes = RuntimesDir::in_dir(data.path());
        let plan = Plan::new(ShimName::Php, &runtimes, &Version::new(8, 4, 3), []);
        let err = plan.check().expect_err("nothing is there");
        assert_eq!(err.code(), "shim_program_missing");

        let bin = plan.program.parent().expect("the program has a folder");
        std::fs::create_dir_all(bin).expect("create bin");
        std::fs::write(&plan.program, "").expect("write program");
        assert!(plan.check().is_ok());
    }
}
