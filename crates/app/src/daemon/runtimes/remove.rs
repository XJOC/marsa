//! `DELETE /v1/runtimes/{tool}/{version}`: the checks, and the uninstall itself.

use marsa_core::codes;
use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use marsa_core::config::{self, ConfigPaths, GlobalConfig, SiteSettings, SitesConfig};
use marsa_core::databases::ServerKind;
use marsa_core::installer::uninstall::{self, Retired, UninstallError};
use marsa_core::resolver::{self, Tool};
use marsa_core::runtimes::{Runtime, RuntimesDir};
use marsa_core::types::{SiteKind, Uninstalled, VersionInUse};
use semver::Version;

use super::api::{bad_version, default_tool, defaults_mut, runtime};
use super::install::{Started, busy};
use crate::daemon::http::routes::HttpError;
use crate::daemon::http::{self, Rejected as _, global_error};
use crate::daemon::services::databases::client_shims;
use crate::daemon::services::php::{fpm_id, wanted_pools};
use crate::daemon::state::Daemon;

pub(super) async fn uninstall(
    State(daemon): State<Arc<Daemon>>,
    path: Result<UrlPath<(String, String)>, PathRejection>,
) -> Result<Json<Uninstalled>, HttpError> {
    let UrlPath((tool, text)) = path.map_err(|err| err.reject(codes::INVALID_PATH))?;
    let runtime = runtime(&tool)?;
    let started = Started::claim(&daemon).ok_or_else(busy)?;
    // A task of its own, so a client that hangs up cannot stop it between
    // the rename and the delete.
    let task = tokio::spawn(remove_version(started, runtime, text));
    task.await.unwrap_or_else(|err| {
        tracing::error!(error = %err, "the uninstall task failed");
        let message = "the uninstall task failed";
        Err(HttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            codes::INTERNAL_ERROR,
            message,
        ))
    })
}

/// The uninstall, from the checks to the new site list. `started` keeps
/// installs out until the end.
async fn remove_version(
    started: Started,
    runtime: Runtime,
    text: String,
) -> Result<Json<Uninstalled>, HttpError> {
    let daemon = Arc::clone(&started.daemon);
    // A start in a database server's turn may be about to run a version, and is
    // not in `running_program` yet. Held until the folder is moved aside: a
    // tokio lock held across `.await` as a serialization token (manager
    // decision 2026-11-14).
    let server_kind = ServerKind::of_runtime(runtime);
    let server_turn = match server_kind {
        Some(kind) => match daemon.server(kind).try_turn() {
            Some(turn) => Some(turn),
            None => {
                let message = format!(
                    "another action on {} runs, and a start may pick this version; nothing was \
                     uninstalled",
                    kind.name()
                );
                return Err(HttpError::new(
                    StatusCode::CONFLICT,
                    codes::SERVICE_BUSY,
                    message,
                ));
            }
        },
        None => None,
    };
    let caddy = daemon.caddy.running_program(&daemon);
    let server = match server_kind {
        Some(kind) => daemon.server(kind).program_in_use(&daemon).await,
        None => None,
    };
    let worker = Arc::clone(&daemon);
    let (version, retired, cleared) = http::blocking("uninstall", move || {
        // `marsa use` waits, so the version cannot become the default
        // between the check and the rename.
        let _one = worker.lock_global();
        let running = Running {
            caddy: caddy.as_deref(),
            server: server.as_deref(),
        };
        retire_unused(&worker.data_dir, runtime, text.trim(), running)
    })
    .await?;
    // Moved aside: a start now finds no such version.
    drop(server_turn);
    if let Some(default) = cleared {
        let tool = runtime.dir_name();
        tracing::info!(runtime = tool, %default, "cleared the default of the last version");
    }
    // The version is hidden now, so no pool can start with it again.
    if runtime == Runtime::Php {
        stop_pools(&daemon, &version).await;
    }
    let path = retired.path().to_path_buf();
    match tokio::task::spawn_blocking(move || retired.remove()).await {
        Ok(Ok(())) => {}
        // The version is gone either way; `marsad` deletes the rest at its
        // next start.
        Ok(Err(err)) => tracing::warn!(code = err.code(), error = %err, "the delete stopped"),
        Err(err) => tracing::warn!(path = %path.display(), error = %err, "the delete stopped"),
    }
    tracing::info!(runtime = runtime.dir_name(), %version, "uninstalled");
    // The clients (`psql`) go with the last version of their server.
    if let Some(kind) = server_kind {
        client_shims::sync(&daemon, kind).await;
    }
    // Sites that ran this version now resolve to another one, or to none,
    // and `sites_changed` makes Caddy and php-fpm follow. `rebuild_sites`
    // logs its errors.
    let _ = daemon.rebuild_sites().await;
    drop(started);
    Ok(Json(Uninstalled {
        runtime,
        version: version.to_string(),
    }))
}

/// Picks the installed version `text` names, checks that nothing uses it,
/// and moves its folder aside. Also returns the default it cleared, if any:
/// the default of the only installed version goes with it. A
/// refusal changes nothing on disk. `running` has the programs that run
/// now. The caller holds [`Daemon::lock_global`].
fn retire_unused(
    data_dir: &Path,
    runtime: Runtime,
    text: &str,
    running: Running<'_>,
) -> Result<(Version, Retired, Option<String>), HttpError> {
    resolver::best_match(text, &[]).map_err(|err| bad_version(text, err))?;
    let runtimes = RuntimesDir::in_dir(data_dir);
    let installed = runtimes.installed(runtime).map_err(|err| {
        let status = StatusCode::INTERNAL_SERVER_ERROR;
        HttpError::new(status, err.code(), err.to_string())
    })?;
    let matching: Vec<&Version> = installed
        .iter()
        .filter(|version| picks(text, std::slice::from_ref(*version), version))
        .collect();
    let version = match matching[..] {
        [one] => one.clone(),
        [] => {
            let message = format!(
                "no installed {} version matches {text:?}",
                runtime.dir_name()
            );
            let status = StatusCode::NOT_FOUND;
            return Err(HttpError::new(
                status,
                codes::VERSION_NOT_INSTALLED,
                message,
            ));
        }
        ref many => {
            let many: Vec<String> = many.iter().map(ToString::to_string).collect();
            let message = format!("{text:?} matches {}; name one", many.join(", "));
            let status = StatusCode::CONFLICT;
            return Err(HttpError::new(status, codes::VERSION_AMBIGUOUS, message));
        }
    };
    let paths = ConfigPaths::in_dir(data_dir);
    let global = config::load_global(&paths.global).map_err(|err| global_error(&err))?;
    let sites = config::load_sites(&paths.sites).map_err(|err| {
        tracing::warn!(code = err.code(), error = %err, "sites.toml");
        HttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            err.code(),
            err.to_string(),
        )
    })?;
    let mut used = in_use(runtime, &version, &installed, &global, &sites);
    let dir = runtimes.version_dir(runtime, &version);
    let runs = |program: Option<&Path>| program.is_some_and(|program| program.starts_with(&dir));
    used.caddy = runs(running.caddy);
    used.server = runs(running.server);
    // The default of the only installed version has no other version to
    // pick, so it goes with this one. The PHP sites that run this last PHP
    // would have none after it, so they keep the version instead, whether
    // the default picks it or not.
    let last = installed.len() == 1;
    let clear = match default_tool(runtime) {
        Some(tool) if last => used.default.take().map(|default| (tool, default)),
        _ => None,
    };
    if last && runtime == Runtime::Php {
        let mut left = sites_running(data_dir, &sites, &version);
        // A site set to it in `sites.toml` is named once, as set to it.
        left.retain(|name| !used.sites.contains(name));
        used.sites_left_without_php = left;
    }
    if used.any() {
        let message = format!("{} {version} is in use: {used:?}", runtime.dir_name());
        let mut err = HttpError::new(StatusCode::CONFLICT, used.code(), message);
        err.in_use = Some(Box::new(used));
        return Err(err);
    }
    // The default first, then the move. The move fails more often (on
    // Windows: a program from the folder runs), and then the default is
    // written back, so a failed uninstall keeps it. In no order of failures
    // does a default name a version that is gone: at worst the version
    // stays without its default, and the log says so.
    if let Some((tool, _)) = &clear {
        let mut cleared = global.clone();
        defaults_mut(&mut cleared, *tool).default = None;
        config::save_global(&paths.global, &cleared).map_err(|err| global_error(&err))?;
    }
    let retired = match uninstall::retire(&runtimes, runtime, &version) {
        Ok(retired) => retired,
        Err(err) => {
            tracing::warn!(code = err.code(), error = %err, "cannot uninstall");
            let mut answer = uninstall_error(&err);
            if clear.is_some()
                && let Err(put_back) = config::save_global(&paths.global, &global)
            {
                let code = put_back.code();
                tracing::error!(code, error = %put_back, "cannot put the default back");
                answer
                    .message
                    .push_str("; the default it had could not be put back");
            }
            return Err(answer);
        }
    };
    Ok((version, retired, clear.map(|(_, default)| default)))
}

/// The programs that run now, which an uninstall must not take away.
#[derive(Debug, Clone, Copy)]
struct Running<'a> {
    /// The running Caddy's program.
    caddy: Option<&'a Path>,
    /// The running database server's program, or the one our old server
    /// runs, which a crashed `marsad` left running
    /// (`ServerManager::program_in_use`).
    server: Option<&'a Path>,
}

/// The answer for an uninstall that could not move the folder aside.
fn uninstall_error(err: &UninstallError) -> HttpError {
    let status = match err {
        UninstallError::Busy => StatusCode::CONFLICT,
        UninstallError::NotInstalled { .. } => StatusCode::NOT_FOUND,
        UninstallError::Io { .. } => StatusCode::INTERNAL_SERVER_ERROR,
    };
    HttpError::new(status, err.code(), err.to_string())
}

/// The PHP sites that run `version` now, by name, whatever picked it: the
/// default, a version file, a `composer.json` constraint, or `sites.toml`.
/// Only Laravel and PHP sites count, as in `wanted_pools`: a static site or
/// a Node proxy runs no PHP. Builds the site list as the daemon does, so it
/// reads the disk.
fn sites_running(data_dir: &Path, sites: &SitesConfig, version: &Version) -> Vec<String> {
    let list = crate::daemon::sites::build(data_dir, sites);
    list.sites
        .into_iter()
        .filter(|site| matches!(site.kind, SiteKind::Laravel | SiteKind::Php))
        .filter(|site| site.php.as_ref() == Some(version))
        .map(|site| site.name)
        .collect()
}

/// Whether the version text `text` picks `version` among `installed`, as the
/// resolver would.
fn picks(text: &str, installed: &[Version], version: &Version) -> bool {
    resolver::best_match(text, installed).is_ok_and(|picked| picked.as_ref() == Some(version))
}

/// The default and the sites' own settings that pick `version` now. Composer
/// and Caddy have neither.
fn in_use(
    runtime: Runtime,
    version: &Version,
    installed: &[Version],
    global: &GlobalConfig,
    sites: &SitesConfig,
) -> VersionInUse {
    let setting: fn(&SiteSettings) -> Option<&String> = match runtime {
        Runtime::Php => |site| site.php.as_ref(),
        Runtime::Node => |site| site.node.as_ref(),
        Runtime::Composer
        | Runtime::Caddy
        | Runtime::Postgres
        | Runtime::Mysql
        | Runtime::Valkey => |_| None,
    };
    let default = default_tool(runtime).and_then(|tool| match tool {
        Tool::Php => global.php.default.as_ref(),
        Tool::Node => global.node.default.as_ref(),
    });
    let uses = |text: &&String| picks(text, installed, version);
    VersionInUse {
        version: version.to_string(),
        default: default.filter(uses).cloned(),
        sites: sites
            .sites
            .iter()
            .filter(|(_, site)| setting(site).is_some_and(|text| uses(&text)))
            .map(|(name, _)| name.clone())
            .collect(),
        sites_left_without_php: Vec::new(),
        caddy: false,
        server: false,
    }
}

/// Stops, through the supervisor, each php-fpm pool that the sites run with
/// `version`. The sync task starts the line again after `sites_changed`, if
/// a site still needs it.
async fn stop_pools(daemon: &Daemon, version: &Version) {
    let sites = daemon.sites().map(|list| list.sites).unwrap_or_default();
    for (minor, picked) in wanted_pools(&sites) {
        if picked == *version
            && let Some(handle) = daemon.take_proc(fpm_id(minor))
        {
            tracing::info!(php = %minor, "stopping php-fpm before its uninstall");
            if let Err(err) = handle.stop().await {
                tracing::warn!(php = %minor, %err, "the php-fpm task failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The PostgreSQL that runs keeps its version; one that does not run
    /// goes, and its cluster in `databases/` stays.
    #[test]
    fn the_running_postgres_keeps_its_version_and_databases_stay() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        let version_dir =
            RuntimesDir::in_dir(data).version_dir(Runtime::Postgres, &Version::new(18, 7, 0));
        std::fs::create_dir_all(version_dir.join("bin")).unwrap();
        let cluster = data.join("databases/postgres/18");
        std::fs::create_dir_all(&cluster).unwrap();
        std::fs::write(cluster.join("PG_VERSION"), "18\n").unwrap();
        let program = version_dir.join("bin/postgres");

        let running = Running {
            caddy: None,
            server: Some(&program),
        };
        let err = retire_unused(data, Runtime::Postgres, "18.7.0", running).unwrap_err();
        assert_eq!(err.code, "version_in_use");
        let in_use = err.in_use.expect("the facts");
        assert!(in_use.server && !in_use.caddy, "{in_use:?}");
        assert!(version_dir.is_dir(), "a refusal changes nothing");

        let stopped = Running {
            caddy: None,
            server: None,
        };
        let (version, retired, cleared) =
            retire_unused(data, Runtime::Postgres, "18.7.0", stopped).unwrap();
        assert_eq!((version, cleared), (Version::new(18, 7, 0), None));
        retired.remove().unwrap();
        assert!(!version_dir.exists());
        assert_eq!(
            std::fs::read_to_string(cluster.join("PG_VERSION")).unwrap(),
            "18\n"
        );
    }
}
