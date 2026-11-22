//! Every way the shim can fail, its stable code, its exit code, and its hint.

use std::error::Error as _;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use marsa_core::config::{ConfigError, ConfigPaths};
use marsa_core::databases::ServerKind;
use marsa_core::databases::clients::{CLIENTS, SHIM_MARK};
use marsa_core::resolver::{ResolveError, Tool};
use marsa_core::runtimes::{Runtime, RuntimesError};

use crate::name::ShimName;

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

    /// A client such as `psql` with no version of its server installed.
    /// `dir` is `runtimes/<server>`.
    #[error("{} is not installed: no version folder in {}", .server.name(), .dir.display())]
    ServerNotInstalled { server: ServerKind, dir: PathBuf },

    /// `[<server>] default` in `global.toml` names a line with no installed
    /// version.
    #[error(
        "{} {line} is not installed (asked for by [{}] default in global.toml)",
        .server.name(),
        .server.id()
    )]
    ServerLineNotInstalled { server: ServerKind, line: String },

    #[error(transparent)]
    Resolve(#[from] ResolveError),

    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Runtimes(#[from] RuntimesError),

    #[error("stopped after {depth} nested shims; a link in runtimes/ may point back to the shim")]
    Loop { depth: u32 },

    #[error("cannot run {}", .path.display())]
    Exec {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// A system Marsa has no builds for.
    #[error("the shim does not run on this system yet")]
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
            ShimError::ServerNotInstalled { .. } => "shim_server_not_installed",
            ShimError::ServerLineNotInstalled { .. } => "shim_server_line_not_installed",
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
        let program = |tool: &Tool| Runtime::from(*tool).dir_name();
        match self {
            ShimError::UnknownName { .. } => Some(names_hint()),
            ShimError::ComposerNotInstalled { .. } => {
                Some("run \"marsa install composer\"".to_owned())
            }
            ShimError::ServerNotInstalled { server, .. } => {
                Some(format!("run \"marsa install {}\"", server.id()))
            }
            ShimError::ServerLineNotInstalled { server, line } => Some(format!(
                "run \"marsa install {} {line}\", or change [{}] default",
                server.id(),
                server.id()
            )),
            ShimError::Resolve(ResolveError::NotInstalled {
                tool, requested, ..
            }) => Some(format!(
                "run \"marsa install {} {requested}\"",
                program(tool)
            )),
            ShimError::Resolve(ResolveError::NoMatchingVersion {
                tool, constraint, ..
            }) => Some(format!(
                "install a {tool} version that satisfies {constraint}: \"marsa install {} <version>\"",
                program(tool)
            )),
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

/// Every name the shim answers to, after [`SHIM_MARK`]: the mark is
/// printed whole, so every build of the shim holds it.
fn names_hint() -> String {
    let tools = ShimName::ALL.map(ShimName::name);
    let clients = CLIENTS.map(|client| client.name);
    let mut names: Vec<&str> = tools.into_iter().chain(clients).collect();
    let last = names.pop().unwrap_or_default();
    format!("{SHIM_MARK}{}, or {last}", names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::data_dir::DataDir;
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
                ShimError::ServerNotInstalled {
                    server: ServerKind::Postgres,
                    dir: "r".into(),
                },
                "shim_server_not_installed",
                127,
            ),
            (
                ShimError::ServerLineNotInstalled {
                    server: ServerKind::Mysql,
                    line: "8.0".into(),
                },
                "shim_server_line_not_installed",
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
        assert_eq!(
            not_installed.hint(Some(data)).as_deref(),
            Some("run \"marsa install php 8.2\"")
        );

        let no_match = ShimError::from(ResolveError::NoMatchingVersion {
            tool: Tool::Node,
            constraint: ">=22".into(),
            origin: Source::GlobalDefault,
        });
        assert_eq!(
            no_match.hint(None).as_deref(),
            Some("install a Node version that satisfies >=22: \"marsa install node <version>\"")
        );

        let no_version = ShimError::from(ResolveError::NoVersion { tool: Tool::Node });
        let hint = no_version.hint(None).expect("no_version has a hint");
        assert!(
            hint.starts_with("add a .nvmrc file, or set [node] default"),
            "{hint}"
        );

        let unknown = ShimError::UnknownName { name: "x".into() };
        let hint = unknown.hint(None).expect("shim_unknown_name has a hint");
        assert_eq!(
            hint,
            "marsa-shim runs through links named php, node, npm, npx, composer, psql, \
             pg_dump, pg_restore, mysql, mysqldump, mysqladmin, valkey-cli, or redis-cli"
        );
        assert!(hint.starts_with(SHIM_MARK), "{hint}");

        let composer = ShimError::ComposerNotInstalled {
            dir: DataDir::new(data).runtimes().join("composer"),
        };
        assert_eq!(
            composer.hint(Some(data)).as_deref(),
            Some("run \"marsa install composer\"")
        );
        assert_eq!(ShimError::Loop { depth: 16 }.hint(None), None);
    }

    /// A client with no server installed names the install, as Composer
    /// does; a `default` line with no version names that line.
    #[test]
    fn server_errors_name_the_install() {
        let dir = Path::new("/data").join("runtimes").join("postgres");
        let missing = ShimError::ServerNotInstalled {
            server: ServerKind::Postgres,
            dir: dir.clone(),
        };
        let want = format!(
            "PostgreSQL is not installed: no version folder in {} [shim_server_not_installed]",
            dir.display()
        );
        assert_eq!(missing.line(), want);
        assert_eq!(
            missing.hint(None).as_deref(),
            Some("run \"marsa install postgres\"")
        );

        let line = ShimError::ServerLineNotInstalled {
            server: ServerKind::Valkey,
            line: "9.0".into(),
        };
        assert_eq!(
            line.line(),
            "Valkey 9.0 is not installed (asked for by [valkey] default in global.toml) \
             [shim_server_line_not_installed]"
        );
        assert_eq!(
            line.hint(None).as_deref(),
            Some("run \"marsa install valkey 9.0\", or change [valkey] default")
        );
    }
}
