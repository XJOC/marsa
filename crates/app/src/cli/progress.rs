//! Turns install events into progress text for stderr. No I/O here: the
//! caller writes what [`Progress::render`] returns, so tests compare strings.
//!
//! On a terminal, the line of a download is rewritten in place with `\r`.
//! Elsewhere (CI, a log file) each download prints a line at 25%, 50%, 75%,
//! and 100%, so the log stays short.

use marsa_core::runtimes::Runtime;
use marsa_core::types::InstallEvent;

use super::strings;

/// Width of the `[####      ]` bar, in characters.
const BAR_WIDTH: u64 = 10;

/// The state between events: which install it is, and the line of the
/// download in progress.
#[derive(Debug)]
pub struct Progress {
    terminal: bool,
    platform: String,
    runtime: Option<Runtime>,
    version: Option<String>,
    file: Option<FileLine>,
}

#[derive(Debug)]
struct FileLine {
    artifact: String,
    total: u64,
    /// Plain mode: the last quarter printed (0 to 4).
    quarter: u64,
    /// Terminal mode: the length of the line on screen, which has no `\n` yet.
    open: Option<usize>,
}

impl Progress {
    /// `terminal`: stderr is a terminal. `platform` is shown in the
    /// "Installing" line, for example `darwin-arm64`.
    pub fn new(terminal: bool, platform: impl Into<String>) -> Self {
        Progress {
            terminal,
            platform: platform.into(),
            runtime: None,
            version: None,
            file: None,
        }
    }

    /// The text to write for `event`. May be empty.
    pub fn render(&mut self, event: &InstallEvent) -> String {
        match event {
            InstallEvent::ManifestVerified { serial } => {
                self.line(&strings::manifest_verified(*serial))
            }
            InstallEvent::Selected { runtime, version } => {
                self.runtime = Some(*runtime);
                self.version = Some(version.clone());
                self.line(&strings::installing(*runtime, version, &self.platform))
            }
            InstallEvent::Downloading {
                artifact,
                done,
                total,
            } => self.downloading(artifact, *done, *total),
            InstallEvent::Verified { artifact } => {
                let size = match &self.file {
                    Some(file) if &file.artifact == artifact => {
                        format!("{} {}, ", bar(file.total, file.total), mb(file.total))
                    }
                    _ => String::new(),
                };
                let text = format!("{} {size}{}", label(artifact), strings::CHECKSUM_OK);
                self.end_file(&text)
            }
            InstallEvent::Extracting { artifact } => {
                self.line(&format!("{} {}", label(artifact), strings::UNPACKING))
            }
            InstallEvent::Checking => {
                let text = strings::checking(self.runtime, self.version.as_deref());
                self.line(&text)
            }
            // The caller prints the result on stdout.
            InstallEvent::Installed { .. } => self.finish(),
        }
    }

    /// Ends a line left open on the terminal, so the next text starts on a
    /// line of its own. Call it before printing an error.
    pub fn finish(&mut self) -> String {
        match self.file.as_mut().and_then(|file| file.open.take()) {
            Some(_) => "\n".to_owned(),
            None => String::new(),
        }
    }

    /// `text` on a line of its own.
    fn line(&mut self, text: &str) -> String {
        format!("{}{text}\n", self.finish())
    }

    fn downloading(&mut self, artifact: &str, done: u64, total: u64) -> String {
        let mut text = String::new();
        if self.file.as_ref().is_none_or(|f| f.artifact != artifact) {
            text.push_str(&self.finish());
            self.file = Some(FileLine {
                artifact: artifact.to_owned(),
                total,
                quarter: 0,
                open: None,
            });
        }
        let Some(file) = self.file.as_mut() else {
            return text;
        };
        file.total = total;
        let shown = format!(
            "{} {} {} of {}",
            label(artifact),
            bar(done, total),
            mb(done),
            mb(total)
        );
        if self.terminal {
            text.push_str(&overwrite(&mut file.open, &shown));
        } else {
            let quarter = fraction(done, total, 4);
            if quarter > file.quarter {
                file.quarter = quarter;
                text.push_str(&format!("{shown} ({}%)\n", quarter * 25));
            }
        }
        text
    }

    /// The last line of a file: rewrites the open line on a terminal.
    fn end_file(&mut self, text: &str) -> String {
        match self.file.as_mut() {
            Some(file) if self.terminal && file.open.is_some() => {
                let mut out = overwrite(&mut file.open, text);
                file.open = None;
                out.push('\n');
                out
            }
            _ => self.line(text),
        }
    }
}

/// `\r` and `text`, padded with spaces to cover a longer line on screen.
fn overwrite(open: &mut Option<usize>, text: &str) -> String {
    let len = text.chars().count();
    let pad = open.map_or(0, |before| before.saturating_sub(len));
    *open = Some(len);
    format!("\r{text}{}", " ".repeat(pad))
}

/// The artifact name in a fixed-width column.
fn label(artifact: &str) -> String {
    format!("  {artifact:<8}")
}

fn bar(done: u64, total: u64) -> String {
    let filled = fraction(done, total, BAR_WIDTH);
    let empty = BAR_WIDTH - filled;
    format!(
        "[{}{}]",
        "#".repeat(filled as usize),
        " ".repeat(empty as usize)
    )
}

/// `done / total` in `parts`, rounded down. An empty file is complete.
fn fraction(done: u64, total: u64, parts: u64) -> u64 {
    if total == 0 {
        return parts;
    }
    (done.min(total).saturating_mul(parts) / total).min(parts)
}

/// Bytes as megabytes (10^6), one decimal.
fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn downloading(done: u64, total: u64) -> InstallEvent {
        InstallEvent::Downloading {
            artifact: "cli".into(),
            done,
            total,
        }
    }

    fn start() -> [InstallEvent; 2] {
        [
            InstallEvent::ManifestVerified { serial: 3 },
            InstallEvent::Selected {
                runtime: Runtime::Php,
                version: "8.4.23".into(),
            },
        ]
    }

    fn end() -> [InstallEvent; 4] {
        [
            InstallEvent::Verified {
                artifact: "cli".into(),
            },
            InstallEvent::Extracting {
                artifact: "cli".into(),
            },
            InstallEvent::Checking,
            InstallEvent::Installed {
                dir: PathBuf::from("/x"),
            },
        ]
    }

    fn render(terminal: bool, events: &[InstallEvent]) -> String {
        let mut progress = Progress::new(terminal, "darwin-arm64");
        events.iter().map(|e| progress.render(e)).collect()
    }

    #[test]
    fn plain_output_prints_each_quarter_once() {
        let total = 36_200_000;
        let mut events = start().to_vec();
        for done in [
            0, 1_000_000, 9_050_000, 9_100_000, 20_000_000, 30_000_000, total,
        ] {
            events.push(downloading(done, total));
        }
        events.extend(end());
        assert_eq!(
            render(false, &events),
            "Verified the version list (serial 3)\n\
             Installing PHP 8.4.23 for darwin-arm64\n\
             \x20 cli      [##        ] 9.1 MB of 36.2 MB (25%)\n\
             \x20 cli      [#####     ] 20.0 MB of 36.2 MB (50%)\n\
             \x20 cli      [########  ] 30.0 MB of 36.2 MB (75%)\n\
             \x20 cli      [##########] 36.2 MB of 36.2 MB (100%)\n\
             \x20 cli      [##########] 36.2 MB, sha256 ok\n\
             \x20 cli      unpacking\n\
             Checking PHP 8.4.23...\n"
        );
    }

    #[test]
    fn terminal_output_rewrites_the_line() {
        let mut events = start().to_vec();
        events.push(downloading(0, 2_000_000));
        events.push(downloading(1_000_000, 2_000_000));
        events.push(downloading(2_000_000, 2_000_000));
        events.extend(end());
        assert_eq!(
            render(true, &events),
            "Verified the version list (serial 3)\n\
             Installing PHP 8.4.23 for darwin-arm64\n\
             \r  cli      [          ] 0.0 MB of 2.0 MB\
             \r  cli      [#####     ] 1.0 MB of 2.0 MB\
             \r  cli      [##########] 2.0 MB of 2.0 MB\
             \r  cli      [##########] 2.0 MB, sha256 ok\n\
             \x20 cli      unpacking\n\
             Checking PHP 8.4.23...\n"
        );
    }

    #[test]
    fn finish_ends_an_open_line_once() {
        let mut progress = Progress::new(true, "linux-x64");
        progress.render(&downloading(5, 10));
        assert_eq!(progress.finish(), "\n");
        assert_eq!(progress.finish(), "");
        // A second file starts on a new line, too.
        let mut progress = Progress::new(true, "linux-x64");
        progress.render(&downloading(5, 10));
        let next = progress.render(&InstallEvent::Downloading {
            artifact: "fpm".into(),
            done: 0,
            total: 10,
        });
        assert!(next.starts_with("\n\r  fpm "), "{next:?}");
    }

    #[test]
    fn a_shorter_line_covers_the_longer_one() {
        let mut open = Some(10);
        assert_eq!(overwrite(&mut open, "abc"), "\rabc       ");
        assert_eq!(open, Some(3));
        assert_eq!(overwrite(&mut open, "abcdef"), "\rabcdef");
    }

    #[test]
    fn an_empty_file_counts_as_complete() {
        assert_eq!(fraction(0, 0, 4), 4);
        assert_eq!(fraction(20, 10, 4), 4);
        assert_eq!(bar(0, 0), "[##########]");
    }
}
