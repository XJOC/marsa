//! The runtimes routes.
//!
//! | Route | Answer |
//! | --- | --- |
//! | `GET /v1/runtimes` | `RuntimeList`: the installed versions and the default of each runtime |
//! | `POST /v1/runtimes/{tool}/install` | 202 `InstallAccepted`, then `install` events on `/v1/events` |
//! | `PUT /v1/runtimes/{tool}/default` | `DefaultVersion`: the new default of PHP or Node |
//! | `DELETE /v1/runtimes/{tool}/{version}` | `Uninstalled`: the version whose folder is gone |
//!
//! The daemon runs the installer (`marsa_core::installer::install`) in a task
//! of its own and sends each `InstallEvent` on the event bus. The last event
//! of an install is `done` or `failed { code }`. In M2 `marsa install` called
//! the installer itself; now only the daemon writes to `runtimes/`.
//!
//! One install at a time: a second request while one runs gets 409
//! `install_busy`. The installer's own file lock (`runtimes/.tmp/install.lock`)
//! still guards against another process; that case ends as `failed` with the
//! same code.
//!
//! `PUT .../default` takes `{"version": "8.3"}`. The text must match an
//! installed version, by the rules of a `.php-version` file; then it goes
//! into `global.toml` as written, `default_changed` goes to `/v1/events`, and
//! the site list is built again. A site that follows the default gets the
//! new PHP, and `sites_changed` makes Caddy and php-fpm follow. A refused
//! request leaves `global.toml` as it was.
//!
//! `DELETE .../{version}` takes `8.3.12`, or `8.3` when one installed version
//! matches. It takes the install's guard, so it never runs with an install,
//! and refuses (409 `version_in_use`, nothing changed) a version that the
//! default in `global.toml` or a site's own setting in `sites.toml` picks
//! now, or that the running Caddy or PostgreSQL runs from. A PostgreSQL
//! version is refused (409 `service_busy`) while an action on PostgreSQL
//! runs, such as a start that may pick it; the uninstall then holds
//! PostgreSQL's turn until the folder is moved aside. Then the folder is
//! moved aside (`installer::uninstall`), each php-fpm pool that runs the
//! version is stopped, the folder is deleted, and the site list is built
//! again.
//!
//! The default of the only installed PHP or Node version has no other
//! version to pick, so it does not keep the version: it is cleared from
//! `global.toml` with the uninstall. The Laravel and PHP sites
//! that run the last PHP would have no PHP after it, whatever picked it for
//! them (the default, a version file, `composer.json`), so they still keep
//! it: 409 `sites_lose_php`, with their names. That holds with no default
//! too.
//!
//! PostgreSQL: after an install, `databases::after_install`
//! sets `[postgres] default` (the first install also turns it on and starts
//! it: ADR 0023, the install is the user's request), in the background and
//! in PostgreSQL's turn. A later install of another line never moves a user
//! to its empty cluster. An uninstall never touches `databases/`: it
//! removes `runtimes/postgres/<version>/` only.
//!
//! A database server's install makes the shims of its clients (`psql`,
//! `mysqldump`, `redis-cli`) before `done`, and the uninstall of its last
//! version removes them ([`client_shims`]).

use marsa_core::codes;
use std::path::Path;
use std::sync::Arc;

use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use marsa_core::config::{self, ConfigPaths, GlobalConfig, ToolDefaults};
use marsa_core::resolver::{self, BadRequest, Tool};
use marsa_core::runtimes::{Runtime, RuntimesDir, RuntimesError};
use marsa_core::types::{DefaultRequest, DefaultVersion, Event, RuntimeList, RuntimeVersions};

use super::install::install;
use super::remove::uninstall;
use crate::daemon::api::HttpError;
use crate::daemon::http::{self, Rejected as _, global_error};
use crate::daemon::state::Daemon;

/// The routes, for `api::router` to merge before its fallbacks.
pub fn routes() -> Router<Arc<Daemon>> {
    Router::new()
        .route("/v1/runtimes", get(runtimes))
        .route("/v1/runtimes/{tool}/install", post(install))
        .route("/v1/runtimes/{tool}/default", put(set_default))
        .route("/v1/runtimes/{tool}/{version}", delete(uninstall))
}

async fn runtimes(State(daemon): State<Arc<Daemon>>) -> Result<Json<RuntimeList>, HttpError> {
    // A small folder read per runtime and one small file; not worth a
    // blocking thread.
    list(&daemon.data_dir).map(Json).map_err(|err| {
        tracing::warn!(code = err.code(), error = %err, "cannot list runtimes");
        HttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            err.code(),
            err.to_string(),
        )
    })
}

/// The installed versions of every runtime, from `runtimes/`, and the
/// defaults of PHP and Node, from `global.toml`. A `global.toml` that cannot
/// be read gives no default, as in the site list.
///
/// # Errors
///
/// `runtimes_read` if a runtime folder exists but cannot be read.
pub fn list(data_dir: &Path) -> Result<RuntimeList, RuntimesError> {
    let dir = RuntimesDir::in_dir(data_dir);
    let global = config::load_global(&ConfigPaths::in_dir(data_dir).global).unwrap_or_else(|err| {
        tracing::warn!(code = err.code(), error = %err, "global.toml");
        GlobalConfig::default()
    });
    let runtimes = Runtime::ALL
        .into_iter()
        .map(|runtime| {
            let installed = dir.installed(runtime)?;
            let default = match runtime {
                Runtime::Php => global.php.default.clone(),
                Runtime::Node => global.node.default.clone(),
                Runtime::Composer
                | Runtime::Caddy
                | Runtime::Postgres
                | Runtime::Mysql
                | Runtime::Valkey => None,
            };
            Ok(RuntimeVersions {
                runtime,
                installed: installed.iter().map(ToString::to_string).collect(),
                default,
            })
        })
        .collect::<Result<_, RuntimesError>>()?;
    Ok(RuntimeList { runtimes })
}

/// The `{tool}` path segment.
pub(super) fn runtime(tool: &str) -> Result<Runtime, HttpError> {
    Runtime::ALL
        .into_iter()
        .find(|runtime| runtime.dir_name() == tool)
        .ok_or_else(|| {
            let names: Vec<&str> = Runtime::ALL.iter().map(|r| r.dir_name()).collect();
            let message = format!("no tool named {tool:?}; use one of: {}", names.join(", "));
            HttpError::new(StatusCode::NOT_FOUND, codes::UNKNOWN_TOOL, message)
        })
}

async fn set_default(
    State(daemon): State<Arc<Daemon>>,
    path: Result<UrlPath<String>, PathRejection>,
    body: Result<Json<DefaultRequest>, JsonRejection>,
) -> Result<Json<DefaultVersion>, HttpError> {
    let UrlPath(tool) = path.map_err(|err| err.reject(codes::INVALID_PATH))?;
    let runtime = runtime(&tool)?;
    let tool = default_tool(runtime).ok_or_else(|| no_default_version(runtime))?;
    let Json(DefaultRequest { version }) = body.map_err(|err| err.reject(codes::INVALID_BODY))?;
    // The disk on a blocking thread; the lock is dropped there, before any
    // `.await`.
    let worker = Arc::clone(&daemon);
    let answer = http::blocking("default version", move || {
        let _one = worker.lock_global();
        save_default(&worker.data_dir, tool, version.trim())
    })
    .await?;
    tracing::info!(
        runtime = runtime.dir_name(),
        default = %answer.default,
        version = %answer.version,
        "set the default version"
    );
    // For the window's runtimes list, also when the CLI made the change.
    daemon.publish(Event::DefaultChanged(answer.clone()));
    // The sites that follow the default now get the new PHP, and
    // `sites_changed` makes Caddy and php-fpm follow. Before
    // the answer, so a client that reads it then lists the new versions.
    // Node is in no site yet; then the list stays the same and nothing is
    // sent. `rebuild_sites` logs its errors; the default is saved either way.
    let _ = daemon.rebuild_sites().await;
    Ok(Json(answer))
}

/// Makes `text` the default of `tool` in `global.toml` if it matches an
/// installed version, and returns it with the version it picks now. Writes
/// nothing when it is the default already, and nothing on any error. The
/// caller holds [`Daemon::lock_global`].
///
/// # Errors
///
/// `invalid_version` or `unsupported_alias` (400) for text that is not a
/// version, `version_not_installed` (409) when no installed version matches,
/// `runtimes_read` (500) when the runtime's folder cannot be read, and a
/// `config_*` code (500) when `global.toml` cannot be read or written.
fn save_default(data_dir: &Path, tool: Tool, text: &str) -> Result<DefaultVersion, HttpError> {
    let runtime = Runtime::from(tool);
    // First alone, so text that is not a version is refused whatever is
    // installed, as `PATCH /v1/sites/{name}` refuses it.
    resolver::best_match(text, &[]).map_err(|err| bad_version(text, err))?;
    let installed = RuntimesDir::in_dir(data_dir)
        .installed(runtime)
        .map_err(|err| {
            tracing::warn!(code = err.code(), error = %err, "cannot list the installed versions");
            HttpError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                err.code(),
                err.to_string(),
            )
        })?;
    let version = match resolver::best_match(text, &installed) {
        Ok(Some(version)) => version,
        Ok(None) => {
            let message = format!(
                "no installed {} version matches {text:?}",
                runtime.dir_name()
            );
            return Err(HttpError::new(
                StatusCode::CONFLICT,
                codes::VERSION_NOT_INSTALLED,
                message,
            ));
        }
        Err(err) => return Err(bad_version(text, err)),
    };
    let path = ConfigPaths::in_dir(data_dir).global;
    let mut global = config::load_global(&path).map_err(|err| global_error(&err))?;
    let defaults = defaults_mut(&mut global, tool);
    if defaults.default.as_deref() != Some(text) {
        defaults.default = Some(text.to_owned());
        config::save_global(&path, &global).map_err(|err| global_error(&err))?;
    }
    Ok(DefaultVersion {
        runtime,
        default: text.to_owned(),
        version: version.to_string(),
    })
}

/// The resolver's tool for `runtime`, or `None` for a runtime with no default
/// version (Composer, Caddy): the newest installed one is used.
pub(super) fn default_tool(runtime: Runtime) -> Option<Tool> {
    match runtime {
        Runtime::Php => Some(Tool::Php),
        Runtime::Node => Some(Tool::Node),
        Runtime::Composer
        | Runtime::Caddy
        | Runtime::Postgres
        | Runtime::Mysql
        | Runtime::Valkey => None,
    }
}

/// The `[php]` or `[node]` table of `global.toml`.
pub(super) fn defaults_mut(global: &mut GlobalConfig, tool: Tool) -> &mut ToolDefaults {
    match tool {
        Tool::Php => &mut global.php,
        Tool::Node => &mut global.node,
    }
}

fn no_default_version(runtime: Runtime) -> HttpError {
    let message = format!(
        "{} has no default version; the newest installed one is used",
        runtime.dir_name()
    );
    HttpError::new(StatusCode::BAD_REQUEST, codes::NO_DEFAULT_VERSION, message)
}

/// The same code and status as `PATCH /v1/sites/{name}` gives for this text.
pub(super) fn bad_version(text: &str, err: BadRequest) -> HttpError {
    let message = format!("{text:?}: {err}");
    HttpError::new(StatusCode::BAD_REQUEST, err.code(), message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use marsa_core::data_dir::DataDir;

    #[test]
    fn an_unknown_tool_is_refused_with_every_known_name() {
        assert_eq!(runtime("postgres").unwrap(), Runtime::Postgres);
        let err = runtime("ruby").unwrap_err();
        assert_eq!(err.code, "unknown_tool");
        for known in Runtime::ALL {
            assert!(err.message.contains(known.dir_name()), "{}", err.message);
        }
    }

    #[test]
    fn only_the_known_tools_are_known() {
        for (tool, runtime) in [
            ("php", Runtime::Php),
            ("node", Runtime::Node),
            ("composer", Runtime::Composer),
            ("caddy", Runtime::Caddy),
            ("postgres", Runtime::Postgres),
            ("mysql", Runtime::Mysql),
            ("valkey", Runtime::Valkey),
        ] {
            assert_eq!(super::runtime(tool).unwrap(), runtime);
        }
        for bad in ["ruby", "PHP", "", "php "] {
            let err = super::runtime(bad).unwrap_err();
            assert_eq!(
                (err.status, err.code),
                (StatusCode::NOT_FOUND, "unknown_tool")
            );
        }
    }

    #[test]
    fn list_reads_every_runtime_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        for dir in [
            "php/8.4.23",
            "php/8.3.27",
            "php/8.4",
            "node/22.11.0",
            "caddy/2.11.7",
            ".tmp",
        ] {
            std::fs::create_dir_all(DataDir::new(tmp.path()).runtimes().join(dir)).unwrap();
        }
        let shown = |got: RuntimeList| -> Vec<_> {
            let row = |r: RuntimeVersions| (r.runtime, r.installed.join(","), r.default);
            got.runtimes.into_iter().map(row).collect()
        };
        let want = [
            (Runtime::Php, "8.3.27,8.4.23".to_owned(), None),
            (Runtime::Node, "22.11.0".to_owned(), None),
            (Runtime::Composer, String::new(), None),
            (Runtime::Caddy, "2.11.7".to_owned(), None),
            (Runtime::Postgres, String::new(), None),
            (Runtime::Mysql, String::new(), None),
            (Runtime::Valkey, String::new(), None),
        ];
        assert_eq!(shown(list(tmp.path()).unwrap()), want);

        // The defaults as written in `global.toml`, for PHP and Node only.
        let mut global = GlobalConfig::default();
        global.php.default = Some("8.4".into());
        global.node.default = Some("22".into());
        config::save_global(&ConfigPaths::in_dir(tmp.path()).global, &global).unwrap();
        let [php, node, composer, caddy, postgres, mysql, valkey] = want;
        let want = [
            (php.0, php.1, Some("8.4".to_owned())),
            (node.0, node.1, Some("22".to_owned())),
            composer,
            caddy,
            postgres,
            mysql,
            valkey,
        ];
        assert_eq!(shown(list(tmp.path()).unwrap()), want);

        // A broken `global.toml` gives no default; the versions still show.
        std::fs::write(ConfigPaths::in_dir(tmp.path()).global, "php = 1\n").unwrap();
        let got = list(tmp.path()).unwrap();
        assert!(got.runtimes.iter().all(|r| r.default.is_none()), "{got:?}");
    }

    // `cfg(unix)` outside `platform`: Unix file modes are the simplest way to
    // make a folder unreadable in a test.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_is_runtimes_read() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let php = DataDir::new(tmp.path()).runtimes().join("php");
        std::fs::create_dir_all(&php).unwrap();
        std::fs::set_permissions(&php, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = list(tmp.path());
        std::fs::set_permissions(&php, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Root can read anything; then there is no error to check.
        if let Err(err) = result {
            assert_eq!(err.code(), "runtimes_read");
        }
    }
}
