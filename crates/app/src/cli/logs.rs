//! `marsa logs <id> [-f]`: the recent output of a service, and with `-f` its
//! new lines as they come, until Ctrl+C. `marsa logs --site <name> [-f]`:
//! the same for a Laravel site's own log, which the daemon reads.
//!
//! Each line goes to stdout as the service printed it, like `tail`. Notes
//! (no lines yet, skipped lines, the end of the log) go to stderr, so a pipe
//! gets the lines only.

use std::io;
use std::path::Path;
use std::process::ExitCode;

use marsa_core::client::{ClientError, DaemonClient, EventStream, StreamItem};
use marsa_core::types::{LogLine, SiteLogLine};
use serde::de::DeserializeOwned;

use super::Failure;
use super::daemon::{connect, failure};
use super::strings::{self, Details};

/// The arguments of `marsa logs`.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct LogsArgs {
    /// The service, such as caddy.
    #[arg(required_unless_present = "site", conflicts_with = "site")]
    pub id: Option<String>,

    /// Show a Laravel site's own log (storage/logs/laravel*.log) instead.
    #[arg(long, value_name = "NAME")]
    pub site: Option<String>,

    /// Keep showing new lines until Ctrl+C.
    #[arg(short, long)]
    pub follow: bool,
}

/// What the lines come from.
#[derive(Debug, Clone, Copy)]
enum Source<'a> {
    Service(&'a str),
    Site(&'a str),
}

impl Source<'_> {
    fn failure(self, err: &ClientError, data_dir: &Path) -> Failure {
        match self {
            Self::Service(id) => failure(err, data_dir, Some(id)),
            Self::Site(name) => {
                let mut failure = failure(err, data_dir, None);
                let details = Details {
                    site: Some(name),
                    ..Details::default()
                };
                failure.message = strings::message(&failure.code, &details);
                failure
            }
        }
    }
}

/// Starts the daemon if needed, then prints the lines.
pub async fn run(args: &LogsArgs, data_dir: &Path) -> Result<ExitCode, Failure> {
    let source = match (&args.site, &args.id) {
        (Some(name), _) => Source::Site(name),
        (None, Some(id)) => Source::Service(id),
        // clap asks for one of the two.
        (None, None) => Source::Service("caddy"),
    };
    let client = connect(data_dir).await?;
    match (source, args.follow) {
        (Source::Service(id), false) => recent_service(&client, id, data_dir).await,
        (Source::Site(name), false) => recent_site(&client, name, data_dir).await,
        (Source::Service(id), true) => match client.follow_logs(id).await {
            Ok(stream) => follow(stream, source, data_dir, |line: &LogLine| &line.text).await,
            Err(err) => Err(source.failure(&err, data_dir)),
        },
        (Source::Site(name), true) => match client.follow_site_log(name).await {
            Ok(stream) => follow(stream, source, data_dir, |line: &SiteLogLine| &line.text).await,
            Err(err) => Err(source.failure(&err, data_dir)),
        },
    }
}

async fn recent_service(
    client: &DaemonClient,
    id: &str,
    data_dir: &Path,
) -> Result<ExitCode, Failure> {
    let lines = match client.logs(id).await {
        Ok(lines) => lines,
        Err(err) => return Err(Source::Service(id).failure(&err, data_dir)),
    };
    if lines.is_empty() {
        eprintln!("{}", strings::no_log_lines(id));
    }
    Ok(print_all(lines.iter().map(|line| line.text.as_str())))
}

async fn recent_site(
    client: &DaemonClient,
    name: &str,
    data_dir: &Path,
) -> Result<ExitCode, Failure> {
    let log = match client.site_log(name).await {
        Ok(log) => log,
        Err(err) => return Err(Source::Site(name).failure(&err, data_dir)),
    };
    match &log.file {
        None => eprintln!("{}", strings::no_site_log(name)),
        Some(file) if log.cut_at_start => eprintln!("{}", strings::site_log_cut(file)),
        Some(_) => {}
    }
    Ok(print_all(log.lines.iter().map(|line| line.text.as_str())))
}

fn print_all<'a>(texts: impl Iterator<Item = &'a str>) -> ExitCode {
    let mut out = io::stdout().lock();
    for text in texts {
        if let Err(err) = print_line(&mut out, text) {
            return stdout_closed(&err);
        }
    }
    ExitCode::SUCCESS
}

/// The first lines, then the new ones, until Ctrl+C or until the daemon
/// closes the stream. Exit code 0 in both cases, 1 if the stream breaks.
async fn follow<T: DeserializeOwned>(
    mut stream: EventStream<T>,
    source: Source<'_>,
    data_dir: &Path,
    text: impl Fn(&T) -> &str,
) -> Result<ExitCode, Failure> {
    eprintln!(
        "{}",
        match source {
            Source::Service(id) => strings::following(id),
            Source::Site(name) => strings::following_site(name),
        }
    );
    // Made once, before the loop: a Ctrl+C between two lines is not lost.
    // If the handler cannot be set up, Ctrl+C keeps its default and ends the
    // process at once, which is also fine.
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %err, "cannot listen for Ctrl+C");
            std::future::pending::<()>().await;
        }
    };
    let mut ctrl_c = std::pin::pin!(ctrl_c);
    // The last error, until a line shows that the stream goes on.
    let mut broken = false;
    loop {
        // `EventStream::next` is cancel-safe: a Ctrl+C while it waits loses
        // no line.
        tokio::select! {
            () = &mut ctrl_c => return Ok(ExitCode::SUCCESS),
            item = stream.next() => match item {
                Some(Ok(StreamItem::Item(line))) => {
                    broken = false;
                    if let Err(err) = print_line(&mut io::stdout().lock(), text(&line)) {
                        return Ok(stdout_closed(&err));
                    }
                }
                Some(Ok(StreamItem::Lagged(count))) => eprintln!("{}", strings::lagged(count)),
                // An event that cannot be read is skipped and the stream goes
                // on; a broken connection ends it, and `None` comes next.
                Some(Err(err)) => {
                    broken = true;
                    eprintln!("{}", source.failure(&err, data_dir).line());
                }
                None if broken => return Ok(ExitCode::FAILURE),
                None => {
                    eprintln!("{}", strings::LOG_ENDED);
                    return Ok(ExitCode::SUCCESS);
                }
            },
        }
    }
}

fn print_line(out: &mut impl io::Write, text: &str) -> io::Result<()> {
    writeln!(out, "{text}")?;
    out.flush()
}

/// stdout is gone, for example `marsa logs caddy | head -1` after its line.
/// Not an error for a closed pipe: the reader has what it wanted.
fn stdout_closed(err: &io::Error) -> ExitCode {
    tracing::debug!(error = %err, "cannot write to stdout");
    if err.kind() == io::ErrorKind::BrokenPipe {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_its_text_alone() {
        let mut out = Vec::new();
        print_line(&mut out, "{\"level\":\"info\",\"msg\":\"serving\"}").expect("write to memory");
        assert_eq!(out, b"{\"level\":\"info\",\"msg\":\"serving\"}\n");
    }

    #[test]
    fn a_closed_pipe_is_not_a_failure() {
        let closed = io::Error::from(io::ErrorKind::BrokenPipe);
        assert_eq!(stdout_closed(&closed), ExitCode::SUCCESS);
        let other = io::Error::other("disk full");
        assert_eq!(stdout_closed(&other), ExitCode::FAILURE);
    }
}
