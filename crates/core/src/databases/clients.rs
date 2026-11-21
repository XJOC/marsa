//! The client programs of the database servers (`psql`, `mysqldump`,
//! `valkey-cli`, ...), which `marsa-shim` runs under their own names, so
//! `php artisan db` and `schema:dump` find them in `PATH` (ADR 0023).
//!
//! One list for the three programs that need it: the shim reads a name, and
//! `marsad` makes and removes the shim files of a server.

use super::ServerKind;

/// A client program of a database server, run by a shim of its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Client {
    /// The shim's name, as the user types it: `psql`, `redis-cli`.
    pub name: &'static str,
    /// The program in the `programs` table of the server's recipe that
    /// runs: `valkey-cli` for `redis-cli`.
    pub program: &'static str,
    /// The server whose installed version runs it.
    pub server: ServerKind,
}

/// Every client shim, in the order of [`ServerKind::ALL`].
pub const CLIENTS: [Client; 8] = [
    client("psql", "psql", ServerKind::Postgres),
    client("pg_dump", "pg_dump", ServerKind::Postgres),
    client("pg_restore", "pg_restore", ServerKind::Postgres),
    client("mysql", "mysql", ServerKind::Mysql),
    client("mysqldump", "mysqldump", ServerKind::Mysql),
    client("mysqladmin", "mysqladmin", ServerKind::Mysql),
    client("valkey-cli", "valkey-cli", ServerKind::Valkey),
    // Laravel's guides and many scripts call `redis-cli`, and `valkey-cli`
    // speaks the same protocol and takes the same arguments.
    client("redis-cli", "valkey-cli", ServerKind::Valkey),
];

const fn client(name: &'static str, program: &'static str, server: ServerKind) -> Client {
    Client {
        name,
        program,
        server,
    }
}

impl Client {
    /// The client with the shim name `name`. Exact and case-sensitive.
    pub fn from_name(name: &str) -> Option<Self> {
        CLIENTS.into_iter().find(|client| client.name == name)
    }
}

impl ServerKind {
    /// Its client shims, in the order of [`CLIENTS`].
    pub fn clients(self) -> impl Iterator<Item = Client> {
        CLIENTS
            .into_iter()
            .filter(move |client| client.server == self)
    }
}

/// The start of what `marsa-shim` prints when it runs under a name it does
/// not know, so every build of the shim holds these bytes. `marsad` looks
/// for them to tell an older copy of the shim, which it may replace, from
/// another program of the same name, which it never touches. Never change
/// them: the shims of earlier releases hold these bytes.
pub const SHIM_MARK: &str = "marsa-shim runs through links named ";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipes::{self, Platform};
    use std::path::Path;

    #[test]
    fn each_name_is_listed_once() {
        for client in CLIENTS {
            let same = CLIENTS.iter().filter(|other| other.name == client.name);
            assert_eq!(same.count(), 1, "{}", client.name);
            assert_eq!(Client::from_name(client.name), Some(client));
        }
        for name in [
            "PSQL",
            "psql.exe",
            "postgres",
            "valkey-server",
            "mysqld",
            "",
        ] {
            assert_eq!(Client::from_name(name), None, "{name:?}");
        }
    }

    #[test]
    fn each_server_has_its_clients_and_redis_cli_runs_valkey_cli() {
        let names = |kind: ServerKind| kind.clients().map(|c| c.name).collect::<Vec<_>>();
        assert_eq!(
            names(ServerKind::Postgres),
            ["psql", "pg_dump", "pg_restore"]
        );
        assert_eq!(
            names(ServerKind::Mysql),
            ["mysql", "mysqldump", "mysqladmin"]
        );
        assert_eq!(names(ServerKind::Valkey), ["valkey-cli", "redis-cli"]);
        let redis = Client::from_name("redis-cli").expect("redis-cli is a client");
        assert_eq!(
            (redis.program, redis.server),
            ("valkey-cli", ServerKind::Valkey)
        );
    }

    /// The shim runs `program` from the server's version folder, so every
    /// recipe of the server must name it on every system it builds for.
    #[test]
    fn every_program_is_in_the_recipe_of_its_server() {
        let dir = Path::new("/data/runtimes/x/1.0.0");
        for client in CLIENTS {
            let recipe = recipes::builtin(client.server.runtime()).expect("a valid recipe");
            let platforms: Vec<Platform> = recipe.platforms().map(|(p, _)| p).collect();
            assert!(!platforms.is_empty(), "{}", client.name);
            for (platform, layout) in recipe.platforms() {
                let path = layout.program_path(dir, client.program);
                assert!(path.is_some(), "{} on {platform:?}", client.name);
            }
        }
    }
}
