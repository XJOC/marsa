//! The two kinds of message the model takes: what the bridge sends
//! ([`Update`]) and what the user did ([`Action`]). Each is one long list,
//! so they live apart from the model that handles them.

use super::*;

/// What the bridge sends to the window.
#[derive(Debug)]
pub enum Update {
    /// Connected. A new session: the daemon may have restarted, so the old
    /// table and log are dropped.
    Connected(Health),
    /// The connection ended; the bridge tries again every second.
    Reconnecting,
    /// The daemon cannot be reached or started. A client error code; the
    /// bridge tries again every second.
    Failed(String),
    /// The whole services table.
    Services(Vec<ServiceStatus>),
    /// Recent lines of the services' logs (each line names its service),
    /// oldest first.
    Log(Vec<LogLine>),
    /// A call failed but the connection is fine. A client error code.
    Notice(String),
    /// The whole site list, read when a session starts and again after the
    /// daemon dropped events.
    Sites(SiteList),
    /// The daemon accepted this folder to park. The new list comes as
    /// `sites_changed`.
    Parked(PathBuf),
    /// The daemon linked `folder` as the site `name`. The new list comes as
    /// `sites_changed`.
    Linked {
        name: String,
        folder: PathBuf,
    },
    /// A sites call failed but the connection is fine. A client error code.
    SitesFailed(String),
    /// Every runtime with its installed versions and default, read when a
    /// session starts and after an install.
    Runtimes(RuntimeList),
    /// The runtimes could not be read. A client error code.
    RuntimesFailed(String),
    /// The daemon saved this change. The new state comes as an event.
    Saved(Change),
    /// The daemon refused this change, or could not be reached. A client
    /// error code.
    NotSaved {
        change: Change,
        code: String,
    },
    /// The daemon accepted `job`: its `install` events carry `id`.
    InstallAccepted {
        job: InstallJob,
        id: u64,
    },
    /// The daemon refused `job` (`install_busy`), or could not be reached. A
    /// client error code.
    InstallRefused {
        job: InstallJob,
        code: String,
    },
    /// The answer to "Run setup".
    SetupDone(SetupReport),
    /// "Run setup" ended before the helper ran (`setup_running`,
    /// `setup_not_supported`), or the daemon could not be reached. A client
    /// error code.
    SetupFailed(String),
    /// Whether the menu-bar icon starts at login, as the daemon says.
    Autostart(Autostart),
    /// Reading or changing it failed. A client error code.
    AutostartFailed(String),
    /// The daemon removed this. After an uninstall the bridge reads the
    /// runtimes again: no event carries them.
    Removed(Removal),
    /// The daemon refused or failed the removal, or could not be reached. A
    /// client error code; `in_use` holds the facts of `version_in_use`.
    NotRemoved {
        removal: Removal,
        code: String,
        in_use: Option<VersionInUse>,
    },
    /// The daemon's checks, as read (no event carries them).
    Doctor(DoctorReport),
    /// Whether PHP loads Xdebug, as read (no event carries it).
    Xdebug(XdebugState),
    /// Reading it failed. A client error code.
    XdebugFailed(String),
    /// The answer to the Xdebug box: the state after the change, or a client
    /// error code.
    XdebugAnswered(Result<XdebugState, String>),
    /// Linux: whether the hosts file holds this data
    /// folder's names, and the sites it lacks. Read when a session starts,
    /// after each site list, after setup, and after "Add to /etc/hosts".
    Hosts(HostsStatus),
    /// The answer to "Add to /etc/hosts".
    HostsSynced(SetupReport),
    /// "Add to /etc/hosts" did not run, or the daemon could not be reached.
    /// A client error code.
    HostsSyncFailed(String),
    /// The newest mails Mailpit caught. The bridge sends a list only when it
    /// differs from the last one it sent.
    Mail(MailList),
    /// The text of a mail the user opened.
    MailText(MailText),
    /// Reading the list failed. A client error code; `mail_not_running`
    /// while Mailpit does not run.
    MailFailed(String),
    /// What the bridge read of the dumps: the ones newer than the last it
    /// sent, or all of them when `replace`.
    Dumps {
        list: DumpList,
        replace: bool,
    },
    /// Reading the dumps failed. A client error code.
    DumpsFailed(String),
    Event(Event),
}

/// What the user did in the window. The shell and the screens return it,
/// and [`Model::apply_action`] turns it into state, and into a bridge
/// [`Command`] when the daemon must act (code.md, State). [Laravel: a form
/// posted to a controller.]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// A sidebar item, or the status at its bottom.
    Navigate(Route),
    /// The fold button of the sidebar: hide or show the labels.
    FoldSidebar,
    /// A service's Start, Stop, or Restart.
    Service {
        service: ServiceId,
        action: ServiceAction,
    },
    /// A level filter of the Logs screen: show or hide its lines.
    ToggleLogLevel(LogLevel),
    /// The text of the filter field of the Logs screen.
    SetLogFilter(String),
    /// The Follow button of the Logs screen, or a scroll away from the end
    /// of the log: keep the newest line in view, or stop.
    SetLogFollow(bool),
    /// A version button of the default picker (Home): save `version` as the
    /// default of `runtime`.
    SetDefault { runtime: Runtime, version: Version },
    /// A fix that installs a program (Home: "Install Caddy"). Queued like a
    /// press of Install; `Model::apply_action` sends it.
    Install(InstallJob),
    /// The fix "Add to /etc/hosts" (Linux).
    SyncHosts,
    /// "Open" on a site. A local action: `App` hands the address
    /// ([`Model::site_url`]) to the system, never a screen (code.md, State).
    OpenSite(String),
    /// "Open folder" on a site's page: the system's file manager shows the
    /// site's folder. A local action like [`Action::OpenSite`]: the model finds
    /// the folder ([`Model::site_root`]), and `App` starts the system's opener.
    OpenFolder(String),
    /// The first-run button that installs what is missing.
    StartFirstRun,
    /// "Run setup".
    RunSetup,
    /// "Park a folder…" or "Link a folder…". A local action: `App` opens the
    /// system's folder dialog, and takes the folder it gives back.
    PickFolder(Pick),
    /// "Not now": Home takes the place of the first-run panel.
    CloseFirstRun,
    /// The text of the search field of the Sites screen.
    SearchSites(String),
    /// An entry of a site's version menu: save `choice` as its PHP, or clear
    /// its own setting ([`Choice::Auto`]).
    SitePhp { site: String, choice: Choice },
    /// The site's menu: serve it over HTTPS (`true`) or plain HTTP.
    SetSecure { site: String, secure: bool },
    /// "Unlink…" of a site: the confirmation opens.
    AskUnlink(String),
    /// "Forget…" of a parked folder: the confirmation opens.
    AskForget(PathBuf),
    /// The site name typed in the link form.
    SetLinkName(String),
    /// "Link" in the link form (or Enter): sends the link if the name is
    /// valid.
    SubmitLink,
    /// "Cancel" in the link form.
    CancelLink,
    /// The close button of a notice, or its 4 seconds running out: the
    /// result it told about goes.
    DismissNotice(NoticeSource),
    /// "Install a version…" (or the dashed "+") of a runtime: its install
    /// dialog opens.
    AskInstall(Runtime),
    /// The text typed in the install dialog's version field.
    SetInstallVersion { runtime: Runtime, text: String },
    /// "Install" in the dialog (or Enter): queues the version in the field,
    /// or says why it is wrong. `apply_action` returns the job to send.
    SubmitInstall(Runtime),
    /// "Cancel" or Escape in the install dialog.
    CancelInstall,
    /// "Uninstall…" in a version's row menu: the confirmation opens.
    AskUninstall { runtime: Runtime, version: Version },
    /// The Xdebug switch.
    ToggleXdebug,
    /// The confirmation's action ("Unlink", "Forget", "Uninstall"): sends the
    /// removal it asked about.
    ConfirmRemoval,
    /// "Cancel" or Escape in the confirmation.
    CancelRemoval,
    /// The Start at login switch in Settings.
    ToggleAutostart,
    /// "Open folder" on the data folder in Settings. A local action like
    /// [`Action::OpenFolder`]: `App` shows [`Model::data_dir`] in the system's
    /// file manager.
    OpenDataFolder,
    /// An option of the Appearance control in Settings. `App` applies it at
    /// once and writes it to `gui.toml`.
    SetAppearance(Appearance),
    /// A row of the Mail list, by the mail's id: its text is read.
    ChooseMail(String),
    /// "Delete all…" on Mail: the confirmation opens.
    AskDeleteMail,
    /// "Open Mailpit": Mailpit's page in the browser.
    OpenMailpit,
    /// "Open in browser": the chosen mail's page in Mailpit, with its HTML.
    OpenMailPage,
    /// A row of the Dumps list, by the dump's id.
    ChooseDump(u64),
    /// "Clear" on Dumps.
    ClearDumps,
    /// The switch "Capture" on Dumps.
    ToggleDumps,
    /// "Copy" on Dumps: the shown dump's text goes to the clipboard.
    CopyDump,
}
