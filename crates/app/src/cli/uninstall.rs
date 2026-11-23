//! `marsa uninstall <tool> <version>`: deletes an installed version, through
//! `POST /v1/jobs/runtimes/{tool}/{version}/uninstall`.
//!
//! The daemon refuses a version in use: the default, a site's own setting,
//! or the running Caddy. Its answer says which, and this command prints it.
//! The only installed PHP or Node version goes with its default, unless PHP
//! sites follow that default.

use marsa_core::codes;
use std::path::Path;
use std::process::ExitCode;

use marsa_core::client::ClientError;
use marsa_core::databases::ServerKind;
use marsa_core::runtimes::Runtime;

use super::Failure;
use super::Tool;
use super::daemon::{connect, failure};
use super::strings::{self, Details};
use super::text::printable;

/// The arguments of `marsa uninstall`.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct UninstallArgs {
    #[arg(value_enum)]
    pub tool: Tool,

    /// The installed version: 8.3.12, or 8.3 when only one installed version
    /// matches it. "marsa runtimes" lists them.
    pub version: String,
}

/// Starts the daemon if needed and uninstalls: one line on stdout, or the
/// failure on stderr with exit code 1.
pub async fn run(args: &UninstallArgs, data_dir: &Path) -> Result<ExitCode, Failure> {
    let runtime = Runtime::from(args.tool);
    let client = connect(data_dir).await?;
    match client
        .run_runtime_uninstall(runtime, args.version.trim())
        .await
    {
        Ok(done) => {
            println!(
                "{}",
                strings::uninstalled(runtime, &printable(&done.version))
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => Err(uninstall_failure(&err, runtime, &args.version, data_dir)),
    }
}

/// The text and code the user sees for `err`. A refusal names what uses the
/// version, and a version code names the program and the version asked for.
fn uninstall_failure(
    err: &ClientError,
    runtime: Runtime,
    version: &str,
    data_dir: &Path,
) -> Failure {
    // `service_busy`: an action on the database server runs, and a start
    // may pick the version.
    let service = ServerKind::of_runtime(runtime).map(ServerKind::id);
    let mut failure = failure(err, data_dir, service);
    let requested = printable(version.trim());
    failure.message = match (err, &*failure.code) {
        // The client cleaned its text already.
        (ClientError::InUse { in_use, .. }, _) => strings::version_in_use(runtime, in_use),
        (_, codes::VERSION_NOT_INSTALLED) => strings::nothing_to_uninstall(runtime, &requested),
        (_, codes::INVALID_VERSION | codes::UNSUPPORTED_ALIAS | codes::VERSION_AMBIGUOUS) => {
            let details = Details {
                runtime: Some(runtime),
                requested: Some(&requested),
                ..Details::default()
            };
            strings::message(&failure.code, &details)
        }
        _ => return failure,
    };
    failure
}

/// The texts the integration tests in `tests/install.rs` do not reach.
#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::types::VersionInUse;

    fn line(err: &ClientError, runtime: Runtime) -> String {
        uninstall_failure(err, runtime, "8.3", Path::new("/d")).line()
    }

    fn in_use(in_use: VersionInUse) -> ClientError {
        let message = "x".into();
        let in_use = Box::new(in_use);
        ClientError::InUse { message, in_use }
    }

    #[test]
    fn each_use_says_what_to_do_first() {
        let php = in_use(VersionInUse {
            version: "8.3.12".into(),
            default: Some("8.3".into()),
            sites: vec!["shop".into()],
            ..VersionInUse::default()
        });
        assert_eq!(
            line(&php, Runtime::Php),
            "marsa: PHP 8.3.12 is in use, so it was not uninstalled. It is the default (8.3): \
             make another version the default first with \"marsa use php <version>\". The \
             site shop is set to it: give it another version first. [version_in_use]"
        );
        let caddy = in_use(VersionInUse {
            version: "2.11.7".into(),
            caddy: true,
            ..VersionInUse::default()
        });
        assert_eq!(
            line(&caddy, Runtime::Caddy),
            "marsa: Caddy 2.11.7 is in use, so it was not uninstalled. Caddy runs from it: \
             stop Caddy first with \"marsa services stop caddy\". [version_in_use]"
        );
        let postgres = in_use(VersionInUse {
            version: "18.7.0".into(),
            server: true,
            ..VersionInUse::default()
        });
        assert_eq!(
            line(&postgres, Runtime::Postgres),
            "marsa: PostgreSQL 18.7.0 is in use, so it was not uninstalled. PostgreSQL runs from \
             it: stop it first with \"marsa services stop postgres\". Its databases stay either \
             way. [version_in_use]"
        );
        // The only PHP, with sites that would have no PHP after it.
        let last = in_use(VersionInUse {
            version: "8.3.12".into(),
            sites_left_without_php: vec!["blog".into(), "shop".into()],
            ..VersionInUse::default()
        });
        assert_eq!(
            line(&last, Runtime::Php),
            "marsa: PHP 8.3.12 is in use, so it was not uninstalled. The sites blog, shop would \
             have no PHP: PHP 8.3.12 is the only PHP installed. Install another version first \
             with \"marsa install php <version>\", then make it the default with \"marsa use \
             php <version>\". [sites_lose_php]"
        );
        let one = in_use(VersionInUse {
            version: "8.3.12".into(),
            sites: vec!["api".into()],
            sites_left_without_php: vec!["blog".into()],
            ..VersionInUse::default()
        });
        assert_eq!(
            line(&one, Runtime::Php),
            "marsa: PHP 8.3.12 is in use, so it was not uninstalled. The site api is set to \
             it: give it another version first. The site blog would have no PHP: PHP 8.3.12 is \
             the only PHP installed. Install another version first with \"marsa install php \
             <version>\", then make it the default with \"marsa use php <version>\". \
             [version_in_use]"
        );
        let ambiguous = ClientError::Api {
            status: 409,
            code: "version_ambiguous".into(),
            message: "x".into(),
        };
        assert_eq!(
            line(&ambiguous, Runtime::Php),
            "marsa: \"8.3\" matches more than one installed PHP version. Name one in full; \
             \"marsa runtimes php\" lists them. [version_ambiguous]"
        );
    }
}
