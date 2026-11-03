//! Every way the shim can fail, its stable code, its exit code, and its hint.

use std::error::Error as _;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use marsa_core::config::{ConfigError, ConfigPaths};
use marsa_core::resolver::{ResolveError, Tool};
use marsa_core::runtimes::{Runtime, RuntimesDir, RuntimesError};

/// Errors from `resolver`, `config`, and `runtimes` are wrapped as they are,
/// and [`ShimError::code`] gives their own code.
#[derive(Debug, thiserror::Error)]
pub enum ShimError {
    #[error("unknown command name {name:?}")]
    UnknownName { name: OsString },

    #[error("cannot read the current folder")]
    NoCwd(#[source] io::Error),

    #[error("{} is missing", .path.display())]
    ProgramMissing { path: PathBuf },

    /// `dir` is `runtimes/composer`.
    #[error("Composer is not installed: no version folder in {}", .dir.display())]
    ComposerNotInstalled { dir: PathBuf },

    #[error(transparent)]
    Resolve(#[from] ResolveError),

    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Runtimes(#[from] RuntimesError),

    #[error("stopped after {depth} nested shims; a link in runtimes/ may point back to the shim")]
    Loop { depth: u32 },

    // Only `sys/unix.rs` builds this variant (and the tests). M1-07 builds it on
    // Windows too.
    #[cfg_attr(
        all(windows, not(test)),
        expect(dead_code, reason = "the Windows stub never runs a program")
    )]
    #[error("cannot run {}", .path.display())]
    Exec {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    // Only `sys/windows.rs` builds this variant (and the tests).
    #[cfg_attr(
        all(not(windows), not(test)),
        expect(dead_code, reason = "only the Windows stub returns it")
    )]
    #[error("the shim does not run on Windows yet")]
    UnsupportedPlatform,
}

impl ShimError {
    /// Stable error name. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            ShimError::UnknownName { .. } => "shim_unknown_name",
            ShimError::NoCwd(_) => "shim_no_cwd",
            ShimError::ProgramMissing { .. } => "shim_program_missing",
            ShimError::ComposerNotInstalled { .. } => "shim_composer_not_installed",
            ShimError::Resolve(err) => err.code(),
            ShimError::Config(err) => err.code(),
            ShimError::Runtimes(err) => err.code(),
            ShimError::Loop { .. } => "shim_loop",
            ShimError::Exec { .. } => "shim_exec",
            ShimError::UnsupportedPlatform => "shim_unsupported_platform",
        }
    }

    /// Shell convention: 127 means there is nothing to run, 126 means the
    /// program was found but cannot run.
    pub fn exit_code(&self) -> u8 {
        match self {
            ShimError::Loop { .. } | ShimError::Exec { .. } | ShimError::UnsupportedPlatform => 126,
            _ => 127,
        }
    }

    /// The text after `marsa: `: the message, each underlying cause, and the code.
    pub fn line(&self) -> String {
        let mut line = self.to_string();
        let mut cause = self.source();
        while let Some(err) = cause {
            line.push_str(": ");
            line.push_str(&err.to_string());
            cause = err.source();
        }
        format!("{line} [{}]", self.code())
    }

    /// A next step, only for errors that have a clear one. All hint texts live
    /// here (AGENTS.md section 9). `data_dir` is `None` if it is unknown.
    pub fn hint(&self, data_dir: Option<&Path>) -> Option<String> {
        let runtimes = |tool: Tool| {
            let program = Runtime::from(tool).dir_name();
            let dir = data_dir.map(|d| RuntimesDir::in_dir(d).runtime_dir(tool.into()));
            let dir = dir.map_or("runtimes".into(), |d| d.display().to_string());
            format!("put {tool} builds in {dir}/<version>/bin/{program}")
        };
        match self {
            ShimError::UnknownName { .. } => Some(
                "marsa-shim runs through links named php, node, npm, npx, or composer".to_owned(),
            ),
            ShimError::ComposerNotInstalled { dir } => Some(format!(
                "put composer.phar in {}/<version>/composer.phar",
                dir.display()
            )),
            ShimError::Resolve(ResolveError::NotInstalled { tool, .. })
            | ShimError::Resolve(ResolveError::NoMatchingVersion { tool, .. }) => {
                Some(runtimes(*tool))
            }
            ShimError::Resolve(ResolveError::NoVersion { tool }) => {
                let (file, table) = match tool {
                    Tool::Php => (".php-version", "php"),
                    Tool::Node => (".nvmrc", "node"),
                };
                let global = data_dir.map_or("global.toml".into(), |d| {
                    ConfigPaths::in_dir(d).global.display().to_string()
                });
                Some(format!(
                    "add a {file} file, or set [{table}] default in {global}"
                ))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::resolver::Source;

    fn io_error() -> io::Error {
        io::Error::from(io::ErrorKind::PermissionDenied)
    }

    #[test]
    fn codes_and_exit_codes() {
        let cases = [
            (
                ShimError::UnknownName { name: "x".into() },
                "shim_unknown_name",
                127,
            ),
            (ShimError::NoCwd(io_error()), "shim_no_cwd", 127),
            (
                ShimError::ComposerNotInstalled { dir: "c".into() },
                "shim_composer_not_installed",
                127,
            ),
            (
                ShimError::ProgramMissing { path: "p".into() },
                "shim_program_missing",
                127,
            ),
            (
                ResolveError::NoVersion { tool: Tool::Php }.into(),
                "no_version",
                127,
            ),
            (ConfigError::NoHome.into(), "config_no_home", 127),
            (
                RuntimesError::Read {
                    path: "r".into(),
                    source: io_error(),
                }
                .into(),
                "runtimes_read",
                127,
            ),
            (ShimError::Loop { depth: 16 }, "shim_loop", 126),
            (
                ShimError::Exec {
                    path: "p".into(),
                    source: io_error(),
                },
                "shim_exec",
                126,
            ),
            (
                ShimError::UnsupportedPlatform,
                "shim_unsupported_platform",
                126,
            ),
        ];
        for (err, code, exit) in cases {
            assert_eq!(err.code(), code);
            assert_eq!(err.exit_code(), exit, "{code}");
        }
    }

    #[test]
    fn the_line_holds_the_cause_and_the_code() {
        let err = ShimError::Exec {
            path: "/x/php".into(),
            source: io_error(),
        };
        let line = err.line();
        assert!(line.starts_with("cannot run /x/php: "), "{line}");
        assert!(line.ends_with(" [shim_exec]"), "{line}");
    }

    #[test]
    fn hints() {
        let data = Path::new("/data");
        let not_installed = ShimError::from(ResolveError::NotInstalled {
            tool: Tool::Php,
            requested: "8.2".into(),
            origin: Source::GlobalDefault,
        });
        let expected = format!(
            "put PHP builds in {}/<version>/bin/php",
            data.join("runtimes").join("php").display()
        );
        assert_eq!(not_installed.hint(Some(data)), Some(expected));
        assert_eq!(
            not_installed.hint(None).as_deref(),
            Some("put PHP builds in runtimes/<version>/bin/php")
        );

        let no_version = ShimError::from(ResolveError::NoVersion { tool: Tool::Node });
        let hint = no_version.hint(None).expect("no_version has a hint");
        assert!(
            hint.starts_with("add a .nvmrc file, or set [node] default"),
            "{hint}"
        );

        let unknown = ShimError::UnknownName { name: "x".into() };
        let hint = unknown.hint(None).expect("shim_unknown_name has a hint");
        assert!(hint.ends_with("php, node, npm, npx, or composer"), "{hint}");

        let composer = ShimError::ComposerNotInstalled {
            dir: data.join("runtimes").join("composer"),
        };
        let expected = format!(
            "put composer.phar in {}/<version>/composer.phar",
            data.join("runtimes").join("composer").display()
        );
        assert_eq!(composer.hint(Some(data)), Some(expected));
        assert_eq!(ShimError::Loop { depth: 16 }.hint(None), None);
    }
}
