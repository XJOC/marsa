//! `marsa-shim`: runs the real program at the version the project asks for,
//! or a database client at the version its server runs.
//!
//! It is installed under many names (`php`, `node`, `npm`, `npx`, `composer`,
//! and the clients `marsad` adds with a server: `psql`, `mysql`, ...) and
//! learns its job from its own file name. It never talks to the daemon,
//! never uses tokio, and never searches `PATH`: it always runs an absolute
//! path inside `runtimes/`.

mod depth;
mod error;
mod name;
mod plan;
mod sys;

use std::convert::Infallible;
use std::env::{self, ArgsOs};
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use marsa_core::config::{self, ConfigPaths};
use marsa_core::databases::ServerKind;
use marsa_core::databases::clients::Client;
use marsa_core::recipes::Platform;
use marsa_core::resolver::{self, Tool};
use marsa_core::runtimes::{Runtime, RuntimesDir};
use semver::Version;

use crate::error::ShimError;
use crate::name::{Role, ShimName};
use crate::plan::{Inherited, Plan, TrustEnv, TrustVar};

fn main() -> ExitCode {
    match run() {
        Ok(never) => match never {},
        Err(err) => {
            report(&err);
            ExitCode::from(err.exit_code())
        }
    }
}

/// Never returns `Ok`: on success the process has become the real program
/// (Unix), or has exited with the real program's code (Windows).
/// The cheapest steps come first.
fn run() -> Result<Infallible, ShimError> {
    let depth = depth::parse(env::var_os(depth::VAR).as_deref());
    depth::check(depth)?;

    // `args_os`, not `args`: arguments may not be UTF-8, and `args` panics on them.
    let mut args = env::args_os();
    let argv0 = args.next().unwrap_or_default();
    let role = sys::own_name(&argv0).ok_or(ShimError::UnknownName { name: argv0 })?;
    let plan = match role {
        Role::Tool(name) => tool_plan(name, args)?,
        Role::Client(client) => client_plan(client, args)?,
    };
    Err(sys::exec(plan.command(depth)))
}

/// `php`, `node`, `npm`, `npx`, or `composer`, at the version the current
/// folder asks for.
fn tool_plan(name: ShimName, args: ArgsOs) -> Result<Plan, ShimError> {
    // The physical path: symlinks resolved, letter case as on disk. `$PWD` is
    // not read.
    let cwd = env::current_dir().map_err(ShimError::NoCwd)?;
    let data_dir = config::app_data_dir()?;
    let runtimes = RuntimesDir::in_dir(&data_dir);
    let version = pick_version(name.tool(), &cwd, &data_dir, &runtimes)?;

    // One copy of Composer serves every PHP version; the highest one runs.
    // The list is sorted, so the highest is the last.
    let composer = match name {
        ShimName::Composer => runtimes.installed(Runtime::Composer)?.pop(),
        _ => None,
    };

    let platform = Platform::current().ok_or(ShimError::UnsupportedPlatform)?;
    let mut plan = Plan::new(name, platform, &runtimes, &version, composer.as_ref(), args)?;
    plan.check()?;
    plan.trust = trust_env(TrustVar::for_shim(name, &data_dir, &version));
    Ok(plan)
}

/// A database client at the version its server runs, pointed at that server
/// on the port of `global.toml` through the variables the user has not set.
/// A broken `global.toml` stops here, as for the tools.
fn client_plan(client: Client, args: ArgsOs) -> Result<Plan, ShimError> {
    let server = client.server;
    let data_dir = config::app_data_dir()?;
    let global = config::load_global(&ConfigPaths::in_dir(&data_dir).global)?;
    let settings = server.settings(&global);
    let runtimes = RuntimesDir::in_dir(&data_dir);
    let installed = runtimes.installed(server.runtime())?;
    let default = settings.default.as_deref();
    let version = plan::server_version(server, default, &installed, &runtimes)?;

    let platform = Platform::current().ok_or(ShimError::UnsupportedPlatform)?;
    let mut plan = Plan::client(client, platform, &runtimes, &version, args);
    plan.check()?;
    let port = settings.port.unwrap_or(server.default_port());
    let is_set = |name: &str| env::var_os(name).is_some();
    plan.server_env = plan::unset_only(plan::server_env(server, port), is_set);
    if server == ServerKind::Valkey {
        plan.args = plan::valkey_args(port, std::mem::take(&mut plan.args));
    }
    Ok(plan)
}

/// Reads the environment and the disk for [`TrustVar::decide`]. The file is
/// not checked when the user set the variable, so this costs one `is_file` at
/// most.
fn trust_env(var: TrustVar) -> Option<TrustEnv> {
    let inherited = var.inherited(env::var_os(var.name).as_deref());
    let file_exists = inherited != Inherited::User && var.file.is_file();
    var.decide(inherited, file_exists)
}

/// The version of `tool` to run in `cwd`. A broken `global.toml` or
/// `sites.toml` stops here: running another version without saying so would
/// confuse the user.
fn pick_version(
    tool: Tool,
    cwd: &Path,
    data_dir: &Path,
    runtimes: &RuntimesDir,
) -> Result<Version, ShimError> {
    let paths = ConfigPaths::in_dir(data_dir);
    let global = config::load_global(&paths.global)?;
    let sites = config::load_sites(&paths.sites)?;
    let site = sites.site_for(cwd).map(|(_, settings)| settings);

    let (project_setting, global_default) = match tool {
        Tool::Php => (site.and_then(|s| s.php.as_deref()), &global.php.default),
        Tool::Node => (site.and_then(|s| s.node.as_deref()), &global.node.default),
    };
    let installed = runtimes.installed(tool.into())?;
    let resolution = resolver::resolve(
        tool,
        cwd,
        project_setting,
        &installed,
        global_default.as_deref(),
    )?;
    Ok(resolution.version)
}

/// Prints the error on stderr: `marsa: <message> [<code>]`, then an optional
/// hint line.
///
/// `writeln!` with the result ignored, not `eprintln!`: `eprintln!` panics if
/// stderr is closed, and the panic would replace our exit code with 101.
fn report(err: &ShimError) {
    let data_dir = config::app_data_dir().ok();
    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "marsa: {}", err.line());
    if let Some(hint) = err.hint(data_dir.as_deref()) {
        let _ = writeln!(stderr, "marsa: hint: {hint}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A variable that no test run sets. These tests only read the
    /// environment; they never change it.
    const UNSET: &str = "MARSA_TEST_TRUST_VAR_NEVER_SET";

    fn var(name: &'static str, file: PathBuf, marsa_dir: Option<PathBuf>) -> TrustVar {
        TrustVar {
            name,
            file,
            marsa_dir,
        }
    }

    fn set(name: &'static str, file: PathBuf) -> Option<TrustEnv> {
        Some(TrustEnv::Set { name, file })
    }

    /// The value of `PATH`, as a path. Every test run has `PATH`.
    fn path_value() -> PathBuf {
        PathBuf::from(env::var_os("PATH").expect("the tests run with PATH"))
    }

    #[test]
    fn an_unset_variable_is_set_once_its_file_exists() {
        assert_eq!(env::var_os(UNSET), None, "{UNSET} must not be set");
        let dir = tempfile::tempdir().expect("create a temp dir");
        let file = dir.path().join("root.crt");
        assert_eq!(trust_env(var(UNSET, file.clone(), None)), None);
        // A folder is not a file.
        let folder = dir.path().to_path_buf();
        assert_eq!(trust_env(var(UNSET, folder, None)), None);

        std::fs::write(&file, "pem").expect("write the file");
        assert_eq!(trust_env(var(UNSET, file.clone(), None)), set(UNSET, file));
    }

    #[test]
    fn a_variable_the_user_set_is_left_alone() {
        // `PATH` stands for a value the user set: it is not inside the
        // `marsa_dir` given here.
        let dir = tempfile::tempdir().expect("create a temp dir");
        let file = dir.path().join("php.ini");
        std::fs::write(&file, "").expect("write the file");
        let marsa_dir = Some(dir.path().join("php"));
        assert_eq!(trust_env(var("PATH", file, marsa_dir)), None);
    }

    #[test]
    fn a_value_inside_marsa_dir_is_replaced_or_removed() {
        // `PATH` stands for a value an outer shim set: its own value is
        // inside `marsa_dir` when `marsa_dir` is that value.
        let marsa_dir = Some(path_value());
        let dir = tempfile::tempdir().expect("create a temp dir");
        let file = dir.path().join("php.ini");
        let removed = Some(TrustEnv::Remove { name: "PATH" });
        assert_eq!(
            trust_env(var("PATH", file.clone(), marsa_dir.clone())),
            removed
        );

        std::fs::write(&file, "").expect("write the file");
        let replaced = set("PATH", file.clone());
        assert_eq!(trust_env(var("PATH", file, marsa_dir)), replaced);
    }
}
