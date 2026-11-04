//! `marsa install <tool> [version] [--force]`: asks `marsad` to install one
//! runtime version into `runtimes/` (`POST /v1/runtimes/{tool}/install`),
//! then prints the install's events from `/v1/events` until it ends.
//!
//! The daemon runs the installer; this file only prints what it reports.

use std::io::IsTerminal as _;
use std::path::Path;
use std::process::ExitCode;

use marsa_core::client::{ClientError, DaemonClient, StreamItem};
use marsa_core::recipes::Platform;
use marsa_core::runtimes::Runtime;
use marsa_core::types::{Event, InstallDone, InstallFailure, InstallUpdate};

use super::Tool;
use super::daemon::{ClientFailure, connect, failure};
use super::progress::Progress;
use super::strings::{self, Details};

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

/// Starts the daemon if needed and installs through it, with progress on
/// stderr and the result on stdout. Exit code 0 also when the version was
/// already installed, 1 for any failure.
pub async fn run(args: &InstallArgs, data_dir: &Path) -> ExitCode {
    let runtime = Runtime::from(args.tool);
    let client = match connect(data_dir).await {
        Ok(client) => client,
        Err(failure) => return failure.report(),
    };
    // Subscribed before the request, so no event of this install is missed.
    let mut stream = match client.events().await {
        Ok(stream) => stream,
        Err(err) => return failure(&err, data_dir, None).report(),
    };
    let accepted = match client
        .install(runtime, args.version.as_deref(), args.force)
        .await
    {
        Ok(accepted) => accepted,
        Err(err) => return failure(&err, data_dir, None).report(),
    };

    let platform = Platform::current().map_or_else(|| "this system".to_owned(), |p| p.to_string());
    let progress = Progress::new(std::io::stderr().is_terminal(), platform);
    let mut follow = Follow::new(accepted.id, progress);
    let end = loop {
        // `EventStream::next` gives `None` after the daemon closed the stream.
        let (text, end) = follow.next(stream.next().await);
        eprint!("{text}");
        if let Some(end) = end {
            break end;
        }
    };

    match end {
        End::Done(done) => {
            print_result(&done, &client).await;
            ExitCode::SUCCESS
        }
        End::Failed(failed) => install_failure(&failed, runtime).report(),
        End::Broken(err) => failure(&err, data_dir, None).report(),
        End::Interrupted => ClientFailure::new("install_interrupted", data_dir, None).report(),
    }
}

/// How an install ended, as this command saw it.
#[derive(Debug)]
pub enum End {
    Done(InstallDone),
    Failed(InstallFailure),
    /// The event stream broke before the install ended.
    Broken(ClientError),
    /// The daemon stopped before the install ended.
    Interrupted,
}

/// Reads the event stream for install `id`: no I/O here, so tests feed it
/// items and compare the text.
#[derive(Debug)]
pub struct Follow {
    id: u64,
    progress: Progress,
    /// The last stream error, until an event shows that the stream goes on.
    broken: Option<ClientError>,
}

impl Follow {
    pub fn new(id: u64, progress: Progress) -> Self {
        Self {
            id,
            progress,
            broken: None,
        }
    }

    /// The text to print for the next stream `item`, and how the install
    /// ended if this item ends it. Events of other installs are skipped.
    pub fn next(
        &mut self,
        item: Option<Result<StreamItem<Event>, ClientError>>,
    ) -> (String, Option<End>) {
        let event = match item {
            Some(Ok(StreamItem::Item(event))) => event,
            // Lost events may be progress lines; the last event of the
            // install comes after them.
            Some(Ok(StreamItem::Lagged(_))) => return (String::new(), None),
            // An event that cannot be read is skipped; a broken connection
            // gives an error, then `None`.
            Some(Err(err)) => {
                self.broken = Some(err);
                return (String::new(), None);
            }
            None => {
                let end = self.broken.take().map_or(End::Interrupted, End::Broken);
                return (self.progress.finish(), Some(end));
            }
        };
        self.broken = None;
        match event {
            Event::Install(step) if step.id == self.id => match step.update {
                InstallUpdate::Running(event) => (self.progress.render(&event), None),
                InstallUpdate::Done(done) => (self.progress.finish(), Some(End::Done(done))),
                InstallUpdate::Failed(failed) => {
                    (self.progress.finish(), Some(End::Failed(failed)))
                }
            },
            Event::ShuttingDown => (self.progress.finish(), Some(End::Interrupted)),
            _ => (String::new(), None),
        }
    }
}

async fn print_result(done: &InstallDone, client: &DaemonClient) {
    let version = printable(&done.version);
    if done.already {
        println!("{}", strings::already_installed(done.runtime, &version));
    } else {
        println!("{}", strings::installed(done.runtime, &version, &done.dir));
    }
    if done.runtime == Runtime::Php {
        // Only a tip: if the list cannot be read, it is left out.
        let no_composer = client.runtimes().await.is_ok_and(|list| {
            list.runtimes
                .iter()
                .any(|r| r.runtime == Runtime::Composer && r.installed.is_empty())
        });
        if no_composer {
            eprintln!("{}", strings::COMPOSER_TIP);
        }
    }
}

/// The text and code the user sees for a `failed` install event. The
/// daemon's English `message` goes to the log only.
pub fn install_failure(failed: &InstallFailure, runtime: Runtime) -> ClientFailure {
    tracing::debug!(code = %failed.code, error = %failed.message, "install failed");
    let code = printable(&failed.code);
    let text = |value: &Option<String>| value.as_deref().map(printable);
    let (requested, platform, url, cause, output) = (
        text(&failed.requested),
        text(&failed.platform),
        text(&failed.url),
        text(&failed.cause),
        text(&failed.output),
    );
    let available: Vec<String> = failed.available.iter().map(|v| printable(v)).collect();
    let details = Details {
        runtime: Some(runtime),
        requested: requested.as_deref(),
        available: &available,
        platform: platform.as_deref(),
        path: failed.path.as_deref(),
        url: url.as_deref(),
        cause: cause.as_deref(),
        output: output.as_deref(),
        service: None,
    };
    ClientFailure {
        message: strings::message(&code, &details),
        code,
    }
}

/// Text from the daemon, safe to print: no control characters but line
/// breaks and tabs, so it cannot move the cursor or recolor the terminal.
fn printable(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::types::{InstallEvent, InstallProgress};
    use std::path::PathBuf;

    type Item = Option<Result<StreamItem<Event>, ClientError>>;

    fn event(id: u64, update: InstallUpdate) -> Item {
        Some(Ok(StreamItem::Item(Event::Install(InstallProgress {
            id,
            update,
        }))))
    }

    fn step(id: u64, event: InstallEvent) -> Item {
        self::event(id, InstallUpdate::Running(event))
    }

    fn done() -> InstallDone {
        InstallDone {
            runtime: Runtime::Php,
            version: "8.4.23".into(),
            dir: PathBuf::from("/d/runtimes/php/8.4.23"),
            already: false,
        }
    }

    /// Feeds `items` to install 2 and returns the text printed and how it
    /// ended.
    fn follow(items: Vec<Item>) -> (String, Option<End>) {
        let mut follow = Follow::new(2, Progress::new(false, "darwin-arm64"));
        let mut printed = String::new();
        for item in items {
            let (text, end) = follow.next(item);
            printed.push_str(&text);
            if end.is_some() {
                return (printed, end);
            }
        }
        (printed, None)
    }

    #[test]
    fn the_steps_of_this_install_are_printed_until_done() {
        let artifact = || "cli".to_owned();
        let (printed, end) = follow(vec![
            // The end of an earlier install, from another client.
            event(1, InstallUpdate::Done(done())),
            step(2, InstallEvent::ManifestVerified { serial: 3 }),
            Some(Ok(StreamItem::Lagged(4))),
            step(
                2,
                InstallEvent::Selected {
                    runtime: Runtime::Php,
                    version: "8.4.23".into(),
                },
            ),
            step(
                2,
                InstallEvent::Downloading {
                    artifact: artifact(),
                    done: 2_000_000,
                    total: 2_000_000,
                },
            ),
            step(
                2,
                InstallEvent::Verified {
                    artifact: artifact(),
                },
            ),
            step(
                2,
                InstallEvent::Installed {
                    dir: PathBuf::from("/d"),
                },
            ),
            event(2, InstallUpdate::Done(done())),
            step(2, InstallEvent::Checking),
        ]);
        assert_eq!(
            printed,
            "Verified the version list (serial 3)\n\
             Installing PHP 8.4.23 for darwin-arm64\n\
             \x20 cli      [##########] 2.0 MB of 2.0 MB (100%)\n\
             \x20 cli      [##########] 2.0 MB, sha256 ok\n"
        );
        assert!(
            matches!(&end, Some(End::Done(got)) if *got == done()),
            "{end:?}"
        );
    }

    #[test]
    fn a_failure_ends_with_its_code() {
        let failed = InstallFailure {
            code: "install_busy".into(),
            message: "another install is running".into(),
            ..InstallFailure::default()
        };
        let (printed, end) = follow(vec![event(2, InstallUpdate::Failed(failed.clone()))]);
        assert_eq!(printed, "");
        assert!(
            matches!(&end, Some(End::Failed(got)) if *got == failed),
            "{end:?}"
        );
    }

    #[test]
    fn the_daemon_stopping_or_a_broken_stream_ends_it() {
        let (_, end) = follow(vec![Some(Ok(StreamItem::Item(Event::ShuttingDown)))]);
        assert!(matches!(end, Some(End::Interrupted)), "{end:?}");
        let (_, end) = follow(vec![None]);
        assert!(matches!(end, Some(End::Interrupted)), "{end:?}");

        let broken = || Some(Err(ClientError::BadResponse { reason: "x".into() }));
        let (_, end) = follow(vec![broken(), None]);
        assert!(
            matches!(&end, Some(End::Broken(err)) if err.code() == "daemon_bad_response"),
            "{end:?}"
        );
        // An event after the error shows that the stream went on.
        let (_, end) = follow(vec![broken(), step(2, InstallEvent::Checking), None]);
        assert!(matches!(end, Some(End::Interrupted)), "{end:?}");
    }

    #[test]
    fn no_such_version_shows_what_was_asked_and_what_exists() {
        let failed = InstallFailure {
            code: "install_no_such_version".into(),
            message: "x".into(),
            requested: Some("8.9".into()),
            available: vec!["8.3.27".into(), "8.4.23".into(), "8.5.4".into()],
            ..InstallFailure::default()
        };
        assert_eq!(
            install_failure(&failed, Runtime::Php).line(),
            "marsa: No PHP version matches \"8.9\". Available: 8.3.27, 8.4.23, 8.5.4 \
             [install_no_such_version]"
        );
    }

    #[test]
    fn download_failed_names_the_last_url_and_its_cause() {
        let failed = InstallFailure {
            code: "download_failed".into(),
            url: Some("https://mirror.example/php.tar.gz".into()),
            cause: Some("download_http_status".into()),
            ..InstallFailure::default()
        };
        let message = install_failure(&failed, Runtime::Php).message;
        assert!(
            message.contains("server answered with an error for https://mirror.example/php.tar.gz"),
            "{message}"
        );
    }

    #[test]
    fn text_from_the_daemon_cannot_move_the_cursor() {
        let failed = InstallFailure {
            code: "install_check_failed".into(),
            output: Some("PHP 8.4.22\u{1b}[2J\n(cli)\r".into()),
            ..InstallFailure::default()
        };
        let message = install_failure(&failed, Runtime::Php).message;
        assert!(
            message.ends_with("It printed: PHP 8.4.22[2J\n(cli)"),
            "{message:?}"
        );
    }
}
