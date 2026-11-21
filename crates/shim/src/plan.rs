//! What to run: the real program, an optional script, the user's arguments,
//! at most one variable that makes the program trust Marsa's CA, and for a
//! database client the address of Marsa's server. Built without touching
//! the disk or the environment, so every case is tested without `exec`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use marsa_core::caddy::CaddyPaths;
use marsa_core::databases::ServerKind;
use marsa_core::databases::clients::Client;
use marsa_core::php::PhpFiles;
use marsa_core::recipes::Platform;
use marsa_core::resolver::Tool;
use marsa_core::runtimes::{Runtime, RuntimesDir};
use semver::Version;

use crate::depth;
use crate::error::ShimError;
use crate::name::ShimName;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Always absolute, so starting it never searches `PATH`.
    pub program: PathBuf,
    /// Passed to `program` before the user's arguments: `npm-cli.js`,
    /// `npx-cli.js`, or `composer.phar`.
    pub script: Option<PathBuf>,
    pub args: Vec<OsString>,
    /// What the command does with one variable of the environment.
    /// [`Plan::new`] leaves it `None`; the caller sets it from
    /// [`TrustVar::decide`].
    pub trust: Option<TrustEnv>,
    /// Variables the command sets for a database client: those of
    /// [`server_env`] that the user has not set ([`unset_only`]). Empty from
    /// [`Plan::new`] and [`Plan::client`].
    pub server_env: Vec<(&'static str, String)>,
}

/// The host of every server Marsa runs: they listen on loopback only.
const SERVER_HOST: &str = "127.0.0.1";

/// The variables that point a client of `server` at Marsa's server on
/// `port`, read by the client when no argument says otherwise:
///
/// - PostgreSQL: `PGHOST`, `PGPORT`, and `PGUSER=root`, the role Marsa
///   makes for Laravel's default settings. Over TCP: the socket in `run/`
///   is the superuser's alone.
/// - MySQL: `MYSQL_HOST` and `MYSQL_TCP_PORT`, so the client uses TCP and
///   not its default socket in `/tmp`, which is not Marsa's.
/// - Valkey: none. `valkey-cli` reads no such variable: [`valkey_args`] gives
///   its port as an argument instead.
pub fn server_env(server: ServerKind, port: u16) -> Vec<(&'static str, String)> {
    let (host, port_var, user) = match server {
        ServerKind::Postgres => ("PGHOST", "PGPORT", Some(("PGUSER", "root"))),
        ServerKind::Mysql => ("MYSQL_HOST", "MYSQL_TCP_PORT", None),
        ServerKind::Valkey => return Vec::new(),
    };
    let mut vars = vec![(host, SERVER_HOST.to_owned()), (port_var, port.to_string())];
    vars.extend(user.map(|(name, value)| (name, value.to_owned())));
    vars
}

/// The arguments of `valkey-cli` (and `redis-cli`, which runs it): `-p <port>`
/// first when Marsa's Valkey is on another port than Valkey's own 6379, and
/// the user's `args` as they are. Nothing is added when the user already says
/// where to connect, in any spelling: a port, host, URI, or socket option.
/// A lookalike value after a command (`SET -h x`) only makes this add nothing,
/// which is how the client behaved before.
pub fn valkey_args(port: u16, args: Vec<OsString>) -> Vec<OsString> {
    if port == ServerKind::Valkey.default_port() || args.iter().any(|arg| names_a_target(arg)) {
        return args;
    }
    let mut all = vec![OsString::from("-p"), OsString::from(port.to_string())];
    all.extend(args);
    all
}

/// Whether `arg` is a `valkey-cli` option that says where to connect:
/// `-h`, `-p`, `-u`, or `-s` (alone or with their value joined, as in
/// `-p6380`), and `--host`, `--port`, `--uri`, or `--socket` (alone or as
/// `--port=6380`).
fn names_a_target(arg: &OsStr) -> bool {
    let Some(arg) = arg.to_str() else {
        // Not UTF-8: not an option of ours. A path or a value.
        return false;
    };
    if let Some(long) = arg.strip_prefix("--") {
        let name = long.split_once('=').map_or(long, |(name, _)| name);
        return matches!(name, "host" | "port" | "uri" | "socket");
    }
    arg.strip_prefix('-')
        .and_then(|short| short.chars().next())
        .is_some_and(|letter| matches!(letter, 'h' | 'p' | 'u' | 's'))
}

/// The variables of `vars` that the user has not set. A variable the user
/// set, even to an empty value, is never changed: the client then reads the
/// user's value from the environment it inherits. `is_set` reads the
/// environment.
pub fn unset_only(
    vars: Vec<(&'static str, String)>,
    is_set: impl Fn(&str) -> bool,
) -> Vec<(&'static str, String)> {
    vars.into_iter().filter(|(name, _)| !is_set(name)).collect()
}

/// The version of `server` that its clients run: the newest installed version
/// of the `default` line, or of all lines when `default` is not set, as
/// `marsad` picks the version it starts. No project file counts: one server
/// serves every project.
///
/// # Errors
///
/// `shim_server_not_installed` when no version is installed, and
/// `shim_server_line_not_installed` when none is in the `default` line.
pub fn server_version(
    server: ServerKind,
    default: Option<&str>,
    installed: &[Version],
    runtimes: &RuntimesDir,
) -> Result<Version, ShimError> {
    if let Some(version) = server.line_version(default, installed) {
        return Ok(version);
    }
    match default {
        Some(line) if !installed.is_empty() => Err(ShimError::ServerLineNotInstalled {
            server,
            line: line.trim().to_owned(),
        }),
        _ => Err(ShimError::ServerNotInstalled {
            server,
            dir: runtimes.runtime_dir(server.runtime()),
        }),
    }
}

/// Node adds the certificates in this file to its own list.
pub const NODE_EXTRA_CA_CERTS: &str = "NODE_EXTRA_CA_CERTS";

/// PHP reads its `php.ini` from this path.
pub const PHPRC: &str = "PHPRC";

/// A variable that makes the real program trust Marsa's CA, so a call from
/// the terminal to a `.test` site passes TLS (architecture.md section 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustVar {
    /// [`NODE_EXTRA_CA_CERTS`] or [`PHPRC`].
    pub name: &'static str,
    /// The file the variable names. It may not exist yet.
    pub file: PathBuf,
    /// A value inside this folder was set by an outer shim, not by the user:
    /// `php/` for [`PHPRC`]. `None` for [`NODE_EXTRA_CA_CERTS`]: every Node
    /// version uses the same root, so any value is the user's.
    pub marsa_dir: Option<PathBuf>,
}

/// Where the value that the shim inherited for a [`TrustVar`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inherited {
    /// The variable is not in the environment.
    Unset,
    /// The user set it, maybe to an empty value. It is never touched.
    User,
    /// An outer shim set it: for example `php` on 8.4 that runs `php` in a
    /// project on 8.3. It may name the `php.ini` of another PHP line.
    Marsa,
}

/// What the command does with the variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustEnv {
    /// Sets `name` to `file`.
    Set { name: &'static str, file: PathBuf },
    /// Removes `name`, so the program does not read the file of another PHP
    /// line.
    Remove { name: &'static str },
}

impl TrustVar {
    /// The variable for `name`. The paths come from `marsa-core`, so the
    /// shim and the daemon always agree:
    ///
    /// - `node`, `npm`, `npx`: [`NODE_EXTRA_CA_CERTS`] =
    ///   [`CaddyPaths::ca_root`], Marsa's root certificate.
    /// - `php`, `composer`: [`PHPRC`] = `php/<minor>/php.ini` from
    ///   [`PhpFiles::new`], the file the daemon writes for this PHP line. PHP
    ///   in the terminal then reads Marsa's config, with its `curl.cainfo`.
    ///
    /// `version` is the version that runs; only a PHP version is used. Nothing
    /// is read from the disk.
    pub fn for_shim(name: ShimName, data_dir: &Path, version: &Version) -> Self {
        match name.tool() {
            Tool::Node => Self {
                name: NODE_EXTRA_CA_CERTS,
                file: CaddyPaths::in_dir(data_dir).ca_root(),
                marsa_dir: None,
            },
            Tool::Php => {
                let file = PhpFiles::new(data_dir, version).ini_file;
                // The file is `php/<minor>/php.ini`, so two levels up is `php/`.
                let marsa_dir = file.parent().and_then(Path::parent).map(Path::to_path_buf);
                Self {
                    name: PHPRC,
                    file,
                    marsa_dir,
                }
            }
        }
    }

    /// Where `value`, the inherited value of the variable, came from.
    ///
    /// A path inside [`TrustVar::marsa_dir`] is Marsa's. `Path::starts_with`
    /// compares whole path parts and never resolves links, so `/data/phpx` is
    /// not inside `/data/php`. Any other value, an empty one too, is the
    /// user's.
    pub fn inherited(&self, value: Option<&OsStr>) -> Inherited {
        let marsa = |value: &OsStr| {
            self.marsa_dir
                .as_deref()
                .is_some_and(|dir| Path::new(value).starts_with(dir))
        };
        match value {
            None => Inherited::Unset,
            Some(value) if marsa(value) => Inherited::Marsa,
            Some(_) => Inherited::User,
        }
    }

    /// What the command does with the variable:
    ///
    /// - unset, and the file exists: set it.
    /// - set by the user: nothing. The user's value is never replaced.
    /// - set by Marsa, and the file exists: set it, so it names this PHP
    ///   line's file.
    /// - set by Marsa, and the file is missing: remove it.
    /// - unset, and the file is missing: nothing. Node prints a warning for a
    ///   missing file, and PHP keeps its usual search for `php.ini`.
    ///
    /// Pure, so every case is tested: the caller reads the environment and
    /// the disk.
    pub fn decide(self, inherited: Inherited, file_exists: bool) -> Option<TrustEnv> {
        match (inherited, file_exists) {
            (Inherited::User, _) | (Inherited::Unset, false) => None,
            (Inherited::Unset | Inherited::Marsa, true) => Some(TrustEnv::Set {
                name: self.name,
                file: self.file,
            }),
            (Inherited::Marsa, false) => Some(TrustEnv::Remove { name: self.name }),
        }
    }
}

impl Plan {
    /// `version` is the PHP or Node version. `composer` is the highest
    /// installed Composer version; only `composer` uses it.
    ///
    /// Both must come from [`RuntimesDir::installed`], never from user text, so
    /// a value like `../../x` cannot reach the path.
    ///
    /// `npm` and `npx` run through `node`, not through `bin/npm`: that file
    /// starts with `#!/usr/bin/env node`, which searches `PATH` and may find the
    /// shim again or another `node` (and on Windows `npm.cmd` would need
    /// `cmd.exe`).
    ///
    /// The paths follow the archives of `platform` ([`Layout`]): `bin/php` on
    /// macOS and Linux, `php.exe` on Windows.
    ///
    /// # Errors
    ///
    /// `shim_composer_not_installed` for `composer` when `composer` is `None`.
    pub fn new(
        name: ShimName,
        platform: Platform,
        runtimes: &RuntimesDir,
        version: &Version,
        composer: Option<&Version>,
        user_args: impl IntoIterator<Item = OsString>,
    ) -> Result<Self, ShimError> {
        let runtime = name.runtime();
        let layout = Layout::of(platform);
        let version_dir = runtimes.version_dir(runtime, version);
        let script = match name {
            ShimName::Php | ShimName::Node => None,
            ShimName::Npm => Some(layout.npm_bin(&version_dir).join("npm-cli.js")),
            ShimName::Npx => Some(layout.npm_bin(&version_dir).join("npx-cli.js")),
            ShimName::Composer => {
                let composer = composer.ok_or_else(|| ShimError::ComposerNotInstalled {
                    dir: runtimes.runtime_dir(Runtime::Composer),
                })?;
                let dir = runtimes.version_dir(Runtime::Composer, composer);
                Some(dir.join("composer.phar"))
            }
        };
        Ok(Self {
            program: layout.program(&version_dir, runtime),
            script,
            args: user_args.into_iter().collect(),
            trust: None,
            server_env: Vec::new(),
        })
    }

    /// The plan of a database client: its program in the `version` folder
    /// of its server, which must come from [`RuntimesDir::installed`], with
    /// the user's arguments as they are. `redis-cli` runs `valkey-cli`.
    pub fn client(
        client: Client,
        platform: Platform,
        runtimes: &RuntimesDir,
        version: &Version,
        user_args: impl IntoIterator<Item = OsString>,
    ) -> Self {
        let version_dir = runtimes.version_dir(client.server.runtime(), version);
        Self {
            program: Layout::of(platform).client(&version_dir, client.program),
            script: None,
            args: user_args.into_iter().collect(),
            trust: None,
            server_env: Vec::new(),
        }
    }

    /// # Errors
    ///
    /// `shim_program_missing` if the program or the script is not a file. The
    /// version folder exists, so this is a broken install, not a missing
    /// version.
    pub fn check(&self) -> Result<(), ShimError> {
        let missing = std::iter::once(&self.program)
            .chain(&self.script)
            .find(|path| !path.is_file());
        match missing {
            Some(path) => Err(ShimError::ProgramMissing { path: path.clone() }),
            None => Ok(()),
        }
    }

    /// The command to run. The environment is inherited: the depth counter
    /// goes up by one, [`Plan::trust`] sets or removes one variable, and
    /// [`Plan::server_env`] adds its variables. `argv[0]` is the absolute
    /// path, so `PHP_BINARY` and `process.execPath` point at the real
    /// program, and `@php` in Composer scripts uses the same PHP.
    pub fn command(&self, depth: u32) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.script)
            .args(&self.args)
            .env(depth::VAR, depth.saturating_add(1).to_string())
            .envs(self.server_env.iter().map(|(name, value)| (name, value)));
        match &self.trust {
            Some(TrustEnv::Set { name, file }) => {
                command.env(name, file);
            }
            Some(TrustEnv::Remove { name }) => {
                command.env_remove(name);
            }
            None => {}
        }
        command
    }
}

/// Where the programs sit inside a version folder: how the archives of each
/// system are laid out. The built-in recipes say the same (`programs` in
/// `recipes/*.toml`), and a test checks that they agree: the shim does not
/// parse the recipes before every command (170 KB more and a TOML parse).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// macOS and Linux builds: `bin/php`, `bin/node`, `lib/node_modules`.
    Unix,
    /// The official Windows zips: `php.exe` and `node.exe` at the top, and
    /// `node_modules` next to them.
    Windows,
}

impl Layout {
    fn of(platform: Platform) -> Self {
        match platform {
            Platform::WindowsX64 => Layout::Windows,
            Platform::DarwinArm64
            | Platform::DarwinX64
            | Platform::LinuxX64
            | Platform::LinuxArm64
            | Platform::Any => Layout::Unix,
        }
    }

    /// `php` or `node` of `runtime`, in its version folder `dir`.
    fn program(self, dir: &Path, runtime: Runtime) -> PathBuf {
        match self {
            Layout::Unix => dir.join("bin").join(runtime.dir_name()),
            Layout::Windows => dir.join(format!("{}.exe", runtime.dir_name())),
        }
    }

    /// A database client `program`, in its server's version folder `dir`:
    /// `bin/` on every system, as in the server's recipe.
    fn client(self, dir: &Path, program: &str) -> PathBuf {
        let bin = dir.join("bin");
        match self {
            Layout::Unix => bin.join(program),
            Layout::Windows => bin.join(format!("{program}.exe")),
        }
    }

    /// The folder of `npm-cli.js` and `npx-cli.js`, in a Node folder `dir`.
    fn npm_bin(self, dir: &Path) -> PathBuf {
        let modules = match self {
            Layout::Unix => dir.join("lib").join("node_modules"),
            Layout::Windows => dir.join("node_modules"),
        };
        modules.join("npm").join("bin")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::databases::clients::CLIENTS;
    use std::ffi::OsStr;
    use std::path::Path;

    fn v(text: &str) -> Version {
        Version::parse(text).expect("valid test version")
    }

    fn plan(name: ShimName, version: &str, composer: Option<&str>, args: &[&str]) -> Plan {
        plan_on(Platform::LinuxX64, name, version, composer, args)
    }

    fn plan_on(
        platform: Platform,
        name: ShimName,
        version: &str,
        composer: Option<&str>,
        args: &[&str],
    ) -> Plan {
        let runtimes = RuntimesDir::in_dir(Path::new("/data"));
        let composer = composer.map(v);
        let args = args.iter().map(OsString::from);
        Plan::new(
            name,
            platform,
            &runtimes,
            &v(version),
            composer.as_ref(),
            args,
        )
        .expect("valid plan")
    }

    #[test]
    fn php_runs_its_own_binary() {
        let plan = plan(ShimName::Php, "8.4.3", None, &["-v", "a b"]);
        let expected = Path::new("/data/runtimes/php/8.4.3/bin/php");
        assert_eq!(plan.program, expected);
        assert_eq!(plan.script, None);
        assert_eq!(plan.args, ["-v", "a b"]);
    }

    #[test]
    fn node_runs_its_own_binary() {
        let plan = plan(ShimName::Node, "22.11.0", None, &[]);
        let expected = Path::new("/data/runtimes/node/22.11.0/bin/node");
        assert_eq!(plan.program, expected);
        assert_eq!(plan.script, None);
        assert!(plan.args.is_empty());
    }

    #[test]
    fn npm_and_npx_run_through_node() {
        let node = Path::new("/data/runtimes/node/22.11.0");
        for (name, script) in [(ShimName::Npm, "npm-cli.js"), (ShimName::Npx, "npx-cli.js")] {
            let plan = plan(name, "22.11.0", None, &["install"]);
            assert_eq!(plan.program, node.join("bin/node"));
            let expected = node.join("lib/node_modules/npm/bin").join(script);
            assert_eq!(plan.script, Some(expected));
            assert_eq!(plan.args, ["install"]);
        }
    }

    #[test]
    fn composer_runs_through_php() {
        let plan = plan(ShimName::Composer, "8.3.14", Some("2.8.4"), &["install"]);
        assert_eq!(plan.program, Path::new("/data/runtimes/php/8.3.14/bin/php"));
        let expected = Path::new("/data/runtimes/composer/2.8.4/composer.phar");
        assert_eq!(plan.script.as_deref(), Some(expected));
        assert_eq!(plan.args, ["install"]);
    }

    #[test]
    fn composer_needs_an_installed_composer() {
        let runtimes = RuntimesDir::in_dir(Path::new("/data"));
        let err = Plan::new(
            ShimName::Composer,
            Platform::LinuxX64,
            &runtimes,
            &v("8.4.3"),
            None,
            [],
        )
        .expect_err("no composer version");
        assert_eq!(err.code(), "shim_composer_not_installed");
    }

    #[test]
    fn macos_runs_the_same_layout_as_linux() {
        for name in [
            ShimName::Php,
            ShimName::Node,
            ShimName::Npm,
            ShimName::Composer,
        ] {
            let mac = plan_on(Platform::DarwinArm64, name, "22.11.0", Some("2.8.4"), &[]);
            assert_eq!(mac, plan(name, "22.11.0", Some("2.8.4"), &[]), "{name:?}");
        }
    }

    #[test]
    fn windows_runs_the_exe_files_of_the_zip_layout() {
        let on =
            |name, version| plan_on(Platform::WindowsX64, name, version, Some("2.8.4"), &["x"]);
        let php = Path::new("/data/runtimes/php/8.4.3");
        let node = Path::new("/data/runtimes/node/22.11.0");
        let npm_bin = node.join("node_modules").join("npm").join("bin");

        let plan = on(ShimName::Php, "8.4.3");
        assert_eq!(plan.program, php.join("php.exe"));
        assert_eq!(plan.script, None);
        assert_eq!(plan.args, ["x"]);

        let plan = on(ShimName::Composer, "8.4.3");
        assert_eq!(plan.program, php.join("php.exe"));
        let phar = Path::new("/data/runtimes/composer/2.8.4/composer.phar");
        assert_eq!(plan.script.as_deref(), Some(phar));

        let plan = on(ShimName::Node, "22.11.0");
        assert_eq!(plan.program, node.join("node.exe"));
        assert_eq!(plan.script, None);

        for (name, script) in [(ShimName::Npm, "npm-cli.js"), (ShimName::Npx, "npx-cli.js")] {
            let plan = on(name, "22.11.0");
            assert_eq!(plan.program, node.join("node.exe"), "{name:?}");
            assert_eq!(plan.script, Some(npm_bin.join(script)), "{name:?}");
        }
    }

    #[test]
    fn every_path_is_where_the_recipe_puts_it() {
        use marsa_core::recipes;
        let runtimes = RuntimesDir::in_dir(Path::new("/data"));
        let recipe_path = |runtime, platform, version: &str, program| {
            let dir = runtimes.version_dir(runtime, &v(version));
            recipes::builtin(runtime)
                .expect("built-in recipe is valid")
                .platform(platform)
                .and_then(|layout| layout.program_path(&dir, program))
                .expect("the recipe names the program")
        };
        for platform in [
            Platform::DarwinArm64,
            Platform::DarwinX64,
            Platform::LinuxX64,
            Platform::LinuxArm64,
            Platform::WindowsX64,
        ] {
            let on = |name| plan_on(platform, name, "8.4.3", Some("2.8.4"), &[]);
            let php = recipe_path(Runtime::Php, platform, "8.4.3", "php");
            let node = recipe_path(Runtime::Node, platform, "8.4.3", "node");
            let phar = recipe_path(Runtime::Composer, platform, "2.8.4", "composer");
            assert_eq!(on(ShimName::Php).program, php, "{platform:?}");
            assert_eq!(on(ShimName::Node).program, node, "{platform:?}");
            assert_eq!(on(ShimName::Composer).program, php, "{platform:?}");
            assert_eq!(on(ShimName::Composer).script, Some(phar), "{platform:?}");
            for (name, program) in [(ShimName::Npm, "npm-cli"), (ShimName::Npx, "npx-cli")] {
                let plan = on(name);
                assert_eq!(plan.program, node, "{platform:?}");
                let script = recipe_path(Runtime::Node, platform, "8.4.3", program);
                assert_eq!(plan.script, Some(script), "{platform:?} {name:?}");
            }
            for client in CLIENTS {
                let runtimes = RuntimesDir::in_dir(Path::new("/data"));
                let plan = Plan::client(client, platform, &runtimes, &v("18.7.0"), []);
                let runtime = client.server.runtime();
                let want = recipe_path(runtime, platform, "18.7.0", client.program);
                assert_eq!(plan.program, want, "{platform:?} {}", client.name);
            }
        }
    }

    fn client(name: &str) -> Client {
        Client::from_name(name).expect("a client name")
    }

    fn client_plan(platform: Platform, name: &str, version: &str, args: &[&str]) -> Plan {
        let runtimes = RuntimesDir::in_dir(Path::new("/data"));
        let args = args.iter().map(OsString::from);
        Plan::client(client(name), platform, &runtimes, &v(version), args)
    }

    #[test]
    fn clients_run_their_program_from_the_folder_of_their_server() {
        for (name, runtime, version, program) in [
            ("psql", "postgres", "18.7.0", "psql"),
            ("pg_dump", "postgres", "18.7.0", "pg_dump"),
            ("pg_restore", "postgres", "17.9.0", "pg_restore"),
            ("mysql", "mysql", "8.4.13", "mysql"),
            ("mysqldump", "mysql", "8.4.13", "mysqldump"),
            ("mysqladmin", "mysql", "8.4.13", "mysqladmin"),
            ("valkey-cli", "valkey", "8.1.11", "valkey-cli"),
            ("redis-cli", "valkey", "8.1.11", "valkey-cli"),
        ] {
            let dir = Path::new("/data/runtimes").join(runtime).join(version);
            let plan = client_plan(Platform::DarwinArm64, name, version, &["-c", "a b"]);
            assert_eq!(plan.program, dir.join("bin").join(program), "{name}");
            assert_eq!(plan.script, None, "{name}");
            assert_eq!(plan.args, ["-c", "a b"], "{name}");
            assert_eq!((plan.trust, plan.server_env), (None, Vec::new()), "{name}");
            let linux = client_plan(Platform::LinuxX64, name, version, &[]);
            assert_eq!(linux.program, dir.join("bin").join(program), "{name}");
            let windows = client_plan(Platform::WindowsX64, name, version, &[]);
            let exe = dir.join("bin").join(format!("{program}.exe"));
            assert_eq!(windows.program, exe, "{name}");
        }
    }

    fn args(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    #[test]
    fn valkey_cli_gets_the_port_when_it_is_not_6379() {
        assert_eq!(
            valkey_args(6380, args(&["ping"])),
            args(&["-p", "6380", "ping"])
        );
        assert_eq!(valkey_args(6380, args(&[])), args(&["-p", "6380"]));
        // Valkey's own port: nothing is added.
        assert_eq!(valkey_args(6379, args(&["ping"])), args(&["ping"]));
        // The user's arguments are never reordered or changed.
        assert_eq!(
            valkey_args(6380, args(&["--raw", "-n", "2", "get", "k"])),
            args(&["-p", "6380", "--raw", "-n", "2", "get", "k"])
        );
    }

    #[test]
    fn valkey_cli_keeps_every_spelling_of_where_to_connect() {
        for given in [
            vec!["-p", "6390"],
            vec!["-p6390"],
            vec!["-h", "127.0.0.1"],
            vec!["-h127.0.0.1"],
            vec!["-u", "redis://127.0.0.1:6390"],
            vec!["-uredis://127.0.0.1:6390"],
            vec!["-s", "/tmp/valkey.sock"],
            vec!["--port", "6390"],
            vec!["--port=6390"],
            vec!["--host", "127.0.0.1"],
            vec!["--host=127.0.0.1"],
            vec!["--uri", "redis://127.0.0.1:6390"],
            vec!["--uri=redis://127.0.0.1:6390"],
            vec!["--socket", "/tmp/valkey.sock"],
            // After another option, or before the command.
            vec!["--raw", "-p", "6390", "ping"],
            vec!["ping", "-p", "6390"],
        ] {
            assert_eq!(valkey_args(6380, args(&given)), args(&given), "{given:?}");
        }
    }

    #[test]
    fn other_options_do_not_count_as_a_target() {
        // `--pass`, `--pipe`, `--user`, `-n`, `-a`, `-x`, and a value that
        // only looks like one: the port is still added.
        for given in [
            vec!["--pass", "secret", "ping"],
            vec!["--pipe"],
            vec!["--user", "app", "ping"],
            vec!["-n", "2", "ping"],
            vec!["-a", "secret", "ping"],
            vec!["-x", "set", "k"],
            vec!["--raw", "get", "host"],
            vec!["get", "port"],
        ] {
            let mut want = args(&["-p", "6380"]);
            want.extend(args(&given));
            assert_eq!(valkey_args(6380, args(&given)), want, "{given:?}");
        }
        // Not UTF-8 is not an option of ours.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let odd = OsString::from_vec(vec![b'-', b'p', 0xff]);
            assert!(!names_a_target(&odd));
        }
    }

    #[test]
    fn each_server_gets_the_variables_its_clients_read() {
        let postgres = server_env(ServerKind::Postgres, 5433);
        let want = [
            ("PGHOST", "127.0.0.1"),
            ("PGPORT", "5433"),
            ("PGUSER", "root"),
        ];
        assert_eq!(postgres, want.map(|(k, v)| (k, v.to_owned())));
        let mysql = server_env(ServerKind::Mysql, 3306);
        let want = [("MYSQL_HOST", "127.0.0.1"), ("MYSQL_TCP_PORT", "3306")];
        assert_eq!(mysql, want.map(|(k, v)| (k, v.to_owned())));
        assert!(server_env(ServerKind::Valkey, 6380).is_empty());
    }

    #[test]
    fn a_variable_the_user_set_is_never_added() {
        let all = server_env(ServerKind::Postgres, 5432);
        let none_set = unset_only(all.clone(), |_| false);
        assert_eq!(none_set, all);
        // `PGHOST=db.example psql`: the user's host stays, the rest is added.
        let host_set = unset_only(all.clone(), |name| name == "PGHOST");
        let want = [("PGPORT", "5432"), ("PGUSER", "root")];
        assert_eq!(host_set, want.map(|(k, v)| (k, v.to_owned())));
        assert!(unset_only(all, |_| true).is_empty());
    }

    #[test]
    fn the_command_adds_the_server_variables() {
        let mut plan = client_plan(Platform::LinuxX64, "psql", "18.7.0", &["-l"]);
        assert!(extra_vars(&plan.command(0)).is_empty());
        plan.server_env = unset_only(server_env(ServerKind::Postgres, 5432), |name| {
            name == "PGUSER"
        });
        let command = plan.command(2);
        assert_eq!(command.get_program(), plan.program.as_os_str());
        assert!(command.get_args().eq([OsStr::new("-l")]));
        let want = [
            (OsStr::new("PGHOST"), Some(OsStr::new("127.0.0.1"))),
            (OsStr::new("PGPORT"), Some(OsStr::new("5432"))),
        ];
        let mut vars = extra_vars(&command);
        vars.sort();
        assert_eq!(vars, want);
    }

    #[test]
    fn a_client_runs_the_default_line_of_its_server_or_the_newest() {
        let runtimes = RuntimesDir::in_dir(Path::new("/data"));
        let installed = [v("17.9.0"), v("18.6.0"), v("18.7.0")];
        let pick = |kind, default, installed: &[Version]| {
            server_version(kind, default, installed, &runtimes)
        };
        let postgres = ServerKind::Postgres;
        assert_eq!(pick(postgres, Some("17"), &installed).unwrap(), v("17.9.0"));
        assert_eq!(
            pick(postgres, Some(" 18 "), &installed).unwrap(),
            v("18.7.0")
        );
        assert_eq!(pick(postgres, None, &installed).unwrap(), v("18.7.0"));

        match pick(postgres, Some(" 16 "), &installed) {
            Err(ShimError::ServerLineNotInstalled { server, line }) => {
                assert_eq!((server, line.as_str()), (postgres, "16"));
            }
            other => panic!("not a missing line: {other:?}"),
        }
        // Nothing installed: the install is what is missing, whatever the
        // default says.
        for default in [None, Some("18")] {
            match pick(ServerKind::Mysql, default, &[]) {
                Err(ShimError::ServerNotInstalled { server, dir }) => {
                    assert_eq!(server, ServerKind::Mysql);
                    assert_eq!(dir, Path::new("/data/runtimes/mysql"));
                }
                other => panic!("not a missing server: {other:?}"),
            }
        }
        let valkey = [v("8.1.11"), v("9.0.0")];
        let pick_valkey = |default| pick(ServerKind::Valkey, default, &valkey).unwrap();
        assert_eq!(pick_valkey(Some("8.1")), v("8.1.11"));
        assert_eq!(pick_valkey(None), v("9.0.0"));
    }

    // Only Unix lets an argument hold bytes that are not UTF-8.
    #[cfg(unix)]
    #[test]
    fn arguments_that_are_not_utf8_pass_unchanged() {
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(vec![b'a', 0xff, b'b']);
        let runtimes = RuntimesDir::in_dir(Path::new("/data"));
        let plan = Plan::new(
            ShimName::Php,
            Platform::LinuxX64,
            &runtimes,
            &v("8.4.3"),
            None,
            [raw.clone()],
        )
        .expect("valid plan");
        assert_eq!(plan.args, [raw]);
    }

    #[test]
    fn the_command_puts_the_script_first_and_adds_one_to_the_depth() {
        let plan = plan(ShimName::Npm, "22.11.0", None, &["run", "dev"]);
        let command = plan.command(3);
        assert_eq!(command.get_program(), plan.program.as_os_str());
        let script = plan.script.as_deref().expect("npm has a script");
        let expected = [script.as_os_str(), OsStr::new("run"), OsStr::new("dev")];
        assert!(command.get_args().eq(expected));
        let vars: Vec<_> = command.get_envs().collect();
        assert_eq!(vars, [(OsStr::new(depth::VAR), Some(OsStr::new("4")))]);
    }

    #[test]
    fn node_tools_point_node_at_marsas_root() {
        let data = Path::new("/data");
        let root = data
            .join("caddy")
            .join("data")
            .join("caddy")
            .join("pki")
            .join("authorities")
            .join("marsa")
            .join("root.crt");
        for name in [ShimName::Node, ShimName::Npm, ShimName::Npx] {
            let var = TrustVar::for_shim(name, data, &v("22.11.0"));
            assert_eq!(var.name, "NODE_EXTRA_CA_CERTS", "{name:?}");
            assert_eq!(var.file, root, "{name:?}");
            assert_eq!(var.file, CaddyPaths::in_dir(data).ca_root(), "{name:?}");
            assert_eq!(var.marsa_dir, None, "{name:?}");
        }
    }

    #[test]
    fn php_tools_point_phprc_at_the_ini_of_their_line() {
        let data = Path::new("/data");
        for name in [ShimName::Php, ShimName::Composer] {
            let var = TrustVar::for_shim(name, data, &v("8.4.3"));
            assert_eq!(var.name, "PHPRC", "{name:?}");
            assert_eq!(var.file, data.join("php").join("8.4").join("php.ini"));
            assert_eq!(var.marsa_dir, Some(data.join("php")), "{name:?}");
            let other_line = TrustVar::for_shim(name, data, &v("8.3.14"));
            assert_eq!(
                other_line.file,
                data.join("php").join("8.3").join("php.ini")
            );
        }
    }

    /// Sorts `value` with `var`, then decides.
    fn decide(var: &TrustVar, value: Option<&OsStr>, file_exists: bool) -> Option<TrustEnv> {
        let inherited = var.inherited(value);
        var.clone().decide(inherited, file_exists)
    }

    #[test]
    fn inherited_values_are_sorted_by_who_set_them() {
        let data = Path::new("/data");
        let php = TrustVar::for_shim(ShimName::Php, data, &v("8.4.3"));
        assert_eq!(php.inherited(None), Inherited::Unset);
        let marsa = [
            data.join("php").join("8.3").join("php.ini"),
            data.join("php").join("8.4").join("php.ini"),
        ];
        for value in &marsa {
            let inherited = php.inherited(Some(value.as_os_str()));
            assert_eq!(inherited, Inherited::Marsa, "{value:?}");
        }
        let user = [
            PathBuf::new(),
            data.join("phpx").join("8.3").join("php.ini"),
            data.join("etc").join("php.ini"),
            Path::new("php").join("8.3").join("php.ini"),
        ];
        for value in &user {
            let inherited = php.inherited(Some(value.as_os_str()));
            assert_eq!(inherited, Inherited::User, "{value:?}");
        }

        // Node keeps the simple rule: every value is the user's.
        let node = TrustVar::for_shim(ShimName::Node, data, &v("22.11.0"));
        assert_eq!(node.inherited(None), Inherited::Unset);
        for value in [node.file.clone(), marsa[0].clone(), PathBuf::new()] {
            let inherited = node.inherited(Some(value.as_os_str()));
            assert_eq!(inherited, Inherited::User, "{value:?}");
        }
    }

    #[test]
    fn phprc_decision_for_every_case() {
        let data = Path::new("/data");
        let var = TrustVar::for_shim(ShimName::Php, data, &v("8.4.3"));
        let set = Some(TrustEnv::Set {
            name: PHPRC,
            file: data.join("php").join("8.4").join("php.ini"),
        });
        let remove = Some(TrustEnv::Remove { name: PHPRC });
        let user = Path::new("/home/me/php.ini");
        let other_line = data.join("php").join("8.3").join("php.ini");
        let cases = [
            // Unset.
            (None, true, set.clone()),
            (None, false, None),
            // A user value.
            (Some(user.as_os_str()), true, None),
            (Some(user.as_os_str()), false, None),
            // An empty value is the user's too.
            (Some(OsStr::new("")), true, None),
            (Some(OsStr::new("")), false, None),
            // Marsa's value for another PHP line: replaced, or removed.
            (Some(other_line.as_os_str()), true, set),
            (Some(other_line.as_os_str()), false, remove),
        ];
        for (value, file_exists, expected) in cases {
            let result = decide(&var, value, file_exists);
            assert_eq!(result, expected, "{value:?} {file_exists}");
        }
    }

    #[test]
    fn node_decision_for_every_case() {
        let data = Path::new("/data");
        let var = TrustVar::for_shim(ShimName::Npm, data, &v("22.11.0"));
        let set = Some(TrustEnv::Set {
            name: NODE_EXTRA_CA_CERTS,
            file: var.file.clone(),
        });
        let user = Path::new("/home/me/ca.pem");
        let in_data_dir = data.join("php").join("8.3").join("php.ini");
        let cases = [
            (None, true, set),
            (None, false, None),
            (Some(user.as_os_str()), true, None),
            (Some(user.as_os_str()), false, None),
            (Some(OsStr::new("")), true, None),
            (Some(in_data_dir.as_os_str()), true, None),
            (Some(var.file.as_os_str()), false, None),
        ];
        for (value, file_exists, expected) in cases {
            let result = decide(&var, value, file_exists);
            assert_eq!(result, expected, "{value:?} {file_exists}");
        }
    }

    /// The variables of `command` that are not the depth counter.
    fn extra_vars(command: &Command) -> Vec<(&OsStr, Option<&OsStr>)> {
        command
            .get_envs()
            .filter(|(name, _)| *name != OsStr::new(depth::VAR))
            .collect()
    }

    #[test]
    fn the_command_sets_the_variable() {
        let data = Path::new("/data");
        for (name, version) in [
            (ShimName::Node, "22.11.0"),
            (ShimName::Npm, "22.11.0"),
            (ShimName::Php, "8.4.3"),
            (ShimName::Composer, "8.4.3"),
        ] {
            let var = TrustVar::for_shim(name, data, &v(version));
            let mut plan = plan(name, version, Some("2.8.4"), &[]);
            assert!(extra_vars(&plan.command(0)).is_empty(), "{name:?}");

            plan.trust = decide(&var, None, true);
            let command = plan.command(0);
            let expected = [(OsStr::new(var.name), Some(var.file.as_os_str()))];
            assert_eq!(extra_vars(&command), expected, "{name:?}");
            // The depth counter is still there.
            let counter = command
                .get_envs()
                .find(|(key, _)| *key == OsStr::new(depth::VAR));
            assert_eq!(
                counter,
                Some((OsStr::new(depth::VAR), Some(OsStr::new("1"))))
            );
        }
    }

    /// An outer shim on PHP 8.4 set `PHPRC`; this one runs PHP 8.3.
    #[test]
    fn the_command_replaces_or_removes_marsas_phprc() {
        let data = Path::new("/data");
        let outer = data.join("php").join("8.4").join("php.ini");
        for name in [ShimName::Php, ShimName::Composer] {
            let var = TrustVar::for_shim(name, data, &v("8.3.14"));
            let mut plan = plan(name, "8.3.14", Some("2.8.4"), &[]);

            plan.trust = decide(&var, Some(outer.as_os_str()), true);
            let expected = [(OsStr::new(PHPRC), Some(var.file.as_os_str()))];
            assert_eq!(extra_vars(&plan.command(0)), expected, "{name:?}");

            // `None` as the value is how `get_envs` shows `env_remove`.
            plan.trust = decide(&var, Some(outer.as_os_str()), false);
            let expected = [(OsStr::new(PHPRC), None)];
            assert_eq!(extra_vars(&plan.command(0)), expected, "{name:?}");
        }
    }

    /// A user value is neither set nor removed, so the program gets it from
    /// the inherited environment. A missing file is not named either.
    #[test]
    fn the_command_never_touches_a_user_value_or_names_a_missing_file() {
        let data = Path::new("/data");
        for (name, version) in [(ShimName::Npx, "22.11.0"), (ShimName::Php, "8.4.3")] {
            let var = TrustVar::for_shim(name, data, &v(version));
            for (value, file_exists) in [
                (Some("/home/me/file"), true),
                (Some("/home/me/file"), false),
                (Some(""), true),
                (Some(""), false),
                (None, false),
            ] {
                let mut plan = plan(name, version, None, &[]);
                plan.trust = decide(&var, value.map(OsStr::new), file_exists);
                let command = plan.command(0);
                let vars = extra_vars(&command);
                assert!(vars.is_empty(), "{name:?} {value:?} {file_exists}");
            }
        }
    }

    #[test]
    fn check_reports_a_missing_program_or_script() {
        let data = tempfile::TempDir::new().expect("create temp dir");
        let runtimes = RuntimesDir::in_dir(data.path());
        let plan = Plan::new(
            ShimName::Npm,
            Platform::LinuxX64,
            &runtimes,
            &v("22.11.0"),
            None,
            [],
        )
        .expect("valid plan");
        let script = plan.script.clone().expect("npm has a script");
        let missing = |plan: &Plan| match plan.check() {
            Err(ShimError::ProgramMissing { path }) => Some(path),
            Err(err) => panic!("unexpected error {err}"),
            Ok(()) => None,
        };
        assert_eq!(missing(&plan), Some(plan.program.clone()));

        for file in [&plan.program, &script] {
            let dir = file.parent().expect("the file has a folder");
            std::fs::create_dir_all(dir).expect("create folder");
        }
        std::fs::write(&plan.program, "").expect("write program");
        assert_eq!(missing(&plan), Some(script.clone()));

        std::fs::write(&script, "").expect("write script");
        assert_eq!(missing(&plan), None);
    }
}
