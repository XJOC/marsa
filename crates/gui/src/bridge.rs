//! The bridge: one thread with a tokio runtime that owns the daemon client.
//!
//! The window and the bridge talk through two channels only: updates go to
//! the window on a `std::sync::mpsc` channel, and commands come back on a
//! tokio channel. [`DaemonClient`] never leaves this module, so the token
//! never reaches the drawing code (architecture.md section 3).

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread;
use std::time::Duration;

use eframe::egui;
use marsa_core::client::{self, ClientError, DaemonClient, StreamItem};
use marsa_core::types::ServiceAction;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::model::{LOG_SERVICE, Update};
use crate::strings;

/// The wait before the next attempt to connect.
const RETRY: Duration = Duration::from_secs(1);

/// A button the user pressed.
#[derive(Debug)]
pub struct Command {
    pub service: String,
    pub action: ServiceAction,
}

/// The window's end of the bridge.
pub struct Handle {
    pub updates: mpsc::Receiver<Update>,
    commands: UnboundedSender<Command>,
    repaint: Arc<OnceLock<egui::Context>>,
}

impl Handle {
    /// From now on each update wakes the window. Updates sent before wait in
    /// the channel for the first frame.
    pub fn attach(&self, ctx: &egui::Context) {
        // A second call keeps the first context: there is only one.
        let _ = self.repaint.set(ctx.clone());
    }

    pub fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            tracing::warn!("the bridge has stopped; the command is dropped");
        }
    }
}

/// The bridge's end of the channel to the window.
#[derive(Clone)]
struct Window {
    updates: mpsc::Sender<Update>,
    repaint: Arc<OnceLock<egui::Context>>,
}

impl Window {
    /// Sends the update and wakes the window. `false` once the window is gone.
    fn send(&self, update: Update) -> bool {
        let open = self.updates.send(update).is_ok();
        if let Some(ctx) = self.repaint.get() {
            ctx.request_repaint();
        }
        open
    }
}

/// Starts the bridge thread. It connects to the daemon (starting `marsad` if
/// needed) at once, while the window opens.
///
/// # Errors
///
/// The OS cannot start a thread.
pub fn start(data_dir: PathBuf) -> std::io::Result<Handle> {
    let (updates_tx, updates) = mpsc::channel();
    let (commands, commands_rx) = unbounded_channel();
    let repaint = Arc::new(OnceLock::new());
    let window = Window {
        updates: updates_tx,
        repaint: Arc::clone(&repaint),
    };
    thread::Builder::new()
        .name("bridge".into())
        .spawn(move || run(&data_dir, &window, commands_rx))?;
    Ok(Handle {
        updates,
        commands,
        repaint,
    })
}

fn run(data_dir: &Path, window: &Window, commands: UnboundedReceiver<Command>) {
    // One thread is enough: the bridge waits on the network most of the time.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    match runtime {
        Ok(runtime) => runtime.block_on(serve(data_dir, window, commands)),
        Err(err) => {
            tracing::error!(error = %err, "cannot start the bridge runtime");
            window.send(Update::Failed(strings::BRIDGE_FAILED.to_owned()));
        }
    }
}

/// How a session ended.
enum End {
    /// The window has closed.
    Closed,
    /// It was connected, then the event stream ended.
    Lost,
    /// It could not connect.
    Failed(ClientError),
}

async fn serve(data_dir: &Path, window: &Window, mut commands: UnboundedReceiver<Command>) {
    loop {
        let update = match session(data_dir, window, &mut commands).await {
            End::Closed => return,
            End::Lost => Update::Reconnecting,
            End::Failed(err) => {
                tracing::warn!(code = err.code(), error = %err, "cannot connect to the daemon");
                Update::Failed(err.code().to_owned())
            }
        };
        if !window.send(update) {
            return;
        }
        tokio::time::sleep(RETRY).await;
    }
}

/// Connects (starting `marsad` if needed), sends the state, then passes on
/// events and runs commands until the event stream ends.
async fn session(
    data_dir: &Path,
    window: &Window,
    commands: &mut UnboundedReceiver<Command>,
) -> End {
    let connected = async {
        let client = client::ensure_running(data_dir).await?;
        let health = client.health().await?;
        // Subscribe before reading the state, so no change falls in between.
        let events = client.events().await?;
        Ok::<_, ClientError>((client, health, events))
    };
    let (client, health, mut events) = match connected.await {
        Ok(connected) => connected,
        Err(err) => return End::Failed(err),
    };
    // Buttons pressed before this connection were meant for the last one.
    while commands.try_recv().is_ok() {}
    if !window.send(Update::Connected(health)) || !refresh(&client, window).await {
        return End::Closed;
    }
    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(StreamItem::Item(event))) => {
                    if !window.send(Update::Event(event)) {
                        return End::Closed;
                    }
                }
                // Events were dropped, so the table may be stale.
                Some(Ok(StreamItem::Lagged(count))) => {
                    tracing::warn!(count, "the daemon dropped events; reading the state again");
                    if !refresh(&client, window).await {
                        return End::Closed;
                    }
                }
                // The stream goes on after a bad event, and ends after a
                // connection error or 45 s of silence.
                Some(Err(err)) => {
                    tracing::warn!(code = err.code(), error = %err, "bad event from the daemon");
                }
                None => return End::Lost,
            },
            command = commands.recv() => match command {
                Some(command) => run_command(&client, command, window.clone()),
                None => return End::Closed,
            },
        }
    }
}

/// Sends the whole services table and the recent log lines. A failed read is
/// a notice: the events still keep the window up to date. `false` once the
/// window is gone.
async fn refresh(client: &DaemonClient, window: &Window) -> bool {
    let services = match client.services().await {
        Ok(services) => Update::Services(services),
        Err(err) => notice("cannot read the services", &err),
    };
    let log = match client.logs(LOG_SERVICE).await {
        Ok(lines) => Update::Log(lines),
        Err(err) => notice("cannot read the log", &err),
    };
    window.send(services) && window.send(log)
}

/// Runs the action on its own task, so a slow restart does not hold up the
/// events. The new state arrives as an event; only a failure is sent here.
fn run_command(client: &DaemonClient, command: Command, window: Window) {
    let client = client.clone();
    tokio::spawn(async move {
        let Command { service, action } = command;
        if let Err(err) = client.service_action(&service, action).await {
            window.send(notice(&format!("cannot {action} {service}"), &err));
        }
    });
}

fn notice(what: &str, err: &ClientError) -> Update {
    tracing::warn!(code = err.code(), error = %err, "{what}");
    Update::Notice(err.code().to_owned())
}
