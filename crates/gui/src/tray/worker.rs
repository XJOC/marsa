//! The worker: one thread with a tokio runtime that owns the daemon client,
//! like the window's bridge. It tells the menu what changed through a
//! callback that wakes the event loop, and takes commands on a channel.
//! [`DaemonClient`] never leaves this module, so the token never reaches the
//! menu code, and no text sent from here holds it.
//!
//! The worker starts `marsad` at the start, and again only on "Start Marsa".
//! While the daemon is stopped it only looks for one every few seconds (a
//! lock-file probe, no network), so "Stop Marsa" stays stopped until the
//! user, the window, or the CLI starts one.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use marsa_core::client::{self, ClientError, DaemonClient, StreamItem};
use marsa_core::platform::daemon_file::DaemonFileError;
use marsa_core::runtimes::Runtime;
use marsa_core::types::{Event, InstallProgress, InstallUpdate};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::menu::{Autostart, Command, Daemon, Update};
use crate::strings;

/// How often a stopped daemon is looked for.
const PROBE: Duration = Duration::from_secs(3);
/// The daemon's answer to `/v1/autostart` on a system it does not support.
const AUTOSTART_UNSUPPORTED: &str = "autostart_unsupported";

/// The menu's end of the worker.
pub struct Handle {
    commands: UnboundedSender<Command>,
}

impl Handle {
    pub fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            tracing::warn!("the worker has stopped; the command is dropped");
        }
    }
}

/// The worker's end of the way to the menu.
#[derive(Clone)]
struct Menu {
    notify: Arc<dyn Fn(Update) -> bool + Send + Sync>,
}

impl Menu {
    /// `false` once the menu is gone.
    fn send(&self, update: Update) -> bool {
        (self.notify)(update)
    }
}

/// Starts the worker thread. `notify` runs on that thread for each update
/// and wakes the event loop; it returns `false` once the loop is gone.
///
/// # Errors
///
/// The OS cannot start a thread.
pub fn start(
    data_dir: PathBuf,
    notify: impl Fn(Update) -> bool + Send + Sync + 'static,
) -> io::Result<Handle> {
    let (commands, commands_rx) = unbounded_channel();
    let menu = Menu {
        notify: Arc::new(notify),
    };
    thread::Builder::new()
        .name("worker".into())
        .spawn(move || run(&data_dir, &menu, commands_rx))?;
    Ok(Handle { commands })
}

fn run(data_dir: &Path, menu: &Menu, commands: UnboundedReceiver<Command>) {
    // One thread is enough: the worker waits on the network most of the time.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    match runtime {
        Ok(runtime) => runtime.block_on(serve(data_dir, menu, commands)),
        Err(err) => {
            tracing::error!(error = %err, "cannot start the worker runtime");
            let code = strings::TRAY_WORKER_FAILED.to_owned();
            menu.send(Update::Daemon(Daemon::Failed(code)));
        }
    }
}

/// Sends each daemon state once: the probe loop would otherwise say
/// "stopped" every few seconds.
struct Reporter<'a> {
    menu: &'a Menu,
    last: Option<Daemon>,
}

impl Reporter<'_> {
    /// `false` once the menu is gone.
    fn daemon(&mut self, daemon: Daemon) -> bool {
        if self.last.as_ref() == Some(&daemon) {
            return true;
        }
        // The state holds an error code at most, never the token.
        tracing::info!(state = ?daemon, "daemon state");
        self.last = Some(daemon.clone());
        self.menu.send(Update::Daemon(daemon))
    }
}

/// How a session ended.
enum End {
    /// The menu is gone.
    Closed,
    /// The event stream ended: the daemon stopped, or is restarting.
    Stopped,
    /// The daemon answered, but its events cannot be followed. A client
    /// error code.
    Failed(String),
}

async fn serve(data_dir: &Path, menu: &Menu, mut commands: UnboundedReceiver<Command>) {
    let mut reporter = Reporter { menu, last: None };
    // `ensure_running` at the start and after "Start Marsa"; a probe
    // otherwise.
    let mut start = true;
    loop {
        let found = if start {
            if !reporter.daemon(Daemon::Starting) {
                return;
            }
            client::ensure_running(data_dir).await.map(Some)
        } else {
            probe(data_dir).await
        };
        start = false;
        let state = match found {
            Ok(Some(client)) => match session(&client, &mut reporter, &mut commands).await {
                End::Closed => return,
                End::Stopped => Daemon::Stopped,
                End::Failed(code) => Daemon::Failed(code),
            },
            Ok(None) => Daemon::Stopped,
            Err(err) => {
                tracing::warn!(code = err.code(), error = %err, "cannot reach the daemon");
                Daemon::Failed(err.code().to_owned())
            }
        };
        if !reporter.daemon(state) {
            return;
        }
        // Until the next probe, or a click.
        tokio::select! {
            () = tokio::time::sleep(PROBE) => {}
            command = commands.recv() => match command {
                None => return,
                Some(Command::Start) => start = true,
                Some(command) => {
                    tracing::debug!(?command, "dropped: the daemon is not running");
                }
            },
        }
    }
}

/// A daemon that holds the lock and answers. `None` if none runs, or one is
/// still starting or stopping: the next probe sees it. Never starts one.
async fn probe(data_dir: &Path) -> Result<Option<DaemonClient>, ClientError> {
    let client = match DaemonClient::connect(data_dir) {
        Ok(client) => client,
        Err(err) if transient(&err) => return Ok(None),
        Err(err) => return Err(err),
    };
    match client.health().await {
        Ok(_) => Ok(Some(client)),
        Err(err) if transient(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

/// Whether the next probe may find the daemon: none runs, or one is starting
/// (no file, or a file being written) or stopping (no answer).
fn transient(err: &ClientError) -> bool {
    matches!(
        err,
        ClientError::NotRunning
            | ClientError::Unreachable { .. }
            | ClientError::DaemonFile(
                DaemonFileError::Missing { .. }
                    | DaemonFileError::Invalid { .. }
                    | DaemonFileError::Unreadable { .. }
            )
    )
}

/// Follows the daemon's events and runs the clicks until the stream ends.
async fn session(
    client: &DaemonClient,
    reporter: &mut Reporter<'_>,
    commands: &mut UnboundedReceiver<Command>,
) -> End {
    // Subscribe before reading the state, so no change falls in between.
    let mut events = match client.events().await {
        Ok(events) => events,
        Err(err) => {
            tracing::warn!(code = err.code(), error = %err, "cannot follow the daemon's events");
            return End::Failed(err.code().to_owned());
        }
    };
    if !reporter.daemon(Daemon::Running) {
        return End::Closed;
    }
    let menu = reporter.menu;
    if !read_php(client, menu).await || !read_autostart(client, menu).await {
        return End::Closed;
    }
    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(StreamItem::Item(event))) => {
                    let sent = match event {
                        Event::DefaultChanged(default) if default.runtime == Runtime::Php => {
                            menu.send(Update::PhpDefault(default.default))
                        }
                        // A new version, and maybe a first default: no
                        // event carries the whole list.
                        Event::Install(InstallProgress { update: InstallUpdate::Done(_), .. }) => {
                            read_php(client, menu).await
                        }
                        Event::ShuttingDown => reporter.daemon(Daemon::Stopping),
                        _ => true,
                    };
                    if !sent {
                        return End::Closed;
                    }
                }
                // Events were dropped, so the menu may be stale.
                Some(Ok(StreamItem::Lagged(count))) => {
                    tracing::warn!(count, "the daemon dropped events; reading the state again");
                    if !read_php(client, menu).await || !read_autostart(client, menu).await {
                        return End::Closed;
                    }
                }
                // The stream goes on after a bad event, and ends after a
                // connection error or 45 s of silence.
                Some(Err(err)) => {
                    tracing::warn!(code = err.code(), error = %err, "bad event from the daemon");
                }
                None => return End::Stopped,
            },
            command = commands.recv() => match command {
                Some(command) => run_command(client, command, menu.clone()),
                None => return End::Closed,
            },
        }
    }
}

/// Sends the PHP versions and default. A failure is a notice: the menu
/// keeps what it had. `false` once the menu is gone.
async fn read_php(client: &DaemonClient, menu: &Menu) -> bool {
    match client.runtimes().await {
        Ok(list) => {
            let php = list
                .runtimes
                .into_iter()
                .find(|row| row.runtime == Runtime::Php);
            let (installed, default) = php.map_or((Vec::new(), None), |row| {
                let installed = row
                    .installed
                    .iter()
                    .filter_map(|text| text.parse().ok())
                    .collect();
                (installed, row.default)
            });
            menu.send(Update::Php { installed, default })
        }
        Err(err) => {
            tracing::warn!(code = err.code(), error = %err, "cannot read the runtimes");
            menu.send(Update::Failed(err.code().to_owned()))
        }
    }
}

/// Sends whether the icon starts at login. A read that fails leaves the line
/// disabled, with a notice that says why. `false` once the menu is gone.
async fn read_autostart(client: &DaemonClient, menu: &Menu) -> bool {
    match client.autostart().await {
        Ok(state) => menu.send(Update::Autostart(state.into())),
        Err(err) if err.code() == AUTOSTART_UNSUPPORTED => {
            menu.send(Update::Autostart(Autostart::Unsupported))
        }
        Err(err) => {
            tracing::warn!(code = err.code(), error = %err, "cannot read the autostart state");
            menu.send(Update::Autostart(Autostart::Unknown))
                && menu.send(Update::Failed(err.code().to_owned()))
        }
    }
}

/// Runs the click on its own task, so a slow answer does not hold up the
/// events. The menu already shows the click's effect; the new state arrives
/// as an event, or is read again here when the daemon refused.
fn run_command(client: &DaemonClient, command: Command, menu: Menu) {
    let client = client.clone();
    tokio::spawn(async move {
        match command {
            // Already running.
            Command::Start => {}
            Command::Stop => {
                if let Err(err) = client.shutdown().await {
                    tracing::warn!(code = err.code(), error = %err, "cannot stop the daemon");
                    // The menu says "stopping": take that back, then say why.
                    menu.send(Update::Daemon(Daemon::Running));
                    menu.send(Update::Failed(err.code().to_owned()));
                }
            }
            Command::SetPhp(version) => match client.set_default(Runtime::Php, &version).await {
                Ok(_) => tracing::info!(version, "default PHP changed"),
                Err(err) => {
                    tracing::warn!(code = err.code(), error = %err, "cannot change the default PHP");
                    read_php(&client, &menu).await;
                    menu.send(Update::Failed(err.code().to_owned()));
                }
            },
            Command::SetAutostart(enabled) => match client.set_autostart(enabled).await {
                Ok(state) => {
                    tracing::info!(enabled, "autostart changed");
                    menu.send(Update::Autostart(state.into()));
                }
                Err(err) => {
                    tracing::warn!(code = err.code(), error = %err, "cannot change autostart");
                    read_autostart(&client, &menu).await;
                    menu.send(Update::Failed(err.code().to_owned()));
                }
            },
        }
    });
}
