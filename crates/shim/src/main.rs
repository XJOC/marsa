//! `marsa-shim`: runs the real program at the version the project asks for.
//!
//! It is installed under many names (`php`, `node`, and later `npm`, `npx`,
//! `composer`) and learns its job from its own file name. It never talks to the
//! daemon, never uses tokio, and never searches `PATH`: it always runs an
//! absolute path inside `runtimes/`.

mod depth;
mod error;
mod name;
mod plan;
mod sys;

use std::convert::Infallible;
use std::env;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use marsa_core::config::{self, ConfigPaths};
use marsa_core::resolver::{self, Tool};
use marsa_core::runtimes::RuntimesDir;
use semver::Version;

use crate::error::ShimError;
use crate::name::ShimName;
use crate::plan::Plan;

fn main() -> ExitCode {
    match run() {
        Ok(never) => match never {},
        Err(err) => {
            report(&err);
            ExitCode::from(err.exit_code())
        }
    }
}

/// Never returns `Ok`: on success the process has become the real program.
/// The cheapest steps come first.
fn run() -> Result<Infallible, ShimError> {
    let depth = depth::parse(env::var_os(depth::VAR).as_deref());
    depth::check(depth)?;

    // `args_os`, not `args`: arguments may not be UTF-8, and `args` panics on them.
    let mut args = env::args_os();
    let argv0 = args.next().unwrap_or_default();
    let name = ShimName::from_argv0(&argv0).ok_or(ShimError::UnknownName { name: argv0 })?;

    // The physical path: symlinks resolved, letter case as on disk. `$PWD` is
    // not read.
    let cwd = env::current_dir().map_err(ShimError::NoCwd)?;
    let data_dir = config::app_data_dir()?;
    let runtimes = RuntimesDir::in_dir(&data_dir);
    let version = pick_version(name.tool(), &cwd, &data_dir, &runtimes)?;

    let plan = Plan::new(name, &runtimes, &version, args);
    plan.check()?;
    Err(sys::exec(plan.command(depth)))
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
