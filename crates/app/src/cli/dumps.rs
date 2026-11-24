//! `marsa dumps`, `marsa dumps --follow`, `marsa dumps clear`, `on`, and
//! `off`: the `dump()` calls of PHP sites that `marsad` caught (ADR 0027),
//! through `GET /v1/dumps`, `DELETE /v1/dumps`, `/v1/dumps/capture`, and the
//! event `dumps_changed`. The CLI never talks to PHP.
//!
//! A dump's text comes from a PHP program, so every character that could
//! move the cursor or change the colors of the terminal is dropped before it
//! is printed.

use std::io;
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use marsa_core::client::{DaemonClient, StreamItem};
use marsa_core::types::{Dump, DumpsStatus, Event};

use super::Failure;
use super::daemon::{connect, failure};
use super::strings;
use super::text::{printable, printable_multiline};

/// How many dumps `marsa dumps` shows.
const LAST: usize = 20;

/// The arguments of `marsa dumps`.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct DumpsArgs {
    /// Keep showing new dumps until Ctrl+C.
    #[arg(short, long)]
    pub follow: bool,

    #[command(subcommand)]
    pub command: Option<DumpsCommand>,
}

/// What `marsa dumps` does. Without one, it lists the last dumps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::Subcommand)]
pub enum DumpsCommand {
    /// Delete every dump.
    Clear,
    /// Send dump() of PHP sites to Marsa (the default).
    On,
    /// Leave dump() in the page.
    Off,
}

/// Starts the daemon if needed, then lists, follows, clears, or switches.
pub async fn run(args: &DumpsArgs, data_dir: &Path) -> Result<ExitCode, Failure> {
    let client = connect(data_dir).await?;
    let result = match args.command {
        None if args.follow => return follow(&client, data_dir).await,
        None => client.dumps(None).await.map(|list| {
            let status = list.status;
            render_list(&list.dumps, list.latest_id, status, now())
        }),
        Some(DumpsCommand::Clear) => client
            .clear_dumps()
            .await
            .map(|_| strings::DUMPS_CLEARED.to_owned()),
        Some(DumpsCommand::On) => switch(&client, true).await,
        Some(DumpsCommand::Off) => switch(&client, false).await,
    };
    let text = result.map_err(|err| failure(&err, data_dir, None))?;
    print!("{text}");
    Ok(ExitCode::SUCCESS)
}

/// Turns the capture on or off, and says what happened: on can find the
/// port taken.
async fn switch(
    client: &DaemonClient,
    enabled: bool,
) -> Result<String, marsa_core::client::ClientError> {
    client.set_dumps_capture(enabled).await?;
    if !enabled {
        return Ok(strings::DUMPS_NOW_OFF.to_owned());
    }
    let list = client.dumps(None).await?;
    Ok(match strings::dumps_status_note(list.status) {
        Some(note) => format!("{note}\n"),
        None => strings::DUMPS_NOW_ON.to_owned(),
    })
}

/// Now, in milliseconds since the Unix epoch.
fn now() -> u64 {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

/// The last [`LAST`] dumps of `newest_first`, oldest first, so the newest
/// is at the bottom of the terminal, where the eye is. `now` gives each
/// dump's age.
pub fn render_list(newest_first: &[Dump], latest: u64, status: DumpsStatus, now: u64) -> String {
    let listed = &newest_first[..newest_first.len().min(LAST)];
    if listed.is_empty() {
        let note = strings::dumps_status_note(status).unwrap_or(strings::DUMPS_NONE);
        return format!("{note}\n");
    }
    let mut text = format!("{}\n\n", strings::dumps_count(listed.len(), latest));
    for dump in listed.iter().rev() {
        text.push_str(&render_dump(dump, now));
    }
    if let Some(note) = strings::dumps_status_note(status) {
        text.push_str(note);
        text.push('\n');
    }
    text
}

/// One dump: a header line, its text, and a blank line.
pub fn render_dump(dump: &Dump, now: u64) -> String {
    let mut header = format!(
        "#{}  {}",
        dump.id,
        strings::age(now.saturating_sub(dump.received_at))
    );
    if let Some(file) = &dump.source_file {
        header.push_str("  ");
        header.push_str(&printable(file));
        if let Some(line) = dump.source_line {
            header.push_str(&format!(":{line}"));
        }
    }
    if let Some(uri) = &dump.uri {
        let method = dump.method.as_deref().unwrap_or("");
        header.push_str(&format!("  {} {}", printable(method), printable(uri)));
    }
    if let Some(label) = &dump.label {
        header.push_str(&format!("  [{}]", printable(label)));
    }
    let header = header.trim_end().to_owned();
    let mut text = format!("{header}\n{}\n", printable_multiline(dump.text.trim_end()));
    if dump.truncated {
        text.push_str(strings::DUMP_CUT);
        text.push('\n');
    }
    text.push('\n');
    text
}

/// The last dumps, then each new one as it comes, until Ctrl+C or until the
/// daemon closes the stream. The stream is opened first, so a dump made
/// while the list is read is not lost.
async fn follow(client: &DaemonClient, data_dir: &Path) -> Result<ExitCode, Failure> {
    let mut events = client
        .events()
        .await
        .map_err(|err| failure(&err, data_dir, None))?;
    let list = client
        .dumps(None)
        .await
        .map_err(|err| failure(&err, data_dir, None))?;
    let mut seen = list.latest_id;
    let mut out = io::stdout().lock();
    if print(
        &mut out,
        &render_list(&list.dumps, list.latest_id, list.status, now()),
    )
    .is_err()
    {
        return Ok(ExitCode::SUCCESS);
    }
    eprintln!("{}", strings::DUMPS_FOLLOWING);
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %err, "cannot listen for Ctrl+C");
            std::future::pending::<()>().await;
        }
    };
    let mut ctrl_c = std::pin::pin!(ctrl_c);
    let mut broken = false;
    loop {
        tokio::select! {
            () = &mut ctrl_c => return Ok(ExitCode::SUCCESS),
            item = events.next() => match item {
                Some(Ok(StreamItem::Item(Event::DumpsChanged { .. }))) | Some(Ok(StreamItem::Lagged(_))) => {
                    broken = false;
                    let news = match client.dumps(Some(seen)).await {
                        Ok(list) => list,
                        Err(err) => {
                            eprintln!("{}", failure(&err, data_dir, None).line());
                            continue;
                        }
                    };
                    seen = news.latest_id.max(seen);
                    let mut text = String::new();
                    for dump in news.dumps.iter().rev() {
                        text.push_str(&render_dump(dump, now()));
                    }
                    if print(&mut out, &text).is_err() {
                        return Ok(ExitCode::SUCCESS);
                    }
                }
                Some(Ok(StreamItem::Item(Event::ShuttingDown))) => {
                    eprintln!("{}", strings::DUMPS_ENDED);
                    return Ok(ExitCode::SUCCESS);
                }
                // Other events are not for this command.
                Some(Ok(StreamItem::Item(_))) => {}
                Some(Err(err)) => {
                    broken = true;
                    eprintln!("{}", failure(&err, data_dir, None).line());
                }
                None if broken => return Ok(ExitCode::FAILURE),
                None => {
                    eprintln!("{}", strings::DUMPS_ENDED);
                    return Ok(ExitCode::SUCCESS);
                }
            },
        }
    }
}

/// Writes and flushes. An error is a closed pipe (`marsa dumps -f | head`):
/// the reader has what it wanted.
fn print(out: &mut impl io::Write, text: &str) -> io::Result<()> {
    out.write_all(text.as_bytes())?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dump(id: u64, text: &str) -> Dump {
        Dump {
            id,
            received_at: 1_000_000,
            source_file: Some("/app/routes/web.php".into()),
            source_line: Some(7),
            uri: Some("http://shop.test/users".into()),
            method: Some("GET".into()),
            label: None,
            text: text.into(),
            truncated: false,
        }
    }

    #[test]
    fn a_dump_shows_its_place_its_url_and_its_text() {
        assert_eq!(
            render_dump(&dump(12, "array:1 [\n  0 => 1\n]"), 1_000_000 + 90_000),
            "#12  1 min ago  /app/routes/web.php:7  GET http://shop.test/users\n\
             array:1 [\n  0 => 1\n]\n\n"
        );
        let plain = Dump {
            source_file: None,
            source_line: None,
            uri: None,
            method: None,
            label: Some("mine".into()),
            truncated: true,
            ..dump(3, "\"x\"")
        };
        assert_eq!(
            render_dump(&plain, 1_000_000),
            "#3  just now  [mine]\n\"x\"\n\
             (This dump was cut: it was longer than Marsa keeps.)\n\n"
        );
    }

    #[test]
    fn control_characters_never_reach_the_terminal() {
        let evil = Dump {
            source_file: Some("/app/\u{1b}[2Jx.php".into()),
            label: Some("a\u{7}b".into()),
            ..dump(1, "\"a\u{1b}[31mred\u{1b}]0;title\u{7}\"\r\nline\u{0}")
        };
        let text = render_dump(&evil, 1_000_000);
        assert!(!text.contains('\u{1b}') && !text.contains('\u{7}') && !text.contains('\0'));
        assert!(!text.contains('\r'));
        assert!(text.contains("[31mred"), "the visible text stays: {text}");
    }

    #[test]
    fn the_list_is_oldest_first_and_at_most_twenty() {
        let newest_first: Vec<Dump> = (1..=25).rev().map(|id| dump(id, "1")).collect();
        let text = render_list(&newest_first, 25, DumpsStatus::Listening, 1_000_000);
        assert!(
            text.starts_with("The last 20 dumps (the newest is #25):\n\n#6 "),
            "{text}"
        );
        assert!(text.contains("#25 "));
        assert!(!text.contains("#5 "));
        assert!(text.find("#6 ") < text.find("#25 "), "the newest is last");
    }

    #[test]
    fn no_dumps_says_why_or_how() {
        let none = |status| render_list(&[], 0, status, 0);
        assert!(none(DumpsStatus::Listening).contains("dd()"));
        assert!(none(DumpsStatus::Listening).contains("VAR_DUMPER_FORMAT=server"));
        assert!(none(DumpsStatus::Disabled).contains("marsa dumps on"));
        assert!(none(DumpsStatus::PortInUse).contains("9912"));
        // A busy port is told after a list, too: the page shows new dumps.
        let text = render_list(&[dump(1, "1")], 1, DumpsStatus::PortInUse, 1_000_000);
        assert!(text.ends_with("then run \"marsa dumps on\".\n"), "{text}");
    }
}
