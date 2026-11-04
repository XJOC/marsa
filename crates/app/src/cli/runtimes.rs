//! `marsa runtimes [tool]`: lists the installed versions. Reads the
//! `runtimes/` folder only, no network.

use std::path::Path;
use std::process::ExitCode;

use marsa_core::runtimes::{Runtime, RuntimesDir, RuntimesError};

use super::{Failure, Tool, strings};

/// The arguments of `marsa runtimes`.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct RuntimesArgs {
    /// Show only this program. Without it: all of them.
    #[arg(value_enum)]
    pub tool: Option<Tool>,
}

const ALL: [Runtime; 3] = [Runtime::Php, Runtime::Node, Runtime::Composer];

/// Prints the installed versions to stdout.
pub fn run(args: &RuntimesArgs, data_dir: &Path) -> ExitCode {
    match list(args, data_dir) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(failure) => failure.report(),
    }
}

/// The text `marsa runtimes` prints.
///
/// # Errors
///
/// `runtimes_read` if a runtime folder exists but cannot be read.
pub fn list(args: &RuntimesArgs, data_dir: &Path) -> Result<String, Failure> {
    let dir = RuntimesDir::in_dir(data_dir);
    let wanted: Vec<Runtime> = match args.tool {
        Some(tool) => vec![tool.into()],
        None => ALL.to_vec(),
    };
    let mut found = Vec::with_capacity(wanted.len());
    for runtime in wanted {
        let versions = dir.installed(runtime).map_err(|err| {
            tracing::warn!(error = %err, "cannot list runtimes");
            let RuntimesError::Read { path, .. } = &err;
            let details = strings::Details {
                runtime: Some(runtime),
                path: Some(path),
                ..strings::Details::default()
            };
            Failure {
                code: err.code(),
                message: strings::message(err.code(), &details),
            }
        })?;
        found.push((runtime, versions.iter().map(ToString::to_string).collect()));
    }
    Ok(render(&found))
}

/// One line per runtime: its name, then its versions, oldest first.
pub fn render(found: &[(Runtime, Vec<String>)]) -> String {
    let mut text = String::new();
    for (runtime, versions) in found {
        let list = if versions.is_empty() {
            strings::NONE_INSTALLED.to_owned()
        } else {
            versions.join(", ")
        };
        text.push_str(&format!("{:<10}{list}\n", strings::runtime_name(*runtime)));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn args(tool: Option<Tool>) -> RuntimesArgs {
        RuntimesArgs { tool }
    }

    #[test]
    fn an_empty_data_dir_lists_nothing_installed() {
        let tmp = tempfile::tempdir().expect("temp dir");
        assert_eq!(
            list(&args(None), tmp.path()).expect("list"),
            "PHP       none installed\n\
             Node.js   none installed\n\
             Composer  none installed\n"
        );
    }

    #[test]
    fn versions_are_listed_oldest_first() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runtimes = tmp.path().join("runtimes");
        for dir in [
            "php/8.4.23",
            "php/8.3.27",
            "php/8.4",
            "node/22.11.0",
            ".tmp",
        ] {
            fs::create_dir_all(runtimes.join(dir)).expect("mkdir");
        }
        // A file named like a version is not a version folder.
        fs::create_dir_all(runtimes.join("composer")).expect("mkdir");
        fs::write(runtimes.join("composer").join("2.8.4"), "").expect("write");

        assert_eq!(
            list(&args(None), tmp.path()).expect("list"),
            "PHP       8.3.27, 8.4.23\n\
             Node.js   22.11.0\n\
             Composer  none installed\n"
        );
        assert_eq!(
            list(&args(Some(Tool::Node)), tmp.path()).expect("list"),
            "Node.js   22.11.0\n"
        );
    }

    // `cfg(unix)` outside `platform`: Unix file modes are the simplest way to
    // make a folder unreadable in a test.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_is_runtimes_read() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("temp dir");
        let php = tmp.path().join("runtimes").join("php");
        fs::create_dir_all(&php).expect("mkdir");
        fs::set_permissions(&php, fs::Permissions::from_mode(0o000)).expect("chmod");
        let result = list(&args(Some(Tool::Php)), tmp.path());
        fs::set_permissions(&php, fs::Permissions::from_mode(0o755)).expect("chmod back");
        // Root can read anything; then there is no error to check.
        if let Err(failure) = result {
            assert_eq!(failure.code, "runtimes_read");
            assert!(
                failure.message.contains("Cannot read"),
                "{}",
                failure.message
            );
        }
    }
}
