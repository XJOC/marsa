//! The bridge: one thread with a tokio runtime that owns the daemon client.
//!
//! The window and the bridge talk through two channels only: updates go to
//! the window on a `std::sync::mpsc` channel, and commands come back on a
//! tokio channel. [`DaemonClient`] never leaves this module, so the token
//! never reaches the drawing code (architecture.md section 3).
//!
//! Each connection is a session with a number, and every message either way is
//! [`Stamped`] with one. A session's tasks end with it, and neither side acts
//! on a message of another session: the window's request numbers start again
//! with each one, so an old answer could pass for a new one.

mod commands;
mod db_browser;
mod dumps;
#[cfg(test)]
pub(crate) mod fake;
mod mail;
#[cfg(test)]
mod session_tests;
mod site_log;

pub use db_browser::{DbCommand, DbUpdate};

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread;
use std::time::Duration;

use eframe::egui;
use marsa_core::client::{self, ClientError, DaemonClient, StreamItem};
use marsa_core::types::{
    Event, HostsStatus, InstallProgress, InstallUpdate, ServiceAction, ServiceId, ServiceStatus,
    SiteKey, SiteList, WorkerKind,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinSet;

use crate::model::{Autostart, Change, InstallJob, Removal, Update};
use crate::strings;

/// The wait before the next attempt to connect.
const RETRY: Duration = Duration::from_secs(1);
/// The longest a closing window waits for the daemon to close the browser.
const CLOSE_WAIT: Duration = Duration::from_secs(2);
/// The daemon's answer to `/v1/autostart` on a system it does not support.
const AUTOSTART_UNSUPPORTED: &str = "autostart_unsupported";

/// What the user asked for.
#[derive(Debug)]
pub enum Command {
    /// A service button.
    Service {
        service: ServiceId,
        action: ServiceAction,
    },
    /// "Park a folder…": every sub-folder of this absolute folder becomes a
    /// site.
    Park(PathBuf),
    /// "Link a folder…": this absolute folder becomes the site `name`.
    Link { name: String, folder: PathBuf },
    /// A site's HTTPS switch.
    Secure { site: String, secure: bool },
    /// A worker's switch on a Laravel site's page.
    SetWorker {
        kind: WorkerKind,
        site: SiteKey,
        on: bool,
    },
    /// A default version or a site's PHP version, picked in a list.
    Save(Change),
    /// An Install button, or the first-run panel.
    Install(InstallJob),
    /// "Run setup". The daemon may wait minutes for the password prompt.
    Setup,
    /// The "Start at login" box.
    SetAutostart(bool),
    /// A confirmed Uninstall, Unlink, or Forget.
    Remove(Removal),
    /// The Xdebug box.
    SetXdebug(bool),
    /// "Add to /etc/hosts" (Linux): the only way the window
    /// syncs the hosts file, so a password prompt comes from a click only.
    SyncHosts,
    /// The Mail screen opened: the newest mails.
    ReadMail,
    /// A mail of the list was opened, by its id: its text. Mailpit marks it
    /// read.
    ReadMailText(String),
    /// The confirmed "Delete all…" of Mail.
    ClearMail,
    /// The Dumps screen opened: what is new.
    ReadDumps,
    /// "Clear" on Dumps.
    ClearDumps,
    /// The switch "Capture" on Dumps: on or off.
    SetDumpsCapture(bool),
    /// A read of the Databases screen, or its close.
    Db(DbCommand),
    /// Logs shows the log of this Laravel site: follow it, in place of the
    /// one followed before.
    FollowSiteLog(String),
    /// Logs no longer shows a site's log: stop following it.
    StopSiteLog,
    /// The window closes: close the database browser, answer here, and end.
    Quit(mpsc::Sender<()>),
}

/// An update or a command, and the session it belongs to: the one that
/// sends an update, or the one the window was in when it made a command. 0 is
/// no session: before the first connection.
#[derive(Debug)]
pub struct Stamped<T> {
    pub session: u64,
    pub body: T,
}

/// The window's end of the bridge.
pub struct Handle {
    pub updates: mpsc::Receiver<Stamped<Update>>,
    commands: UnboundedSender<Stamped<Command>>,
    repaint: Arc<OnceLock<egui::Context>>,
    /// The demo's end of `updates`: kept so the channel stays open and the
    /// window does not take it for a bridge that stopped.
    _demo: Option<mpsc::Sender<Stamped<Update>>>,
}

impl Handle {
    /// From now on each update wakes the window. Updates sent before wait in
    /// the channel for the first frame.
    pub fn attach(&self, ctx: &egui::Context) {
        // A second call keeps the first context: there is only one.
        let _ = self.repaint.set(ctx.clone());
    }

    /// A bridge with no thread and no daemon behind it, for the design
    /// review's demo (see `demo`) and the measurement mode (see `measure`).
    /// Its commands go nowhere.
    // Debug builds, the `measure` feature, and the tests only: a shipped
    // binary has no bridge that goes nowhere.
    #[cfg(any(debug_assertions, feature = "measure", test))]
    pub fn detached() -> Self {
        let (demo, updates) = mpsc::channel();
        let (commands, _) = unbounded_channel();
        Self {
            updates,
            commands,
            repaint: Arc::new(OnceLock::new()),
            _demo: Some(demo),
        }
    }

    /// The window closes while the daemon may hold its database
    /// connections: has them closed, which stops a running statement, and
    /// waits for that, at most [`CLOSE_WAIT`]. The process ends next, and
    /// would take a request not yet sent with it.
    pub fn close_databases(&self, session: u64) {
        let (done, closed) = mpsc::channel();
        self.send(session, Command::Quit(done));
        // An error: the time ran out, or the bridge dropped the command and
        // `done` with it.
        let _ = closed.recv_timeout(CLOSE_WAIT);
    }

    /// Sends a command the window made in `session`. The bridge drops it if
    /// that session has ended.
    pub fn send(&self, session: u64, command: Command) {
        let command = Stamped {
            session,
            body: command,
        };
        if self.commands.send(command).is_err() {
            tracing::warn!("the bridge has stopped; the command is dropped");
        }
    }
}

/// The bridge's end of the channel to the window.
#[derive(Clone)]
struct Window {
    updates: mpsc::Sender<Stamped<Update>>,
    repaint: Arc<OnceLock<egui::Context>>,
    /// The session whose updates go through this end.
    session: u64,
}

impl Window {
    /// Sends the update and wakes the window. `false` once the window is gone.
    fn send(&self, update: Update) -> bool {
        let update = Stamped {
            session: self.session,
            body: update,
        };
        let open = self.updates.send(update).is_ok();
        if let Some(ctx) = self.repaint.get() {
            ctx.request_repaint();
        }
        open
    }
}

/// What the bridge does when no daemon runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Daemon {
    /// Start `marsad`: the window is a way to use Marsa.
    Start,
    /// Wait for one: the menu-bar panel only looks. "Stop Marsa" in the icon
    /// stays stopped when the panel opens.
    Attach,
}

/// Starts the bridge thread. It connects to the daemon (starting `marsad` if
/// needed) at once, while the window opens.
///
/// # Errors
///
/// The OS cannot start a thread.
pub fn start(data_dir: PathBuf) -> std::io::Result<Handle> {
    start_with(data_dir, Daemon::Start)
}

/// [`start`] for the menu-bar panel: it connects to a daemon
/// that runs, and never starts one. With none, the model says the daemon is
/// not running, and tries again every second.
///
/// # Errors
///
/// The OS cannot start a thread.
pub fn attach(data_dir: PathBuf) -> std::io::Result<Handle> {
    start_with(data_dir, Daemon::Attach)
}

fn start_with(data_dir: PathBuf, daemon: Daemon) -> std::io::Result<Handle> {
    let (updates_tx, updates) = mpsc::channel();
    let (commands, commands_rx) = unbounded_channel();
    let repaint = Arc::new(OnceLock::new());
    let window = Window {
        updates: updates_tx,
        repaint: Arc::clone(&repaint),
        session: 0,
    };
    thread::Builder::new()
        .name("bridge".into())
        .spawn(move || run(&data_dir, &window, commands_rx, daemon))?;
    Ok(Handle {
        updates,
        commands,
        repaint,
        _demo: None,
    })
}

fn run(
    data_dir: &Path,
    window: &Window,
    commands: UnboundedReceiver<Stamped<Command>>,
    daemon: Daemon,
) {
    // One thread is enough: the bridge waits on the network most of the time.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    match runtime {
        Ok(runtime) => runtime.block_on(serve(data_dir, window, commands, daemon)),
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

async fn serve(
    data_dir: &Path,
    window: &Window,
    mut commands: UnboundedReceiver<Stamped<Command>>,
    daemon: Daemon,
) {
    // Each attempt to connect takes the next number.
    for number in 1_u64.. {
        let window = Window {
            session: number,
            ..window.clone()
        };
        let update = match session(data_dir, &window, &mut commands, daemon).await {
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
    commands: &mut UnboundedReceiver<Stamped<Command>>,
    daemon: Daemon,
) -> End {
    let connected = async {
        let client = match daemon {
            Daemon::Start => client::ensure_running(data_dir).await?,
            Daemon::Attach => DaemonClient::connect(data_dir)?,
        };
        let health = client.health().await?;
        // Subscribe before reading the state, so no change falls in between.
        let events = client.events().await?;
        Ok::<_, ClientError>((client, health, events))
    };
    let (client, health, mut events) = match connected.await {
        Ok(connected) => connected,
        Err(err) => return End::Failed(err),
    };
    // Each of these stops with this session when it is dropped.
    let mut tasks = JoinSet::new();
    let mail = mail::Mail::start(&client, window);
    let dumps = dumps::Dumps::start(&client, window);
    let mut site_log = site_log::SiteLog::default();
    if !window.send(Update::Connected(health)) || !refresh(&client, window, &mail).await {
        return End::Closed;
    }
    read_doctor(&mut tasks, &client, window);
    // A command's task asks here to read the runtimes again, so the read
    // runs in order with the events. This end lives as long as the loop, so
    // `recv` never ends.
    let (reread, mut rereads) = unbounded_channel::<()>();
    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(StreamItem::Item(event))) => {
                    if let Event::SitesChanged(list) = &event {
                        log_sites("sites changed", list);
                    }
                    if let Event::ServiceChanged(status) = &event {
                        mail.follow(status);
                    }
                    if matches!(&event, Event::DumpsChanged { .. }) {
                        dumps.send(dumps::Job::Read);
                    }
                    let installed = matches!(
                        &event,
                        Event::Install(InstallProgress { update: InstallUpdate::Done(_), .. })
                    );
                    // A new site may lack its hosts line; setup may have
                    // written the block (Linux).
                    let setup_finished = matches!(&event, Event::SetupFinished(_));
                    let hosts_changed = setup_finished || matches!(&event, Event::SitesChanged(_));
                    if !window.send(Update::Event(event)) {
                        return End::Closed;
                    }
                    if hosts_changed && !window.send(hosts(&client).await) {
                        return End::Closed;
                    }
                    // Setup changes what the checks find.
                    if setup_finished {
                        read_doctor(&mut tasks, &client, window);
                    }
                    // A new version, and maybe a first default: no event
                    // carries the whole list, so read it again.
                    if installed && !runtimes_changed(&client, window).await {
                        return End::Closed;
                    }
                }
                // Events were dropped, so the table may be stale.
                Some(Ok(StreamItem::Lagged(count))) => {
                    tracing::warn!(count, "the daemon dropped events; reading the state again");
                    // One of them may have been `dumps_changed`.
                    dumps.send(dumps::Job::Read);
                    if !refresh(&client, window, &mail).await {
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
            command = commands.recv() => {
                let Some(command) = command else {
                    return End::Closed;
                };
                // Pressed in a session that has ended, or before this one.
                if command.session != window.session {
                    continue;
                }
                match command.body {
                    Command::ReadMail => mail.send(mail::Job::Read),
                    Command::ReadMailText(id) => mail.send(mail::Job::Text(id)),
                    Command::ClearMail => mail.send(mail::Job::Clear),
                    Command::ReadDumps => dumps.send(dumps::Job::Read),
                    Command::ClearDumps => dumps.send(dumps::Job::Clear),
                    Command::SetDumpsCapture(on) => dumps.send(dumps::Job::Capture(on)),
                    Command::FollowSiteLog(site) => site_log.follow(&client, window, site),
                    Command::StopSiteLog => site_log.stop(),
                    Command::Quit(done) => {
                        // Nothing else of this window may follow the close.
                        tasks.shutdown().await;
                        match tokio::time::timeout(CLOSE_WAIT, client.db_close()).await {
                            Ok(Ok(())) => {}
                            Ok(Err(err)) => {
                                tracing::warn!(code = err.code(), "the browser was not closed");
                            }
                            Err(_) => tracing::warn!("the daemon did not close the browser in time"),
                        }
                        // The window may have given up waiting.
                        let _ = done.send(());
                        return End::Closed;
                    }
                    command => {
                        let (window, reread) = (window.clone(), reread.clone());
                        commands::run_command(&mut tasks, &client, command, window, reread);
                    }
                }
            }
            // Forgets the tasks that have ended.
            Some(_) = tasks.join_next() => {}
            Some(()) = rereads.recv() => {
                if !runtimes_changed(&client, window).await {
                    return End::Closed;
                }
            }
        }
    }
}

/// After an install or an uninstall: the runtimes list, and the Xdebug
/// state, whose lists name the installed PHP versions. `false` once the
/// window is gone.
async fn runtimes_changed(client: &DaemonClient, window: &Window) -> bool {
    window.send(runtimes(client).await) && window.send(xdebug(client).await)
}

/// Sends the whole services table, the site list, the runtimes, whether the
/// icon starts at login, whether PHP loads Xdebug, and the recent log lines,
/// and tells `mail` whether Mailpit runs. A failed read is a notice: the
/// events still keep the window up to date. `false` once the window is gone.
async fn refresh(client: &DaemonClient, window: &Window, mail: &mail::Mail) -> bool {
    let table = client.services().await;
    for service in table.as_deref().unwrap_or_default() {
        mail.follow(service);
    }
    // The log of every service the daemon lists, and Caddy's whatever the
    // table says: the Logs screen has one source for each.
    let log_ids = log_ids(table.as_deref().unwrap_or_default());
    let services = match table {
        Ok(services) => Update::Services(services),
        Err(err) => notice("cannot read the services", &err),
    };
    let sites = match client.sites().await {
        Ok(list) => {
            log_sites("site list read", &list);
            Update::Sites(list)
        }
        Err(err) => sites_failed("cannot read the sites", &err),
    };
    let runtimes = runtimes(client).await;
    // No event tells when it changes (the menu-bar icon may change it), so
    // it is read with the rest.
    let autostart = autostart(client).await;
    // No event either: only the Xdebug box and `marsa xdebug` change it.
    let xdebug = xdebug(client).await;
    let mut logs = Vec::with_capacity(log_ids.len());
    for id in &log_ids {
        logs.push(match client.logs(&id.to_string()).await {
            Ok(lines) => Update::Log(lines),
            Err(err) => notice("cannot read the log", &err),
        });
    }
    let hosts = hosts(client).await;
    window.send(services)
        && window.send(sites)
        && window.send(hosts)
        && window.send(runtimes)
        && window.send(autostart)
        && window.send(xdebug)
        && logs.into_iter().all(|log| window.send(log))
}

/// The ids whose logs are read: Caddy, then each other service listed.
fn log_ids(table: &[ServiceStatus]) -> Vec<ServiceId> {
    let others = table
        .iter()
        .map(|service| service.id)
        .filter(|id| *id != ServiceId::Caddy);
    std::iter::once(ServiceId::Caddy).chain(others).collect()
}

/// Reads the daemon's checks on their own task: they take seconds (the
/// resolver, `netstat`), and nothing else waits for them. A failure is in the
/// log only; Settings then says "Not checked".
fn read_doctor(tasks: &mut JoinSet<()>, client: &DaemonClient, window: &Window) {
    let (client, window) = (client.clone(), window.clone());
    tasks.spawn(async move {
        match client.doctor().await {
            Ok(report) => {
                window.send(Update::Doctor(report));
            }
            Err(err) => {
                tracing::warn!(code = err.code(), error = %err, "cannot read the doctor checks");
            }
        }
    });
}

/// Whether the hosts file holds this data folder's names (Linux). Reading
/// shows no prompt. A daemon of an older build has no such route; any
/// failure counts as "not used", in the log only: `marsa doctor` checks it.
async fn hosts(client: &DaemonClient) -> Update {
    match client.hosts().await {
        Ok(status) => Update::Hosts(status),
        Err(err) => {
            if err.code() != "not_found" {
                tracing::warn!(code = err.code(), error = %err, "cannot read the hosts status");
            }
            Update::Hosts(HostsStatus::default())
        }
    }
}

/// Whether PHP loads Xdebug, or why it cannot be read.
async fn xdebug(client: &DaemonClient) -> Update {
    match client.xdebug().await {
        Ok(state) => Update::Xdebug(state),
        Err(err) => {
            tracing::warn!(code = err.code(), error = %err, "cannot read the Xdebug state");
            Update::XdebugFailed(err.code().to_owned())
        }
    }
}

/// Whether the icon starts at login, or why it cannot be read.
async fn autostart(client: &DaemonClient) -> Update {
    match client.autostart().await {
        Ok(state) => Update::Autostart(state.into()),
        Err(err) if err.code() == AUTOSTART_UNSUPPORTED => {
            Update::Autostart(Autostart::Unsupported)
        }
        Err(err) => {
            tracing::warn!(code = err.code(), error = %err, "cannot read the autostart state");
            Update::AutostartFailed(err.code().to_owned())
        }
    }
}

/// The runtimes list, or why it cannot be read.
async fn runtimes(client: &DaemonClient) -> Update {
    match client.runtimes().await {
        Ok(list) => Update::Runtimes(list),
        Err(err) => {
            tracing::warn!(code = err.code(), error = %err, "cannot read the runtimes");
            Update::RuntimesFailed(err.code().to_owned())
        }
    }
}

fn notice(what: &str, err: &ClientError) -> Update {
    tracing::warn!(code = err.code(), error = %err, "{what}");
    Update::Notice(err.code().to_owned())
}

/// As [`notice`], for the line in the sites section.
fn sites_failed(what: &str, err: &ClientError) -> Update {
    tracing::warn!(code = err.code(), error = %err, "{what}");
    Update::SitesFailed(err.code().to_owned())
}

/// The size of a site list, to follow the window's updates in the log.
fn log_sites(what: &str, list: &SiteList) {
    tracing::debug!(
        sites = list.sites.len(),
        skipped = list.skipped.len(),
        parked = list.parked.len(),
        "{what}"
    );
}

#[cfg(test)]
mod tests {
    use marsa_core::data_dir::DataDir;
    use marsa_core::types::ServiceState;

    use super::*;

    fn service(id: ServiceId) -> ServiceStatus {
        ServiceStatus {
            id,
            state: ServiceState::Running,
            pid: None,
            restarts: 0,
            since_ms: None,
        }
    }

    #[test]
    fn the_logs_read_are_caddys_and_each_other_service_listed_once() {
        let table = [
            service(ServiceId::Caddy),
            service("php-fpm-8.3".parse::<ServiceId>().unwrap()),
            service("php-fpm-8.4".parse::<ServiceId>().unwrap()),
        ];
        assert_eq!(
            log_ids(&table),
            [
                ServiceId::Caddy,
                "php-fpm-8.3".parse::<ServiceId>().unwrap(),
                "php-fpm-8.4".parse::<ServiceId>().unwrap()
            ]
        );
        // Caddy is read even when the table has none, or could not be read.
        assert_eq!(
            log_ids(&[service("php-fpm-8.4".parse::<ServiceId>().unwrap())]),
            [
                ServiceId::Caddy,
                "php-fpm-8.4".parse::<ServiceId>().unwrap()
            ]
        );
        assert_eq!(log_ids(&[]), [ServiceId::Caddy]);
    }

    /// The menu-bar panel looks; it never starts a daemon. In an empty data
    /// folder its bridge says the daemon is not running, and starts nothing:
    /// the window's bridge would run `marsad` there.
    #[test]
    fn an_attached_bridge_reports_no_daemon_and_starts_none() {
        let dir = tempfile::tempdir().expect("temp dir");
        let handle = attach(dir.path().to_path_buf()).expect("a thread");
        let update = handle
            .updates
            .recv_timeout(Duration::from_secs(10))
            .expect("an answer")
            .body;
        assert!(
            matches!(&update, Update::Failed(code) if code == "daemon_not_running"),
            "{update:?}"
        );
        // Nothing was written where a daemon would put its files.
        assert!(!DataDir::new(dir.path()).run().join("daemon.json").exists());
    }
}
