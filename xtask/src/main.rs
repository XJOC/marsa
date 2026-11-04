//! `xtask`: repository chores that are easier in Rust than in a shell script.
//!
//! Run it as `cargo xtask <command>`; the alias lives in `.cargo/config.toml`.

mod dev_shims;
mod manifest;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use marsa_core::recipes::Platform;

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
        .subcommand(
            Command::new("manifest")
                .about("Build or verify the signed runtimes manifest")
                .subcommand_required(true)
                .subcommand(
                    Command::new("build")
                        .about("Download every pinned file and write manifest.json, ready to sign")
                        .arg(
                            Arg::new("out")
                                .long("out")
                                .value_name("DIR")
                                .value_parser(value_parser!(PathBuf))
                                .help("Output folder [default: target/manifest]"),
                        )
                        .arg(
                            Arg::new("pins")
                                .long("pins")
                                .value_name("FILE")
                                .value_parser(value_parser!(PathBuf))
                                .help("Pins file [default: recipes/pins.toml]"),
                        )
                        .arg(
                            Arg::new("previous-serial")
                                .long("previous-serial")
                                .value_name("N")
                                .value_parser(value_parser!(u64))
                                .conflicts_with("previous-manifest")
                                .help("Serial of the published manifest (default: download it)"),
                        )
                        .arg(
                            Arg::new("previous-manifest")
                                .long("previous-manifest")
                                .value_name("FILE")
                                .value_parser(value_parser!(PathBuf))
                                .help("Read the published serial from this manifest.json"),
                        )
                )
                .subcommand(
                    Command::new("check-php")
                        .about("Run the PHP checks of `build` on one local archive of our own PHP build")
                        .arg(
                            Arg::new("archive")
                                .required(true)
                                .value_parser(value_parser!(PathBuf)),
                        )
                        .arg(
                            Arg::new("platform")
                                .long("platform")
                                .required(true)
                                .value_name("PLATFORM")
                                .value_parser(value_parser!(Platform))
                                .help("darwin-arm64, darwin-x64, linux-x64, or linux-arm64"),
                        )
                        .arg(
                            Arg::new("version")
                                .long("version")
                                .required(true)
                                .value_name("VERSION")
                                .value_parser(value_parser!(semver::Version))
                                .help("The PHP version, such as 8.4.27"),
                        )
                        .arg(
                            Arg::new("pins")
                                .long("pins")
                                .value_name("FILE")
                                .value_parser(value_parser!(PathBuf))
                                .help("Pins file, for spc_line [default: recipes/pins.toml]"),
                        ),
                )
                .subcommand(
                    Command::new("verify")
                        .about("Check manifest.json and its signature with the key built into marsa")
                        .arg(
                            Arg::new("manifest")
                                .required(true)
                                .value_parser(value_parser!(PathBuf)),
                        )
                        .arg(
                            Arg::new("signature")
                                .value_parser(value_parser!(PathBuf))
                                .help("[default: <manifest>.minisig]"),
                        ),
                ),
        )
}

fn manifest_command(args: &ArgMatches) -> Result<()> {
    match args.subcommand() {
        Some(("build", args)) => {
            let root = manifest::workspace_root()?;
            let previous = match (
                args.get_one::<u64>("previous-serial"),
                args.get_one::<PathBuf>("previous-manifest"),
            ) {
                (Some(serial), _) => manifest::Previous::Serial(*serial),
                (None, Some(path)) => manifest::Previous::File(path.clone()),
                (None, None) => manifest::Previous::Download,
            };
            manifest::run_build(manifest::BuildOptions {
                pins: args
                    .get_one::<PathBuf>("pins")
                    .cloned()
                    .unwrap_or_else(|| root.join("recipes").join("pins.toml")),
                out: args
                    .get_one::<PathBuf>("out")
                    .cloned()
                    .unwrap_or_else(|| root.join("target").join("manifest")),
                previous,
            })
        }
        Some(("check-php", args)) => {
            let root = manifest::workspace_root()?;
            let archive = args
                .get_one::<PathBuf>("archive")
                .context("clap requires the archive")?;
            let platform = args
                .get_one::<Platform>("platform")
                .context("clap requires --platform")?;
            let version = args
                .get_one::<semver::Version>("version")
                .context("clap requires --version")?;
            let pins = args
                .get_one::<PathBuf>("pins")
                .cloned()
                .unwrap_or_else(|| root.join("recipes").join("pins.toml"));
            manifest::run_check_php(archive, *platform, version, &pins)
        }
        Some(("verify", args)) => match args.get_one::<PathBuf>("manifest") {
            Some(path) => manifest::verify(path, args.get_one::<PathBuf>("signature").cloned()),
            None => Ok(()),
        },
        // `subcommand_required` makes clap reject anything else.
        _ => Ok(()),
    }
}

fn main() -> Result<()> {
    let matches = cli().get_matches();
    match matches.subcommand() {
        Some(("dev-shims", args)) => dev_shims::run(
            args.get_one::<PathBuf>("data-dir").cloned(),
            args.get_flag("dry-run"),
        ),
        Some(("manifest", args)) => manifest_command(args),
        // `subcommand_required` makes clap reject anything else before we get here.
        _ => Ok(()),
    }
}
