//! `marsa`: the command-line client. Every command that changes state, and
//! every command that reads it, is a call to `marsad` (task M3-12 moved
//! `marsa install` there).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use marsa::cli::autostart::{self, AutostartArgs};
use marsa::cli::daemon::{self, DaemonCommand};
use marsa::cli::default_version::{self, UseArgs};
use marsa::cli::install::{self, InstallArgs};
use marsa::cli::logs::{self, LogsArgs};
use marsa::cli::runtimes::{self, RuntimesArgs};
use marsa::cli::services::{self, ServicesArgs};
use marsa::cli::setup;
use marsa::cli::sites::{self, SitesCommand};
use marsa::cli::uninstall::{self, UninstallArgs};

/// Local development runtimes and sites.
#[derive(Debug, Parser)]
#[command(name = "marsa", version, about)]
struct Cli {
    /// Use this folder as the app data folder, for tests. The shims always
    /// read the default folder, so they do not see what is installed here.
    /// There is no environment variable for it, on purpose.
    #[arg(long, global = true, value_name = "PATH")]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Install a version of PHP, Node.js, Composer, or Caddy.
    Install(InstallArgs),
    /// List the installed versions.
    Runtimes(RuntimesArgs),
    /// Make an installed PHP or Node.js version the default: every folder
    /// that sets no version of its own runs it.
    Use(UseArgs),
    /// Delete an installed version. Marsa refuses a version that is the
    /// default, that a site is set to, or that the running Caddy uses.
    Uninstall(UninstallArgs),
    /// Start, stop, or check the Marsa daemon (marsad).
    #[command(subcommand)]
    Daemon(DaemonCommand),
    /// List the services, or start, stop, or restart one.
    Services(ServicesArgs),
    /// Show the output of a service.
    Logs(LogsArgs),
    /// `sites`, `park`, `link`, `secure`, and `unsecure`, each a command of
    /// its own.
    #[command(flatten)]
    Sites(SitesCommand),
    /// Trust Marsa's HTTPS certificates, send .test names to Marsa, and route
    /// ports 80 and 443 to it (macOS). macOS may ask for an administrator
    /// password, and may then ask you to approve trusting Marsa's
    /// certificate authority for your user.
    Setup,
    /// Start Marsa at login, or not: on or off. Without on or off, show
    /// whether it does (macOS and Linux).
    Autostart(AutostartArgs),
}

// One thread is enough: the command waits on one thing at a time.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    // Wrong arguments exit here with code 2 and clap's own message.
    let cli = Cli::parse();
    marsa::cli::init_logging();
    let data_dir = match marsa::cli::data_dir(cli.data_dir) {
        Ok(dir) => dir,
        Err(failure) => return failure.report(),
    };
    match &cli.command {
        Command::Install(args) => install::run(args, &data_dir).await,
        Command::Runtimes(args) => runtimes::run(args, &data_dir).await,
        Command::Use(args) => default_version::run(args, &data_dir).await,
        Command::Uninstall(args) => uninstall::run(args, &data_dir).await,
        Command::Daemon(command) => daemon::run(*command, &data_dir).await,
        Command::Services(args) => services::run(args, &data_dir).await,
        Command::Logs(args) => logs::run(args, &data_dir).await,
        Command::Sites(command) => sites::run(command, &data_dir).await,
        Command::Setup => setup::run(&data_dir).await,
        Command::Autostart(args) => autostart::run(args, &data_dir).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;
    use clap::error::ErrorKind;
    use marsa::cli::Tool;
    use marsa::cli::services::ServicesCommand;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("marsa").chain(args.iter().copied()))
    }

    fn install(args: &[&str]) -> InstallArgs {
        match parse(args).expect("valid arguments").command {
            Command::Install(args) => args,
            other => panic!("not install: {other:?}"),
        }
    }

    #[test]
    fn the_cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn install_takes_a_tool_a_version_and_force() {
        assert_eq!(
            install(&["install", "php", "8.4"]),
            InstallArgs {
                tool: Tool::Php,
                version: Some("8.4".into()),
                force: false,
            }
        );
        assert_eq!(
            install(&["install", "node"]),
            InstallArgs {
                tool: Tool::Node,
                version: None,
                force: false,
            }
        );
        assert_eq!(
            install(&["install", "composer", "2", "--force"]),
            InstallArgs {
                tool: Tool::Composer,
                version: Some("2".into()),
                force: true,
            }
        );
        assert_eq!(
            install(&["install", "caddy", "2.11.8"]),
            InstallArgs {
                tool: Tool::Caddy,
                version: Some("2.11.8".into()),
                force: false,
            }
        );
        match parse(&["runtimes", "caddy"]).expect("valid").command {
            Command::Runtimes(args) => assert_eq!(args.tool, Some(Tool::Caddy)),
            other => panic!("not runtimes: {other:?}"),
        }
    }

    #[test]
    fn bad_arguments_exit_with_code_2() {
        for args in [
            &["install", "ruby"][..],
            &["install"],
            &["install", "php", "8.4", "8.3"],
            &["runtimes", "python"],
            &[],
        ] {
            let err = parse(args).expect_err("invalid arguments");
            assert_eq!(err.exit_code(), 2, "{args:?}: {:?}", err.kind());
        }
        assert_eq!(
            parse(&["install", "ruby"]).expect_err("ruby").kind(),
            ErrorKind::InvalidValue
        );
    }

    #[test]
    fn use_takes_a_tool_and_a_version() {
        for (args, tool, version) in [
            (&["use", "php", "8.3"][..], Tool::Php, "8.3"),
            (&["use", "node", "22"], Tool::Node, "22"),
            (
                &["--data-dir", "/x", "use", "php", "8.3.12"],
                Tool::Php,
                "8.3.12",
            ),
            // The daemon answers `no_default_version`, with its own text.
            (&["use", "composer", "2"], Tool::Composer, "2"),
        ] {
            match parse(args).expect("valid").command {
                Command::Use(got) => assert_eq!(
                    got,
                    UseArgs {
                        tool,
                        version: version.into(),
                    },
                    "{args:?}"
                ),
                other => panic!("not use: {other:?}"),
            }
        }
        for args in [
            &["use"][..],
            &["use", "php"],
            &["use", "ruby", "3"],
            &["use", "php", "8.3", "8.4"],
        ] {
            let err = parse(args).expect_err("invalid arguments");
            assert_eq!(err.exit_code(), 2, "{args:?}: {:?}", err.kind());
        }
    }

    #[test]
    fn daemon_takes_start_stop_or_status() {
        for (word, want) in [
            ("start", DaemonCommand::Start),
            ("stop", DaemonCommand::Stop),
            ("status", DaemonCommand::Status),
        ] {
            match parse(&["daemon", word]).expect("valid").command {
                Command::Daemon(got) => assert_eq!(got, want),
                other => panic!("not daemon: {other:?}"),
            }
        }
    }

    #[test]
    fn services_lists_or_runs_an_action_on_one_service() {
        match parse(&["services"]).expect("valid").command {
            Command::Services(args) => assert_eq!(args.command, None),
            other => panic!("not services: {other:?}"),
        }
        match parse(&["services", "restart", "caddy"])
            .expect("valid")
            .command
        {
            Command::Services(args) => assert_eq!(
                args.command,
                Some(ServicesCommand::Restart { id: "caddy".into() })
            ),
            other => panic!("not services: {other:?}"),
        }
    }

    #[test]
    fn logs_takes_an_id_and_follow() {
        for (args, follow) in [
            (&["logs", "caddy"][..], false),
            (&["logs", "caddy", "-f"], true),
            (&["logs", "--follow", "caddy"], true),
        ] {
            match parse(args).expect("valid").command {
                Command::Logs(got) => assert_eq!(
                    got,
                    LogsArgs {
                        id: "caddy".into(),
                        follow,
                    }
                ),
                other => panic!("not logs: {other:?}"),
            }
        }
    }

    #[test]
    fn bad_daemon_services_and_logs_arguments_exit_with_code_2() {
        for args in [
            &["daemon"][..],
            &["daemon", "restart"],
            &["services", "start"],
            &["services", "kill", "caddy"],
            &["services", "start", "caddy", "php"],
            &["logs"],
            &["logs", "caddy", "--tail"],
        ] {
            let err = parse(args).expect_err("invalid arguments");
            assert_eq!(err.exit_code(), 2, "{args:?}: {:?}", err.kind());
        }
    }

    /// The full parsing tests are in `cli/sites.rs`.
    #[test]
    fn the_sites_commands_are_top_level_commands() {
        for (args, want) in [
            (&["sites"][..], SitesCommand::Sites),
            (
                &["park", "code"],
                SitesCommand::Park {
                    dir: Some(PathBuf::from("code")),
                },
            ),
            (
                &["link", "--data-dir", "/x", "api"],
                SitesCommand::Link {
                    name: Some("api".into()),
                    path: None,
                },
            ),
            (
                &["secure", "shop"],
                SitesCommand::Secure {
                    name: "shop".into(),
                },
            ),
            (
                &["unsecure", "shop"],
                SitesCommand::Unsecure {
                    name: "shop".into(),
                },
            ),
        ] {
            match parse(args).expect("valid").command {
                Command::Sites(got) => assert_eq!(got, want, "{args:?}"),
                other => panic!("not a sites command: {other:?}"),
            }
        }
        let err = parse(&["sites", "park"]).expect_err("not a sub-command");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn setup_takes_no_argument() {
        for args in [&["setup"][..], &["setup", "--data-dir", "/x"]] {
            match parse(args).expect("valid").command {
                Command::Setup => {}
                other => panic!("not setup: {other:?}"),
            }
        }
        for args in [&["setup", "now"][..], &["setup", "--force"]] {
            let err = parse(args).expect_err("invalid arguments");
            assert_eq!(err.exit_code(), 2, "{args:?}: {:?}", err.kind());
        }
    }

    #[test]
    fn autostart_takes_on_off_or_nothing() {
        use marsa::cli::autostart::Switch;

        for (args, switch) in [
            (&["autostart"][..], None),
            (&["autostart", "on"], Some(Switch::On)),
            (&["autostart", "off"], Some(Switch::Off)),
            (&["--data-dir", "/x", "autostart", "on"], Some(Switch::On)),
            (&["autostart", "off", "--data-dir", "/x"], Some(Switch::Off)),
        ] {
            match parse(args).expect("valid").command {
                Command::Autostart(got) => assert_eq!(got, AutostartArgs { switch }, "{args:?}"),
                other => panic!("not autostart: {other:?}"),
            }
        }
        for args in [
            &["autostart", "yes"][..],
            &["autostart", "ON"],
            &["autostart", "on", "off"],
            &["autostart", "--on"],
        ] {
            let err = parse(args).expect_err("invalid arguments");
            assert_eq!(err.exit_code(), 2, "{args:?}: {:?}", err.kind());
        }
    }

    #[test]
    fn data_dir_is_global_and_runtimes_takes_an_optional_tool() {
        let cli = parse(&["runtimes", "--data-dir", "/tmp/m"]).expect("valid");
        assert_eq!(cli.data_dir, Some(PathBuf::from("/tmp/m")));
        match cli.command {
            Command::Runtimes(args) => assert_eq!(args.tool, None),
            other => panic!("not runtimes: {other:?}"),
        }
        match parse(&["--data-dir", "/x", "runtimes", "php"])
            .expect("valid")
            .command
        {
            Command::Runtimes(args) => assert_eq!(args.tool, Some(Tool::Php)),
            other => panic!("not runtimes: {other:?}"),
        }
    }
}
