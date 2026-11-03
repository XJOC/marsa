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
//! [`sites_config`] adds the sites (M4): PHP through php-fpm or php-cgi,
//! static files, and Node proxies, with HTTPS from Caddy's own CA.
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

use crate::php::PhpMinor;
use crate::sites::{Site, SiteKind};

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

/// The name of the HTTPS server in [`sites_config`].
const TLS_SERVER: &str = "marsa-tls";

/// Where the HTTP server sends a secure site: the same host and path over
/// HTTPS. There is no port: on macOS the `pf` redirect sends 443 to the
/// HTTPS port (ADR 0008, finding 9).
const HTTPS_REDIRECT: &str = "https://{http.request.host}{http.request.uri}";

/// The id of our certificate authority in Caddy's `pki` app.
const CA_ID: &str = "marsa";

/// The name of the CA and the common name of its root certificate. Users see
/// it in the browser's certificate details.
const CA_NAME: &str = "Marsa Local CA";

/// What a PHP site answers when the pool of its PHP line is not running.
const PHP_NOT_RUNNING: &str = "Marsa: PHP {version} is not running for {host}.";

/// What a PHP site answers when no PHP version was picked for it.
const NO_PHP_VERSION: &str = "Marsa: no PHP version is installed for {host}.";

/// How many more times Caddy picks another upstream when one does not answer.
/// It matters only with more than one upstream (php-cgi on Windows).
const PHP_RETRIES: u32 = 2;

/// One address of a PHP pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dial {
    /// A php-fpm Unix socket (macOS, Linux).
    Unix(PathBuf),
    /// A php-cgi process on `127.0.0.1:<port>` (Windows).
    Tcp(u16),
}

/// The addresses of the running PHP pools, one entry per PHP line. A line with
/// more than one address (Windows runs several php-cgi processes) is load
/// balanced.
///
/// A `BTreeMap`, not a `HashMap`, so the order is always the same.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhpPools(pub BTreeMap<PhpMinor, Vec<Dial>>);

/// Why [`sites_config`] could not build the config. [`CaddyError::code`]
/// gives a stable name that the UI maps to a message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaddyError {
    /// JSON text is UTF-8, so such a path cannot be written in the config.
    /// The site scan already skips these folders; this is a second guard.
    #[error("{} is not valid UTF-8, so it cannot go in the Caddy config", .path.display())]
    PathNotUtf8 { path: PathBuf },
}

impl CaddyError {
    /// Stable error name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            CaddyError::PathNotUtf8 { .. } => "caddy_path_not_utf8",
        }
    }
}

/// Builds the Caddy config for the sites. With no sites it is exactly
/// [`caddy_config`].
///
/// - The `marsa` server (on `http_port`) serves the sites that are not secure,
///   sends every secure site to HTTPS with a `308`, and answers any other
///   host with the greeting of [`caddy_config`].
/// - The `marsa-tls` server (on `https_port`) serves the secure sites, with
///   certificates from our own CA in Caddy. Caddy does not add the CA to the
///   system trust store: the helper does that (M4-09).
///
/// The sites are sorted by name first, so the same sites in any order give
/// the same JSON.
///
/// # Errors
///
/// `caddy_path_not_utf8` if a docroot or a socket path is not valid UTF-8.
pub fn sites_config(
    input: &CaddyInput,
    sites: &[Site],
    pools: &PhpPools,
) -> Result<Value, CaddyError> {
    let mut config = caddy_config(input);
    if sites.is_empty() {
        return Ok(config);
    }
    let mut sorted: Vec<&Site> = sites.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let (secure, plain): (Vec<&Site>, Vec<&Site>) =
        sorted.into_iter().partition(|site| site.secure);
    let secure_hosts: Vec<String> = secure.iter().map(|site| site.host()).collect();

    let mut http_routes = site_routes(&plain, pools)?;
    if !secure_hosts.is_empty() {
        http_routes.push(json!({
            "match": [{ "host": secure_hosts }],
            "handle": [{
                "handler": "static_response",
                "status_code": 308,
                "headers": { "Location": [HTTPS_REDIRECT] },
            }],
            "terminal": true,
        }));
    }
    // The site routes go before the greeting, so the greeting answers only
    // the hosts that are not sites.
    config["apps"]["http"]["servers"]["marsa"]["routes"]
        .as_array_mut()
        .expect("caddy_config always gives the marsa server a list of routes")
        .splice(0..0, http_routes);

    if secure_hosts.is_empty() {
        return Ok(config);
    }
    // Like `$config['apps']['tls'] = [...]` in PHP: a new key is added. This
    // cannot panic, because `caddy_config` builds `apps` and `servers` as
    // objects.
    config["apps"]["http"]["servers"][TLS_SERVER] = json!({
        "listen": [loopback(input.https_port)],
        // No HTTP/3: the `pf` redirect on macOS covers TCP only (ADR 0008).
        "protocols": ["h1", "h2"],
        "routes": site_routes(&secure, pools)?,
        // The `marsa` server already sends HTTP to HTTPS.
        "automatic_https": { "disable_redirects": true },
    });
    config["apps"]["tls"] = json!({
        "automation": {
            "policies": [{
                "subjects": secure_hosts,
                "issuers": [{ "module": "internal", "ca": CA_ID }],
            }],
        },
    });
    config["apps"]["pki"] = json!({
        "certificate_authorities": {
            CA_ID: {
                "name": CA_NAME,
                "root_common_name": CA_NAME,
                "install_trust": false,
            },
        },
    });
    Ok(config)
}

/// The routes of the sites, in the order given.
fn site_routes(sites: &[&Site], pools: &PhpPools) -> Result<Vec<Value>, CaddyError> {
    sites.iter().map(|site| site_route(site, pools)).collect()
}

/// The route of one site: its exact host, then its handlers in a `subroute`.
/// `terminal` stops Caddy before the next routes, such as the greeting.
fn site_route(site: &Site, pools: &PhpPools) -> Result<Value, CaddyError> {
    let routes = match site.kind {
        SiteKind::Laravel | SiteKind::Php => php_routes(site, pools)?,
        SiteKind::Static => {
            let root = utf8(&site.docroot)?;
            vec![json!({
                "handle": [
                    { "handler": "vars", "root": root },
                    { "handler": "file_server" },
                ],
            })]
        }
        SiteKind::NodeProxy { port } => vec![json!({
            "handle": [{
                "handler": "reverse_proxy",
                "upstreams": [{ "dial": loopback(port) }],
            }],
        })],
    };
    Ok(json!({
        "match": [{ "host": [site.host()] }],
        "handle": [{ "handler": "subroute", "routes": routes }],
        "terminal": true,
    }))
}

/// What Caddyfile `php_fastcgi` expands to, as `caddy adapt` (Caddy v2.11.7)
/// prints it. Only `"hide": ["./Caddyfile"]` is left out of `file_server`:
/// there is no Caddyfile. The routes, in order:
///
/// 1. Set the root for the file matchers and the file server.
/// 2. Send a folder with an `index.php` to the same path with a `/` (308).
/// 3. Rewrite to the first that exists: the file, the folder's `index.php`,
///    else `index.php` (the front controller of Laravel).
/// 4. Send `*.php` to the pool over FastCGI.
/// 5. Serve every other file as it is.
///
/// Without a running pool the site answers 503 with a clear text.
fn php_routes(site: &Site, pools: &PhpPools) -> Result<Vec<Value>, CaddyError> {
    let root = utf8(&site.docroot)?;
    let minor = site.php.as_ref().map(PhpMinor::from);
    let dials = minor
        .and_then(|minor| pools.0.get(&minor))
        .filter(|dials| !dials.is_empty());
    let Some(dials) = dials else {
        let body = match minor {
            Some(minor) => PHP_NOT_RUNNING.replace("{version}", &minor.to_string()),
            None => NO_PHP_VERSION.to_string(),
        }
        .replace("{host}", &site.host());
        return Ok(vec![json!({
            "handle": [{ "handler": "static_response", "status_code": 503, "body": body }],
        })]);
    };

    let upstreams = dials
        .iter()
        .map(|dial| Ok(json!({ "dial": dial_address(dial)? })))
        .collect::<Result<Vec<Value>, CaddyError>>()?;
    let mut proxy = json!({
        "handler": "reverse_proxy",
        "transport": { "protocol": "fastcgi", "split_path": [".php"] },
        "upstreams": upstreams,
    });
    if dials.len() > 1 {
        proxy["load_balancing"] = json!({
            "selection_policy": { "policy": "least_conn" },
            "retries": PHP_RETRIES,
        });
    }

    Ok(vec![
        json!({ "handle": [{ "handler": "vars", "root": root }] }),
        json!({
            "match": [{
                "file": { "try_files": ["{http.request.uri.path}/index.php"] },
                "not": [{ "path": ["*/"] }],
            }],
            "handle": [{
                "handler": "static_response",
                "status_code": 308,
                "headers": {
                    "Location": [
                        "{http.request.orig_uri.path}/{http.request.orig_uri.prefixed_query}",
                    ],
                },
            }],
        }),
        json!({
            "match": [{
                "file": {
                    "split_path": [".php"],
                    "try_files": [
                        "{http.request.uri.path}",
                        "{http.request.uri.path}/index.php",
                        "index.php",
                    ],
                    "try_policy": "first_exist_fallback",
                },
            }],
            "handle": [{ "handler": "rewrite", "uri": "{http.matchers.file.relative}" }],
        }),
        json!({ "match": [{ "path": ["*.php"] }], "handle": [proxy] }),
        json!({ "handle": [{ "handler": "file_server" }] }),
    ])
}

/// The `dial` text of one pool address: `unix//run/php.sock` (`unix/` and
/// the absolute path) or `127.0.0.1:9001`.
fn dial_address(dial: &Dial) -> Result<String, CaddyError> {
    match dial {
        Dial::Unix(path) => Ok(format!("unix/{}", utf8(path)?)),
        Dial::Tcp(port) => Ok(loopback(*port)),
    }
}

/// A port on the loopback address. Every address in the config is
/// `127.0.0.1` (architecture.md section 10).
fn loopback(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

/// A path as JSON text. JSON text is UTF-8, so any other path is an error.
fn utf8(path: &Path) -> Result<&str, CaddyError> {
    path.to_str().ok_or_else(|| CaddyError::PathNotUtf8 {
        path: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use semver::Version;

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

    /// Collects every `dial` value in the JSON, at any depth.
    fn dial_addresses(value: &Value, found: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    if key == "dial" {
                        let address = child.as_str().expect("dial values are strings");
                        found.push(address.to_string());
                    }
                    dial_addresses(child, found);
                }
            }
            Value::Array(list) => list.iter().for_each(|v| dial_addresses(v, found)),
            _ => {}
        }
    }

    /// A site for the tests. The paths are literal text, not `join`, so the
    /// snapshots are the same on Windows.
    fn site(name: &str, kind: SiteKind, docroot: &str, php: Option<&str>, secure: bool) -> Site {
        Site {
            name: name.to_string(),
            root: PathBuf::from(format!("/code/{name}")),
            docroot: PathBuf::from(docroot),
            kind,
            php: php.map(|text| Version::parse(text).expect("test versions are valid")),
            secure,
        }
    }

    fn minor(major: u64, minor: u64) -> PhpMinor {
        PhpMinor { major, minor }
    }

    fn shop() -> Site {
        site(
            "shop",
            SiteKind::Laravel,
            "/code/shop/public",
            Some("8.4.3"),
            true,
        )
    }

    /// Not sorted by name on purpose.
    fn four_sites() -> Vec<Site> {
        vec![
            shop(),
            site(
                "legacy",
                SiteKind::Php,
                "/code/legacy",
                Some("8.3.14"),
                false,
            ),
            site("docs", SiteKind::Static, "/code/docs/public", None, true),
            site(
                "vite",
                SiteKind::NodeProxy { port: 5173 },
                "/code/vite",
                None,
                true,
            ),
        ]
    }

    /// php-fpm on macOS and Linux: one socket per PHP line.
    fn socket_pools() -> PhpPools {
        PhpPools(BTreeMap::from([
            (
                minor(8, 3),
                vec![Dial::Unix(PathBuf::from("/run/php-8.3.sock"))],
            ),
            (
                minor(8, 4),
                vec![Dial::Unix(PathBuf::from("/run/php-8.4.sock"))],
            ),
        ]))
    }

    /// php-cgi on Windows: four processes for PHP 8.4.
    fn windows_pools() -> PhpPools {
        PhpPools(BTreeMap::from([(
            minor(8, 4),
            (9001..=9004).map(Dial::Tcp).collect(),
        )]))
    }

    fn build(sites: &[Site], pools: &PhpPools) -> Value {
        sites_config(&CaddyInput::default(), sites, pools).expect("build the config")
    }

    #[test]
    fn one_secure_laravel_site_snapshot() {
        insta::assert_json_snapshot!("one_secure_laravel_site", build(&[shop()], &socket_pools()));
    }

    #[test]
    fn four_sites_snapshot() {
        insta::assert_json_snapshot!("four_sites", build(&four_sites(), &socket_pools()));
    }

    #[test]
    fn laravel_on_four_tcp_upstreams_snapshot() {
        insta::assert_json_snapshot!(
            "laravel_on_four_tcp_upstreams",
            build(&[shop()], &windows_pools())
        );
    }

    #[test]
    fn php_site_without_a_pool_snapshot() {
        let legacy = site(
            "legacy",
            SiteKind::Php,
            "/code/legacy",
            Some("8.3.14"),
            false,
        );
        insta::assert_json_snapshot!(
            "php_site_without_a_pool",
            build(&[legacy], &windows_pools())
        );
    }

    #[test]
    fn no_sites_is_the_default_config() {
        let input = CaddyInput {
            admin_port: 1,
            http_port: 2,
            https_port: 3,
        };
        assert_eq!(
            sites_config(&input, &[], &PhpPools::default()),
            Ok(caddy_config(&input))
        );
    }

    #[test]
    fn every_address_with_sites_is_loopback() {
        // Unix sockets for PHP 8.3, four TCP upstreams for PHP 8.4.
        let mut pools = windows_pools();
        pools.0.extend(socket_pools().0.into_iter().take(1));
        let config = build(&four_sites(), &pools);

        let mut listen = Vec::new();
        listen_addresses(&config, &mut listen);
        // The admin address and the two servers.
        assert_eq!(listen.len(), 3);
        for address in &listen {
            assert!(address.starts_with("127.0.0.1:"), "bad address: {address}");
        }

        let mut dials = Vec::new();
        dial_addresses(&config, &mut dials);
        let (unix, tcp): (Vec<&String>, Vec<&String>) =
            dials.iter().partition(|dial| dial.starts_with("unix/"));
        assert_eq!(unix, ["unix//run/php-8.3.sock"]);
        // Four php-cgi processes and one Node proxy.
        assert_eq!(tcp.len(), 5);
        for address in tcp {
            assert!(address.starts_with("127.0.0.1:"), "bad dial: {address}");
        }
    }

    #[test]
    fn input_order_does_not_matter() {
        let sites = four_sites();
        let expected = serde_json::to_string(&build(&sites, &socket_pools())).expect("to text");
        let mut reversed = sites.clone();
        reversed.reverse();
        let mut rotated = sites;
        rotated.rotate_left(1);
        for other in [reversed, rotated] {
            let text = serde_json::to_string(&build(&other, &socket_pools())).expect("to text");
            assert_eq!(text, expected);
        }
    }

    #[test]
    fn php_without_a_version_or_with_an_empty_pool_answers_503() {
        let no_version = Site {
            php: None,
            secure: false,
            ..shop()
        };
        let not_running = Site {
            secure: false,
            ..shop()
        };
        let empty = PhpPools(BTreeMap::from([(minor(8, 4), Vec::new())]));
        let cases = [
            (
                no_version,
                "Marsa: no PHP version is installed for shop.test.",
            ),
            (not_running, "Marsa: PHP 8.4 is not running for shop.test."),
        ];
        for (site, body) in cases {
            let config = build(&[site], &empty);
            let site_route = &config["apps"]["http"]["servers"]["marsa"]["routes"][0];
            let handler = &site_route["handle"][0]["routes"][0]["handle"][0];
            assert_eq!(handler["handler"], "static_response");
            assert_eq!(handler["status_code"], 503);
            assert_eq!(handler["body"], body);
        }
    }

    // A Unix path is any bytes. Nothing is written to disk, so this runs on
    // macOS too.
    #[cfg(unix)]
    #[test]
    fn non_utf8_path_is_an_error() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let bad = PathBuf::from(OsString::from_vec(b"/code/bad\xff".to_vec()));
        let input = CaddyInput::default();

        let docs = Site {
            docroot: bad.clone(),
            ..site("docs", SiteKind::Static, "/code/docs", None, true)
        };
        let err = sites_config(&input, &[docs], &PhpPools::default()).expect_err("bad docroot");
        assert_eq!(err.code(), "caddy_path_not_utf8");
        assert_eq!(err, CaddyError::PathNotUtf8 { path: bad.clone() });

        let pools = PhpPools(BTreeMap::from([(
            minor(8, 4),
            vec![Dial::Unix(bad.clone())],
        )]));
        let err = sites_config(&input, &[shop()], &pools).expect_err("bad socket");
        assert_eq!(err, CaddyError::PathNotUtf8 { path: bad });
    }
}
