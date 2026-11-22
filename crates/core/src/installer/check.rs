//! The recipe's check program, run after an install: the version it prints
//! must be the one just installed.

use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use semver::Version;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use super::InstallError;
use crate::recipes::{Check, PlatformRecipe};

pub(super) const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
/// The most of each output stream of the check that is read.
const CHECK_OUTPUT_MAX: u64 = 64 * 1024;
/// The most characters of the check's output put in an error.
pub(super) const CHECK_OUTPUT_SHOWN: usize = 500;
/// `STATUS_DLL_NOT_FOUND`, the exit code of a Windows program that misses a
/// DLL. For PHP and Node that is the Visual C++ runtime. Never a Unix code.
const STATUS_DLL_NOT_FOUND: i32 = 0xC000_0135_u32.cast_signed();
/// `STATUS_ENTRYPOINT_NOT_FOUND`: the DLL is there but lacks a function, as
/// a Visual C++ runtime older than the program's build tools does (ADR
/// 0019: PHP 8.4 needs 14.44 or later). A Windows too old for the program
/// gives it too; Marsa supports none, so the runtime is the likely cause.
const STATUS_ENTRYPOINT_NOT_FOUND: i32 = 0xC000_0139_u32.cast_signed();
/// Variables that would change how the check program starts or which
/// libraries it loads. The rest of the environment stays: Windows programs
/// need `SystemRoot`.
pub(super) const CHECK_ENV_REMOVED: [&str; 7] = [
    "PHPRC",
    "PHP_INI_SCAN_DIR",
    "NODE_OPTIONS",
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "DYLD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
];

/// Runs the recipe's check program from `root` by its full path: no `PATH`
/// search, no shell. It must exit with success, and its output must start
/// with the expected text.
pub(super) async fn run_check(
    check: &Check,
    programs: &PlatformRecipe,
    root: &Path,
    version: &Version,
    timeout: Duration,
) -> Result<(), InstallError> {
    let failed = |output: String, exit_code| InstallError::CheckFailed { output, exit_code };
    // The recipe parser checks that the program is in `programs`.
    let program = programs
        .program_path(root, &check.program)
        .ok_or_else(|| failed(format!("no program {:?}", check.program), None))?;
    // The app data dir is absolute, so `Command` never searches `PATH`.
    debug_assert!(program.is_absolute(), "{}", program.display());
    let mut command = Command::new(&program);
    command.args(&check.args);
    clean_check_env(&mut command);

    let (status, stdout, stderr) = match run_capped(command, timeout).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            return Err(failed(
                format!("cannot run {}: {err}", program.display()),
                None,
            ));
        }
        Err(_) => {
            return Err(failed(
                format!("no answer after {} ms", timeout.as_millis()),
                None,
            ));
        }
    };

    if matches!(
        status.code(),
        Some(STATUS_DLL_NOT_FOUND | STATUS_ENTRYPOINT_NOT_FOUND)
    ) {
        return Err(InstallError::MissingVcRuntime);
    }
    let stdout = String::from_utf8_lossy(&stdout);
    if status.success() && shows_version(&stdout, check, version) {
        return Ok(());
    }
    let text = format!("{stdout}{}", String::from_utf8_lossy(&stderr));
    Err(failed(
        text.chars().take(CHECK_OUTPUT_SHOWN).collect(),
        status.code(),
    ))
}

/// Whether `stdout` shows `version` as `check` expects it: at its start
/// ([`version_matches`]), or after its start, at any place where the
/// expected text is followed by no digit.
fn shows_version(stdout: &str, check: &Check, version: &Version) -> bool {
    if let Some(prefix) = check.expected_prefix(version) {
        return version_matches(stdout, &prefix);
    }
    check.expected_contains(version).is_some_and(|text| {
        stdout
            .match_indices(text.as_str())
            .any(|(at, _)| version_matches(&stdout[at..], &text))
    })
}

/// Whether `stdout` starts with `prefix` and the version ends there: the
/// next character is not a digit. So an expected `17.1` never accepts a
/// program that prints `17.12`, whatever the recipe's prefix ends with.
fn version_matches(stdout: &str, prefix: &str) -> bool {
    stdout
        .strip_prefix(prefix)
        .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_digit()))
}

pub(super) fn clean_check_env(command: &mut Command) {
    for name in CHECK_ENV_REMOVED {
        command.env_remove(name);
    }
}

type CheckOutput = (std::process::ExitStatus, Vec<u8>, Vec<u8>);

/// Runs `command` with no input, reads at most [`CHECK_OUTPUT_MAX`] bytes of
/// each output, and waits at most `timeout`. On a timeout the future is
/// dropped, and `kill_on_drop` stops the child.
pub(super) async fn run_capped(
    mut command: Command,
    timeout: Duration,
) -> Result<io::Result<CheckOutput>, tokio::time::error::Elapsed> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = async {
        let mut child = command.spawn()?;
        let (stdout, stderr) = tokio::join!(
            read_capped(child.stdout.take()),
            read_capped(child.stderr.take())
        );
        let status = child.wait().await?;
        Ok((status, stdout?, stderr?))
    };
    tokio::time::timeout(timeout, output).await
}

/// Reads at most [`CHECK_OUTPUT_MAX`] bytes, then closes the pipe: a program
/// that keeps writing then gets a broken pipe and stops.
async fn read_capped(pipe: Option<impl AsyncRead + Unpin>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if let Some(pipe) = pipe {
        pipe.take(CHECK_OUTPUT_MAX).read_to_end(&mut bytes).await?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipes;

    #[test]
    fn the_version_ends_where_the_prefix_does() {
        let pg = "postgres (PostgreSQL) 17.1";
        assert!(version_matches("postgres (PostgreSQL) 17.1\n", pg));
        assert!(version_matches("postgres (PostgreSQL) 17.1", pg));
        assert!(!version_matches("postgres (PostgreSQL) 17.12\n", pg));
        assert!(!version_matches("postgres (PostgreSQL) 17.10", pg));
        assert!(!version_matches("postgres (PostgreSQL) 16.1\n", pg));
        assert!(version_matches("PHP 8.4.1 (cli)", "PHP 8.4.1 "));
        assert!(!version_matches("PHP 8.4.12 (cli)", "PHP 8.4.1"));
        assert!(version_matches("v22.11.0\r\n", "v22.11.0"));
    }

    /// `mysqld --version` prints its own path first.
    #[test]
    fn a_version_inside_the_output_ends_where_the_text_does() {
        let text = "name = \"mysql\"\n[check]\nprogram = \"mysqld\"\nargs = [\"--version\"]\n\
                    expect_contains = \" Ver {version}\"\n[platform.darwin-arm64]\n\
                    programs = { mysqld = \"bin/mysqld\" }\n[[platform.darwin-arm64.artifact]]\n\
                    name = \"a\"\nurl = \"https://example.test/a.tar.gz\"\nformat = \"tar.gz\"\n";
        let recipe = recipes::parse("mysql.toml", text).unwrap();
        let check = recipe.check().unwrap();
        let version = Version::new(8, 4, 1);
        let shows = |stdout: &str| shows_version(stdout, check, &version);
        let path = "/Users/a/Library/Application Support/dev.marsa.Marsa/runtimes/mysql/8.4.1";
        assert!(shows(&format!(
            "{path}/bin/mysqld  Ver 8.4.1 for macos15 on arm64 (MySQL Community Server - GPL)\n"
        )));
        assert!(!shows(&format!(
            "{path}/bin/mysqld  Ver 8.4.13 for macos15 on arm64\n"
        )));
        // Any place counts: a path with ` Ver 8.4.13` in it is passed over.
        assert!(shows("/x Ver 8.4.13/mysqld  Ver 8.4.1 for linux\n"));
        assert!(!shows("mysqld  Ver 8.4.10 for linux\n"));
        assert!(!shows(""));
    }

    #[test]
    fn the_missing_dll_code_is_the_windows_status() {
        assert_eq!(STATUS_DLL_NOT_FOUND, -1_073_741_515);
        assert_eq!(STATUS_ENTRYPOINT_NOT_FOUND, -1_073_741_511);
    }
}
