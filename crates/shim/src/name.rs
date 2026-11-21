//! What the shim runs, from the name it was started as.

use std::ffi::OsStr;
use std::path::Path;

use marsa_core::databases::clients::Client;
use marsa_core::resolver::Tool;
use marsa_core::runtimes::Runtime;

/// The shim's role, from its name: a tool whose version the project picks,
/// or a database server's client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Tool(ShimName),
    /// `psql`, `mysqldump`, `redis-cli`, ...: the version is the one the
    /// server runs, never a project's.
    Client(Client),
}

impl Role {
    /// Reads `argv[0]`. Only the file name counts, so `/any/path/php` is `php`.
    ///
    /// Not `current_exe()`: on Linux it follows symlinks and gives `marsa-shim`.
    /// The match is exact and case-sensitive.
    #[cfg_attr(
        all(windows, not(test)),
        expect(dead_code, reason = "Windows reads its own program path instead")
    )]
    pub fn from_argv0(argv0: &OsStr) -> Option<Self> {
        Self::from_name(Path::new(argv0).file_name()?.to_str()?)
    }

    /// Reads the path of a Windows program, such as `C:\...\shims\php.exe`
    /// from `current_exe()`, or `php` from `argv[0]`: the file name without
    /// `.exe`, in any letter case, as Windows itself finds `PHP` and
    /// `php.EXE`. Another extension (`npm.cmd`) is not a shim.
    #[cfg_attr(
        all(unix, not(test)),
        expect(dead_code, reason = "only `sys/windows.rs` reads a program path")
    )]
    pub fn from_exe_path(path: &Path) -> Option<Self> {
        let exe = path
            .extension()
            .is_none_or(|extension| extension.eq_ignore_ascii_case("exe"));
        if !exe {
            return None;
        }
        Self::from_name(&path.file_stem()?.to_str()?.to_ascii_lowercase())
    }

    /// The exact name, case-sensitive.
    fn from_name(name: &str) -> Option<Self> {
        let tool = match name {
            "php" => ShimName::Php,
            "node" => ShimName::Node,
            "npm" => ShimName::Npm,
            "npx" => ShimName::Npx,
            "composer" => ShimName::Composer,
            _ => return Client::from_name(name).map(Role::Client),
        };
        Some(Role::Tool(tool))
    }
}

/// A name the shim answers to whose version the project picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimName {
    Php,
    Node,
    /// Runs through `node`, with npm's own `npm-cli.js`.
    Npm,
    /// Runs through `node`, with npm's own `npx-cli.js`.
    Npx,
    /// Runs through `php`, with `composer.phar`.
    Composer,
}

impl ShimName {
    /// The names, in the order the shim's hint lists them.
    pub const ALL: [ShimName; 5] = [
        ShimName::Php,
        ShimName::Node,
        ShimName::Npm,
        ShimName::Npx,
        ShimName::Composer,
    ];

    /// The name a link has.
    pub fn name(self) -> &'static str {
        match self {
            ShimName::Php => "php",
            ShimName::Node => "node",
            ShimName::Npm => "npm",
            ShimName::Npx => "npx",
            ShimName::Composer => "composer",
        }
    }

    /// The tool whose version is picked.
    pub fn tool(self) -> Tool {
        match self {
            ShimName::Php | ShimName::Composer => Tool::Php,
            ShimName::Node | ShimName::Npm | ShimName::Npx => Tool::Node,
        }
    }

    /// The folder under `runtimes/` that holds the real program: `php` for
    /// `composer`, `node` for `npm` and `npx`.
    pub fn runtime(self) -> Runtime {
        self.tool().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::databases::ServerKind;
    use marsa_core::databases::clients::CLIENTS;

    fn name(text: &str) -> Option<Role> {
        Role::from_argv0(OsStr::new(text))
    }

    fn tool(name: ShimName) -> Option<Role> {
        Some(Role::Tool(name))
    }

    fn client(name: &str) -> Option<Role> {
        Some(Role::Client(Client::from_name(name).expect("a client")))
    }

    #[test]
    fn known_names() {
        assert_eq!(name("php"), tool(ShimName::Php));
        assert_eq!(name("/any/path/php"), tool(ShimName::Php));
        assert_eq!(name("node"), tool(ShimName::Node));
        assert_eq!(name("shims/node"), tool(ShimName::Node));
        assert_eq!(name("npm"), tool(ShimName::Npm));
        assert_eq!(name("npx"), tool(ShimName::Npx));
        assert_eq!(name("/any/path/composer"), tool(ShimName::Composer));
        for shim in ShimName::ALL {
            assert_eq!(name(shim.name()), tool(shim), "{shim:?}");
        }
    }

    #[test]
    fn every_client_name_is_its_client() {
        for (text, program, server) in [
            ("psql", "psql", ServerKind::Postgres),
            ("pg_dump", "pg_dump", ServerKind::Postgres),
            ("pg_restore", "pg_restore", ServerKind::Postgres),
            ("mysql", "mysql", ServerKind::Mysql),
            ("mysqldump", "mysqldump", ServerKind::Mysql),
            ("mysqladmin", "mysqladmin", ServerKind::Mysql),
            ("valkey-cli", "valkey-cli", ServerKind::Valkey),
            ("redis-cli", "valkey-cli", ServerKind::Valkey),
        ] {
            let want = Some(Role::Client(Client {
                name: text,
                program,
                server,
            }));
            assert_eq!(name(text), want, "{text}");
            assert_eq!(name(&format!("/any/shims/{text}")), want, "{text}");
        }
        assert_eq!(CLIENTS.len(), 8);
    }

    #[test]
    fn unknown_names() {
        for text in [
            "marsa-shim",
            "PHP",
            "",
            "/",
            "php8",
            "php.exe",
            "Composer",
            "npm.cmd",
            // Caddy is a runtime that `marsad` runs, not a shim tool.
            "caddy",
            "caddy.exe",
            // The servers themselves run under `marsad` only.
            "postgres",
            "mysqld",
            "valkey-server",
            "redis-server",
            "PSQL",
            "psql.exe",
            "Redis-Cli",
            "pg_ctl",
        ] {
            assert_eq!(name(text), None, "{text:?}");
        }
    }

    fn exe(parts: &[&str]) -> Option<Role> {
        let path: std::path::PathBuf = parts.iter().collect();
        Role::from_exe_path(&path)
    }

    #[test]
    fn windows_names_ignore_the_letter_case_and_exe() {
        for (parts, expected) in [
            (&["C:", "shims", "php.exe"][..], tool(ShimName::Php)),
            (&["shims", "PHP.EXE"], tool(ShimName::Php)),
            (&["php"], tool(ShimName::Php)),
            (&["Node.Exe"], tool(ShimName::Node)),
            (&["npm.exe"], tool(ShimName::Npm)),
            (&["NPX"], tool(ShimName::Npx)),
            (&["shims", "composer.exe"], tool(ShimName::Composer)),
            (&["C:", "shims", "psql.exe"], client("psql")),
            (&["PG_DUMP.EXE"], client("pg_dump")),
            (&["Pg_Restore"], client("pg_restore")),
            (&["MySQL.exe"], client("mysql")),
            (&["mysqldump.EXE"], client("mysqldump")),
            (&["mysqladmin"], client("mysqladmin")),
            (&["Valkey-Cli.exe"], client("valkey-cli")),
            (&["shims", "REDIS-CLI.exe"], client("redis-cli")),
        ] {
            assert_eq!(exe(parts), expected, "{parts:?}");
        }
        for parts in [
            &["marsa-shim.exe"][..],
            &["npm.cmd"],
            &["composer.bat"],
            &["php.exe.old"],
            &["php8.exe"],
            &["caddy.exe"],
            &[""],
            &["psql.cmd"],
            &["redis-cli.exe.old"],
            &["postgres.exe"],
            &["valkey-server.exe"],
        ] {
            assert_eq!(exe(parts), None, "{parts:?}");
        }
    }

    // Only Unix lets a file name hold bytes that are not UTF-8.
    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_is_unknown() {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(Role::from_argv0(OsStr::from_bytes(b"ph\xffp")), None);
    }

    #[test]
    fn tool_and_runtime() {
        for (name, tool, runtime) in [
            (ShimName::Php, Tool::Php, Runtime::Php),
            (ShimName::Composer, Tool::Php, Runtime::Php),
            (ShimName::Node, Tool::Node, Runtime::Node),
            (ShimName::Npm, Tool::Node, Runtime::Node),
            (ShimName::Npx, Tool::Node, Runtime::Node),
        ] {
            assert_eq!(name.tool(), tool, "{name:?}");
            assert_eq!(name.runtime(), runtime, "{name:?}");
        }
    }
}
