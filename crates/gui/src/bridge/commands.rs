//! The user's commands that stand alone: each runs on a task of its session
//! and tells the window how it went. Reads that must stay in order have
//! readers of their own (`mail`, `dumps`, `site_log`).

use marsa_core::client::{ClientError, DaemonClient};
use marsa_core::types::SitePatch;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinSet;

use super::{Command, Window, autostart, db_browser, hosts, notice, sites_failed, xdebug};
use crate::model::{Change, InstallJob, Removal, Update};

/// Runs the command on its own task, so a slow restart or a setup that waits
/// for the password does not hold up the events. The new state arrives as
/// an event, so the daemon's answer is not passed on: an older answer could
/// arrive after a newer event. Only a failure, an accepted park or link for
/// the notice line, whether a [`Change`] was saved (its list shows the pick
/// until then), the id of an accepted install, the setup report, whether a
/// removal was done, and the autostart and Xdebug states (no event carries
/// them) are sent here. After an uninstall, `reread` asks the session to
/// read the runtimes again.
pub(super) fn run_command(
    tasks: &mut JoinSet<()>,
    client: &DaemonClient,
    command: Command,
    window: Window,
    reread: UnboundedSender<()>,
) {
    let client = client.clone();
    tasks.spawn(async move {
        let update = match command {
            Command::Service { service, action } => {
                let result = client
                    .run_service_action(&service.to_string(), action)
                    .await;
                result
                    .err()
                    .map(|err| notice(&format!("cannot {action} {service}"), &err))
            }
            Command::Db(command) => db_browser::run(&client, command, &window).await,
            Command::Park(folder) => Some(match client.park(&folder).await {
                Ok(_) => {
                    tracing::info!(folder = %folder.display(), "folder parked");
                    Update::Parked(folder)
                }
                Err(err) => sites_failed(&format!("cannot park {}", folder.display()), &err),
            }),
            Command::Link { name, folder } => Some(match client.link(&name, &folder).await {
                Ok(_) => {
                    tracing::info!(site = %name, folder = %folder.display(), "folder linked");
                    Update::Linked { name, folder }
                }
                Err(err) => {
                    let what = format!("cannot link {} as {name}", folder.display());
                    sites_failed(&what, &err)
                }
            }),
            Command::Secure { site, secure } => {
                let patch = SitePatch {
                    secure: Some(secure),
                    ..SitePatch::default()
                };
                let result = client.update_site(&site, &patch).await;
                if result.is_ok() {
                    tracing::info!(site = %site, secure, "HTTPS setting changed");
                }
                result
                    .err()
                    .map(|err| sites_failed(&format!("cannot change HTTPS for {site}"), &err))
            }
            Command::SetWorker { kind, site, on } => {
                match client.set_worker(site.as_str(), kind, on).await {
                    Ok(_) => {
                        tracing::info!(site = %site, worker = kind.prefix(), on, "worker setting changed");
                        None
                    }
                    Err(err) => {
                        let (worker, code) = (kind.prefix(), err.code());
                        tracing::warn!(code, error = %err, site = %site, worker, "cannot change the worker");
                        let code = code.to_owned();
                        Some(Update::WorkerNotSaved { kind, site, code })
                    }
                }
            }
            Command::Save(change) => Some(save(&client, change).await),
            Command::Install(job) => Some(install(&client, job).await),
            Command::Setup => Some(match client.run_setup().await {
                Ok(report) => {
                    let error = report.error.as_ref().map(|error| error.code.as_str());
                    tracing::info!(error, "setup finished");
                    Update::SetupDone(report)
                }
                Err(err) => {
                    tracing::warn!(code = err.code(), error = %err, "setup did not run");
                    Update::SetupFailed(err.code().to_owned())
                }
            }),
            Command::SetAutostart(enabled) => Some(match client.set_autostart(enabled).await {
                Ok(state) => {
                    tracing::info!(enabled, "autostart changed");
                    Update::Autostart(state.into())
                }
                Err(err) => {
                    tracing::warn!(code = err.code(), error = %err, "cannot change autostart");
                    window.send(Update::AutostartFailed(err.code().to_owned()));
                    // The box showed the new state: show the real one.
                    autostart(&client).await
                }
            }),
            Command::Remove(removal) => {
                let uninstall = matches!(removal, Removal::Uninstall { .. });
                window.send(remove(&client, removal).await);
                // Also after a refusal: `version_not_installed` means the
                // list is old. A send fails only once the session has
                // ended, and the next session reads everything.
                if uninstall {
                    let _ = reread.send(());
                }
                None
            }
            Command::SyncHosts => {
                let update = match client.run_sync_hosts().await {
                    Ok(report) => {
                        let error = report.error.as_ref().map(|error| error.code.as_str());
                        tracing::info!(error, "hosts sync finished");
                        Update::HostsSynced(report)
                    }
                    Err(err) => {
                        tracing::warn!(code = err.code(), error = %err, "hosts sync did not run");
                        Update::HostsSyncFailed(err.code().to_owned())
                    }
                };
                // No event tells of the new block: read it again.
                if window.send(update) {
                    window.send(hosts(&client).await);
                }
                None
            }
            // The session's mail reader runs these, one at a time.
            Command::ReadMail | Command::ReadMailText(_) | Command::ClearMail => None,
            // The session's dumps reader runs these, one at a time.
            Command::ReadDumps | Command::ClearDumps | Command::SetDumpsCapture(_) => None,
            // The session's site log reader runs these.
            Command::FollowSiteLog(_) | Command::StopSiteLog => None,
            Command::SetXdebug(enabled) => Some(match client.set_xdebug(enabled).await {
                Ok(state) => {
                    tracing::info!(enabled, "Xdebug changed");
                    Update::XdebugAnswered(Ok(state))
                }
                Err(err) => {
                    tracing::warn!(code = err.code(), error = %err, "cannot change Xdebug");
                    window.send(Update::XdebugAnswered(Err(err.code().to_owned())));
                    // The box showed the new state: show the real one.
                    xdebug(&client).await
                }
            }),
        };
        if let Some(update) = update {
            window.send(update);
        }
    });
}

/// Asks the daemon for `job`. Its progress comes as `install` events on the
/// stream this session opened first, so none is missed.
async fn install(client: &DaemonClient, job: InstallJob) -> Update {
    let version = job.version.as_deref();
    match client.install(job.runtime, version, false).await {
        Ok(accepted) => {
            let runtime = job.runtime.dir_name();
            let version = version.unwrap_or("newest");
            tracing::info!(id = accepted.id, runtime, version, "install accepted");
            Update::InstallAccepted {
                job,
                id: accepted.id,
            }
        }
        Err(err) => {
            tracing::warn!(code = err.code(), error = %err, ?job, "install refused");
            let code = err.code().to_owned();
            Update::InstallRefused { job, code }
        }
    }
}

/// Asks the daemon for `removal`. `Removed`, or `NotRemoved` with the error
/// code and, for `version_in_use`, what uses the version.
async fn remove(client: &DaemonClient, removal: Removal) -> Update {
    let result = match &removal {
        Removal::Uninstall { runtime, version } => client
            .run_runtime_uninstall(*runtime, &version.to_string())
            .await
            .map(drop),
        Removal::Unlink { site } => client.unlink(site).await.map(drop),
        Removal::Forget { folder } => client.forget(folder).await.map(drop),
        // The window sends `ClearMail` for this one, to the mail reader,
        // which also sends the list after; this deletes the same.
        Removal::Mail => client.clear_mail().await.map(drop),
    };
    match result {
        Ok(()) => {
            tracing::info!(?removal, "removed");
            Update::Removed(removal)
        }
        Err(err) => {
            tracing::warn!(code = err.code(), error = %err, ?removal, "cannot remove");
            let code = err.code().to_owned();
            let in_use = match err {
                ClientError::InUse { in_use, .. } => Some(*in_use),
                _ => None,
            };
            Update::NotRemoved {
                removal,
                code,
                in_use,
            }
        }
    }
}

/// Sends `change` to the daemon. `Saved` or `NotSaved` with the error code.
async fn save(client: &DaemonClient, change: Change) -> Update {
    let result = match &change {
        Change::Default { runtime, version } => {
            client.set_default(*runtime, version).await.map(drop)
        }
        Change::SitePhp { site, php } => {
            let patch = SitePatch {
                php: Some(php.clone()),
                ..SitePatch::default()
            };
            client.update_site(site, &patch).await.map(drop)
        }
    };
    match result {
        Ok(()) => {
            tracing::info!(?change, "setting saved");
            Update::Saved(change)
        }
        Err(err) => {
            tracing::warn!(code = err.code(), error = %err, ?change, "cannot save the setting");
            let code = err.code().to_owned();
            Update::NotSaved { change, code }
        }
    }
}
