//! `marsa install <tool> [version] [--force]`: installs one runtime version
//! into `runtimes/`. Every step is in `marsa_core::installer`; this file only
//! starts it and prints what it reports.

use std::error::Error;
use std::io::IsTerminal as _;
use std::path::Path;
use std::process::ExitCode;

use marsa_core::installer::extract::ExtractError;
use marsa_core::installer::fetch::FetchError;
use marsa_core::installer::{self, InstallEnv, InstallError, InstallRequest, InstalledVersion};
use marsa_core::recipes::Platform;
use marsa_core::runtimes::{Runtime, RuntimesDir, RuntimesError};
use marsa_core::types::InstallEvent;

use super::progress::Progress;
use super::strings::{self, Details};
use super::{Failure, Tool};

/// The arguments of `marsa install`.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct InstallArgs {
    /// The program to install.
    #[arg(value_enum)]
    pub tool: Tool,

    /// 8, 8.4, or 8.4.23: the same rules as a .php-version file. Without it:
    /// the newest stable version.
    pub version: Option<String>,

    /// Install again, even if this version is already installed.
    #[arg(long)]
    pub force: bool,
}

/// Installs, with progress on stderr and the result on stdout. Exit code 0
/// also when the version was already installed.
pub async fn run(args: &InstallArgs, data_dir: &Path) -> ExitCode {
    let runtime = Runtime::from(args.tool);
    let request = InstallRequest {
        runtime,
        version: args.version.as_deref(),
        force: args.force,
    };
    let platform = Platform::current().map_or_else(|| "this system".to_owned(), |p| p.to_string());
    let mut progress = Progress::new(std::io::stderr().is_terminal(), platform);

    let result = {
        let mut events = |event: InstallEvent| eprint!("{}", progress.render(&event));
        install_directly(&request, data_dir, &mut events).await
    };
    eprint!("{}", progress.finish());

    match result {
        Ok(done) => {
            print_result(&done, data_dir);
            ExitCode::SUCCESS
        }
        Err(err) => {
            tracing::debug!(code = err.code(), error = %chain(&err), "install failed");
            failure(&err, &request).report()
        }
    }
}

/// Runs the installer in this process.
///
/// M2 only: no daemon yet. M3 moves this call into marsad (task M3-12,
/// `POST /v1/runtimes/{tool}/install`, with the events sent over SSE), and
/// `marsa install` becomes a client of it through `marsa_core::client`. Keep
/// every direct call to the installer inside this one function.
async fn install_directly(
    request: &InstallRequest<'_>,
    data_dir: &Path,
    events: &mut (dyn FnMut(InstallEvent) + Send),
) -> Result<InstalledVersion, InstallError> {
    let env = InstallEnv::production(data_dir)?;
    installer::install(request, &env, events).await
}

fn print_result(done: &InstalledVersion, data_dir: &Path) {
    let version = done.version.to_string();
    if done.already {
        println!("{}", strings::already_installed(done.runtime, &version));
    } else {
        println!("{}", strings::installed(done.runtime, &version, &done.dir));
    }
    if done.runtime == Runtime::Php {
        let composer = RuntimesDir::in_dir(data_dir).installed(Runtime::Composer);
        if composer.is_ok_and(|versions| versions.is_empty()) {
            eprintln!("{}", strings::COMPOSER_TIP);
        }
    }
}

/// The text and code the user sees for `err`.
pub fn failure(err: &InstallError, request: &InstallRequest<'_>) -> Failure {
    let mut details = Details {
        runtime: Some(request.runtime),
        ..Details::default()
    };
    match err {
        InstallError::UnsupportedPlatform { platform, .. } => details.platform = Some(platform),
        InstallError::NoSuchVersion {
            requested,
            available,
            ..
        } => {
            details.requested = requested.as_deref();
            details.available = available;
        }
        InstallError::CheckFailed { output, .. } => details.output = Some(output),
        InstallError::SerialCorrupt { path } | InstallError::Io { path, .. } => {
            details.path = Some(path);
        }
        InstallError::Fetch(fetch) => {
            details.url = url_of(fetch);
            details.cause = fetch.last_code();
            if let FetchError::Io { path, .. } = fetch {
                details.path = Some(path);
            }
        }
        InstallError::Extract(ExtractError::Io(path, _))
        | InstallError::Runtimes(RuntimesError::Read { path, .. }) => details.path = Some(path),
        InstallError::Version(_) => details.requested = request.version,
        _ => {}
    }
    Failure {
        code: err.code(),
        message: strings::message(err.code(), &details),
    }
}

/// The URL a download error is about. For `download_failed`, the one of the
/// last mirror tried.
fn url_of(err: &FetchError) -> Option<&str> {
    match err {
        FetchError::InsecureUrl { url }
        | FetchError::HttpStatus { url, .. }
        | FetchError::Network { url, .. }
        | FetchError::TooLong { url, .. }
        | FetchError::TooShort { url, .. }
        | FetchError::ChecksumMismatch { url, .. }
        | FetchError::TooLarge { url, .. } => Some(url),
        FetchError::Failed {
            last: Some(last), ..
        } => url_of(last),
        FetchError::Failed { last: None, .. } | FetchError::Io { .. } => None,
    }
}

/// The message of `err` and of each cause under it, for the log.
fn chain(err: &dyn Error) -> String {
    let mut text = err.to_string();
    let mut cause = err.source();
    while let Some(current) = cause {
        text.push_str(": ");
        text.push_str(&current.to_string());
        cause = current.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::installer::manifest::ManifestError;
    use marsa_core::resolver::BadRequest;
    use std::path::PathBuf;

    fn request(version: Option<&str>) -> InstallRequest<'_> {
        InstallRequest {
            runtime: Runtime::Php,
            version,
            force: false,
        }
    }

    #[test]
    fn no_such_version_shows_what_was_asked_and_what_exists() {
        let err = InstallError::NoSuchVersion {
            runtime: "php",
            requested: Some("8.9".into()),
            available: vec!["8.3.27".into(), "8.4.23".into(), "8.5.4".into()],
        };
        let failure = failure(&err, &request(Some("8.9")));
        assert_eq!(failure.code, "install_no_such_version");
        assert_eq!(
            failure.line(),
            "marsa: No PHP version matches \"8.9\". Available: 8.3.27, 8.4.23, 8.5.4 \
             [install_no_such_version]"
        );
    }

    #[test]
    fn download_failed_names_the_last_url_and_its_cause() {
        let err = InstallError::Fetch(FetchError::Failed {
            tried: 2,
            last: Some(Box::new(FetchError::HttpStatus {
                url: "https://mirror.example/php.tar.gz".into(),
                status: 503,
            })),
        });
        let failure = failure(&err, &request(None));
        assert_eq!(failure.code, "download_failed");
        assert!(
            failure
                .message
                .contains("server answered with an error for https://mirror.example/php.tar.gz"),
            "{}",
            failure.message
        );
    }

    #[test]
    fn details_come_from_the_error() {
        let serial = InstallError::SerialCorrupt {
            path: PathBuf::from("/d/runtimes/.manifest-serial"),
        };
        assert!(
            failure(&serial, &request(None))
                .message
                .contains(".manifest-serial is damaged"),
        );

        let alias = InstallError::Version(BadRequest::Alias);
        assert!(
            failure(&alias, &request(Some("latest")))
                .message
                .starts_with("\"latest\" is a name, not a version.")
        );

        let key = InstallError::Manifest(ManifestError::NoTrustedKey);
        let failure = failure(&key, &request(Some("8.4")));
        assert_eq!(failure.code, "manifest_no_trusted_key");
        assert!(failure.message.contains("no trusted manifest key yet"));
    }

    #[test]
    fn the_log_text_holds_every_cause() {
        let err = InstallError::Io {
            path: PathBuf::from("/d"),
            source: std::io::Error::other("disk full"),
        };
        assert_eq!(chain(&err), "disk error at /d: disk full");
    }
}
