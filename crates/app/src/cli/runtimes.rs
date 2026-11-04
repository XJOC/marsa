//! `marsa runtimes [tool]`: lists the installed versions, from the daemon's
//! `GET /v1/runtimes`. No network beyond the daemon on `127.0.0.1`.

use std::path::Path;
use std::process::ExitCode;

use marsa_core::runtimes::Runtime;
use marsa_core::types::RuntimeList;

use super::daemon::{connect, failure};
use super::{Tool, strings};

/// The arguments of `marsa runtimes`.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct RuntimesArgs {
    /// Show only this program. Without it: all of them.
    #[arg(value_enum)]
    pub tool: Option<Tool>,
}

/// Starts the daemon if needed, then prints the installed versions to stdout.
pub async fn run(args: &RuntimesArgs, data_dir: &Path) -> ExitCode {
    let client = match connect(data_dir).await {
        Ok(client) => client,
        Err(failure) => return failure.report(),
    };
    match client.runtimes().await {
        Ok(list) => {
            print!("{}", render(&list, args.tool));
            ExitCode::SUCCESS
        }
        Err(err) => failure(&err, data_dir, None).report(),
    }
}

/// One line per runtime (only `tool`'s if given): its name, then its
/// versions, oldest first. A runtime the daemon did not list has none.
pub fn render(list: &RuntimeList, tool: Option<Tool>) -> String {
    let wanted: Vec<Runtime> = match tool {
        Some(tool) => vec![tool.into()],
        None => Runtime::ALL.to_vec(),
    };
    let mut text = String::new();
    for runtime in wanted {
        let versions = list
            .runtimes
            .iter()
            .find(|r| r.runtime == runtime)
            .map_or(&[][..], |r| r.installed.as_slice());
        let shown = if versions.is_empty() {
            strings::NONE_INSTALLED.to_owned()
        } else {
            // From the daemon: no control characters on the terminal.
            versions
                .iter()
                .map(|v| v.chars().filter(|c| !c.is_control()).collect::<String>())
                .collect::<Vec<_>>()
                .join(", ")
        };
        text.push_str(&format!("{:<10}{shown}\n", strings::runtime_name(runtime)));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::types::RuntimeVersions;

    fn list(found: &[(Runtime, &[&str])]) -> RuntimeList {
        RuntimeList {
            runtimes: found
                .iter()
                .map(|(runtime, versions)| RuntimeVersions {
                    runtime: *runtime,
                    installed: versions.iter().map(|v| (*v).to_owned()).collect(),
                })
                .collect(),
        }
    }

    #[test]
    fn nothing_installed_reads_none_installed() {
        let empty = list(&[
            (Runtime::Php, &[]),
            (Runtime::Node, &[]),
            (Runtime::Composer, &[]),
            (Runtime::Caddy, &[]),
        ]);
        assert_eq!(
            render(&empty, None),
            "PHP       none installed\n\
             Node.js   none installed\n\
             Composer  none installed\n\
             Caddy     none installed\n"
        );
    }

    #[test]
    fn versions_are_listed_as_the_daemon_sent_them() {
        let found = list(&[
            (Runtime::Php, &["8.3.27", "8.4.23"]),
            (Runtime::Node, &["22.11.0"]),
            (Runtime::Caddy, &["2.11.8"]),
        ]);
        assert_eq!(
            render(&found, None),
            "PHP       8.3.27, 8.4.23\n\
             Node.js   22.11.0\n\
             Composer  none installed\n\
             Caddy     2.11.8\n"
        );
        assert_eq!(render(&found, Some(Tool::Node)), "Node.js   22.11.0\n");
        assert_eq!(render(&found, Some(Tool::Caddy)), "Caddy     2.11.8\n");
        assert_eq!(
            render(
                &list(&[(Runtime::Php, &["8.4.23\u{1b}[2J"])]),
                Some(Tool::Php)
            ),
            "PHP       8.4.23[2J\n"
        );
    }
}
