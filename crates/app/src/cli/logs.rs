//! `marsa logs <id> [-f]`: the recent output of a service, and with `-f` its
//! new lines as they come, until Ctrl+C. Task M3-09.
//!
//! Each line goes to stdout as the service printed it, like `tail`. Notes
//! (no lines yet, skipped lines, the end of the log) go to stderr, so a pipe
//! gets the lines only.

use std::io;
use std::path::Path;
use std::process::ExitCode;

use marsa_core::client::{DaemonClient, StreamItem};
use marsa_core::types::LogLine;

use super::daemon::{connect, failure};
use super::strings;

/// The arguments of `marsa logs`.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct LogsArgs {
    /// The service, such as caddy.
    pub id: String,

    /// Keep showing new lines until Ctrl+C.
    #[arg(short, long)]
    pub follow: bool,
}

/// Starts the daemon if needed, then prints the lines.
pub async fn run(args: &LogsArgs, data_dir: &Path) -> ExitCode {
    let client = match connect(data_dir).await {
        Ok(client) => client,
        Err(failure) => return failure.report(),
    };
    if args.follow {
        follow(&client, &args.id, data_dir).await
    } else {
        recent(&client, &args.id, data_dir).await
    }
}

async fn recent(client: &DaemonClient, id: &str, data_dir: &Path) -> ExitCode {
    let lines = match client.logs(id).await {
        Ok(lines) => lines,
        Err(err) => return failure(&err, data_dir, Some(id)).report(),
    };
    if lines.is_empty() {
        eprintln!("{}", strings::no_log_lines(id));
    }
    let mut out = io::stdout().lock();
    for line in &lines {
        if let Err(err) = print_line(&mut out, line) {
            return stdout_closed(&err);
        }
    }
    ExitCode::SUCCESS
}

/// The recent lines, then the new ones, until Ctrl+C or until the daemon
/// closes the stream. Exit code 0 in both cases, 1 if the stream breaks.
async fn follow(client: &DaemonClient, id: &str, data_dir: &Path) -> ExitCode {
    let mut stream = match client.follow_logs(id).await {
        Ok(stream) => stream,
        Err(err) => return failure(&err, data_dir, Some(id)).report(),
    };
    eprintln!("{}", strings::following(id));
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
            () = &mut ctrl_c => return ExitCode::SUCCESS,
            item = stream.next() => match item {
                Some(Ok(StreamItem::Item(line))) => {
                    broken = false;
                    if let Err(err) = print_line(&mut io::stdout().lock(), &line) {
                        return stdout_closed(&err);
                    }
                }
                Some(Ok(StreamItem::Lagged(count))) => eprintln!("{}", strings::lagged(count)),
                // An event that cannot be read is skipped and the stream goes
                // on; a broken connection ends it, and `None` comes next.
                Some(Err(err)) => {
                    broken = true;
                    eprintln!("{}", failure(&err, data_dir, Some(id)).line());
                }
                None if broken => return ExitCode::FAILURE,
                None => {
                    eprintln!("{}", strings::LOG_ENDED);
                    return ExitCode::SUCCESS;
                }
            },
        }
    }
}

fn print_line(out: &mut impl io::Write, line: &LogLine) -> io::Result<()> {
    writeln!(out, "{}", line.text)?;
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
    use marsa_core::types::LogStream;

    #[test]
    fn a_line_is_its_text_alone() {
        let line = LogLine {
            service: "caddy".into(),
            seq: 7,
            at_ms: 1,
            stream: LogStream::Stderr,
            text: "{\"level\":\"info\",\"msg\":\"serving\"}".into(),
        };
        let mut out = Vec::new();
        print_line(&mut out, &line).expect("write to memory");
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
