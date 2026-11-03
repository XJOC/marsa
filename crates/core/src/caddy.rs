//! Pure function: the settings become a Caddy JSON config.
//!
//! This module only builds data. It does not start Caddy and does not call
//! its admin API; the daemon does that (task M3-07), like a Blade view that
//! returns text and leaves the sending to a controller.
//!
//! In M3 there are no sites yet (they come in M4), so the config has one
//! server that answers a fixed text. That proves Caddy runs and listens where
//! we expect.
//!
//! Every address in the config is `127.0.0.1` (architecture.md section 10).
//! Caddy runs without admin rights on high ports; the helper redirects ports
//! 80 and 443 to them later (ADR 0008).
//!
//! This module is not behind the `full` feature: it is pure data, with no
//! tokio.

use std::collections::BTreeMap;
use std::env::consts::EXE_SUFFIX;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The port of Caddy's admin API. It is private to Marsa, not Caddy's default
/// `2019`, so a Caddy of the user's own never answers for us (ADR 0009).
pub const ADMIN_PORT: u16 = 18019;

/// The port Caddy serves HTTP on. It is high so Caddy needs no admin rights
/// (ADR 0008).
pub const HTTP_PORT: u16 = 18080;

/// The port Caddy serves HTTPS on. It is high for the same reason as
/// [`HTTP_PORT`].
pub const HTTPS_PORT: u16 = 18443;

/// The `@id` tag on our server. `GET /id/marsa-srv` on the admin API tells the
/// daemon that a running Caddy is ours (ADR 0009).
pub const SERVER_ID: &str = "marsa-srv";

/// What the only server answers in M3.
const GREETING: &str = "Marsa is running. No sites yet.";

/// Where Caddy's files live inside the app data dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaddyPaths {
    /// The Caddy executable: `bin/caddy` (`bin/caddy.exe` on Windows).
    pub program: PathBuf,
    /// Caddy's own folder: `caddy/`. Its state stays inside it.
    pub home: PathBuf,
    /// The config file: `caddy/caddy.json`.
    pub config_file: PathBuf,
}

impl CaddyPaths {
    /// The paths for an app data dir. Nothing is created on disk.
    pub fn in_dir(app_data_dir: &Path) -> Self {
        let home = app_data_dir.join("caddy");
        Self {
            program: app_data_dir.join("bin").join(format!("caddy{EXE_SUFFIX}")),
            config_file: home.join("caddy.json"),
            home,
        }
    }
}

/// The environment variables Caddy needs.
///
/// Without them Caddy keeps its data in the user's own folder (for example
/// `~/Library/Application Support/Caddy` on macOS), outside our folder. With
/// them it uses `caddy/data/caddy` and `caddy/config/caddy`: Caddy adds the
/// last `caddy` itself.
pub fn caddy_env(paths: &CaddyPaths) -> BTreeMap<String, String> {
    // `to_string_lossy` would change a path that is not valid Unicode, but the
    // app data dir comes from the OS and is valid in practice.
    let dir = |name: &str| paths.home.join(name).to_string_lossy().into_owned();
    BTreeMap::from([
        ("XDG_DATA_HOME".to_string(), dir("data")),
        ("XDG_CONFIG_HOME".to_string(), dir("config")),
    ])
}

/// What the config is built from. For now only the ports; M4 adds the sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaddyInput {
    pub admin_port: u16,
    pub http_port: u16,
    pub https_port: u16,
}

impl Default for CaddyInput {
    fn default() -> Self {
        Self {
            admin_port: ADMIN_PORT,
            http_port: HTTP_PORT,
            https_port: HTTPS_PORT,
        }
    }
}

/// Builds the Caddy config as JSON. The same input always gives the same
/// output.
pub fn caddy_config(input: &CaddyInput) -> Value {
    json!({
        "admin": {
            "listen": format!("127.0.0.1:{}", input.admin_port),
            // No `autosave.json`: the daemon sends the config at every start.
            "config": { "persist": false },
        },
        "apps": {
            "http": {
                "http_port": input.http_port,
                "https_port": input.https_port,
                "servers": {
                    "marsa": {
                        "@id": SERVER_ID,
                        "listen": [format!("127.0.0.1:{}", input.http_port)],
                        "routes": [{
                            "handle": [{
                                "handler": "static_response",
                                "status_code": 200,
                                "body": GREETING,
                            }],
                        }],
                        // No sites and no certificates yet (M4).
                        "automatic_https": { "disable": true },
                    },
                },
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collects every `listen` value in the JSON, at any depth. A value is
    /// one string or a list of strings.
    fn listen_addresses(value: &Value, found: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    if key == "listen" {
                        match child {
                            Value::String(address) => found.push(address.clone()),
                            Value::Array(list) => found.extend(list.iter().map(|item| {
                                item.as_str().expect("listen items are strings").to_string()
                            })),
                            other => panic!("unexpected listen value: {other}"),
                        }
                    }
                    listen_addresses(child, found);
                }
            }
            Value::Array(list) => list.iter().for_each(|v| listen_addresses(v, found)),
            _ => {}
        }
    }

    #[test]
    fn default_config_snapshot() {
        insta::assert_json_snapshot!("default_config", caddy_config(&CaddyInput::default()));
    }

    #[test]
    fn every_listen_address_is_loopback() {
        let mut found = Vec::new();
        listen_addresses(&caddy_config(&CaddyInput::default()), &mut found);
        // The admin address and the one server.
        assert_eq!(found.len(), 2);
        for address in &found {
            assert!(address.starts_with("127.0.0.1:"), "bad address: {address}");
        }
    }

    #[test]
    fn custom_ports_reach_the_config() {
        let input = CaddyInput {
            admin_port: 1,
            http_port: 2,
            https_port: 3,
        };
        let config = caddy_config(&input);

        assert_eq!(config["admin"]["listen"], "127.0.0.1:1");
        assert_eq!(config["apps"]["http"]["http_port"], 2);
        assert_eq!(config["apps"]["http"]["https_port"], 3);
        assert_eq!(
            config["apps"]["http"]["servers"]["marsa"]["listen"][0],
            "127.0.0.1:2"
        );
    }

    #[test]
    fn server_carries_the_marsa_id() {
        let config = caddy_config(&CaddyInput::default());
        assert_eq!(config["apps"]["http"]["servers"]["marsa"]["@id"], SERVER_ID);
    }

    #[test]
    fn paths_sit_inside_the_app_data_dir() {
        let dir = tempfile::tempdir().expect("create a temp dir");
        let paths = CaddyPaths::in_dir(dir.path());

        assert_eq!(
            paths.program,
            dir.path().join("bin").join(format!("caddy{EXE_SUFFIX}"))
        );
        assert_eq!(paths.home, dir.path().join("caddy"));
        assert_eq!(
            paths.config_file,
            dir.path().join("caddy").join("caddy.json")
        );
    }

    #[test]
    fn env_points_caddy_into_its_home() {
        let dir = tempfile::tempdir().expect("create a temp dir");
        let paths = CaddyPaths::in_dir(dir.path());
        let env = caddy_env(&paths);

        assert_eq!(env.len(), 2);
        assert_eq!(
            env["XDG_DATA_HOME"],
            dir.path().join("caddy").join("data").to_string_lossy()
        );
        assert_eq!(
            env["XDG_CONFIG_HOME"],
            dir.path().join("caddy").join("config").to_string_lossy()
        );
    }
}
