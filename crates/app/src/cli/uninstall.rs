//! `marsa uninstall <tool> <version>`: deletes an installed version, through
//! `DELETE /v1/runtimes/{tool}/{version}`. Task M5-07.
//!
//! The daemon refuses a version in use: the default, a site's own setting,
//! or the running Caddy. Its answer says which, and this command prints it.
//! The only installed PHP or Node version goes with its default, unless PHP
//! sites follow that default (task M5-19).

use std::path::Path;
use std::process::ExitCode;

use marsa_core::client::ClientError;
use marsa_core::runtimes::Runtime;

use super::Tool;
use super::daemon::{ClientFailure, connect, failure};
use super::sites::printable;
use super::strings::{self, Details};

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
pub async fn run(args: &UninstallArgs, data_dir: &Path) -> ExitCode {
    let runtime = Runtime::from(args.tool);
    let client = match connect(data_dir).await {
        Ok(client) => client,
        Err(failure) => return failure.report(),
    };
    match client.uninstall(runtime, args.version.trim()).await {
        Ok(done) => {
            println!(
                "{}",
                strings::uninstalled(runtime, &printable(&done.version))
            );
            ExitCode::SUCCESS
        }
        Err(err) => uninstall_failure(&err, runtime, &args.version, data_dir).report(),
    }
}

/// The text and code the user sees for `err`. A refusal names what uses the
/// version, and a version code names the program and the version asked for.
pub fn uninstall_failure(
    err: &ClientError,
    runtime: Runtime,
    version: &str,
    data_dir: &Path,
) -> ClientFailure {
    let mut failure = failure(err, data_dir, None);
    let requested = printable(version.trim());
    failure.message = match (err, failure.code.as_str()) {
        // The client cleaned its text already.
        (ClientError::InUse { in_use, .. }, _) => strings::version_in_use(runtime, in_use),
        (_, "version_not_installed") => strings::nothing_to_uninstall(runtime, &requested),
        (_, "invalid_version" | "unsupported_alias" | "version_ambiguous") => {
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
        // M5-19: the only PHP, with sites that would have no PHP after it.
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
