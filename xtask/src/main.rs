//! `xtask`: repository chores that are easier in Rust than in a shell script.
//!
//! Run it as `cargo xtask <command>`; the alias lives in `.cargo/config.toml`.

mod dev_shims;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Arg, ArgAction, Command, value_parser};

fn cli() -> Command {
    Command::new("xtask")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Repository tasks for the Marsa workspace")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("dev-shims")
                .about("Build marsa-shim and link php, node, npm, npx, composer for manual testing")
                .arg(
                    Arg::new("data-dir")
                        .long("data-dir")
                        .value_name("PATH")
                        .value_parser(value_parser!(PathBuf))
                        .help("Use this folder instead of Marsa's data folder"),
                )
                .arg(
                    Arg::new("dry-run")
                        .long("dry-run")
                        .action(ArgAction::SetTrue)
                        .help("Print the plan only: no build, nothing written"),
                ),
        )
}

fn main() -> Result<()> {
    let matches = cli().get_matches();
    match matches.subcommand() {
        Some(("dev-shims", args)) => dev_shims::run(
            args.get_one::<PathBuf>("data-dir").cloned(),
            args.get_flag("dry-run"),
        ),
        // `subcommand_required` makes clap reject anything else before we get here.
        _ => Ok(()),
    }
}
